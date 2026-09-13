<template>
  <div id="app">
    <div class="app-content">
      <router-view />
    </div>

    <!-- P0-6 修复:配对对话框全局挂载——配对同意门/输码是任意页面随时可能
         到达的全局事件,此前仅设备页挂载导致离开该页弹窗永不出现 -->
    <PairingDialog />

    <!-- 底部导航栏 -->
    <nav class="bottom-nav">
      <router-link
        v-for="tab in tabs"
        :key="tab.path"
        :to="tab.path"
        class="nav-item"
        :class="{ active: $route.path === tab.path }"
        :data-testid="`nav-${tab.icon}-link`"
      >
        <NavIcons :name="tab.icon" class="nav-icon" />
        <span class="nav-label">{{ tab.label }}</span>
      </router-link>
    </nav>

    <!-- Toast 容器 -->
    <div class="toast-container">
      <div
        v-for="toast in toastStore.toasts"
        :key="toast.id"
        class="toast"
        :class="`toast-${toast.level}`"
      >
        {{ toast.text }}
      </div>
    </div>

    <!-- P0-2c 远程删除确认模态 -->
    <div v-if="deleteAsk" class="offer-modal-overlay" @click="handleDeleteRespond(false)">
      <div class="offer-modal" @click.stop>
        <div class="modal-header">
          <h3>删除确认</h3>
          <button @click="handleDeleteRespond(false)" class="btn-close">×</button>
        </div>
        <div v-if="deleteCountdown.remainingMs.value > 0" class="offer-countdown" :class="{ urgent: deleteCountdown.urgent.value }" data-testid="delete-countdown">
          {{ Math.floor(deleteCountdown.remainingMs.value / 60000) }}:{{ String(Math.floor((deleteCountdown.remainingMs.value % 60000) / 1000)).padStart(2, '0') }} 后自动拒绝
        </div>
        <div class="modal-body">
          <div class="offer-info">
            <div class="info-item">
              <span class="info-label">请求设备:</span>
              <span class="info-value">{{ deleteAsk.peer_name }}</span>
            </div>
            <div class="info-item">
              <span class="info-label">将删除:</span>
              <span class="info-value">{{ deleteAsk.name }}{{ deleteAsk.is_dir ? `(文件夹,共 ${deleteAsk.entry_count} 项)` : '' }}</span>
            </div>
          </div>
        </div>
        <div class="modal-footer">
          <button @click="handleDeleteRespond(false)" class="btn btn-secondary">拒绝</button>
          <button @click="handleDeleteRespond(true)" class="btn btn-danger">允许删除</button>
        </div>
      </div>
    </div>

    <!-- 推送确认弹窗 -->
    <div v-if="offerRequest" class="offer-modal-overlay" data-testid="offer-modal" @click="handleRejectOffer">
      <div class="offer-modal" @click.stop>
        <div class="modal-header">
          <h3>文件推送请求</h3>
          <button @click="handleRejectOffer" class="btn-close">×</button>
        </div>

        <div v-if="remainingMs > 0" class="offer-countdown" :class="{ urgent }" data-testid="offer-countdown">
          {{ Math.floor(remainingMs / 60000) }}:{{ String(Math.floor((remainingMs % 60000) / 1000)).padStart(2, '0') }} 后自动拒绝
        </div>

        <div class="modal-body">
          <div class="offer-info">
            <div class="info-item">
              <span class="info-label">来源设备:</span>
              <span class="info-value">{{ peerName }}</span>
            </div>

            <div class="info-item">
              <span class="info-label">文件数量:</span>
              <span class="info-value">{{ offerRequest.files.length }} 个文件</span>
            </div>

            <div class="info-item">
              <span class="info-label">总大小:</span>
              <span class="info-value">{{ formatSize(offerRequest.files.reduce((s, f) => s + (f.size || 0), 0)) }}</span>
            </div>
          </div>

          <div class="files-list">
            <div
              v-for="(file, index) in (filesExpanded ? offerRequest.files : offerRequest.files.slice(0, 8))"
              :key="index"
              class="file-item"
            >
              <div class="file-icon">📄</div>
              <div class="file-info">
                <div class="file-name">{{ file.name }}</div>
                <div class="file-meta">
                  {{ formatSize(file.size) }}
                  <span v-if="file.rel_dir" class="file-dir">{{ file.rel_dir }}</span>
                </div>
              </div>
            </div>
            <button v-if="offerRequest.files.length > 8" class="link-btn" data-testid="btn-expand-files"
              @click="filesExpanded = !filesExpanded">
              {{ filesExpanded ? '收起' : `共 ${offerRequest.files.length} 个文件（展开查看）` }}
            </button>
          </div>
        </div>

        <div class="modal-footer">
          <button @click="handleRejectOffer" class="btn btn-secondary">
            拒绝
          </button>
          <button @click="handleSaveAs" class="btn btn-primary">
            另存到…
          </button>
          <button @click="handleAcceptOffer" class="btn btn-success" data-testid="offer-accept-btn">
            接收
          </button>
        </div>
      </div>
    </div>

    <!-- 关闭拦截模态 -->
    <Teleport to="body">
      <div v-if="closeGuard.guardOpen.value" class="modal-backdrop" @click.self="closeGuard.cancelClose()">
        <div class="modal-card">
          <h3>有 {{ closeGuard.activeCount.value }} 个传输正在进行</h3>
          <p>关闭会中断传输,但进度已自动保存,对方设备也会收到通知;下次启动可断点续传。</p>
          <div class="modal-actions">
            <button @click="closeGuard.cancelClose()" class="btn-secondary">取消关闭,继续传输</button>
            <button @click="closeGuard.doForceClose()" class="btn-danger">仍要关闭</button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- v0.2.7 启动断点恢复提示 -->
    <Teleport to="body">
      <div v-if="resumePromptOpen" class="modal-backdrop">
        <div class="modal-card">
          <h3>检测到未完成的传输</h3>
          <p>上次有 {{ interruptedCount }} 个传输被中断,断点数据已保存。是否现在恢复?</p>
          <p class="modal-hint">需要对方设备在线才能续传;也可以稍后在传输页手动恢复。</p>
          <div class="modal-actions">
            <button @click="handleIgnoreInterrupted" class="btn-secondary" data-testid="resume-ignore-btn" :disabled="resuming">暂不恢复</button>
            <button @click="handleResumeAllInterrupted" class="btn-primary" :disabled="resuming">
              {{ resuming ? '恢复中...' : '全部恢复' }}
            </button>
          </div>
        </div>
      </div>
    </Teleport>

    <!-- 全局确认对话框宿主(替代 WebView2 不支持的 window.confirm) -->
    <Teleport to="body">
      <ConfirmDialog
        v-if="confirmState.open"
        :title="confirmState.title"
        :message="confirmState.message"
        :hint="confirmState.hint"
        :ok-text="confirmState.okText"
        @resolve="resolveDialog"
      />
    </Teleport>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch } from 'vue'
