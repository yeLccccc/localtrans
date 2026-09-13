/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useTransfersStore } from '../stores/transfers'

// Mock the api module with the actual flat structure
vi.mock('../api', () => ({
  api: {
    transfers: {
      list: vi.fn().mockResolvedValue([]),
      pendingResumeJobs: vi.fn().mockResolvedValue([]),
      resumePending: vi.fn(),
      clearCompleted: vi.fn(),
      removeTransfer: vi.fn(),
      transferThrottle: vi.fn(),
    },
    browse: {
      transferAction: vi.fn(),
      startDownload: vi.fn(),
      pushFiles: vi.fn(),
    },
  },
  onTransferProgress: vi.fn(),
}))

describe('transfersStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('clearCompleted calls api and refreshes', async () => {
    const store = useTransfersStore()
    const { api } = await import('../api')

    // Mock clearCompleted to return a number
    vi.mocked(api.transfers.clearCompleted).mockResolvedValue(3)

    const n = await store.clearCompleted()

    expect(n).toBe(3)
    expect(api.transfers.clearCompleted).toHaveBeenCalled()
    expect(api.transfers.list).toHaveBeenCalled() // refreshTransfers calls this
  })

  it('removeTransfer calls api and filters local list', async () => {
    const store = useTransfersStore()
    const { api } = await import('../api')

    // Set up some initial transfers
    store.transfers = [
      { job_id: 'job1', name: 'Task 1', total: 100, done: 50, state: 'active', speed_bps: 0, peer: '', direction: 'pull', local_role: 'source-pull', health: null, started_at_ms: null },
      { job_id: 'job2', name: 'Task 2', total: 200, done: 100, state: 'active', speed_bps: 0, peer: '', direction: 'pull', local_role: 'source-pull', health: null, started_at_ms: null },
      { job_id: 'job3', name: 'Task 3', total: 300, done: 150, state: 'active', speed_bps: 0, peer: '', direction: 'pull', local_role: 'source-pull', health: null, started_at_ms: null },
    ] as any

    await store.removeTransfer('job2', 'destroy')

    expect(api.transfers.removeTransfer).toHaveBeenCalledWith('job2', 'destroy')
    expect(store.transfers.length).toBe(2)
    expect(store.transfers.find(t => t.job_id === 'job2')).toBeUndefined()
  })

  it('throttleTransfer passes args through to api', async () => {
    const store = useTransfersStore()
    const { api } = await import('../api')

    await store.throttleTransfer('job1', 5)

    expect(api.transfers.transferThrottle).toHaveBeenCalledWith('job1', 5)
  })

  // ===== R2 速度平滑:事件速度进表前过 EMA+尖峰抑制,终态归零 =====
  // (平滑器输入=进度导数优先,事件喂一致的 done 差分+speed)
  const MB = 1024 * 1024
  const mkJob = (over: Record<string, unknown>) => ({
    job_id: 'j1', name: 'f.bin', total: 500 * MB, done: 0, state: 'active',
    speed_bps: 0, peer: 'ab', direction: 'push', local_role: 'source-push',
    health: null, started_at_ms: 0, ...over,
  })

  async function driveEvents() {
    vi.useFakeTimers()
    vi.setSystemTime(0)
    const store = useTransfersStore()
    const { onTransferProgress } = await import('../api')
    await store.initialize()
    const cb = vi.mocked(onTransferProgress).mock.calls[vi.mocked(onTransferProgress).mock.calls.length - 1][0] as (p: { jobs: unknown[] }) => void
    const emit = async (jobs: unknown[]) => {
      cb({ jobs })
      await vi.advanceTimersByTimeAsync(250) // 冲刷 scheduleUpdate 防抖
    }
    return { store, emit, cleanup: () => vi.useRealTimers() }
  }

  it('4Hz 稳态速度:表内 speed_bps 是平滑显示值(低于瞬时原始值)', async () => {
    const { store, emit, cleanup } = await driveEvents()
    try {
      let done = 0
      for (let i = 1; i <= 8; i++) {
        vi.setSystemTime(i * 250)
        done += MB // 4MB/s × 250ms
        await emit([mkJob({ speed_bps: 4 * MB, done })])
      }
      const shown = store.transfers[0].speed_bps
      expect(shown).toBeGreaterThan(0)
      expect(shown).toBeLessThanOrEqual(4 * MB) // 导数基准+EMA 爬升,未超真实速率
      expect(shown).toBeGreaterThan(0.5 * MB) // 但已可见爬升
    } finally {
      cleanup()
    }
  })

  it('块边界尖峰:单拍 10× 突跳被窗口均速并入,显示值不跳 >3×', async () => {
    const { store, emit, cleanup } = await driveEvents()
    try {
      let done = 0
      for (let i = 1; i <= 12; i++) {
        vi.setSystemTime(i * 250)
        done += 0.25 * MB // 1MB/s 稳态
        await emit([mkJob({ speed_bps: MB, done })])
      }
      const before = store.transfers[0].speed_bps
      vi.setSystemTime(3250)
      await emit([mkJob({ speed_bps: 10 * MB, done: done + 2.5 * MB })])
      const after = store.transfers[0].speed_bps
      expect(after).toBeLessThan(before * 3)
    } finally {
      cleanup()
    }
  })

  it('尾段泵突发停滞:done 停滞时显示值衰减不虚高(2026-09-08 run-all 实测回归)', async () => {
    const { store, emit, cleanup } = await driveEvents()
    try {
      let done = 0
      // 前 12 拍 1MB/s 真实推进
      for (let i = 1; i <= 12; i++) {
        vi.setSystemTime(i * 250)
        done += 0.25 * MB
        await emit([mkJob({ speed_bps: MB, done })])
      }
      // 后 16 拍 done 停滞但泵仍报 8MB/s(发送窗口残余突发)
      for (let i = 13; i <= 28; i++) {
        vi.setSystemTime(i * 250)
        await emit([mkJob({ speed_bps: 8 * MB, done })])
      }
      const shown = store.transfers[0].speed_bps
      expect(shown).toBeLessThanOrEqual(MB) // 不得跟随泵虚高
    } finally {
      cleanup()
    }
  })

  it('终态卡速度强制归零且平滑器清理(结束归零)', async () => {
    const { store, emit, cleanup } = await driveEvents()
    try {
      let done = 0
      for (let i = 1; i <= 4; i++) {
        vi.setSystemTime(i * 250)
        done += MB
        await emit([mkJob({ speed_bps: 4 * MB, done })])
      }
      vi.setSystemTime(1500)
      await emit([mkJob({ state: 'done', speed_bps: 0, done })])
      expect(store.transfers[0].speed_bps).toBe(0)
      // 复活同 id(不应带旧 EMA 残值)
      vi.setSystemTime(2000)
      await emit([mkJob({ speed_bps: 4 * MB, done: done + MB })])
      expect(store.transfers[0].speed_bps).toBeLessThan(1 * MB) // 重新爬升
    } finally {
      cleanup()
    }
  })
})
