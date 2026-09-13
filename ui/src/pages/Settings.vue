<template>
  <div class="settings-page">
    <h1>设置</h1>

    <div class="settings-grid">
      <!-- 共享区卡片 -->
      <div class="setting-card">
        <div class="card-header">
          <h2>共享区</h2>
          <button class="btn btn-secondary" data-testid="settings-share-add-btn" @click="handleAddShare">添加</button>
        </div>
        <div class="card-body">
          <div v-if="!config || config.shares.length === 0" class="empty-state">
            暂无共享区
          </div>
          <div v-else class="shares-list">
            <div v-for="share in config?.shares" :key="share.id" class="share-item" :data-testid="`settings-share-item-${share.id}`">
              <div class="share-info">
                <div class="share-alias">{{ share.alias }}</div>
                <div class="share-path">{{ share.path }}</div>
              </div>
              <button class="btn btn-danger" data-testid="settings-share-remove-btn" @click="handleRemoveShare(share.id, share.alias)">删除</button>
            </div>
          </div>
        </div>
      </div>

      <!-- 下载目录卡片 -->
      <div class="setting-card">
        <div class="card-header">
          <h2>下载目录</h2>
        </div>
        <div class="card-body">
          <div class="download-dir-section">
            <div class="current-dir">{{ config?.download_dir || '未设置' }}</div>
            <button class="btn btn-secondary" data-testid="settings-download-dir-change-btn" @click="handleChangeDownloadDir">修改</button>
          </div>
        </div>
      </div>

      <!-- 本机身份卡片 -->
      <div class="setting-card">
        <div class="card-header">
          <h2>本机身份</h2>
        </div>
        <div class="card-body">
          <div v-if="deviceFingerprint" class="identity-section">
            <div class="identity-info">
              <div class="identity-item">
                <span class="label">设备名:</span>
                <input
                  v-model="editableDeviceName"
                  class="device-name-input"
                  data-testid="settings-device-name-input"
                  @blur="handleDeviceNameChange"
                  @keyup.enter="($event.target as HTMLInputElement)?.blur()"
                />
              </div>
              <div class="identity-item">
                <span class="label">指纹:</span>
                <span class="value fingerprint">{{ formatFingerprint(deviceFingerprint.fingerprint_hex) }}</span>
              </div>
              <div class="identity-item">
                <span class="label">短码:</span>
                <span class="value short-code">{{ deviceFingerprint.short_code }}</span>
              </div>
            </div>
          </div>
          <div v-else class="loading-state">加载中...</div>
        </div>
      </div>

      <!-- 信任设备列表卡片 -->
      <div class="setting-card">
        <div class="card-header">
          <h2>信任设备</h2>
        </div>
        <div class="card-body">
          <div v-if="trustedPeers.length === 0" class="empty-state">
            暂无信任设备
          </div>
          <div v-else class="trusted-list">
            <div v-for="peer in trustedPeers" :key="peer.fingerprint" class="trusted-item" :data-testid="`settings-trusted-item-${peer.fingerprint}`">
              <div class="peer-info">
                <div class="peer-name">{{ displayName(peer) }}</div>
                <div class="peer-alias-row">
                  <span class="alias-label">备注:</span>
                  <input
                    :value="peer.alias"
                    class="alias-input"
                    data-testid="settings-peer-alias-input"
                    placeholder="给这台设备起个名(可选,只在本机显示)"
                    @focus="editingAliasFp = peer.fingerprint"
                    @blur="handleAliasChange(peer, ($event.target as HTMLInputElement).value)"
                    @keyup.enter="($event.target as HTMLInputElement)?.blur()"
                  />
                </div>
                <div class="peer-fingerprint">{{ formatFingerprint(peer.fingerprint) }}</div>
                <div class="peer-time">配对时间: {{ formatTime(peer.paired_at) }}</div>
              </div>
              <div class="peer-permissions">
                <div class="perm-item">
                  <span>浏览:</span>
                  <span :class="peer.browse ? 'perm-enabled' : 'perm-disabled'">
                    {{ peer.browse ? '允许' : '拒绝' }}
                  </span>
                </div>
                <div class="perm-item">
                  <span>下载:</span>
                  <span :class="peer.download ? 'perm-enabled' : 'perm-disabled'">
                    {{ peer.download ? '允许' : '拒绝' }}
                  </span>
                </div>
                <div class="perm-item">
                  <span>推送:</span>
                  <span class="perm-push">{{ formatPushPolicy(peer.push) }}</span>
                </div>
              </div>
              <button class="btn btn-danger" data-testid="settings-trusted-remove-btn" @click="handleRemoveTrusted(peer.fingerprint, peer.name)">移除</button>
            </div>
          </div>
        </div>
      </div>

      <!-- 防火墙卡片（含网络体检） -->
      <div class="setting-card">
        <div class="card-header">
          <h2>防火墙 / 网络体检</h2>
        </div>
        <div class="card-body">
          <div class="firewall-section">
            <div v-if="netStatus" class="fw-status">
              <div class="fw-row">
                <span class="fw-label">放行规则 (UDP 47600-47601):</span>
                <span :class="netStatus.rule_enabled ? 'fw-ok' : 'fw-bad'">
                  {{ netStatus.rule_enabled
                    ? '✓ 已生效'
                    : (netStatus.rule_exists ? '已添加但未启用' : '✗ 未添加——对方将发现不了你') }}
                </span>
              </div>
              <div class="fw-row">
                <span class="fw-label">本机 IP:</span>
                <span class="fw-mono">{{ netStatus.local_ip || '未知' }}</span>
              </div>
              <div class="fw-row">
                <span class="fw-label">防火墙 (域/专用/公用):</span>
                <span>{{ fwProfilesText }}</span>
              </div>
            </div>

            <div class="firewall-info">
              <p>首次运行需要添加防火墙规则以允许局域网发现。</p>
              <p>你也可以通过以下命令手动添加：</p>
              <code class="firewall-command">
                netsh advfirewall firewall add rule name="LocalTrans" dir=in action=allow protocol=UDP localport=47600-47601
              </code>
            </div>
            <div class="fw-buttons">
              <button class="btn btn-primary" data-testid="settings-firewall-add-btn" @click="handleAddFirewallRule" :disabled="isAddingFirewallRule">
                {{ isAddingFirewallRule ? '添加中...' : '添加防火墙规则' }}
              </button>
              <button class="btn btn-secondary" data-testid="settings-firewall-logs-btn" @click="handleOpenLogs" title="排障时把当天日志发给开发者">
                打开日志
              </button>
            </div>
          </div>
        </div>
      </div>

      <!-- 中继卡片(紧随防火墙之后,落在其右侧格位)。发布态 RELAY_ENABLED=false 整体隐藏,后端能力保留 -->
      <div v-if="RELAY_ENABLED" class="setting-card">
        <div class="card-header">
          <h2>中继(跨公网)</h2>
          <label class="relay-toggle">
            <input type="checkbox" data-testid="settings-relay-enabled-toggle" v-model="relayEnabled" @change="handleRelaySave" />
            <span>{{ relayEnabled ? '已启用' : '已停用' }}</span>
          </label>
        </div>
        <div class="card-body">
          <div class="relay-form">
            <div class="identity-item">
              <span class="label">服务器:</span>
              <input v-model="relayServer" data-testid="settings-relay-server-input" placeholder="例如 1.2.3.4:9443" class="device-name-input" />
            </div>
            <div class="identity-item">
              <span class="label">密钥(PSK):</span>
              <input v-model="relayPsk" type="password" data-testid="settings-relay-psk-input" placeholder="与服务端配置一致" class="device-name-input" />
            </div>
            <button class="btn btn-secondary" data-testid="settings-relay-save-btn" @click="handleRelaySave">保存并连接</button>
            <div v-if="relayState" class="relay-status" :class="relayState.connected ? 'ok' : (relayState.error ? 'bad' : '')">
              {{ relayStatusText }}
            </div>
          </div>
          <div class="relay-hint">两台设备都开启中继并互为信任后,即可跨公网互传。传输数据端到端加密,中继只见密文。</div>
        </div>
      </div>

      <!-- 连接安全卡片 -->
      <div class="setting-card">
        <div class="card-header">
          <h2>连接安全</h2>
        </div>
        <div class="card-body">
          <div class="security-form">
            <div class="identity-item">
              <span class="label">同意超时(秒):</span>
              <input
                v-model.number="consentTimeoutSecs"
                type="number"
                min="15"
                max="600"
                class="device-name-input"
                data-testid="settings-consent-timeout-input"
                @blur="handleConsentTimeoutChange"
              />
            </div>
            <div class="security-hint">配对时对方确认的超时时间，超时将自动拒绝。范围: 15-600 秒。</div>

            <div class="identity-item">
              <span class="label">推送确认超时(秒):</span>
              <input
                v-model.number="offerTimeoutSecs"
                type="number"
                min="15"
                max="600"
                class="device-name-input"
                data-testid="settings-offer-timeout-input"
                @blur="handleConsentTimeoutChange"
              />
            </div>
            <div class="security-hint">对方推送文件时,你有多少秒时间确认;超时自动拒绝(15-600)</div>

            <div class="identity-item">
              <span class="label">并发任务数:</span>
              <input
                v-model.number="maxActiveTransfers"
                type="number"
                min="1"
                max="8"
                class="device-name-input"
                data-testid="settings-max-active-input"
                @blur="handleMaxActiveChange"
              />
            </div>
            <div class="security-hint">同时进行的传输任务上限,超出排队等待(1-8,重启后生效)</div>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted } from 'vue'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { open } from '@tauri-apps/plugin-dialog'
