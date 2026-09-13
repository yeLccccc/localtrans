<template>
  <div class="devices-page">
    <div class="page-header">
      <h1>设备</h1>
      <div class="header-actions">
        <button
          class="local-ip-chip"
          data-testid="devices-local-ip-chip"
          @click="copyLocalIp"
          title="点击复制本机名片（含全部直连地址），发给对方粘贴到手动添加即可发现你"
        >
          本机 {{ localIp || 'IP 获取中...' }}
        </button>

        <div class="switch-container">
          <label class="switch">
            <input
              type="checkbox"
              data-testid="devices-hidden-toggle"
              :checked="isHidden"
              @change="handleHiddenToggle"
            />
            <span class="slider"></span>
          </label>
          <span class="switch-label">隐身</span>
        </div>

        <button
          class="btn btn-secondary"
          data-testid="devices-manual-add-open-btn"
          @click="showManualAddDialog = true"
          title="手动添加设备"
        >
          +
        </button>

        <button
          class="btn btn-primary"
          @click="pushWizardOpen = true"
          data-testid="btn-push-wizard"
        >
          推送文件
        </button>

      </div>
    </div>

    <div v-if="devicesStore.loading" class="loading-state">
      <div class="loading-spinner"></div>
      <p>加载设备列表中...</p>
    </div>

    <div v-else-if="devicesStore.error" class="error-state">
      <p>{{ devicesStore.error }}</p>
      <button class="btn btn-primary" data-testid="devices-error-retry-btn" @click="devicesStore.refreshDevices()">
        重试
      </button>
    </div>

    <div v-else-if="devicesStore.devices.length === 0" class="empty-state">
      <p>暂无设备</p>
      <p class="empty-hint">请点击"刷新"按钮探测网络中的设备</p>
    </div>

    <div v-else class="devices-grid">
      <DeviceCard
        v-for="device in devicesStore.devices"
        :key="device.fingerprint"
        :device="device"
        :is-drag-over="dragOverFp === device.fingerprint"
        @request-push="cardPushFp = $event"
      />
    </div>

    <!-- 手动添加设备对话框 -->
    <div v-if="showManualAddDialog" class="dialog-overlay" @click.self="showManualAddDialog = false">
      <div class="dialog">
        <div class="dialog-header">
          <h2>手动添加设备</h2>
          <button class="close-btn" @click="showManualAddDialog = false">×</button>
        </div>

        <div class="dialog-content">
          <div class="form-group">
            <label>设备地址 / 名片</label>
            <!-- T4:textarea 支持整段粘贴名片全文(单行 input 粘贴会把换行折成空格,名片解析不了) -->
            <textarea
              v-model="manualDeviceAddress"
              class="form-input manual-add-input"
              rows="3"
              data-testid="devices-manual-addr-input"
              placeholder="IP、IP:端口，或直接粘贴对方名片全文"
            ></textarea>
            <div class="form-hint">可整段粘贴对方名片（对方在设备页点「本机」徽章一键复制）；只填 IP 时使用默认发现端口 47600</div>
            <div class="form-hint">对方需在线且未开启隐身才会出现；把你的名片（点右上角「本机 {{ localIp || '?' }}」复制）发给对方即可让对方加你</div>
          </div>

          <div v-if="manualAddError" class="error-message">
            {{ manualAddError }}
          </div>

          <div class="dialog-actions">
            <button
              class="btn btn-secondary"
              data-testid="devices-manual-cancel-btn"
              @click="showManualAddDialog = false"
            >
              取消
            </button>
            <button
              class="btn btn-primary"
              data-testid="devices-manual-submit-btn"
              @click="handleAddManualDevice"
              :disabled="!manualDeviceAddress || isAddingManual"
            >
              {{ isAddingManual ? '添加中...' : '添加' }}
            </button>
          </div>
        </div>
      </div>
    </div>

    <!-- 推送向导 -->
    <PushWizard v-if="pushWizardOpen" @close="pushWizardOpen = false" />
    <PushWizard v-if="cardPushFp" :preset-fingerprint="cardPushFp" @close="cardPushFp = null" />
  </div>
</template>

<script setup lang="ts">
import { ref, onMounted, onUnmounted } from 'vue'
import { listen } from '@tauri-apps/api/event'
import { useDevicesStore } from '../stores/devices'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { useTransfersStore } from '../stores/transfers'
import { api, formatConnectError } from '../api'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { systemApi } from '../api'
import DeviceCard from '../components/DeviceCard.vue'
import PushWizard from '../components/PushWizard.vue'

const devicesStore = useDevicesStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()
const transfersStore = useTransfersStore()

// 本机 IP（展示 + 一键复制给对方）
const localIp = ref<string | null>(null)

// 手动添加对话框状态
const showManualAddDialog = ref(false)

