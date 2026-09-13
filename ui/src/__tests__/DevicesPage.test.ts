/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'

// T4:Devices 页用 listen(探测回执)与 getCurrentWebview(页面拖拽),jsdom 下都 mock 掉
vi.mock('@tauri-apps/api/event', () => ({
  listen: vi.fn().mockResolvedValue(() => {}),
}))
vi.mock('@tauri-apps/api/webview', () => ({
  getCurrentWebview: () => ({
    onDragDropEvent: vi.fn().mockResolvedValue(() => {}),
  }),
}))

// 名片复制/粘贴添加走 api.devices(getBusinessCard/addByCard),mock 整个 api 模块
// (Devices.vue 与 devices/toast/transfers/settings store 均从 '../api' 取依赖)
vi.mock('../api', () => ({
  api: {
    devices: {
      list: vi.fn().mockResolvedValue([]),
      listChannels: vi.fn().mockResolvedValue([]),
      addManual: vi.fn().mockResolvedValue(undefined),
      getBusinessCard: vi.fn().mockResolvedValue(''),
      addByCard: vi.fn(),
    },
    pairing: { getPending: vi.fn().mockResolvedValue([]) },
  },
  systemApi: {
    getNetworkStatus: vi.fn().mockResolvedValue({ local_ip: '192.168.1.5' }),
  },
  onDeviceList: vi.fn(),
  onConnectionState: vi.fn(),
  onPeerReconnecting: vi.fn(),
  onToast: vi.fn(),
  onTransferProgress: vi.fn(),
}))

import DevicesPage from '../pages/Devices.vue'
import { useToastStore } from '../stores/toast'
import { api } from '../api'

const FP = '3f2a4b5c6d7e8f90112233445566778899aabbccddeeff001122334455667788'
// 与 core business_card to_text 钉死格式一致的双地址名片
const CARD = [
  'LocalTrans 名片',
  '名称: huss_pc',
  `指纹: ${FP}`,
  '地址: 192.168.1.10:47601',
  '地址: 10.8.0.5:47601',
].join('\n')

const invokeOf = () => vi.mocked(api.devices)

/** jsdom 无 clipboard,挂一个可断言的 writeText */
const writeText = vi.fn().mockResolvedValue(undefined)
beforeEach(() => {
  setActivePinia(createPinia())
  writeText.mockClear()
  Object.defineProperty(navigator, 'clipboard', {
    value: { writeText },
    configurable: true,
  })
  invokeOf().getBusinessCard.mockResolvedValue(CARD)
  invokeOf().addByCard.mockReset()
  invokeOf().addManual.mockClear()
  invokeOf().list.mockClear()
})

async function mountPage() {
  const w = mount(DevicesPage)
  await flushPromises() // onMounted:refreshDevices + getNetworkStatus + 监听器
  return w
}

async function openManualAdd(w: ReturnType<typeof mount>) {
  await w.find('[data-testid="devices-manual-add-open-btn"]').trigger('click')
}

describe('DevicesPage 本机 IP 徽章一键复制名片(M3c T4)', () => {
  it('点击徽章:getBusinessCard → 剪贴板写入名片全文 → toast「名片已复制（含 N 个地址）」', async () => {
    const w = await mountPage()
    const toast = useToastStore()
    await w.find('[data-testid="devices-local-ip-chip"]').trigger('click')
    await flushPromises()

    expect(api.devices.getBusinessCard).toHaveBeenCalled()
    expect(writeText).toHaveBeenCalledWith(CARD)
    expect(toast.toasts.some(t => t.level === 'success' && t.text === '名片已复制（含 2 个地址）')).toBe(true)
  })

  it('名片获取失败:回退复制裸 IP(旧行为 toast)', async () => {
    invokeOf().getBusinessCard.mockRejectedValue(new Error('命令失败'))
    const w = await mountPage()
    const toast = useToastStore()
    await w.find('[data-testid="devices-local-ip-chip"]').trigger('click')
    await flushPromises()

    expect(writeText).toHaveBeenCalledWith('192.168.1.5')
    expect(toast.toasts.some(t => t.text.includes('已复制 192.168.1.5'))).toBe(true)
  })
})

describe('DevicesPage 手动添加支持粘名片全文(M3c T4)', () => {
  it('粘名片全文:走 add_by_card → toast「已通过名片添加，正在探测 N 个地址」并关弹窗', async () => {
    invokeOf().addByCard.mockResolvedValue({ accepted: true, addresses_tried: 2 })
    const w = await mountPage()
    const toast = useToastStore()
    await openManualAdd(w)
    await w.find('[data-testid="devices-manual-addr-input"]').setValue(CARD)
    await w.find('[data-testid="devices-manual-submit-btn"]').trigger('click')
    await flushPromises()

    expect(api.devices.addByCard).toHaveBeenCalledWith(CARD)
    expect(toast.toasts.some(t => t.text === '已通过名片添加，正在探测 2 个地址')).toBe(true)
    expect(w.find('.dialog').exists()).toBe(false)
  })

  it('纯 IP(非名片):回退 add_manual_device,裸 IP 补默认发现端口', async () => {
    invokeOf().addByCard.mockRejectedValue(new Error('名片解析失败: 名片缺少「名称」行'))
    const w = await mountPage()
    await openManualAdd(w)
    await w.find('[data-testid="devices-manual-addr-input"]').setValue('192.168.1.99')
    await w.find('[data-testid="devices-manual-submit-btn"]').trigger('click')
    await flushPromises()

    expect(api.devices.addManual).toHaveBeenCalledWith('192.168.1.99:47600')
  })

  it('本机自身名片(accepted=false):弹窗内提示,不误报成功', async () => {
    invokeOf().addByCard.mockResolvedValue({ accepted: false, addresses_tried: 0 })
    const w = await mountPage()
    await openManualAdd(w)
    await w.find('[data-testid="devices-manual-addr-input"]').setValue(CARD)
    await w.find('[data-testid="devices-manual-submit-btn"]').trigger('click')
    await flushPromises()

    expect(w.find('.dialog').exists()).toBe(true)
    expect(w.find('.error-message').text()).toContain('本机自己的名片')
  })

  it('既非名片也非 IP:弹窗内格式错误提示', async () => {
    invokeOf().addByCard.mockRejectedValue(new Error('名片解析失败: 名片缺少「名称」行'))
    const w = await mountPage()
    await openManualAdd(w)
    await w.find('[data-testid="devices-manual-addr-input"]').setValue('随便打的一句话')
    await w.find('[data-testid="devices-manual-submit-btn"]').trigger('click')
    await flushPromises()

    expect(w.find('.dialog').exists()).toBe(true)
    expect(w.find('.error-message').text()).toContain('无法识别')
  })
})
