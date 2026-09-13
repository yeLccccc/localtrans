// confirm 的测试构建适配层（spec"原生对话框边界"的打通信）：
// - 正式构建：行为与 @tauri-apps/plugin-dialog 的 confirm 完全一致（原生系统弹窗）；
// - 测试构建（VITE_TEST_API=1）：挂载 DOM 版 ConfirmDialog（confirm-ok/confirm-cancel
//   testid），使 test-api 的 ui/* 可以驱动确认流——原生弹窗不在 DOM 里，远程无法点击。
// 仅适配 confirm（消息确认）；文件选择器仍走原生 + 测试场景语义注入路径。
import { createApp, h } from 'vue'
import ConfirmDialog from '../components/ConfirmDialog.vue'

const isTestBuild = import.meta.env.VITE_TEST_API === '1'

export function uiConfirm(message: string, options?: { title?: string }): Promise<boolean> {
  if (!isTestBuild) {
    // 动态 import 保持正式构建零额外打包面，行为不变
    return import('@tauri-apps/plugin-dialog').then((m) => m.confirm(message, options))
  }
  return new Promise((resolve) => {
    const host = document.createElement('div')
    document.body.appendChild(host)
    const app = createApp({
      render: () =>
        h(ConfirmDialog, {
          title: options?.title ?? '确认',
          message,
          onResolve: (ok: boolean) => {
            app.unmount()
            host.remove()
            resolve(ok)
          },
        }),
    })
    app.mount(host)
  })
}
