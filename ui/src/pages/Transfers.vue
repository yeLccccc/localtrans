<template>
  <div class="transfers-page">
    <div class="page-header">
      <h1>传输任务</h1>
      <div class="header-actions">
        <button @click="toggleDiskHistory" class="btn-secondary" data-testid="transfers-disk-history-toggle-btn">
          {{ diskHistoryExpanded ? '收起历史记录' : '历史记录' }}
        </button>
        <button @click="handleClearCompleted" class="btn-secondary" data-testid="transfers-clear-completed-btn">清除已完成/失败</button>
      </div>
    </div>

    <!-- 传输任务列表 -->
    <!-- v0.2.5：续传横幅移除——中断任务卡片上本就有"续传"按钮，
         横幅是重复入口且打扰（用户反馈） -->
    <div v-if="loading" class="loading-state">
      <div class="spinner"></div>
      <p>加载中...</p>
    </div>

    <div v-else-if="error" class="error-state">
      <p>{{ error }}</p>
      <button @click="initialLoad" class="btn-secondary" data-testid="transfers-error-retry-btn">重试</button>
    </div>

    <template v-else>
      <!-- 进行中 -->
      <div v-if="activeTransfers.length === 0" class="empty-state">
        <p>暂无传输任务</p>
        <p class="hint">试试从设备页发起传输,或在浏览页下载文件</p>
      </div>
      <div v-else class="transfer-list">
        <TransferItem
          v-for="transfer in activeTransfers"
          :key="transfer.job_id"
          :transfer="transfer"
        />
      </div>

      <!-- 历史(默认折叠) -->
      <div v-if="historyTransfers.length > 0" class="section section-history">
        <button class="section-toggle" data-testid="transfers-history-fold-btn" @click="historyExpanded = !historyExpanded">
          <span class="section-arrow">{{ historyExpanded ? '▾' : '▸' }}</span>
          历史 ({{ historyTransfers.length }})
        </button>
        <div v-if="historyExpanded" class="transfer-list">
          <TransferItem
            v-for="transfer in historyTransfers"
            :key="transfer.job_id"
            :transfer="transfer"
          />
        </div>
      </div>

      <!-- 磁盘历史(含已移除) -->
      <div v-if="diskHistoryExpanded" class="section section-disk">
        <div class="section-header">磁盘历史（含已移除）</div>
        <div v-if="diskJobs.length === 0" class="disk-empty">暂无磁盘历史记录</div>
        <div v-else class="disk-list">
          <div v-for="job in diskJobs" :key="job.job_id" class="disk-row" :data-testid="`transfers-disk-item-${job.job_id}`">
            <div class="disk-main">
              <span class="disk-name" :title="job.display_name">{{ job.display_name }}</span>
              <span :class="`status-badge status-${job.state}`">{{ diskStateText(job.state) }}</span>
              <span class="disk-meta">{{ formatSize(job.total) }}</span>
              <span class="disk-meta">{{ directionText(job.direction) }}</span>
              <span class="disk-meta">{{ formatTime(job.created_at_ms) }}</span>
            </div>
            <div class="disk-actions">
              <button class="btn-mini" data-testid="transfers-disk-restore-btn" @click="handleRestoreDiskJob(job.job_id)">恢复到列表</button>
              <button class="btn-mini btn-danger" data-testid="transfers-disk-destroy-btn" @click="handleDestroyDiskJob(job.job_id)">彻底删除</button>
            </div>
          </div>
        </div>
      </div>
    </template>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { storeToRefs } from 'pinia'
import { uiConfirm as confirmDialog } from '../test-support/confirmAdapter'
import TransferItem from '../components/TransferItem.vue'
import { useTransfersStore } from '../stores/transfers'
import { useToastStore } from '../stores/toast'
import { splitActiveHistory } from '../lib/transferDisplay'

const transfersStore = useTransfersStore()
const toastStore = useToastStore()

const { transfers, loading, error, diskJobs } = storeToRefs(transfersStore)

const historyExpanded = ref(false)
const diskHistoryExpanded = ref(false)

const split = computed(() => splitActiveHistory(transfers.value ?? []))
const activeTransfers = computed(() => split.value.active)
const historyTransfers = computed(() => split.value.history)

/**
 * 磁盘历史区状态文案(未知原样)
 */
function diskStateText(state: string): string {
  const map: Record<string, string> = {
    pending: '等待中',
    active: '进行中',
    paused: '已暂停',
    done: '已完成',
    failed: '失败',
    interrupted: '已中断',
  }
  return map[state] || state
}

function directionText(direction: string): string {
  return direction === 'pull' ? '接收' : '发送'
}

/**
 * 字节量格式化(轻量行用,与 TransferItem 同口径)
 */
function formatSize(bytes: number): string {
  if (!bytes) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unitIndex = 0
  while (value >= 1024 && unitIndex < units.length - 1) {
    value /= 1024
    unitIndex++
  }
  return `${value.toFixed(1)} ${units[unitIndex]}`
}

