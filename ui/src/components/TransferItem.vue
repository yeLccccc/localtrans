<template>
  <div class="transfer-item" :class="[
    `transfer-${transfer.state}`,
    `transfer-role-${transfer.local_role || 'destination'}`
  ]" :data-testid="`transfer-item-${transfer.job_id}`">
    <div class="transfer-header">
      <div class="transfer-info">
        <div class="transfer-name">{{ transfer.name }}</div>
        <div class="transfer-peer">
          <span class="role-badge">{{ roleText }}</span><span :title="peerAbbr">{{ peerName }}</span>
        </div>
      </div>
      <div class="transfer-status">
        <span :class="`status-badge status-${transfer.state}`">
          {{ stateText }}
        </span>
        <span v-if="transfer.state === 'failed' && transfer.fail_reason" class="fail-reason">{{ failReasonText }}</span>
      </div>
      <button
        v-if="hasChildren"
        class="expand-toggle"
        :title="expanded ? '收起子项' : '展开子项'"
        data-testid="transfer-expand-btn"
        @click="expanded = !expanded"
      >{{ expanded ? '▾' : '▸' }}</button>
    </div>

    <div v-if="showProgress" class="transfer-progress">
      <div class="progress-bar">
        <div class="progress-fill" :class="{ awaiting: isSender && senderState === 'awaiting-confirm' }" :style="{ width: `${mainPercent}%` }"></div>
      </div>
      <div class="progress-info">
        <span class="progress-percent">{{ mainPercent.toFixed(1) }}%</span>
        <span v-if="transfer.total > 0" class="progress-bytes">
          {{ formatSize(mainDone) }} / {{ formatSize(transfer.total) }}
        </span>
        <!-- R1:积压>20% 或满格未确认 → 只显"等待对方确认"文案态(不渲染第二条/角标) -->
        <span v-if="awaitingConfirm" class="awaiting-confirm-text" data-testid="transfer-awaiting-confirm">
          等待对方确认
        </span>
        <span v-if="transfer.speed_bps > 0 && !isTerminal" class="progress-speed" data-testid="transfer-speed">
          {{ formatSpeed(transfer.speed_bps) }}
        </span>
      </div>
    </div>

    <div v-if="expanded && hasChildren" class="children-panel">
      <div v-for="child in transfer.children" :key="child.job_id" class="child-row">
        <span class="child-name" :title="child.name">{{ child.name }}</span>
        <span class="child-size">{{ formatSize(child.total) }}</span>
        <span :class="`status-badge status-${child.state}`">{{ childStateText(child.state) }}</span>
        <div class="child-bar">
          <div class="child-fill" :style="{ width: `${childPercent(child)}%` }"></div>
        </div>
        <button
          v-if="child.state === 'failed' && child.job_id"
          class="btn btn-secondary child-retry"
          @click="handleRetryChild(child)"
        >重试</button>
      </div>
    </div>

    <div v-if="transfer.state === 'active' && transfer.health" class="health-panel">
      <span class="metric"><span class="metric-label">丢包</span>{{ (transfer.health.loss_ratio * 100).toFixed(1) }}%</span>
      <span class="metric"><span class="metric-label">RTT</span>{{ transfer.health.rtt_ms }}ms</span>
      <span class="metric"><span class="metric-label">cwnd</span>{{ transfer.health.cwnd }}</span>
      <span class="metric"><span class="metric-label">流</span>{{ transfer.health.streams }}</span>
    </div>

    <div class="time-info">
      <span v-if="elapsedSeconds !== null">已用 {{ formatDuration(elapsedSeconds) }}</span>
      <span v-if="etaSeconds !== null && etaSeconds > 0">剩余 {{ formatDuration(etaSeconds) }}</span>
      <span v-else-if="etaSeconds === 0">即将完成</span>
    </div>

    <div class="transfer-actions">
      <!-- destination:暂停/继续/取消/重试/续传/打开/删除 -->
      <template v-if="(transfer.local_role || 'destination') === 'destination'">
        <template v-if="transfer.state === 'pending'">
          <span class="pending-hint">{{ queueHint }}</span>
        </template>
        <template v-else-if="transfer.state === 'active'">
          <button @click="handlePause" class="btn btn-warning" data-testid="transfer-pause-btn">暂停</button>
          <button @click="handleCancel" class="btn btn-danger" data-testid="transfer-cancel-btn">取消</button>
        </template>
        <template v-else-if="transfer.state === 'paused'">
          <button @click="handleResume" class="btn btn-primary" data-testid="transfer-resume-btn">继续</button>
          <button @click="handleCancel" class="btn btn-danger" data-testid="transfer-cancel-btn">取消</button>
        </template>
        <template v-else-if="['failed', 'interrupted'].includes(transfer.state)">
          <button v-if="hasRequestRecord" @click="handleRetry" class="btn btn-primary" data-testid="transfer-retry-btn">重试</button>
          <button @click="handleResumePending" class="btn btn-primary" data-testid="transfer-resume-pending-btn">续传</button>
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
        <template v-else-if="transfer.state === 'done'">
          <button @click="handleOpenFolder" class="btn btn-success" data-testid="transfer-open-folder-btn">打开所在文件夹</button>
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
      </template>

      <!-- source-push:暂停/继续/取消 + 删除 -->
      <template v-else-if="transfer.local_role === 'source-push'">
        <template v-if="transfer.state === 'pending'">
          <span class="pending-hint">{{ pushQueueHint }}</span>
        </template>
        <template v-else-if="transfer.state === 'active'">
          <button @click="handlePause" class="btn btn-warning" data-testid="transfer-pause-btn">暂停</button>
          <button @click="handleCancel" class="btn btn-danger" data-testid="transfer-cancel-btn">取消</button>
        </template>
        <template v-else-if="transfer.state === 'paused'">
          <button @click="handleResume" class="btn btn-primary" data-testid="transfer-resume-btn">继续</button>
          <button @click="handleCancel" class="btn btn-danger" data-testid="transfer-cancel-btn">取消</button>
        </template>
        <template v-else-if="transfer.state === 'interrupted'">
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
        <template v-else-if="transfer.state === 'failed'">
          <button v-if="canResend" @click="handleResend" class="btn btn-primary" data-testid="btn-resend">重发</button>
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
        <template v-else>
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
      </template>

      <!-- source-pull:限速 / 踢人 -->
      <template v-else-if="transfer.local_role === 'source-pull'">
        <template v-if="transfer.state === 'pending'">
          <span class="pending-hint">等待对方连接...</span>
        </template>
        <template v-else-if="transfer.state === 'active'">
          <button @click="throttleMenuOpen = !throttleMenuOpen" class="btn btn-warning" data-testid="transfer-throttle-btn">限速</button>
          <div v-if="throttleMenuOpen" v-click-outside="() => throttleMenuOpen = false" class="throttle-menu">
            <button @click="throttleTo(1)">1 流</button>
            <button @click="throttleTo(2)">2 流</button>
            <button @click="throttleTo(4)">4 流</button>
            <button @click="throttleTo(0)">不限</button>
          </div>
          <button @click="handleKick" class="btn btn-danger" data-testid="transfer-kick-btn">踢人</button>
        </template>
        <template v-else-if="transfer.state === 'interrupted'">
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
        <template v-else>
          <button @click="handleRemove" class="btn btn-secondary" data-testid="transfer-remove-btn">删除</button>
        </template>
      </template>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import type { ChildDto, TransferDto } from '../types'
