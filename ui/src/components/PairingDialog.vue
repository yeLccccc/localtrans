<template>
  <div v-if="state" class="pairing-dialog-overlay" @click.self="closeDialog">
    <div class="pairing-dialog" data-testid="pairing-dialog">
      <div class="dialog-header">
        <h2>{{ dialogTitle }}</h2>
        <button v-if="canClose" class="close-btn" data-testid="pairing-close-btn" @click="closeDialog">×</button>
      </div>

      <div class="dialog-content">
        <div class="device-info">
          <div class="device-name">{{ state.name || '未知设备' }}</div>
          <div class="device-fingerprint">{{ formatFingerprint(state.fingerprint) }}</div>
        </div>

        <!-- B 侧:同意门阶段 -->
        <div v-if="state.role === 'acceptor' && state.phase === 'gate'" class="consent-gate">
          <div class="gate-message">
            <p>请求与本机配对，请确认是否为您的操作。</p>
            <div v-if="remainingSecs > 0" class="countdown">
              {{ remainingSecs }}秒后自动拒绝
            </div>
            <div v-else class="countdown expired">
              已自动拒绝
            </div>
          </div>

          <div class="dialog-actions">
            <button
              class="btn btn-reject"
              data-testid="btn-deny"
              @click="handleDeny"
              :disabled="isProcessing"
            >
              拒绝
            </button>
            <button
              class="btn btn-confirm"
              data-testid="btn-grant"
              @click="handleGrant"
              :disabled="isProcessing"
            >
              {{ isProcessing ? '处理中...' : '同意' }}
            </button>
          </div>
        </div>

        <!-- B 侧:亮码阶段 -->
        <div v-else-if="state.role === 'acceptor' && state.phase === 'code'" class="code-display">
          <div class="code-section">
            <div class="code-section-title">配对码</div>
            <CodeBadge :code="state.ownCode" label="请在对方设备上输入此码" />
          </div>

          <div class="dialog-actions">
            <button
              class="btn btn-reject"
              data-testid="btn-cancel-wait"
              @click="handleCancelWait"
              :disabled="isProcessing"
            >
              结束等待
            </button>
          </div>
        </div>

        <!-- A 侧:等待阶段 -->
        <div v-else-if="state.role === 'initiator' && state.phase === 'waiting'" class="waiting-state">
          <div class="waiting-message">
            <p>等待对方确认配对...</p>
          </div>

          <div class="dialog-actions">
            <button
              class="btn btn-reject"
              data-testid="pairing-cancel-wait-btn"
              @click="handleCancelWait"
              :disabled="isProcessing"
            >
              取消
            </button>
          </div>
        </div>

        <!-- A 侧:输入码阶段 -->
        <div v-else-if="state.role === 'initiator' && state.phase === 'entry'" class="code-entry">
          <div class="code-section">
            <div class="code-section-title">输入对方屏幕上显示的配对码</div>
            <input
              v-model="peerCode"
              type="text"
              class="code-input"
              data-testid="code-input"
              placeholder="000000"
              maxlength="6"
              @input="handleCodeInput"
            />
            <div class="code-hint">
              配对码由双方加密通道自动派生，正常情况下两台设备显示<b>完全相同</b>的 6 位码。
            </div>
          </div>

          <div v-if="errorMessage" class="error-message" data-testid="code-error">
            {{ errorMessage }}
          </div>

          <div class="dialog-actions">
            <button
              class="btn btn-reject"
              data-testid="pairing-cancel-entry-btn"
              @click="closeDialog"
              :disabled="isProcessing"
            >
              取消
            </button>
            <button
              class="btn btn-confirm"
              data-testid="btn-submit"
              @click="handleSubmit"
              :disabled="!canSubmit || isProcessing"
            >
              {{ isProcessing ? '提交中...' : '提交' }}
            </button>
          </div>
        </div>

        <!-- A 侧:已提交阶段 -->
        <div v-else-if="state.role === 'initiator' && state.phase === 'submitted'" class="submitted-state">
          <div class="submitted-message">
            <p>配对码已提交，等待对方确认...</p>
          </div>
        </div>

        <!-- A 侧:终态失败(带可操作建议与重试直达) -->
        <div v-else-if="state.role === 'initiator' && state.phase === 'failed'" class="failed-state">
          <p class="failed-reason" data-testid="pairing-failed-reason">{{ state.reason }}</p>
          <div class="dialog-actions">
            <button
              class="btn btn-reject"
              data-testid="pairing-failed-close-btn"
              @click="closeDialog"
            >
              关闭
            </button>
            <button
              v-if="state.canRetry"
              class="btn btn-confirm"
              data-testid="pairing-retry-btn"
              :disabled="isProcessing"
              @click="handleRetry"
            >
              {{ isProcessing ? '连接中...' : '重新发起' }}
            </button>
          </div>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { ref, computed, onUnmounted } from 'vue'
