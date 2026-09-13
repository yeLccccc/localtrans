<template>
  <div class="browse-page">
    <div class="page-header">
      <h1>文件浏览</h1>
    </div>

    <!-- 设备选择栏：本页自足，不再依赖从设备页带参跳入 -->
    <div class="device-bar">
      <span class="device-bar-label">浏览设备</span>
      <div v-if="pairedOnlineDevices.length > 0" class="device-chips">
        <button
          v-for="device in pairedOnlineDevices"
          :key="device.fingerprint"
          class="device-chip"
          :class="{ active: device.fingerprint === selectedFingerprint }"
          :disabled="switchingDevice === device.fingerprint"
          :data-testid="`browse-device-chip-${device.fingerprint}`"
          @click="handleSelectDevice(device.fingerprint)"
        >
          {{ device.name || device.fingerprint.slice(0, 8) }}
          <span v-if="switchingDevice === device.fingerprint" class="chip-spinner"></span>
        </button>
      </div>
      <div v-else class="device-bar-empty">
        暂无已配对的在线设备
        <router-link to="/devices" class="device-bar-link" data-testid="browse-goto-devices-link">去连接</router-link>
      </div>
    </div>

    <!-- 未选中设备提示 -->
    <div v-if="!selectedFingerprint" class="no-device-state">
      <div class="no-device-content">
        <div class="no-device-icon">📱</div>
        <h3>选择一台设备</h3>
        <p>点击上方设备标签，即可浏览对方共享的文件</p>
      </div>
    </div>

    <!-- 浏览界面 -->
    <div v-else class="browse-container">
      <!-- 左栏：共享区列表 -->
      <div class="shares-panel">
        <div class="panel-header">
          <h3>共享区</h3>
        </div>

        <div v-if="loadingShares" class="loading-state">
          <div class="spinner-small"></div>
        </div>

        <div v-else-if="sharesError" class="error-state">
          <p class="error-text">{{ sharesError }}</p>
          <button @click="handleRefreshShares" class="btn-small">重试</button>
        </div>

        <div v-else-if="shares.length === 0" class="empty-state">
          <p>无共享区</p>
        </div>

        <div v-else class="shares-list">
          <div
            v-for="share in shares"
            :key="share.id"
            class="share-item"
            :class="{ active: currentShare?.id === share.id }"
            :data-testid="`browse-share-item-${share.id}`"
            @click="handleSelectShare(share)"
          >
            <div class="share-icon">📁</div>
            <div class="share-info">
              <div class="share-name">{{ share.alias }}</div>
              <div class="share-id">{{ share.id.slice(0, 8) }}...</div>
            </div>
          </div>
        </div>
      </div>

      <!-- 右栏：文件浏览 -->
      <div class="files-panel">
        <div v-if="!currentShare" class="select-share-hint">
          <div class="hint-content">
            <div class="hint-icon">👈</div>
            <h3>选择共享区</h3>
            <p>从左侧列表选择一个共享区开始浏览</p>
          </div>
        </div>

        <div v-else class="file-browser">
          <!-- 工具栏 -->
          <div class="browser-toolbar">
            <div class="breadcrumb">
              <!-- v0.2.8 返回上一级 -->
              <button
                v-if="currentPath"
                class="btn-up"
                title="返回上一级"
                data-testid="browse-go-up-btn"
                @click="handleGoUp"
              >
                ↑
              </button>
              <!-- v0.2.8 根段可点：回共享区根目录 -->
              <span class="breadcrumb-item clickable" @click="handleNavigateTo(-1)">
                {{ currentShare.alias }}
              </span>
              <template v-if="currentPath">
                <span
                  v-for="(_, index) in pathSegments"
                  :key="'sep-' + index"
                  class="breadcrumb-separator"
                >/</span>
                <!-- v0.2.8 每段可点：跳转到该层级（最后一段是当前位置，不可点） -->
                <span
                  v-for="(segment, index) in pathSegments"
                  :key="'seg-' + index"
                  class="breadcrumb-item"
                  :class="{ clickable: index < pathSegments.length - 1 }"
                  @click="handleNavigateTo(index)"
                >
                  {{ segment }}
                </span>
              </template>
            </div>

            <div class="toolbar-actions">
              <!-- 2026-08-30 浏览页定案:删刷新按钮(事件驱动自动刷新),
                   加本地排序(名称/大小/时间 + 升降,目录优先) -->
              <div class="sort-control" data-testid="browse-sort-control">
                <button
                  v-for="k in (['name', 'size', 'time'] as const)"
                  :key="k"
                  class="sort-btn"
                  :class="{ active: sortKey === k }"
                  :data-testid="`browse-sort-${k}`"
                  @click="toggleSort(k)"
                >
                  {{ k === 'name' ? '名称' : k === 'size' ? '大小' : '时间' }}
                </button>
                <button
                  class="sort-btn dir"
                  data-testid="browse-sort-dir"
                  @click="sortAsc = !sortAsc"
                  :title="sortAsc ? '升序（点击切换降序）' : '降序（点击切换升序）'"
                >
                  {{ sortAsc ? '↑' : '↓' }}
                </button>
              </div>
            </div>
          </div>

          <!-- 文件列表 -->
          <div v-if="loadingFiles" class="loading-state">
            <div class="spinner-small"></div>
          </div>

          <div v-else-if="filesError" class="error-state">
            <p class="error-text">{{ filesError }}</p>
            <button @click="handleRefreshFiles" class="btn-small">重试</button>
          </div>

          <div v-else-if="files.length === 0" class="empty-state">
            <p>文件夹为空</p>
          </div>

          <div v-else class="files-list">
            <div
              v-for="file in sortedFiles"
              :key="file.name"
              class="file-item"
              :class="{ selected: selectedFiles.has(file.name) }"
            >
              <div class="file-checkbox">
                <input
                  type="checkbox"
                  :id="`file-${file.name}`"
                  :value="file.name"
                  v-model="selectedFilesSet"
                  :data-testid="`browse-file-check-${file.name}`"
                  @change="handleFileSelect(file)"
                />
              </div>

              <div
                v-if="file.is_dir"
                class="file-icon directory"
                @click="handleEnterDirectory(file)"
              >
                📁
              </div>
              <div v-else class="file-icon file">📄</div>

              <div
                v-if="file.is_dir"
                class="file-info directory"
                @click="handleEnterDirectory(file)"
              >
                <div class="file-name">{{ file.name }}</div>
                <div class="file-meta">文件夹</div>
              </div>
              <div v-else class="file-info">
                <div class="file-name">{{ file.name }}</div>
                <div class="file-meta">{{ formatSize(file.size) }}</div>
              </div>

              <!-- v0.2.6 文件夹快捷下载（不进目录直接拉整棵） -->
              <button
                v-if="file.is_dir"
                class="dir-download-btn"
                title="下载整个文件夹"
                :data-testid="`browse-dir-download-btn-${file.name}`"
                @click.stop="handleDownloadDir(file)"
              >
                下载
              </button>
            </div>

            <!-- 加载更多 -->
            <div
              v-if="hasMore"
              class="load-more-item"
              @click="handleLoadMore"
            >
              <div class="load-more-text">加载更多...</div>
            </div>
          </div>

          <!-- 底部操作栏 -->
          <div v-if="selectedFiles.size > 0" class="files-actions">
            <div class="selection-info">
              已选择 {{ selectedFiles.size }} 个文件
            </div>
            <button
              @click="handleDownloadSelected"
              class="btn btn-primary"
              data-testid="browse-download-selected-btn"
              :disabled="downloading"
            >
              {{ downloading ? '下载中...' : '下载选中' }}
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { storeToRefs } from 'pinia'
import type { ShareInfo, FileEntry } from '../types'
import { useDevicesStore } from '../stores/devices'
import { useTransfersStore } from '../stores/transfers'
import { useToastStore } from '../stores/toast'
import { useSettingsStore } from '../stores/settings'
import { api, friendlyError, formatConnectError } from '../api'

