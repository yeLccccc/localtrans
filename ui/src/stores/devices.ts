/**
 * 设备 Store
 * 管理设备列表和配对请求
 */

import { defineStore } from 'pinia'
import { ref, shallowRef } from 'vue'
import type { DeviceDto, PairingDto, ChannelDto } from '../types'
import { api, onDeviceList, onConnectionState, onPeerReconnecting, onPairingResult } from '../api'

/** UI 侧配对冷却时长镜像(core pairing::COOLDOWN_SECS=300;仅展示用途,
 * 真实冷却判定在 core 会话层,此处倒计时只是设备卡禁用态的数据源) */
const UI_COOLDOWN_SECS = 300

export const useDevicesStore = defineStore('devices', () => {
  // 设备列表（使用 shallowRef 避免深层响应式）
  const devices = shallowRef<DeviceDto[]>([])

  // M3c T1:通道探测记录表(设备卡通道标签数据源;与设备列表同节奏刷新,
  // 拉取失败不影响设备列表——标签退化为「未知」)
  const channels = shallowRef<ChannelDto[]>([])

  // 待配对列表
  const pairingPending = ref<PairingDto[]>([])

  // P2 配对冷却表(fp → 到期 epoch ms)。数据源:
  // a. connect 报 pairing_cooldown:{secs}(本机即冷却持有方,剩余秒数精确);
  // b. pairing-result 终态错码(接受方,reason 含「3 次」);
  // c. PairingDialog 错码 3 次终态(发起方——对端已记本机冷却,本端镜像)。
  const cooldowns = ref<Record<string, number>>({})
  let cooldownTicker: ReturnType<typeof setInterval> | null = null

  // 选中的设备指纹（用于连接后跳转到浏览页）
  const selected_fp = ref<string | null>(null)

  /** 中继自愈中的指纹(设备卡片显示"重连中…") */
  const reconnecting = ref<Set<string>>(new Set())

  // 加载状态
  const loading = ref(false)

  // 错误信息
  const error = ref<string | null>(null)

  /**
   * 刷新通道探测记录表(设备卡通道标签;失败静默——展示层退化为「未知」)
   */
  async function refreshChannels() {
    try {
      channels.value = await api.devices.listChannels()
    } catch (e) {
      console.warn('Failed to load channels:', e)
    }
  }

  /**
   * 手动单对端快检(M3c T2 通道面板「重新探测」;调用方负责随后刷新通道表)
   */
  async function probeNowPeer(fingerprint: string) {
    await api.devices.probeNowPeer(fingerprint)
  }

  /**
   * M3c T3:强制走中继开关(持久化;成功后乐观修补本地列表,徽章/菜单勾选
   * 即时生效,不等下一轮 device-list 事件)
   */
  async function setForceRelay(fingerprint: string, enabled: boolean) {
    try {
      await api.devices.setForceRelay(fingerprint, enabled)
      devices.value = devices.value.map((d) =>
        d.fingerprint === fingerprint ? { ...d, force_relay: enabled } : d
      )
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to set force relay:', e)
      throw e
    }
  }

  /**
   * 刷新设备列表
   */
  async function refreshDevices() {
    loading.value = true
    error.value = null
    try {
      // 通道表与设备列表同节奏刷新(不阻塞设备列表;探测数据晚几秒到,
      // 下一轮 device-list 事件跟上)
      void refreshChannels()
      devices.value = await api.devices.list()
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to load devices:', e)
    } finally {
      loading.value = false
    }
  }

  /**
   * 刷新待配对列表
   */
  async function refreshPairingPending() {
    try {
      pairingPending.value = await api.pairing.getPending()
    } catch (e) {
      console.error('Failed to load pairing pending:', e)
    }
  }

  /**
   * 立即探测网络
   */
  async function probeNow() {
    try {
      await api.devices.probeNow()
      // 探测后稍等片刻再刷新列表，让发现服务有时间更新
      setTimeout(() => refreshDevices(), 500)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to probe network:', e)
    }
  }

  /**
   * 添加手动设备
   */
  async function addManualDevice(addr: string) {
    try {
      await api.devices.addManual(addr)
      // 添加后稍等片刻再刷新列表
      setTimeout(() => refreshDevices(), 500)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to add manual device:', e)
      throw e
    }
  }

  /**
   * 连接到指定设备
   */
  async function connectDevice(fingerprint: string) {
    try {
      await api.devices.connect(fingerprint)
    } catch (e) {
      // P2:冷却期错误结构化解析 → 记入冷却表(设备卡倒计时禁用态)
      const msg = e instanceof Error ? e.message : String(e)
      const m = msg.match(/^pairing_cooldown:(\d+)/)
      if (m) markCooldown(fingerprint, Number(m[1]))
      error.value = msg
      console.error('Failed to connect device:', e)
      throw e
    }
  }

  /**
   * P2:记入配对冷却(fp 进入 secs 秒冷却,设备卡禁用+倒计时)
   */
  function markCooldown(fingerprint: string, secs: number) {
    cooldowns.value = { ...cooldowns.value, [fingerprint]: Date.now() + secs * 1000 }
    startCooldownTicker()
  }

  /**
   * P2:查询剩余冷却秒数(0=无冷却;过期项惰性清理)
   */
  function cooldownRemainingSecs(fingerprint: string): number {
    const until = cooldowns.value[fingerprint]
    if (!until) return 0
    const left = Math.ceil((until - Date.now()) / 1000)
    if (left <= 0) {
      const next = { ...cooldowns.value }
      delete next[fingerprint]
      cooldowns.value = next
      return 0
    }
    return left
  }

  /** 冷却倒计时节拍器(有冷却项时才跑,驱动设备卡倒计时刷新) */
  function startCooldownTicker() {
    if (cooldownTicker) return
    cooldownTicker = setInterval(() => {
      if (Object.keys(cooldowns.value).length > 0) {
        cooldowns.value = { ...cooldowns.value }
      }
    }, 1000)
  }

  /**
   * 设置是否隐藏
   */
  async function setHidden(hidden: boolean) {
    try {
      await api.devices.setHidden(hidden)
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to set hidden:', e)
      throw e
    }
  }

  /**
   * 提交配对码
   */
  async function submitPairCode(fingerprint: string, peer_code: string) {
    try {
      const ok = await api.pairing.submitCode(fingerprint, peer_code)
      if (ok) {
        // 配对成功，从待配对列表中移除
        pairingPending.value = pairingPending.value.filter(
          (p) => p.fingerprint !== fingerprint
        )
      }
      return ok
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to submit pair code:', e)
      throw e
    }
  }

  /**
   * 拒绝配对
   */
  async function rejectPairing(fingerprint: string) {
    try {
      await api.pairing.deny(fingerprint)
      // 从待配对列表中移除
      pairingPending.value = pairingPending.value.filter(
        (p) => p.fingerprint !== fingerprint
      )
    } catch (e) {
      error.value = e instanceof Error ? e.message : String(e)
      console.error('Failed to reject pairing:', e)
      throw e
    }
  }

  /**
   * 初始化事件监听
   */
  function setupEventListeners() {
    // 监听设备列表更新
    onDeviceList((deviceList) => {
      devices.value = deviceList
      // M3c T1:通道表随 device-list 同节奏刷新(探测数据晚几秒到,
      // 下一轮跟上;连接/断连后标签即时性靠 connection-state 补)
      void refreshChannels()
    })

    // 监听连接状态（QUIC 会话上下线）：即时修补 connected 徽章，
    // 不等下一次 device-list（发现层 5s 周期才有变化）
    onConnectionState(({ fingerprint, up }) => {
      devices.value = devices.value.map((d) =>
        d.fingerprint === fingerprint ? { ...d, connected: up } : d
      )
      // 会话恢复 → 清"重连中"态(SessionUp 自然到达)
      if (up) {
        const s = new Set(reconnecting.value)
        s.delete(fingerprint)
        reconnecting.value = s
      }
    })

    // 中继自愈状态(后端放弃时 active:false;成功由 connection-state up 清)
    onPeerReconnecting(({ fingerprint, active }) => {
      const s = new Set(reconnecting.value)
      if (active) s.add(fingerprint)
      else s.delete(fingerprint)
      reconnecting.value = s
    })

    // P2:错码 3 次终态 → 本端(接受方)真实持有对端冷却,镜像进冷却表;
    // 发起方的冷却镜像由 PairingDialog 终态路径调用 markCooldown 补齐。
    // 其余失败(拒绝/超时/断线)core 不记冷却,不得误标;配对成功清镜像。
    onPairingResult(({ fingerprint, ok, reason }) => {
      if (ok) {
        if (cooldowns.value[fingerprint]) {
          const next = { ...cooldowns.value }
          delete next[fingerprint]
          cooldowns.value = next
        }
      } else if (reason && reason.includes('3 次')) {
        markCooldown(fingerprint, UI_COOLDOWN_SECS)
      }
    })
  }

  /**
   * 初始化 store
   */
  async function initialize() {
    setupEventListeners()
    await refreshDevices()
    await refreshPairingPending()
  }

  /**
   * Task M3: 测试快照（spec §7.1.6 Pinia 侧）。
   * 返回纯 JSON 可序列化对象：JSON 往返剥响应式/函数/undefined，
   * Set 显式转数组。契约见 docs/contracts/test-api.md §5.3。
   */
  function toTestSnapshot() {
    return JSON.parse(
      JSON.stringify({
        schemaVersion: 1,
        devices: devices.value,
        pairingPending: pairingPending.value,
        selectedFp: selected_fp.value,
        reconnecting: Array.from(reconnecting.value),
        loading: loading.value,
        error: error.value,
      }),
    )
  }

  return {
    // 状态
    devices,
    channels,
    pairingPending,
    selected_fp,
    reconnecting,
    loading,
    error,

    // 方法
    refreshDevices,
    refreshChannels,
    refreshPairingPending,
    probeNow,
    probeNowPeer,
    setForceRelay,
    addManualDevice,
    connectDevice,
    markCooldown,
    cooldownRemainingSecs,
    setHidden,
    submitPairCode,
    rejectPairing,
    initialize,
    toTestSnapshot,
  }
})
