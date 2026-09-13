<template>
  <div
    class="device-card"
    :class="{
      'is-online': device.online,
      'is-drag-over': isDragOver,
      'is-trusted': isTrusted
    }"
    :data-fingerprint="device.fingerprint"
    :data-testid="`device-card-${device.fingerprint}`"
  >
    <div class="device-header">
      <div class="device-info">
        <div class="device-name">
          {{ device.name }}
          <span v-if="device.via_relay" class="badge badge-remote" title="经中继连接">远程</span>
          <!-- M3c T3:强制走中继角标(卡片级可见,与菜单勾选态同源) -->
          <span
            v-if="RELAY_ENABLED && device.force_relay"
            class="badge badge-force-relay"
            data-testid="device-force-relay-badge"
            title="强制走中继已开启:连接绕过自动选路,固定经中继"
          >强制中继</span>
        </div>
        <div class="device-fingerprint">{{ formatFingerprint(device.fingerprint) }}</div>
      </div>
      <div class="device-status">
        <div v-if="isReconnecting" class="status-badge reconnecting">重连中…</div>
        <div v-else-if="device.connected" class="status-badge connected">已连接</div>
        <div v-else-if="device.online" class="status-badge online">在线</div>
        <div v-else class="status-badge offline">离线</div>
        <div v-if="isTrusted" class="paired-badge">已配对</div>
        <div v-else class="paired-badge unpaired">待配对</div>
      </div>
    </div>

    <div
      class="device-address device-address-clickable"
      data-testid="device-channel-label"
      title="点击查看通道详情"
      @click="showChannelPanel = true"
    >
      <span class="channel-dot" :class="channelLabel ? channelLabel.kind : 'unknown'"></span>
      <span>{{ channelLabel ? channelLabel.text : '未知' }}</span>
    </div>

    <!-- M3c T2:通道面板(每地址明细 + 重新探测) -->
    <ChannelPanel
      v-if="showChannelPanel"
      :fingerprint="device.fingerprint"
      :device-name="device.name"
      @close="showChannelPanel = false"
    />

    <div class="device-actions">
      <!-- P2:配对冷却期(错码 3 次触发,core 拒连)→ 连接按钮禁用+倒计时 -->
      <div v-if="cooldownSecs > 0" class="cooldown-badge" data-testid="device-cooldown-badge">
        配对冷却 {{ cooldownSecs }}s
      </div>
      <button
        v-if="!isTrusted"
        class="btn btn-primary"
        data-testid="device-connect-btn"
        @click="handleConnect"
        :disabled="!device.online || isConnecting || cooldownSecs > 0"
        :title="cooldownSecs > 0 ? `配对码连续错误触发冷却，剩余 ${cooldownSecs} 秒` : undefined"
      >
        {{ isConnecting ? '连接中...' : (cooldownSecs > 0 ? `冷却 ${cooldownSecs}s` : '连接') }}
      </button>

      <template v-else>
        <button
          class="btn btn-primary"
          data-testid="device-browse-btn"
          @click="handleBrowse"
          :disabled="!device.online || isConnecting"
        >
          {{ isConnecting ? '连接中...' : '浏览' }}
        </button>
        <!-- M3c T4:独立「推送」按钮(已配对且在线常驻,直开推送向导步 2;
             与 ⋮ 菜单推送项同一 request-push emit,Devices.vue 统一接线) -->
        <button
          v-if="device.online"
          class="btn device-push-btn"
          data-testid="device-push-btn"
          title="向该设备推送文件"
          @click="emit('request-push', device.fingerprint)"
        >
          推送
        </button>
        <span v-if="hasActivePush" class="push-spinner" title="推送进行中">⏳</span>

        <div class="dropdown" ref="dropdownRef">
          <button
            class="btn btn-dropdown"
            data-testid="device-menu-btn"
            @click="toggleDropdown"
          >
            ⋮
          </button>

          <div v-if="showDropdown" class="dropdown-menu" :class="{ 'drop-up': dropUp }">
            <div class="dropdown-section">
              <div class="dropdown-section-title">权限设置</div>

              <label class="dropdown-item">
                <input
                  type="checkbox"
                  data-testid="device-perm-browse-toggle"
                  :checked="permissions?.browse"
                  @change="handlePermissionChange('browse', ($event.target as HTMLInputElement).checked)"
                />
                <span>允许浏览</span>
              </label>

              <label class="dropdown-item">
                <input
                  type="checkbox"
                  data-testid="device-perm-download-toggle"
                  :checked="permissions?.download"
                  @change="handlePermissionChange('download', ($event.target as HTMLInputElement).checked)"
                />
                <span>允许下载</span>
              </label>

              <div class="dropdown-item">
                <span>文件推送</span>
                <select
                  :value="permissions?.push"
                  data-testid="device-perm-push-select"
                  @change="handlePushPermissionChange(($event.target as HTMLSelectElement).value)"
                >
                  <option value="ask">每次询问</option>
                  <option value="auto">自动接受</option>
                  <option value="deny">拒绝</option>
                </select>
              </div>
            </div>

            <div class="dropdown-divider"></div>

            <!-- M3c T3:强制走中继勾选项(per 设备持久化;connect 决策跳过评分)。发布态隐藏 -->
            <label v-if="RELAY_ENABLED" class="dropdown-item" data-testid="device-force-relay-toggle">
              <input
                type="checkbox"
                :checked="device.force_relay"
                @change="handleForceRelayChange"
              />
              <span>强制走中继</span>
            </label>

            <div class="dropdown-divider"></div>
            <button class="dropdown-item" data-testid="btn-push-menu"
              :disabled="!device.online" @click="emit('request-push', device.fingerprint)">
              推送文件…
            </button>

            <div class="dropdown-divider"></div>

            <button
              class="dropdown-item dropdown-item-danger"
              data-testid="device-remove-trust-btn"
              @click="handleRemoveTrust"
            >
              {{ confirmRemove ? '确认移除' : '移除信任' }}
            </button>
          </div>
        </div>
      </template>
    </div>

    <div v-if="isDragOver" class="drop-hint">
      拖放文件到此设备进行传输
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onUnmounted, nextTick } from 'vue'
import { useRouter } from 'vue-router'
import { useDevicesStore } from '../stores/devices'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { useTransfersStore } from '../stores/transfers'
import { formatConnectError } from '../api'
import { RELAY_ENABLED } from '../featureFlags'
import ChannelPanel from './ChannelPanel.vue'
import type { DeviceDto, PushPolicy } from '../types'