const devicesStore = useDevicesStore()
const transfersStore = useTransfersStore()
const toastStore = useToastStore()
const settingsStore = useSettingsStore()

const { selected_fp: selectedFingerprint } = storeToRefs(devicesStore)

// 设备切换中的指纹（按钮局部 spinner）
const switchingDevice = ref<string | null>(null)

/**
 * 已配对且在线的设备（浏览的候选）——配对信息在 settingsStore.trustedPeers
 */
const pairedOnlineDevices = computed(() => {
  const trusted = new Set(settingsStore.trustedPeers.map(p => p.fingerprint))
  return devicesStore.devices.filter(d => d.online && trusted.has(d.fingerprint))
})

/**
 * 选择/切换浏览设备：确保会话已建立，然后刷新共享区
 */
async function handleSelectDevice(fingerprint: string) {
  if (fingerprint === selectedFingerprint.value) return
  switchingDevice.value = fingerprint
  try {
    await devicesStore.connectDevice(fingerprint)
    devicesStore.selected_fp = fingerprint
    await handleRefreshShares()
  } catch (e) {
    toastStore.push('error', formatConnectError(e))
  } finally {
    switchingDevice.value = null
  }
}

// 共享区列表状态
const shares = ref<ShareInfo[]>([])
const currentShare = ref<ShareInfo | null>(null)
const loadingShares = ref(false)
const sharesError = ref<string | null>(null)