import { useToastStore } from '../stores/toast'
import { useSettingsStore } from '../stores/settings'
import { useDevicesStore } from '../stores/devices'
import {
  onPairingConsentNeeded,
  onPairingCodeShown,
  onPairingWaitConsent,
  onPairingCodeEntry,
  onPairingResult,
  onConnectionState
} from '../api'
import { api, formatConnectError } from '../api'
import CodeBadge from './CodeBadge.vue'

const toastStore = useToastStore()
const settingsStore = useSettingsStore()
const devicesStore = useDevicesStore()

// 配对码最大错误次数(core PairingMachine failed_out 阈值;超限对端进入
// COOLDOWN_SECS=300 冷却)。仅用于本端反馈分层:已错次数=本地提交失败
// 计数,与对端计数天然同源(每个提交恰被对端判定一次)。
const MAX_CODE_ATTEMPTS = 3

// 组件内状态机
type DialogState =
  | { role: 'acceptor'; phase: 'gate'; fingerprint: string; name: string }
  | { role: 'acceptor'; phase: 'code'; fingerprint: string; name: string; ownCode: string }
  | { role: 'initiator'; phase: 'waiting'; fingerprint: string; name: string }
  | { role: 'initiator'; phase: 'entry'; fingerprint: string; name: string }
  | { role: 'initiator'; phase: 'submitted'; fingerprint: string; name: string }
  | { role: 'initiator'; phase: 'failed'; fingerprint: string; name: string; reason: string; canRetry: boolean }

const state = ref<DialogState | null>(null)
const peerCode = ref('')
const errorMessage = ref('')
const isProcessing = ref(false)
const remainingSecs = ref(0)
// 本对话内已错码次数(错码反馈分层:第 1/2 次可重输并提示剩余次数,
// 第 3 次终态+冷却提示)
let failedCodeAttempts = 0
let countdownTimer: ReturnType<typeof setInterval> | null = null
let gateExpiryTimer: ReturnType<typeof setTimeout> | null = null
let submittedWatchdog: ReturnType<typeof setTimeout> | null = null

// 对话框标题
const dialogTitle = computed(() => {
  if (!state.value) return ''

  if (state.value.role === 'acceptor') {
    return state.value.phase === 'gate' ? '配对请求' : '配对码'
  }
  return state.value.phase === 'failed' ? '配对失败' : '配对'
})

// 是否可以关闭对话框
const canClose = computed(() => {
  if (!state.value) return false
  // A 侧等待/失败态与 B 侧亮码阶段允许关闭
  return (state.value.role === 'initiator' &&
           (state.value.phase === 'waiting' || state.value.phase === 'failed')) ||
         (state.value.role === 'acceptor' && state.value.phase === 'code')
})

// 是否可以提交
const canSubmit = computed(() => peerCode.value.length === 6 && !isProcessing.value)

/**
 * 失败原因 → 可操作建议文案(P2 打磨)。
 * 与 src-tauri main.rs 的同名映射保持一致(事件只携带 reason 字符串,
 * 双端各自映射,文案必须同步修改)。
 */
function failureAdvice(reason: string | undefined): string {
  const r = reason || ''
  if (r.includes('拒绝')) return '对方拒绝了本次配对请求，请与对方确认后再试'
  if (r.includes('超时')) return '同意门超时：未在限时内完成确认，请重新发起配对'
  if (r.includes('已结束等待')) return '对方结束了本次配对，可重新发起配对'
  if (r.includes('断开')) return '配对连接已断开：对方可能已离线或取消，请重新发起配对'
  if (r.includes('3 次')) return '配对码连续错误 3 次，配对已终止。对方设备进入约 5 分钟冷却期，请稍后再试'
  if (r.includes('不匹配')) return '配对码不匹配，请核对对方屏幕上的码后重试'
  return r ? `配对失败: ${r}` : '配对失败，请重新发起配对'
}