import { useTransfersStore } from '../stores/transfers'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { api, friendlyError } from '../api'
import { uiConfirm as confirmDialog } from '../test-support/confirmAdapter'
import {
  peerDisplayName,
  progressPair,
  senderDisplayState,
  queueText,
  fingerprintAbbr,
  childRetryParams,
} from '../lib/transferDisplay'
import { formatSpeed } from '../lib/speedSmooth'

const props = defineProps<{
  transfer: TransferDto
}>()

const transfersStore = useTransfersStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()

// Throttle menu state
const throttleMenuOpen = ref(false)

// 父卡展开状态
const expanded = ref(false)

// 计算属性
const peerName = computed(() => peerDisplayName(props.transfer.peer, settingsStore.trustedPeers))
const peerAbbr = computed(() => fingerprintAbbr(props.transfer.peer))

const isSender = computed(() => props.transfer.local_role === 'source-push')

const senderState = computed(() => senderDisplayState(props.transfer))

const pair = computed(() => progressPair(props.transfer))
const mainDone = computed(() => pair.value.mainDone)

// R1 单进度条:发送方主进度=对端确认(remoteDone 优先,无镜像数据回退 done);
// 积压>20% 或满格未确认 → "等待对方确认"文案态(对齐 Android isAwaitingConfirmText:
// source-push + active/paused + 非正常态)
const awaitingConfirm = computed(() => {
  return isSender.value
    && ['active', 'paused'].includes(props.transfer.state)
    && senderState.value !== 'normal'
})