// 文件浏览状态
const currentPath = ref('')
const files = ref<FileEntry[]>([])

// 2026-08-30 浏览页定案:本地排序(名称/大小/时间可切+升降,目录恒优先)
type SortKey = 'name' | 'size' | 'time'
const sortKey = ref<SortKey>('name')
const sortAsc = ref(true)
function toggleSort(k: SortKey) {
  if (sortKey.value === k) {
    sortAsc.value = !sortAsc.value
  } else {
    sortKey.value = k
    sortAsc.value = true
  }
}
const sortedFiles = computed<ReadonlyArray<FileEntry>>(() => {
  const dir = sortAsc.value ? 1 : -1
  const collator = new Intl.Collator(undefined, { numeric: true, sensitivity: 'base' })
  return [...files.value].sort((a, b) => {
    if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1 // 目录优先,不受升降影响
    switch (sortKey.value) {
      case 'size':
        return (a.size - b.size) * dir
      case 'time':
        return (a.mtime - b.mtime) * dir
      default:
        return collator.compare(a.name, b.name) * dir
    }
  })
})
const loadingFiles = ref(false)
const filesError = ref<string | null>(null)
const nextCursor = ref<number | null>(null)
const hasMore = ref(false)

// 文件选择状态
const selectedFilesSet = ref<Set<string>>(new Set())
const selectedFiles = ref<Map<string, FileEntry>>(new Map())
const downloading = ref(false)

/**
 * 路径分段（用于面包屑）
 */
const pathSegments = computed(() => {
  if (!currentPath.value) return []
  return currentPath.value.split('/').filter(segment => segment.length > 0)
})

/**
 * 刷新共享区列表
 */
async function handleRefreshShares() {
  if (!selectedFingerprint.value) return

  loadingShares.value = true
  sharesError.value = null

  try {
    shares.value = await api.browse.listSharesRemote(selectedFingerprint.value)
    // 清空当前选择
    currentShare.value = null
    currentPath.value = ''
    files.value = []
    selectedFilesSet.value.clear()
    selectedFiles.value.clear()
  } catch (e) {
    sharesError.value = e instanceof Error ? e.message : String(e)
    if (sharesError.value?.includes('响应通道已被占用')) {
      toastStore.push('warning', '传输进行中，请稍后再试')
    }
  } finally {
    loadingShares.value = false
  }
}

/**
 * 选择共享区
 */
function handleSelectShare(share: ShareInfo) {
  currentShare.value = share
  currentPath.value = ''
  files.value = []
  selectedFilesSet.value.clear()
  selectedFiles.value.clear()
  loadFiles()
}

/**
 * 加载文件列表
 */
async function loadFiles(loadMore = false) {
  if (!selectedFingerprint.value || !currentShare.value) return

  loadingFiles.value = true
  filesError.value = null

  const cursor = loadMore ? (nextCursor.value || 0) : 0

  try {
    const response = await api.browse.listDirRemote(
      selectedFingerprint.value,
      currentShare.value.id,
      currentPath.value,
      cursor
    )

    if (loadMore) {
      files.value = [...files.value, ...response.entries]
    } else {
      files.value = response.entries
    }

    nextCursor.value = response.next_cursor
    hasMore.value = response.next_cursor !== null
  } catch (e) {
    filesError.value = friendlyError(e)
    if (filesError.value?.includes('响应通道已被占用')) {
      toastStore.push('warning', '传输进行中，请稍后再试')
    }
  } finally {
    loadingFiles.value = false
  }
}

