/**
 * Task M3: 四个 store 的 toTestSnapshot 序列化契约（spec §7.1.6 Pinia 侧）
 * - JSON.stringify 不抛（纯 JSON 可序列化：无函数/循环引用）
 * - schemaVersion:1 与关键字段在
 * - Set（devices.reconnecting）转数组；transfers.lastRequest 不进快照
 */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'

vi.mock('../../api', () => ({
  api: {
    devices: { list: vi.fn().mockResolvedValue([]), probeNow: vi.fn(), addManual: vi.fn(), connect: vi.fn(), setHidden: vi.fn() },
    pairing: { getPending: vi.fn().mockResolvedValue([]), submitCode: vi.fn(), deny: vi.fn() },
    settings: {
      get: vi.fn().mockResolvedValue({}),
      listTrusted: vi.fn().mockResolvedValue([]),
      save: vi.fn(),
      addShare: vi.fn(),
      removeShare: vi.fn(),
      setPerms: vi.fn(),
      removeTrusted: vi.fn(),
      setAlias: vi.fn(),
    },
    system: { addFirewallRule: vi.fn() },
    transfers: {
      list: vi.fn().mockResolvedValue([]),
      pendingResumeJobs: vi.fn().mockResolvedValue([]),
      listDiskJobs: vi.fn().mockResolvedValue([]),
      clearCompleted: vi.fn(),
      removeTransfer: vi.fn(),
      restoreDiskJob: vi.fn(),
      destroyDiskJob: vi.fn(),
      resumePending: vi.fn(),
      transferThrottle: vi.fn(),
    },
    browse: { transferAction: vi.fn(), startDownload: vi.fn(), pushFiles: vi.fn(), pushFilesRel: vi.fn() },
  },
  onDeviceList: vi.fn(() => () => {}),
  onConnectionState: vi.fn(() => () => {}),
  onPeerReconnecting: vi.fn(() => () => {}),
  onTransferProgress: vi.fn(() => () => {}),
  onToast: vi.fn(async () => () => {}),
  friendlyError: vi.fn((s: string) => s),
}))

import { useDevicesStore } from '../devices'
import { useSettingsStore } from '../settings'
import { useToastStore } from '../toast'
import { useTransfersStore } from '../transfers'

/** JSON.stringify 必须不抛且往返等值（纯 JSON 断言的根基） */
function expectPlainJson(value: unknown): void {
  const text = JSON.stringify(value)
  expect(text).toBeDefined()
  expect(JSON.parse(text as string)).toEqual(value)
}

beforeEach(() => {
  setActivePinia(createPinia())
})

describe('devices store toTestSnapshot', () => {
  it('关键字段在且可序列化', () => {
    const st = useDevicesStore()
    st.devices = [
      { fingerprint: 'ab'.repeat(32), name: 'A 机', addr: '192.168.1.5:47601', online: true, connected: true },
      { fingerprint: 'cd'.repeat(32), name: 'B 机', addr: '192.168.1.6:47601', online: false, connected: false },
    ]
    st.pairingPending = [{ fingerprint: 'ef'.repeat(32), name: 'C 机', own_code: '1234' }]
    st.selected_fp = 'ab'.repeat(32)
    st.reconnecting = new Set(['cd'.repeat(32)])
    st.loading = false
    st.error = null

    const snap = st.toTestSnapshot()
    expectPlainJson(snap)
    expect(snap.schemaVersion).toBe(1)
    expect(snap.devices).toHaveLength(2)
    expect(snap.devices[0]).toMatchObject({ name: 'A 机', connected: true })
    expect(snap.pairingPending[0].own_code).toBe('1234')
    expect(snap.selectedFp).toBe('ab'.repeat(32))
    expect(snap.reconnecting).toEqual(['cd'.repeat(32)]) // Set 转数组
    expect(snap.loading).toBe(false)
    expect(snap.error).toBeNull()
  })
})

describe('settings store toTestSnapshot', () => {
  it('config 与 trustedPeers 快照', () => {
    const st = useSettingsStore()
    st.config = {
      device_name: '本机',
      download_dir: 'D:/dl',
      hidden: false,
      quic_port: 47601,
      discovery_port: 47600,
      shares: [{ id: 's1', alias: '电影', path: 'D:/video' }],
    }
    st.trustedPeers = [
      { fingerprint: 'ab'.repeat(32), name: 'A 机', alias: '', paired_at: 1, browse: true, download: true, push: 'ask' },
    ]
    st.loading = false
    st.error = '加载失败'

    const snap = st.toTestSnapshot()
    expectPlainJson(snap)
    expect(snap.schemaVersion).toBe(1)
    expect(snap.config.device_name).toBe('本机')
    expect(snap.config.shares[0].alias).toBe('电影')
    expect(snap.trustedPeers[0].push).toBe('ask')
    expect(snap.error).toBe('加载失败')
  })
})

describe('toast store toTestSnapshot', () => {
  it('toasts 快照', () => {
    const st = useToastStore()
    st.push('error', '传输失败')
    st.push('success', '已完成')

    const snap = st.toTestSnapshot()
    expectPlainJson(snap)
    expect(snap.schemaVersion).toBe(1)
    expect(snap.toasts).toHaveLength(2)
    expect(snap.toasts[0]).toMatchObject({ level: 'error', text: '传输失败' })
  })
})

describe('transfers store toTestSnapshot', () => {
  it('传输列表/历史/可恢复任务快照，lastRequest 不进快照', () => {
    const st = useTransfersStore()
    st.transfers = [
      {
        job_id: '0'.repeat(15) + '1',
        name: '大文件.bin',
        total: 2048,
        done: 1024,
        state: 'active',
        speed_bps: 512,
        peer: 'ab'.repeat(32),
        direction: 'pull',
        local_role: 'destination',
        health: { loss_ratio: 0.01, rtt_ms: 12, cwnd: 64, streams: 2 },
        started_at_ms: 100,
      },
    ] as any
    st.resumeJobs = [['0'.repeat(15) + '2', '待恢复']]
    st.diskJobs = [
      { job_id: '9', display_name: '老任务', total: 5, done: 5, state: 'done', direction: 'push', peer_hex: 'aabb', created_at_ms: 1, removed_from_view: true },
    ]
    st.recordPullRequest('0'.repeat(15) + '1', 'ab'.repeat(32), 's1', '/a/b')

    const snap = st.toTestSnapshot()
    expectPlainJson(snap)
    expect(snap.schemaVersion).toBe(1)
    expect(snap.transfers[0]).toMatchObject({ name: '大文件.bin', state: 'active', done: 1024, total: 2048 })
    expect(snap.transfers[0].health.rtt_ms).toBe(12) // 嵌套 DTO 保留
    expect(snap.resumeJobs).toEqual([['0'.repeat(15) + '2', '待恢复']])
    expect(snap.diskJobs[0].state).toBe('done')
    expect('lastRequest' in snap).toBe(false) // lastRequest（Map 簿记）不进快照
  })

  it('error 与 loading 透传', () => {
    const st = useTransfersStore()
    st.error = '网络异常'
    st.loading = true
    const snap = st.toTestSnapshot()
    expect(snap.error).toBe('网络异常')
    expect(snap.loading).toBe(true)
  })
})
