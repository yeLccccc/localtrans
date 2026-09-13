/** @vitest-environment jsdom */
/**
 * Task M1: 前端 console/错误日志桥单测
 * - 非 Tauri（含浏览器 mock）环境完全 no-op：不 patch console、不调 invoke
 * - Tauri 环境：50ms 批量 flush、attach 锚点、console 原行为保留、
 *   Vue errorHandler / unhandledrejection 上报、路由随行、消息截断
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }))

import { invoke } from '@tauri-apps/api/core'
import { createApp, type App as VueApp } from 'vue'
import { createRouter, createMemoryHistory, type Router } from 'vue-router'
import { createLogBridge, isTauriEnv, __resetLogBridgeForTest } from '../logBridge'

// 真实 console 引用：用例里临时替换后恢复，避免污染 vitest 自身输出
const REAL_WARN = console.warn
const REAL_ERROR = console.error

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { render: () => null } },
      { path: '/settings', component: { render: () => null } },
    ],
  })
}

function makeApp(router: Router): VueApp {
  const app = createApp({ render: () => null })
  app.use(createLogBridge(router))
  return app
}

/** 模拟真实 Tauri 注入（无 mock 标记） */
function fakeTauriInternals() {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: vi.fn(),
  }
}

/** 模拟 tauriMock 注入浏览器的实例（带 __localtransMock 标记） */
function fakeBrowserMockInternals() {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = {
    invoke: vi.fn(),
    __localtransMock: true,
  }
}