/**
 * 刷新文件列表
 */
async function handleRefreshFiles() {
  nextCursor.value = null
  await loadFiles()
}

/**
 * 进入目录
 */
/**
 * v0.2.8 面包屑导航：跳到指定层级。
 * index=-1 → 共享区根；0..n-1 → 截断到该段（含）
 */
function handleNavigateTo(index: number) {
  if (index < 0) {
    currentPath.value = ''
  } else {
    currentPath.value = pathSegments.value.slice(0, index + 1).join('/')
  }
  selectedFilesSet.value.clear()
  selectedFiles.value.clear()
  loadFiles(false)
}

/**
 * v0.2.8 返回上一级
 */
function handleGoUp() {
  const segs = pathSegments.value
  currentPath.value = segs.slice(0, segs.length - 1).join('/')
  selectedFilesSet.value.clear()
  selectedFiles.value.clear()
  loadFiles(false)
}

function handleEnterDirectory(file: FileEntry) {
  if (!file.is_dir) return

  // 更新路径
  const newPath = currentPath.value
    ? `${currentPath.value}/${file.name}`
    : file.name

  currentPath.value = newPath
  selectedFilesSet.value.clear()
  selectedFiles.value.clear()
  loadFiles()
}

/**
 * 文件选择处理
 */
function handleFileSelect(file: FileEntry) {
  if (selectedFilesSet.value.has(file.name)) {
    selectedFiles.value.set(file.name, file)
  } else {
    selectedFiles.value.delete(file.name)
  }
}

/**
 * 加载更多
 */
async function handleLoadMore() {
  await loadFiles(true)
}

/**
 * v0.2.6 下载整个文件夹（递归 + 结构保持）
 */
async function handleDownloadDir(file: FileEntry) {
  if (!selectedFingerprint.value || !currentShare.value) return
  const fullPath = currentPath.value
    ? `${currentPath.value}/${file.name}`
    : file.name
  try {
    const job_id = await api.browse.startDownloadDir(
      selectedFingerprint.value,
      currentShare.value.id,
      fullPath
    )
    transfersStore.recordPullRequest(
      job_id,
      selectedFingerprint.value,
      currentShare.value.id,
      fullPath
    )
    toastStore.push('success', `开始下载文件夹 ${file.name}`)
  } catch (e) {
    toastStore.push('error', `下载文件夹失败: ${e instanceof Error ? e.message : String(e)}`)
  }
}

/**
 * 下载选中文件
 */
async function handleDownloadSelected() {
  if (!selectedFingerprint.value || !currentShare.value || selectedFiles.value.size === 0) {
    return
  }

  downloading.value = true

  // 2026-08-30 浏览页定案:多选文件发 1 批次 → 传输页 1 张父卡片
  // (文件夹仍走整棵直拉,各自成卡——批次化文件夹留待后续)
  const filePaths: string[] = []
  const dirEntries: FileEntry[] = []
  for (const [, file] of selectedFiles.value) {
    const fullPath = currentPath.value
      ? `${currentPath.value}/${file.name}`
      : file.name
    if (file.is_dir) {
      dirEntries.push(file)
    } else {
      filePaths.push(fullPath)
    }
  }

  let failCount = 0

  if (filePaths.length > 0) {
    try {
      const job_id = await api.browse.startDownloadBatch(
        selectedFingerprint.value,
        currentShare.value.id,
        filePaths
      )
      // 记录请求参数用于重试(父卡整批重传)
      for (const p of filePaths) {
        transfersStore.recordPullRequest(
          job_id,
          selectedFingerprint.value,
          currentShare.value.id,
          p
        )
      }
    } catch (e) {
      failCount += filePaths.length
      toastStore.push('error', `批量下载失败: ${e instanceof Error ? e.message : String(e)}`)
    }
  }

  for (const file of dirEntries) {
    const fullPath = currentPath.value
      ? `${currentPath.value}/${file.name}`
      : file.name
    try {
      const job_id = await api.browse.startDownloadDir(
        selectedFingerprint.value,
        currentShare.value.id,
        fullPath
      )
      transfersStore.recordPullRequest(
        job_id,
        selectedFingerprint.value,
        currentShare.value.id,
        fullPath
      )
    } catch (e) {
      failCount++
      console.error(`Failed to download ${file.name}:`, e)
      toastStore.push('error', `下载 ${file.name} 失败: ${e instanceof Error ? e.message : String(e)}`)
    }
  }

  if (failCount === 0) {
    const total = filePaths.length + dirEntries.length
    toastStore.push('success', `已开始下载 ${total} 项`)
    selectedFilesSet.value.clear()
    selectedFiles.value.clear()
  } else {
    toastStore.push('error', `${failCount} 项下载发起失败`)
  }

  downloading.value = false
}

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

