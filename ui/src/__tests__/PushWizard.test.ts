/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import PushWizard from '../components/PushWizard.vue'

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn().mockResolvedValue(['C:/a.txt', 'C:/b.txt']),
}))
vi.mock('../api', () => ({
  api: {
    browse: {
      expandLocalPaths: vi.fn().mockResolvedValue([
        ['C:/a.txt', ''], ['C:/b.txt', ''],
      ]),
      pushFilesRel: vi.fn().mockResolvedValue('0x64'),
      connectDevice: vi.fn(),
    },
  },
}))
vi.mock('../stores/devices', () => ({
  useDevicesStore: () => ({
    devices: [
      { fingerprint: 'f1', name: '电脑甲', online: true },
      { fingerprint: 'f2', name: '电脑乙', online: false },
    ],
  }),
}))
vi.mock('../stores/settings', () => ({
  useSettingsStore: () => ({
    trustedPeers: [
      { fingerprint: 'f1', perms: {} },
      { fingerprint: 'f2', perms: {} },
    ],
  }),
}))
vi.mock('../stores/toast', () => ({
  useToastStore: () => ({ push: vi.fn() }),
}))

describe('PushWizard', () => {
  beforeEach(() => { setActivePinia(createPinia()) })

  it('step1 lists only trusted+online devices', async () => {
    const w = mount(PushWizard)
    expect(w.text()).toContain('电脑甲')
    expect(w.text()).not.toContain('电脑乙')
  })

  it('presetFingerprint skips to file step', async () => {
    const w = mount(PushWizard, { props: { presetFingerprint: 'f1' } })
    expect(w.find('[data-testid="btn-pick-files"]').exists()).toBe(true)
  })
})
