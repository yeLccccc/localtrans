/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useCloseGuard } from '../composables/useCloseGuard'

// Mock the Tauri window API
const mockPreventDefault = vi.fn()
const mockDestroy = vi.fn()
let onCloseRequestedCallback: ((event: { preventDefault: () => void }) => void | Promise<void>) | null = null

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    onCloseRequested: vi.fn().mockImplementation((callback) => {
      onCloseRequestedCallback = callback
      return Promise.resolve(vi.fn())
    }),
    destroy: mockDestroy,
  }),
}))

// Mock the transfers store with different states for different tests
let mockTransfers: any[] = []

vi.mock('../stores/transfers', () => ({
  useTransfersStore: () => ({
    transfers: mockTransfers,
  }),
}))

describe('useCloseGuard', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
    vi.clearAllMocks()
    onCloseRequestedCallback = null
    mockPreventDefault.mockClear()
    mockDestroy.mockClear()
    mockTransfers = []
  })

  it('returns guardOpen and activeCount', () => {
    mockTransfers = []
    const closeGuard = useCloseGuard()
    expect(closeGuard.guardOpen.value).toBe(false)
    expect(closeGuard.activeCount.value).toBe(0)
  })

  it('active transfers are counted correctly', async () => {
    mockTransfers = [
      { job_id: '1', name: 'file1.bin', total: 1000, done: 500, state: 'active' },
      { job_id: '2', name: 'file2.bin', total: 2000, done: 1000, state: 'active' },
      { job_id: '3', name: 'file3.bin', total: 3000, done: 3000, state: 'done' },
    ]

    const closeGuard = useCloseGuard()
    await closeGuard.init()

    expect(onCloseRequestedCallback).toBeTruthy()

    // Start the close process but don't wait for it
    const event = { preventDefault: mockPreventDefault }
    const closePromise = onCloseRequestedCallback!(event)

    // The guard should be open immediately
    expect(mockPreventDefault).toHaveBeenCalled()
    expect(closeGuard.guardOpen.value).toBe(true)
    expect(closeGuard.activeCount.value).toBe(2) // Only active transfers counted

    // Cancel to resolve the promise
    closeGuard.cancelClose()
    await closePromise
  })

  it('no active transfers → preventDefault NOT called', async () => {
    mockTransfers = [
      { job_id: '1', name: 'file1.bin', total: 1000, done: 1000, state: 'done' },
      { job_id: '2', name: 'file2.bin', total: 2000, done: 2000, state: 'done' },
    ]

    const closeGuard = useCloseGuard()
    await closeGuard.init()

    const event = { preventDefault: mockPreventDefault }
    const closePromise = onCloseRequestedCallback!(event)
    await closePromise

    expect(mockPreventDefault).not.toHaveBeenCalled()
    expect(closeGuard.guardOpen.value).toBe(false)
    expect(closeGuard.activeCount.value).toBe(0)
  })

  it('doForceClose path closes guard and destroys window', async () => {
    mockTransfers = [
      { job_id: '1', name: 'file1.bin', total: 1000, done: 500, state: 'active' },
    ]

    const closeGuard = useCloseGuard()
    await closeGuard.init()

    // Start the close process
    const event = { preventDefault: mockPreventDefault }
    const closePromise = onCloseRequestedCallback!(event)

    expect(closeGuard.guardOpen.value).toBe(true)

    // Then force close
    closeGuard.doForceClose()

    expect(closeGuard.guardOpen.value).toBe(false)

    // Wait for the promise to resolve
    await closePromise

    expect(mockDestroy).toHaveBeenCalled()
  })

  it('cancelClose closes guard without destroying window', async () => {
    mockTransfers = [
      { job_id: '1', name: 'file1.bin', total: 1000, done: 500, state: 'active' },
    ]

    const closeGuard = useCloseGuard()
    await closeGuard.init()

    // Start the close process
    const event = { preventDefault: mockPreventDefault }
    const closePromise = onCloseRequestedCallback!(event)

    expect(closeGuard.guardOpen.value).toBe(true)

    // Then cancel close
    closeGuard.cancelClose()

    expect(closeGuard.guardOpen.value).toBe(false)

    // Wait for the promise to resolve
    await closePromise

    expect(mockDestroy).not.toHaveBeenCalled()
  })

  it('cleanup removes event listener', () => {
    mockTransfers = []
    const closeGuard = useCloseGuard()
    closeGuard.cleanup()

    // Should not throw even if cleanup is called without init
    expect(closeGuard.guardOpen.value).toBe(false)
  })
})