function clearInternals() {
  delete (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__
}

beforeEach(() => {
  vi.clearAllMocks()
  __resetLogBridgeForTest()
  clearInternals()
})

afterEach(() => {
  __resetLogBridgeForTest()
  clearInternals()
  console.warn = REAL_WARN
  console.error = REAL_ERROR
  vi.useRealTimers()
})

describe('isTauriEnv', () => {
  it('无 internals → false', () => {
    expect(isTauriEnv()).toBe(false)
  })

  it('浏览器 mock 注入（带标记）→ false', () => {
    fakeBrowserMockInternals()
    expect(isTauriEnv()).toBe(false)
  })

  it('真实 Tauri internals → true', () => {
    fakeTauriInternals()
    expect(isTauriEnv()).toBe(true)
  })
})

describe('非 Tauri 环境 no-op', () => {
  it('不 patch console、不调用 invoke', async () => {
    vi.useFakeTimers()
    const warnRef = console.warn
    const errorRef = console.error
    makeApp(makeRouter())

    // console 完全未被包装，行为保持原样
    expect(console.warn).toBe(warnRef)
    expect(console.error).toBe(errorRef)

    console.warn('浏览器预览的一条警告')
    await vi.advanceTimersByTimeAsync(200)
    expect(invoke).not.toHaveBeenCalled()
  })

  it('tauriMock 注入的浏览器环境同样 no-op', async () => {
    vi.useFakeTimers()
    fakeBrowserMockInternals()
    const warnRef = console.warn
    makeApp(makeRouter())

    expect(console.warn).toBe(warnRef)
    console.error('浏览器 mock 下的一条错误')
    await vi.advanceTimersByTimeAsync(200)
    expect(invoke).not.toHaveBeenCalled()
  })
})

describe('Tauri 环境日志桥', () => {
  it('attach 后上报 info 级 logBridge attached（50ms 批量窗口内聚合）', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    makeApp(makeRouter())

    // 窗口期内先缓冲，不立即 IPC
    expect(invoke).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(50)
    expect(invoke).toHaveBeenCalledWith('ui_log', {
      level: 'info',
      message: 'logBridge attached',
      route: null,
    })
  })

  it('console.error 拦截上报且保留原始输出，route 随当前路由', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    // 安装前占位原始 console.error，验证透传
    const origError = vi.fn()
    console.error = origError
    const router = makeRouter()
    makeApp(router)

    await router.push('/settings')
    const err = new Error('传输失败')
    console.error('下载出错', err)

    // 原始行为保留：参数原样透传
    expect(origError).toHaveBeenCalledWith('下载出错', err)
    await vi.advanceTimersByTimeAsync(50)
    expect(invoke).toHaveBeenCalledWith('ui_log', {
      level: 'error',
      message: `下载出错 Error: ${err.message}`,
      route: '/settings',
    })
  })

  it('console.warn 拦截上报 warn 级', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    const origWarn = vi.fn()
    console.warn = origWarn
    makeApp(makeRouter())

    console.warn('配置缺失', { key: 'relay' })
    expect(origWarn).toHaveBeenCalledWith('配置缺失', { key: 'relay' })
    await vi.advanceTimersByTimeAsync(50)
    expect(invoke).toHaveBeenCalledWith('ui_log', {
      level: 'warn',
      message: '配置缺失 {"key":"relay"}',
      route: null,
    })
  })

  it('50ms 窗口内多条聚合成多次 invoke、窗口静默期不触发', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    const origError = vi.fn()
    console.error = origError
    makeApp(makeRouter())

    console.error('e1')
    console.error('e2')
    console.error('e3')
    // 尚未到窗口期
    await vi.advanceTimersByTimeAsync(10)
    expect(invoke).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(40)
    const calls = vi.mocked(invoke).mock.calls.filter((c) => c[0] === 'ui_log')
    const messages = calls.map((c) => (c[1] as { message: string }).message)
    expect(messages).toContain('e1')
    expect(messages).toContain('e2')
    expect(messages).toContain('e3')
    // 窗口后静默：不再有新调用
    const countAfterWindow = vi.mocked(invoke).mock.calls.length
    await vi.advanceTimersByTimeAsync(200)
    expect(vi.mocked(invoke).mock.calls.length).toBe(countAfterWindow)
  })

  it('单条消息截断到 500 字符防爆', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    const origError = vi.fn()
    console.error = origError
    makeApp(makeRouter())

    console.error('x'.repeat(600))
    await vi.advanceTimersByTimeAsync(50)
    const call = vi
      .mocked(invoke)
      .mock.calls.find((c) => (c[1] as { message: string }).message.length > 400)
    expect(call).toBeDefined()
    expect((call![1] as { message: string }).message.length).toBe(501) // 500 + 省略号
  })

  it('Vue errorHandler 经桥上报（且不吞原始输出）', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    const origError = vi.fn()
    console.error = origError
    const app = makeApp(makeRouter())

    expect(app.config.errorHandler).toBeDefined()
    app.config.errorHandler?.(new Error('渲染炸了'), null, 'render')
    expect(origError).toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync(50)
    expect(invoke).toHaveBeenCalledWith('ui_log', {
      level: 'error',
      message: '[Vue render] Error: 渲染炸了',
      route: null,
    })
  })

  it('unhandledrejection 上报 error 级', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    makeApp(makeRouter())

    window.dispatchEvent(
      new PromiseRejectionEvent('unhandledrejection', {
        promise: Promise.resolve(),
        reason: '网络中断',
      }),
    )
    await vi.advanceTimersByTimeAsync(50)
    expect(invoke).toHaveBeenCalledWith('ui_log', {
      level: 'error',
      message: 'unhandledrejection: 网络中断',
      route: null,
    })
  })

  it('重复 install 幂等（console 只 patch 一次）', async () => {
    vi.useFakeTimers()
    fakeTauriInternals()
    const router = makeRouter()
    makeApp(router)
    const patchedWarn = console.warn
    makeApp(router) // 第二次 install 应直接返回

    expect(console.warn).toBe(patchedWarn)
    console.warn('只应有一条')
    await vi.advanceTimersByTimeAsync(50)
    const warns = vi
      .mocked(invoke)
      .mock.calls.filter(
        (c) => c[0] === 'ui_log' && (c[1] as { message: string }).message === '只应有一条',
      )
    expect(warns.length).toBe(1)
  })
})
