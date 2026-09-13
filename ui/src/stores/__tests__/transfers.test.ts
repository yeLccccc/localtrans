import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'

vi.mock('../../api', () => ({
  api: {
    transfers: {
      list: vi.fn().mockResolvedValue([]),
      clearCompleted: vi.fn().mockResolvedValue(0),
      removeTransfer: vi.fn().mockResolvedValue(true),
      listDiskJobs: vi.fn().mockResolvedValue([
        { job_id: '3', display_name: '老任务', total: 5, done: 2,
          state: 'interrupted', direction: 'pull', peer_hex: 'aabb',
          created_at_ms: 1, removed_from_view: true },
      ]),
      restoreDiskJob: vi.fn().mockResolvedValue(undefined),
      destroyDiskJob: vi.fn().mockResolvedValue(undefined),
      pendingResumeJobs: vi.fn().mockResolvedValue([]),
      resumePending: vi.fn(),
      transferThrottle: vi.fn(),
    },
    browse: { transferAction: vi.fn(), startDownload: vi.fn(), pushFiles: vi.fn(), pushFilesRel: vi.fn() },
  },
  onTransferProgress: vi.fn(() => () => {}),
  friendlyError: vi.fn((s: string) => s),
}))

import { useTransfersStore } from '../transfers'

describe('transfers store 新接口', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
  })

  it('removeTransfer 传 level 而非 deleteParts', async () => {
    const st = useTransfersStore()
    st.transfers = [{ job_id: '9', name: 'x', total: 1, done: 0, state: 'done',
      speed_bps: 0, peer: 'a', direction: 'pull', local_role: 'destination',
      health: null, started_at_ms: 1, finished_at_ms: 2 } as any]
    await st.removeTransfer('9', 'view')
    const { api } = await import('../../api')
    expect(api.transfers.removeTransfer).toHaveBeenCalledWith('9', 'view')
  })

  it('destroy 级删除才乐观过滤本地列表', async () => {
    const st = useTransfersStore()
    st.transfers = [{ job_id: '9', name: 'x', total: 1, done: 0, state: 'done',
      speed_bps: 0, peer: 'a', direction: 'pull', local_role: 'destination',
      health: null, started_at_ms: 1 } as any]
    await st.removeTransfer('9', 'destroy')
    expect(st.transfers.length).toBe(0)
  })

  it('view 级删除保留本地列表(由 4Hz 事件自然收敛)', async () => {
    const st = useTransfersStore()
    st.transfers = [{ job_id: '9', name: 'x', total: 1, done: 0, state: 'done',
      speed_bps: 0, peer: 'a', direction: 'pull', local_role: 'destination',
      health: null, started_at_ms: 1 } as any]
    await st.removeTransfer('9', 'view')
    expect(st.transfers.length).toBe(1)
  })

  it('refreshDiskJobs 拉取并缓存磁盘历史', async () => {
    const st = useTransfersStore()
    await st.refreshDiskJobs()
    expect(st.diskJobs.length).toBe(1)
    expect(st.diskJobs[0].removed_from_view).toBe(true)
  })

  it('restoreDiskJob 操作后刷新历史', async () => {
    const st = useTransfersStore()
    await st.restoreDiskJob('3')
    const { api } = await import('../../api')
    expect(api.transfers.restoreDiskJob).toHaveBeenCalledWith('3')
    expect(api.transfers.listDiskJobs).toHaveBeenCalled()
  })

  it('destroyDiskJob 操作后刷新历史', async () => {
    const st = useTransfersStore()
    await st.destroyDiskJob('3')
    const { api } = await import('../../api')
    expect(api.transfers.destroyDiskJob).toHaveBeenCalledWith('3')
    expect(api.transfers.listDiskJobs).toHaveBeenCalled()
  })
})
