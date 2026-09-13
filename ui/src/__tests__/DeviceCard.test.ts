/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import DeviceCard from '../components/DeviceCard.vue'
import { useDevicesStore } from '../stores/devices'
import { useSettingsStore } from '../stores/settings'
import type { ChannelDto, DeviceDto } from '../types'

// DeviceCard setup 里用了 vue-router(useRouter),stub 掉
vi.mock('vue-router', () => ({
  useRouter: () => ({ push: vi.fn() })
}))

// M3c T3:强制走中继开关走 invoke(其余 invoke 由组件树内不发)
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

function mountCard(online: boolean, connected = false, overrides: Partial<DeviceDto> = {}) {
  return mount(DeviceCard, {
    props: {
      device: {
        fingerprint: 'fp-device-1',
        name: '测试设备',
        addr: '192.168.1.23:47601',
        online,
        connected,
        ...overrides,
      },
    },
  })
}

beforeEach(() => {
  setActivePinia(createPinia())
  // DeviceCard 读 trustedPeers(权限/信任徽章)
  const settingsStore = useSettingsStore()
  settingsStore.trustedPeers = []
})

describe('DeviceCard 通道标签(M3c T1 三态)', () => {
  it('直连通道:绿点 + 「直连 · 2ms」', () => {
    const store = useDevicesStore()
    store.channels = [makeChannel({ via_relay: false, rtt_ms: 2 })]
    const wrapper = mountCard(true, true)
    const label = wrapper.find('[data-testid="device-channel-label"]')
    expect(label.text()).toBe('直连 · 2ms')
    expect(label.find('.channel-dot.direct').exists()).toBe(true)
  })

  it('经中继通道:「经中继 · 120ms」', () => {
    const store = useDevicesStore()
    store.channels = [makeChannel({ via_relay: true, rtt_ms: 120, addr: '10.8.0.17:51022' })]
    const wrapper = mountCard(true, true)
    const label = wrapper.find('[data-testid="device-channel-label"]')
    expect(label.text()).toBe('经中继 · 120ms')
    expect(label.find('.channel-dot.relay').exists()).toBe(true)
  })

  it('无通道记录(离线常驻卡):「未知」', () => {
    const store = useDevicesStore()
    store.channels = []
    const wrapper = mountCard(false)
    const label = wrapper.find('[data-testid="device-channel-label"]')
    expect(label.text()).toBe('未知')
    expect(label.find('.channel-dot.unknown').exists()).toBe(true)
  })

  it('记录存在但 RTT 未测得:只显示路径不显示耗时', () => {
    const store = useDevicesStore()
    store.channels = [makeChannel({ rtt_ms: null })]
    const wrapper = mountCard(true, true)
    expect(wrapper.find('[data-testid="device-channel-label"]').text()).toBe('直连')
  })

  it('当前通道优先;无 current 记录时按发现地址匹配', () => {
    const store = useDevicesStore()
    store.channels = [
      makeChannel({ addr: '10.8.0.17:51022', via_relay: true, rtt_ms: 120, current: false }),
      makeChannel({ addr: '192.168.1.23:47601', via_relay: false, rtt_ms: 2, current: false }),
    ]
    const wrapper = mountCard(true, true)
    // 设备发现地址 = 192.168.1.23:47601 → 匹配直连记录
    expect(wrapper.find('[data-testid="device-channel-label"]').text()).toBe('直连 · 2ms')
  })
})

describe('DeviceCard 通道面板入口(M3c T2)', () => {
  it('点击通道标签弹出通道面板,面板含每地址行', async () => {
    const store = useDevicesStore()
    store.channels = [makeChannel()]
    const wrapper = mountCard(true, true)
    expect(wrapper.find('[data-testid="device-channel-panel"]').exists()).toBe(false)
    await wrapper.find('[data-testid="device-channel-label"]').trigger('click')
    expect(wrapper.find('[data-testid="device-channel-panel"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="channel-row-192.168.1.23:47601"]').exists()).toBe(true)
  })

  it('面板关闭按钮收回面板', async () => {
    const store = useDevicesStore()
    store.channels = [makeChannel()]
    const wrapper = mountCard(true, true)
    await wrapper.find('[data-testid="device-channel-label"]').trigger('click')
    expect(wrapper.find('[data-testid="device-channel-panel"]').exists()).toBe(true)
    await wrapper.find('[data-testid="channel-panel-close-btn"]').trigger('click')
    expect(wrapper.find('[data-testid="device-channel-panel"]').exists()).toBe(false)
  })
})