// 手动设备地址
const manualDeviceAddress = ref('')

// 手动添加错误信息
const manualAddError = ref('')

// 添加状态
const isAddingManual = ref(false)

// 探测状态

// 推送向导状态
const pushWizardOpen = ref(false)
const cardPushFp = ref<string | null>(null)

// 探测结果事件监听器
let probeResultUnlisten: (() => void) | null = null

// A7 剪贴板自清定时器(45s 后清空,防名片/IP 长期驻留)
let clipboardClearTimer: ReturnType<typeof setTimeout> | null = null

/**
 * T4:一键复制本机名片全文(设备名+指纹+全部直连地址,对方粘贴到手动
 * 添加即整段接受,跨网段也能加)。名片获取/写剪贴板失败 → 回退复制裸 IP
 * (旧行为)。A7:名片与 IP 同策略,45 秒后自动清空剪贴板。
 */
async function copyLocalIp() {
  if (!localIp.value) return
  try {
    const card = await api.devices.getBusinessCard()
    // 数名片「地址:」行,toast 告诉用户覆盖了几个直连地址
    const addrCount = card.split('\n').filter(l => l.trim().startsWith('地址')).length
    await navigator.clipboard.writeText(card)
    toastStore.push('success', `名片已复制（含 ${addrCount} 个地址）`)
  } catch {
    try {
      await navigator.clipboard.writeText(localIp.value)
      toastStore.push('success', '已复制 ' + localIp.value + '，发给对方手动添加即可(45 秒后自动清空)')
    } catch {
      toastStore.push('error', '复制失败，请手动抄下: ' + localIp.value)
    }
  }
  if (clipboardClearTimer !== null) clearTimeout(clipboardClearTimer)
  clipboardClearTimer = setTimeout(() => {
    navigator.clipboard.writeText('').catch(() => {})
  }, 45000)
}

// 是否隐身
const isHidden = ref(settingsStore.config?.hidden ?? false)

// 当前拖拽悬停的设备指纹
const dragOverFp = ref<string | null>(null)

// 拖拽事件监听器
let dragDropUnlisten: (() => void) | null = null

/**
 * 处理隐身开关切换
 */