import { useDevicesStore } from './stores/devices'
import { useTransfersStore } from './stores/transfers'
import { useSettingsStore } from './stores/settings'
import { useToastStore } from './stores/toast'
import { onOfferRequest, onDeleteRequest, onPairingResult, api, respondDelete, friendlyError } from './api'
import PairingDialog from './components/PairingDialog.vue'
import type { OfferRequestEvent } from './types'
import { useCloseGuard } from './composables/useCloseGuard'
import { useOfferCountdown } from './composables/useOfferCountdown'
import { useConfirm } from './composables/useConfirm'
import NavIcons from './components/NavIcons.vue'
import ConfirmDialog from './components/ConfirmDialog.vue'

const devicesStore = useDevicesStore()
const transfersStore = useTransfersStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()
const closeGuard = useCloseGuard()

// 全局确认对话框(WebView2 无原生 confirm)
const { dialogState: confirmState, resolveDialog } = useConfirm()

// 推送请求状态
const offerRequest = ref<OfferRequestEvent | null>(null)

// v0.5.0 倒计时功能
const { remainingMs, expired, urgent, reset } = useOfferCountdown(computed(() => offerRequest.value?.deadline_epoch_ms ?? null))

// 文件列表展开状态
const filesExpanded = ref(false)

// v0.2.7 启动断点恢复提示：上次的 interrupted 任务（磁盘有 parts）
const resumePromptOpen = ref(false)
const resuming = ref(false)

