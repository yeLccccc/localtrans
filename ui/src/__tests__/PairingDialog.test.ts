/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import PairingDialog from '../components/PairingDialog.vue'
import { useSettingsStore } from '../stores/settings'

// Mock Tauri APIs
const eventListeners = new Map<string, Set<(event: { payload: unknown }) => void>>()
const invokeMock = vi.fn()

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (cmd: string, _args?: Record<string, unknown>) => invokeMock(cmd, _args)
}))

vi.mock('@tauri-apps/api/event', () => ({
  listen: (eventName: string, callback: (event: { payload: unknown }) => void) => {
    if (!eventListeners.has(eventName)) {
      eventListeners.set(eventName, new Set())
    }
    eventListeners.get(eventName)!.add(callback)
    return Promise.resolve(() => {
      eventListeners.get(eventName)?.delete(callback)
    })
  }
}))

function emitEvent(name: string, payload: unknown) {
  eventListeners.get(name)?.forEach(callback => callback({ payload }))
}

beforeEach(() => {
  setActivePinia(createPinia())
  vi.clearAllMocks()
  eventListeners.clear()

  // Initialize settings store with default config
  const settingsStore = useSettingsStore()
  settingsStore.config = {
    device_name: 'Test Device',
    download_dir: '/tmp',
    hidden: false,
    quic_port: 47601,
    discovery_port: 47600,
    shares: [],
    consent_timeout_secs: 60
  }

  // Setup invoke mock to return expected values
  invokeMock.mockImplementation(async (cmd: string, _args?: Record<string, unknown>) => {
    if (cmd === 'grant_consent') return { own_code: '482913' }
    if (cmd === 'submit_pair_code') return true
    return undefined
  })
})

