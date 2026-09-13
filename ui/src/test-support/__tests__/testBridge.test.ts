/** @vitest-environment jsdom */
/**
 * Task M2: 前端测试通道桥单测（spec §7.1.4）
 * - 选择器翻译纯函数（[testid=x] 简写 / ^= 前缀 / 引号 / 嵌套组合）
 * - tree 遍历：收录、可见性剪枝、aria-hidden 剪枝、文本截断、隐式 role
 * - input 的 v-model 兼容序列（真实 Vue 组件，input 事件后 ref 更新）
 * - wait 命中与超时（fake timers + MutationObserver/轮询）
 * - 注册门控：默认环境/浏览器 mock/非 Tauri 不注册；测试模式 + Tauri 注册并握手
 * - exec 回包：ok/error 经 invoke('test_bridge_result') 回传
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(undefined) }))

import { invoke } from '@tauri-apps/api/core'
import { createApp, h, ref, type App as VueApp } from 'vue'
import { createPinia, setActivePinia } from 'pinia'
import { createRouter, createMemoryHistory, type Router } from 'vue-router'
import {
  setupTestBridge,
  runAction,
  parseBridgeRequest,
  translateSelector,
  collectTree,
  describeElement,
  setNativeValue,
  truncateText,
  __resetTestBridgeForTest,
} from '../testBridge'
import { useDevicesStore } from '../../stores/devices'
import { useTransfersStore } from '../../stores/transfers'
import { useToastStore } from '../../stores/toast'

const invokeMock = vi.mocked(invoke)

function makeRouter(): Router {
  return createRouter({
    history: createMemoryHistory(),
    routes: [
      { path: '/', component: { render: () => null } },
      { path: '/settings', component: { render: () => null } },
    ],
  })
}

/** 模拟真实 Tauri 注入（无 mock 标记，与 logBridge 测试同款口径） */
function fakeTauriInternals() {
  ;(window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ = { invoke: vi.fn() }
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

/** jsdom 无布局：手工点亮 offsetParent/getClientRects 模拟"布局可见" */
function makeVisible(el: Element, rect = { x: 10, y: 20, width: 100, height: 30 }) {
  Object.defineProperty(el, 'offsetParent', {
    value: document.body,
    configurable: true,
  })
  ;(el as Element & { getClientRects: () => DOMRectList }).getClientRects = () =>
    [{ width: rect.width }] as unknown as DOMRectList
  ;(el as Element & { getBoundingClientRect: () => DOMRect }).getBoundingClientRect = () =>
    ({ ...rect, top: rect.y, left: rect.x, right: rect.x + rect.width, bottom: rect.y + rect.height, toJSON: () => ({}) }) as unknown as DOMRect
}

/** 造一个可交互元素并挂到 body */
function mountInteractive(html: string): HTMLElement {
  const host = document.createElement('div')
  host.innerHTML = html
  document.body.appendChild(host)
  return host
}

/** 取 exec 回包（invoke('test_bridge_result', {id, ok, payload}) 的调用记录） */
function resultCall(): { id: number; ok: boolean; payload: unknown } | undefined {
  const call = invokeMock.mock.calls.find((c) => c[0] === 'test_bridge_result')
  if (!call) return undefined
  return call[1] as { id: number; ok: boolean; payload: unknown }
}

async function waitResult(): Promise<{ id: number; ok: boolean; payload: unknown }> {
  await vi.waitFor(() => {
    if (!resultCall()) throw new Error('回包未到')
  })
  return resultCall()!
}

beforeEach(() => {
  vi.clearAllMocks()
  __resetTestBridgeForTest()
  clearInternals()
  vi.unstubAllEnvs()
  document.body.innerHTML = ''
})

afterEach(() => {
  __resetTestBridgeForTest()
  clearInternals()
  vi.unstubAllEnvs()
  vi.useRealTimers()
})

// ---------------------------------------------------------------------------
// 选择器翻译（纯函数）
// ---------------------------------------------------------------------------
describe('translateSelector', () => {
  it('普通 CSS 原样保留', () => {
    expect(translateSelector('#app .btn > button')).toBe('#app .btn > button')
    expect(translateSelector('button[data-testid="x"]')).toBe('button[data-testid="x"]')
  })

  it('[testid=x] 简写翻译为 data-testid 属性', () => {
    expect(translateSelector('[testid=nav-settings-link]')).toBe('[data-testid="nav-settings-link"]')
  })

  it('引号形式（含空格/特殊字符）', () => {
    expect(translateSelector('[testid="a b"]')).toBe('[data-testid="a b"]')
    expect(translateSelector("[testid='c-d']")).toBe('[data-testid="c-d"]')
  })

  it('^= 前缀匹配（动态列表项约定）', () => {
    expect(translateSelector('[testid^=transfer-item-]')).toBe('[data-testid^="transfer-item-"]')
  })

  it('$= *= ~= 等其他操作符同样翻译', () => {
    expect(translateSelector('[testid$=-btn]')).toBe('[data-testid$="-btn"]')
    expect(translateSelector('[testid*=card]')).toBe('[data-testid*="card"]')
  })

  it('嵌套组合：多个简写与普通选择器混排', () => {
    expect(translateSelector('[testid=transfer-item-1] [testid^=transfer-cancel] button')).toBe(
      '[data-testid="transfer-item-1"] [data-testid^="transfer-cancel"] button',
    )
    expect(translateSelector('div.card [testid=x] input[type=text]')).toBe(
      'div.card [data-testid="x"] input[type=text]',
    )
  })

  it('jsdom 实际可命中翻译结果（testid 简写 → 真元素）', () => {
    const host = mountInteractive('<button data-testid="nav-settings-link">设置</button>')
    expect(document.querySelector(translateSelector('[testid=nav-settings-link]'))).toBe(
      host.querySelector('button'),
    )
    host.remove()
  })
})

// ---------------------------------------------------------------------------
// tree 序列化契约（spec §7.1.4）
// ---------------------------------------------------------------------------
describe('collectTree', () => {
  it('收录可交互元素并输出契约字段', () => {
    const host = mountInteractive(`
      <button data-testid="send-btn">发送</button>
      <input data-testid="name-input" value="hello" />
      <a href="/x">链接</a>
      <div>纯文本不收录</div>
    `)
    for (const el of host.querySelectorAll('button, input, a')) makeVisible(el)

    const tree = collectTree()
    const tags = tree.map((e) => e.tag)
    expect(tags).toContain('button')
    expect(tags).toContain('input')
    expect(tags).toContain('a')
    expect(tags).not.toContain('div')

    const btn = tree.find((e) => e.testId === 'send-btn')!
    expect(btn).toMatchObject({
      tag: 'button',
      testId: 'send-btn',
      role: 'button',
      text: '发送',
      value: null,
      disabled: false,
      visible: true,
    })
    expect(btn.rect).toEqual([10, 20, 100, 30])

    const input = tree.find((e) => e.testId === 'name-input')!
    expect(input.value).toBe('hello')
    expect(input.role).toBe('textbox')
    host.remove()
  })

  it('不可见元素剪枝（无 offsetParent 且无 client rects，等价 display:none）', () => {
    const host = mountInteractive('<button data-testid="hidden-btn">x</button>')
    // 不调 makeVisible：jsdom 下两信号皆空 → 剪枝
    expect(collectTree().find((e) => e.testId === 'hidden-btn')).toBeUndefined()

    // 点亮后收录
    makeVisible(host.querySelector('button')!)
    expect(collectTree().find((e) => e.testId === 'hidden-btn')).toBeDefined()
    host.remove()
  })

  it('aria-hidden 子树剪枝', () => {
    const host = mountInteractive(`
      <div aria-hidden="true"><button data-testid="a11y-hidden-btn">x</button></div>
      <button data-testid="a11y-visible-btn">y</button>
    `)
    makeVisible(host.querySelector('[data-testid=a11y-hidden-btn]')!)
    makeVisible(host.querySelector('[data-testid=a11y-visible-btn]')!)
    const tree = collectTree()
    expect(tree.find((e) => e.testId === 'a11y-hidden-btn')).toBeUndefined()
    expect(tree.find((e) => e.testId === 'a11y-visible-btn')).toBeDefined()
    host.remove()
  })

  it('文本截断 80 字符', () => {
    const long = '长'.repeat(100)
    const host = mountInteractive(`<button data-testid="long-btn">${long}</button>`)
    const btn = host.querySelector('button')!
    makeVisible(btn)
    const entry = collectTree().find((e) => e.testId === 'long-btn')!
    expect(entry.text.length).toBe(80)
    expect(entry.text).toBe('长'.repeat(80))
    // 纯函数口径一致
    expect(truncateText(long)).toBe(long.slice(0, 80))
    host.remove()
  })

  it('隐式 role：checkbox/radio/link/combobox', () => {
    const host = mountInteractive(`
      <input type="checkbox" data-testid="cb" />
      <input type="radio" data-testid="rd" />
      <select data-testid="sel"><option>a</option></select>
      <a href="#nowhere">l</a>
      <span role="switch" data-testid="sw" tabindex="0"></span>
    `)
    for (const el of host.querySelectorAll('input, select, a, span')) makeVisible(el)
    const tree = collectTree()
    expect(tree.find((e) => e.testId === 'cb')!.role).toBe('checkbox')
    expect(tree.find((e) => e.testId === 'rd')!.role).toBe('radio')
    expect(tree.find((e) => e.testId === 'sel')!.role).toBe('combobox')
    expect(tree.find((e) => e.tag === 'a')!.role).toBe('link')
    // 显式 role 属性优先于隐式推断
    expect(tree.find((e) => e.testId === 'sw')!.role).toBe('switch')
    host.remove()
  })

  it('disabled 元素如实上报', () => {
    const host = mountInteractive('<button disabled data-testid="dis-btn">x</button>')
    makeVisible(host.querySelector('button')!)
    expect(collectTree().find((e) => e.testId === 'dis-btn')!.disabled).toBe(true)
    expect(describeElement(host.querySelector('button')!).disabled).toBe(true)
    host.remove()
  })
})

// ---------------------------------------------------------------------------
// input 动作：v-model 兼容序列
// ---------------------------------------------------------------------------
describe('input 动作（v-model 兼容）', () => {
  it('原生 setter + input/change 事件驱动真实 Vue 组件的 ref 更新', async () => {
    // v-model 在 input 上编译为 :value + @input 重读 value property，
    // 这里挂等价语义的真实组件验证桥的输入序列能驱动它
    const model = ref('init')
    const app: VueApp = createApp({
      setup: () => () =>
        h('input', {
          'data-testid': 'vm-input',
          value: model.value,
          onInput: (e: Event) => {
            model.value = (e.target as HTMLInputElement).value
          },
        }),
    })
    const container = document.createElement('div')
    document.body.appendChild(container)
    app.mount(container)
    makeVisible(container.querySelector('input')!)

    const router = makeRouter()
    const data = (await runAction(router, 'input', {
      selector: '[testid=vm-input]',
      value: '你好 e2e',
    })) as { value: string }

    expect(data.value).toBe('你好 e2e')
    // v-model（@input 重读 value）后 ref 已更新
    expect(model.value).toBe('你好 e2e')
    app.unmount()
    container.remove()
  })

  it('clear=true 先清空再写入；事件序列 input+change 均派发', async () => {
    const seen: string[] = []
    const host = mountInteractive('<input data-testid="ev-input" value="旧值" />')
    const input = host.querySelector('input')!
    input.addEventListener('input', () => seen.push('input'))
    input.addEventListener('change', () => seen.push('change'))
    makeVisible(input)

    const router = makeRouter()
    const data = (await runAction(router, 'input', {
      selector: '[testid=ev-input]',
      value: '新值',
      clear: true,
    })) as { value: string }
    expect(data.value).toBe('新值')
    expect(input.value).toBe('新值')
    expect(seen).toEqual(['input', 'change'])
    host.remove()
  })

  it('events 显式派发：blur/enter 触发保存流兜底（程序化设值无真实焦点）', async () => {
    const host = mountInteractive('<input data-testid="ev2-input" />')
    const input = host.querySelector('input')!
    const seen: string[] = []
    input.addEventListener('blur', () => seen.push('blur'))
    input.addEventListener('keyup', (e) => {
      if ((e as KeyboardEvent).key === 'Enter') seen.push('enter')
    })
    makeVisible(input)

    const router = makeRouter()
    await runAction(router, 'input', {
      selector: '[testid=ev2-input]',
      value: 'x',
      events: ['blur', 'enter'],
    })
    expect(seen).toEqual(['blur', 'enter'])

    await expect(
      runAction(makeRouter(), 'input', {
        selector: '[testid=ev2-input]',
        value: 'x',
        events: 'blur' as never,
      }),
    ).rejects.toThrow()
    host.remove()
  })

  it('setNativeValue 走原型 setter（Vue 包装不拦截）', () => {
    const host = mountInteractive('<input />')
    const input = host.querySelector('input')!
    setNativeValue(input, 'proto')
    expect(input.value).toBe('proto')
    // textarea 同理
    const ta = document.createElement('textarea')
    setNativeValue(ta, 'multi\nline')
    expect(ta.value).toBe('multi\nline')
    host.remove()
  })

  it('目标非输入控件 → BAD_REQUEST', async () => {
    const host = mountInteractive('<button data-testid="not-input">b</button>')
    const err = await runAction(makeRouter(), 'input', {
      selector: '[testid=not-input]',
      value: 'x',
    }).then(
      () => null,
      (e) => e,
    )
    expect(err?.code).toBe('BAD_REQUEST')
    host.remove()
  })

  it('value 非字符串 → BAD_REQUEST；缺 value → BAD_REQUEST', async () => {
    const host = mountInteractive('<input data-testid="i" />')
    for (const params of [{ selector: '[testid=i]', value: 42 }, { selector: '[testid=i]' }]) {
      const err = await runAction(makeRouter(), 'input', params).then(
        () => null,
        (e) => e,
      )
      expect(err?.code).toBe('BAD_REQUEST')
    }
    host.remove()
  })
})

// ---------------------------------------------------------------------------
// click / text / navigate 动作
// ---------------------------------------------------------------------------
describe('click 与 text 动作', () => {
  it('click：派发冒泡 click 并回传元素文本', async () => {
    let clicked = 0
    const host = mountInteractive('<button data-testid="go-btn">开始传输</button>')
    const btn = host.querySelector('button')!
    btn.addEventListener('click', () => clicked++)
    makeVisible(btn)

    const data = (await runAction(makeRouter(), 'click', {
      selector: '[testid=go-btn]',
    })) as { text: string }
    expect(clicked).toBe(1)
    expect(data.text).toBe('开始传输')
    host.remove()
  })

  it('click：元素不存在（短轮询 1s 后）→ ELEMENT_NOT_FOUND', async () => {
    vi.useFakeTimers()
    const p = runAction(makeRouter(), 'click', { selector: '[testid=ghost]' }).then(
      () => null,
      (e) => e,
    )
    // 推过 1s 短轮询窗口
    await vi.advanceTimersByTimeAsync(1200)
    const err = await p
    expect(err?.code).toBe('ELEMENT_NOT_FOUND')
  })

  it('click：短轮询容忍渲染间隙（600ms 后元素出现 → 命中）', async () => {
    vi.useFakeTimers()
    const p = runAction(makeRouter(), 'click', { selector: '[testid=lazy-btn]' })
    // 模拟异步渲染：600ms 后插入元素
    setTimeout(() => {
      const host = mountInteractive('<button data-testid="lazy-btn">晚了</button>')
      makeVisible(host.querySelector('button')!)
    }, 600)
    const data = (await vi.advanceTimersByTimeAsync(700).then(() => p)) as { text: string }
    expect(data.text).toBe('晚了')
  })

  it('text：缺省全文（textContent 回退），指定 selector 取元素文本', async () => {
    const host = mountInteractive(`
      <div data-testid="panel">面板 文本</div>
      <span>其他</span>
    `)
    const router = makeRouter()
    const all = (await runAction(router, 'text', {})) as string
    expect(all).toContain('面板')
    expect(all).toContain('其他')

    const part = (await runAction(router, 'text', { selector: '[testid=panel]' })) as string
    expect(part).toBe('面板 文本')
    host.remove()
  })

  it('navigate：router.push 完成后回传当前路径；非法路径 → BAD_REQUEST', async () => {
    const router = makeRouter()
    await router.push('/')
    const data = (await runAction(router, 'navigate', { path: '/settings' })) as { path: string }
    expect(data.path).toBe('/settings')
    expect(router.currentRoute.value.path).toBe('/settings')

    for (const bad of [{ path: 'settings' }, { path: 'http://x' }, { path: '' }, {}]) {
      const err = await runAction(router, 'navigate', bad).then(
        () => null,
        (e) => e,
      )
      expect(err?.code).toBe('BAD_REQUEST')
    }
  })
})

// ---------------------------------------------------------------------------
// wait 动作（fake timers）
// ---------------------------------------------------------------------------
describe('wait 动作', () => {
  it('selector 命中：元素异步插入后立即返回', async () => {
    vi.useFakeTimers()
    const p = runAction(makeRouter(), 'wait', { selector: '[testid=later]', timeoutMs: 5000 })
    setTimeout(() => {
      const host = mountInteractive('<div data-testid="later">到</div>')
      makeVisible(host.querySelector('div')!)
    }, 200)
    const data = (await vi.advanceTimersByTimeAsync(300).then(() => p)) as { matched: boolean }
    expect(data.matched).toBe(true)
  })

  it('text 命中：页面文本出现后返回', async () => {
    vi.useFakeTimers()
    const p = runAction(makeRouter(), 'wait', { text: '共享区', timeoutMs: 5000 })
    setTimeout(() => {
      mountInteractive('<h2>共享区</h2>')
    }, 150)
    const data = (await vi.advanceTimersByTimeAsync(250).then(() => p)) as { matched: boolean }
    expect(data.matched).toBe(true)
  })

  it('已存在时同步命中（不依赖定时器）', async () => {
    mountInteractive('<div data-testid="now">n</div>')
    const data = (await runAction(makeRouter(), 'wait', { selector: '[testid=now]' })) as {
      matched: boolean
    }
    expect(data.matched).toBe(true)
  })

  it('超时 → WAIT_TIMEOUT（detail 带 selector）', async () => {
    vi.useFakeTimers()
    const p = runAction(makeRouter(), 'wait', {
      selector: '[testid=never]',
      timeoutMs: 300,
    }).then(
      () => null,
      (e) => e,
    )
    await vi.advanceTimersByTimeAsync(500)
    const err = await p
    expect(err?.code).toBe('WAIT_TIMEOUT')
    expect(err?.message).toContain('[testid=never]')
  })

  it('参数互斥与缺失 → BAD_REQUEST', async () => {
    for (const bad of [
      { selector: '#a', text: 'x' },
      {},
      { selector: '  ' },
    ]) {
      const err = await runAction(makeRouter(), 'wait', bad).then(
        () => null,
        (e) => e,
      )
      expect(err?.code).toBe('BAD_REQUEST')
    }
  })
})

// ---------------------------------------------------------------------------
// state 动作（Task M3：四 store 聚合快照）
// ---------------------------------------------------------------------------
describe('state 动作', () => {
  it('聚合四 store 快照，键齐全且可序列化', async () => {
    setActivePinia(createPinia())
    const devices = useDevicesStore()
    devices.devices = [
      { fingerprint: 'ab'.repeat(32), name: 'A 机', addr: '1.2.3.4:47601', online: true, connected: true },
    ]
    const transfers = useTransfersStore()
    transfers.transfers = [
      { job_id: '1', name: 'f.bin', total: 10, done: 5, state: 'active', speed_bps: 1,
        peer: 'ab'.repeat(32), direction: 'pull', local_role: 'destination', health: null,
        started_at_ms: 1 } as any,
    ]
    const toast = useToastStore()
    toast.push('success', '已连接')

    const data = (await runAction(makeRouter(), 'state', {})) as Record<string, any>
    expect(data.schemaVersion).toBe(1)
    for (const key of ['devices', 'transfers', 'settings', 'toast']) {
      expect(data[key], `聚合应含 ${key}`).toBeTypeOf('object')
      expect(data[key].schemaVersion).toBe(1)
    }
    // 值真实来自 store（非空壳）
    expect(data.devices.devices[0].name).toBe('A 机')
    expect(data.transfers.transfers[0].state).toBe('active')
    expect(data.toast.toasts[0].text).toBe('已连接')
    // 纯 JSON 可序列化（Rust 侧要整体回传 HTTP）
    expect(() => JSON.stringify(data)).not.toThrow()
  })

  it('无 pinia（未传参也无 active）→ INTERNAL', async () => {
    // setActivePinia(null) 清掉 active pinia
    setActivePinia(null as never)
    const err = (await runAction(makeRouter(), 'state', {}).then(
      () => null,
      (e) => e,
    )) as { code: string }
    expect(err?.code).toBe('INTERNAL')
  })

  it('显式传入 pinia 优先于 active', async () => {
    const first = createPinia()
    setActivePinia(first)
    const devices = useDevicesStore(first)
    devices.devices = [
      { fingerprint: '11'.repeat(32), name: '显式实例', addr: '5.6.7.8:47601', online: false, connected: false },
    ]
    const data = (await runAction(makeRouter(), 'state', {}, first)) as Record<string, any>
    expect(data.devices.devices[0].name).toBe('显式实例')
  })
})

// ---------------------------------------------------------------------------
// 注册门控与 exec 回包
// ---------------------------------------------------------------------------
describe('setupTestBridge 门控', () => {
  it('默认环境（无 VITE_TEST_API，即正常构建语义）完全不注册', () => {
    fakeTauriInternals()
    setupTestBridge(makeRouter())
    expect(window.__testBridge).toBeUndefined()
    expect(invokeMock).not.toHaveBeenCalled()
  })

  it('浏览器 mock 注入（tauriMock 标记）不注册', () => {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeBrowserMockInternals()
    setupTestBridge(makeRouter())
    expect(window.__testBridge).toBeUndefined()
    expect(invokeMock).not.toHaveBeenCalled()
  })

  it('测试模式 + 真实 Tauri → 注册并握手 hello', () => {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeTauriInternals()
    setupTestBridge(makeRouter())
    expect(window.__testBridge?.exec).toBeTypeOf('function')
    expect(invokeMock).toHaveBeenCalledWith('test_bridge_hello')
  })

  it('重复 setup 幂等', () => {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeTauriInternals()
    const router = makeRouter()
    setupTestBridge(router)
    const first = window.__testBridge
    setupTestBridge(router)
    expect(window.__testBridge).toBe(first)
    expect(invokeMock.mock.calls.filter((c) => c[0] === 'test_bridge_hello').length).toBe(1)
  })
})

describe('exec 回包', () => {
  function execWith(json: string) {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeTauriInternals()
    const router = makeRouter()
    setupTestBridge(router)
    window.__testBridge!.exec(json)
    return router
  }

  it('合法请求：执行动作并经 test_bridge_result 回传 ok', async () => {
    mountInteractive('<button data-testid="hello-btn">你好</button>')
    makeVisible(document.querySelector('[data-testid=hello-btn]')!)
    execWith(JSON.stringify({ id: 42, action: 'text', params: { selector: '[testid=hello-btn]' } }))
    const r = await waitResult()
    expect(r.id).toBe(42)
    expect(r.ok).toBe(true)
    expect(r.payload).toBe('你好')
  })

  it('未知 action → 回传 BAD_REQUEST 包络', async () => {
    execWith(JSON.stringify({ id: 7, action: 'screenshot', params: {} }))
    const r = await waitResult()
    expect(r.id).toBe(7)
    expect(r.ok).toBe(false)
    expect(r.payload).toMatchObject({ code: 'BAD_REQUEST' })
  })

  it('非法 JSON：无 id 可回配 → 静默丢弃（不 invoke）', async () => {
    execWith('{not json')
    await new Promise((r) => setTimeout(r, 10))
    expect(invokeMock.mock.calls.filter((c) => c[0] === 'test_bridge_result')).toHaveLength(0)
  })

  it('params 缺省按空对象处理', async () => {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeTauriInternals()
    setupTestBridge(makeRouter())
    window.__testBridge!.exec(JSON.stringify({ id: 9, action: 'text' }))
    const r = await waitResult()
    expect(r.id).toBe(9)
    expect(r.ok).toBe(true)
    expect(typeof r.payload).toBe('string')
  })
})

describe('parseBridgeRequest 载荷形态', () => {
  it('契约形态：JSON 字符串（Rust 侧字符串字面量嵌入 eval）', () => {
    const req = parseBridgeRequest('{"id":1,"action":"text","params":{}}')
    expect(req).toEqual({ id: 1, action: 'text', params: {} })
  })

  it('容错形态：已求值对象（上游误嵌对象字面量时 exec 直接采用）', () => {
    const req = parseBridgeRequest({ id: 2, action: 'tree' } as unknown as string)
    expect(req).toEqual({ id: 2, action: 'tree' })
  })

  it('非法载荷 → null（字符串解析失败 / 基本类型 / null）', () => {
    expect(parseBridgeRequest('{not json')).toBeNull()
    expect(parseBridgeRequest('42')).toBeNull()
    expect(parseBridgeRequest('null')).toBeNull()
    expect(parseBridgeRequest(7)).toBeNull()
    expect(parseBridgeRequest(null)).toBeNull()
  })

  it('exec 对对象载荷同样回包（端到端容错）', async () => {
    vi.stubEnv('VITE_TEST_API', '1')
    fakeTauriInternals()
    setupTestBridge(makeRouter())
    window.__testBridge!.exec({ id: 33, action: 'text' } as unknown as string)
    const r = await waitResult()
    expect(r.id).toBe(33)
    expect(r.ok).toBe(true)
    expect(typeof r.payload).toBe('string')
  })
})
