/**
 * Task M2: 前端测试通道桥（spec §7.1.4 UI Bridge 机制）
 *
 * 解决 webview.eval 单向性的回配侧：Rust HTTP 端 eval
 * `window.__testBridge.exec(<JSON>)` 触发动作，本模块执行后
 * `invoke('test_bridge_result', {id, ok, payload})` 把结果送回，
 * Rust 侧按 id 唤醒对应 oneshot（src-tauri/src/test_api/bridge.rs）。
 *
 * 双重门控（缺一不注册，零副作用）：
 * - 构建期：vite `--mode test-api`（ui/.env.test-api 的 VITE_TEST_API=1）。
 *   main.ts 以 `import.meta.env.VITE_TEST_API === '1'` 动态 import 本模块，
 *   正式构建整段（含本文件）被 rollup 剪除，dist 无 __testBridge 特征；
 * - 运行期：真实 Tauri WebView（复用 logBridge 的探测口径，排除 tauriMock
 *   注入浏览器的实例），vitest/jsdom/浏览器 mock 模式完全不注册。
 */

import type { Router } from 'vue-router'
import { invoke } from '@tauri-apps/api/core'
import { getActivePinia, type Pinia } from 'pinia'
import { isTauriEnv } from '../lib/logBridge'
import { useDevicesStore } from '../stores/devices'
import { useTransfersStore } from '../stores/transfers'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'

/** tree 序列化契约：文本截断上限（spec §7.1.4） */
const MAX_TEXT_LENGTH = 80

/** 元素查找短轮询窗口：容忍渲染间隙（Vue 异步挂载/路由切换） */
const FIND_POLL_TIMEOUT_MS = 1000

/** 短轮询间隔 */
const FIND_POLL_INTERVAL_MS = 50

/** wait 动作缺省超时（spec §7.1.3 /api/ui/wait） */
const WAIT_DEFAULT_TIMEOUT_MS = 10000

/** wait 轮询间隔（配合 MutationObserver 兜底，spec §7.1.5） */
const WAIT_POLL_INTERVAL_MS = 100

/** tree 收集的可交互元素集合（spec §7.1.4） */
const INTERACTIVE_SELECTOR = 'button, input, select, textarea, a, [role], [data-testid]'

/** 桥错误：Rust 侧按 code 映射 HTTP 状态（ELEMENT_NOT_FOUND→404 等，封闭集合） */
export interface BridgeError {
  code: 'BAD_REQUEST' | 'ELEMENT_NOT_FOUND' | 'WAIT_TIMEOUT' | 'INTERNAL'
  message: string
}

/** tree 单元素摘要（spec §7.1.4 序列化契约） */
export interface TreeElement {
  tag: string
  testId: string | null
  role: string | null
  text: string
  value: string | null
  disabled: boolean
  visible: boolean
  rect: [number, number, number, number]
}

/** 桥请求（Rust 侧 eval 注入的 JSON） */
interface BridgeRequest {
  id: number
  action: string
  params: Record<string, unknown>
}

/**
 * Task M3: /api/state/app 聚合快照（spec §7.1.6 Pinia 侧）。
 * 四个 store 的 toTestSnapshot() 拼装；各 store 内层结构见
 * docs/contracts/test-api.md §5.3。
 */
export interface AppTestSnapshot {
  schemaVersion: number
  devices: Record<string, unknown>
  transfers: Record<string, unknown>
  settings: Record<string, unknown>
  toast: Record<string, unknown>
}

declare global {
  interface Window {
    __testBridge?: { exec: (jsonString: string) => void }
  }
}

// ---------------------------------------------------------------------------
// 选择器语义（spec §7.1.3）：CSS 直用 + `[testid=x]` 简写 + `^=` 前缀匹配
// ---------------------------------------------------------------------------

/**
 * 把 `[testid(空|^|$|*|~|~)=value]` 简写翻译为 `[data-testid(...)="value"]`。
 * 纯函数（vitest 直测）；其余 CSS 原样保留，支持嵌套组合：
 *   `[testid=transfer-item-1] [testid^=transfer-cancel]` →
 *   `[data-testid="transfer-item-1"] [data-testid^="transfer-cancel"]`
 * value 支持裸串 / 双引号 / 单引号三种写法。
 */
