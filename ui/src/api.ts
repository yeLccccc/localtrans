/**
 * LocalTrans Tauri API 封装
 * 与 Task 16 的 command 名与事件名严格一致
 */

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import type {
  DeviceDto,
  PairingDto,
  ChannelDto,
  TransferDto,
  ConfigDto,
  ShareDefDto,
  TrustedPeerDto,
  ListRespDto,
  DeviceFingerprint,
  NetworkStatus,
  ResumeJobInfo,
  DiskJobDto,
  ShareInfo,
  TransferAction,
  PushPolicy,
  ToastMessage,
  RelayStatus,
  GrantConsentDto,
  CardProbeResult,
} from './types'

/**
 * A8 隐私修复:错误消息友好化——把 Windows 绝对路径替换为其主文件名,
 * 防止本地目录结构泄露到 UI/日志。
 * 例:"无法读取 C:\\Users\\me\\docs\\report.pdf: 拒绝访问" → "无法读取 report.pdf: 拒绝访问"
 */
export function friendlyError(raw: unknown): string {
  const msg = raw instanceof Error ? raw.message : String(raw)
  // 盘符路径(正/反斜杠)与 UNC 路径两形态,均截为主文件名
  const winPath = /[A-Za-z]:[\\/][^\s'"`,;]+|\\\\[^\s'"`,;]+/g
  return msg.replace(winPath, (m) => {
    const parts = m.split(/[\\/]/)
    return parts[parts.length - 1] || m
  })
}

/**
 * connect 错误 → 用户可读文案(P2 配对健壮性 T2)。
 * 壳层 fmt_connect_err(commands.rs)把冷却期错误结构化为
 * `pairing_cooldown:{secs}` 前缀,此处映射为可操作建议文案;
 * 其余错误附加「连接设备失败:」前缀原样透传。
 */
export function formatConnectError(e: unknown): string {
  const msg = e instanceof Error ? e.message : String(e)
  const m = msg.match(/^pairing_cooldown:(\d+)/)
  if (m) {
    const secs = Number(m[1])
    return `对方设备处于配对冷却（剩余 ${secs} 秒），此前的配对码连续错误触发，请稍后再试`
  }
  return '连接设备失败: ' + msg
}

/**
 * 基础 API 调用封装
 */
async function invokeCommand<T>(command: string, args?: Record<string, unknown>): Promise<T> {
  // 15s 客户端超时兜底:Tauri IPC 在某些路径下(WebView 卡顿 + Rust
  // 端阻塞 + 中继数据面丢包)可能挂死不 reject,前端看到的就是永久
  // loading。race 一旦超时立即 reject,UI 显示错误并退出 loading 态。
  // Rust 侧本身已有 10s 服务端超时,客户端兜底只是双保险。
  let timeoutHandle: ReturnType<typeof setTimeout> | undefined
  try {
    return await Promise.race([
      invoke<T>(command, args),
      new Promise<never>((_, reject) => {
        timeoutHandle = setTimeout(
          () => reject(new Error(`调用 ${command} 超时(15s)`)),
          15000,
        )
      }),
    ])
  } catch (error) {
    // A8 隐私修复:只打命令名 + 路径脱敏后的消息,且降为 warn(错误本身 throw 给调用方)
    console.warn(`Command ${command} failed: ${friendlyError(error)}`)
    throw error
  } finally {
    if (timeoutHandle !== undefined) clearTimeout(timeoutHandle)
  }
}

/**
 * 设备命令 API
 */
export const devicesApi = {
  /**
   * 列出当前发现的设备
   */
  list: (): Promise<DeviceDto[]> =>
    invokeCommand('list_devices'),

  /**
   * 设置是否隐藏
   */
  setHidden: (hidden: boolean): Promise<void> =>
    invokeCommand('set_hidden', { hidden }),

  /**
   * 立即探测网络
   */
  probeNow: (): Promise<void> =>
    invokeCommand('probe_now'),

  /**
   * 添加手动设备
   */
  addManual: (addr: string): Promise<void> =>
    invokeCommand('add_manual_device', { addr }),

  /**
   * 连接到指定设备（按指纹）
   */
  connect: (fingerprint: string): Promise<void> =>
    invokeCommand('connect', { fingerprint }),

  /**
   * 通道探测记录表(M3c T1:设备卡通道标签数据源;随设备列表同节奏刷新)
   */
  listChannels: (): Promise<ChannelDto[]> =>
    invokeCommand('list_channels'),

  /**
   * 手动单对端快检(M3c T2:通道面板「重新探测」按钮;当前通道 64KB 快检,
   * 掉 50% 升级全量——与后台 5min 调度器同路径,只更新内存通道表)
   */
  probeNowPeer: (fingerprint: string): Promise<void> =>
    invokeCommand('probe_now_peer', { fingerprint }),

  /**
   * 强制走中继开关(M3c T3:per 设备持久化;开启后 connect 决策跳过评分
   * 直选中继路径。排障后门,PC/安卓设备卡 ⋮ 菜单勾选项)
   */
  setForceRelay: (fingerprint: string, enabled: boolean): Promise<boolean> =>
    invokeCommand('set_force_relay', { fingerprint, enabled }),

  /**
   * 生成本机名片文本(M3a T3 FR3:人可读多行,含设备名/指纹/全部直连地址
   * 与可选公网/中继行;只含公开事实)。T4:设备页本机 IP 徽章一键复制用。
   */
  getBusinessCard: (): Promise<string> =>
    invokeCommand('get_business_card'),

  /**
   * 粘贴名片添加(M3a T3 FR4):解析→逐地址单播探测,结果经既有
   * manual-probe-result 事件异步回执。T4:手动添加弹窗粘名片全文用。
   */
  addByCard: (text: string): Promise<CardProbeResult> =>
    invokeCommand('add_by_card', { text }),
}

/**
 * 配对命令 API
 */
export const pairingApi = {
  /**
   * 获取待配对列表
   */
  getPending: (): Promise<PairingDto[]> =>
    invokeCommand('get_pairing_pending'),

  /**
   * 提交配对码
   */
  submitCode: (fingerprint: string, peer_code: string): Promise<boolean> =>
    // Tauri 2 要求 JS 参数名用 camelCase（Rust 侧 peer_code → peerCode）
    invokeCommand('submit_pair_code', { fingerprint, peerCode: peer_code }),

  /**
   * 同意配对
   */
  grant: (fingerprint: string): Promise<GrantConsentDto> =>
    invokeCommand('grant_consent', { fingerprint }),

  /**
   * 拒绝配对
   */
  deny: (fingerprint: string): Promise<void> =>
    invokeCommand('deny_consent', { fingerprint }),

  /**
   * 取消等待配对
   */
  cancelWait: (fingerprint: string): Promise<void> =>
    invokeCommand('cancel_pairing_wait', { fingerprint }),
}

/**
 * 浏览/传输命令 API
 */
export const browseApi = {
  /**
   * 远程列出共享区
   */
  listSharesRemote: (fingerprint: string): Promise<ShareInfo[]> =>
    invokeCommand('list_shares_remote', { fingerprint }),

  /**
   * 远程列出目录
   */
  listDirRemote: (
    fingerprint: string,
    share_id: string,
    path: string,
    cursor: number
  ): Promise<ListRespDto> =>
    invokeCommand('list_dir_remote', { fingerprint, shareId: share_id, path, cursor }),

  /**
   * 开始下载
   */
  startDownload: (
    fingerprint: string,
    share_id: string,
    path: string
  ): Promise<string> =>
    invokeCommand('start_download', { fingerprint, shareId: share_id, path }),

  /**
   * v0.2.6 文件夹下载（递归枚举 + 结构保持）
   */
  startDownloadDir: (
    fingerprint: string,
    share_id: string,
    path: string
  ): Promise<string> =>
    invokeCommand('start_download_dir', { fingerprint, shareId: share_id, path }),

  /**
   * N1-T4: 多选文件批量下载（1 批次 → 传输页 1 张父卡片）
   */
  startDownloadBatch: (
    fingerprint: string,
    share_id: string,
    paths: string[]
  ): Promise<string> =>
    invokeCommand('start_download_batch', { fingerprint, shareId: share_id, paths }),

  /**
   * 推送文件
   */
  pushFiles: (fingerprint: string, local_paths: string[]): Promise<string> =>
    invokeCommand('push_files', { fingerprint, localPaths: local_paths }),

  /**
   * v0.2.6 带结构推送（文件夹）。items: [本地路径, 相对目录]
   */
  pushFilesRel: (fingerprint: string, items: [string, string][]): Promise<string> =>
    invokeCommand('push_files_rel', { fingerprint, items }),

  /**
   * v0.2.6 展开本地路径（文件夹递归）为 [文件, 相对目录] 列表
   */
  expandLocalPaths: (paths: string[]): Promise<[string, string][]> =>
    invokeCommand('expand_local_paths', { paths }),

  /**
   * 响应对等方文件推送请求
   */
  respondOffer: (
    job_id: number,
    accepted: boolean,
    save_dir?: string
  ): Promise<void> =>
    invokeCommand('respond_offer', { jobId: job_id, accepted, saveDir: save_dir }),

  /**
   * v0.5.0 顺延推送确认超时
   */
  offerExtend: (job_id: number): Promise<void> =>
    invokeCommand('offer_extend', { jobId: job_id }),

  /**
   * 传输任务控制
   */
  transferAction: (job_id: string, action: TransferAction): Promise<void> =>
    invokeCommand('transfer_action', { jobId: job_id, action }),
}

/**
 * 队列命令 API
 */
export const transfersApi = {
  /**
   * 列出传输任务
   */
  list: (): Promise<TransferDto[]> =>
    invokeCommand('list_transfers'),

  /**
   * 列出可恢复的待处理任务
   */
  pendingResumeJobs: (): Promise<ResumeJobInfo[]> =>
    invokeCommand('pending_resume_jobs'),

  /**
   * 恢复待处理任务
   */
  resumePending: (job_id: string): Promise<void> =>
    invokeCommand('resume_pending', { jobId: job_id }),

  /**
   * 清除已完成的任务
   */
  clearCompleted: (): Promise<number> =>
    invokeCommand('clear_completed_transfers'),

  /**
   * 删除传输任务(两级:level="view" 移除视图 | level="destroy" 彻底删除)
   */
  removeTransfer: (job_id: string, level: 'view' | 'destroy'): Promise<boolean> =>
    invokeCommand('remove_transfer', { jobId: job_id, level }),

  /**
   * 列出磁盘历史任务(传输域重构,历史记录区)
   */
  listDiskJobs: (): Promise<DiskJobDto[]> =>
    invokeCommand('list_disk_jobs'),

  /**
   * 恢复磁盘历史任务到视图
   */
  restoreDiskJob: (job_id: string): Promise<void> =>
    invokeCommand('restore_disk_job', { jobId: job_id }),

  /**
   * 彻底销毁磁盘历史任务(含分块与记录)
   */
  destroyDiskJob: (job_id: string): Promise<void> =>
    invokeCommand('destroy_disk_job', { jobId: job_id }),

  /**
   * 重试父卡下失败子项(path=本地绝对路径, rel_dir=对端落盘相对目录,
   * child_job_id=失败子项的 engine job hex)
   */
  retryChild: (
    parent_card_id: string,
    child_job_id: string,
    fp: string,
    rel_dir: string,
    path: string
  ): Promise<number> =>
    invokeCommand('retry_child', {
      parentCardId: parent_card_id,
      childJobId: child_job_id,
      fp,
      relDir: rel_dir,
      path,
    }),

  /**
   * 限制传输并发流数
   */
  transferThrottle: (job_id: string, maxStreams: number): Promise<void> =>
    invokeCommand('transfer_throttle', { jobId: job_id, maxStreams }),

  /**
   * 检查任务是否有残留分块
   */
  hasParts: (job_id: string): Promise<boolean> =>
    invokeCommand('has_parts', { jobId: job_id }),
}

/**
 * 设置命令 API
 */
export const settingsApi = {
  /**
   * 获取设置
   */
  get: (): Promise<ConfigDto> =>
    invokeCommand('get_settings'),

  /**
   * 保存设置
   */
  save: (dto: ConfigDto): Promise<void> =>
    invokeCommand('save_settings', { dto }),

  /**
   * 添加共享区
   */
  addShare: (alias: string, path: string): Promise<ShareDefDto> =>
    invokeCommand('add_share', { alias, path }),

  /**
   * 移除共享区
   */
  removeShare: (id: string): Promise<boolean> =>
    invokeCommand('remove_share', { id }),

  /**
   * 列出信任对等方
   */
  listTrusted: (): Promise<TrustedPeerDto[]> =>
    invokeCommand('list_trusted'),

  /**
   * 设置权限
   */
  setPerms: (
    fingerprint: string,
    browse: boolean,
    download: boolean,
    push: PushPolicy
  ): Promise<boolean> =>
    invokeCommand('set_perms', { fingerprint, browse, download, push }),

  /**
   * 移除信任对等方
   */
  removeTrusted: (fingerprint: string): Promise<boolean> =>
    invokeCommand('remove_trusted', { fingerprint }),

  /**
   * 设置信任对端本地别名(空串=清除,回退显示对方广播名)
   */
  setAlias: (fingerprint: string, alias: string): Promise<boolean> =>
    invokeCommand('set_alias', { fingerprint, alias }),

  /**
   * 设置中继配置
   */
  setRelayConfig: (enabled: boolean, server: string, psk: string): Promise<void> =>
    invokeCommand('set_relay_config', { enabled, server, psk }),

  /**
   * 获取中继状态
   */
  relayStatus: (): Promise<RelayStatus> =>
    invokeCommand('relay_status'),
}

/**
 * 系统命令 API
 */
export const systemApi = {
  /**
   * 添加防火墙规则
   */
  addFirewallRule: (): Promise<string> =>
    invokeCommand('add_firewall_rule'),

  /**
   * 获取设备指纹信息
   */
  getDeviceFingerprint: (): Promise<DeviceFingerprint> =>
    invokeCommand('get_device_fingerprint'),

  /**
   * 网络状态体检（本机IP / 防火墙规则 / 配置文件开关）
   */
  getNetworkStatus: (): Promise<NetworkStatus> =>
    invokeCommand('get_network_status'),

  /**
   * 打开日志文件夹（排障用）
   */
  openLogsDir: (): Promise<void> =>
    invokeCommand('open_logs_dir'),

  /**
   * v0.2.7 优雅关闭准备：Goodbye 通知对端 + 任务表落盘
   */
  prepareShutdown: (): Promise<void> =>
    invokeCommand('prepare_shutdown'),
}

/**
 * 统一 API 导出
 */
export const api = {
  devices: devicesApi,
  pairing: pairingApi,
  browse: browseApi,
  transfers: transfersApi,
  settings: settingsApi,
  system: systemApi,
}

/**
 * 事件监听封装
 * 与 Rust emit() 事件名严格一致
 */
export function onEvent<T>(
  eventName: string,
  callback: (event: T) => void
) {
  return listen(eventName, (event) => {
    callback(event.payload as T)
  })
}

/**
 * 设备列表事件
 */
export function onDeviceList(callback: (devices: DeviceDto[]) => void) {
  return onEvent('device-list', callback)
}

/**
 * 配对同意门需要同意事件(B侧)
 */
export function onPairingConsentNeeded(callback: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-consent-needed', callback)
}

/**
 * 配对码显示事件(B侧)
 */
export function onPairingCodeShown(callback: (p: { fingerprint: string; own_code: string }) => void) {
  return onEvent('pairing-code-shown', callback)
}

/**
 * 配对等待同意事件(A侧)
 */
export function onPairingWaitConsent(callback: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-wait-consent', callback)
}

/**
 * 配对码输入事件(A侧)
 */
export function onPairingCodeEntry(callback: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-code-entry', callback)
}

/**
 * 配对结果事件
 */
export function onPairingResult(callback: (result: { fingerprint: string; ok: boolean; reason?: string }) => void) {
  return onEvent('pairing-result', callback)
}

/**
 * 连接状态事件
 */
export function onConnectionState(callback: (state: { fingerprint: string; up: boolean }) => void) {
  return onEvent('connection-state', callback)
}

/**
 * 中继会话自愈状态(重连中/放弃)
 */
export function onPeerReconnecting(callback: (s: { fingerprint: string; active: boolean }) => void) {
  return onEvent('peer-reconnecting', callback)
}

/**
 * 文件推送请求事件
 */
export function onOfferRequest(callback: (request: { job_id: number; peer: string; files: unknown[] }) => void) {
  return onEvent('offer-request', callback)
}

/**
 * 远程删除确认事件
 */
export function onDeleteRequest(callback: (req: {
  ask_id: number; fingerprint: string; peer_name: string; share_id: string;
  name: string; is_dir: boolean; entry_count: number; deadline_epoch_ms: number;
}) => void) {
  return onEvent('delete-request', callback)
}

/**
 * 应答远程删除确认
 */
export function respondDelete(askId: number, allow: boolean) {
  return invokeCommand('respond_delete', { askId, allow })
}

/**
 * Toast 消息事件
 */
export function onToast(callback: (toast: ToastMessage) => void) {
  return onEvent('toast', callback)
}

/**
 * 传输进度事件
 */
export function onTransferProgress(callback: (progress: { jobs: TransferDto[] }) => void) {
  return onEvent('transfer-progress', callback)
}

/**
 * 对端共享区变化事件（对端 watchdog 检测到目录内容变化后推送）
 */
export function onRemoteSharesChanged(callback: (n: { fingerprint: string; share_id: string }) => void) {
  return onEvent('remote-shares-changed', callback)
}

/**
 * 中继状态变化事件
 */
export function onRelayState(callback: (state: { status: string }) => void) {
  return onEvent('relay-state', callback)
}