function formatTime(ms: number | null): string {
  if (!ms) return '—'
  const d = new Date(ms)
  const pad = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}`
}

/**
 * 打开/关闭磁盘历史区(打开时拉取一次)
 */
async function toggleDiskHistory() {
  diskHistoryExpanded.value = !diskHistoryExpanded.value
  if (diskHistoryExpanded.value) {
    await transfersStore.refreshDiskJobs()
  }
}

/**
 * 恢复磁盘历史任务到视图(后端 removed=false,卡片回活动区)
 */
async function handleRestoreDiskJob(jobId: string) {
  try {
    await transfersStore.restoreDiskJob(jobId)
    await transfersStore.refreshTransfers()
    toastStore.push('success', '已恢复到列表')
  } catch (e) {
    toastStore.push('error', '恢复失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

/**
 * 彻底删除磁盘历史任务(需确认)
 */
async function handleDestroyDiskJob(jobId: string) {
  const ok = await confirmDialog('彻底删除该历史记录?此操作不可恢复', { title: '彻底删除' })
  if (!ok) return
  try {
    await transfersStore.destroyDiskJob(jobId)
    toastStore.push('success', '已彻底删除')
  } catch (e) {
    toastStore.push('error', '删除失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

/**
 * 清除已完成/失败的传输任务(语义=移除视图)
 */
async function handleClearCompleted() {
  try {
    const n = await transfersStore.clearCompleted()
    toastStore.push('success', `已清除 ${n} 个历史任务`)
  } catch (e) {
    toastStore.push('error', '清除失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

/**
 * 初始化:事件驱动已覆盖后续刷新,这里只做一次初始加载
 */
async function initialLoad() {
  await transfersStore.refreshTransfers()
  await transfersStore.refreshResumeJobs()
}

onMounted(initialLoad)
</script>

<style scoped>
.transfers-page {
  padding: var(--space-5);
  padding-bottom: var(--space-8);
  max-width: 1200px;
  margin: 0 auto;
  min-height: calc(100vh - 160px); /* 减去顶部导航+底部tab */
  display: flex;
  flex-direction: column;
}

.page-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: var(--space-6);
}

.header-actions {
  display: flex;
  gap: var(--space-2);
}

.page-header h1 {
  font-size: var(--text-xl);
  font-weight: 600;
  color: var(--gray-800);
  margin: 0;
}

.btn-secondary {
  padding: var(--space-2) var(--space-4);
  min-height: var(--control-h-md);
  background: white;
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  color: var(--gray-700);
  font-size: var(--text-base);
  cursor: pointer;
  transition: background-color var(--dur-fast) ease, border-color var(--dur-fast) ease;
}

.btn-secondary:hover {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

.loading-state, .error-state, .empty-state {
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  padding: 60px var(--space-5);
  text-align: center;
}

.spinner {
  width: 40px;
  height: 40px;
  border: 4px solid var(--gray-200);
  border-top: 4px solid var(--primary-500);
  border-radius: 50%;
  animation: spin 1s linear infinite;
  margin-bottom: var(--space-4);
}

@keyframes spin {
  0% { transform: rotate(0deg); }
  100% { transform: rotate(360deg); }
}

.loading-state p, .error-state p, .empty-state p {
  color: var(--gray-500);
  font-size: var(--text-md);
  margin: 0 0 var(--space-4) 0;
}

.error-state p {
  color: var(--danger-500);
}

.empty-state .hint {
  color: var(--gray-400);
  font-size: var(--text-base);
  margin-top: var(--space-2);
}

.transfer-list {
  display: flex;
  flex-direction: column;
}

.section {
  margin-top: var(--space-5);
}

/* 历史/磁盘历史收起时贴底——用户反馈:悬浮中间不美观 */
.section-history,
.section-disk {
  margin-top: auto;
  padding-top: var(--space-5);
}

.section-toggle {
  display: inline-flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  background: white;
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  color: var(--gray-700);
  font-size: var(--text-base);
  font-weight: 600;
  cursor: pointer;
  transition: background-color var(--dur-fast) ease, border-color var(--dur-fast) ease;
}

.section-toggle:hover {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

.section-arrow {
  color: var(--gray-400);
}

.section-header {
  font-size: var(--text-base);
  font-weight: 600;
  color: var(--gray-700);
  margin-bottom: var(--space-3);
}

.disk-empty {
  color: var(--gray-400);
  font-size: var(--text-base);
  padding: var(--space-4) 0;
}

.disk-list {
  display: flex;
  flex-direction: column;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  overflow: hidden;
}

.disk-row {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: var(--space-3);
  padding: var(--space-2) var(--space-4);
  background: white;
}

.disk-row + .disk-row {
  border-top: 1px solid var(--gray-100);
}

.disk-main {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  min-width: 0;
  flex: 1;
}

.disk-name {
  color: var(--gray-800);
  font-size: var(--text-base);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
  max-width: 280px;
}

.disk-meta {
  color: var(--gray-500);
  font-size: var(--text-sm, var(--text-base));
  white-space: nowrap;
}

.disk-actions {
  display: flex;
  gap: var(--space-2);
  flex-shrink: 0;
}

.btn-mini {
  padding: var(--space-1) var(--space-3);
  background: white;
  border: 1px solid var(--gray-300);
  border-radius: var(--radius-sm);
  color: var(--gray-700);
  font-size: var(--text-sm, var(--text-base));
  cursor: pointer;
  transition: background-color var(--dur-fast) ease, border-color var(--dur-fast) ease;
}

.btn-mini:hover {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

.btn-danger:hover {
  background: var(--danger-50, var(--gray-50));
  border-color: var(--danger-500);
  color: var(--danger-500);
}
</style>
