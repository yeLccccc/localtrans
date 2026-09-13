/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import { flushPromises } from '@vue/test-utils'

// Mock the api module with the full pattern from existing tests
vi.mock('../api', () => ({
  api: {
    transfers: {
      list: vi.fn().mockResolvedValue([]),
      pendingResumeJobs: vi.fn().mockResolvedValue([]),
      resumePending: vi.fn(),
      clearCompleted: vi.fn(),
      removeTransfer: vi.fn(),
      transferThrottle: vi.fn(),
      hasParts: vi.fn(),
    },
    browse: {
      transferAction: vi.fn(),
      startDownload: vi.fn(),
      pushFiles: vi.fn(),
    },
  },
  onTransferProgress: vi.fn(),
}))

import TransfersPage from '../pages/Transfers.vue'

describe('TransfersPage', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('renders clear completed button', async () => {
    const w = mount(TransfersPage)
    await flushPromises() // Wait for onMounted handleRefresh to complete
    expect(w.text()).toContain('清除已完成/失败')
  })

  it('renders empty-state hint after flushPromises', async () => {
    const w = mount(TransfersPage)
    await flushPromises() // Wait for onMounted handleRefresh to complete
    expect(w.text()).toContain('试试从设备页发起传输,或在浏览页下载文件')
  })

  it('clicking clear button calls api.transfers.clearCompleted', async () => {
    const w = mount(TransfersPage)
    await flushPromises() // Wait for onMounted handleRefresh to complete

    const { api } = await import('../api')
    vi.mocked(api.transfers.clearCompleted).mockResolvedValue(2)

    await w.findAll('button').find((b) => b.text() === '清除已完成/失败')!.trigger('click')
    await flushPromises() // Wait for async handler to complete

    expect(api.transfers.clearCompleted).toHaveBeenCalled()
  })
})
