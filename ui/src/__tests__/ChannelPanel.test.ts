/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import ChannelPanel from '../components/ChannelPanel.vue'
import { useDevicesStore } from '../stores/devices'
import type { ChannelDto } from '../types'

vi.mock('@tauri-apps/api/core', () => ({
  invoke: vi.fn(),
}))

import { invoke } from '@tauri-apps/api/core'
const invokeMock = vi.mocked(invoke)

function makeChannel(overrides: Partial<ChannelDto> = {}): ChannelDto {
  return {
    fingerprint: 'fp-device-1',
    addr: '192.168.1.23:47601',
    via_relay: false,
    rtt_ms: 2,
    est_bps: 90_000_000,
    loss_rate: 0,
    current: true,
    score_ready: true,
    probe_disabled: false,
    age_secs: 3,
    ...overrides,
  }
}

function mountPanel() {
  return mount(ChannelPanel, {
    props: { fingerprint: 'fp-device-1', deviceName: '测试设备' },
    global: {
      stubs: { teleport: true },
    },
  })
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.useFakeTimers()
  invokeMock.mockReset()
  invokeMock.mockResolvedValue(null)
})

afterEach(() => {
  vi.useRealTimers()
})

describe('ChannelPanel 通道面板(M3c T2)', () => {
  it('每地址一行:addr/RTT/估速/丢包/当前标记/最近探测时间', () => {
    const store = useDevicesStore()
    store.channels = [
      makeChannel({ addr: '10.8.0.17:51022', via_relay: true, rtt_ms: 120, est_bps: 8_000_000, current: false, age_secs: 65 }),
      makeChannel({ addr: '192.168.1.23:47601', rtt_ms: 2, est_bps: 90_000_000, current: true }),
    ]
    const wrapper = mountPanel()
    // 当前置首
    const rows = wrapper.findAll('[data-testid^="channel-row-"]')
    expect(rows.length).toBe(2)
    expect(rows[0].attributes('data-testid')).toBe('channel-row-192.168.1.23:47601')
    expect(rows[0].text()).toContain('✓ 当前')
    expect(rows[0].text()).toContain('2ms')
    expect(rows[0].text()).toContain('90.0 Mbps')
    expect(rows[0].text()).toContain('刚刚')
    // 中继行:经中继标注 + 估速 Kbps 档 + 分前
    expect(rows[1].text()).toContain('中继')
    expect(rows[1].text()).toContain('120ms')
    expect(rows[1].text()).toContain('8.0 Mbps')
    expect(rows[1].text()).toContain('1分前')
  })

  it('估速 humanize:Kbps 与缺测', () => {
    const store = useDevicesStore()
    store.channels = [
      makeChannel({ est_bps: 850_000 }),
      makeChannel({ addr: '10.8.0.17:51022', est_bps: null, current: false }),
    ]
    const wrapper = mountPanel()
    expect(wrapper.text()).toContain('850 Kbps')
    expect(wrapper.text()).toContain('—')
  })

  it('空表显示空态,重测按钮禁用(无会话)', () => {
    const store = useDevicesStore()
    store.channels = []
    const wrapper = mountPanel()
    expect(wrapper.text()).toContain('暂无通道记录')
    const btn = wrapper.find('[data-testid="channel-reprobe-btn"]')
    expect((btn.element as HTMLButtonElement).disabled).toBe(true)
  })

  it('重新探测按钮调用 probe_now_peer 并刷新通道表', async () => {
    const store = useDevicesStore()
    store.channels = [makeChannel()]
    const wrapper = mountPanel()
    await wrapper.find('[data-testid="channel-reprobe-btn"]').trigger('click')
    await flushPromises()
    expect(invokeMock).toHaveBeenCalledWith('probe_now_peer', { fingerprint: 'fp-device-1' })
    expect(wrapper.text()).toContain('已完成')
    // 立即 + 3s 后各补刷一次(刷新走 list_channels)
    const listCalls = () => invokeMock.mock.calls.filter(c => c[0] === 'list_channels').length
    expect(listCalls()).toBe(1)
    vi.advanceTimersByTime(3_100)
    await flushPromises()
    expect(listCalls()).toBe(2)
  })

  it('探测失败显示错误文案不崩', async () => {
    const store = useDevicesStore()
    store.channels = [makeChannel()]
    invokeMock.mockRejectedValue('设备未连接,无法探测')
    const wrapper = mountPanel()
    await wrapper.find('[data-testid="channel-reprobe-btn"]').trigger('click')
    await flushPromises()
    expect(wrapper.text()).toContain('设备未连接,无法探测')
  })

  it('关闭按钮发出 close 事件', async () => {
    const store = useDevicesStore()
    store.channels = [makeChannel()]
    const wrapper = mountPanel()
    await wrapper.find('[data-testid="channel-panel-close-btn"]').trigger('click')
    expect(wrapper.emitted('close')).toBeTruthy()
  })
})
