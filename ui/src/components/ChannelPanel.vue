<template>
  <div class="panel-mask" @click.self="emit('close')">
    <div class="channel-panel" data-testid="device-channel-panel">
      <div class="panel-header">
        <div class="panel-title">
          {{ deviceName }} · 通道
          <span class="panel-sub">共 {{ rows.length }} 条地址记录</span>
        </div>
        <button class="panel-close" data-testid="channel-panel-close-btn" @click="emit('close')">✕</button>
      </div>

      <div v-if="rows.length === 0" class="panel-empty">
        暂无通道记录(设备未连接或探测未完成)
      </div>

      <div
        v-for="row in rows"
        :key="row.addr"
        class="channel-row"
        :class="{ 'is-current': row.current }"
        :data-testid="`channel-row-${row.addr}`"
      >
        <span class="row-dot" :class="row.via_relay ? 'relay' : 'direct'"></span>
        <span class="row-addr" :title="row.addr">{{ row.addr }}</span>
        <span class="row-path">{{ row.via_relay ? '中继' : '直连' }}</span>
        <span class="row-rtt">{{ row.rtt_ms != null ? row.rtt_ms + 'ms' : '—' }}</span>
        <span class="row-bps">{{ formatEstBps(row.est_bps) }}</span>
        <span class="row-loss" :class="{ 'loss-warn': row.loss_rate > 0 }">{{ (row.loss_rate * 100).toFixed(0) }}%</span>
        <span v-if="row.current" class="row-current">✓ 当前</span>
        <span v-else class="row-current row-idle"></span>
        <span class="row-age">{{ formatAge(row.age_secs) }}</span>
      </div>

      <div class="panel-footer">
        <button
          class="btn btn-reprobe"
          data-testid="channel-reprobe-btn"
          :disabled="probing || !hasSession"
          @click="handleReprobe"
        >
          {{ probing ? '探测中…' : '重新探测' }}
        </button>
        <span v-if="probeMsg" class="probe-msg" :class="{ 'probe-err': probeErr }">{{ probeMsg }}</span>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onUnmounted } from 'vue'
import { useDevicesStore } from '../stores/devices'
import { useToastStore } from '../stores/toast'
import { friendlyError } from '../api'
import type { ChannelDto } from '../types'

interface Props {
  fingerprint: string
  deviceName?: string
}

const props = withDefaults(defineProps<Props>(), {
  deviceName: ''
})

const emit = defineEmits<{
  (e: 'close'): void
}>()

const devicesStore = useDevicesStore()
const toastStore = useToastStore()

// 该设备的通道记录(当前通道置首,其余按地址稳定排序)
const rows = computed<ChannelDto[]>(() => {
  const recs = devicesStore.channels.filter(c => c.fingerprint === props.fingerprint)
  return [...recs].sort((a, b) => {
    if (a.current !== b.current) return a.current ? -1 : 1
    return a.addr.localeCompare(b.addr)
  })
})

// 有活会话(存在 current 记录)才允许手动快检——探测骑既有会话
const hasSession = computed(() => rows.value.some(r => r.current))

// 重新探测(手动单对端快检;完成后立即刷新通道表,探测数据晚几秒到则轮询补刷)
const probing = ref(false)
const probeMsg = ref('')
const probeErr = ref(false)
let refreshTimers: ReturnType<typeof setTimeout>[] = []

function clearTimers() {
  for (const t of refreshTimers) clearTimeout(t)
  refreshTimers = []
}

async function handleReprobe() {
  if (probing.value) return
  probing.value = true
  probeMsg.value = ''
  probeErr.value = false
  clearTimers()
  try {
    await devicesStore.probeNowPeer(props.fingerprint)
    probeMsg.value = '已完成'
    // 快检命令返回时内存表已更新,立即刷一次;升级全量时数据晚几秒到,3s 后补刷
    void devicesStore.refreshChannels()
    refreshTimers.push(setTimeout(() => void devicesStore.refreshChannels(), 3000))
  } catch (e) {
    probeErr.value = true
    probeMsg.value = friendlyError(e)
    toastStore.push('error', '重新探测失败: ' + friendlyError(e))
  } finally {
    probing.value = false
  }
}