import { systemApi, settingsApi, onRelayState } from '../api'
import { useConfirm } from '../composables/useConfirm'
import type { ConfigDto, DeviceFingerprint, NetworkStatus, RelayStatus } from '../types'
import { RELAY_ENABLED } from '../featureFlags'

const settingsStore = useSettingsStore()
const toastStore = useToastStore()

// WebView2 无原生 confirm/prompt,统一走应用内对话框
const { confirm, prompt } = useConfirm()

// 状态
const config = computed(() => settingsStore.config)
const trustedPeers = computed(() => settingsStore.trustedPeers)
const deviceFingerprint = ref<DeviceFingerprint | null>(null)
const editableDeviceName = ref('')
const isAddingFirewallRule = ref(false)
const netStatus = ref<NetworkStatus | null>(null)

// 中继状态
const relayEnabled = ref(false)
const relayServer = ref('')
const relayPsk = ref('')
const relayState = ref<RelayStatus | null>(null)
let relayStateUnlisten: (() => void) | null = null

/** 中继状态一行字:已连接 / 配置错误(带原因) / 连接中 / 未启用 */
const relayStatusText = computed(() => {
  const r = relayState.value
  if (!r) return ''
  if (r.connected) return `已连接 · ${r.devices} 台远程设备`
  if (r.error) return `配置错误: ${r.error}`
  return r.enabled ? '连接中…' : '未启用'
})

