/**
 * P3-T4:信任列表缓存滞后修复
 * - refreshTrusted():仅回读信任列表(配对成功事件驱动)
 * - removeTrusted():本地摘除 + 回读复同步
 */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'

const listTrustedMock = vi.fn()

vi.mock('../../api', () => ({
  api: {
    settings: {
      get: vi.fn().mockResolvedValue({ device_name: 'pc', shares: [] }),
      listTrusted: (...args: unknown[]) => listTrustedMock(...args),
      save: vi.fn(),
      addShare: vi.fn(),
      removeShare: vi.fn(),
      setPerms: vi.fn(),
      removeTrusted: vi.fn().mockResolvedValue(true),
      setAlias: vi.fn(),
    },
    system: { addFirewallRule: vi.fn() },
  },
}))

import { useSettingsStore } from '../settings'

const PEER_A = { fingerprint: 'aa', name: '设备A', alias: '', paired_at: 1, browse: true, download: true, push: 'ask' }
const PEER_B = { fingerprint: 'bb', name: '设备B', alias: '', paired_at: 2, browse: true, download: false, push: 'ask' }

beforeEach(() => {
  setActivePinia(createPinia())
  listTrustedMock.mockReset()
})

describe('settings store refreshTrusted (P3-T4)', () => {
  it('refreshTrusted 回读信任列表并更新 store', async () => {
    listTrustedMock.mockResolvedValue([PEER_A, PEER_B])
    const store = useSettingsStore()

    expect(store.trustedPeers).toEqual([])
    await store.refreshTrusted()
    expect(store.trustedPeers).toHaveLength(2)

    // 配对成功后再次回读 → 新设备出现(设置页停留期间事件驱动刷新)
    listTrustedMock.mockResolvedValue([PEER_A, PEER_B, { ...PEER_A, fingerprint: 'cc', name: '新设备' }])
    await store.refreshTrusted()
    expect(store.trustedPeers).toHaveLength(3)
    expect(store.trustedPeers.some((p) => p.name === '新设备')).toBe(true)
  })

  it('removeTrusted 本地摘除并回读复同步', async () => {
    listTrustedMock.mockResolvedValue([PEER_A, PEER_B])
    const store = useSettingsStore()
    await store.refreshTrusted()
    expect(store.trustedPeers).toHaveLength(2)

    // 移除 A:本地先行摘除,再回读(回读结果也不含 A)
    listTrustedMock.mockResolvedValue([PEER_B])
    const ok = await store.removeTrusted('aa')
    expect(ok).toBe(true)
    expect(store.trustedPeers.map((p) => p.fingerprint)).toEqual(['bb'])
    expect(listTrustedMock).toHaveBeenCalledTimes(2)
  })

  it('refreshTrusted 失败时记 error 且不清空现有列表', async () => {
    listTrustedMock.mockResolvedValue([PEER_A])
    const store = useSettingsStore()
    await store.refreshTrusted()
    expect(store.trustedPeers).toHaveLength(1)

    listTrustedMock.mockRejectedValue(new Error('backend down'))
    await store.refreshTrusted()
    expect(store.error).toContain('backend down')
    expect(store.trustedPeers).toHaveLength(1)
  })
})
