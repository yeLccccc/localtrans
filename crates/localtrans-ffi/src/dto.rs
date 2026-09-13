// uniFFI Record types for Android FFI layer

/// Device information DTO
#[derive(uniffi::Record, Clone, Debug)]
pub struct DeviceDto {
    pub fingerprint: String,
    pub name: String,
    pub addr: String,
    pub online: bool,
    pub connected: bool,
    pub via_relay: bool,
    /// M3c T3:强制走中继开关在位(config.json force_relay_map;卡片角标+菜单勾选态)
    pub force_relay: bool,
}

/// Transfer task DTO
/// Note: progress_percent uses u8 (0-100), speed_bps uses u64, eta_secs uses i64 (-1 for unknown)
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct TransferDto {
    pub job_id: u64,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,
    pub speed_bps: u64,
    pub peer: String,
    pub direction: String,
    pub local_role: String,
    pub progress_percent: u8,
    pub eta_secs: i64,
    pub fail_reason: String, // Empty string for no failure (uniffi 0.28 supports Option but using empty string for stability)
    /// 接收完成后首个落盘文件的绝对路径(发送侧/未落盘为 None)。
    /// Android "查看"跳转用;uniffi 0.28 Option → Kotlin String?
    pub local_path: Option<String>,
    /// v0.10.0 push 方向:对端累计已确认字节(RecvProgress 驱动;0=对端未上报)
    pub remote_done: u64,
    /// v0.10.0 秒传命中标记(接收侧 InstantHit 置 true;done+instant 显示"秒传"徽标)
    pub instant: bool,
    /// v0.11.0 排队位置(队列卡片展示;None=不在排队)——M2 T1 契约先行,本阶段恒 None
    pub queue_pos: Option<u32>,
    /// v0.11.0 批次 id(同一次多选传输归属同批,折叠展示)——M2 T1 契约先行,本阶段恒 None
    pub batch_id: Option<String>,
    /// v0.11.0 文件夹任务的子文件列表(父卡片聚合展示)——M2 T1 契约先行,本阶段恒空
    pub children: Vec<ChildDto>,
    /// v0.11.0 分块续传任务归属的 parts 目录名——M2 T1 契约先行,本阶段恒 None
    pub parts_id: Option<String>,
    /// 开始时间戳(毫秒);None=未开始——M2 T1 契约先行,本阶段恒 None
    pub started_at_ms: Option<i64>,
    /// 终态时间戳(done/failed/interrupted 盖戳;None=非终态)——M2 T1 契约先行,本阶段恒 None
    pub finished_at_ms: Option<i64>,
    /// v0.2.8 文件夹任务的恢复参数("share_id|dir_path");单文件任务为 None——M2 T1 契约先行,本阶段恒 None
    pub source_path: Option<String>,
    /// 传输健康度(QUIC 丢包/RTT/拥塞窗口/流数)——M2 T1 契约先行,本阶段恒 None
    pub health: Option<HealthDto>,
}

/// v0.11.0 文件夹任务子文件 DTO(父卡片 children 展开行;对齐 PC 端 ChildDto)
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct ChildDto {
    pub job_id: String,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,
}

/// 传输健康度 DTO(对齐 PC 端 HealthDto:QUIC 丢包率/RTT/拥塞窗口/流数)
#[derive(uniffi::Record, Clone, Debug, Default)]
pub struct HealthDto {
    pub loss_ratio: f64,
    pub rtt_ms: u64,
    pub cwnd: u64,
    pub streams: u32,
}

/// 磁盘历史条目 DTO(M2 T3;对齐 PC 端 commands.rs DiskJobDto 字段)。
/// 数据源=parts 根下的 manifest.json(+meta)全量扫盘,合并表内"磁盘已无
/// parts 且非 open"的终态卡;display_name 只进 DTO 不进日志(隐私红线)。
/// job_id 用 u64(PC 端因前端传 hex 字串故用 String;ffi 直传整数)。
#[derive(uniffi::Record, Clone, Debug)]
pub struct DiskJobDto {
    pub job_id: u64,
    pub display_name: String,
    pub total: u64,
    /// 已收块数(PC list_disk_jobs 同款口径:块计数而非字节;卡片 dto.done 才是字节)
    pub done: u64,
    /// "interrupted"(缺块)/"failed"(位图全真未 finalize,完整性存疑)/表内卡原状态
    pub state: String,
    pub direction: String,
    pub peer_hex: String,
    pub created_at_ms: Option<i64>,
    /// 视图已移除(两级删除第一级);纯磁盘条目恒 false
    pub removed_from_view: bool,
}

/// Settings DTO
#[derive(uniffi::Record, Clone, Debug)]
pub struct SettingsDto {
    pub device_name: String,
    pub hidden: bool,
    pub offer_timeout_secs: u64,
    pub consent_timeout_secs: u64,
    pub relay_enabled: bool,
    pub relay_addr: String,
    pub relay_psk: String,
    /// M2 T4:全局并发传输上限(闸门容量,钳制 1-8;启动构建,改动重启生效)
    pub max_active_transfers: u32,
    pub backup_enabled: bool,
    pub backup_target_fp: String,
    pub backup_photos: bool,
    pub backup_videos: bool,
}

/// File entry DTO for directory listings
#[derive(uniffi::Record, Clone, Debug)]
pub struct FileEntryDto {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ms: i64,
}

/// Share information DTO
#[derive(uniffi::Record, Clone, Debug)]
pub struct ShareDto {
    pub share_id: String,
    pub alias: String,
}

/// File operation enum
#[derive(uniffi::Enum, Clone, Debug)]
pub enum FileOp {
    Rename,
    Delete,
    Mkdir,
}

/// Push file with relative directory (for folder structure preservation)
#[derive(uniffi::Record, Clone, Debug)]
pub struct PushFileDto {
    pub path: String,
    pub rel_dir: String,
}

/// 中继状态(设置页显示;拉模式)
#[derive(uniffi::Record, Clone, Debug)]
pub struct RelayStatusDto {
    pub enabled: bool,
    pub connected: bool,
    pub status: String,
    pub error: String,
    /// M3a FR2:中继 RegisterAck 回报的本机公网出口 ip:port(镜像桌面
    /// get_network_status.public_exit)。仅中继启用且已注册时为 Some;旧服务端为 None。
    pub public_exit: Option<String>,
}

/// 本机一条可用地址(M3a FR1:全网卡枚举;镜像桌面 get_network_status.local_ips 元素)。
/// ip 为 IPv4 点分字串;if_name 为接口名(Android 如 "wlan0")。虚拟网卡/环回已在 core 过滤。
#[derive(uniffi::Record, Clone, Debug, PartialEq, Eq)]
pub struct LocalIpDto {
    pub ip: String,
    pub if_name: String,
}

/// 一条通道探测记录的只读 DTO(M3b T4;镜像桌面 list_channels 命令的字段口径)。
/// 数据源 = AppState.channels(core::routing::ChannelTable,内存态不持久化)。
/// age_secs = 距最近一次记录更新的秒数(updated_at 是单调钟 Instant,只能给
/// elapsed 不给时刻)。current = 该地址是否为该设备的当前通道(最近一次成功连接)。
#[derive(uniffi::Record, Clone, Debug)]
pub struct ChannelDto {
    pub fingerprint: String,
    pub addr: String,
    pub via_relay: bool,
    pub rtt_ms: Option<u64>,
    pub est_bps: Option<u64>,
    pub loss_rate: f64,
    pub current: bool,
    pub score_ready: bool,
    pub probe_disabled: bool,
    pub age_secs: u64,
}