/**
 * 初始化
 */
let remoteSharesUnlisten: (() => void) | null = null
let connStateUnlisten: (() => void) | null = null
// 远端变化刷新的防抖定时器（连续变化只刷一次）
let refreshDebounceTimer: number | null = null

onMounted(async () => {
  // settingsStore 可能尚未加载信任列表（直接进入本页时）
  if (settingsStore.trustedPeers.length === 0) {
    await settingsStore.refreshSettings()
  }
  if (selectedFingerprint.value) {
    await handleRefreshShares()
  }

  // 对端共享区变化 → 自动刷新（watchdog 推送，500ms 防抖合并连续变化）
  const { onRemoteSharesChanged, onConnectionState } = await import('../api')
  remoteSharesUnlisten = await onRemoteSharesChanged(({ fingerprint, share_id }) => {
    if (fingerprint !== selectedFingerprint.value) return
    if (currentShare.value && share_id !== currentShare.value.id) {
      // 变的不是当前浏览的共享区：刷左栏列表即可
      handleRefreshShares()
      return
    }
    if (refreshDebounceTimer !== null) {
      window.clearTimeout(refreshDebounceTimer)
    }
    refreshDebounceTimer = window.setTimeout(() => {
      refreshDebounceTimer = null
      // 静默刷新当前目录（不闪 loading 态）；只在空闲时刷，避免与用户操作打架
      if (!loadingFiles.value && !downloading.value) {
        loadFiles()
      }
    }, 500)
  })

  // 连接断开时提示（对端退出/网络中断，浏览会话失效）
  connStateUnlisten = await onConnectionState(({ fingerprint, up }) => {
    if (fingerprint !== selectedFingerprint.value) return
    if (!up) {
      toastStore.push('warning', '与对方的连接已断开，列表可能不是最新')
    } else {
      // 重连成功：主动刷一次（断线期间的变化没有推送）
      handleRefreshShares()
    }
  })
})

onUnmounted(() => {
  remoteSharesUnlisten?.()
  connStateUnlisten?.()
  if (refreshDebounceTimer !== null) {
    window.clearTimeout(refreshDebounceTimer)
  }
})
</script>

<style scoped>
.browse-page {
  padding: 20px;
  max-width: 1400px;
  margin: 0 auto;
  height: calc(100vh - 120px);
  display: flex;
  flex-direction: column;
}

.page-header {
  margin-bottom: 20px;
}

.page-header h1 {
  font-size: var(--text-xl);
  font-weight: 600;
  color: var(--gray-800);
  margin: 0;
}

/* 设备选择栏 */
.device-bar {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  flex-wrap: wrap;
  margin-bottom: var(--space-4);
}

.device-bar-label {
  font-size: var(--text-sm);
  color: var(--gray-500);
}

.device-chips {
  display: flex;
  gap: var(--space-2);
  flex-wrap: wrap;
}

.device-chip {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  min-height: var(--control-h-sm);
  padding: var(--space-1) var(--space-3);
  background: white;
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-full);
  font-size: var(--text-sm);
  color: var(--gray-700);
  cursor: pointer;
  transition: background-color var(--dur-fast) ease, border-color var(--dur-fast) ease;
}

