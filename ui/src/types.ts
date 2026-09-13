/**
 * LocalTrans TypeScript 类型定义
 * 字段名与 Rust serde 输出一致（蛇形），不进行 camelCase 转换
 */

/**
 * 设备信息 DTO
 */
export interface DeviceDto {
  fingerprint: string
  name: string
  addr: string
  online: boolean
  /** QUIC 会话已建立（心跳确认的"真在线"；online 仅代表广播可见） */
  connected: boolean
  /** 是否通过中继连接（本地发现的设备为 false，仅中继名册中的设备为 true） */
  via_relay?: boolean
  /** M3c T3:强制走中继开关在位(卡片角标+⋮菜单勾选态) */
  force_relay?: boolean
}

/**
 * 配对信息 DTO
 */
export interface PairingDto {
  fingerprint: string
  name: string
  own_code: string
}

/**
 * 通道探测记录 DTO(M3b T4 list_channels;内存态随会话生命周期,
 * 表空 = 该设备无探测记录 → 设备卡通道标签显示「未知」)
 */
export interface ChannelDto {
  fingerprint: string
  addr: string
  via_relay: boolean
  rtt_ms: number | null
  est_bps: number | null
  loss_rate: number
  /** 是否为该设备的当前通道(最近一次成功连接地址) */
  current: boolean
  score_ready: boolean
  probe_disabled: boolean
  age_secs: number
}

/**
 * 同意配对响应 DTO
 */
export interface GrantConsentDto {
  own_code: string
}

/**
 * 本地角色类型
 */
export type LocalRole = 'destination' | 'source-push' | 'source-pull'

/**
 * 健康度 DTO
 */
export interface HealthDto {
  loss_ratio: number
  rtt_ms: number
  cwnd: number
  streams: number
}

/**
 * 传输任务 DTO
 */
export interface TransferDto {
  job_id: string
  name: string
  total: number
  done: number
  state: string // "pending", "active", "paused", "done", "failed", "interrupted"
  speed_bps: number
  peer: string
  direction: string // "pull" | "push"
  local_role: LocalRole
  health: HealthDto | null
  started_at_ms: number | null
  /** 终态时间戳（done/failed/interrupted）——前端据此冻结"已用"计时 */
  finished_at_ms?: number | null
  /** v0.5.0 失败原因（推送被拒绝/超时） */
  fail_reason?: string | null
  /** 传输域重构:排队位次(等待槽位期间 1s 上报,null=未排队) */
  queue_pos?: number | null
  /** 传输域重构:批次 ID(文件夹批量任务归属) */
  batch_id?: string | null
  /** 传输域重构:父卡聚合的子项列表 */
  children?: ChildDto[]
  /** 传输域重构:残留分块组 ID(两级删除 view/destroy 判定用) */
  parts_id?: string | null
  /** 对端已接收字节数(发送侧进度镜像) */
  remote_done?: number
  /** 秒传命中 */
  instant?: boolean
}

/**
 * 父卡子项 DTO(传输域重构)
 */
export interface ChildDto {
  job_id: string
  name: string
  total: number
  done: number
  state: string
}

/**
 * 磁盘历史任务 DTO(传输域重构,历史记录区)
 */
export interface DiskJobDto {
  job_id: string
  display_name: string
  total: number
  done: number
  state: string
  direction: string
  peer_hex: string
  created_at_ms: number | null
  removed_from_view: boolean
}

/**
 * 配置 DTO
 */
export interface ConfigDto {
  device_name: string
  download_dir: string
  hidden: boolean
  quic_port: number
  discovery_port: number
  shares: ShareDefDto[]
  relay_enabled?: boolean
  relay_server?: string
  relay_psk?: string
  consent_timeout_secs?: number
  /** v0.5.0 推送确认超时(秒) */
  offer_timeout_secs?: number
  /** v0.12.0 全局并发任务数上限(1-8,重启后生效) */
  max_active_transfers?: number
}

/**
 * 共享区定义 DTO
 */
export interface ShareDefDto {
  id: string
  alias: string
  path: string
}

/**
 * 信任对等方 DTO
 */
export interface TrustedPeerDto {
  fingerprint: string
  name: string
  /** 本地别名(空=未设置,展示回退 name=对方广播名) */
  alias: string
  paired_at: number
  browse: boolean
  download: boolean
  push: string // "ask", "auto", "deny"
}

/**
 * 目录列表响应 DTO
 */
export interface ListRespDto {
  entries: FileEntry[]
  next_cursor: number | null
}

/**
 * 文件条目
 */
export interface FileEntry {
  name: string
  is_dir: boolean
  size: number
  mtime: number
}

/**
 * 共享区信息（远程）
 */
export interface ShareInfo {
  id: string
  alias: string
}

/**
 * 设备指纹信息
 */
export interface DeviceFingerprint {
  fingerprint_hex: string
  short_code: string
  name: string
}

/** 本机一条可用地址(M3a FR1:全网卡枚举,虚拟网卡已过滤) */
export interface LocalIp {
  ip: string
  if_name: string
}

/** 网络状态体检结果 */
export interface NetworkStatus {
  /** 首选接口地址(兼容别名,与 local_ips[0] 同口径) */
  local_ip: string | null
  /** 全网卡非环回 IPv4 列表,首选置首 */
  local_ips: LocalIp[]
  /** 中继回报的本机公网出口 ip:port(仅中继启用且已注册时非空;M3a FR2) */
  public_exit: string | null
  rule_exists: boolean
  rule_enabled: boolean
  fw_domain: boolean
  fw_private: boolean
  fw_public: boolean
}

/**
 * Toast 消息级别
 */
export type ToastLevel = 'info' | 'success' | 'warning' | 'error'

/**
 * Toast 消息
 */
export interface ToastMessage {
  level: ToastLevel
  text: string
}

/**
 * 传输动作
 */
export type TransferAction = 'pause' | 'resume' | 'cancel'

/**
 * 推送策略
 */
export type PushPolicy = 'ask' | 'auto' | 'deny'

/**
 * 待配对请求事件
 */
export interface PairingRequestEvent {
  fingerprint: string
  name: string
  own_code: string
}

/**
 * 配对结果事件
 */
export interface PairingResultEvent {
  fingerprint: string
  ok: boolean
  reason?: string
}

/**
 * 连接状态事件
 */
export interface ConnectionStateEvent {
  fingerprint: string
  up: boolean
}

/**
 * 文件推送请求事件
 */
export interface OfferRequestEvent {
  job_id: number
  peer: string
  files: OfferFile[]
  /** v0.5.0 超时时间戳(毫秒) */
  deadline_epoch_ms: number
}

/**
 * 推送文件信息
 */
export interface OfferFile {
  name: string
  size: number
  rel_dir: string
}

/**
 * 传输进度事件
 */
export interface TransferProgressEvent {
  jobs: TransferDto[]
}

/**
 * 可恢复任务信息
 */
export type ResumeJobInfo = [string, string]

/**
 * 中继状态 DTO
 */
export interface RelayStatus {
  enabled: boolean
  connected: boolean
  server: string
  devices: number
  /** 配置校验错误(空串=无错);有值时 UI 显示"配置错误: ..." */
  error?: string
}

/**
 * 名片粘贴添加回执(M3a add_by_card;T4 接入 UI)
 * accepted=名片是否被接受进入探测(本机自身名片拒绝,false);
 * addresses_tried=派发单播探测的地址数。
 */
export interface CardProbeResult {
  accepted: boolean
  addresses_tried: number
}
