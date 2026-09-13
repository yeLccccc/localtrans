/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import TransferItem from '../components/TransferItem.vue'
import { vClickOutside } from '../directives/clickOutside'

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
      hasParts: vi.fn(),
    },
    browse: {
      transferAction: vi.fn(),
      startDownload: vi.fn(),
      pushFiles: vi.fn(),
      pushFilesRel: vi.fn(),
    },
  },
  onTransferProgress: vi.fn(),
  friendlyError: (e: unknown) => (e instanceof Error ? e.message : String(e)),
}))

const baseTransfer = {
  job_id: '0000000000000001',
  name: 'test.bin',
  total: 1000,
  done: 500,
  state: 'active',
  speed_bps: 100,
  peer: 'aabbccddeeff',
  direction: 'pull',
  local_role: 'destination' as const,
  health: { loss_ratio: 0.01, rtt_ms: 5, cwnd: 100, streams: 4 },
  started_at_ms: Date.now() - 10000,
}

describe('TransferItem', () => {
  const mountOptions = {
    global: {
      directives: {
        'click-outside': vClickOutside
      }
    }
  }

  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('renders role text for destination', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer }, ...mountOptions })
    expect(w.text()).toContain('接收方')
  })

  it('renders role text for source-push', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, local_role: 'source-push' as const } },
      ...mountOptions
    })
    expect(w.text()).toContain('推送方')
  })

  it('renders role text for source-pull', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const } },
      ...mountOptions
    })
    expect(w.text()).toContain('被取方')
  })

  it('renders health panel when active', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer }, ...mountOptions })
    expect(w.text()).toContain('丢包')
    expect(w.text()).toContain('RTT')
    expect(w.text()).toContain('cwnd')
  })

  it('hides health panel when not active', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, state: 'done' } },
      ...mountOptions
    })
    expect(w.text()).not.toContain('丢包')
  })

  it('renders time info with elapsed time', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer }, ...mountOptions })
    expect(w.text()).toContain('已用')
  })

  it('renders ETA when speed > 0 and not complete', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer }, ...mountOptions })
    expect(w.text()).toContain('剩余')
  })

  it('shows "即将完成" when etaSeconds is 0', () => {
    const almostDoneTransfer = {
      ...baseTransfer,
      total: 1000,
      done: 1000, // Complete transfer means etaSeconds = 0
      speed_bps: 100,
    }
    const w = mount(TransferItem, { props: { transfer: almostDoneTransfer }, ...mountOptions })
    expect(w.text()).toContain('即将完成')
  })

  // New tests for Task 16
  it('destination + active shows pause/cancel', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'destination' as const, state: 'active' } }, ...mountOptions })
    expect(w.text()).toContain('暂停')
    expect(w.text()).toContain('取消')
  })

  it('destination + paused shows resume/cancel', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'destination' as const, state: 'paused' } }, ...mountOptions })
    expect(w.text()).toContain('继续')
  })

  it('source-pull + active shows throttle/kick', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const, state: 'active' } }, ...mountOptions })
    expect(w.text()).toContain('限速')
    expect(w.text()).toContain('踢人')
  })

  it('source-push + active shows pause/cancel', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-push' as const, state: 'active' } }, ...mountOptions })
    expect(w.text()).toContain('暂停')
    expect(w.text()).toContain('取消')
  })

  it('done state shows open folder / delete', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, state: 'done' } }, ...mountOptions })
    expect(w.text()).toContain('打开所在文件夹')
    expect(w.text()).toContain('删除')
  })

  it('interrupted state shows 已中断', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, state: 'interrupted' } }, ...mountOptions })
    expect(w.text()).toContain('已中断')
  })

  // T16 tests for pending hints and interrupted states
  it('destination + pending shows pending hint', () => {
    // 文案按方向区分:destination=pull → 排队等调度/连对端
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'destination' as const, state: 'pending' } }, ...mountOptions })
    expect(w.text()).toContain('排队中，连接对端...')
  })

  it('source-push + pending shows pending hint', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-push' as const, state: 'pending' } }, ...mountOptions })
    expect(w.text()).toContain('等待对方确认接收...')
  })

  it('source-pull + pending shows pending hint', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const, state: 'pending' } }, ...mountOptions })
    expect(w.text()).toContain('等待对方连接...')
  })

  it('source-push + interrupted shows only 删除 button', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-push' as const, state: 'interrupted' } }, ...mountOptions })
    expect(w.text()).toContain('删除')
    expect(w.text()).not.toContain('续传')
  })

  it('source-pull + interrupted shows only 删除 button', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const, state: 'interrupted' } }, ...mountOptions })
    expect(w.text()).toContain('删除')
    expect(w.text()).not.toContain('续传')
  })

  it('throttle menu is closed initially', () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const, state: 'active' } }, ...mountOptions })
    expect(w.find('.throttle-menu').exists()).toBe(false)
  })

  it('clicking 限速 button opens throttle menu', async () => {
    const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull' as const, state: 'active' } }, ...mountOptions })
    await w.find('.btn-warning').trigger('click')
    expect(w.find('.throttle-menu').exists()).toBe(true)
    expect(w.text()).toContain('1 流')
    expect(w.text()).toContain('2 流')
    expect(w.text()).toContain('4 流')
    expect(w.text()).toContain('不限')
  })

  // v0.5.0 tests for push-resend functionality
  it('source-push failed with deny reason shows 重发 when request recorded', async () => {
    const { useTransfersStore } = await import('../stores/transfers')
    const store = useTransfersStore()
    store.lastRequest.set('0000000000000001', {
      type: 'push-rel' as any, fp: 'aabb', items: [['C:/a.txt', '']],
    })
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          state: 'failed',
          local_role: 'source-push' as const,
          direction: 'push' as const,
          fail_reason: '推送请求被对方拒绝',
        } as any,
      },
      ...mountOptions,
    })
    expect(w.text()).toContain('推送请求被对方拒绝')
    const btn = w.find('[data-testid="btn-resend"]')
    expect(btn.exists()).toBe(true)
    await btn.trigger('click')
    const { api } = await import('../api')
    expect(api.browse.pushFilesRel).toHaveBeenCalledWith('aabb', [['C:/a.txt', '']])
  })

  it('source-push failed without record shows no 重发', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          state: 'failed',
          local_role: 'source-push' as const,
          direction: 'push' as const,
          fail_reason: '对方超时未确认',
        } as any,
      },
      ...mountOptions,
    })
    expect(w.find('[data-testid="btn-resend"]').exists()).toBe(false)
  })

  // ===== R1 单进度条核对(P1 打磨卡)=====
  it('只渲染一条进度条(发送方积压态也不出现第二条)', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'source-push' as const,
          direction: 'push' as const,
          done: 60, remote_done: 10, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.findAll('.progress-bar').length).toBe(1)
    expect(w.findAll('.progress-fill').length).toBe(1)
  })

  it('发送方积压>20% → 显示"等待对方确认"文案态,不再渲染"网络积压"角标', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'source-push' as const,
          direction: 'push' as const,
          done: 60, remote_done: 10, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.text()).toContain('等待对方确认')
    expect(w.find('[data-testid="transfer-awaiting-confirm"]').exists()).toBe(true)
    expect(w.text()).not.toContain('网络积压')
    expect(w.find('.backlog-badge').exists()).toBe(false)
  })

  it('发送方满格未确认 → 显示"等待对方确认"文案态', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'source-push' as const,
          direction: 'push' as const,
          done: 100, remote_done: 60, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.find('[data-testid="transfer-awaiting-confirm"]').exists()).toBe(true)
  })

  it('发送方正常(积压≤20%) → 不显示"等待对方确认"', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'source-push' as const,
          direction: 'push' as const,
          done: 50, remote_done: 48, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.find('[data-testid="transfer-awaiting-confirm"]').exists()).toBe(false)
  })

  it('发送方已发辅助小字移除(与单进度=对端确认语义冲突)', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'source-push' as const,
          direction: 'push' as const,
          done: 60, remote_done: 10, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.text()).not.toContain('已发')
    expect(w.find('.progress-sent').exists()).toBe(false)
  })

  it('接收方永不显示"等待对方确认"', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          local_role: 'destination' as const,
          done: 50, remote_done: 10, total: 100,
        } as any,
      },
      ...mountOptions,
    })
    expect(w.find('[data-testid="transfer-awaiting-confirm"]').exists()).toBe(false)
  })
})
