/**
 * 设置 Store
 * 管理应用设置和信任列表
 */

import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import type { ConfigDto, TrustedPeerDto, PushPolicy } from '../types'
import { api } from '../api'

export const useSettingsStore = defineStore('settings', () => {
  // 应用配置
  const config = shallowRef<ConfigDto | null>(null)

  // 信任列表
  const trustedPeers = shallowRef<TrustedPeerDto[]>([])

  // 加载状态
  const loading = ref(false)

  // 错误信息
  const error = ref<string | null>(null)

  /**
   * 刷新设置
   */
  async function refreshSettings() {
    loading.value = true
    error.value = null
    try {
      config.value = await api.settings.get()
      trustedPeers.value = await api.settings.listTrusted()
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to load settings:', e)
    } finally {
      loading.value = false
    }
  }

  /**
   * 保存设置
   */
  async function saveSettings(newConfig: ConfigDto) {
    try {
      await api.settings.save(newConfig)
      config.value = newConfig
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to save settings:', e)
      throw e
    }
  }

  /**
   * 添加共享区
   */
  async function addShare(alias: string, path: string) {
    try {
      const share = await api.settings.addShare(alias, path)

      if (config.value) {
        config.value = {
          ...config.value,
          shares: [...config.value.shares, share],
        }
      }
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to add share:', e)
      throw e
    }
  }

  /**
   * 移除共享区
   */
  async function removeShare(id: string) {
    try {
      const removed = await api.settings.removeShare(id)

      if (removed && config.value) {
        config.value = {
          ...config.value,
          shares: config.value.shares.filter((s) => s.id !== id),
        }
      }
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to remove share:', e)
      throw e
    }
  }

  /**
   * 设置权限
   */
  async function setPerms(
    fingerprint: string,
    browse: boolean,
    download: boolean,
    push: PushPolicy
  ) {
    try {
      const ok = await api.settings.setPerms(fingerprint, browse, download, push)

      if (ok) {
        // 更新本地信任列表
        trustedPeers.value = trustedPeers.value.map((peer) =>
          peer.fingerprint === fingerprint
            ? { ...peer, browse, download, push }
            : peer
        )
      }

      return ok
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to set permissions:', e)
      throw e
    }
  }

  /**
   * 仅刷新信任列表（P3-T4：配对成功事件/移除信任后调用——原实现只在
   * refreshSettings（设置页 onMounted）里加载一次，设置页停留期间配对
   * 完成/别处移除时列表不动）
   */
  async function refreshTrusted() {
    try {
      trustedPeers.value = await api.settings.listTrusted()
      error.value = null
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to refresh trusted peers:', e)
    }
  }

  /**
   * 移除信任对等方
   */
  async function removeTrusted(fingerprint: string) {
    try {
      const removed = await api.settings.removeTrusted(fingerprint)

      if (removed) {
        // 本地先行摘除（即时反馈）+ 回读复同步（P3-T4：防本地视图与后端漂移）
        trustedPeers.value = trustedPeers.value.filter(
          (peer) => peer.fingerprint !== fingerprint
        )
        await refreshTrusted()
      }

      return removed
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to remove trusted peer:', e)
      throw e
    }
  }

  /**
   * 设置信任对端本地别名(空串=清除)
   */
  async function setAlias(fingerprint: string, alias: string) {
    try {
      const ok = await api.settings.setAlias(fingerprint, alias)

      if (ok) {
        trustedPeers.value = trustedPeers.value.map((peer) =>
          peer.fingerprint === fingerprint
            ? { ...peer, alias: alias.trim() }
            : peer
        )
      }

      return ok
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to set alias:', e)
      throw e
    }
  }

  /**
   * 添加防火墙规则
   */
  async function addFirewallRule() {
    try {
      return await api.system.addFirewallRule()
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to add firewall rule:', e)
      throw e
    }
  }

  /**
   * 初始化 store
   */
  async function initialize() {
    await refreshSettings()
  }

  /**
   * Task M3: 测试快照（spec §7.1.6 Pinia 侧）。
   * 返回纯 JSON 可序列化对象：JSON 往返剥响应式/函数/undefined。
   * 契约见 docs/contracts/test-api.md §5.3。
   */
  function toTestSnapshot() {
    return JSON.parse(
      JSON.stringify({
        schemaVersion: 1,
        config: config.value,
        trustedPeers: trustedPeers.value,
        loading: loading.value,
        error: error.value,
      }),
    )
  }

  return {
    // 状态
    config,
    trustedPeers,
    loading,
    error,

    // 方法
    refreshSettings,
    refreshTrusted,
    saveSettings,
    addShare,
    removeShare,
    setPerms,
    removeTrusted,
    setAlias,
    addFirewallRule,
    initialize,
    toTestSnapshot,
  }
})