interface Props {
  device: DeviceDto
  isDragOver?: boolean
}

const props = withDefaults(defineProps<Props>(), {
  isDragOver: false
})

const emit = defineEmits<{
  (e: 'request-push', fingerprint: string): void
}>()

const router = useRouter()
const devicesStore = useDevicesStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()
const transfersStore = useTransfersStore()

// 下拉菜单状态
const showDropdown = ref(false)
const dropdownRef = ref<HTMLElement | null>(null)
// 贴近视口底部时向上弹(权限菜单总高约 320px,避免溢出屏幕)
const dropUp = ref(false)

// 移除确认状态
const confirmRemove = ref(false)

// M3c T2:通道面板开关(点击通道标签弹出)
const showChannelPanel = ref(false)

// 连接状态
const isConnecting = ref(false)

// P2:配对冷却倒计时(错码 3 次触发;connect 报 pairing_cooldown:{secs} 时
// 由 devices store 记入)。>0 时连接按钮禁用并显示剩余秒数
const cooldownSecs = computed(() => devicesStore.cooldownRemainingSecs(props.device.fingerprint))

// 中继自愈中(卡片状态徽章显示"重连中…")
const isReconnecting = computed(() => devicesStore.reconnecting.has(props.device.fingerprint))

// M3c T1:通道标签(地址行三态)。数据源 = 通道表(list_channels):
// 优先当前通道记录,其次按发现地址匹配;无记录 = 未知(离线常驻卡/未建会话)。
const channelLabel = computed<{ kind: 'direct' | 'relay'; text: string } | null>(() => {
  const recs = devicesStore.channels.filter(c => c.fingerprint === props.device.fingerprint)
  const rec = recs.find(r => r.current) ?? recs.find(r => r.addr === props.device.addr)
  if (!rec) return null
  const rtt = rec.rtt_ms != null ? ` · ${rec.rtt_ms}ms` : ''
  return rec.via_relay
    ? { kind: 'relay', text: `经中继${rtt}` }
    : { kind: 'direct', text: `直连${rtt}` }
})