async function handleHiddenToggle() {
  const newValue = !isHidden.value

  try {
    await devicesStore.setHidden(newValue)
    isHidden.value = newValue
    // 重新加载设置以更新状态
    await settingsStore.refreshSettings()
  } catch (error) {
    toastStore.push('error', '切换隐身模式失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 立即探测网络
 */
/**
 * 添加手动设备。T4:输入支持对方名片全文或 IP[:端口]——
 * 先按名片解析(add_by_card,成功即逐地址单播探测;本机自身名片
 * accepted=false 提示不误报);解析失败(非名片)回退按 IP 处理
 * (旧行为:裸 IP 补发现端口后 add_manual_device)。
 */
async function handleAddManualDevice() {
  const text = manualDeviceAddress.value.trim()
  if (!text) {
    manualAddError.value = '请输入设备地址或名片全文'
    return
  }

  isAddingManual.value = true
  manualAddError.value = ''

  try {
    // 名片优先:能解析就整段接受
    try {
      const res = await api.devices.addByCard(text)
      if (!res.accepted) {
        manualAddError.value = '这是本机自己的名片，无需添加自己'
        return
      }
      showManualAddDialog.value = false
      manualDeviceAddress.value = ''
      // 逐地址探测的"对方是否出现"仍由 manual-probe-result 事件逐条回执
      toastStore.push('success', `已通过名片添加，正在探测 ${res.addresses_tried} 个地址`)
      return
    } catch {
      // 非名片文本 → 回退按 IP 处理
    }

    // 验证地址格式
    const addressPattern = /^[\d.]+:\d+$|^[a-fA-F0-9:]+:\d+$|^[\d.]+$/
    if (!addressPattern.test(text)) {
      manualAddError.value = '无法识别：既不是有效名片，也不符合 IP:端口 或 IP 格式'
      return
    }

    let addr = text

    // 如果只输入了 IP，没有端口，则添加默认端口
    if (!addr.includes(':')) {
      // 尝试从配置中获取端口，或使用默认发现端口 47600
      const defaultPort = settingsStore.config?.discovery_port ?? 47600
      addr = `${addr}:${defaultPort}`
    }

    await devicesStore.addManualDevice(addr)
    showManualAddDialog.value = false
    manualDeviceAddress.value = ''
    // 探测是异步单播：结果由 5 秒后的 manual-probe-result 事件明确反馈
    toastStore.push('info', '已向 ' + addr + ' 发送探测，等待对方回应...')
  } catch (error) {
    manualAddError.value = error instanceof Error ? error.message : '添加设备失败'
  } finally {
    isAddingManual.value = false
  }
}

/**
 * 探测结果回查（后端 add_manual_device 5 秒后 emit）
 * 把"对方到底出现没有"明确告诉用户，而不是让人盯着列表猜
 */
async function setupProbeResultListener() {
  probeResultUnlisten = await listen<{ target: string; found: boolean }>(
    'manual-probe-result',
    (e) => {
      const { target, found } = e.payload
      if (found) {
        toastStore.push('success', `对方 ${target} 已回应，已加入设备列表`)
      } else {
        toastStore.push(
          'warning',
          `5 秒内未收到 ${target} 回应：对方未开启 LocalTrans、隐身中，或防火墙拦截（设置页→防火墙 可体检）`
        )
      }
    }
  )
}

/**
 * 推送文件/文件夹到设备。
 * v0.2.6：Rust 侧展开文件夹（含子目录结构），push_files_rel 带相对目录推送——
 * 接收方按原目录树落盘。拖进来的路径若全是文件则行为与旧版一致。
 */
async function handlePushFilesToDevice(fingerprint: string, filePaths: string[]) {
  try {
    // 确保会话在（卡片拖放时可能尚未连接）
    await devicesStore.connectDevice(fingerprint)

    const items = await api.browse.expandLocalPaths(filePaths)
    if (items.length === 0) {
      toastStore.push('warning', '没有可推送的文件（文件夹可能为空或全是隐藏项）')
      return
    }
    const jobId = await api.browse.pushFilesRel(fingerprint, items)
    transfersStore.recordPushRelRequest(jobId, fingerprint, items)
    toastStore.push('success', `开始推送 ${items.length} 个文件`)
  } catch (error) {
    toastStore.push('error', formatConnectError(error))
  }
}

/**
 * 设置页面级拖拽事件监听
 */
async function setupPageDragDropListener() {
  try {
    const webview = getCurrentWebview()

    dragDropUnlisten = await webview.onDragDropEvent((event) => {
      const payload = event.payload

      if (payload.type === 'drop') {
        // 找到对应的设备卡片
        const cardElements = document.querySelectorAll('.device-card')
        for (const cardElement of cardElements) {
          const rect = cardElement.getBoundingClientRect()
          const position = payload.position
          const isInCard =
            position.x >= rect.left &&
            position.x <= rect.right &&
            position.y >= rect.top &&
            position.y <= rect.bottom

          if (isInCard) {
            const fp = cardElement.getAttribute('data-fingerprint')
            if (fp) {
              const device = devicesStore.devices.find(d => d.fingerprint === fp)
              if (device && device.online && payload.paths.length > 0) {
                // 检查是否已配对
                const isTrusted = settingsStore.trustedPeers.some(p => p.fingerprint === fp)
                if (isTrusted) {
                  handlePushFilesToDevice(fp, payload.paths)
                }
              }
            }
            break
          }
        }
        dragOverFp.value = null
      } else if (payload.type === 'over') {
        // 找到拖拽悬停的设备
        let foundFp: string | null = null
        const cardElements = document.querySelectorAll('.device-card')
        for (const cardElement of cardElements) {
          const rect = cardElement.getBoundingClientRect()
          const position = payload.position
          const isInCard =
            position.x >= rect.left &&
            position.x <= rect.right &&
            position.y >= rect.top &&
            position.y <= rect.bottom

          if (isInCard) {
            const fp = cardElement.getAttribute('data-fingerprint')
            if (fp) {
              const device = devicesStore.devices.find(d => d.fingerprint === fp)
              if (device && device.online) {
                const isTrusted = settingsStore.trustedPeers.some(p => p.fingerprint === fp)
                if (isTrusted) {
                  foundFp = fp
                  break
                }
              }
            }
          }
        }
        dragOverFp.value = foundFp
      } else if (payload.type === 'leave') {
        dragOverFp.value = null
      }
    })
  } catch (error) {
    console.error('Failed to setup page drag drop listener:', error)
  }
}

/**
 * 初始化页面
 */
onMounted(async () => {
  // 确保设备列表已加载
  if (devicesStore.devices.length === 0) {
    await devicesStore.refreshDevices()
  }

  // 本机 IP 展示（拿不到也不影响使用）
  try {
    const st = await systemApi.getNetworkStatus()
    localIp.value = st.local_ip
  } catch (e) {
    console.error('获取本机 IP 失败:', e)
  }

  // 设置页面级拖拽监听 + 探测结果监听
  await setupPageDragDropListener()
  await setupProbeResultListener()
})

/**
 * 清理
 */
onUnmounted(() => {
  dragDropUnlisten?.()
  probeResultUnlisten?.()
})
</script>

<style scoped>
.devices-page {
  padding: var(--space-5);
  max-width: 1200px;
  margin: 0 auto;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: var(--space-6);
}

.page-header h1 {
  margin: 0;
  font-size: var(--text-xl);
  font-weight: 600;
  color: var(--gray-800);
}

.header-actions {
  display: flex;
  gap: var(--space-3);
  align-items: center;
}

.switch-container {
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.switch {
  position: relative;
  display: inline-block;
  width: 44px;
  height: 24px;
}

.switch input {
  opacity: 0;
  width: 0;
  height: 0;
}

.slider {
  position: absolute;
  cursor: pointer;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background-color: var(--gray-300);
  transition: 0.3s;
  border-radius: var(--radius-full);
}

.slider:before {
  position: absolute;
  content: "";
  height: 18px;
  width: 18px;
  left: 3px;
  bottom: 3px;
  background-color: white;
  transition: 0.3s;
  border-radius: 50%;
}

input:checked + .slider {
  background-color: var(--primary-600);
}

input:checked + .slider:before {
  transform: translateX(20px);
}

.switch-label {
  font-size: var(--text-base);
  color: var(--gray-700);
  font-weight: 500;
}

/* 本机 IP 徽章（点击复制） */
.local-ip-chip {
  padding: var(--space-1) var(--space-3);
  border: 1px solid var(--primary-200);
  border-radius: var(--radius-full);
  background: var(--primary-50);
  color: var(--primary-700);
  font-size: var(--text-sm);
  font-family: var(--font-mono);
  cursor: pointer;
  white-space: nowrap;
  transition: background-color var(--dur-fast) ease;
}

.local-ip-chip:hover {
  background: var(--primary-100);
}

.btn {
  padding: var(--space-2) var(--space-4);
  min-height: var(--control-h-md);
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  border: none;
  transition: background-color var(--dur-fast) ease;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-primary {
  background: var(--primary-600);
  color: white;
}

.btn-primary:hover:not(:disabled) {
  background: var(--primary-700);
}

.btn-secondary {
  background: var(--gray-100);
  color: var(--gray-700);
}

.btn-secondary:hover:not(:disabled) {
  background: var(--gray-200);
}

.devices-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(300px, 1fr));
  gap: var(--space-4);
}

.loading-state,
.error-state,
.empty-state {
  text-align: center;
  padding: 60px var(--space-5);
}

.loading-spinner {
  width: 40px;
  height: 40px;
  border: 4px solid var(--gray-200);
  border-top: 4px solid var(--primary-500);
  border-radius: 50%;
  animation: spin 1s linear infinite;
  margin: 0 auto var(--space-4);
}

@keyframes spin {
  0% { transform: rotate(0deg); }
  100% { transform: rotate(360deg); }
}

.error-state {
  color: var(--danger-600);
}

.error-state p {
  margin-bottom: var(--space-4);
}

.empty-state {
  color: var(--gray-500);
}

.empty-hint {
  font-size: var(--text-base);
  margin-top: var(--space-2);
}

.dialog-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(17, 24, 39, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
  padding: var(--space-5);
}

.dialog {
  background: white;
  border-radius: var(--radius-md);
  max-width: 400px;
  width: 100%;
  box-shadow: var(--shadow-md);
}

.dialog-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: var(--space-5);
  border-bottom: 1px solid var(--gray-200);
}

.dialog-header h2 {
  margin: 0;
  font-size: var(--text-lg);
  font-weight: 600;
  color: var(--gray-800);
}

.close-btn {
  background: none;
  border: none;
  font-size: 32px;
  color: var(--gray-500);
  cursor: pointer;
  width: 40px;
  height: 40px;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 50%;
  transition: background-color var(--dur-fast) ease;
}

.close-btn:hover {
  background: var(--gray-100);
}

.dialog-content {
  padding: var(--space-5);
}

.form-group {
  margin-bottom: var(--space-5);
}

.form-group label {
  display: block;
  margin-bottom: var(--space-2);
  font-size: var(--text-base);
  font-weight: 500;
  color: var(--gray-700);
}

.form-input {
  width: 100%;
  box-sizing: border-box;  /* padding 计入宽度,不再溢出弹窗 */
  padding: 10px var(--space-3);
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  transition: border-color var(--dur-fast) ease, box-shadow var(--dur-fast) ease;
}

.form-input:focus {
  outline: none;
  border-color: var(--primary-500);
  box-shadow: 0 0 0 3px var(--primary-100);
}

/* T4:名片粘贴多行输入 */
textarea.form-input.manual-add-input {
  resize: vertical;
  min-height: 72px;
  font-family: var(--font-mono);
  line-height: 1.5;
}

.form-hint {
  margin-top: var(--space-1);
  font-size: var(--text-xs);
  color: var(--gray-500);
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

.dialog-actions {
  display: flex;
  gap: var(--space-3);
  justify-content: flex-end;
  margin-top: var(--space-6);
}
</style>