const mainPercent = computed(() => {
  if (props.transfer.total === 0) return 0
  return (mainDone.value / props.transfer.total) * 100
})

const hasChildren = computed(() => (props.transfer.children?.length ?? 0) > 0)

function childPercent(child: ChildDto): number {
  if (child.total === 0) return child.state === 'done' ? 100 : 0
  return Math.min(100, (child.done / child.total) * 100)
}

function childStateText(state: string): string {
  const map: Record<string, string> = {
    pending: '等待中',
    active: '进行中',
    paused: '已暂停',
    done: '完成',
    failed: '失败',
    interrupted: '已中断'
  }
  return map[state] || state
}

const stateText = computed(() => {
  const stateMap: Record<string, string> = {
    pending: '等待中',
    active: '进行中',
    paused: '已暂停',
    done: '已完成',
    failed: '失败',
    interrupted: '已中断'
  }
  return stateMap[props.transfer.state] || props.transfer.state
})

// A8 隐私修复:fail_reason 展示前脱敏 Windows 绝对路径为主文件名
const failReasonText = computed(() => props.transfer.fail_reason ? friendlyError(props.transfer.fail_reason) : '')

const roleText = computed(() => {
  const map: Record<string, string> = {
    'destination': '接收方',
    'source-push': '推送方',
    'source-pull': '被取方',
  }
  return map[props.transfer.local_role] || '传输'
})

// pending 提示:有排队位次显示位次,否则回退方向固定文案
const queueHint = computed(() => queueText(props.transfer.queue_pos, props.transfer.direction))
// 推送方 pending 回退文案是"等待对方确认接收...",与其他方向不同
const pushQueueHint = computed(() =>
  props.transfer.queue_pos != null
    ? queueText(props.transfer.queue_pos, props.transfer.direction)
    : '等待对方确认接收...'
)

// 字节量格式化（进度明细行用）
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

const elapsedSeconds = computed(() => {
  if (!props.transfer.started_at_ms) return null
  // 终态冻结计时：以 finished_at_ms 为终点，不再随墙上时钟走表
  const end = props.transfer.finished_at_ms ?? Date.now()
  return Math.max(0, Math.floor((end - props.transfer.started_at_ms) / 1000))
})

const isTerminal = computed(() => ['done', 'failed', 'interrupted'].includes(props.transfer.state))

const etaSeconds = computed(() => {
  // 终态不再显示"即将完成"——已完成/已失败的任务没有剩余时间概念
  // speed_bps 已是 store 平滑后的显示值(R2)——ETA 随平滑速度计算,不随瞬跳
  if (isTerminal.value) return null
  if (props.transfer.speed_bps <= 0) return null
  if (props.transfer.done >= props.transfer.total) return 0
  return Math.ceil((props.transfer.total - props.transfer.done) / props.transfer.speed_bps)
})