// P0-2c 远程删除确认状态
interface DeleteAskEvent {
  ask_id: number; fingerprint: string; peer_name: string; share_id: string;
  name: string; is_dir: boolean; entry_count: number; deadline_epoch_ms: number;
}
const deleteAsk = ref<DeleteAskEvent | null>(null)
const deleteCountdown = useOfferCountdown(computed(() => deleteAsk.value?.deadline_epoch_ms ?? null))
let deleteUnlisten: (() => void) | null = null

/** 启动检测：interrupted 且磁盘有断点数据的任务 → 弹恢复提示 */
async function checkInterruptedOnStartup() {
  const interrupted = transfersStore.transfers.filter(t => t.state === 'interrupted')
  if (interrupted.length === 0) return
  // 有 parts 目录的任务才可断点恢复（孤儿已在后端 migrate 时清掉）
  resumePromptOpen.value = true
}

/** 全部恢复：逐个续传（后端串行排队） */
async function handleResumeAllInterrupted() {
  resuming.value = true
  let ok = 0
  let fail = 0
  for (const t of transfersStore.transfers.filter(x => x.state === 'interrupted')) {
    try {
      await transfersStore.resumePending(t.job_id)
      ok++
    } catch {
      fail++
    }
  }
  resuming.value = false
  resumePromptOpen.value = false
  if (ok > 0) toastStore.push('success', `已恢复 ${ok} 个传输任务`)
  if (fail > 0) toastStore.push('error', `${fail} 个任务恢复失败（对端可能不在线）`)
}

/** 忽略：不弹了，任务留在传输页（卡片上有续传按钮可随时手动恢复） */
function handleIgnoreInterrupted() {
  resumePromptOpen.value = false
}

/** 当前 interrupted 任务数（弹窗文案用） */
const interruptedCount = computed(() =>
  transfersStore.transfers.filter(t => t.state === 'interrupted').length
)

// 底部导航栏配置
const tabs = [
  { path: '/devices', label: '设备', icon: 'devices' as const },
  { path: '/transfers', label: '传输', icon: 'transfers' as const },
  { path: '/browse', label: '浏览', icon: 'browse' as const },
  { path: '/settings', label: '设置', icon: 'settings' as const },
]

/**
 * 对端设备名称（简化显示）
 */
const peerName = computed(() => {
  if (!offerRequest.value) return ''
  const fp = offerRequest.value.peer
  return fp.length > 12 ? `${fp.slice(0, 6)}...${fp.slice(-6)}` : fp
})

/**
 * 格式化文件大小
 */
function formatSize(bytes: number): string {
  if (bytes === 0) return '0 B'

  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unitIndex = 0

  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex++
  }

  return `${value.toFixed(1)} ${units[unitIndex]}`
}

// v0.5.0 监听超时自动关闭弹窗
watch(expired, v => {
  if (v && offerRequest.value) {
    offerRequest.value = null
    toastStore.push('info', '已超时自动拒绝')
  }
})

/**
 * 接收推送请求（使用默认下载目录）
 */
async function handleAcceptOffer() {
  if (!offerRequest.value) return

  try {
    await api.browse.respondOffer(offerRequest.value.job_id, true)
    toastStore.push('success', '已接受文件推送')
    offerRequest.value = null
  } catch (e) {
    // 应答失败(条目已被超时看门狗清掉/会话已断)时弹窗已无意义——
    // 强制关闭,否则弹窗永驻且按钮永远报错(用户卡死无出路)
    toastStore.push('error', '接受推送失败: ' + friendlyError(e))
    offerRequest.value = null
  }
}

/**
 * 拒绝推送请求
 */
async function handleRejectOffer() {
  if (!offerRequest.value) return

  try {
    await api.browse.respondOffer(offerRequest.value.job_id, false)
    toastStore.push('info', '已拒绝文件推送')
    offerRequest.value = null
  } catch (e) {
    // 同上:失败也关弹窗,给用户出路
    toastStore.push('error', '拒绝推送失败: ' + friendlyError(e))
    offerRequest.value = null
  }
}

/**
 * 另存到指定目录
 */