export function translateSelector(selector: string): string {
  const re = /\[testid\s*([~^$*|]?)=\s*(?:"([^"]*)"|'([^']*)'|([^\]\s]+))\s*\]/g
  return selector.replace(re, (_m, op: string, dq?: string, sq?: string, bare?: string) => {
    const value = dq ?? sq ?? bare ?? ''
    return `[data-testid${op}="${value}"]`
  })
}

// ---------------------------------------------------------------------------
// 元素可见性 / 描述（tree 契约）
// ---------------------------------------------------------------------------

/**
 * 布局可见（spec §7.1.4 以 offsetParent==null 为剪枝信号）。
 * 例外：position:fixed 元素 offsetParent 也为 null（本项目底部导航就是
 * fixed 布局），用 getClientRects 兜底——display:none 子树两种信号皆空，
 * 可见 fixed 元素有 client rects。
 */
export function isLayoutVisible(el: Element): boolean {
  return (
    (el as HTMLElement).offsetParent !== null || el.getClientRects().length > 0
  )
}

/** 元素自身或祖先 aria-hidden="true" → 剪枝（spec §7.1.4） */
function isAriaHidden(el: Element): boolean {
  return el.closest('[aria-hidden="true"]') !== null
}

/** 元素可读文本：innerText 优先（保留渲染语义），jsdom 等无实现时回退 textContent；空白折叠 */
function elementText(el: Element): string {
  const inner = (el as HTMLElement).innerText
  const raw = typeof inner === 'string' && inner.length > 0 ? inner : (el.textContent ?? '')
  return raw.replace(/\s+/g, ' ').trim()
}

/** 截断到契约上限 */
export function truncateText(s: string, max = MAX_TEXT_LENGTH): string {
  return s.length <= max ? s : s.slice(0, max)
}

/** 隐式 ARIA role（tree 契约的 role 字段：显式 role 属性优先） */
function implicitRole(el: Element): string | null {
  switch (el.tagName.toLowerCase()) {
    case 'button':
      return 'button'
    case 'a':
      return el instanceof HTMLAnchorElement && el.hasAttribute('href') ? 'link' : null
    case 'select':
      return 'combobox'
    case 'textarea':
      return 'textbox'
    case 'input': {
      const type = (el as HTMLInputElement).type
      switch (type) {
        case 'checkbox':
          return 'checkbox'
        case 'radio':
          return 'radio'
        case 'range':
          return 'slider'
        case 'button':
        case 'submit':
        case 'reset':
          return 'button'
        default:
          return 'textbox'
      }
    }
    default:
      return null
  }
}

/** 表单控件的 value，其余元素 null */
function elementValue(el: Element): string | null {
  if (
    el instanceof HTMLInputElement ||
    el instanceof HTMLTextAreaElement ||
    el instanceof HTMLSelectElement
  ) {
    return el.value
  }
  return null
}

/** 表单控件 disabled，或 aria-disabled="true" */
function elementDisabled(el: Element): boolean {
  if (
    el instanceof HTMLInputElement ||
    el instanceof HTMLTextAreaElement ||
    el instanceof HTMLSelectElement ||
    el instanceof HTMLButtonElement
  ) {
    return el.disabled
  }
  return el.getAttribute('aria-disabled') === 'true'
}

/** 单元素摘要（tree 序列化契约，spec §7.1.4） */
export function describeElement(el: Element): TreeElement {
  const r = el.getBoundingClientRect()
  return {
    tag: el.tagName.toLowerCase(),
    testId: el.getAttribute('data-testid'),
    role: el.getAttribute('role') ?? implicitRole(el),
    text: truncateText(elementText(el)),
    value: elementValue(el),
    disabled: elementDisabled(el),
    visible: true,
    rect: [r.x, r.y, r.width, r.height].map(Math.round) as [number, number, number, number],
  }
}

/**
 * 遍历可交互元素输出摘要数组（/api/ui/tree 数据面）。
 * 剪枝：不可见（isLayoutVisible）与 aria-hidden 子树。
 */
export function collectTree(root: ParentNode = document): TreeElement[] {
  const out: TreeElement[] = []
  for (const el of Array.from(root.querySelectorAll(INTERACTIVE_SELECTOR))) {
    if (isAriaHidden(el) || !isLayoutVisible(el)) continue
    out.push(describeElement(el))
  }
  return out
}

// ---------------------------------------------------------------------------
// 动作实现
// ---------------------------------------------------------------------------

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms))

/** 桥错误构造（code ∈ 封闭集合，Rust 侧据此映射 HTTP 状态） */
function bridgeError(
  code: BridgeError['code'],
  message: string,
): BridgeError & { __bridgeError: true } {
  return { code, message, __bridgeError: true }
}