/**
 * 格式化指纹显示
 */
function formatFingerprint(fp?: string): string {
  if (!fp) return ''
  return `${fp.slice(0, 8)}...${fp.slice(-8)}`
}

/**
 * 处理配对码输入
 */
function handleCodeInput(event: Event) {
  const target = event.target as HTMLInputElement
  // 只允许数字
  peerCode.value = target.value.replace(/\D/g, '').slice(0, 6)
  errorMessage.value = ''
}

/**
 * 关闭对话框
 */
function closeDialog() {
  stopCountdown()
  clearGateExpiry()
  clearSubmittedWatchdog()
  state.value = null
  peerCode.value = ''
  errorMessage.value = ''
  isProcessing.value = false
  failedCodeAttempts = 0
}

/**
 * 发起方进入终态失败态(带建议文案;canRetry=false 时隐藏重新发起,
 * 如错码 3 次冷却期——立即重试必然失败)
 */
function enterFailed(fingerprint: string, name: string, reason: string, canRetry: boolean) {
  clearSubmittedWatchdog()
  state.value = { role: 'initiator', phase: 'failed', fingerprint, name, reason, canRetry }
  peerCode.value = ''
  errorMessage.value = ''
}

/**
 * 错码耗尽终态:镜像对端冷却 + 冷却提示 + 关闭。
 * 两条路径:第 3 次 mismatch 事件到达(确定);或终局消息在竞态中丢失时
 * 的断连兜底(core 实证:FailedOut 的 PairResult 可能没跑赢连接关闭,
 * 只剩 SessionDown)。兜底时本地只确认 2 次错+第 3 次提交在途,无法区分
 * 「对端判错 3 次后断连」与「对端离线」——文案做两可表述,冷却镜像照记
 * (仅 UI 展示;真实冷却以 connect 报错的 pairing_cooldown 为准,会精确刷新)。
 */
function terminalizeCodeFailure(fingerprint: string, probable = false) {
  devicesStore.markCooldown(fingerprint, 300)
  toastStore.push('error', probable
    ? '配对已终止：配对码多次错误或对方已离线。若为码错误，对方设备将进入约 5 分钟冷却，请稍后再试'
    : failureAdvice('配对码错误超过 3 次'))
  closeDialog()
}

/** 本地失败计数是否已耗尽(=3) */
function codesExhausted() {
  return failedCodeAttempts >= MAX_CODE_ATTEMPTS
}

/**
 * 启动倒计时
 */
function startCountdown() {
  // 从配置读取超时时间，默认 60 秒
  const timeout = settingsStore.config?.consent_timeout_secs || 60
  remainingSecs.value = timeout

  countdownTimer = setInterval(() => {
    remainingSecs.value--
    if (remainingSecs.value <= 0) {
      stopCountdown()
      // 倒计时归零只改显示,不主动调 deny:core 门超时任务是唯一事实源,
      // 它会断连并发 PairingResult("同意超时")。此前本地抢跑 handleDeny
      // 与 core 竞态,常弹「拒绝失败: 会话不存在」(P2 T1 表 #2)。
      if (state.value && state.value.role === 'acceptor' && state.value.phase === 'gate') {
        // 兜底:core 事件迟到/丢失时 3s 后收敛,防弹窗悬挂
        clearGateExpiry()
        gateExpiryTimer = setTimeout(() => {
          if (state.value && state.value.role === 'acceptor' && state.value.phase === 'gate') {
            toastStore.push('info', '配对请求已失效（已超时自动拒绝）')
            closeDialog()
          }
        }, 3000)
      }
    }
  }, 1000)
}

function clearGateExpiry() {
  if (gateExpiryTimer) {
    clearTimeout(gateExpiryTimer)
    gateExpiryTimer = null
  }
}

/**
 * submitted 阶段看护:提交后对端长期不判定(边界:码先于同意到达被
 * 暂存、判定后 wire 无回包)时,回输码态给出提示,避免无限挂起。
 */
function startSubmittedWatchdog() {
  clearSubmittedWatchdog()
  submittedWatchdog = setTimeout(() => {
    if (state.value && state.value.role === 'initiator' && state.value.phase === 'submitted') {
      state.value = { role: 'initiator', phase: 'entry', fingerprint: state.value.fingerprint, name: state.value.name }
      errorMessage.value = '对方尚未判定，请确认对方已同意并亮码后重试'
    }
  }, 15000)
}