async function handleSaveAs() {
  if (!offerRequest.value) return

  try {
    // v0.5.0 打开对话框前顺延超时
    try {
      await api.browse.offerExtend(offerRequest.value.job_id)
      const timeoutSecs = settingsStore.config?.offer_timeout_secs ?? 60
      reset(Date.now() + timeoutSecs * 1000)
    } catch {
      // 已超时则由后续 respond 报错
    }

    // 使用 dialog 插件选择目录
    const { open } = await import('@tauri-apps/plugin-dialog')

    const selected = await open({
      directory: true,
      multiple: false,
      title: '选择保存目录'
    })

    if (selected && typeof selected === 'string') {
      await api.browse.respondOffer(offerRequest.value.job_id, true, selected)
      toastStore.push('success', '已接受文件推送到指定目录')
      offerRequest.value = null
    }
  } catch (e) {
    if (e instanceof Error && e.message !== 'User cancelled') {
      toastStore.push('error', '选择目录失败: ' + friendlyError(e))
    }
  }
}

/**
 * P0-2c 应答远程删除确认
 */
function handleDeleteRespond(allow: boolean) {
  if (!deleteAsk.value) return
  respondDelete(deleteAsk.value.ask_id, allow)
  deleteAsk.value = null
}

/**
 * 初始化应用
 */
onMounted(async () => {
  try {
    // 初始化 toast store（会自动监听 Rust 事件）
    const toastUnlisten = await toastStore.initialize()

    // 初始化其他 stores
    await Promise.all([
      devicesStore.initialize(),
      transfersStore.initialize(),
      settingsStore.initialize(),
    ])

    // 监听推送请求事件
    const offerUnlisten = await onOfferRequest((request) => {
      // 类型断言确保files数组类型正确
      // deadline 无效(缺失/0,事件负载损坏)时按 now+60s 兜底——否则
      // expired 永不翻转,弹窗永驻且应答通道已死,用户无出路
      const rawDeadline = (request as any).deadline_epoch_ms as number | undefined
      const deadline = (typeof rawDeadline === 'number' && rawDeadline > Date.now())
        ? rawDeadline
        : Date.now() + 60_000
      offerRequest.value = {
        job_id: request.job_id,
        peer: request.peer,
        files: request.files as any,
        deadline_epoch_ms: deadline
      }
    })

    // P0-2c 监听远程删除确认事件
    deleteUnlisten = await onDeleteRequest((req) => { deleteAsk.value = req })

    // P3-T4 监听配对结果事件：配对成功即回读信任列表——设置页停留期间
    // 完成的配对也能即时出现在信任设备卡片中（原实现只在页面挂载时加载）
    const pairingUnlisten = await onPairingResult((result) => {
      if (result?.ok) {
        settingsStore.refreshTrusted()
      }
    })

    // 初始化关闭拦截
    closeGuard.init()

    // v0.2.7 启动断点检测（在 transfers 初始化完成后）
    await checkInterruptedOnStartup()

    // 组件卸载时取消监听
    onUnmounted(() => {
      toastUnlisten?.()
      offerUnlisten?.()
      deleteUnlisten?.()
      pairingUnlisten?.()
      closeGuard.cleanup()
    })
  } catch (error) {
    toastStore.push('error', '应用初始化失败: ' + friendlyError(error))
  }
})
</script>

<style scoped>
#app {
  display: flex;
  flex-direction: column;
  height: 100vh;
  overflow: hidden;
}

.app-content {
  flex: 1;
  overflow-y: auto;
  padding-bottom: 68px; /* 为底部导航栏留空间 */
}

.bottom-nav {
  position: fixed;
  bottom: 0;
  left: 0;
  right: 0;
  display: flex;
  background: white;
  border-top: 1px solid var(--gray-200);
  padding: var(--space-2) 0;
  z-index: 100;
}

.nav-item {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: var(--space-1);
  text-decoration: none;
  color: var(--gray-400);
  transition: color var(--dur-fast) ease;
  cursor: pointer;
  min-height: 52px;
}

.nav-item:hover {
  color: var(--gray-600);
}

.nav-item.active {
  color: var(--primary-600);
}

.nav-label {
  font-size: var(--text-xs);
  line-height: 1;
}

.toast-container {
  position: fixed;
  bottom: 72px; /* 底部导航栏上方——用户反馈:toast 覆盖刚点击的按钮 */
  left: 50%;
  transform: translateX(-50%);
  z-index: 1000;
  display: flex;
  flex-direction: column-reverse; /* 新 toast 在下方,不遮住旧 toast 的关闭区域 */
  gap: var(--space-2);
  pointer-events: none; /* toast 不拦截鼠标——下方按钮仍可点 */
}
.toast-container .toast {
  pointer-events: auto;
}

