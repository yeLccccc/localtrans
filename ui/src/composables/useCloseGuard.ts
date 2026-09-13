// ui/src/composables/useCloseGuard.ts
import { ref, watch } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useTransfersStore } from '../stores/transfers'
import { api } from '../api'

export function useCloseGuard() {
  const transfersStore = useTransfersStore()
  const guardOpen = ref(false)
  const activeCount = ref(0)
  const forceClose = ref(false)

  let unlisten: (() => void) | null = null

  async function init() {
    try {
      const win = getCurrentWindow()
      unlisten = await win.onCloseRequested(async (event) => {
        // 拦截范围：传输中 + 排队/暂停（重启后都能断点恢复）
        const active = transfersStore.transfers.filter(
          t => ['active', 'pending', 'paused'].includes(t.state)
        )
        if (active.length === 0) return  // 放行

        event.preventDefault()
        activeCount.value = active.length
        guardOpen.value = true

        // 等用户决定
        await new Promise<void>((resolve) => {
          const stop = watch(guardOpen, (open) => {
            if (!open) { stop(); resolve() }
          })
        })

        if (forceClose.value) {
          // v0.2.7 优雅关闭：Goodbye 通知对端 + 任务表落盘 + 位图持久化，
          // 然后才真正退出——对端不用等 60s idle 超时，下次启动可断点恢复
          try {
            await api.system.prepareShutdown()
          } catch (e) {
            console.error('优雅关闭准备失败（继续退出）:', e)
          }
          await win.destroy()
        }
      })
    } catch {
      // Silently fail in browser dev context where Tauri APIs aren't available
      // No-op degradation - app will close normally
    }
  }

  function cancelClose() {
    forceClose.value = false
    guardOpen.value = false
  }

  function doForceClose() {
    forceClose.value = true
    guardOpen.value = false
  }

  function cleanup() {
    if (unlisten) unlisten()
  }

  return { guardOpen, activeCount, init, cancelClose, doForceClose, cleanup }
}