// 权限信息
const permissions = computed(() => {
  return settingsStore.trustedPeers.find(
    peer => peer.fingerprint === props.device.fingerprint
  )
})

// 是否已信任
const isTrusted = computed(() => !!permissions.value)

// 推送进行中状态（角标）
const hasActivePush = computed(() => {
  return transfersStore.transfers.some(t =>
    t.local_role === 'source-push' &&
    t.peer === props.device.fingerprint &&
    ['pending', 'active'].includes(t.state)
  )
})

/**
 * 格式化指纹显示
 */
function formatFingerprint(fp: string): string {
  return `${fp.slice(0, 4)}...${fp.slice(-4)}`
}

/**
 * 连接或浏览设备
 */
async function handleConnectOrBrowse(browse: boolean = false) {
  if (!props.device.online || isConnecting.value) return

  isConnecting.value = true

  try {
    await devicesStore.connectDevice(props.device.fingerprint)
    // 连接成功后跳转到浏览页面
    devicesStore.selected_fp = props.device.fingerprint
    if (browse) {
      await router.push('/browse')
    }
  } catch (error) {
    toastStore.push('error', formatConnectError(error))
  } finally {
    isConnecting.value = false
  }
}

/**
 * 连接设备
 */
function handleConnect() {
  handleConnectOrBrowse(false)
}

/**
 * 浏览设备
 */
function handleBrowse() {
  handleConnectOrBrowse(true)
}

/**
 * 切换下拉菜单
 */
function toggleDropdown() {
  showDropdown.value = !showDropdown.value
  confirmRemove.value = false

  // 点击外部关闭下拉菜单
  if (showDropdown.value) {
    nextTick(() => {
      // 视口底部空间不足则向上弹(菜单不再压缩出滚动条)
      const btn = dropdownRef.value?.querySelector('.btn-dropdown')
      if (btn) {
        const rect = (btn as HTMLElement).getBoundingClientRect()
        dropUp.value = rect.bottom + 340 > window.innerHeight
      }
      document.addEventListener('click', handleOutsideClick)
    })
  } else {
    document.removeEventListener('click', handleOutsideClick)
  }
}

/**
 * 处理外部点击
 */
function handleOutsideClick(event: MouseEvent) {
  if (dropdownRef.value && !dropdownRef.value.contains(event.target as Node)) {
    showDropdown.value = false
    confirmRemove.value = false
    document.removeEventListener('click', handleOutsideClick)
  }
}

/**
 * 处理权限变更
 */