function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`
  if (secs < 3600) return `${Math.floor(secs/60)}m${secs%60}s`
  const h = Math.floor(secs/3600)
  const m = Math.floor((secs%3600)/60)
  return `${h}h${m}m`
}


const showProgress = computed(() => {
  return props.transfer.state === 'active' ||
         props.transfer.state === 'paused' ||
         (props.transfer.state === 'done' && props.transfer.total > 0)
})

const hasRequestRecord = computed(() => {
  return transfersStore.lastRequest.has(props.transfer.job_id)
})

// v0.5.0 推送任务失败重发条件
const canResend = computed(() => {
  if (props.transfer.local_role !== 'source-push' || props.transfer.state !== 'failed') return false
  return transfersStore.lastRequest.get(props.transfer.job_id)?.type === 'push-rel'
})

// 格式化速度:R2 起收口到 lib/speedSmooth(与 Android Formatters.formatSpeed 口径一致)

// 事件处理
async function handlePause() {
  try {
    await transfersStore.transferAction(props.transfer.job_id, 'pause')
    toastStore.push('info', '任务已暂停')
  } catch (e) {
    toastStore.push('error', '暂停失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function handleResume() {
  try {
    await transfersStore.transferAction(props.transfer.job_id, 'resume')
    toastStore.push('info', '任务已继续')
  } catch (e) {
    toastStore.push('error', '继续失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function handleCancel() {
  try {
    await transfersStore.transferAction(props.transfer.job_id, 'cancel')
    toastStore.push('info', '任务已取消')
  } catch (e) {
    toastStore.push('error', '取消失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function handleRetry() {
  try {
    await transfersStore.retryTransfer(props.transfer.job_id)
    toastStore.push('info', '正在重试任务...')
  } catch (e) {
    toastStore.push('error', '重试失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function handleOpenFolder() {
  try {
    // 走 Rust 侧命令打开下载目录:download_dir 是用户自定义路径,
    // JS opener scope 盖不住(v0.11.0 收紧后自定义目录被拒),Rust 侧
    // 只开 config 配置的那一个目录,不放宽 JS 权限面
    const { invoke } = await import('@tauri-apps/api/core')
    await invoke('open_download_dir')
  } catch (e) {
    toastStore.push('error', '打开文件夹失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

// New handlers for Task 16
async function handleKick() {
  try {
    const ok = await confirmDialog(`踢出对端 ${peerName.value} 的拉取?此操作不可恢复`, { title: '踢人' })
    if (!ok) return
    await transfersStore.transferAction(props.transfer.job_id, 'cancel')
    toastStore.push('info', '已踢出对端')
  } catch (e) {
    toastStore.push('error', '踢人失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function throttleTo(maxStreams: number) {
  try {
    throttleMenuOpen.value = false
    await transfersStore.throttleTransfer(props.transfer.job_id, maxStreams || 0xFFFFFFFF)
    toastStore.push('info', maxStreams ? `已限速到 ${maxStreams} 流` : '已放开限速')
  } catch (e) {
    toastStore.push('error', '限速失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

// 两级删除:终态卡弹两级选择(ok=彻底删除磁盘数据,cancel=仅移除列表视图,可在历史记录找回);
// 活动卡维持 destroy 流程
async function handleRemove() {
  try {
    if (isTerminal.value) {
      const destroy = await confirmDialog(
        '彻底删除磁盘数据?此不可恢复。\n[确定] = 彻底删除;[取消] = 仅从列表移除(磁盘保留,可在历史记录找回)',
        { title: '删除任务' }
      )
      await transfersStore.removeTransfer(props.transfer.job_id, destroy ? 'destroy' : 'view')
      toastStore.push('info', destroy ? '任务已彻底删除' : '任务已移除,可在历史记录找回')
    } else {
      await transfersStore.removeTransfer(props.transfer.job_id, 'destroy')
      toastStore.push('info', '任务已删除')
    }
  } catch (e) {
    toastStore.push('error', '删除失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

// 子项重试:fp 取父卡对端,rel_dir/path 从父卡记录的请求参数取,取不到则提示
async function handleRetryChild(child: ChildDto) {
  try {
    const req = transfersStore.lastRequest.get(props.transfer.job_id)
    // push(绝对路径)/pull 只需 path(relDir 传空串);push-rel 需要 path+relDir
    const params = childRetryParams(req)
    if (!params) {
      toastStore.push('error', '缺少重试参数')
      return
    }
    await api.transfers.retryChild(props.transfer.job_id, child.job_id, props.transfer.peer, params.relDir, params.path)
    toastStore.push('info', '正在重试子项...')
  } catch (e) {
    toastStore.push('error', '重试失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

async function handleResumePending() {
  try {
    await api.transfers.resumePending(props.transfer.job_id)
    toastStore.push('info', '正在续传...')
  } catch (e) {
    toastStore.push('error', '续传失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}

// v0.5.0 重发推送任务
async function handleResend() {
  try {
    await transfersStore.retryTransfer(props.transfer.job_id)
    toastStore.push('info', '已重新发起推送')
  } catch (e) {
    toastStore.push('error', '重发失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}
</script>

<style scoped>
.transfer-item {
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-md);
  padding: var(--space-4);
  margin-bottom: var(--space-3);
  background: white;
  box-shadow: var(--shadow-sm);
  transition: box-shadow var(--dur-fast) ease;
}

.transfer-item:hover {
  box-shadow: var(--shadow-md);
}

/* 状态左边条:优先按状态,角色色作底色补充 */
.transfer-active {
  border-left: 4px solid var(--primary-500);
}

.transfer-paused {
  border-left: 4px solid var(--warning-500);
}

.transfer-done {
  border-left: 4px solid var(--success-500);
}

.transfer-failed {
  border-left: 4px solid var(--danger-500);
}

.transfer-interrupted {
  border-left: 4px solid var(--gray-400);
}

.transfer-role-destination {
  border-left: 4px solid var(--role-destination);
}

.transfer-role-source-push {
  border-left: 4px solid var(--role-source-push);
}

.transfer-role-source-pull {
  border-left: 4px solid var(--role-source-pull);
}

/* 状态条与角色条并存时,active 用角色色 */
.transfer-active.transfer-role-destination { border-left-color: var(--role-destination); }
.transfer-active.transfer-role-source-push { border-left-color: var(--role-source-push); }
.transfer-active.transfer-role-source-pull { border-left-color: var(--role-source-pull); }

.transfer-header {
  display: flex;
  justify-content: space-between;
  align-items: flex-start;
  margin-bottom: var(--space-3);
}

.transfer-info {
  flex: 1;
  min-width: 0;
}

.transfer-name {
  font-weight: 600;
  font-size: var(--text-md);
  color: var(--gray-800);
  margin-bottom: var(--space-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.transfer-peer {
  font-size: var(--text-xs);
  color: var(--gray-500);
  display: flex;
  align-items: center;
  gap: var(--space-2);
}

.transfer-status {
  flex-shrink: 0;
  margin-left: var(--space-2);
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
}

.fail-reason {
  font-size: var(--text-xs);
  color: var(--danger-600);
  font-weight: 500;
}

.status-badge {
  display: inline-block;
  padding: var(--space-1) var(--space-3);
  border-radius: var(--radius-full);
  font-size: var(--text-xs);
  font-weight: 500;
}

.status-pending {
  background: var(--gray-100);
  color: var(--gray-600);
}

.status-active {
  background: var(--primary-100);
  color: var(--primary-700);
}

.status-paused {
  background: var(--warning-50);
  color: var(--warning-600);
}

.status-done {
  background: var(--success-50);
  color: var(--success-600);
}

.status-failed {
  background: var(--danger-50);
  color: var(--danger-600);
}

.status-interrupted {
  background: var(--gray-100);
  color: var(--gray-600);
}

.transfer-progress {
  margin-bottom: var(--space-3);
}

.progress-bar {
  width: 100%;
  height: 8px;
  background: var(--gray-100);
  border-radius: var(--radius-full);
  overflow: hidden;
  margin-bottom: var(--space-1);
}

.progress-fill {
  height: 100%;
  background: var(--primary-500);
  border-radius: var(--radius-full);
  /* v0.2.6：4Hz 更新间的视觉补间（与滑动窗口的块级到达叠加，
     台阶在视觉上基本抹平）；终态一步到位不再动画 */
  transition: width 0.28s linear;
}

.transfer-done .progress-fill,
.transfer-failed .progress-fill,
.transfer-interrupted .progress-fill {
  transition: none;
}

/* 按角色着色进度条 */
.transfer-role-destination .progress-fill { background: var(--role-destination); }
.transfer-role-source-push .progress-fill { background: var(--role-source-push); }
.transfer-role-source-pull .progress-fill { background: var(--role-source-pull); }

.progress-info {
  display: flex;
  gap: var(--space-3);
  justify-content: space-between;
  align-items: baseline;
  font-size: var(--text-xs);
  color: var(--gray-500);
  font-variant-numeric: tabular-nums;
}

.progress-percent {
  font-weight: 600;
  color: var(--gray-700);
}

.progress-bytes {
  margin-left: auto;
}

.transfer-actions {
  position: relative;
  display: flex;
  gap: var(--space-2);
  align-items: center;
  margin-top: var(--space-3);
}

.btn {
  padding: var(--space-2) var(--space-4);
  min-height: var(--control-h-md);
  border: none;
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  transition: background-color var(--dur-fast) ease;
}

.btn-primary {
  background: var(--primary-600);
  color: white;
}

.btn-primary:hover {
  background: var(--primary-700);
}

.btn-warning {
  background: var(--warning-500);
  color: white;
}

.btn-warning:hover {
  background: var(--warning-600);
}

.btn-danger {
  background: var(--danger-500);
  color: white;
}

.btn-danger:hover {
  background: var(--danger-600);
}

.btn-success {
  background: var(--success-500);
  color: white;
}

.btn-success:hover {
  background: var(--success-600);
}

.btn-secondary {
  background: transparent;
  color: var(--gray-600);
  border: 1px solid var(--gray-300);
}

.btn-secondary:hover {
  background: var(--gray-50);
  border-color: var(--gray-400);
}

.retry-hint, .push-hint, .pending-hint {
  font-size: var(--text-xs);
  color: var(--gray-500);
  font-style: italic;
}

/* 父卡展开切换钮 */
.expand-toggle {
  background: none;
  border: none;
  color: var(--gray-500);
  cursor: pointer;
  font-size: var(--text-md);
  padding: 0 var(--space-1);
  line-height: 1;
}

.expand-toggle:hover {
  color: var(--gray-700);
}

/* 满格未确认:条纹动画,终态关动画 */
.progress-fill.awaiting {
  background-image: repeating-linear-gradient(
    45deg,
    transparent,
    transparent 6px,
    rgba(255, 255, 255, 0.35) 6px,
    rgba(255, 255, 255, 0.35) 12px
  );
  animation: awaiting-stripes 1.2s linear infinite;
}

.transfer-done .progress-fill.awaiting,
.transfer-failed .progress-fill.awaiting,
.transfer-interrupted .progress-fill.awaiting {
  animation: none;
}

@keyframes awaiting-stripes {
  from { background-position: 0 0; }
  to { background-position: 24px 0; }
}

/* R1:积压/满格未确认 → 仅"等待对方确认"文案态(不渲染第二条进度条/角标) */
.awaiting-confirm-text {
  color: var(--warning-600, var(--gray-500));
  white-space: nowrap;
}

/* 父卡子项展开区 */
.children-panel {
  margin-top: var(--space-2);
  border-top: 1px dashed var(--gray-200);
  padding-top: var(--space-2);
}

.child-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-1) 0;
  font-size: var(--text-xs);
  color: var(--gray-600);
}

.child-name {
  flex: 1;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.child-size {
  flex-shrink: 0;
  color: var(--gray-400);
  font-variant-numeric: tabular-nums;
}

.child-bar {
  width: 80px;
  height: 4px;
  flex-shrink: 0;
  background: var(--gray-100);
  border-radius: var(--radius-full);
  overflow: hidden;
}

.child-fill {
  height: 100%;
  background: var(--primary-500);
  border-radius: var(--radius-full);
}

.child-retry {
  flex-shrink: 0;
  min-height: var(--control-h-sm);
  padding: 0 var(--space-2);
  font-size: var(--text-xs);
}

/* 健康面板:分组 + 分隔点 + 等宽数字 */
.health-panel {
  display: flex;
  align-items: center;
  flex-wrap: wrap;
  gap: var(--space-1) var(--space-2);
  font-size: var(--text-xs);
  color: var(--gray-500);
  margin-top: var(--space-1);
  font-variant-numeric: tabular-nums;
}

.health-panel .metric + .metric::before {
  content: "·";
  margin-right: var(--space-2);
  color: var(--gray-300);
}

.metric {
  font-family: var(--font-mono);
  font-size: var(--text-xs);
}

.metric-label {
  color: var(--gray-400);
  margin-right: 2px;
}

.time-info {
  display: flex;
  gap: var(--space-3);
  font-size: var(--text-xs);
  color: var(--gray-500);
  margin-top: var(--space-1);
  font-variant-numeric: tabular-nums;
}

.role-badge {
  display: inline-block;
  padding: 1px var(--space-2);
  border-radius: var(--radius-full);
  font-size: var(--text-xs);
  font-weight: 500;
  white-space: nowrap;
}

/* 角色徽章按角色着色 */
.transfer-role-destination .role-badge {
  background: var(--role-destination-bg);
  color: var(--success-600);
}
.transfer-role-source-push .role-badge {
  background: var(--role-source-push-bg);
  color: var(--warning-600);
}
.transfer-role-source-pull .role-badge {
  background: var(--role-source-pull-bg);
  color: var(--role-source-pull);
}

.throttle-menu {
  position: absolute;
  top: 100%;
  left: 0;
  background: white;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  padding: var(--space-1);
  box-shadow: var(--shadow-md);
  z-index: 10;
  display: flex;
  flex-direction: column;
  min-width: 96px;
}

.throttle-menu button {
  padding: var(--space-2) var(--space-3);
  min-height: var(--control-h-sm);
  background: none;
  border: none;
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
  text-align: left;
  cursor: pointer;
}

.throttle-menu button:hover { background: var(--gray-100); }
</style>