.toast {
  padding: var(--space-3) var(--space-4);
  border-radius: var(--radius-sm);
  background: white;
  box-shadow: var(--shadow-md);
  min-width: 200px;
  animation: slideIn 0.3s ease-out;
}

.toast-info {
  border-left: 4px solid var(--primary-500);
}

.toast-success {
  border-left: 4px solid var(--success-500);
}

.toast-warning {
  border-left: 4px solid var(--warning-500);
}

.toast-error {
  border-left: 4px solid var(--danger-500);
}

@keyframes slideIn {
  from {
    transform: translateX(100%);
    opacity: 0;
  }
  to {
    transform: translateX(0);
    opacity: 1;
  }
}

/* 推送确认弹窗样式 */
.offer-modal-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(17, 24, 39, 0.45);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 2000;
  animation: fadeIn 0.2s ease-out;
}

.offer-modal {
  background: white;
  border-radius: var(--radius-lg);
  width: 90%;
  max-width: 600px;
  max-height: 80vh;
  display: flex;
  flex-direction: column;
  box-shadow: var(--shadow-md);
  animation: slideUp 0.3s var(--ease-out);
}

.modal-header {
  padding: var(--space-5);
  border-bottom: 1px solid var(--gray-200);
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.modal-header h3 {
  font-size: var(--text-lg);
  font-weight: 600;
  color: var(--gray-800);
  margin: 0;
}

.btn-close {
  width: var(--control-h-md);
  height: var(--control-h-md);
  border: none;
  background: transparent;
  font-size: 24px;
  color: var(--gray-500);
  cursor: pointer;
  border-radius: var(--radius-sm);
  transition: background-color var(--dur-fast) ease, color var(--dur-fast) ease;
  line-height: 1;
}

.btn-close:hover {
  background: var(--gray-100);
  color: var(--gray-800);
}

.offer-countdown {
  padding: var(--space-3) var(--space-5);
  background: var(--primary-50);
  border-left: 4px solid var(--primary-500);
  color: var(--primary-700);
  font-weight: 600;
  text-align: center;
  font-variant-numeric: tabular-nums;
}

.offer-countdown.urgent {
  background: var(--danger-50);
  border-left-color: var(--danger-500);
  color: var(--danger-700);
}

.modal-body {
  padding: var(--space-5);
  overflow-y: auto;
}

.offer-info {
  margin-bottom: var(--space-5);
}

.info-item {
  display: flex;
  justify-content: space-between;
  padding: var(--space-2) 0;
  border-bottom: 1px solid var(--gray-100);
}

.info-label {
  font-weight: 500;
  color: var(--gray-500);
}

.info-value {
  color: var(--gray-800);
  font-weight: 600;
}

.files-list {
  display: flex;
  flex-direction: column;
  gap: var(--space-2);
}

.file-item {
  display: flex;
  align-items: center;
  padding: var(--space-3);
  background: var(--gray-50);
  border-radius: var(--radius-sm);
  border: 1px solid var(--gray-200);
}

.file-icon {
  font-size: 20px;
  margin-right: var(--space-3);
}

.file-info {
  flex: 1;
  min-width: 0;
}

.file-name {
  font-weight: 500;
  color: var(--gray-800);
  font-size: var(--text-base);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.file-meta {
  font-size: var(--text-xs);
  color: var(--gray-500);
  margin-top: 2px;
  font-variant-numeric: tabular-nums;
}

.file-dir {
  color: var(--gray-400);
  margin-left: var(--space-2);
}

.modal-footer {
  padding: var(--space-4) var(--space-5);
  border-top: 1px solid var(--gray-200);
  display: flex;
  justify-content: flex-end;
  gap: var(--space-3);
  background: var(--gray-50);
  border-radius: 0 0 var(--radius-lg) var(--radius-lg);
}

.btn {
  padding: 10px 20px;
  border: none;
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  transition: background-color var(--dur-fast) ease;
  text-align: center;
}

.btn-secondary {
  background: var(--gray-100);
  color: var(--gray-700);
}

.btn-secondary:hover {
  background: var(--gray-200);
}

.btn-primary {
  background: var(--primary-600);
  color: white;
}

.btn-primary:hover {
  background: var(--primary-700);
}

.btn-success {
  background: var(--success-500);
  color: white;
}

.btn-success:hover {
  background: var(--success-600);
}

.btn-danger:hover {
  background: var(--danger-600);
}
</style>