.device-chip:hover:not(:disabled) {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

.device-chip.active {
  background: var(--primary-50);
  border-color: var(--primary-500);
  color: var(--primary-700);
  font-weight: 500;
}

.device-chip:disabled {
  opacity: 0.6;
  cursor: wait;
}

.chip-spinner {
  width: 12px;
  height: 12px;
  border: 2px solid var(--gray-200);
  border-top: 2px solid var(--primary-500);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

.device-bar-empty {
  font-size: var(--text-sm);
  color: var(--gray-400);
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.device-bar-link {
  color: var(--primary-500);
  text-decoration: none;
}

.device-bar-link:hover {
  color: var(--primary-600);
  text-decoration: underline;
}

.no-device-state {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
}

.no-device-content {
  text-align: center;
  max-width: 400px;
}

.no-device-icon {
  font-size: 64px;
  margin-bottom: 16px;
}

.no-device-content h3 {
  font-size: var(--text-lg);
  color: var(--gray-800);
  margin: 0 0 8px 0;
}

.no-device-content p {
  color: var(--gray-500);
  margin: 0 0 24px 0;
}

.browse-container {
  flex: 1;
  display: grid;
  grid-template-columns: 300px 1fr;
  gap: 20px;
  overflow: hidden;
}

.shares-panel, .files-panel {
  background: white;
  border-radius: var(--radius-md);
  border: 1px solid var(--gray-200);
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.panel-header {
  padding: 16px;
  border-bottom: 1px solid var(--gray-200);
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.panel-header h3 {
  font-size: var(--text-md);
  font-weight: 600;
  color: var(--gray-800);
  margin: 0;
}

.sort-control {
  display: flex;
  align-items: center;
  gap: var(--space-1, 4px);
}

.sort-btn {
  padding: 2px 8px;
  font-size: 12px;
  border: 1px solid var(--gray-300, #d1d5db);
  background: var(--gray-50, #f9fafb);
  color: var(--gray-600, #4b5563);
  border-radius: 6px;
  cursor: pointer;
}

.sort-btn.active {
  border-color: var(--primary, #6366f1);
  color: var(--primary, #6366f1);
  font-weight: 600;
}

.sort-btn:hover {
  background: var(--gray-100, #f3f4f6);
}

.btn-refresh-small {
  padding: 4px 8px;
  background: var(--gray-100);
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  color: var(--gray-700);
  cursor: pointer;
  transition: all 0.2s;
}

.btn-refresh-small:hover:not(:disabled) {
  background: var(--gray-200);
}

.btn-refresh-small:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.shares-list, .files-list {
  flex: 1;
  overflow-y: auto;
  padding: 8px;
}

.share-item {
  display: flex;
  align-items: center;
  padding: 12px;
  border-radius: var(--radius-md);
  cursor: pointer;
  transition: all 0.2s;
  margin-bottom: 4px;
}

.share-item:hover {
  background: var(--gray-50);
}

.share-item.active {
  background: var(--primary-100);
  border: 1px solid var(--primary-500);
}

.share-icon {
  font-size: var(--text-xl);
  margin-right: 12px;
}

.share-info {
  flex: 1;
}

.share-name {
  font-weight: 500;
  color: var(--gray-800);
  font-size: var(--text-base);
}

.share-id {
  font-size: var(--text-xs);
  color: var(--gray-500);
  margin-top: 2px;
}

.select-share-hint {
  flex: 1;
  display: flex;
  align-items: center;
  justify-content: center;
}

.hint-content {
  text-align: center;
}

.hint-icon {
  font-size: 48px;
  margin-bottom: 16px;
}

.hint-content h3 {
  font-size: var(--text-lg);
  color: var(--gray-800);
  margin: 0 0 8px 0;
}

.hint-content p {
  color: var(--gray-500);
  margin: 0;
}

.file-browser {
  flex: 1;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}

.browser-toolbar {
  padding: 12px 16px;
  border-bottom: 1px solid var(--gray-200);
  display: flex;
  justify-content: space-between;
  align-items: center;
}

.breadcrumb {
  display: flex;
  align-items: center;
  font-size: var(--text-base);
  color: var(--gray-500);
}

.breadcrumb-item {
  color: var(--gray-700);
  font-weight: 500;
  white-space: nowrap;
}

/* v0.2.8 可点击层级：跳转导航 */
.breadcrumb-item.clickable {
  cursor: pointer;
  color: var(--primary-600);
}

.breadcrumb-item.clickable:hover {
  text-decoration: underline;
}

.breadcrumb-separator {
  margin: 0 4px;
  color: var(--gray-400);
  flex-shrink: 0;
}

/* v0.2.8 返回上一级按钮 */
.btn-up {
  width: 28px;
  height: 28px;
  margin-right: var(--space-2);
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  background: white;
  color: var(--gray-600);
  font-size: 14px;
  cursor: pointer;
  flex-shrink: 0;
  transition: background-color var(--dur-fast) ease, border-color var(--dur-fast) ease;
}

.btn-up:hover {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

/* 深层路径溢出滚动，不把工具栏撑爆 */
.breadcrumb {
  overflow-x: auto;
  min-width: 0;
}

.file-item {
  display: flex;
  align-items: center;
  padding: 12px;
  border-radius: var(--radius-md);
  margin-bottom: 4px;
  transition: all 0.2s;
}

.file-item:hover {
  background: var(--gray-50);
}

.file-item.selected {
  background: var(--primary-100);
  border: 1px solid var(--primary-500);
}

/* v0.2.6 文件夹快捷下载：默认隐形，hover 行时浮出 */
.dir-download-btn {
  margin-left: auto;
  padding: 4px 12px;
  min-height: 28px;
  border: 1px solid var(--primary-500);
  border-radius: var(--radius-sm);
  background: white;
  color: var(--primary-600);
  font-size: var(--text-sm);
  cursor: pointer;
  opacity: 0;
  transition: opacity var(--dur-fast) ease, background-color var(--dur-fast) ease;
}

.file-item:hover .dir-download-btn {
  opacity: 1;
}

.dir-download-btn:hover {
  background: var(--primary-50);
}

.file-checkbox {
  margin-right: 12px;
}

.file-checkbox input[type="checkbox"] {
  width: 16px;
  height: 16px;
  cursor: pointer;
}

.file-icon {
  font-size: var(--text-lg);
  margin-right: 12px;
  cursor: pointer;
  user-select: none;
}

.file-icon.directory:hover {
  opacity: 0.7;
}

.file-info {
  flex: 1;
  min-width: 0;
}

.file-info.directory {
  cursor: pointer;
}

.file-info.directory:hover .file-name {
  color: var(--primary-500);
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
}

.load-more-item {
  padding: 12px;
  text-align: center;
  cursor: pointer;
  color: var(--primary-500);
  font-size: var(--text-base);
  border-radius: var(--radius-md);
  transition: all 0.2s;
}

.load-more-item:hover {
  background: var(--gray-50);
}

.files-actions {
  padding: 12px 16px;
  border-top: 1px solid var(--gray-200);
  display: flex;
  justify-content: space-between;
  align-items: center;
  background: var(--gray-50);
}

.selection-info {
  font-size: var(--text-base);
  color: var(--gray-700);
  font-weight: 500;
}

.btn {
  padding: 8px 16px;
  border: none;
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  text-decoration: none;
  transition: all 0.2s;
  display: inline-block;
}

.btn-primary {
  background: var(--primary-500);
  color: white;
}

.btn-primary:hover:not(:disabled) {
  background: var(--primary-600);
}

.btn-primary:disabled {
  opacity: 0.7;
  cursor: not-allowed;
}

.btn-small {
  padding: 4px 8px;
  font-size: var(--text-xs);
  background: var(--primary-500);
  color: white;
  border: none;
  border-radius: var(--radius-sm);
  cursor: pointer;
}

.loading-state, .error-state, .empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 40px 20px;
  text-align: center;
}

.spinner-small {
  width: 24px;
  height: 24px;
  border: 3px solid var(--gray-200);
  border-top: 3px solid var(--primary-500);
  border-radius: 50%;
  animation: spin 1s linear infinite;
}

@keyframes spin {
  0% { transform: rotate(0deg); }
  100% { transform: rotate(360deg); }
}

.error-text {
  color: var(--danger-500);
  font-size: var(--text-base);
  margin: 0 0 12px 0;
}

.error-state p, .empty-state p {
  color: var(--gray-500);
  font-size: var(--text-base);
  margin: 0 0 12px 0;
}
</style>