function isBridgeError(e: unknown): e is BridgeError {
  return typeof e === 'object' && e !== null && 'code' in e && 'message' in e
}

function toBridgeError(e: unknown): BridgeError {
  if (isBridgeError(e)) return { code: e.code, message: e.message }
  const msg = e instanceof Error ? e.message : String(e)
  return { code: 'INTERNAL', message: msg }
}

/**
 * 元素查找：先查再短轮询（≤1s）容忍渲染间隙，仍无匹配 → ELEMENT_NOT_FOUND。
 */
async function findElement(selector: string): Promise<HTMLElement> {
  const css = translateSelector(selector)
  const deadline = Date.now() + FIND_POLL_TIMEOUT_MS
  for (;;) {
    const el = document.querySelector(css)
    if (el) return el as HTMLElement
    if (Date.now() >= deadline) {
      throw bridgeError('ELEMENT_NOT_FOUND', `选择器无匹配: ${selector}`)
    }
    await sleep(FIND_POLL_INTERVAL_MS)
  }
}

/** 非空字符串参数提取（trim 后为空视为缺失） */
function requiredString(params: Record<string, unknown>, key: string): string {
  const v = params[key]
  if (typeof v !== 'string' || v.trim() === '') {
    throw bridgeError('BAD_REQUEST', `参数 ${key} 必须为非空字符串`)
  }
  return v
}

/** navigate：router.push 并等待导航完成 */
async function doNavigate(router: Router, params: Record<string, unknown>): Promise<unknown> {
  const path = requiredString(params, 'path')
  if (!path.startsWith('/')) {
    throw bridgeError('BAD_REQUEST', `path 必须以 / 开头（应用内路由路径），收到: ${path}`)
  }
  await router.push(path)
  return { path: router.currentRoute.value.path }
}

/** click：派发冒泡 click，回传元素文本（spec §7.1.3） */
async function doClick(params: Record<string, unknown>): Promise<unknown> {
  const selector = requiredString(params, 'selector')
  const el = await findElement(selector)
  // 不带 view：jsdom 对 view 成员做 Window 类型校验会拒（见 vitest），
  // webview 侧点击处理也不依赖它
  el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }))
  return { text: truncateText(elementText(el), 200) }
}

/**
 * toggle：checkbox/radio 专用——合成 click 不触发浏览器 label→input 的
 * 激活转发（原生行为），对 label 派发 click 永远不改 checked（force-relay
 * 场景实证）。此动作找到 input（自身或内部）翻转 checked 并派发 change。
 */
async function doToggle(params: Record<string, unknown>): Promise<unknown> {
  const selector = requiredString(params, 'selector')
  const el = await findElement(selector)
  const input =
    el instanceof HTMLInputElement
      ? el
      : el.querySelector<HTMLInputElement>('input[type="checkbox"], input[type="radio"]')
  if (!input) throw new Error(`toggle 目标无 checkbox: ${selector}`)
  // 原生 click() 触发完整激活链:label 转发→input.checked 翻转→change 事件→
  // Vue @change 处理器→store/invoke 全链(手改 checked+dispatch change 不走
  // 浏览器激活语义,Vue 侧数据流断裂,实测 config 不落盘)
  input.click()
  return { checked: input.checked }
}

/**
 * input：原生 value setter（绕过 Vue 对 property 的包装）+ 派发 input/change，
 * 保证 v-model 响应（spec §7.1.4）。input/textarea/select 同一序列。
 */
async function doInput(params: Record<string, unknown>): Promise<unknown> {
  const selector = requiredString(params, 'selector')
  const value = params.value
  if (typeof value !== 'string') {
    throw bridgeError('BAD_REQUEST', '参数 value 必须为字符串（清空请传空串）')
  }
  const clear = params.clear === true
  const el = await findElement(selector)
  if (
    !(el instanceof HTMLInputElement) &&
    !(el instanceof HTMLTextAreaElement) &&
    !(el instanceof HTMLSelectElement)
  ) {
    throw bridgeError('BAD_REQUEST', `目标不是输入控件（${el.tagName}）: ${selector}`)
  }
  if (clear) setNativeValue(el, '')
  setNativeValue(el, value)
  el.dispatchEvent(new Event('input', { bubbles: true }))
  el.dispatchEvent(new Event('change', { bubbles: true }))
  // events：显式追加派发（程序化设值不产生真实焦点，@blur/@keyup.enter 类
  // 保存流必须显式派发；'enter' 展开为 keydown+keyup，blur/focus 不冒泡）
  const events = params.events
  if (events !== undefined) {
    if (!Array.isArray(events) || !events.every((e) => typeof e === 'string')) {
      throw bridgeError('BAD_REQUEST', '参数 events 必须为字符串数组')
    }
    for (const ev of events) {
      if (ev === 'enter') {
        el.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }))
        el.dispatchEvent(new KeyboardEvent('keyup', { key: 'Enter', bubbles: true }))
      } else if (ev === 'blur' || ev === 'focus') {
        el.dispatchEvent(new FocusEvent(ev))
      } else {
        el.dispatchEvent(new Event(ev, { bubbles: true }))
      }
    }
  }
  return { value: el.value }
}

