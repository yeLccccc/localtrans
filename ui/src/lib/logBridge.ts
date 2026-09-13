/**
 * Task M1: 前端 console/错误日志桥（spec §7.1.7-2）
 *
 * 仅在真实 Tauri WebView 内激活：拦截 console.warn/error（保留原行为再上报）、
 * app.config.errorHandler、window unhandledrejection，50ms 批量聚合走
 * invoke('ui_log', {level, message, route})，由 Rust 侧以 target="ui" 写 tracing
 * （正式版落滚动文件，test-api 构建额外进 /api/logs/tail 环形缓冲）。
 *
 * 环境 detection 与项目现有习惯保持一致（tauriMock 的探测口径）：
 * window.__TAURI_INTERNALS__ 存在且非浏览器 mock 注入的实例。浏览器预览与
 * vitest/jsdom 环境完全 no-op（不 patch console、不注册任何监听）。
 */

import type { Plugin, App } from 'vue'
import type { Router } from 'vue-router'
import { invoke } from '@tauri-apps/api/core'

/** 单条消息截断长度（防一条超长错误撑爆 IPC 与环形缓冲） */
const MAX_MESSAGE_LENGTH = 500

/** 批量聚合窗口：攒数组，定时器到点 flush */
const FLUSH_INTERVAL_MS = 50

type LogLevel = 'info' | 'warn' | 'error'

interface PendingEntry {
  level: LogLevel
  message: string
  route: string | null
}

// 模块级状态（应用单实例，install 幂等）
let installed = false
let pending: PendingEntry[] = []
let flushTimer: ReturnType<typeof setTimeout> | null = null
let currentRoute: string | null = null
let rawConsoleWarn: typeof console.warn | null = null
let rawConsoleError: typeof console.error | null = null
let rejectListener: ((e: PromiseRejectionEvent) => void) | null = null
let visibilityListener: (() => void) | null = null

/**
 * 是否真实 Tauri 环境。探测口径与 tauriMock 一致（window.__TAURI_INTERNALS__），
 * 并排除浏览器 mock 注入的同形对象（tauriMock 会打 __localtransMock 标记），
 * 因此与 main.ts 里 installTauriMockIfBrowser 的调用顺序无关。
 */
export function isTauriEnv(): boolean {
  if (typeof window === 'undefined') return false
  const internals = (window as unknown as Record<string, unknown>).__TAURI_INTERNALS__ as
    | Record<string, unknown>
    | undefined
  return !!internals && !internals.__localtransMock
}

/** 任意 console 参数 → 单行字符串（Error 取名字+消息，对象尽力 JSON 化） */
function formatArgs(args: unknown[]): string {
  return args
    .map((a) => {
      if (typeof a === 'string') return a
      if (a instanceof Error) return `${a.name || 'Error'}: ${a.message}`
      try {
        const s = JSON.stringify(a)
        return s === undefined ? String(a) : s
      } catch {
        return String(a)
      }
    })
    .join(' ')
}

function truncate(s: string): string {
  return s.length <= MAX_MESSAGE_LENGTH ? s : `${s.slice(0, MAX_MESSAGE_LENGTH)}…`
}

function enqueue(level: LogLevel, message: string): void {
  if (!installed) return
  pending.push({ level, message: truncate(message), route: currentRoute })
  if (flushTimer === null) {
    flushTimer = setTimeout(flush, FLUSH_INTERVAL_MS)
  }
}

/** 定时器到点（或页面隐藏前）批量上报。失败静默——日志桥不能成为新错误源。 */
async function flush(): Promise<void> {
  flushTimer = null
  if (pending.length === 0) return
  const batch = pending
  pending = []
  for (const e of batch) {
    try {
      await invoke('ui_log', { level: e.level, message: e.message, route: e.route })
    } catch {
      // 静默丢弃：ui_log 不可用（如正式版未注册的意外情况）不影响 UI
    }
  }
}

function installConsoleCapture(): void {
  rawConsoleWarn = console.warn
  rawConsoleError = console.error
  // 保留原行为再上报：先透传原始 console，再入队
  console.warn = (...args: unknown[]) => {
    enqueue('warn', formatArgs(args))
    rawConsoleWarn?.(...args)
  }
  console.error = (...args: unknown[]) => {
    enqueue('error', formatArgs(args))
    rawConsoleError?.(...args)
  }
}

/**
 * Vue 插件形式接入（main.ts: app.use(createLogBridge(router))）。
 * 非 Tauri 环境 install 直接返回，零副作用。
 */
export function createLogBridge(router: Router): Plugin {
  return {
    install(app: App) {
      if (installed || !isTauriEnv()) return
      installed = true

      // 1) 当前路由由 router.afterEach 维护（如 "/settings"）
      router.afterEach((to) => {
        currentRoute = to.path
      })

      // 2) console.warn/error 拦截（保留原行为）
      installConsoleCapture()

      // 3) Vue 渲染/生命周期错误：交给（已被桥捕获的）console.error 输出，
      //    既保留默认可见性又只上报一次；已有 handler 链式保留
      const prevHandler = app.config.errorHandler
      app.config.errorHandler = (err, instance, info) => {
        const opts = instance?.$options as { name?: string; __name?: string } | undefined
        const compName = opts?.name || opts?.__name
        const prefix = compName ? `[Vue ${info}] <${compName}>` : `[Vue ${info}]`
        // 走被拦截的 console.error → 上报 + 原始输出
        console.error(prefix, err)
        prevHandler?.(err, instance, info)
      }

      // 4) 未处理的 Promise 拒绝
      rejectListener = (e: PromiseRejectionEvent) => {
        enqueue('error', `unhandledrejection: ${formatArgs([e.reason])}`)
      }
      window.addEventListener('unhandledrejection', rejectListener)

      // 5) 页面隐藏前尽力 flush 一次（窗口关闭前的尾巴）
      visibilityListener = () => {
        if (document.visibilityState === 'hidden') void flush()
      }
      document.addEventListener('visibilitychange', visibilityListener)

      // M1 冒烟锚点：attach 成功立即上报一条 info
      // （tests/e2e/smoke-m1.mjs 靠它确认桥已通，比制造 error 可控）
      enqueue('info', 'logBridge attached')
    },
  }
}

/** 仅测试用：还原所有补丁与模块状态（vitest 每用例隔离） */
export function __resetLogBridgeForTest(): void {
  if (rawConsoleWarn !== null) console.warn = rawConsoleWarn
  if (rawConsoleError !== null) console.error = rawConsoleError
  rawConsoleWarn = null
  rawConsoleError = null
  if (rejectListener !== null) {
    window.removeEventListener('unhandledrejection', rejectListener)
    rejectListener = null
  }
  if (visibilityListener !== null) {
    document.removeEventListener('visibilitychange', visibilityListener)
    visibilityListener = null
  }
  if (flushTimer !== null) {
    clearTimeout(flushTimer)
    flushTimer = null
  }
  pending = []
  currentRoute = null
  installed = false
}