function clearSubmittedWatchdog() {
  if (submittedWatchdog) {
    clearTimeout(submittedWatchdog)
    submittedWatchdog = null
  }
}

/**
 * 停止倒计时
 */
function stopCountdown() {
  if (countdownTimer) {
    clearInterval(countdownTimer)
    countdownTimer = null
  }
}

/**
 * B 侧:同意配对
 */
async function handleGrant() {
  if (!state.value || state.value.role !== 'acceptor' || state.value.phase !== 'gate') return

  isProcessing.value = true
  stopCountdown()

  try {
    const r = await api.pairing.grant(state.value.fingerprint)
    // 乐观更新：直接使用返回的码
    state.value = {
      role: 'acceptor',
      phase: 'code',
      fingerprint: state.value.fingerprint,
      name: state.value.name,
      ownCode: r.own_code
    }
  } catch (error) {
    const msg = error instanceof Error ? error.message : String(error)
    // 双盲并发/门竞态下 grant 可能命中「会话不处于待同意状态」「会话不存在」:
    // 文案可操作化,不裸抛内部错误(P2 T1 表 #7)
    if (msg.includes('不处于待同意状态') || msg.includes('会话不存在')) {
      toastStore.push('warning', '配对请求已失效（可能已超时或被新请求取代），请稍后重试')
    } else {
      toastStore.push('error', '同意失败: ' + msg)
    }
    closeDialog()
  } finally {
    isProcessing.value = false
  }
}

/**
 * B 侧:拒绝配对
 */
async function handleDeny() {
  if (!state.value || state.value.role !== 'acceptor') return

  isProcessing.value = true
  stopCountdown()

  try {
    await api.pairing.deny(state.value.fingerprint)
    toastStore.push('info', '已拒绝配对请求')
  } catch (error) {
    toastStore.push('error', '拒绝失败: ' + (error instanceof Error ? error.message : String(error)))
  } finally {
    isProcessing.value = false
    closeDialog()
  }
}

/**
 * 取消等待
 */
async function handleCancelWait() {
  if (!state.value) return

  isProcessing.value = true
  stopCountdown()

  try {
    await api.pairing.cancelWait(state.value.fingerprint)
    toastStore.push('info', '已取消配对')
  } catch (error) {
    toastStore.push('error', '取消失败: ' + (error instanceof Error ? error.message : String(error)))
  } finally {
    isProcessing.value = false
    closeDialog()
  }
}

/**
 * A 侧:提交配对码
 */
async function handleSubmit() {
  if (!state.value || state.value.role !== 'initiator' || state.value.phase !== 'entry') return
  if (!canSubmit.value) return

  isProcessing.value = true
  errorMessage.value = ''

  try {
    await api.pairing.submitCode(state.value.fingerprint, peerCode.value)
    // 切换到挂起态(15s 看护:对端长期不判定则回输码态,防无限挂起)
    state.value = { ...state.value, phase: 'submitted' } as DialogState
    startSubmittedWatchdog()
  } catch (error) {
    errorMessage.value = error instanceof Error ? error.message : '提交失败，请重试'
    peerCode.value = ''
  } finally {
    isProcessing.value = false
  }
}

/**
 * 失败态「重新发起」:重连对端,成功后 pairing-wait-consent 事件会把
 * 对话框带回等待态(重试直达,不必回设备列表重新找设备)
 */
async function handleRetry() {
  if (!state.value || state.value.role !== 'initiator' || state.value.phase !== 'failed') return

  isProcessing.value = true
  try {
    await api.devices.connect(state.value.fingerprint)
    // 成功:等待 wait-consent/code-entry 事件驱动状态机;失败:留在失败态
  } catch (error) {
    toastStore.push('error', formatConnectError(error))
  } finally {
    isProcessing.value = false
  }
}

// 事件监听器（脚本顶层注册）
// v0.11.0 低危批修复:此前分别保存 5 个 unlisten,若组件在 Promise.all 完成
// 前卸载(注销竞态),then 回调虽写入变量但无人再调用——监听器泄漏且闭包
// 内仍会 setState。改为同步收集到数组 + isUnmounted 标志双保险。
const unlisteners: Array<() => void> = []
let isUnmounted = false