/**
 * 原型链上的原生 value setter。Vue 的 v-model 通过监听 input 事件重读
 * value property 生效，若直接 el.value = x 赋值在某些封装下不触发，
 * 必须走 HTMLInputElement/HTMLTextAreaElement/HTMLSelectElement 原型 setter。
 */
export function setNativeValue(
  el: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement,
  value: string,
): void {
  const proto =
    el instanceof HTMLTextAreaElement
      ? HTMLTextAreaElement.prototype
      : el instanceof HTMLSelectElement
        ? HTMLSelectElement.prototype
        : HTMLInputElement.prototype
  const setter = Object.getOwnPropertyDescriptor(proto, 'value')?.set
  if (setter) {
    setter.call(el, value)
  } else {
    // 兜底（理论不可达）：直接赋值
    el.value = value
  }
}

/** text：缺省整个 body 文本，指定 selector 则取该元素文本 */
async function doText(params: Record<string, unknown>): Promise<string> {
  const selector = typeof params.selector === 'string' ? params.selector.trim() : ''
  if (selector === '') {
    const raw = (document.body as HTMLElement).innerText
    return typeof raw === 'string' && raw.length > 0 ? raw : (document.body.textContent ?? '')
  }
  const el = await findElement(selector)
  return elementText(el)
}

/** 页面文本匹配源（wait 的 text 条件） */
function bodyText(): string {
  const raw = (document.body as HTMLElement).innerText
  return typeof raw === 'string' && raw.length > 0 ? raw : (document.body.textContent ?? '')
}

/**
 * wait：MutationObserver + 100ms 轮询双通道（spec §7.1.5），
 * 命中即回；超时回 WAIT_TIMEOUT。
 */
async function doWait(params: Record<string, unknown>): Promise<unknown> {
  const selector = typeof params.selector === 'string' ? params.selector.trim() : ''
  const text = typeof params.text === 'string' ? params.text : ''
  if (selector === '' && text === '') {
    throw bridgeError('BAD_REQUEST', 'wait 需要 selector 或 text 之一')
  }
  if (selector !== '' && text !== '') {
    throw bridgeError('BAD_REQUEST', 'selector 与 text 只能二选一')
  }
  const timeoutMs = typeof params.timeoutMs === 'number' && params.timeoutMs > 0
    ? Math.min(params.timeoutMs, 120_000)
    : WAIT_DEFAULT_TIMEOUT_MS

  const css = selector !== '' ? translateSelector(selector) : ''
  const check = (): boolean =>
    css !== '' ? document.querySelector(css) !== null : bodyText().includes(text)

  await new Promise<void>((resolve, reject) => {
    if (check()) {
      resolve()
      return
    }
    const deadline = Date.now() + timeoutMs
    let settled = false
    const observer = new MutationObserver(() => attempt())
    const timer = setInterval(attempt, WAIT_POLL_INTERVAL_MS)

    function cleanup(): void {
      if (settled) return
      settled = true
      clearInterval(timer)
      observer.disconnect()
    }
    function attempt(): void {
      if (settled) return
      if (check()) {
        cleanup()
        resolve()
      } else if (Date.now() >= deadline) {
        cleanup()
        const cond = css !== '' ? `selector ${selector}` : `text "${text}"`
        reject(bridgeError('WAIT_TIMEOUT', `等待 ${cond} 超时（${timeoutMs}ms）`))
      }
    }

    const root = document.body ?? document.documentElement
    observer.observe(root, { childList: true, subtree: true, characterData: true })
  })
  return { matched: true }
}

// ---------------------------------------------------------------------------
// 桥装配（exec 分发 + 回包）
// ---------------------------------------------------------------------------