describe('DeviceCard 强制走中继(M3c T3,发布态 RELAY_ENABLED=false 隐藏)', () => {
  /** 已信任设备才能展开 ⋮ 菜单 */
  function mountTrusted(forceRelay: boolean) {
    const settings = useSettingsStore()
    settings.trustedPeers = [{
      fingerprint: 'fp-device-1',
      name: '测试设备',
      alias: '',
      paired_at: 1,
      browse: true,
      download: true,
      push: 'ask',
    }]
    return mountCard(true, true, { force_relay: forceRelay })
  }

  beforeEach(() => {
    invokeMock.mockReset()
    invokeMock.mockResolvedValue(true)
  })

  it('发布态隐藏:角标与 ⋮ 勾选项不渲染,无任何 set_force_relay 入口', async () => {
    const on = mountTrusted(true)
    expect(on.find('[data-testid="device-force-relay-badge"]').exists()).toBe(false)
    await on.find('[data-testid="device-menu-btn"]').trigger('click')
    expect(on.find('[data-testid="device-force-relay-toggle"]').exists()).toBe(false)
    const off = mountTrusted(false)
    expect(off.find('[data-testid="device-force-relay-badge"]').exists()).toBe(false)
    expect(invokeMock).not.toHaveBeenCalled()
  })
})

describe('DeviceCard 独立推送按钮(M3c T4)', () => {
  /** 已信任设备的挂载 helper(推送按钮只对已配对卡有意义) */
  function mountTrusted(online: boolean) {
    const settings = useSettingsStore()
    settings.trustedPeers = [{
      fingerprint: 'fp-device-1',
      name: '测试设备',
      alias: '',
      paired_at: 1,
      browse: true,
      download: true,
      push: 'ask',
    }]
    return mountCard(online, online)
  }

  it('已配对且在线:显示「推送」按钮,点击 emit request-push(Devices.vue 接线直开向导步 2)', async () => {
    const wrapper = mountTrusted(true)
    const btn = wrapper.find('[data-testid="device-push-btn"]')
    expect(btn.exists()).toBe(true)
    expect(btn.text()).toBe('推送')
    await btn.trigger('click')
    expect(wrapper.emitted('request-push')).toEqual([['fp-device-1']])
  })

  it('已配对但离线:不显示推送按钮(浏览仍在、置灰)', () => {
    const wrapper = mountTrusted(false)
    expect(wrapper.find('[data-testid="device-push-btn"]').exists()).toBe(false)
    const browse = wrapper.find('[data-testid="device-browse-btn"]')
    expect(browse.exists()).toBe(true)
    expect((browse.element as HTMLButtonElement).disabled).toBe(true)
  })

  it('未配对(待配对卡):只有连接按钮,无推送入口', () => {
    const wrapper = mountCard(true)
    expect(wrapper.find('[data-testid="device-push-btn"]').exists()).toBe(false)
    expect(wrapper.find('[data-testid="device-connect-btn"]').exists()).toBe(true)
  })
})

describe('DeviceCard 配对冷却(P2 T2)', () => {
  it('冷却期内:连接按钮禁用+倒计时徽章,过期后恢复', () => {
    const store = useDevicesStore()
    // 无冷却:正常连接钮
    const w0 = mountCard(true)
    expect(w0.find('[data-testid="device-cooldown-badge"]').exists()).toBe(false)
    expect((w0.find('[data-testid="device-connect-btn"]').element as HTMLButtonElement).disabled).toBe(false)

    // 记入 120s 冷却:徽章出现、按钮禁用、文案带倒计时
    store.markCooldown('fp-device-1', 120)
    const w1 = mountCard(true)
    expect(w1.find('[data-testid="device-cooldown-badge"]').exists()).toBe(true)
    expect(w1.find('[data-testid="device-cooldown-badge"]').text()).toContain('120')
    const btn = w1.find('[data-testid="device-connect-btn"]')
    expect((btn.element as HTMLButtonElement).disabled).toBe(true)
    expect(btn.text()).toContain('冷却')

    // 过期(0s)后恢复
    store.markCooldown('fp-device-1', 0)
    const w2 = mountCard(true)
    expect(w2.find('[data-testid="device-cooldown-badge"]').exists()).toBe(false)
    expect((w2.find('[data-testid="device-connect-btn"]').element as HTMLButtonElement).disabled).toBe(false)
  })
})
