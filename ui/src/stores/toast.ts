/**
 * Toast Store
 * 管理全局 Toast 消息
 */

import { defineStore } from 'pinia'
import { ref } from 'vue'
import type { ToastLevel } from '../types'
import { onToast } from '../api'

export const useToastStore = defineStore('toast', () => {
  // Toast 消息列表
  const toasts = ref<Array<{ id: number; level: ToastLevel; text: string }>>([])

  // Toast ID 计数器
  let toastIdCounter = 0

  /**
   * 显示 Toast 消息
   */
  function push(level: ToastLevel, text: string) {
    const id = toastIdCounter++
    toasts.value.push({ id, level, text })

    // 3秒后自动移除
    setTimeout(() => {
      toasts.value = toasts.value.filter((t) => t.id !== id)
    }, 3000)
  }

  /**
   * 清除所有 Toast
   */
  function clear() {
    toasts.value = []
  }

  /**
   * 移除指定 Toast
   */
  function remove(id: number) {
    toasts.value = toasts.value.filter((t) => t.id !== id)
  }

  /**
   * 初始化事件监听
   */
  async function initialize() {
    // 监听来自 Rust 的 Toast 事件
    const unlisten = await onToast((toast) => {
      push(toast.level, toast.text)
    })

    return unlisten
  }

  /**
   * Task M3: 测试快照（spec §7.1.6 Pinia 侧）。
   * 返回纯 JSON 可序列化对象（JSON 往返剥响应式/函数/undefined）。
   * 契约见 docs/contracts/test-api.md §5.3。
   */
  function toTestSnapshot() {
    return JSON.parse(JSON.stringify({ schemaVersion: 1, toasts: toasts.value }))
  }

  return {
    // 状态
    toasts,

    // 方法
    push,
    clear,
    remove,
    initialize,
    toTestSnapshot,
  }
})