function registerAll(un: Array<() => void>) {
  unlisteners.push(...un)
  if (isUnmounted) {
    // 已卸载才完成注册的:立即清理,不留悬挂监听
    un.forEach(fn => fn())
    unlisteners.length = 0
  }
}

onUnmounted(() => {
  isUnmounted = true
})

// 立即注册事件监听器（不在 onMounted 中）
Promise.all([
  // B 侧:收到同意门请求
  onPairingConsentNeeded(({ fingerprint, name }) => {
    state.value = { role: 'acceptor', phase: 'gate', fingerprint, name }
    startCountdown()
  }),

  // B 侧:配对码显示（兜底事件，覆盖乐观更新）
  onPairingCodeShown(({ fingerprint, own_code }) => {
    if (state.value &&
        state.value.role === 'acceptor' &&
        state.value.phase === 'gate' &&
        state.value.fingerprint === fingerprint) {
      stopCountdown()
      state.value = {
        role: 'acceptor',
        phase: 'code',
        fingerprint,
        name: state.value.name,
        ownCode: own_code
      }
    }
  }),

  // A 侧:等待对方同意
  onPairingWaitConsent(({ fingerprint, name }) => {
    state.value = { role: 'initiator', phase: 'waiting', fingerprint, name }
  }),

  // A 侧:可以输入码了
  onPairingCodeEntry(({ fingerprint, name }) => {
    if (state.value &&
        state.value.role === 'initiator' &&
        state.value.phase === 'waiting' &&
        state.value.fingerprint === fingerprint) {
      state.value = { role: 'initiator', phase: 'entry', fingerprint, name }
    }
  }),

  // 配对结果
  onPairingResult(({ fingerprint, ok, reason }) => {
    // 只有当前对话框的指纹匹配时才处理
    if (!state.value || state.value.fingerprint !== fingerprint) {
      return
    }

    if (ok) {
      toastStore.push('success', '配对成功')
      closeDialog()
      return
    }

    const r = reason || ''

    // ---- 错码反馈分层(P2 T1 表 #3/#4):第 1/2 次可重输(提示剩余次数),
    // 第 3 次终态+冷却提示。已错次数用本地提交失败计数——每个提交恰被
    // 对端判定一次,本地计数与对端 PairingMachine 计数天然同源。
    const isMismatch = r.includes('不匹配') || r.includes('码错误')
    if (isMismatch && state.value.role === 'initiator' &&
        (state.value.phase === 'submitted' || state.value.phase === 'entry')) {
      failedCodeAttempts++
      if (r.includes('3 次') || codesExhausted()) {
        terminalizeCodeFailure(fingerprint)
      } else {
        // 回到 entry 态，允许重输
        state.value = { role: 'initiator', phase: 'entry', fingerprint, name: state.value.name }
        errorMessage.value = `配对码不匹配，还可重试 ${MAX_CODE_ATTEMPTS - failedCodeAttempts} 次`
        peerCode.value = ''
      }
      return
    }

    if (state.value.role === 'initiator') {
      // 发起方终态失败:进失败态(建议文案 + 重试直达)。例外:错码已计满
      // 3 次(确定冷却),或第 3 次码在途时收到断连结果(竞态丢失形态,按
      // 概率冷却终态收敛)——均不显示重试按钮
      const thirdLost = state.value.phase === 'submitted' &&
        failedCodeAttempts >= MAX_CODE_ATTEMPTS - 1 && r.includes('断开')
      if (codesExhausted() || thirdLost) {
        terminalizeCodeFailure(fingerprint, !codesExhausted())
      } else {
        const cooldownish = r.includes('3 次')
        enterFailed(fingerprint, state.value.name, failureAdvice(reason), !cooldownish)
      }
    } else {
      // 接受方:失败即关闭(toast 由壳层弹出同款建议文案)
      closeDialog()
    }
  }),

  // 连接断开兜底(P2 T1 表 #6):输码/等待中对端离线或会话被对端终止时,
  // 核心未必发 PairingResult(如错码 3 次后对端断连),用 connection-state
  // 收敛对话框,防 UI 悬挂在等待/输码态。第 3 次码在途时断连 = 错码耗尽
  // 的竞态丢失形态,按(概率)冷却终态收敛
  onConnectionState(({ fingerprint, up }) => {
    if (up || !state.value) return
    if (state.value.fingerprint !== fingerprint) return
    if (state.value.role !== 'initiator') return
    if (state.value.phase === 'waiting' || state.value.phase === 'entry' || state.value.phase === 'submitted') {
      const thirdInFlight = state.value.phase === 'submitted' && failedCodeAttempts >= MAX_CODE_ATTEMPTS - 1
      if (codesExhausted() || thirdInFlight) {
        terminalizeCodeFailure(fingerprint, !codesExhausted())
      } else {
        enterFailed(fingerprint, state.value.name, failureAdvice('连接已断开'), true)
      }
    }
  })
]).then(registerAll)