async function handlePermissionChange(type: 'browse' | 'download', value: boolean) {
  try {
    const pushPolicy: PushPolicy = (permissions.value?.push ?? 'ask') as PushPolicy
    await settingsStore.setPerms(
      props.device.fingerprint,
      type === 'browse' ? value : permissions.value?.browse ?? false,
      type === 'download' ? value : permissions.value?.download ?? false,
      pushPolicy
    )
  } catch (error) {
    toastStore.push('error', '更新权限失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 处理推送权限变更
 */
async function handlePushPermissionChange(value: string) {
  try {
    let pushPolicy: PushPolicy = 'ask'
    if (value === 'auto') pushPolicy = 'auto'
    else if (value === 'deny') pushPolicy = 'deny'
    else pushPolicy = 'ask'

    await settingsStore.setPerms(
      props.device.fingerprint,
      permissions.value?.browse ?? false,
      permissions.value?.download ?? false,
      pushPolicy
    )
  } catch (error) {
    toastStore.push('error', '更新推送权限失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * M3c T3:强制走中继开关切换(持久化;失败回滚勾选态)
 */
async function handleForceRelayChange(event: Event) {
  const input = event.target as HTMLInputElement
  const on = input.checked
  try {
    await devicesStore.setForceRelay(props.device.fingerprint, on)
    toastStore.push('info', on
      ? '已开启强制走中继:下次连接固定经中继(无中继时报错)'
      : '已关闭强制走中继:恢复自动选路')
  } catch (error) {
    input.checked = !on // 失败回滚,勾选态与后端保持一致
    toastStore.push('error', '设置强制走中继失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 移除信任
 */
async function handleRemoveTrust() {
  if (!confirmRemove.value) {
    confirmRemove.value = true
    // 3秒后重置确认状态
    setTimeout(() => {
      if (confirmRemove.value) {
        confirmRemove.value = false
      }
    }, 3000)
    return
  }

  try {
    await settingsStore.removeTrusted(props.device.fingerprint)
    showDropdown.value = false
    confirmRemove.value = false
  } catch (error) {
    toastStore.push('error', '移除信任设备失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 清理
 */
onUnmounted(() => {
  document.removeEventListener('click', handleOutsideClick)
})
</script>

<style scoped>
.device-card {
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-md);
  padding: var(--space-4);
  background: white;
  box-shadow: var(--shadow-sm);
  transition: border-color var(--dur-fast) ease, box-shadow var(--dur-fast) ease;
  position: relative;
}

.device-card.is-online {
  border-color: var(--success-500);
}

.device-card.is-drag-over {
  border-color: var(--primary-500);
  background: var(--primary-50);
  box-shadow: 0 0 0 2px var(--primary-500);
}

.device-card.is-trusted {
  border-left: 4px solid var(--success-500);
}

.device-header {
  display: flex;
  justify-content: space-between;
  align-items: start;
  margin-bottom: var(--space-3);
}

.device-info {
  flex: 1;
  min-width: 0;
}

.device-name {
  font-size: var(--text-md);
  font-weight: 600;
  color: var(--gray-800);
  margin-bottom: var(--space-1);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.device-fingerprint {
  font-size: var(--text-xs);
  color: var(--gray-400);
  font-family: var(--font-mono);
}

.device-status {
  display: flex;
  flex-direction: column;
  gap: var(--space-1);
  align-items: flex-end;
  flex-shrink: 0;
  margin-left: var(--space-2);
}

.status-badge {
  padding: 2px var(--space-2);
  border-radius: var(--radius-full);
  font-size: var(--text-xs);
  font-weight: 500;
}

.status-badge.online {
  background: var(--success-50);
  color: var(--success-600);
}

/* QUIC 会话已建立（心跳确认）：实底色与"在线"区分 */
.status-badge.connected {
  background: var(--success-500);
  color: white;
}

/* 中继自愈中：显示重连状态 */
.status-badge.reconnecting {
  background: rgba(240, 154, 10, 0.15);
  color: #f09a0a;
}

.status-badge.offline {
  background: var(--gray-100);
  color: var(--gray-500);
}

.paired-badge {
  padding: 2px var(--space-2);
  border-radius: var(--radius-full);
  font-size: var(--text-xs);
  font-weight: 500;
}

.paired-badge.unpaired {
  background: var(--warning-50);
  color: var(--warning-600);
}

.paired-badge:not(.unpaired) {
  background: var(--primary-100);
  color: var(--primary-700);
}

.device-address {
  font-size: var(--text-xs);
  color: var(--gray-500);
  margin-bottom: var(--space-4);
  font-family: var(--font-mono);
  display: flex;
  align-items: center;
  gap: 6px;
}

/* M3c T1 通道标签三态点:直连=绿,经中继=琥珀,未知=灰 */
.channel-dot {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  flex-shrink: 0;
}

.channel-dot.direct {
  background: var(--success-500);
}

.channel-dot.relay {
  background: #f09a0a;
}

.channel-dot.unknown {
  background: var(--gray-300);
}

.device-address .channel-dot + span {
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* M3c T2:通道标签可点(弹通道面板) */
.device-address-clickable {
  cursor: pointer;
  border-radius: var(--radius-sm);
  padding: 2px 4px;
  margin-left: -4px;
}

.device-address-clickable:hover {
  background: var(--gray-100);
}

.device-actions {
  display: flex;
  gap: var(--space-2);
  align-items: center;
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
  flex: 1;
}

.btn-primary:hover:not(:disabled) {
  background: var(--primary-700);
}

/* M3c T4:独立推送按钮(次级样式,与「浏览」并排各占一半) */
.device-push-btn {
  flex: 1;
  background: var(--gray-100);
  color: var(--gray-700);
}

.device-push-btn:hover {
  background: var(--gray-200);
}

.btn-dropdown {
  background: var(--gray-100);
  color: var(--gray-700);
  width: var(--control-h-md);
  min-height: var(--control-h-md);
  padding: 0;
  font-size: 18px;
  /* ⋮ 字形在 WebView2 度量下宽于 36px 内容盒,固定盒会溢出:
     flex 居中 + 缩放 containment,字形再宽也裁在按钮内 */
  display: inline-flex;
  align-items: center;
  justify-content: center;
  line-height: 1;
  overflow: hidden;
  flex-shrink: 0;
}

.btn-dropdown:hover {
  background: var(--gray-200);
}

.dropdown {
  position: relative;
}

.dropdown-menu {
  position: absolute;
  top: 100%;
  right: 0;
  margin-top: var(--space-1);
  background: white;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-md);
  box-shadow: var(--shadow-md);
  min-width: 280px;
  z-index: 10;
  /* 贴近视口底部时向上弹(打开时 JS 计算方向,见 toggleDropdown) */
}

/* 向上弹变体:菜单底部对齐按钮顶边 */
.dropdown-menu.drop-up {
  top: auto;
  bottom: 100%;
  margin-top: 0;
  margin-bottom: var(--space-1);
}

.dropdown-section {
  padding: var(--space-2) 0;
}

.dropdown-section-title {
  padding: var(--space-2) var(--space-4);
  font-size: var(--text-xs);
  font-weight: 600;
  color: var(--gray-500);
}

.dropdown-item {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-2);
  padding: var(--space-2) var(--space-4);
  cursor: pointer;
  transition: background-color var(--dur-fast) ease;
  border: none;
  background: none;
  width: 100%;
  /* 项目无全局 border-box 重置:content-box 下 width:100% + 32px 横向 padding
     会让行比菜单宽 32px,文字/hover 背景溢出菜单边界 */
  box-sizing: border-box;
  text-align: left;
  font-size: var(--text-base);
  min-height: var(--control-h-sm);
  color: var(--gray-800);
  /* 不换行基础约束:select/checkbox 一行放得下,文字过长省略 */
  flex-wrap: nowrap;
}

.dropdown-item:hover {
  background: var(--gray-50);
}

.dropdown-item label {
  display: flex;
  align-items: center;
  gap: var(--space-3);
  flex: 1;
  min-width: 0;
  cursor: pointer;
}

.dropdown-item label > span:first-child {
  flex: 1;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.dropdown-item input[type="checkbox"] {
  flex-shrink: 0;
  accent-color: var(--primary-600);
}

.dropdown-item select {
  padding: var(--space-1) var(--space-2);
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  background: white;
  flex-shrink: 0;
  max-width: 130px;
}

.dropdown-divider {
  height: 1px;
  background: var(--gray-200);
  margin: var(--space-2) 0;
}

/* "文件推送"行:div.dropdown-item > span + select(无 label 包裹) */
.dropdown-item > span:first-child {
  flex: 1;
  min-width: 0;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.dropdown-item-danger {
  color: var(--danger-600);
}

.dropdown-item-danger:hover {
  background: var(--danger-50);
}

.drop-hint {
  position: absolute;
  top: 50%;
  left: 50%;
  transform: translate(-50%, -50%);
  background: rgba(37, 99, 235, 0.92);
  color: white;
  padding: var(--space-3) var(--space-6);
  border-radius: var(--radius-md);
  font-size: var(--text-base);
  font-weight: 500;
  pointer-events: none;
  white-space: nowrap;
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

/* M3c T3:强制走中继角标(琥珀=经中继语义色) */
.badge-force-relay {
  background: #f09a0a;
  color: #fff;
}

.push-spinner {
  margin-left: var(--space-2);
  font-size: 16px;
  animation: pulse 2s ease-in-out infinite;
}

/* P2:配对冷却倒计时徽章(错码 3 次后 core 拒连期) */
.cooldown-badge {
  width: 100%;
  text-align: center;
  font-size: var(--text-xs);
  color: var(--warning-600, #b45309);
  background: rgba(240, 154, 10, 0.12);
  border: 1px solid rgba(240, 154, 10, 0.35);
  border-radius: var(--radius-sm);
  padding: 4px 8px;
  margin-bottom: var(--space-2);
}

@keyframes pulse {
  0%, 100% { opacity: 1; }
  50% { opacity: 0.5; }
}
</style>