/**
 * Task M3: state 动作——四 store 聚合快照（/api/state/app 数据面）。
 * pinia 显式传入（setupTestBridge 捕获）或回退 active pinia；
 * 两者皆无（应用未装 pinia 就 exec）按 INTERNAL 报错。
 */
export function doState(pinia: Pinia | null | undefined): AppTestSnapshot {
  const active = pinia ?? getActivePinia()
  if (!active) {
    throw bridgeError('INTERNAL', 'Pinia 未初始化，无法生成应用快照')
  }
  return {
    schemaVersion: 1,
    devices: useDevicesStore(active).toTestSnapshot(),
    transfers: useTransfersStore(active).toTestSnapshot(),
    settings: useSettingsStore(active).toTestSnapshot(),
    toast: useToastStore(active).toTestSnapshot(),
  }
}

/**
 * 结果回传：invoke('test_bridge_result', {id, ok, payload})。
 * payload 为数据本体（ok）或 {code, message}（错误），与 Rust 侧命令签名
 * test_bridge_result(id, ok, payload) 对齐。失败静默（通道已断时无路可回）。
 */
function respond(id: number, ok: boolean, payload: unknown): void {
  invoke('test_bridge_result', { id, ok, payload }).catch(() => {})
}

/** 动作分发（供 exec 与单测直调；state 动作需要 pinia 实例） */
export async function runAction(
  router: Router,
  action: string,
  params: Record<string, unknown>,
  pinia?: Pinia | null,
): Promise<unknown> {
  switch (action) {
    case 'navigate':
      return doNavigate(router, params)
    case 'click':
      return doClick(params)
    case 'toggle':
      return doToggle(params)
    case 'input':
      return doInput(params)
    case 'text':
      return doText(params)
    case 'tree':
      return collectTree()
    case 'wait':
      return doWait(params)
    case 'state':
      return doState(pinia)
    default:
      throw bridgeError('BAD_REQUEST', `未知 action: ${action}`)
  }
}

/**
 * eval 注入点入口载荷解析（纯函数便于直测）。
 * 契约形态是 JSON 字符串（Rust 侧把请求整体作为一个 JS 字符串字面量嵌入
 * eval，spec §7.1.4）；容错：若上游嵌成了对象字面量（exec 收到已求值
 * 对象），直接采用，不强行 JSON.parse。
 */
export function parseBridgeRequest(raw: unknown): BridgeRequest | null {
  if (typeof raw === 'string') {
    try {
      const v = JSON.parse(raw) as BridgeRequest
      return v && typeof v === 'object' ? v : null
    } catch {
      return null
    }
  }
  if (raw && typeof raw === 'object') {
    return raw as BridgeRequest
  }
  return null
}

/** eval 注入点：解析请求并异步执行（多请求可并发在途） */
function makeExec(router: Router, pinia?: Pinia | null): (payload: string) => void {
  return (payload: string): void => {
    const req = parseBridgeRequest(payload)
    if (!req || typeof req.id !== 'number' || typeof req.action !== 'string') {
      // 连请求 id/action 都拿不到，无法回配——只能静默丢弃（Rust 侧按超时报错）
      return
    }
    void runAction(router, req.action, req.params ?? {}, pinia).then(
      (data) => respond(req.id, true, data),
      (err) => respond(req.id, false, toBridgeError(err)),
    )
  }
}

// 模块状态（应用单实例；重复 setup 幂等）
let installed = false

/**
 * 注册 window.__testBridge 并握手（main.ts 在 VITE_TEST_API===1 时动态
 * import 后调用）。未过双重门控时完全 no-op。pinia 可选（state 动作用，
 * 不传则动作执行时回退 active pinia）。
 */
export function setupTestBridge(router: Router, pinia?: Pinia | null): void {
  if (installed) return
  if (import.meta.env.VITE_TEST_API !== '1') return
  if (!isTauriEnv()) return
  installed = true

  window.__testBridge = { exec: makeExec(router, pinia) }

  // 就绪握手（spec §7.1.4）：置位 Rust 侧 bridgeReady；失败静默
  // （非 test-api 构建的 Rust 壳会拒绝，ui/* 端点也就不会存在）
  invoke('test_bridge_hello').catch(() => {})
}

/** 仅测试用：还原注册状态（vitest 每用例隔离） */
export function __resetTestBridgeForTest(): void {
  installed = false
  delete window.__testBridge
}
