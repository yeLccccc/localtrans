/**
 * 编程式确认对话框
 * WebView2 不支持 window.confirm/prompt(静默返回 false/null),
 * 组件内统一用本组合式替换原生对话框。
 *
 * 用法:App.vue 挂 ConfirmDialog 宿主;组件里
 *   const { confirm } = useConfirm()
 *   if (await confirm({ title, message })) { ... }   // 确认框
 *   const alias = await prompt({ title, initial })   // 输入框(取消返回 null)
 */
import { reactive, readonly } from 'vue'

export interface ConfirmOptions {
  title: string
  message?: string
  hint?: string
  okText?: string
  /** 输入模式:带输入框,确认返回输入值,取消返回 null */
  inputInitial?: string
  inputPlaceholder?: string
}

interface ConfirmState {
  open: boolean
  title: string
  message: string
  hint: string | undefined
  okText: string | undefined
  inputInitial: string | undefined
  inputPlaceholder: string | undefined
}

const state = reactive<ConfirmState>({
  open: false,
  title: '',
  message: '',
  hint: undefined,
  okText: undefined,
  inputInitial: undefined,
  inputPlaceholder: undefined,
})

type Resolver = (ok: boolean, input?: string) => void
let pending: Resolver | null = null

/** 确认框:返回 true/false */
function confirm(opts: ConfirmOptions): Promise<boolean> {
  return open(opts).then((r) => r === true)
}

/** 输入框:确认返回输入值(可能为空串),取消返回 null */
function prompt(opts: ConfirmOptions): Promise<string | null> {
  return open({ ...opts, inputInitial: opts.inputInitial ?? '' }).then((r) =>
    typeof r === 'string' ? r : null
  )
}

function open(opts: ConfirmOptions): Promise<boolean | string | null> {
  // 已有弹窗时直接拒绝新请求,不排队(fail-closed,与原生单弹窗语义一致)
  const isInput = opts.inputInitial !== undefined || opts.inputPlaceholder !== undefined
  if (state.open) return Promise.resolve(isInput ? null : false)
  Object.assign(state, {
    title: opts.title,
    message: opts.message ?? '',
    hint: opts.hint,
    okText: opts.okText ?? (isInput ? '确定' : '删除'),
    inputInitial: opts.inputInitial,
    inputPlaceholder: opts.inputPlaceholder,
    open: true,
  })
  return new Promise((resolve) => {
    pending = (ok, input) => {
      if (!ok) resolve(isInput ? null : false)
      else resolve(isInput ? (input ?? '') : true)
    }
  })
}

function resolveDialog(ok: boolean, input?: string) {
  state.open = false
  if (pending) {
    pending(ok, input)
    pending = null
  }
}

export function useConfirm() {
  return {
    confirm,
    prompt,
    dialogState: readonly(state),
    resolveDialog,
  }
}