// 连接安全
const consentTimeoutSecs = ref(60)
const offerTimeoutSecs = ref(60)
const maxActiveTransfers = ref(3)

/** 防火墙三个配置文件开关的简短展示 */
const fwProfilesText = computed(() => {
  if (!netStatus.value) return ''
  const n = netStatus.value
  const fmt = (on: boolean) => on ? '开启' : '关闭'
  return `${fmt(n.fw_domain)} / ${fmt(n.fw_private)} / ${fmt(n.fw_public)}`
})

/** 拉取网络体检状态 */
async function loadNetStatus() {
  try {
    netStatus.value = await systemApi.getNetworkStatus()
  } catch (error) {
    console.error('获取网络状态失败:', error)
  }
}

/** 打开日志文件夹 */
async function handleOpenLogs() {
  try {
    await systemApi.openLogsDir()
  } catch (error) {
    toastStore.push('error', '打开日志失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 格式化指纹显示
 */
function formatFingerprint(fp: string): string {
  return `${fp.slice(0, 4)}...${fp.slice(-4)}`
}

/**
 * 格式化时间戳
 */
function formatTime(timestamp: number): string {
  const date = new Date(timestamp * 1000)
  return date.toLocaleString('zh-CN')
}

/**
 * 格式化推送策略
 */
function formatPushPolicy(policy: string): string {
  const map: Record<string, string> = {
    ask: '每次询问',
    auto: '自动接受',
    deny: '拒绝'
  }
  return map[policy] || policy
}

/**
 * 添加共享区
 */
async function handleAddShare() {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: '选择共享目录'
    })

    if (selected) {
      // 默认别名取目录最后一段——完整路径当别名会超长,列表行被撑破
      const dirName = selected.split(/[\\/]/).filter(Boolean).pop() ?? selected
      const alias = await prompt({
        title: '添加共享区',
        message: '给这个共享区起个别名(对方浏览时看到的名字)',
        inputInitial: dirName,
        okText: '添加',
      })
      if (alias !== null && alias.trim() !== '') {
        await settingsStore.addShare(alias.trim(), selected)
        toastStore.push('success', '共享区添加成功')
      }
    }
  } catch (error) {
    toastStore.push('error', '添加共享区失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 移除共享区
 */
async function handleRemoveShare(id: string, alias: string) {
  if (await confirm({
    title: '删除共享区',
    message: `确定要删除共享区 "${alias}" 吗?`,
    hint: '只是不再对外共享,磁盘上的文件夹和文件不会被删除。',
  })) {
    try {
      await settingsStore.removeShare(id)
      toastStore.push('success', '共享区删除成功')
    } catch (error) {
      toastStore.push('error', '删除共享区失败: ' + (error instanceof Error ? error.message : String(error)))
    }
  }
}

/**
 * 修改下载目录
 */
async function handleChangeDownloadDir() {
  try {
    const selected = await open({
      directory: true,
      multiple: false,
      title: '选择下载目录'
    })

    if (selected && config.value) {
      const newConfig: ConfigDto = {
        ...config.value,
        download_dir: selected
      }
      await settingsStore.saveSettings(newConfig)
      toastStore.push('success', '下载目录修改成功')
    }
  } catch (error) {
    toastStore.push('error', '修改下载目录失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 设备名修改
 */
async function handleDeviceNameChange() {
  if (!config.value || !editableDeviceName.value) return

  if (editableDeviceName.value.trim() === '') {
    editableDeviceName.value = config.value.device_name
    toastStore.push('warning', '设备名不能为空')
    return
  }

  if (editableDeviceName.value === config.value.device_name) return

  try {
    const newConfig: ConfigDto = {
      ...config.value,
      device_name: editableDeviceName.value.trim()
    }
    await settingsStore.saveSettings(newConfig)
    toastStore.push('success', '设备名修改成功，已即时生效')

    // 更新指纹信息中的名称
    if (deviceFingerprint.value) {
      deviceFingerprint.value = {
        ...deviceFingerprint.value,
        name: editableDeviceName.value.trim()
      }
    }
  } catch (error) {
    toastStore.push('error', '修改设备名失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 移除信任设备
 */
async function handleRemoveTrusted(fingerprint: string, name: string) {
  if (await confirm({
    title: '移除信任设备',
    message: `确定要移除信任设备 "${name}" 吗?`,
    hint: '移除后需要重新配对才能建立连接;对方也将立即失去访问权限。',
  })) {
    try {
      await settingsStore.removeTrusted(fingerprint)
      toastStore.push('success', '信任设备移除成功')
    } catch (error) {
      toastStore.push('error', '移除信任设备失败: ' + (error instanceof Error ? error.message : String(error)))
    }
  }
}

/**
 * 显示名:别名优先,未设置回退广播名
 */
function displayName(peer: { name: string; alias: string }): string {
  return peer.alias.trim() !== '' ? peer.alias : peer.name
}

/** 当前正在编辑别名的设备指纹(失焦时清) */
const editingAliasFp = ref<string | null>(null)

/**
 * 别名变更(失焦/回车触发;空串=清除别名回退广播名)
 */
async function handleAliasChange(peer: { fingerprint: string; alias: string }, value: string) {
  editingAliasFp.value = null
  const trimmed = value.trim()
  if (trimmed === peer.alias) return
  try {
    await settingsStore.setAlias(peer.fingerprint, trimmed)
    toastStore.push('success', trimmed === '' ? '已清除备注,恢复显示对方设备名' : '备注已保存')
  } catch (error) {
    toastStore.push('error', '保存备注失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 添加防火墙规则
 */
async function handleAddFirewallRule() {
  try {
    isAddingFirewallRule.value = true
    const result = await settingsStore.addFirewallRule()
    // result 含后端回读验证结论；规则生效则按成功提示，否则警告
    if (result.includes('已生效')) {
      toastStore.push('success', result)
    } else {
      toastStore.push('warning', result)
    }
    await loadNetStatus()
  } catch (error) {
    toastStore.push('error', '添加防火墙规则失败: ' + (error instanceof Error ? error.message : String(error)))
  } finally {
    isAddingFirewallRule.value = false
  }
}

/**
 * 加载中继状态
 */
async function loadRelayStatus() {
  if (!RELAY_ENABLED) return
  try {
    relayState.value = await settingsApi.relayStatus()
  } catch (error) {
    console.error('获取中继状态失败:', error)
  }
}

/**
 * 保存中继配置
 */
async function handleRelaySave() {
  try {
    await settingsApi.setRelayConfig(relayEnabled.value, relayServer.value, relayPsk.value)
    toastStore.push('success', '中继配置保存成功，连接中...')
    // 状态更新将由 relay-state 事件驱动
  } catch (error) {
    toastStore.push('error', '保存中继配置失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 保存同意超时配置
 */
async function handleConsentTimeoutChange() {
  if (!config.value) return

  // 验证范围
  if (consentTimeoutSecs.value < 15) {
    consentTimeoutSecs.value = 15
    toastStore.push('warning', '同意超时最少为 15 秒')
    return
  }
  if (consentTimeoutSecs.value > 600) {
    consentTimeoutSecs.value = 600
    toastStore.push('warning', '同意超时最多为 600 秒')
    return
  }

  try {
    const newConfig: ConfigDto = {
      ...config.value,
      consent_timeout_secs: consentTimeoutSecs.value,
      offer_timeout_secs: offerTimeoutSecs.value,
      max_active_transfers: Math.min(8, Math.max(1, maxActiveTransfers.value || 3))
    }
    await settingsStore.saveSettings(newConfig)
    toastStore.push('success', '同意超时设置已保存')
  } catch (error) {
    toastStore.push('error', '保存同意超时失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 保存并发任务数(独立处理器,避免复用同意超时的成功/失败文案)
 */
async function handleMaxActiveChange() {
  if (!config.value) return

  // 钳制 1-8(与同意超时输入一致的提示风格,钳制后仍保存)
  if (maxActiveTransfers.value < 1) {
    maxActiveTransfers.value = 1
    toastStore.push('warning', '并发任务数最少为 1')
  }
  if (maxActiveTransfers.value > 8) {
    maxActiveTransfers.value = 8
    toastStore.push('warning', '并发任务数最多为 8')
  }

  try {
    const newConfig: ConfigDto = {
      ...config.value,
      consent_timeout_secs: consentTimeoutSecs.value,
      offer_timeout_secs: offerTimeoutSecs.value,
      max_active_transfers: Math.min(8, Math.max(1, maxActiveTransfers.value || 3))
    }
    await settingsStore.saveSettings(newConfig)
    toastStore.push('success', '并发任务数已保存')
  } catch (error) {
    toastStore.push('error', '保存并发任务数失败: ' + (error instanceof Error ? error.message : String(error)))
  }
}

/**
 * 初始化
 */
onMounted(async () => {
  try {
    await settingsStore.initialize()

    // 网络体检 + 设备指纹
    loadNetStatus()

    const fp = await systemApi.getDeviceFingerprint()
    if (fp) {
      deviceFingerprint.value = fp
      editableDeviceName.value = fp.name
    }

    // 加载中继配置(发布态 RELAY_ENABLED=false:中继 UI 已隐藏,不回填不订阅)
    if (config.value) {
      consentTimeoutSecs.value = config.value.consent_timeout_secs || 60
      offerTimeoutSecs.value = config.value.offer_timeout_secs || 60
      maxActiveTransfers.value = config.value.max_active_transfers || 3
      if (RELAY_ENABLED) {
        relayEnabled.value = config.value.relay_enabled || false
        relayServer.value = config.value.relay_server || ''
        relayPsk.value = config.value.relay_psk || ''
        await loadRelayStatus()

        // 监听中继状态变化
        relayStateUnlisten = await onRelayState(async () => {
          await loadRelayStatus()
        })
      }
    }
  } catch (error) {
    console.error('初始化设置页失败:', error)
    toastStore.push('error', '初始化设置页失败: ' + (error instanceof Error ? error.message : String(error)))
  }
})

/**
 * 清理
 */
onUnmounted(() => {
  if (relayStateUnlisten) {
    relayStateUnlisten()
  }
})
</script>

<style scoped>
.settings-page {
  padding: 20px;
  max-width: 1200px;
  margin: 0 auto;
}

.settings-page h1 {
  margin-bottom: 24px;
  font-size: 28px;
  font-weight: 600;
  color: var(--gray-800);
}

.settings-grid {
  display: grid;
  grid-template-columns: repeat(auto-fill, minmax(350px, 1fr));
  gap: 20px;
}

/* 跨全宽卡片:中继表单一行铺开,避免 auto-fill 把它挤成半行孤位 */
.setting-card-wide {
  grid-column: 1 / -1;
}

.setting-card-wide .relay-form {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-3, 12px);
}

.setting-card-wide .relay-form .identity-item {
  flex: 1 1 280px;
  min-width: 0;
}

/* 半宽卡片内的中继表单:纵向排列,输入占满卡宽 */
.setting-card .relay-form {
  display: flex;
  flex-direction: column;
  gap: var(--space-3, 12px);
}

.setting-card {
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-md);
  background: white;
  overflow: hidden;
}

.setting-card.disabled-card {
  opacity: 0.6;
  pointer-events: none;
}

.card-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 16px;
  border-bottom: 1px solid var(--gray-200);
  background: var(--gray-50);
}

.card-header h2 {
  margin: 0;
  font-size: var(--text-lg);
  font-weight: 600;
  color: var(--gray-800);
}

.card-body {
  padding: 16px;
}

.empty-state {
  text-align: center;
  color: var(--gray-500);
  padding: 20px;
}

.loading-state {
  text-align: center;
  color: var(--gray-500);
  padding: 20px;
}

/* 共享区列表 */
.shares-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.share-item {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  background: var(--gray-50);
  /* 无全局 border-box 重置:防行宽超卡片 */
  box-sizing: border-box;
}

.share-info {
  flex: 1;
  /* flex 子项默认不收缩,超长别名/路径会把删除按钮挤出卡片 */
  min-width: 0;
}

.share-alias {
  font-weight: 600;
  color: var(--gray-800);
  margin-bottom: 4px;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.share-path {
  font-size: var(--text-xs);
  color: var(--gray-500);
  font-family: var(--font-mono);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

/* 下载目录 */
.download-dir-section {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.current-dir {
  flex: 1;
  padding: 8px 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  background: var(--gray-50);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  word-break: break-all;
}

/* 本机身份 */
.identity-section {
  display: flex;
  flex-direction: column;
  gap: 16px;
}

.identity-info {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.identity-item {
  display: flex;
  align-items: center;
  gap: 8px;
}

.identity-item .label {
  font-weight: 600;
  color: var(--gray-700);
  min-width: 60px;
}

.identity-item .value {
  color: var(--gray-500);
  font-family: var(--font-mono);
}

.identity-item .value.fingerprint,
.identity-item .value.short-code {
  color: var(--primary-500);
}

.device-name-input {
  flex: 1;
  padding: 8px 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
}

.device-name-input:focus {
  outline: none;
  border-color: var(--primary-500);
  box-shadow: 0 0 0 3px rgba(59, 130, 246, 0.1);
}

/* 信任设备列表 */
.trusted-list {
  display: flex;
  flex-direction: column;
  gap: 12px;
  max-height: 400px;
  overflow-y: auto;
}

.trusted-item {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding: 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  background: var(--gray-50);
}

.peer-info {
  flex: 1;
}

.peer-name {
  font-weight: 600;
  color: var(--gray-800);
  margin-bottom: 4px;
}

.peer-fingerprint {
  font-size: var(--text-xs);
  color: var(--gray-500);
  font-family: var(--font-mono);
  margin-bottom: 4px;
}

.peer-time {
  font-size: var(--text-xs);
  color: var(--gray-400);
}

.peer-permissions {
  display: flex;
  gap: 16px;
  font-size: var(--text-xs);
}

.perm-item {
  display: flex;
  align-items: center;
  gap: 4px;
}

.perm-enabled {
  color: var(--success-600);
  font-weight: 500;
}

.perm-disabled {
  color: var(--danger-600);
  font-weight: 500;
}

.perm-push {
  color: var(--primary-500);
  font-weight: 500;
}

/* 信任设备别名(备注)行 */
.peer-alias-row {
  display: flex;
  align-items: center;
  gap: var(--space-2);
  margin-top: var(--space-1);
}

.alias-label {
  font-size: var(--text-xs);
  color: var(--gray-500);
  flex-shrink: 0;
}

.alias-input {
  flex: 1;
  min-width: 0;
  padding: 4px var(--space-2);
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
  color: var(--gray-700);
  transition: border-color var(--dur-fast) ease, box-shadow var(--dur-fast) ease;
}

.alias-input:focus {
  outline: none;
  border-color: var(--primary-500);
  box-shadow: 0 0 0 3px var(--primary-100);
}

/* 防火墙 */
.firewall-section {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* 网络体检状态 */
.fw-status {
  display: flex;
  flex-direction: column;
  gap: 6px;
  padding: 10px 12px;
  border: 1px solid var(--gray-200);
  border-radius: var(--radius-sm);
  background: var(--gray-50);
  font-size: var(--text-sm);
}

.fw-row {
  display: flex;
  gap: 8px;
  align-items: baseline;
}

.fw-label {
  color: var(--gray-500);
  white-space: nowrap;
}

.fw-ok {
  color: var(--success-600);
  font-weight: 500;
}

.fw-bad {
  color: var(--danger-600);
  font-weight: 500;
}

.fw-mono {
  font-family: var(--font-mono);
  color: var(--primary-500);
}

.fw-buttons {
  display: flex;
  gap: 12px;
}

.firewall-info {
  display: flex;
  flex-direction: column;
  gap: 8px;
}

.firewall-info p {
  margin: 0;
  font-size: var(--text-base);
  color: var(--gray-600);
}

.firewall-command {
  display: block;
  padding: 8px;
  background: var(--gray-800);
  color: var(--success-500);
  font-family: var(--font-mono);
  font-size: var(--text-xs);
  border-radius: var(--radius-sm);
  word-break: break-all;
  white-space: pre-wrap;
}

.disabled-content {
  text-align: center;
  color: var(--gray-500);
  padding: 20px;
}

/* 中继配置 */
.relay-toggle {
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  user-select: none;
}

.relay-toggle input[type="checkbox"] {
  cursor: pointer;
}

.relay-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.relay-status {
  padding: 8px 12px;
  border-radius: var(--radius-sm);
  font-size: var(--text-sm);
  font-weight: 500;
  text-align: center;
}

.relay-status.ok {
  background: var(--success-50);
  color: var(--success-600);
}

.relay-status.bad {
  background: var(--warning-50);
  color: var(--warning-600);
}

.relay-hint {
  margin-top: 8px;
  padding: 8px 12px;
  background: var(--gray-50);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  color: var(--gray-600);
  line-height: 1.5;
}

/* 连接安全 */
.security-form {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.security-hint {
  margin-top: 8px;
  padding: 8px 12px;
  background: var(--gray-50);
  border-radius: var(--radius-sm);
  font-size: var(--text-xs);
  color: var(--gray-600);
  line-height: 1.5;
}

/* 按钮样式 */
.btn {
  padding: 8px 16px;
  border-radius: var(--radius-sm);
  font-size: var(--text-base);
  font-weight: 500;
  cursor: pointer;
  border: none;
  transition: all 0.2s;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-primary {
  background: var(--primary-500);
  color: white;
}

.btn-primary:hover:not(:disabled) {
  background: var(--primary-600);
}

.btn-secondary {
  background: var(--gray-500);
  color: white;
}

.btn-secondary:hover:not(:disabled) {
  background: var(--gray-600);
}

.btn-danger {
  background: var(--danger-500);
  color: white;
}

.btn-danger:hover:not(:disabled) {
  background: var(--danger-600);
}
</style>