/** 估速 humanize(bps → Mbps/KBps;未测得 —) */
function formatEstBps(bps: number | null): string {
  if (bps == null || bps === 0) return '—'
  if (bps >= 1_000_000) return `${(bps / 1_000_000).toFixed(1)} Mbps`
  if (bps >= 1_000) return `${Math.round(bps / 1_000)} Kbps`
  return `${bps} bps`
}

/** 最近探测时间 age(秒 → 刚刚/n秒前/n分前) */
function formatAge(ageSecs: number): string {
  if (ageSecs < 5) return '刚刚'
  if (ageSecs < 60) return `${ageSecs}s前`
  return `${Math.round(ageSecs / 60)}分前`
}

onUnmounted(clearTimers)
</script>

<style scoped>
.panel-mask {
  position: fixed;
  inset: 0;
  background: rgba(0, 0, 0, 0.35);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 100;
}

.channel-panel {
  background: white;
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-md);
  min-width: 640px;
  max-width: 92vw;
  max-height: 80vh;
  display: flex;
  flex-direction: column;
  padding: var(--space-4);
}

.panel-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: var(--space-3);
}

.panel-title {
  font-size: var(--text-md);
  font-weight: 600;
  color: var(--gray-800);
}

.panel-sub {
  font-size: var(--text-xs);
  font-weight: 400;
  color: var(--gray-400);
  margin-left: var(--space-2);
}

.panel-close {
  border: none;
  background: none;
  font-size: 14px;
  color: var(--gray-500);
  cursor: pointer;
  padding: 4px 8px;
  border-radius: var(--radius-sm);
}

.panel-close:hover {
  background: var(--gray-100);
  color: var(--gray-700);
}

.panel-empty {
  color: var(--gray-500);
  font-size: var(--text-sm);
  padding: var(--space-6) 0;
  text-align: center;
}

.channel-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-3);
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
  font-family: var(--font-mono);
  color: var(--gray-700);
  white-space: nowrap;
}

.channel-row:nth-child(odd) {
  background: var(--gray-50);
}

.channel-row.is-current {
  background: var(--primary-50);
}

.row-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.row-dot.direct {
  background: var(--success-500);
}

.row-dot.relay {
  background: #f09a0a;
}

.row-addr {
  min-width: 200px;
  overflow: hidden;
  text-overflow: ellipsis;
}

.row-path {
  width: 34px;
  flex-shrink: 0;
  font-family: var(--font-sans);
  color: var(--gray-500);
}

.row-rtt {
  width: 64px;
  flex-shrink: 0;
  text-align: right;
}

.row-bps {
  width: 90px;
  flex-shrink: 0;
  text-align: right;
}

.row-loss {
  width: 44px;
  flex-shrink: 0;
  text-align: right;
  color: var(--gray-500);
}

.row-loss.loss-warn {
  color: var(--danger-600);
}

.row-current {
  width: 56px;
  flex-shrink: 0;
  color: var(--primary-600);
  font-family: var(--font-sans);
  font-weight: 500;
}

.row-age {
  width: 52px;
  flex-shrink: 0;
  text-align: right;
  color: var(--gray-400);
  font-family: var(--font-sans);
}

.panel-footer {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  margin-top: var(--space-3);
}

.btn-reprobe {
  padding: var(--space-2) var(--space-4);
  border: none;
  border-radius: var(--radius-sm);
  background: var(--primary-600);
  color: white;
  font-size: var(--text-base);
  cursor: pointer;
}

.btn-reprobe:hover:not(:disabled) {
  background: var(--primary-700);
}

.btn-reprobe:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.probe-msg {
  font-size: var(--text-sm);
  color: var(--success-600);
}

.probe-msg.probe-err {
  color: var(--danger-600);
}
</style>
