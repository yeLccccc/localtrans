<template>
  <div class="modal-backdrop" @click.self="emit('close')">
    <div class="modal-card push-wizard" data-testid="push-wizard">
      <h3>推送文件</h3>

      <!-- 步 1：选设备（仅已配对+在线，单选） -->
      <div v-if="step === 1">
        <p class="wizard-hint">选择要推送到的设备</p>
        <div v-if="selectableDevices.length === 0" class="wizard-empty">
          没有已配对且在线的设备——先在设备页完成配对
        </div>
        <label
          v-for="d in selectableDevices" :key="d.fingerprint"
          class="wizard-device" :class="{ selected: selectedFp === d.fingerprint }"
          :data-testid="`push-wizard-device-${d.fingerprint}`"
        >
          <input type="radio" :value="d.fingerprint" v-model="selectedFp" />
          <span>{{ d.name }}</span>
          <span v-if="d.via_relay" class="badge badge-remote">远程</span>
        </label>
        <div class="modal-actions">
          <button class="btn-secondary" @click="emit('close')">取消</button>
          <button class="btn-primary" data-testid="push-wizard-next-btn" :disabled="!selectedFp" @click="step = 2">下一步</button>
        </div>
      </div>

      <!-- 步 2：选文件 + 确认 -->
      <div v-else>
        <p class="wizard-hint">推送到 <b>{{ targetName }}</b>，共 {{ items.length }} 个文件</p>
        <div class="wizard-buttons">
          <button class="btn-secondary" data-testid="btn-pick-files" @click="pickFiles">选择文件</button>
          <button class="btn-secondary" data-testid="btn-pick-folder" @click="pickFolder">选择文件夹</button>
        </div>
        <ul class="wizard-files">
          <li v-for="(it, i) in items" :key="it[0]">
            <span class="file-path">{{ it[0] }}</span>
            <button class="link-btn" @click="items.splice(i, 1)">移除</button>
          </li>
        </ul>
        <div v-if="expandError" class="error-message">{{ expandError }}</div>
        <div class="modal-actions">
          <button v-if="!presetFingerprint" class="btn-secondary" data-testid="push-wizard-back-btn" @click="step = 1">上一步</button>
          <button class="btn-secondary" @click="emit('close')">取消</button>
          <button class="btn-primary" data-testid="btn-send" :disabled="items.length === 0 || sending" @click="send">
            {{ sending ? '发送中...' : `发送 ${items.length} 个文件` }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { api, formatConnectError } from '../api'
import { useDevicesStore } from '../stores/devices'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { useTransfersStore } from '../stores/transfers'

const props = defineProps<{ presetFingerprint?: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const devicesStore = useDevicesStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()
const transfersStore = useTransfersStore()

const step = ref(props.presetFingerprint ? 2 : 1)
const selectedFp = ref(props.presetFingerprint ?? '')
const items = ref<[string, string][]>([])
const expandError = ref('')
const sending = ref(false)

const selectableDevices = computed(() =>
  devicesStore.devices.filter(d =>
    d.online && settingsStore.trustedPeers.some(p => p.fingerprint === d.fingerprint)))

const targetName = computed(() =>
  devicesStore.devices.find(d => d.fingerprint === selectedFp.value)?.name ?? selectedFp.value)

async function appendPaths(paths: string[] | string | null) {
  if (!paths) return
  const list = Array.isArray(paths) ? paths : [paths]
  if (list.length === 0) return
  expandError.value = ''
  try {
    const expanded = await api.browse.expandLocalPaths(list)
    if (expanded.length === 0) {
      expandError.value = '没有可推送的文件（文件夹可能为空或全是隐藏项）'
      return
    }
    // 去重合并
    const seen = new Set(items.value.map(i => i[0]))
    for (const it of expanded) if (!seen.has(it[0])) items.value.push(it)
  } catch (e) {
    expandError.value = e instanceof Error ? e.message : String(e)
  }
}

async function pickFiles() {
  await appendPaths(await open({ multiple: true, title: '选择要推送的文件' }) as string[] | null)
}

async function pickFolder() {
  await appendPaths(await open({ directory: true, multiple: false, title: '选择要推送的文件夹' }) as string | null)
}

async function send() {
  if (!selectedFp.value || items.value.length === 0) return
  sending.value = true
  try {
    await devicesStore.connectDevice(selectedFp.value)
    const jobId = await api.browse.pushFilesRel(selectedFp.value, items.value)
    transfersStore.recordPushRelRequest(jobId, selectedFp.value, items.value.map(i => [i[0], i[1]]))
    toastStore.push('success', `开始推送 ${items.value.length} 个文件`)
    emit('close')
  } catch (e) {
    toastStore.push('error', formatConnectError(e))
  } finally {
    sending.value = false
  }
}
</script>

<style scoped>
.modal-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(17, 24, 39, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 9999;
}

.modal-card {
  background: white;
  padding: var(--space-6);
  border-radius: var(--radius-md);
  min-width: 320px;
  max-width: 540px;
  box-shadow: var(--shadow-md);
}

.modal-card h3 {
  margin: 0 0 var(--space-4) 0;
  color: var(--gray-800);
}

.wizard-hint {
  color: var(--gray-600);
  margin-bottom: var(--space-4);
}

.wizard-empty {
  padding: var(--space-4);
  background: var(--gray-50);
  border-radius: var(--radius-sm);
  color: var(--gray-500);
  text-align: center;
  margin-bottom: var(--space-4);
}

.wizard-device {
  display: flex;
  align-items: center;
  padding: var(--space-3);
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  margin-bottom: var(--space-2);
  cursor: pointer;
  transition: background-color var(--dur-fast) ease;
}

.wizard-device:hover {
  background: var(--gray-50);
}

.wizard-device.selected {
  background: var(--primary-50);
  border-color: var(--primary-500);
}

.wizard-device input[type="radio"] {
  margin-right: var(--space-3);
  accent-color: var(--primary-600);
}

.wizard-buttons {
  display: flex;
  gap: var(--space-2);
  margin-bottom: var(--space-3);
}

.wizard-files {
  list-style: none;
  padding: 0;
  margin: 0 0 var(--space-4) 0;
  max-height: 200px;
  overflow-y: auto;
}

.wizard-files li {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: var(--space-2);
  border-bottom: 1px solid var(--gray-100);
}

.file-path {
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  color: var(--gray-700);
  flex: 1;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.link-btn {
  background: none;
  border: none;
  color: var(--danger-600);
  cursor: pointer;
  font-size: var(--text-xs);
  padding: var(--space-1);
}

.link-btn:hover {
  text-decoration: underline;
}

.error-message {
  background: var(--danger-50);
  border: 1px solid #fecaca;
  color: var(--danger-600);
  padding: var(--space-2);
  border-radius: var(--radius-sm);
  margin-bottom: var(--space-4);
  font-size: var(--text-sm);
}

.modal-actions {
  display: flex;
  gap: var(--space-2);
  justify-content: flex-end;
}

.btn-secondary {
  padding: var(--space-2) var(--space-4);
  background: var(--gray-100);
  color: var(--gray-700);
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  cursor: pointer;
}

.btn-secondary:hover:not(:disabled) {
  background: var(--gray-200);
}

.btn-primary {
  padding: var(--space-2) var(--space-4);
  background: var(--primary-600);
  color: white;
  border: none;
  border-radius: var(--radius-sm);
  cursor: pointer;
}

.btn-primary:hover:not(:disabled) {
  background: var(--primary-700);
}

.btn-primary:disabled,
.btn-secondary:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.badge {
  display: inline-block;
  padding: 1px 6px;
  border-radius: 4px;
  font-size: 11px;
  font-weight: 500;
  margin-left: 6px;
}

.badge-remote {
  background: var(--accent);
  color: #fff;
}
</style>