/**
 * 清理事件监听
 */
onUnmounted(() => {
  stopCountdown()
  unlisteners.forEach(fn => fn())
  unlisteners.length = 0
})
</script>

<style scoped>
.pairing-dialog-overlay {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  bottom: 0;
  background: rgba(0, 0, 0, 0.5);
  display: flex;
  align-items: center;
  justify-content: center;
  z-index: 1000;
  padding: 20px;
}

.pairing-dialog {
  background: white;
  border-radius: var(--radius-md);
  max-width: 500px;
  width: 100%;
  box-shadow: 0 20px 25px -5px rgba(0, 0, 0, 0.1);
}

.dialog-header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  padding: 20px;
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
  transition: background-color 0.2s;
}

.close-btn:hover {
  background: var(--gray-100);
}

.dialog-content {
  padding: 20px;
}

.device-info {
  text-align: center;
  margin-bottom: 24px;
}

.device-name {
  font-size: var(--text-lg);
  font-weight: 600;
  color: var(--gray-800);
  margin-bottom: 4px;
}

.device-fingerprint {
  font-size: var(--text-xs);
  color: var(--gray-500);
  font-family: var(--font-mono);
}

/* 同意门 */
.consent-gate {
  text-align: center;
}

.gate-message {
  margin-bottom: 24px;
}

.gate-message p {
  font-size: var(--text-base);
  color: var(--gray-600);
  margin-bottom: 12px;
}

.countdown {
  font-size: var(--text-sm);
  color: var(--warning-600);
  font-weight: 500;
}

.countdown.expired {
  color: var(--danger-600);
}

/* 亮码和输入码 */
.code-section {
  text-align: center;
  margin-bottom: 24px;
}

.code-section-title {
  font-size: var(--text-base);
  font-weight: 500;
  color: var(--gray-500);
  margin-bottom: 12px;
}

.code-input {
  width: 100%;
  max-width: 200px;
  padding: 16px;
  font-size: var(--text-xl);
  font-family: 'Courier New', Courier, monospace;
  text-align: center;
  letter-spacing: 8px;
  border: 2px solid var(--gray-200);
  border-radius: var(--radius-md);
  background: var(--gray-50);
  margin: 0 auto;
  display: block;
}

.code-input:focus {
  outline: none;
  border-color: var(--primary-500);
  background: white;
}

.code-hint {
  margin-top: 12px;
  font-size: var(--text-xs);
  line-height: 1.6;
  color: var(--gray-400);
}

/* 等待和提交状态 */
.waiting-state,
.submitted-state,
.failed-state {
  text-align: center;
}

.failed-reason {
  font-size: var(--text-base);
  color: var(--gray-600);
  line-height: 1.7;
  margin: 8px 0 24px;
}

.waiting-message p,
.submitted-message p {
  font-size: var(--text-base);
  color: var(--gray-600);
}

.error-message {
  background: var(--danger-50);
  border: 1px solid #fecaca;
  color: var(--danger-600);
  padding: 12px;
  border-radius: var(--radius-sm);
  margin-bottom: 16px;
  font-size: var(--text-base);
  text-align: center;
}

.dialog-actions {
  display: flex;
  gap: 12px;
  justify-content: center;
}

.btn {
  padding: 12px 24px;
  border-radius: var(--radius-md);
  font-size: var(--text-md);
  font-weight: 500;
  cursor: pointer;
  border: none;
  transition: all 0.2s;
  min-width: 120px;
}

.btn:disabled {
  opacity: 0.5;
  cursor: not-allowed;
}

.btn-reject {
  background: var(--gray-100);
  color: var(--gray-700);
}

.btn-reject:hover:not(:disabled) {
  background: var(--gray-200);
}

.btn-confirm {
  background: var(--primary-500);
  color: white;
}

.btn-confirm:hover:not(:disabled) {
  background: var(--primary-600);
}
</style>