describe('PairingDialog 同意门', () => {
  it('B 侧:收到 consent-needed 弹同意门,点同意后亮码(无输入框)', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()

    emitEvent('pairing-consent-needed', { fingerprint: 'aa', name: '甲的电脑' })
    await flushPromises()

    expect(wrapper.text()).toContain('甲的电脑')
    expect(wrapper.text()).toContain('同意')

    // 点同意
    const btn = wrapper.find('[data-testid="btn-grant"]')
    await btn.trigger('click')
    await flushPromises()

    expect(invokeMock).toHaveBeenCalledWith('grant_consent', { fingerprint: 'aa' })
    // 亮码,无输入框 (CodeBadge 会格式化为 "48 29 13")
    expect(wrapper.text()).toContain('48')
    expect(wrapper.text()).toContain('29')
    expect(wrapper.text()).toContain('13')
    expect(wrapper.find('[data-testid="code-input"]').exists()).toBe(false)
    // 有结束等待按钮
    expect(wrapper.find('[data-testid="btn-cancel-wait"]').exists()).toBe(true)
  })

  it('A 侧:wait → code-entry 流转,输入并提交', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()

    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙的电脑' })
    await flushPromises()
    expect(wrapper.text()).toContain('等待')

    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙的电脑' })
    await flushPromises()
    expect(wrapper.find('[data-testid="code-input"]').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('本机')  // A 不显示本机码

    const input = wrapper.find('[data-testid="code-input"]')
    await input.setValue('556677')
    await wrapper.find('[data-testid="btn-submit"]').trigger('click')
    await flushPromises()
    expect(invokeMock).toHaveBeenCalledWith('submit_pair_code', {
      fingerprint: 'bb', peerCode: '556677',
    })
  })

  it('A 侧:收到 deny 结果进失败态(建议文案+重试直达),可关闭', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-result', { fingerprint: 'bb', ok: false, reason: '对方拒绝连接' })
    await flushPromises()
    // P2:终态失败不再直接关弹窗——显示建议文案与「重新发起」
    expect(wrapper.find('.pairing-dialog').exists()).toBe(true)
    expect(wrapper.find('[data-testid="pairing-failed-reason"]').text())
      .toContain('对方拒绝了本次配对请求')
    expect(wrapper.find('[data-testid="pairing-retry-btn"]').exists()).toBe(true)
    // 关闭按钮收敛
    await wrapper.find('[data-testid="pairing-failed-close-btn"]').trigger('click')
    await flushPromises()
    expect(wrapper.find('.pairing-dialog').exists()).toBe(false)
  })

  it('A 侧:错码第 1 次回输码态并提示剩余 2 次(分层反馈)', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    const input = wrapper.find('[data-testid="code-input"]')
    await input.setValue('556677')
    await wrapper.find('[data-testid="btn-submit"]').trigger('click')
    await flushPromises()
    // 提交后进 submitted;错码结果回 entry + 剩余次数
    emitEvent('pairing-result', { fingerprint: 'bb', ok: false, reason: '配对码不匹配' })
    await flushPromises()
    expect(wrapper.find('[data-testid="code-input"]').exists()).toBe(true)
    expect(wrapper.find('[data-testid="code-error"]').text()).toBe('配对码不匹配，还可重试 2 次')
  })

  it('A 侧:错码第 3 次终态关闭并提示冷却(镜像设备卡冷却)', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    for (let i = 0; i < 3; i++) {
      const el = wrapper.find('[data-testid="code-input"]')
      await el.setValue('111111')
      await wrapper.find('[data-testid="btn-submit"]').trigger('click')
      await flushPromises()
      emitEvent('pairing-result', { fingerprint: 'bb', ok: false, reason: '配对码不匹配' })
      await flushPromises()
    }
    // 第 3 次:弹窗关闭 + 冷却提示 toast
    expect(wrapper.find('.pairing-dialog').exists()).toBe(false)
    const { useToastStore } = await import('../stores/toast')
    const toasts = useToastStore().toasts.map(t => t.text).join('|')
    expect(toasts).toContain('配对码连续错误 3 次')
    expect(toasts).toContain('5 分钟冷却')
  })

  it('A 侧:第 3 次错码终局消息竞态丢失时,断连兜底按冷却终态收敛', async () => {
    // core 实证:FailedOut 的 PairResult 可能没跑赢连接关闭,只剩
    // SessionDown(connection-state up:false)。本地计数已满 3 → 冷却终态。
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    for (let i = 0; i < 2; i++) {
      await wrapper.find('[data-testid="code-input"]').setValue('111111')
      await wrapper.find('[data-testid="btn-submit"]').trigger('click')
      await flushPromises()
      emitEvent('pairing-result', { fingerprint: 'bb', ok: false, reason: '配对码不匹配' })
      await flushPromises()
    }
    // 第 3 次提交后无 mismatch 事件,直接断连
    await wrapper.find('[data-testid="code-input"]').setValue('111111')
    await wrapper.find('[data-testid="btn-submit"]').trigger('click')
    await flushPromises()
    emitEvent('connection-state', { fingerprint: 'bb', up: false })
    await flushPromises()
    expect(wrapper.find('.pairing-dialog').exists()).toBe(false)
    const { useToastStore } = await import('../stores/toast')
    const toasts = useToastStore().toasts.map(t => t.text).join('|')
    // 概率形态:文案两可(码错误或离线),冷却镜像照记
    expect(toasts).toContain('配对已终止')
    expect(toasts).toContain('冷却')
    // 未满 3 次的断连仍是可重试失败态(对照组已在 deny 用例覆盖)
  })

  it('输码框过滤非数字且最长 6 位', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    const input = wrapper.find('[data-testid="code-input"]')
    await input.setValue('ab12c34d56789')
    expect((input.element as HTMLInputElement).value).toBe('123456')
    // 提交按钮就绪
    expect(wrapper.find('[data-testid="btn-submit"]').attributes('disabled')).toBeUndefined()
  })

  it('输码/等待中连接断开(connection-state up:false)进失败态', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('connection-state', { fingerprint: 'bb', up: false })
    await flushPromises()
    expect(wrapper.find('[data-testid="pairing-failed-reason"]').text())
      .toContain('对方可能已离线或取消')
  })
})
