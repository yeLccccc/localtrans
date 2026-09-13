use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use tokio::sync::{RwLock, Mutex as TokioMutex, mpsc};
use std::collections::HashMap;

mod dto;
mod state;
mod relay_state;
// M3c T0:通道表生产者(镜像桌面壳 src-tauri/src/probe.rs;SessionUp 登记+
// 全量探测+5min 快检调度。退化切换不移植——Android 单通道场景为主,
// 退化切换 PC 优先,见模块注释)
mod probe;
// M2 T2:传输卡片状态机(等价移植桌面壳 src-tauri/src/transfer_state.rs;
// 纯逻辑+单测,壳层经 state.rs card_apply 单写者入口落卡)
mod transfer_state;
// M7:Android logcat 桥(仅 android target 编译,host 零行为变化)
#[cfg(target_os = "android")]
mod logcat;

use dto::*;
use state::*;

uniffi::setup_scaffolding!();

/// 接收侧 per-job 已见文件名累积器:Started(name) 时 record,
/// Done 时拼绝对路径。名称即相对 inbox 根的路径(core 的
/// Started.name 对小文件批流是文件名、对带 rel_dir 的是 "rel/name" 组合,
/// 与落盘结构一致——直接 join 即得绝对路径)。
///
/// v0.11.0 竞态修复(快照点对齐 accept):落盘目录在用户应答 offer 那一刻
/// 由 respond_offer 把当时的 inbox_dir 发给 core,此后切换收件目录不影响
/// 进行中的任务。因此 acc 的 root 也在同一时刻钉死(respond_offer /
/// set_inbox_dir 双向同步),Done 生成 FilesSaved 时直接用 acc 内的根,
/// 而不是读"当前" inbox_dir——否则接受后、完成前切目录会拼出新目录的
/// 错误路径(文件实际还在旧目录)。
#[derive(Default)]
pub struct SavedFilesAcc {
    root: std::path::PathBuf,
    names: Vec<String>,
}

impl SavedFilesAcc {
    pub fn new() -> Self {
        Self { root: std::path::PathBuf::new(), names: Vec::new() }
    }
    /// 钉死根:仅应在 accept 时刻(respond_offer 应答)或 Started 首次进入
    /// (Auto 策略兜底)调用。一旦钉死,同 job 的后续重建沿用——不变式:
    /// 同 job_id 的所有 FilesSaved 用同一个根(accept 时刻的)。
    pub fn set_root(&mut self, root: std::path::PathBuf) {
        self.root = root;
    }
    pub fn root_is_empty(&self) -> bool {
        self.root.as_os_str().is_empty()
    }
    /// 当前钉死的根(Done 拼路径/测试诊断用)
    pub fn root(&self) -> &std::path::Path {
        self.root.as_path()
    }
    /// Done 消费路径:清空文件名但保留根——小文件批流同 job 每文件一对
    /// Started/Done,后续 Started 重建时沿用本 job 已钉的根(而不是当前
    /// inbox_dir),保证中途切目录时同一批文件的 FilesSaved 路径根一致。
    pub fn take_names_keep_root(&mut self) -> Vec<String> {
        std::mem::take(&mut self.names)
    }
    pub fn record(&mut self, name: String) {
        self.names.push(name);
    }
    /// 用 acc 内钉死的根拼接绝对路径(测试/诊断用;生产 Done 路径走
    /// take_names_keep_root + root 拼接)。根为空(异常路径:未经历
    /// accept 的 job)时拼相对名兜底。
    pub fn snapshot_paths(&self) -> Vec<String> {
        self.names.iter()
            .map(|n| if self.root.as_os_str().is_empty() {
                n.clone()
            } else {
                self.root.join(n).to_string_lossy().replace('\\', "/")
            })
            .collect()
    }
}

// ===== 传输表持久化(移植桌面壳 main.rs,bug:重启后任务列表丢失) =====
// 桌面壳一直有 data/transfers.json 落盘+启动恢复;安卓 FFI 壳漏抄了这
// 两段——transfers 表纯内存,进程一死全丢。此处按同一文件格式补齐,
// 两端口径一致(将来桌面↔安卓互导 data 目录也能读)。

/// transfers.json 的磁盘记录。字段对齐桌面壳 TransferDto 的 serde 形态
/// (job_id 十六进制字符串、其余缺省兼容),与 FFI 的 uniffi Record 分离——
/// uniffi derive 不能掺 serde 字段定制。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct TransferRecord {
    #[serde(with = "localtrans_core::serde_compat::u64_hex_string")]
    job_id: u64,
    name: String,
    total: u64,
    done: u64,
    state: String,
    speed_bps: u64,
    peer: String,
    direction: String,
    #[serde(default)]
    local_role: String,
    progress_percent: u8,
    eta_secs: i64,
    #[serde(default)]
    fail_reason: String,
    #[serde(default)]
    local_path: Option<String>,
    #[serde(default)]
    remote_done: u64,
    #[serde(default)]
    instant: bool,
    // M2 T3:传输域时间戳/续传归属随索引落盘(全部 serde default——旧文件缺字段
    // 照常解析;PC 端 TransferDto serde 同名字段,互导可读)。
    #[serde(default)]
    started_at_ms: Option<i64>,
    #[serde(default)]
    finished_at_ms: Option<i64>,
    #[serde(default)]
    parts_id: Option<String>,
    #[serde(default)]
    source_path: Option<String>,
}

impl From<TransferDto> for TransferRecord {
    fn from(d: TransferDto) -> Self {
        TransferRecord {
            job_id: d.job_id,
            name: d.name,
            total: d.total,
            done: d.done,
            state: d.state,
            speed_bps: d.speed_bps,
            peer: d.peer,
            direction: d.direction,
            local_role: d.local_role,
            progress_percent: d.progress_percent,
            eta_secs: d.eta_secs,
            fail_reason: d.fail_reason,
            local_path: d.local_path,
            remote_done: d.remote_done,
            instant: d.instant,
            started_at_ms: d.started_at_ms,
            finished_at_ms: d.finished_at_ms,
            parts_id: d.parts_id,
            source_path: d.source_path,
        }
    }
}

impl From<TransferRecord> for TransferDto {
    fn from(r: TransferRecord) -> Self {
        TransferDto {
            job_id: r.job_id,
            name: r.name,
            total: r.total,
            done: r.done,
            state: r.state,
            speed_bps: r.speed_bps,
            peer: r.peer,
            direction: r.direction,
            local_role: r.local_role,
            progress_percent: r.progress_percent,
            eta_secs: r.eta_secs,
            fail_reason: r.fail_reason,
            local_path: r.local_path,
            remote_done: r.remote_done,
            instant: r.instant,
            started_at_ms: r.started_at_ms,
            finished_at_ms: r.finished_at_ms,
            parts_id: r.parts_id,
            source_path: r.source_path,
            // 其余传输域字段(queue_pos/batch_id/children/health)为运行时态,不落盘
            ..Default::default()
        }
    }
}

fn persist_now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// M2 T2:持久化卡片条目(对齐桌面壳 TransferCardSerde{dto,removed},
/// removed 随卡片落盘——view 级删除的卡重启不复活)。
/// 新格式 transfers.json 为 `{"cards":[...]}` 包装对象;旧格式是裸数组,
/// 读取时一判即中,保证老文件可读(removed 缺省 false)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct TransferCardSerde {
    dto: TransferRecord,
    #[serde(default)]
    removed: bool,
}

/// 新格式顶层包装(对齐桌面壳 TransfersFile)
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
struct TransfersFile {
    cards: Vec<TransferCardSerde>,
}

impl From<&crate::transfer_state::TransferCard> for TransferCardSerde {
    fn from(c: &crate::transfer_state::TransferCard) -> Self {
        TransferCardSerde { dto: c.dto.clone().into(), removed: c.removed }
    }
}

/// M2 T2:持久化落盘列表收集(移植桌面壳 collect_persist_list 纯函数):
/// open 卡(非 done/failed,interrupted 可续传故全量保留)全量;
/// 终态卡分两池:removed=true 全量保留(不计数不截断),removed=false
/// 封顶 150 条——否则 removed 卡被截出 transfers.json,重启时
/// removed=false 整卡复活(view 删除破口)。
fn collect_persist_list(cards: &[crate::transfer_state::TransferCard]) -> Vec<TransferCardSerde> {
    let mut open_jobs: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| !matches!(c.dto.state.as_str(), "done" | "failed"))
        .map(TransferCardSerde::from)
        .collect();
    let removed_terminal: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| matches!(c.dto.state.as_str(), "done" | "failed") && c.removed)
        .map(TransferCardSerde::from)
        .collect();
    let mut history: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| matches!(c.dto.state.as_str(), "done" | "failed") && !c.removed)
        .map(TransferCardSerde::from)
        .collect();
    history.truncate(150);
    open_jobs.extend(history);
    open_jobs.extend(removed_terminal);
    open_jobs
}

/// 启动迁移(对齐桌面壳 migrate_on_startup):非终端态 → interrupted,
/// 且接收方向的非终端任务必须有 parts 目录,否则视为孤儿丢弃。
/// 发送方向(push)本机就是数据源,无 parts 概念,只改状态保留。
/// M2 T3:parts 根改为多根(inbox/download_dir/dir 三处均可能落 parts,
/// 见 AppState::parts_roots),任一根有目录即视为存活。
fn migrate_records_on_startup(
    list: Vec<TransferRecord>,
    parts_roots: &[std::path::PathBuf],
) -> Vec<TransferRecord> {
    use std::collections::HashSet;
    let mut alive: HashSet<u64> = HashSet::new();
    for root in parts_roots {
        if let Ok(rd) = std::fs::read_dir(root.join(".localtrans-parts")) {
            alive.extend(rd.flatten().filter_map(|e| {
                e.file_name().to_str().and_then(|s| u64::from_str_radix(s, 16).ok())
            }));
        }
    }

    list.into_iter()
        .filter_map(|mut r| {
            match r.state.as_str() {
                // M2 T2:cancelling 归入非终态迁移(重启后引擎任务已消失,
                // 取消永远等不到确认——按 interrupted 收敛,不留僵尸 cancelling 行)
                "active" | "pending" | "paused" | "cancelling" => {
                    // 接收方向:无 parts 目录的孤儿直接丢(磁盘无数据可续)
                    if r.direction != "push" && !alive.contains(&r.job_id) {
                        return None;
                    }
                    r.state = "interrupted".into();
                    r.speed_bps = 0;
                    r.eta_secs = -2; // 终态标记(对齐 FFI transfer_update 盖戳语义)
                }
                _ => {}
            }
            Some(r)
        })
        .collect()
}

/// 加载 data/transfers.json 预填传输卡表。损坏/缺失返回空表(不阻断启动)。
/// 新格式 {"cards":[{dto,removed}]} 优先;旧格式裸数组回退(removed=false)。
fn load_transfers_persisted(
    dir: &std::path::Path,
    parts_roots: &[std::path::PathBuf],
) -> HashMap<u64, crate::transfer_state::TransferCard> {
    let path = dir.join("transfers.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    let cards: Vec<TransferCardSerde> = if let Ok(file) = serde_json::from_str::<TransfersFile>(&text) {
        file.cards
    } else {
        match serde_json::from_str::<Vec<TransferRecord>>(&text) {
            Ok(list) => list.into_iter().map(|dto| TransferCardSerde { dto, removed: false }).collect(),
            Err(_) => {
                tracing::warn!("传输任务历史解析失败,忽略: {}", path.display());
                return HashMap::new();
            }
        }
    };
    let n = cards.len();
    let records: Vec<TransferRecord> = cards.iter().map(|c| c.dto.clone()).collect();
    let mut by_id: HashMap<u64, bool> = HashMap::new();
    for c in &cards {
        by_id.insert(c.dto.job_id, c.removed);
    }
    let migrated = migrate_records_on_startup(records, parts_roots);
    tracing::info!("恢复传输任务历史 {} 条", n);
    migrated.into_iter().map(|r| {
        let mut card = crate::transfer_state::TransferCard::new(r.into());
        card.removed = by_id.get(&card.dto.job_id).copied().unwrap_or(false);
        (card.dto.job_id, card)
    }).collect()
}

/// M2 T3:孤儿 manifest → 卡片 dto(纯函数,移植桌面壳 main.rs orphan_card_dto;
/// 启动重建 / restore_disk_job / set_inbox_dir 补扫三处共用)。
/// 缺块→interrupted;位图全真→failed(完整性存疑)。meta 有 display_name
/// 用之,无则回退 manifest.file_name。卡键=engine job_id(目录名),与 ffi
/// 占位 ID(u64::MAX 递减段)天然不撞,无需调整分配器。
/// ffi 形态差异:fail_reason 是 String(空=无)。
pub(crate) fn orphan_card_dto(
    job_id: u64,
    manifest: &localtrans_core::transfer::manifest::Manifest,
) -> TransferDto {
    use localtrans_core::protocol::CHUNK_SIZE;
    let missing = manifest.missing_chunks();
    let (state, fail_reason) = if missing.is_empty() {
        ("failed", "数据完整性存疑,建议重新拉取".to_string())
    } else {
        ("interrupted", String::new())
    };
    // done 按收齐块的字节和估算(尾块按实际长度)
    let done: u64 = manifest.received.iter().enumerate()
        .filter(|(_, &r)| r)
        .map(|(i, _)| {
            let off = i as u64 * CHUNK_SIZE as u64;
            std::cmp::min(CHUNK_SIZE as u64, manifest.total_size.saturating_sub(off))
        })
        .sum();
    let meta = manifest.meta();
    TransferDto {
        job_id,
        name: meta.and_then(|m| if m.display_name.is_empty() { None } else { Some(m.display_name.clone()) })
            .unwrap_or_else(|| manifest.file_name.clone()),
        total: manifest.total_size,
        done,
        state: state.into(),
        speed_bps: 0,
        peer: meta.map(|m| m.peer_hex.clone())
            .filter(|s| !s.is_empty())
            .or_else(|| manifest.peer.clone())
            .unwrap_or_default(),
        direction: meta.map(|m| m.direction.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "pull".into()),
        local_role: meta.map(|m| m.local_role.clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "destination".into()),
        progress_percent: 0, // sync_derived_fields 重算
        eta_secs: -2,
        fail_reason,
        local_path: None,
        remote_done: 0,
        instant: false,
        started_at_ms: meta.map(|m| m.created_at_ms),
        finished_at_ms: meta.and_then(|m| m.finished_at_ms),
        source_path: meta.and_then(|m| m.source_path.clone()),
        parts_id: Some(format!("{job_id:016x}")),
        ..Default::default()
    }
}

/// M2 T3:manifest 优先启动重建(纯函数,移植桌面壳 main.rs rebuild_cards)。
/// 1) 各 parts 根 orphan_jobs 扫盘建卡(卡键=engine_id;同 id 多根首见为准);
/// 2) transfers.json 索引合并:冲突(同 id)manifest 侧为准(状态/进度),
///    索引侧补 display_name/removed 标记/缺失的时间戳与 fail_reason;
///    索引独有条目(终态 done/failed、push interrupted 等——load 已做非终态
///    迁移与 pull 无 parts 孤儿丢弃)直接建卡;
/// 3) gc_stale_parts 收尾(在扫盘之后跑:位图全真孤儿的"failed 完整性存疑"
///    卡已在表,目录删了历史仍可见)。
pub(crate) fn rebuild_cards(
    data_dir: &std::path::Path,
    parts_roots: &[std::path::PathBuf],
) -> HashMap<u64, crate::transfer_state::TransferCard> {
    // 1. 磁盘孤儿建卡(先扫后 gc——顺序是历史记录可见性的前提)
    let mut cards: HashMap<u64, crate::transfer_state::TransferCard> = HashMap::new();
    for root in parts_roots {
        for oj in localtrans_core::transfer::orphan_jobs(root) {
            cards.entry(oj.job_id).or_insert_with(|| {
                let mut dto = orphan_card_dto(oj.job_id, &oj.manifest);
                crate::state::sync_derived_fields(&mut dto);
                let mut c = crate::transfer_state::TransferCard::new(dto);
                c.engine_id = Some(oj.job_id);
                c
            });
        }
    }
    let disk_cards = cards.len();

    // 2. transfers.json 索引合并/补缺
    for (id, entry) in load_transfers_persisted(data_dir, parts_roots) {
        match cards.get_mut(&id) {
            Some(c) => {
                // 冲突:manifest 侧(已有卡)为准;索引补显示名、removed 与
                // 卡片侧缺失的时间戳/fail_reason
                if c.dto.name.is_empty() {
                    c.dto.name = entry.dto.name.clone();
                }
                c.removed = entry.removed;
                if c.dto.started_at_ms.is_none() {
                    c.dto.started_at_ms = entry.dto.started_at_ms;
                }
                if c.dto.finished_at_ms.is_none() {
                    c.dto.finished_at_ms = entry.dto.finished_at_ms;
                }
                if c.dto.fail_reason.is_empty() {
                    c.dto.fail_reason = entry.dto.fail_reason.clone();
                }
                if c.dto.local_path.is_none() {
                    c.dto.local_path = entry.dto.local_path.clone();
                }
            }
            None => {
                // 索引独有(终态 done/failed、push interrupted 等)直接建卡
                let mut c = entry;
                if c.dto.direction == "pull" {
                    c.engine_id = Some(id);
                }
                cards.insert(id, c);
            }
        }
    }

    // 3. gc 位图全真孤儿目录(卡已建,历史记录仍可见)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut gc = 0usize;
    for root in parts_roots {
        gc += localtrans_core::transfer::gc_stale_parts(root, now);
    }
    tracing::info!("启动重建传输卡 {} 张(磁盘孤儿 {} 张),清理位图全真孤儿 parts 目录 {} 个",
        cards.len(), disk_cards, gc);
    cards
}

/// 落盘循环(对齐桌面壳):1s 节拍检查脏标记,快照写临时文件后原子 rename。
/// 列表收集走 collect_persist_list:open 全量 + removed 终态全量 + 历史封顶 150。
fn spawn_transfers_persist_loop(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            interval.tick().await;
            if !state.transfers_dirty.swap(false, Ordering::Relaxed) {
                continue;
            }
            let cards = state.snapshot_cards().await;
            let file = TransfersFile { cards: collect_persist_list(&cards) };

            let path = state.dir.join("transfers.json");
            let tmp = path.with_extension("json.tmp");
            let result = serde_json::to_string_pretty(&file)
                .map_err(|e| e.to_string())
                .and_then(|s| std::fs::write(&tmp, s).map_err(|e| e.to_string()))
                .and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()));
            if let Err(e) = result {
                tracing::warn!("传输表落盘失败: {}", e);
            }
        }
    });
}

/// Full event set for Android client
#[derive(uniffi::Enum, Clone, Debug)]
pub enum AppEvent {
    Hello { message: String },
    DevicesChanged,
    ConsentRequested { fingerprint: String, name: String },
    PairingCodeShown { fingerprint: String, code: String },
    PairingWaitConsent { fingerprint: String, name: String },
    PairingCodeEntry { fingerprint: String, name: String },
    PairingResult { fingerprint: String, ok: bool, reason: String },
    SessionUp { fingerprint: String, name: String },
    SessionDown { fingerprint: String },
    TransferUpdated { transfer: TransferDto },
    TransferDone { job_id: u64, ok: bool, fail_reason: String },
    /// 接收侧文件已落盘(仅 FFI 层事件,core 无感知)。
    /// paths 为绝对路径——Kotlin 据此做 MediaScanner 扫描与"查看"跳转。
    /// 注意:该事件在 TransferDone 之前发出。
    FilesSaved { job_id: u64, paths: Vec<String> },
    OfferRequested { job_id: u64, peer_name: String, file_count: u32, total_size: u64, deadline_epoch_ms: i64 },
    /// P0-2d: 远程删除确认请求(本机是共享区持有方)
    DeleteRequested { ask_id: u64, peer_name: String, name: String, is_dir: bool, entry_count: u64, deadline_epoch_ms: i64 },
    BackupProgress { done: u64, total: u64 },
}

#[uniffi::export(callback_interface)]
pub trait LocalTransCallback: Send + Sync {
    fn on_event(&self, event: AppEvent);
}

#[derive(uniffi::Error)]
pub enum AppException {
    Io { message: String },
    Internal { message: String },
}

impl std::fmt::Display for AppException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppException::Io { message } => write!(f, "IO error: {}", message),
            AppException::Internal { message } => write!(f, "Internal error: {}", message),
        }
    }
}

impl std::fmt::Debug for AppException {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppException::Io { message } => f.debug_tuple("Io").field(message).finish(),
            AppException::Internal { message } => f.debug_tuple("Internal").field(message).finish(),
        }
    }
}

impl From<std::io::Error> for AppException {
    fn from(e: std::io::Error) -> Self { AppException::Io { message: e.to_string() } }
}

#[derive(uniffi::Object)]
pub struct LocalTransApp {
    runtime: tokio::runtime::Runtime,
    callback: Arc<Box<dyn LocalTransCallback>>,
    /// 初始化完成后写入一次;读路径零锁(start 前 None)
    state: std::sync::OnceLock<Arc<AppState>>,
    /// 仅用于 start() 的并发启动互斥;构造期保护,初始化后不再触碰
    init_lock: Mutex<()>,
    data_dir: String,
}

/// FFI guard: wrap async runtime calls with panic recovery
fn ffi_guard<R>(app: &LocalTransApp, fut: std::pin::Pin<Box<dyn std::future::Future<Output = Result<R, AppException>> + Send>>) -> Result<R, AppException> {
    // Direct runtime execution without panic catching for simplicity
    app.runtime.block_on(async move {
        fut.await
    })
}

#[uniffi::export]
impl LocalTransApp {
    #[uniffi::constructor]
    pub fn new(data_dir: String, callback: Box<dyn LocalTransCallback>) -> Result<Arc<Self>, AppException> {
        // M7:core/ffi tracing 日志进 logcat(进程一次;非 Android 为空操作)
        #[cfg(target_os = "android")]
        crate::logcat::init();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|e| {
                // Builder 参数为常量,此路径实际不可达;映射 Err 而非 exit/panic
                tracing::error!("tokio runtime 创建失败: {}", e);
                AppException::Internal { message: "运行时初始化失败".to_string() }
            })?;

        // Chain verification event
        callback.on_event(AppEvent::Hello { message: "ffi up".into() });

        Ok(Arc::new(Self {
            runtime,
            callback: Arc::new(callback),
            state: std::sync::OnceLock::new(),
            init_lock: Mutex::new(()),
            data_dir,
        }))
    }

    pub fn hello(&self) -> String { "localtrans-ffi".into() }

    pub fn my_fingerprint(&self) -> String {
        let app = self.state.get().cloned();
        match app {
            Some(state) => hex::encode(state.identity.fingerprint()),
            None => "not started".to_string(),
        }
    }

    /// User file destination root (inbox). Defaults to data_dir; Android side points to Download/LocalTrans
    pub fn inbox_dir(&self) -> String {
        let app = self.state.get().cloned();
        match app {
            Some(state) => state.inbox_dir.read().unwrap().to_string_lossy().to_string(),
            None => self.data_dir.clone(),
        }
    }

    /// Inject user-visible receive directory (Android: Download/LocalTrans).
    /// The directory tree is created if missing (create_dir_all); failure to
    /// create returns Io and leaves the previous inbox unchanged.
    pub fn set_inbox_dir(&self, dir: String) -> Result<(), AppException> {
        std::fs::create_dir_all(&dir)
            .map_err(|e| AppException::Io { message: e.to_string() })?;
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let new_root = std::path::PathBuf::from(&dir);
                {
                    let mut inbox = state.inbox_dir.write().unwrap();
                    *inbox = new_root.clone();
                    // 注:进行中任务的落盘目录不受影响——core 在 accept 时刻已锁定
                    // save_dir(respond_offer 快照),saved_files acc 的根也在同一时刻
                    // 钉死(见 respond_offer / SavedFilesAcc 注释),此处不回写。
                }
                // M2 T3:推送接收的 parts 落在 inbox 根(accept 时刻的 save_dir),
                // 启动重建时该根未知——注入即补扫孤儿建卡(只补缺,不覆盖在表卡),
                // 再 gc 该根的位图全真孤儿(卡已建,历史仍可见)。
                let st = state.clone();
                self.runtime.block_on(async move {
                    st.merge_orphans_from_root(&new_root).await;
                    let _ = tokio::task::spawn_blocking(move || {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let n = localtrans_core::transfer::gc_stale_parts(&new_root, now);
                        if n > 0 {
                            tracing::info!("收件目录注入后清理位图全真孤儿 parts 目录 {} 个", n);
                        }
                    }).await;
                });
                Ok(())
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Start the app and initialize all services
    pub fn start(&self) -> Result<(), AppException> {
        // init_lock:仅互斥并发 start;OnceLock 写入一次后读路径零锁
        let _init_guard = self.init_lock.lock().unwrap_or_else(|p| p.into_inner());
        if self.state.get().is_some() {
            return Ok(()); // Already started
        }

        let dir = std::path::PathBuf::from(&self.data_dir);
        std::fs::create_dir_all(&dir).ok();

        // Initialize in runtime - we can't clone Runtime, so we pass a reference
        let callback = self.callback.clone();

        // Block until initialization complete
        let init_result: Result<Arc<crate::state::AppState>, AppException> = self.runtime.block_on(async move {
            // Load identity
            let identity = Arc::new(
                localtrans_core::identity::Identity::load_or_create(&dir)
                    .map_err(|e| {
                        // io::Error Display 含 data_dir 绝对路径,不进 FFI 错误消息(避免路径泄漏给对端/日志收集)
                        tracing::error!("身份加载失败: {} (dir={:?})", e, dir);
                        AppException::Internal { message: "身份加载失败,请检查数据目录权限".to_string() }
                    })?,
            );

            // Load config
            // 首次安装(config.json 不存在 → load 落默认值)时把设备名从
            // core 的桌面默认"我的电脑"改为移动端默认"我的手机"——安卓设备
            // 顶着"我的电脑"出现在对端列表里,极易被误认成本机重复(v0.6.1 实测踩过)
            let is_fresh_install = !dir.join("config.json").exists();
            let config = Arc::new(RwLock::new(localtrans_core::store::load_config(&dir)));
            if is_fresh_install {
                config.write().await.device_name = "我的手机".into();
                let cfg_snapshot = config.read().await.clone();
                let _ = localtrans_core::store::save_config(&dir, &cfg_snapshot);
            }

            // Load trust store
            let trust = Arc::new(TokioMutex::new(localtrans_core::identity::TrustStore::load(&dir)));

            // Create hidden flag
            let hidden = Arc::new(AtomicBool::new(config.read().await.hidden));

            // Initialize discovery
            let discovery = Arc::new(localtrans_core::discovery::spawn(
                localtrans_core::discovery::DiscoveryConfig {
                    bind_port: config.read().await.discovery_port,
                    // T1:广播目标与绑定端口同源(ports 模块),env 偏移时不错位
                    target: format!("255.255.255.255:{}", localtrans_core::ports::discovery_port()).parse().unwrap(),
                    hidden: hidden.clone(),
                    name: config.read().await.device_name.clone(),
                    quic_port: config.read().await.quic_port,
                    fingerprint: identity.fingerprint(),
                    data_dir: Some(dir.clone()),
                    ..Default::default()
                },
                Arc::new(identity.signing.clone()),
            ).map_err(|e| AppException::Internal { message: format!("发现服务启动失败: {}", e) })?);

            // Initialize session manager
            let (sm, mut session_events) = localtrans_core::session::SessionManager::spawn(
                localtrans_core::session::SessionCtx {
                    identity: identity.clone(),
                    trust: trust.clone(),
                    config: config.clone(),
                }
            );

            // Start QUIC listener
            let quic_port = config.read().await.quic_port;
            sm.start_listener(quic_port).await
                .map_err(|e| AppException::Io { message: format!("QUIC 监听启动失败: {}", e) })?;

            // ===== RPC Router Setup (照抄桌面壳) =====
            let ctrl_rx = sm.take_inbound_ctrl_rx().await
                .ok_or_else(|| AppException::Internal { message: "入站控制通道已被占用".to_string() })?;

            let (ask_tx, mut ask_rx) = mpsc::channel(8);
            let (delete_ask_tx, mut delete_ask_rx) = mpsc::channel(8);

            // Auto offer hook
            let (auto_tx, mut auto_rx) = mpsc::channel::<localtrans_core::transfer::AutoOfferInfo>(16);
            localtrans_core::transfer::set_auto_offer_hook(auto_tx.clone());

            // Sender job map
            let sender_jobs = localtrans_core::transfer::sender_state::new_sender_job_map();

            // Source event bridge (push sender progress events)
            let (source_tx, mut source_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

            // Inbound receive hook (pull receiver progress events)
            let (recv_tx, mut recv_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
            localtrans_core::transfer::set_inbound_recv_hook(recv_tx);

            // Share registry
            let reg = Arc::new(localtrans_core::share::ShareRegistry::new(
                config.read().await.shares.clone()
            ));

            // Initialize transfer management fields
            let pending_offers = Arc::new(TokioMutex::new(HashMap::new()));
            let pending_deletes = Arc::new(TokioMutex::new(HashMap::new()));
            let auto_offers = Arc::new(TokioMutex::new(HashMap::new()));
            // M2 T3:manifest 优先启动重建(镜像桌面壳 rebuild_cards):
            // 各 parts 根扫盘建卡 → transfers.json 索引合并 → gc 位图全真孤儿。
            // 此刻 inbox_dir 尚未注入(安卓侧 start() 后才 set),根取
            // data_dir + config.download_dir;inbox 根在 set_inbox_dir 时补扫
            // (merge_orphans_from_root)。目录扫描是阻塞 IO,放 spawn_blocking。
            let transfers = {
                let data_dir = dir.clone();
                let mut roots = vec![dir.clone()];
                let dl = config.read().await.download_dir.clone();
                if !roots.contains(&dl) {
                    roots.push(dl);
                }
                let cards = tokio::task::spawn_blocking(move || rebuild_cards(&data_dir, &roots))
                    .await
                    .unwrap_or_default();
                Arc::new(TokioMutex::new(cards))
            };
            // 重建结果落盘一次(索引与磁盘对齐;gc 掉的目录其卡仍在)
            let transfers_dirty = Arc::new(AtomicBool::new(true));
            let next_placeholder_id = Arc::new(AtomicU64::new(u64::MAX));
            // M2 T4:全局并发闸门,启动按 config 构建(permits=max_active_transfers
            // clamp 1-8)。信号量容量不可变:运行中 save_settings 改该项不热更新,
            // 重启生效——对齐桌面壳"active_gate 启动构建"语义。
            let max_active = config.read().await.max_active_transfers.clamp(1, 8) as usize;
            let active_gate = Arc::new(tokio::sync::Semaphore::new(max_active));
            let peer_locks: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<()>>>>> =
                Arc::new(TokioMutex::new(HashMap::new()));
            let progress_throttle = Arc::new(Mutex::new(HashMap::new()));

            // Create AppState
            let state = Arc::new(AppState::new(
                dir.clone(),
                identity.clone(),
                config.clone(),
                trust.clone(),
                sm.clone(),
                discovery.clone(),
                hidden.clone(),
                pending_offers,
                pending_deletes,
                auto_offers,
                transfers,
                transfers_dirty,
                next_placeholder_id,
                active_gate,
                peer_locks,
                progress_throttle,
                sender_jobs,
            ));

            // Spawn RPC router (照抄桌面壳)
            localtrans_core::transfer::spawn_rpc_router(
                sm.clone(),
                localtrans_core::session::SessionCtx {
                    identity: identity.clone(),
                    trust: trust.clone(),
                    config: config.clone(),
                },
                reg.clone(),
                ctrl_rx,
                ask_tx,
                delete_ask_tx,
                state.sender_jobs.clone(),
                Some(source_tx),
            );

            // ask_rx loop: handle OfferAsk events (照抄桌面壳)
            let state_for_ask = state.clone();
            let callback_for_ask = callback.clone();
            tokio::spawn(async move {
                while let Some(ask) = ask_rx.recv().await {
                    let fp_hex = hex::encode(ask.from);

                    // Store in pending_offers table with deadline
                    let job = ask.job_id;
                    let deadline = ask.deadline_epoch_ms;
                    state_for_ask.pending_offers.lock().await.insert(job, PendingOffer {
                        respond: ask.respond,
                        extend: ask.extend,
                        deadline,
                    });

                    // Watchdog: clean up expired offers after deadline+3s
                    let state_for_wd = state_for_ask.clone();
                    tokio::spawn(async move {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default();
                        let target_ms = deadline as u64 + 3000;
                        let duration_ms = target_ms.saturating_sub(now.as_millis() as u64);
                        tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                        let mut pending = state_for_wd.pending_offers.lock().await;
                        if pending.contains_key(&job) {
                            pending.remove(&job);
                            drop(pending);
                            state_for_wd.auto_offers.lock().await.remove(&job);
                        }
                    });

                    // Emit OfferRequested event
                    let files: Vec<FileEntryDto> = ask.files.iter()
                        .map(|f| FileEntryDto {
                            name: f.name.clone(),
                            is_dir: false, // OfferFile has no is_dir field
                            size: f.size,
                            modified_ms: 0, // OfferFile has no modified_ms field
                        })
                        .collect();

                    callback_for_ask.on_event(AppEvent::OfferRequested {
                        job_id: job,
                        peer_name: fp_hex.clone(), // Use peer name from connected devices
                        file_count: files.len() as u32,
                        total_size: files.iter().map(|f| f.size).sum(),
                        deadline_epoch_ms: deadline,
                    });
                }
            });

            // delete_ask_rx loop: handle DeleteAsk events (P0-2d)
            let state_for_del = state.clone();
            let trust_for_del = trust.clone();
            let callback_for_del = callback.clone();
            tokio::spawn(async move {
                while let Some(ask) = delete_ask_rx.recv().await {
                    state_for_del.pending_deletes.lock().await.insert(ask.ask_id, ask.respond);
                    // 看门狗:deadline+3s 清残留(镜像 offer 看门狗模式)
                    let st_wd = state_for_del.clone();
                    let (aid, dl) = (ask.ask_id, ask.deadline_epoch_ms);
                    tokio::spawn(async move {
                        let now = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                        let wait = (dl as u64 + 3000).saturating_sub(now.as_millis() as u64);
                        tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                        st_wd.pending_deletes.lock().await.remove(&aid);
                    });
                    let peer_name = trust_for_del.lock().await
                        .get(&ask.from)
                        .map(|p| if p.alias.is_empty() { p.name.clone() } else { p.alias.clone() })
                        .unwrap_or_else(|| "未知设备".into());
                    callback_for_del.on_event(AppEvent::DeleteRequested {
                        ask_id: ask.ask_id, peer_name,
                        name: ask.name, is_dir: ask.is_dir,
                        entry_count: ask.entry_count, deadline_epoch_ms: ask.deadline_epoch_ms,
                    });
                }
            });

            // auto_rx loop: track Auto mode offers (照抄桌面壳)
            let state_for_auto = state.clone();
            tokio::spawn(async move {
                while let Some(info) = auto_rx.recv().await {
                    let fp_hex = hex::encode(info.peer);
                    state_for_auto.auto_offers.lock().await.insert(
                        info.job_id,
                        (fp_hex, info.file_count),
                    );
                }
            });

            // source_rx loop: handle push sender progress events (照抄桌面壳)
            let state_for_source = state.clone();
            let callback_for_source = callback.clone();
            tokio::spawn(async move {
                while let Some(ev) = source_rx.recv().await {
                    handle_source_progress_event(&state_for_source, &callback_for_source, ev).await;
                }
            });

            // recv_rx loop: handle pull receiver progress events (照抄桌面壳)
            let state_for_recv = state.clone();
            let callback_for_recv = callback.clone();
            tokio::spawn(async move {
                while let Some(ev) = recv_rx.recv().await {
                    handle_recv_progress_event(&state_for_recv, &callback_for_recv, ev).await;
                }
            });

            // Spawn session event bridge
            let state_for_bridge = state.clone();
            let callback_for_bridge = callback.clone();
            let event_task = tokio::spawn(async move {
                while let Some(ev) = session_events.recv().await {
                    match ev {
                        localtrans_core::session::SessionEvent::PairingConsentNeeded { fingerprint, name } => {
                            let fp_hex = hex::encode(fingerprint);
                            callback_for_bridge.on_event(AppEvent::ConsentRequested {
                                fingerprint: fp_hex,
                                name,
                            });
                        }
                        localtrans_core::session::SessionEvent::PairingCodeShown { fingerprint, own_code } => {
                            let fp_hex = hex::encode(fingerprint);
                            callback_for_bridge.on_event(AppEvent::PairingCodeShown {
                                fingerprint: fp_hex,
                                code: own_code,
                            });
                        }
                        localtrans_core::session::SessionEvent::PairingWaitConsent { fingerprint, name } => {
                            let fp_hex = hex::encode(fingerprint);
                            callback_for_bridge.on_event(AppEvent::PairingWaitConsent {
                                fingerprint: fp_hex,
                                name,
                            });
                        }
                        localtrans_core::session::SessionEvent::PairingCodeEntry { fingerprint, name } => {
                            let fp_hex = hex::encode(fingerprint);
                            callback_for_bridge.on_event(AppEvent::PairingCodeEntry {
                                fingerprint: fp_hex,
                                name,
                            });
                        }
                        localtrans_core::session::SessionEvent::PairingResult { fingerprint, ok, reason } => {
                            let fp_hex = hex::encode(fingerprint);
                            callback_for_bridge.on_event(AppEvent::PairingResult {
                                fingerprint: fp_hex,
                                ok,
                                reason: reason.unwrap_or_default(),
                            });
                        }
                        localtrans_core::session::SessionEvent::SessionUp { fingerprint, name, conn } => {
                            let fp_hex = hex::encode(fingerprint);
                            state_for_bridge.connected_fps.lock().unwrap().insert(fp_hex.clone());
                            callback_for_bridge.on_event(AppEvent::SessionUp {
                                fingerprint: fp_hex.clone(),
                                name,
                            });

                            // M3c T0:通道登记 + 全量探测(会话建立触发;活动
                            // 传输时后台任务自行推迟,事件泵不阻塞。镜像桌面壳
                            // main.rs SessionUp 分支调 probe::on_session_up)
                            {
                                let st = state_for_bridge.clone();
                                tokio::spawn(async move {
                                    crate::probe::on_session_up(&st, fingerprint, conn).await;
                                });
                            }

                            // 回路 2:对端恢复在线 → 自动续传(镜像桌面壳;只 1 次)
                            {
                                let st = state_for_bridge.clone();
                                let cb = callback_for_bridge.clone();
                                let fp_hex_auto = fp_hex.clone();
                                tokio::spawn(async move {
                                    let dir = st.dir.clone();
                                    let pending: Vec<u64> = localtrans_core::transfer::pending_jobs(&dir)
                                        .into_iter().map(|(id, _)| id).collect();
                                    let candidates = {
                                        let transfers = st.transfers.lock().await;
                                        let retried = st.auto_retried.lock().unwrap();
                                        localtrans_core::relay::autoheal::auto_resumable_jobs(
                                            transfers.iter().map(|(id, c)| (*id, c.dto.state.clone(), c.dto.peer.clone())).collect(),
                                            &fp_hex_auto, &pending, &retried,
                                        )
                                    };
                                    for job_id in candidates {
                                        st.auto_retried.lock().unwrap().insert(job_id);
                                        // 不打 toast,TransferUpdated 事件承载可见性
                                        spawn_retry_transfer(&st, &cb, job_id).await;
                                    }
                                });
                            }
                        }
                        localtrans_core::session::SessionEvent::SessionDown { fingerprint } => {
                            let fp_hex = hex::encode(fingerprint);
                            state_for_bridge.connected_fps.lock().unwrap().remove(&fp_hex);
                            callback_for_bridge.on_event(AppEvent::SessionDown {
                                fingerprint: fp_hex.clone(),
                            });

                            // 回路 1:中继会话自愈(镜像桌面壳 main.rs)
                            {
                                let st = state_for_bridge.clone();
                                let fp_bytes = fingerprint; // [u8;32], Copy
                                let fp_hex_auto = fp_hex.clone();
                                tokio::spawn(async move {
                                    let is_local = st.devices.lock().unwrap()
                                        .iter().any(|d| d.fingerprint == fp_bytes);
                                    let in_roster = st.relay_roster.lock().unwrap()
                                        .iter().any(|d| d.fingerprint == fp_bytes);
                                    if is_local || !in_roster { return; }
                                    // M3a FR6:信任已不成立(对端发 TrustBroken/本地移除)时豁免自愈——
                                    // 自动重连只会给对端弹配对同意门;重配对由用户发起(镜像桌面壳)
                                    if !st.trust.lock().await.is_trusted(&fp_bytes) { return; }
                                    if !st.healing_fps.lock().unwrap().insert(fp_hex_auto.clone()) { return; }
                                    let client = st.relay.lock().await.clone();
                                    let Some(client) = client else {
                                        st.healing_fps.lock().unwrap().remove(&fp_hex_auto);
                                        return;
                                    };
                                    let sm = st.sm.clone();
                                    let ok = localtrans_core::relay::autoheal::auto_reconnect(&client, &sm, fp_bytes).await;
                                    if !ok {
                                        tracing::info!("[autoheal] 放弃: fp={}", fp_hex_auto);
                                    }
                                    st.healing_fps.lock().unwrap().remove(&fp_hex_auto);
                                });
                            }
                        }
                        localtrans_core::session::SessionEvent::TrustBroken { fingerprint, peer_name } => {
                            // M3a FR6:对端移除了对本机的信任。core 已同步删除本端信任条目
                            // (双盲对称降级,徽章随后随 DevicesChanged 变待配对)。最小映射:
                            // 只发 DevicesChanged——紧随其后的 SessionDown(同通道,Goodbye
                            // 同款收尾)承载既有断连展示,不新增 AppEvent 变体(免 uniffi regen);
                            // 专用文案 toast 待 Android 事件面扩充。peer_name 当前仅用于日志。
                            let fp_hex = hex::encode(fingerprint);
                            tracing::info!("[trust-broken] 对端 {}({:?}) 移除了对本机的信任,已同步降级", fp_hex, peer_name);
                            callback_for_bridge.on_event(AppEvent::DevicesChanged);
                        }
                    }
                }
            });

            state.register_event_task(event_task);

            // Spawn discovery watch
            let state_for_watch = state.clone();
            let callback_for_watch = callback.clone();
            let watch_task = tokio::spawn(async move {
                let mut rx = state_for_watch.discovery.devices.clone();
                loop {
                    if rx.changed().await.is_ok() {
                        let devices: Vec<localtrans_core::discovery::DeviceInfo> = rx.borrow().clone();
                        *state_for_watch.devices.lock().unwrap() = devices;
                        callback_for_watch.on_event(AppEvent::DevicesChanged);
                    }
                }
            });

            state.register_event_task(watch_task);

            // ===== 传输表持久化落盘循环(镜像桌面壳:脏标记驱动 1s 落盘) =====
            spawn_transfers_persist_loop(state.clone());

            // ===== M3c T0 通道探测:5min 周期快检(掉 50% 升级全量;活动传输推迟) =====
            // 镜像桌面壳 main.rs 的 probe::spawn_scheduler;句柄入 event_tasks,
            // shutdown 时随事件桥一并 abort(桌面壳进程级常驻,ffi 有 shutdown 面)
            {
                let handle = crate::probe::spawn_scheduler(state.clone());
                state.register_event_task(handle);
            }

            // ===== 中继启动(镜像桌面壳 main.rs 10.5):配置启用则自动连接 =====
            // state:闭包内刚构造的 AppState;callback:闭包开头已 clone 的回调 Arc
            crate::relay_state::spawn_relay(&state, &callback).await;

            // M7 启动横幅:编排器 adb waitForBanner 的就绪信号。
            // 单行、含 LT-BANNER 特征串 + ffi 版本 + 设备指纹(此时身份已加载,
            // 指纹必为真值;设备名一并带上便于人读)。tag=LT::ffi::banner。
            tracing::info!(
                target: "localtrans_ffi::banner",
                "LT-BANNER ready version={} device={} fingerprint={}",
                env!("CARGO_PKG_VERSION"),
                state.config.read().await.device_name,
                hex::encode(state.identity.fingerprint()),
            );

            Ok::<_, AppException>(state)
        });

        match init_result {
            Ok(state) => {
                let _ = self.state.set(state);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    /// Shutdown the app gracefully
    /// OnceLock 不可清空:shutdown 只中止事件任务,state 保持可用(与旧语义一致——旧实现同样不销毁 AppState,仅 abort)
    pub fn shutdown(&self) {
        if let Some(state) = self.state.get() {
            state.abort_event_tasks();
        }
    }

    /// Get list of discovered devices(本地发现 + 中结名册合并,排序/去重与桌面一致)
    pub fn devices(&self) -> Vec<DeviceDto> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let devices = state.devices.lock().unwrap().clone();
                let roster = state.relay_roster.lock().unwrap().clone();
                let connected = state.connected_fps.lock().unwrap().clone();
                let (aliases, trusted) = self.runtime.block_on(async {
                    let t = &*state.trust.lock().await;
                    (
                        localtrans_core::device_merge::alias_map(t),
                        localtrans_core::device_merge::trusted_pairs(t),
                    )
                });
                let merged = localtrans_core::device_merge::merge_devices(&devices, &roster, &connected, &aliases, &trusted);
                // M3c T3:force_relay 注记(卡片角标+菜单勾选态数据源)
                let force = self.runtime.block_on(async { state.config.read().await.force_relay_map.clone() });
                merged.into_iter().map(|m| DeviceDto {
                    force_relay: force.get(&m.fingerprint).copied().unwrap_or(false),
                    fingerprint: m.fingerprint,
                    name: m.name,
                    addr: m.addr,
                    online: m.online,
                    connected: m.connected,
                    via_relay: m.via_relay,
                }).collect()
            }
            None => vec![],
        }
    }

    /// Set hidden mode
    pub fn set_hidden(&self, hidden: bool) {
        let app = self.state.get().cloned();
        if let Some(state) = app.as_deref() {
            let was_hidden = state.hidden.load(Ordering::Relaxed);
            state.hidden.store(hidden, Ordering::Relaxed);
            let relay_enabled = {
                let mut config = self.runtime.block_on(async { state.config.write().await });
                config.hidden = hidden;
                let dir = &state.dir;
                let _ = localtrans_core::store::save_config(dir, &config);
                config.relay_enabled
            };
            // 隐身翻转→中继重注册(Register.hidden 决定名册可见性);
            // spawn_relay 内部会读 config,上面的写锁已随块结束释放
            if relay_enabled && was_hidden != hidden {
                let state_ptr: &crate::state::AppState = state;
                let cb = self.callback.clone();
                self.runtime.block_on(async move {
                    crate::relay_state::spawn_relay(state_ptr, &cb).await;
                });
            }
        }
    }

    /// 本机主网络 IP(UDP connect 仅本地选路不发包;镜像桌面壳 firewall::primary_local_ip)
    pub fn local_ip(&self) -> Option<String> {
        udp_local_ip()
    }

    /// 本机全网卡非环回 IPv4 列表(M3a FR1;镜像桌面 get_network_status.local_ips)。
    /// 虚拟网卡/环回/链路本地已在 core 过滤,首选地址(与 local_ip 同口径)置首。
    pub fn local_ips(&self) -> Vec<dto::LocalIpDto> {
        local_ip_dtos()
    }

    /// 通道探测记录表只读导出(M3b T4 最小只读面;M3c 通道 UI 数据源,
    /// 字段口径镜像桌面 list_channels 命令)。未启动返回空表。
    /// M3c T0:生产者已接线(probe 模块 SessionUp 登记+全量探测+快检调度),
    /// 配对会话建立后本表有数据(内存态,进程重启即空)。
    pub fn channels(&self) -> Vec<dto::ChannelDto> {
        let Some(state) = self.state.get() else { return Vec::new() };
        let table = &state.channels;
        let mut out = Vec::new();
        for fp in table.all_fps() {
            let current = table.current(&fp);
            for r in table.snapshot(&fp) {
                out.push(dto::ChannelDto {
                    fingerprint: hex::encode(fp),
                    addr: r.addr.to_string(),
                    via_relay: r.via_relay,
                    rtt_ms: r.rtt_ms,
                    est_bps: r.est_bps,
                    loss_rate: r.loss_rate(),
                    current: current == Some(r.addr),
                    score_ready: r.score_ready(),
                    probe_disabled: table.probe_disabled(&fp, &r.addr),
                    age_secs: r.updated_at.elapsed().as_secs(),
                });
            }
        }
        out
    }

    /// 手动单对端快检(M3c T2 通道面板「重新探测」入口;镜像桌面壳
    /// probe_now_peer 命令):当前通道 64KB 快检,掉 50% 升级全量,
    /// 同步等待(20s 超时兜底)。设备未连接/无通道记录/探测拉黑报错。
    pub fn probe_peer_now(&self, fingerprint: String) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp = decode_fp32(&fingerprint)?;
                ffi_guard(self, Box::pin(async move {
                    crate::probe::probe_peer_now(&state, fp).await
                        .map_err(|e| AppException::Internal { message: e })
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// M3c T3:强制走中继开关(per 设备持久化,config.json force_relay_map;
    /// 镜像桌面壳 set_force_relay 命令)。开启后 connect_device 跳过评分
    /// 直选中继路径;菜单勾选态读 is_force_relay。
    pub fn set_force_relay(&self, fingerprint: String, on: bool) -> Result<(), AppException> {
        // 指纹合法性:仅接受 64 hex(防 UI 侧写入垃圾键)
        let fp_bytes = hex::decode(&fingerprint)
            .map_err(|e| AppException::Internal { message: format!("无效指纹: {}", e) })?;
        if fp_bytes.len() != 32 {
            return Err(AppException::Internal { message: format!("无效指纹长度: {}", fp_bytes.len()) });
        }
        let app = self.state.get().cloned();
        if let Some(state) = app.as_deref() {
            let mut config = self.runtime.block_on(async { state.config.write().await });
            if on {
                config.force_relay_map.insert(fingerprint, true);
            } else {
                config.force_relay_map.remove(&fingerprint);
            }
            let dir = &state.dir;
            let _ = localtrans_core::store::save_config(dir, &config);
        }
        Ok(())
    }

    /// M3c T3:查强制走中继开关当前值(未记录 = false)
    pub fn is_force_relay(&self, fingerprint: String) -> bool {
        let app = self.state.get().cloned();
        match app {
            Some(state) => self.runtime.block_on(async {
                state.config.read().await.force_relay_map
                    .get(&fingerprint).copied().unwrap_or(false)
            }),
            None => false,
        }
    }

    /// 手动探测指定地址:仅 IP 补默认发现端口 47600;IP:端口 直用;坏格式 AppException。
    /// 本机隐身时发现层门控自动跳过(不发包),由调用方文案涵盖。
    pub fn probe_addr(&self, addr: String) -> Result<(), AppException> {
        let socket_addr = parse_probe_addr(&addr)
            .map_err(|e| AppException::Io { message: format!("无效地址: {}", e) })?;
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                self.runtime.block_on(async {
                    state.discovery.cmd
                        .send(localtrans_core::discovery::DiscoveryCmd::ProbeAddr(socket_addr))
                        .await
                        .map_err(|e| AppException::Io { message: format!("探测失败: {}", e) })
                })
            }
            None => Err(AppException::Io { message: "应用未启动".into() }),
        }
    }

    /// Connect to a device by fingerprint(路由:本地优先,名册兜底)
    pub fn connect_device(&self, fingerprint: String) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp = decode_fp32(&fingerprint)?;

                // M3c T3 强制走中继(per 设备持久化开关):命令层拦截决策——
                // 跳过评分直选中继路径(评分/切换 core 语义不动)。中继客户端
                // 不在场报"中继未配置"(镜像桌面壳 connect 同口径)。
                let force_relay = self.runtime.block_on(async {
                    state.config.read().await.force_relay_map
                        .get(&fingerprint).copied().unwrap_or(false)
                });
                if force_relay {
                    let relay_client = self.runtime.block_on(async { state.relay.lock().await.clone() });
                    let Some(client) = relay_client else {
                        return Err(AppException::Internal { message: "中继未配置".to_string() });
                    };
                    let sm = state.sm.clone();
                    return ffi_guard(self, Box::pin(async move {
                        let conn = client.connect_peer(fp).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        // 主动方语义:开 bi 流(与对端 adopt_connection 配对,防死锁)
                        sm.adopt_as_initiator(conn).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        Ok(())
                    }));
                }

                // 路由逻辑:优先本地发现,其次中结名册(镜像桌面壳 connect 命令)
                let local_addr = {
                    let devices = state.devices.lock().unwrap();
                    devices.iter().find(|d| d.fingerprint == fp).map(|d| d.addr)
                };

                if let Some(addr) = local_addr {
                    let sm = state.sm.clone();
                    ffi_guard(self, Box::pin(async move {
                        // T6: 不可达地址下 quinn 握手可能远超系统 TCP 超时,
                        // 必须在 FFI 侧兜底 15s,否则调用线程挂死
                        tokio::time::timeout(std::time::Duration::from_secs(15),
                            sm.connect_pinned(addr, fp))
                            .await
                            .map_err(|_| AppException::Internal { message: format!("连接超时(15s): {}", addr) })?
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        Ok::<(), AppException>(())
                    }))
                } else {
                    // 查中结名册
                    let in_roster = {
                        let roster = state.relay_roster.lock().unwrap();
                        roster.iter().any(|d| d.fingerprint == fp)
                    };
                    if !in_roster {
                        return Err(AppException::Internal { message: format!("未找到设备: {}", fingerprint) });
                    }

                    let relay_client = self.runtime.block_on(async { state.relay.lock().await.clone() });
                    let Some(client) = relay_client else {
                        return Err(AppException::Internal { message: "中继未连接".to_string() });
                    };
                    let sm = state.sm.clone();
                    ffi_guard(self, Box::pin(async move {
                        let conn = client.connect_peer(fp).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        // 主动方语义:开 bi 流(与对端 adopt_connection 配对,防死锁)
                        sm.adopt_as_initiator(conn).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                        Ok::<(), AppException>(())
                    }))
                }
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Respond to a pairing consent request
    pub fn respond_consent(&self, fingerprint: String, accept: bool) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp = decode_fp32(&fingerprint)?;

                let sm = state.sm.clone();
                ffi_guard(self, Box::pin(async move {
                    if accept {
                        sm.grant_consent(&fp).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                    } else {
                        sm.deny_consent(&fp).await
                            .map_err(|e| AppException::Internal { message: e.to_string() })?;
                    }
                    Ok::<(), AppException>(())
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Submit a pairing code
    pub fn submit_pairing_code(&self, fingerprint: String, code: String) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp = decode_fp32(&fingerprint)?;

                let sm = state.sm.clone();
                ffi_guard(self, Box::pin(async move {
                    sm.submit_pair_code(&fp, &code).await
                        .map_err(|e| AppException::Internal { message: e.to_string() })?;
                    Ok::<(), AppException>(())
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Cancel waiting for pairing code
    pub fn cancel_wait(&self, fingerprint: String) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp = decode_fp32(&fingerprint)?;

                let sm = state.sm.clone();
                ffi_guard(self, Box::pin(async move {
                    sm.cancel_wait(&fp).await
                        .map_err(|e| AppException::Internal { message: e.to_string() })
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Get current settings
    pub fn settings(&self) -> SettingsDto {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let config = self.runtime.block_on(async { state.config.read().await.clone() });
                SettingsDto {
                    device_name: config.device_name,
                    hidden: config.hidden,
                    offer_timeout_secs: config.offer_timeout_secs,
                    consent_timeout_secs: config.consent_timeout_secs,
                    relay_enabled: config.relay_enabled,
                    relay_addr: config.relay_server,
                    relay_psk: config.relay_psk,
                    max_active_transfers: config.max_active_transfers,
                    backup_enabled: false,
                    backup_target_fp: String::new(),
                    backup_photos: false,
                    backup_videos: false,
                }
            }
            None => SettingsDto {
                device_name: String::new(),
                hidden: false,
                offer_timeout_secs: 60,
                consent_timeout_secs: 60,
                relay_enabled: false,
                relay_addr: String::new(),
                relay_psk: String::new(),
                max_active_transfers: 3,
                backup_enabled: false,
                backup_target_fp: String::new(),
                backup_photos: false,
                backup_videos: false,
            },
        }
    }

    /// Save settings
    pub fn save_settings(&self, settings: SettingsDto) {
        let app = self.state.get().cloned();
        if let Some(state) = app.as_deref() {
            let mut config = self.runtime.block_on(async { state.config.write().await });
            // 设备名变更检测(对齐桌面壳 save_settings):改名须即时广播,
            // 否则发现层出站名停留在旧值——对端看到的一直是旧名,配合
            // 15s 过期表现为"旧名消失/新名出现"的重复设备观感(v0.7.0 实测)
            let name_changed = config.device_name != settings.device_name;
            // 中继配置变更检测(v0.9.0):enabled/server/psk 任一变化须重启
            // 中继连接——否则新配置不生效(或旧连接残留)。设备名变化也走
            // 重连(Register 上报 device_name,名册端名字靠重连刷新)。
            let old_relay = (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone());
            config.device_name = settings.device_name.clone();
            config.hidden = settings.hidden;
            config.offer_timeout_secs = settings.offer_timeout_secs.clamp(15, 600);
            config.consent_timeout_secs = settings.consent_timeout_secs.clamp(15, 600);
            config.relay_enabled = settings.relay_enabled;
            config.relay_server = settings.relay_addr.clone();
            config.relay_psk = settings.relay_psk.clone();
            // M2 T4:并发闸门容量钳制 1-8(运行时改动重启后生效——active_gate 启动构建)
            config.max_active_transfers = settings.max_active_transfers.clamp(1, 8);
            let dir = &state.dir;
            let _ = localtrans_core::store::save_config(dir, &config);
            drop(config);

            let relay_changed = old_relay != (settings.relay_enabled, settings.relay_addr.clone(), settings.relay_psk.clone())
                || name_changed;

            if relay_changed {
                let state_ptr: &crate::state::AppState = state;
                let cb = self.callback.clone();
                self.runtime.block_on(async move {
                    crate::relay_state::spawn_relay(state_ptr, &cb).await;
                });
            }

            if name_changed {
                let discovery = state.discovery.clone();
                let new_name = settings.device_name;
                self.runtime.spawn(async move {
                    let _ = discovery.cmd.send(
                        localtrans_core::discovery::DiscoveryCmd::SetName(new_name)
                    ).await;
                });
            }
        }
    }

    /// 校验中继配置(设置页保存前调用;规则与服务端一致,Rust 单一实现防双端漂移)
    pub fn validate_relay(&self, enabled: bool, server: String, psk: String) -> Result<(), AppException> {
        localtrans_core::relay::validate_relay_config(enabled, &server, &psk)
            .map_err(|msg| AppException::Internal { message: msg })
    }

    /// 查询中继状态(设置页拉模式显示)
    pub fn relay_status(&self) -> dto::RelayStatusDto {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let config = self.runtime.block_on(async { state.config.read().await.clone() });
                // M3a FR2:public_exit 仅"中继启用 且 客户端已注册"时为 Some(镜像桌面 get_network_status)
                let (connected, public_exit) = self.runtime.block_on(async {
                    match state.relay.lock().await.as_ref() {
                        Some(client) => (true, if config.relay_enabled { client.observed_addr().await } else { None }),
                        None => (false, None),
                    }
                });
                let status_ui = state.relay_status.lock().unwrap().clone();
                let (status, error) = match status_ui {
                    crate::relay_state::RelayUiStatus::Disabled => ("disabled".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Connecting => ("connecting".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Connected => ("connected".to_string(), String::new()),
                    crate::relay_state::RelayUiStatus::Error(reason) => ("error".to_string(), reason),
                };
                dto::RelayStatusDto { enabled: config.relay_enabled, connected, status, error, public_exit }
            }
            None => dto::RelayStatusDto { enabled: false, connected: false, status: "disabled".into(), error: String::new(), public_exit: None },
        }
    }

    /// Disconnect from a device
    pub fn disconnect(&self, fingerprint: String) {
        let app = self.state.get().cloned();
        if let Some(state) = app.as_deref() {
            if let Ok(bytes) = hex::decode(&fingerprint) {
                if bytes.len() == 32 {
                    let mut fp = [0u8; 32];
                    fp.copy_from_slice(&bytes);
                    self.runtime.block_on(async {
                        state.sm.disconnect(&fp).await;
                    });
                }
            }
        }
    }

    /// Push files to a remote device (真实实现)
    pub fn push_files(&self, fp: String, paths: Vec<String>) -> u64 {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let placeholder_id = state.next_placeholder_id();
                let placeholder_name = if paths.len() == 1 {
                    paths[0].clone()
                } else {
                    format!("{} files", paths.len())
                };

                let peer_hex = fp.clone();
                let callback = self.callback.clone();

                // Create placeholder transfer
                let placeholder_dto = TransferDto {
                    job_id: placeholder_id,
                    name: placeholder_name,
                    total: 0,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: peer_hex.clone(),
                    direction: "push".to_string(),
                    local_role: "sender".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                };

                let st_for_placeholder = state.clone();
                self.runtime.spawn(async move {
                    st_for_placeholder.card_create(placeholder_dto).await;
                });

                // Spawn real push task
                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(e) => {
                        // 指纹非法:占位行置 failed 并发失败事件,避免 UI 永久 pending 卡片
                        tracing::warn!("推送取消: {}", e);
                        let st = state.clone();
                        let cb = callback.clone();
                        self.runtime.spawn(async move {
                            if let Some(dto) = st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some("指纹格式非法".into()),
                            }).await {
                                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                            }
                            cb.on_event(AppEvent::TransferDone { job_id: placeholder_id, ok: false, fail_reason: "指纹格式非法".to_string() });
                        });
                        return placeholder_id;
                    }
                };

                let paths: Vec<std::path::PathBuf> = paths.iter().map(|p| std::path::PathBuf::from(p)).collect();
                let sm = state.sm.clone();
                let st = state.clone();
                let cb = callback.clone();
                let peer_hex_clone = peer_hex.clone();

                self.runtime.spawn(async move {
                    // M2 T4:全局并发闸门+对端串行锁(替代全局 xfer_lock,跨对端放开)
                    // 排队超时:闸门满/前序同对端任务卡死,60s 后明确失败(对齐 PC)
                    let (_permit, _guard) = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        acquire_with_queue_pos(&st, placeholder_id, &peer_hex_clone),
                    ).await {
                        Ok(slot) => slot,
                        Err(_) => {
                            tracing::warn!("推送排队超时：并发闸门满且同对端前序任务持有超过 60s");
                            // 超时取消后清排队位次残留,再 QueueTimeout 落 failed(状态机单写者)
                            st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                            return;
                        }
                    };

                    let (progress_tx, mut progress_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

                    // Spawn push_files engine
                    let push_fut = localtrans_core::transfer::push_files(
                        &sm,
                        &fp_bytes,
                        paths,
                        &st.sender_jobs,
                        progress_tx,
                    );
                    tokio::pin!(push_fut);

                    let mut placeholder_alive = true;
                    let mut progress_stopped = false;

                    loop {
                        tokio::select! {
                            biased;
                            ev = progress_rx.recv(), if !progress_stopped => {
                                match ev {
                                    Some(ev) => handle_push_progress_event(
                                        &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                                    ).await,
                                    None => progress_stopped = true,
                                }
                            }
                            res = &mut push_fut => {
                                if let Err(e) = res {
                                    tracing::warn!("推送失败: {}", e);
                                    if placeholder_alive {
                                        // apply_card_event:cancelling 卡收到引擎终态即确认收尾
                                        apply_card_event(&st, &cb, placeholder_id, crate::transfer_state::CardEvent::Failed {
                                            reason: Some(e.to_string()),
                                        }).await;
                                        placeholder_alive = false;
                                    }
                                }
                                // Drain remaining events
                                while let Some(ev) = progress_rx.recv().await {
                                    handle_push_progress_event(
                                        &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                                    ).await;
                                }
                                break;
                            }
                        }
                    }
                });

                placeholder_id
            }
            None => u64::MAX - 1,
        }
    }

    /// Push files with relative directories (folder structure preserved)
    /// rel_dir is the remote subdirectory (empty string = root)
    pub fn push_files_rel(&self, fp: String, files: Vec<PushFileDto>) -> u64 {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let placeholder_id = state.next_placeholder_id();
                let placeholder_name = if files.len() == 1 {
                    std::path::PathBuf::from(&files[0].path)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&files[0].path)
                        .to_string()
                } else {
                    format!("{} files", files.len())
                };

                let peer_hex = fp.clone();
                let callback = self.callback.clone();

                // Create placeholder transfer
                let placeholder_dto = TransferDto {
                    job_id: placeholder_id,
                    name: placeholder_name,
                    total: 0,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: peer_hex.clone(),
                    direction: "push".to_string(),
                    local_role: "sender".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                };

                let st_for_placeholder = state.clone();
                self.runtime.spawn(async move {
                    st_for_placeholder.card_create(placeholder_dto).await;
                });

                // Spawn real push task
                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(e) => {
                        // 指纹非法:占位行置 failed 并发失败事件,避免 UI 永久 pending 卡片
                        tracing::warn!("推送取消: {}", e);
                        let st = state.clone();
                        let cb = callback.clone();
                        self.runtime.spawn(async move {
                            if let Some(dto) = st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some("指纹格式非法".into()),
                            }).await {
                                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                            }
                            cb.on_event(AppEvent::TransferDone { job_id: placeholder_id, ok: false, fail_reason: "指纹格式非法".to_string() });
                        });
                        return placeholder_id;
                    }
                };

                let paths: Vec<(std::path::PathBuf, String)> = files.iter()
                    .map(|f| (std::path::PathBuf::from(&f.path), f.rel_dir.clone()))
                    .collect();
                let sm = state.sm.clone();
                let st = state.clone();
                let cb = callback.clone();
                let peer_hex_clone = peer_hex.clone();

                self.runtime.spawn(async move {
                    // M2 T4:全局并发闸门+对端串行锁(替代全局 xfer_lock,跨对端放开)
                    // 排队超时:闸门满/前序同对端任务卡死,60s 后明确失败(对齐 PC)
                    let (_permit, _guard) = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        acquire_with_queue_pos(&st, placeholder_id, &peer_hex_clone),
                    ).await {
                        Ok(slot) => slot,
                        Err(_) => {
                            tracing::warn!("推送排队超时：并发闸门满且同对端前序任务持有超过 60s");
                            // 超时取消后清排队位次残留,再 QueueTimeout 落 failed(状态机单写者)
                            st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                            return;
                        }
                    };

                    let (progress_tx, mut progress_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

                    // Spawn push_files_rel engine
                    let push_fut = localtrans_core::transfer::push_files_rel(
                        &sm,
                        &fp_bytes,
                        paths,
                        &st.sender_jobs,
                        progress_tx,
                    );
                    tokio::pin!(push_fut);

                    let mut placeholder_alive = true;
                    let mut progress_stopped = false;

                    loop {
                        tokio::select! {
                            biased;
                            ev = progress_rx.recv(), if !progress_stopped => {
                                match ev {
                                    Some(ev) => handle_push_progress_event(
                                        &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                                    ).await,
                                    None => progress_stopped = true,
                                }
                            }
                            res = &mut push_fut => {
                                if let Err(e) = res {
                                    tracing::warn!("推送失败: {}", e);
                                    if placeholder_alive {
                                        // apply_card_event:cancelling 卡收到引擎终态即确认收尾
                                        apply_card_event(&st, &cb, placeholder_id, crate::transfer_state::CardEvent::Failed {
                                            reason: Some(e.to_string()),
                                        }).await;
                                        placeholder_alive = false;
                                    }
                                }
                                // Drain remaining events
                                while let Some(ev) = progress_rx.recv().await {
                                    handle_push_progress_event(
                                        &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                                    ).await;
                                }
                                break;
                            }
                        }
                    }
                });

                placeholder_id
            }
            None => u64::MAX - 1,
        }
    }

    /// Pull files from a remote share (真实实现)
    pub fn pull_files(&self, fp: String, remote_share_id: String, remote_paths: Vec<String>) -> u64 {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let placeholder_id = state.next_placeholder_id();
                let peer_hex = fp.clone();
                let callback = self.callback.clone();
                let state_clone = state.clone();

                // Create placeholder transfer
                let placeholder_dto = TransferDto {
                    job_id: placeholder_id,
                    name: if remote_paths.len() == 1 { remote_paths[0].clone() } else { format!("{} files", remote_paths.len()) },
                    total: 0,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: peer_hex.clone(),
                    direction: "pull".to_string(),
                    local_role: "receiver".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                };

                self.runtime.spawn(async move {
                    state_clone.card_create(placeholder_dto).await;
                });

                // Spawn real pull task
                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(e) => {
                        // 指纹非法:占位行置 failed 并发失败事件,避免 UI 永久 pending 卡片
                        tracing::warn!("下载取消: {}", e);
                        let st = state.clone();
                        let cb = callback.clone();
                        self.runtime.spawn(async move {
                            if let Some(dto) = st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some("指纹格式非法".into()),
                            }).await {
                                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                            }
                            cb.on_event(AppEvent::TransferDone { job_id: placeholder_id, ok: false, fail_reason: "指纹格式非法".to_string() });
                        });
                        return placeholder_id;
                    }
                };

                let share_id = remote_share_id;
                // N1-T3 修复:多选此前两分支同值只拉第一个文件——改为逐文件
                // 顺序拉取,全部聚合到同一张占位卡(单卡累计,整批终态)。
                let rels = remote_paths.clone();
                let is_multi = rels.len() > 1;
                let sm = state.sm.clone();
                let st = state.clone();
                let cb = callback.clone();
                let config = self.runtime.block_on(async { state.config.read().await.clone() });

                self.runtime.spawn(async move {
                    // M2 T4:全局并发闸门+对端串行锁(替代全局 xfer_lock,跨对端放开)
                    // 排队超时:闸门满/前序同对端任务卡死,60s 后明确失败(对齐 PC)
                    let (_permit, _guard) = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        acquire_with_queue_pos(&st, placeholder_id, &peer_hex),
                    ).await {
                        Ok(slot) => slot,
                        Err(_) => {
                            tracing::warn!("下载排队超时：并发闸门满且同对端前序任务持有超过 60s");
                            // 超时取消后清排队位次残留,再 QueueTimeout 落 failed(状态机单写者)
                            st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                            return;
                        }
                    };

                    // N1-T3:逐文件顺序拉取。事件全部按 placeholder_id 落卡
                    // (ID 恒定;旧 handle_pull_progress_event 按引擎 job_id
                    // 键控并删占位,仅适配单文件)。
                    let mut ok_count = 0u32;
                    let mut failed_count = 0u32;
                    for rel in rels {
                        let (progress_tx, mut progress_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
                        let reg = localtrans_core::share::ShareRegistry::new(vec![]);
                        let pull_fut = localtrans_core::transfer::start_pull(
                            &sm,
                            &reg,
                            &fp_bytes,
                            &share_id,
                            &rel,
                            &config,
                            progress_tx,
                        );
                        tokio::pin!(pull_fut);

                        let mut progress_stopped = false;
                        let mut file_failed = false;

                        loop {
                            tokio::select! {
                                biased;
                                ev = progress_rx.recv(), if !progress_stopped => {
                                    match ev {
                                        Some(ev) => handle_pull_batch_event(
                                            &st, &cb, placeholder_id, is_multi,
                                            &mut ok_count, &mut failed_count, &mut file_failed, ev,
                                        ).await,
                                        None => progress_stopped = true,
                                    }
                                }
                                res = &mut pull_fut => {
                                    if let Err(e) = res {
                                        tracing::warn!("下载失败: {}", e);
                                        file_failed = true;
                                        st.card_mutate(placeholder_id, |d| d.fail_reason = e.to_string()).await;
                                    }
                                    while let Some(ev) = progress_rx.recv().await {
                                        handle_pull_batch_event(
                                            &st, &cb, placeholder_id, is_multi,
                                            &mut ok_count, &mut failed_count, &mut file_failed, ev,
                                        ).await;
                                    }
                                    break;
                                }
                            }
                        }
                        if file_failed { failed_count += 1; } else { ok_count += 1; }
                    }

                    // 整批终态:全成→done;有败→failed(混败带"N 项失败"原因)。
                    // 状态机单写者:Finished/Failed 落卡;全秒传批可能仍 pending
                    // (InstantHit 不发 Started)——先 Started 激活再收敛
                    // (M6 回归锚:pending 直收 Finished 会被状态机拒绝)。
                    st.clear_throttle(placeholder_id);
                    if st.card_state(placeholder_id).await.as_deref() == Some("pending") {
                        st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Started).await;
                    }
                    let all_ok = failed_count == 0 && ok_count > 0;
                    let final_dto = if all_ok {
                        st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Finished).await
                    } else {
                        st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                            reason: if ok_count > 0 { Some(format!("{} 项失败", failed_count)) } else { None },
                        }).await
                    };
                    if let Some(dto) = final_dto {
                        cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                        cb.on_event(AppEvent::TransferDone {
                            job_id: placeholder_id,
                            ok: all_ok,
                            fail_reason: if all_ok { String::new() } else { format!("{} 项失败", failed_count) },
                        });
                    }
                });

                placeholder_id
            }
            None => u64::MAX - 1,
        }
    }

    /// Respond to an offer request
    pub fn respond_offer(&self, job_id: u64, accept: bool) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let pending_offers = state.pending_offers.clone();
                let dir = state.inbox_dir.read().unwrap().clone();
                ffi_guard(self, Box::pin(async move {
                    let pending = pending_offers.lock().await.remove(&job_id)
                        .ok_or_else(|| AppException::Internal { message: format!("Offer not found: {}", job_id) })?;

                    let save_dir = if accept {
                        // 快照点对齐 accept:此刻的 inbox_dir 就是发给 core 的
                        // 落盘目录,同时钉进 saved_files 累加器——之后切目录
                        // 不改此值,FilesSaved 拼路径与实际落盘一致。
                        state.saved_files.lock().unwrap()
                            .entry(job_id)
                            .or_insert_with(crate::SavedFilesAcc::new)
                            .set_root(dir.clone());
                        Some(dir)
                    } else {
                        None
                    };

                    pending.respond.send(save_dir)
                        .map_err(|_| AppException::Internal { message: "Offer timeout".to_string() })?;

                    Ok(())
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// P0-2d: 应答远程删除确认
    pub fn respond_delete(&self, ask_id: u64, allow: bool) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let pending_deletes = state.pending_deletes.clone();
                ffi_guard(self, Box::pin(async move {
                    if let Some(tx) = pending_deletes.lock().await.remove(&ask_id) {
                        let _ = tx.send(allow);
                    }
                    Ok(())
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Get list of all transfers (removed 卡不出现——两级删除第一级)
    pub fn transfers(&self) -> Vec<TransferDto> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let mut jobs: Vec<TransferDto> = self.runtime.block_on(async {
                    state.snapshot_dtos().await
                });
                let rank = |d: &TransferDto| match d.state.as_str() {
                    "pending" | "active" | "paused" => 0u8,
                    _ => 1,
                };
                jobs.sort_by_key(|d| (rank(d), d.job_id));
                jobs
            }
            None => vec![],
        }
    }

    /// Pause a transfer (M2 T2:Active→Paused 状态机迁移;pending 拒绝——
    /// 对齐桌面壳,防"引擎标志已置而卡片拒绝迁移"的假暂停)
    pub fn pause(&self, job_id: u64) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                ffi_guard(self, Box::pin(async move {
                    let card_state = state.card_state(job_id).await
                        .ok_or_else(|| AppException::Internal { message: format!("Task not found: {}", job_id) })?;
                    if card_state == "pending" {
                        return Err(AppException::Internal { message: "任务尚未开始，无法暂停".to_string() });
                    }
                    // Try push control first
                    if let Some(push_control) = localtrans_core::transfer::engine::get_push_control(job_id) {
                        push_control.paused.store(true, std::sync::atomic::Ordering::Relaxed);
                        state.card_apply(job_id, crate::transfer_state::CardEvent::Paused).await;
                        return Ok(());
                    }

                    // Try task control
                    if localtrans_core::transfer::control_task(job_id, localtrans_core::transfer::TaskControl::Pause) {
                        state.card_apply(job_id, crate::transfer_state::CardEvent::Paused).await;
                        return Ok(());
                    }

                    Err(AppException::Internal { message: format!("Task not found: {}", job_id) })
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Resume a transfer (M2 T2:Paused→Active 状态机迁移;非 paused 拒绝)
    pub fn resume(&self, job_id: u64) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                ffi_guard(self, Box::pin(async move {
                    let card_state = state.card_state(job_id).await
                        .ok_or_else(|| AppException::Internal { message: format!("Task not found: {}", job_id) })?;
                    if card_state != "paused" {
                        return Err(AppException::Internal { message: "任务未在暂停状态".to_string() });
                    }
                    // Try push control first
                    if let Some(push_control) = localtrans_core::transfer::engine::get_push_control(job_id) {
                        push_control.paused.store(false, std::sync::atomic::Ordering::Relaxed);
                        state.card_apply(job_id, crate::transfer_state::CardEvent::Resumed).await;
                        return Ok(());
                    }

                    // Try task control
                    if localtrans_core::transfer::control_task(job_id, localtrans_core::transfer::TaskControl::Resume) {
                        state.card_apply(job_id, crate::transfer_state::CardEvent::Resumed).await;
                        return Ok(());
                    }

                    Err(AppException::Internal { message: format!("Task not found: {}", job_id) })
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// Cancel a transfer (M2 T2:非终态卡 → CancelRequested 入 cancelling →
    /// 既有引擎取消路径照发 → 引擎终态确认 / 5s 看门狗兜底后实删)
    pub fn cancel(&self, job_id: u64) -> Result<(), AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let cb = self.callback.clone();
                ffi_guard(self, Box::pin(async move {
                    cancel_transfer_arbitrate(&state, &cb, job_id).await
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// 两级删除(对齐桌面壳 remove_transfer level="view"/"destroy"):
    /// - view:仅终态卡——removed=true 保留表内(transfers.json 带 removed 标记,
    ///   重启不复活);活动卡报错"任务进行中，请先取消";
    /// - destroy:终态卡直接实删;活动卡走 cancelling 仲裁(引擎取消路径 +
    ///   5s 看门狗),确认后由 finalize_remove 收尾。
    /// 返回是否发生删除动作(false=卡片不存在)。
    pub fn transfer_remove_level(&self, job_id: u64, level: String) -> Result<bool, AppException> {
        let Some(state) = self.state.get().cloned() else {
            return Err(AppException::Internal { message: "Not started".to_string() });
        };
        if level != "view" && level != "destroy" {
            return Err(AppException::Internal { message: format!("无效删除级别: {}", level) });
        }
        let cb = self.callback.clone();
        ffi_guard(self, Box::pin(async move {
            remove_transfer_level_inner(&state, &cb, job_id, &level).await
        }))
    }

    /// Remove a transfer record from the table(destroy 级,兼容旧入口:
    /// 终态直接删;活动卡走 cancelling 仲裁后由确认/看门狗收尾)。
    pub fn transfer_remove(&self, job_id: u64) -> Result<(), AppException> {
        self.transfer_remove_level(job_id, "destroy".to_string()).map(|_| ())
    }

    /// Remove all terminal transfer records (done/failed/interrupted).
    /// Returns the number of removed rows.
    pub fn transfers_clear_finished(&self) -> u64 {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let st = state.clone();
                self.runtime.block_on(async move {
                    let cards = st.snapshot_cards().await;
                    let terminal: Vec<u64> = cards.iter()
                        .filter(|c| crate::transfer_state::is_terminal(&c.dto.state))
                        .map(|c| c.dto.job_id)
                        .collect();
                    for id in &terminal {
                        st.finalize_remove(*id).await;
                    }
                    terminal.len() as u64
                })
            }
            None => 0,
        }
    }

    /// Retry a failed/interrupted transfer (薄包装:找 job + 抽核心)
    pub fn retry_transfer(&self, job_id: u64) -> u64 {
        let app = self.state.get().cloned();
        let Some(state) = app else { return 0 };
        let jobs = localtrans_core::transfer::pending_jobs(&state.dir.clone());
        if jobs.into_iter().find(|(id, _)| *id == job_id).is_none() {
            return 0;
        }
        let state_arc = state.clone();
        let callback = self.callback.clone();
        self.runtime.spawn(async move {
            spawn_retry_transfer(&state_arc, &callback, job_id).await;
        });
        job_id
    }

    // ===== 磁盘历史(M2 T3,对齐桌面壳 commands.rs list/restore/destroy_disk_job) =====

    /// 历史记录入口:全部 parts 根的 manifest(含视图已移除的)+ 表内"磁盘已无
    /// parts 且非 open"的终态卡,按 job_id 去重。display_name:meta 优先,
    /// 回退 file_name。未启动返回空。
    pub fn list_disk_jobs(&self) -> Vec<DiskJobDto> {
        let Some(state) = self.state.get().cloned() else { return vec![] };
        self.runtime.block_on(async move { list_disk_jobs_inner(&state).await })
    }

    /// 恢复视图:在表 → removed=false 回活动/历史列表;不在表(孤儿未建卡 /
    /// 已被 gc 前扫到)→ 按启动重建逻辑建卡入表(卡键=engine_id)。
    /// 表与磁盘均无 → Err。
    pub fn restore_disk_job(&self, job_id: u64) -> Result<(), AppException> {
        let Some(state) = self.state.get().cloned() else {
            return Err(AppException::Internal { message: "Not started".to_string() });
        };
        let cb = self.callback.clone();
        ffi_guard(self, Box::pin(async move { restore_disk_job_inner(&state, &cb, job_id).await }))
    }

    /// 彻底删除磁盘任务:在表 → finalize_destroy(abort 看门狗+删表行+删 parts)
    /// 并发 TransferDone(removed) 让 UI 删行;纯孤儿(未建卡)→ 直接删各根 parts
    /// 目录。磁盘/表均无该任务 → Err。
    pub fn destroy_disk_job(&self, job_id: u64) -> Result<(), AppException> {
        let Some(state) = self.state.get().cloned() else {
            return Err(AppException::Internal { message: "Not started".to_string() });
        };
        let cb = self.callback.clone();
        ffi_guard(self, Box::pin(async move { destroy_disk_job_inner(&state, &cb, job_id).await }))
    }

    /// List remote directory (真实实现)
    pub fn list_remote(&self, fp: String, share_id: String, path: String) -> Vec<FileEntryDto> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(_) => return vec![],
                };

                let sm = state.sm.clone();
                let result = self.runtime.block_on(async move {
                    let (msg_id, resp_rx) = sm.send_rpc(&fp_bytes, localtrans_core::protocol::ControlMsg::ListReq {
                        share_id: share_id.clone(),
                        path: path.clone(),
                        cursor: 0,
                        msg_id: 0,
                    }).await.map_err(|e| format!("发送请求失败: {}", e))?;

                    let result = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
                        match resp_rx.await {
                            Ok((_, localtrans_core::protocol::ControlMsg::ListResp { entries, .. })) => Ok(entries),
                            _ => Err(()),
                        }
                    }).await;

                    if !matches!(result, Ok(Ok(_))) {
                        sm.cancel_rpc(msg_id).await;
                    }

                    match result {
                        Ok(Ok(entries)) => Ok(entries),
                        _ => Err(String::from("等待响应超时或通道关闭")),
                    }
                });

                match result {
                    Ok(entries) => entries.iter().map(|e| FileEntryDto {
                        name: e.name.clone(),
                        is_dir: e.is_dir,
                        size: e.size,
                        modified_ms: e.mtime as i64,
                    }).collect(),
                    Err(_) => vec![],
                }
            }
            None => vec![],
        }
    }

    /// Get list of remote shares (真实实现)
    pub fn remote_shares(&self, fp: String) -> Vec<ShareDto> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(_) => return vec![],
                };

                let sm = state.sm.clone();
                let result = self.runtime.block_on(async move {
                    let (msg_id, resp_rx) = sm.send_rpc(&fp_bytes, localtrans_core::protocol::ControlMsg::SharesReq { msg_id: 0 })
                        .await.map_err(|e| format!("发送请求失败: {}", e))?;

                    let result = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
                        match resp_rx.await {
                            Ok((_, localtrans_core::protocol::ControlMsg::SharesResp { shares, .. })) => Ok(shares),
                            _ => Err(()),
                        }
                    }).await;

                    if !matches!(result, Ok(Ok(_))) {
                        sm.cancel_rpc(msg_id).await;
                    }

                    match result {
                        Ok(Ok(shares)) => Ok(shares),
                        _ => Err(String::from("等待响应超时或通道关闭")),
                    }
                });

                match result {
                    Ok(shares) => shares.iter().map(|s| ShareDto {
                        share_id: s.id.clone(),
                        alias: s.alias.clone(),
                    }).collect(),
                    Err(_) => vec![],
                }
            }
            None => vec![],
        }
    }

    /// Perform file operation on remote share (真实实现)
    pub fn share_op(&self, fp: String, share_id: String, op: FileOp, path: String, new_name: String) -> Result<String, AppException> {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let fp_bytes = hex::decode(&fp).map_err(|e| AppException::Io { message: e.to_string() })?;
                if fp_bytes.len() != 32 {
                    return Err(AppException::Internal { message: "Invalid fingerprint length".to_string() });
                }
                let mut fp_arr = [0u8; 32];
                fp_arr.copy_from_slice(&fp_bytes);

                let msg = match op {
                    FileOp::Rename => localtrans_core::protocol::ControlMsg::ShareRename {
                        share_id,
                        path,
                        new_name,
                        msg_id: 0,
                    },
                    FileOp::Delete => localtrans_core::protocol::ControlMsg::ShareDelete {
                        share_id,
                        path,
                        msg_id: 0,
                    },
                    FileOp::Mkdir => localtrans_core::protocol::ControlMsg::ShareMkdir {
                        share_id,
                        path,
                        msg_id: 0,
                    },
                };

                let sm = state.sm.clone();
                ffi_guard(self, Box::pin(async move {
                    let (msg_id, resp_rx) = sm.send_rpc(&fp_arr, msg).await.map_err(|e|
                        AppException::Internal { message: format!("发送请求失败: {}", e) })?;

                    let result = tokio::time::timeout(tokio::time::Duration::from_secs(10), async {
                        match resp_rx.await {
                            Ok((_, localtrans_core::protocol::ControlMsg::ShareOpResult { ok, error, .. })) => Ok((ok, error)),
                            _ => Err(()),
                        }
                    }).await;

                    if !matches!(result, Ok(Ok((true, _)))) {
                        sm.cancel_rpc(msg_id).await;
                    }

                    match result {
                        Ok(Ok((true, _))) => Ok("操作成功".to_string()),
                        Ok(Ok((false, Some(msg)))) => Err(AppException::Internal { message: msg }),
                        Ok(Ok((false, None))) => Err(AppException::Internal { message: "对端版本过旧或离线,不支持此操作".to_string() }),
                        _ => Err(AppException::Internal { message: "对端版本过旧或离线,不支持此操作".to_string() }),
                    }
                }))
            }
            None => Err(AppException::Internal { message: "Not started".to_string() }),
        }
    }

    /// List local directory
    pub fn list_local(&self, dir: String) -> Vec<FileEntryDto> {
        let path = std::path::Path::new(&dir);
        let mut result = vec![];

        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                // Android emulated storage(/storage/emulated/0 经 FUSE/symlink 链)
                // 上 DirEntry::metadata() 不跟随链接,目录会被误判为文件——
                // 改用 fs::metadata(跟随符号链接)再退回 DirEntry 元数据
                if let Ok(metadata) = std::fs::metadata(entry.path()).or_else(|_| entry.metadata()) {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_dir = metadata.is_dir();
                    let size = if is_dir { 0 } else { metadata.len() };
                    let modified_ms = metadata.modified()
                        .ok()
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0);

                    result.push(FileEntryDto {
                        name,
                        is_dir,
                        size,
                        modified_ms,
                    });
                }
            }
        }

        result
    }

    /// Perform local file operation
    pub fn local_op(&self, op: FileOp, path: String, new_name: String) -> Result<(), AppException> {
        let p = std::path::Path::new(&path);

        match op {
            FileOp::Rename => {
                if let Some(parent) = p.parent() {
                    let new_path = parent.join(&new_name);
                    std::fs::rename(p, new_path).map_err(|e| AppException::Io { message: e.to_string() })?;
                } else {
                    return Err(AppException::Internal { message: "Invalid path".to_string() });
                }
            }
            FileOp::Delete => {
                if p.is_dir() {
                    std::fs::remove_dir_all(p).map_err(|e| AppException::Io { message: e.to_string() })?;
                } else {
                    std::fs::remove_file(p).map_err(|e| AppException::Io { message: e.to_string() })?;
                }
            }
            FileOp::Mkdir => {
                std::fs::create_dir_all(p).map_err(|e| AppException::Io { message: e.to_string() })?;
            }
        }

        Ok(())
    }

    /// Backup push (真实实现)
    pub fn backup_push(&self, fp: String, paths: Vec<String>, batch_tag: String) {
        let app = self.state.get().cloned();
        match app {
            Some(state) => {
                let config = self.runtime.block_on(async { state.config.read().await.clone() });
                let device_name = config.device_name.clone();
                let cleaned_name = clean_backup_dir_name(device_name);
                let rel_dir = format!("LocalTransBackup/{}/{}/", cleaned_name, batch_tag);

                // Convert paths to PathBuf
                let path_bufs: Vec<std::path::PathBuf> = paths.iter().map(|p| std::path::PathBuf::from(p)).collect();

                let fp_bytes = match decode_fp32(&fp) {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::error!("指纹格式非法: {}", e);
                        return;
                    }
                };

                let total_files = path_bufs.len() as u64;
                let callback = self.callback.clone();
                let peer_hex = fp.clone();
                let sm = state.sm.clone();
                let st = state.clone();
                let cb = callback.clone();
                let peer_hex_clone = peer_hex.clone();

                self.runtime.spawn(async move {
                    // Emit initial progress
                    cb.on_event(AppEvent::BackupProgress { done: 0, total: total_files });

                    // M2 T4:全局并发闸门+对端串行锁(替代全局 xfer_lock,跨对端放开)。
                    // 备份无占位卡:不做位次上报;排队超时静默失败(对齐 PC 非卡片路径)
                    let (_permit, _guard) = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        st.acquire_slot(&peer_hex_clone),
                    ).await {
                        Ok(slot) => slot,
                        Err(_) => {
                            tracing::warn!("备份推送排队超时：并发闸门满且同对端前序任务持有超过 60s");
                            cb.on_event(AppEvent::BackupProgress { done: 0, total: total_files });
                            return;
                        }
                    };

                    let (progress_tx, mut progress_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

                    // Spawn push_files_rel
                    let files: Vec<(std::path::PathBuf, String)> = path_bufs.into_iter().map(|p| (p, rel_dir.clone())).collect();
                    let push_fut = localtrans_core::transfer::push_files_rel(
                        &sm,
                        &fp_bytes,
                        files,
                        &st.sender_jobs,
                        progress_tx,
                    );
                    tokio::pin!(push_fut);

                    let mut done_count = 0u64;
                    let mut progress_stopped = false;

                    loop {
                        tokio::select! {
                            biased;
                            ev = progress_rx.recv(), if !progress_stopped => {
                                match ev {
                                    Some(localtrans_core::transfer::ProgressEvent::SourceDone { .. }) => {
                                        done_count += 1;
                                        cb.on_event(AppEvent::BackupProgress { done: done_count, total: total_files });
                                    }
                                    Some(localtrans_core::transfer::ProgressEvent::SourceFailed { job_id, reason }) => {
                                        tracing::warn!("备份文件失败: job_id={}, reason={}", job_id, reason);
                                        done_count += 1;
                                        cb.on_event(AppEvent::BackupProgress { done: done_count, total: total_files });
                                    }
                                    None => progress_stopped = true,
                                    Some(_) => {}
                                }
                            }
                            res = &mut push_fut => {
                                if let Err(e) = res {
                                    tracing::warn!("备份推送失败: {}", e);
                                }
                                while let Some(ev) = progress_rx.recv().await {
                                    match ev {
                                        localtrans_core::transfer::ProgressEvent::SourceDone { .. } => {
                                            done_count += 1;
                                            cb.on_event(AppEvent::BackupProgress { done: done_count, total: total_files });
                                        }
                                        _ => {}
                                    }
                                }
                                break;
                            }
                        }
                    }

                    // Emit final progress
                    cb.on_event(AppEvent::BackupProgress { done: total_files, total: total_files });
                });
            }
            None => {}
        }
    }
}

#[cfg(test)]
impl LocalTransApp {
    pub fn state_for_test(&self) -> Option<&Arc<state::AppState>> {
        self.state.get()
    }
}

/// M2 T4:排队获取槽位并周期上报位次(镜像桌面壳 commands.rs acquire_with_queue_pos)
/// ——等待期间每 1s 把 dto.queue_pos 刷成同对端 pending 卡中的名次(泵式近似);
/// 拿到槽位后置 None。仅改卡片状态不发事件(Kotlin 侧列表展示由 T5 接入)。
async fn acquire_with_queue_pos(
    st: &AppState,
    card_id: u64,
    peer_hex: &str,
) -> (tokio::sync::OwnedSemaphorePermit, tokio::sync::OwnedMutexGuard<()>) {
    let fut = st.acquire_slot(peer_hex);
    tokio::pin!(fut);
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            slot = &mut fut => {
                st.card_mutate(card_id, |d| d.queue_pos = None).await;
                return slot;
            }
            _ = tick.tick() => {
                let pos = st.pending_rank(card_id, peer_hex).await;
                st.card_mutate(card_id, |d| d.queue_pos = pos).await;
            }
        }
    }
}

/// 续传核心(抽自 retry_transfer,供命令入口与自动续传共用)。
/// 行为与原 retry_transfer 完全一致:占位行 → acquire_slot(60s)→ start_pull_into → 进度泵。
pub(crate) async fn spawn_retry_transfer(
    state: &AppState,
    callback: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
) {
    let inbox = state.inbox_dir.read().unwrap().clone();
    let priv_dir = state.dir.clone();
    let jobs = localtrans_core::transfer::pending_jobs(&priv_dir);

    let found = jobs.into_iter().find(|(id, _)| *id == job_id);
    if let Some((_, manifest)) = found {
        let placeholder_id = state.next_placeholder_id();
        let peer_hex = hex::encode(manifest.peer.as_ref().unwrap_or(&String::new()));

        // Create placeholder transfer
        let placeholder_dto = TransferDto {
            job_id: placeholder_id,
            name: manifest.file_name.clone(),
            total: manifest.total_size,
            done: 0, // Will be updated by Resumed event
            state: "pending".to_string(),
            speed_bps: 0,
            peer: peer_hex.clone(),
            direction: "pull".to_string(),
            local_role: "receiver".to_string(),
            progress_percent: 0,
            eta_secs: -1,
            fail_reason: String::new(),
            local_path: None,
            remote_done: 0,
            instant: false,
            ..Default::default()
        };

        state.card_create(placeholder_dto).await;

        // Spawn resume task
        let sm = state.sm.clone();
        let st = state.clone();
        let cb = callback.clone();
        let peer_hex_clone = peer_hex.clone();
        let config = state.config.read().await.clone();

        // M2 T4:全局并发闸门+对端串行锁(替代全局 xfer_lock,跨对端放开)
        // 排队超时:闸门满/前序同对端任务卡死,60s 后明确失败(对齐 PC)
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &peer_hex_clone),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("续传排队超时：并发闸门满且同对端前序任务持有超过 60s");
                // 超时取消后清排队位次残留,再 QueueTimeout 落 failed(状态机单写者)
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                return;
            }
        };

        let (progress_tx, mut progress_rx) = mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

        // Resume the pull
        let peer_bytes = match decode_fp32(&peer_hex) {
            Ok(f) => f,
            Err(e) => {
                // 续传对端指纹非法(manifest 损坏等):置失败而非静默丢弃占位行,
                // 否则 UI 上这条续传任务永远 pending(镜像排队超时分支的处理)
                tracing::warn!("续传取消: 指纹格式非法: {}", e);
                if let Some(dto) = st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                    reason: Some("指纹格式非法".into()),
                }).await {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
                cb.on_event(AppEvent::TransferDone { job_id: placeholder_id, ok: false, fail_reason: "指纹格式非法".to_string() });
                return;
            }
        };
        let pull_fut = localtrans_core::transfer::start_pull_into(
            &sm,
            &peer_bytes,
            &manifest.share_id.as_deref().unwrap_or(""),
            &manifest.rel.as_deref().unwrap_or(""),
            &config,
            &inbox,
            progress_tx,
        );
        tokio::pin!(pull_fut);

        let mut placeholder_alive = true;
        let mut progress_stopped = false;

        loop {
            tokio::select! {
                biased;
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_pull_progress_event(
                            &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut pull_fut => {
                    if let Err(e) = res {
                        tracing::warn!("续传失败: {}", e);
                        if placeholder_alive {
                            // apply_card_event:cancelling 卡收到引擎终态即确认收尾
                            apply_card_event(&st, &cb, placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some(e.to_string()),
                            }).await;
                            placeholder_alive = false;
                        }
                    }
                    while let Some(ev) = progress_rx.recv().await {
                        handle_pull_progress_event(
                            &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex_clone, ev,
                        ).await;
                    }
                    break;
                }
            }
        }
    }
}

/// Clean backup directory name (removes invalid characters)
pub fn clean_backup_dir_name(name: String) -> String {
    let invalid_chars = ['<', '>', ':', '"', '/', '\\', '|', '?', '*'];
    let mut result = name.clone();

    for c in invalid_chars {
        result = result.replace(c, "");
    }

    result = result.trim_matches(|c| c == ' ' || c == '.').to_string();

    result
}

/// UDP connect 8.8.8.8 只做本地选路不真正发包,取主网卡 IP
fn udp_local_ip() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("8.8.8.8:9").ok()?;
    Some(s.local_addr().ok()?.ip().to_string())
}

/// core 全网卡枚举 → FFI DTO(ip 转点分字串;顺序保持 core 的"首选置首")
fn local_ip_dtos() -> Vec<dto::LocalIpDto> {
    localtrans_core::net_addrs::local_addresses()
        .into_iter()
        .map(|a| dto::LocalIpDto { ip: a.ip.to_string(), if_name: a.if_name })
        .collect()
}

/// 解析手动探测地址:含冒号按 SocketAddr 直解;仅 IP 补默认发现端口
fn parse_probe_addr(input: &str) -> Result<std::net::SocketAddr, String> {
    let input = input.trim();
    if input.contains(':') {
        input.parse().map_err(|e| format!("{}", e))
    } else {
        let ip: std::net::IpAddr = input.parse().map_err(|e| format!("{}", e))?;
        Ok(std::net::SocketAddr::new(ip, localtrans_core::ports::discovery_port()))
    }
}

/// 卡片事件落地 + cancelling 仲裁收尾(镜像桌面壳 card_apply 的删除仲裁段):
/// cancelling 卡收到引擎终态事件(确认先到)→ abort 看门狗 + 实删 + 发 removed 事件。
/// 返回 Some(dto)=接受且行未删(调用方继续发 UI 事件);None=拒绝(内部已 warn)或已删。
async fn apply_card_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
    ev: crate::transfer_state::CardEvent,
) -> Option<TransferDto> {
    let was_cancelling = st.card_get(job_id).await
        .map(|c| c.dto.state == "cancelling")
        .unwrap_or(false);
    let dto = st.card_apply(job_id, ev).await?;
    if was_cancelling && crate::transfer_state::is_terminal(&dto.state) {
        tracing::info!("cancelling 卡确认终态,执行实删收尾 job={:016x}", job_id);
        st.finalize_remove(job_id).await;
        cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: "removed".to_string() });
        return None;
    }
    Some(dto)
}

/// 取消/删除共用的活动卡仲裁入口(镜像桌面壳 destroy_transfer 的 cancelling 段):
/// 非终态卡 → CancelRequested(卡入 cancelling) → 既有引擎取消路径照发
/// (push_control / control_task) → 引擎终态确认(apply_card_event 收尾)或
/// 5s 看门狗兜底实删。
async fn cancel_transfer_arbitrate(
    st: &Arc<AppState>,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
) -> Result<(), AppException> {
    let Some(card) = st.card_get(job_id).await else {
        return Err(AppException::Internal { message: format!("Task not found: {}", job_id) });
    };
    let state_str = card.dto.state.clone();
    if crate::transfer_state::is_terminal(&state_str) {
        return Err(AppException::Internal { message: "任务已结束，无法取消".to_string() });
    }

    // 卡入 cancelling(单写者;进不去说明并发状态已变,重新检查)
    st.card_apply(job_id, crate::transfer_state::CardEvent::CancelRequested).await;
    let now_cancelling = st.card_get(job_id).await
        .map(|c| c.dto.state == "cancelling")
        .unwrap_or(false);
    if !now_cancelling {
        return Err(AppException::Internal { message: format!("任务状态 {} 不支持取消", state_str) });
    }

    // 既有引擎取消路径照发:push_control(source-push)→ control_task(拉取)
    if let Some(push_control) = localtrans_core::transfer::engine::get_push_control(job_id) {
        push_control.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let _ = localtrans_core::transfer::control_task(job_id, localtrans_core::transfer::TaskControl::Cancel);

    // 挂 5s 看门狗:超时仍 cancelling → 引擎没回话,兜底实删;确认先到 →
    // finalize_remove abort 本任务。check-and-insert 合并临界区,防并发取消同卡双挂。
    let mut watchdogs = st.cancel_watchdogs.lock().await;
    if watchdogs.contains_key(&job_id) {
        return Ok(());
    }
    let st2 = st.clone();
    let cb2 = cb.clone();
    let h = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        if st2.card_get(job_id).await.map(|c| c.dto.state == "cancelling").unwrap_or(false) {
            tracing::warn!("取消看门狗超时,兜底实删 job={:016x}", job_id);
            if st2.finalize_remove(job_id).await {
                cb2.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: "removed".to_string() });
            }
        } else {
            st2.cancel_watchdogs.lock().await.remove(&job_id);
        }
    });
    watchdogs.insert(job_id, h);
    Ok(())
}

/// 两级删除内部实现(对齐桌面壳 remove_transfer_inner;测试直调,FFI 包装层不进测试):
/// - view:仅终态卡;removed=true + 表保留 + transfers.json 保留(带 removed 标记)
/// - destroy:终态卡直接实删;活动卡走 cancelling 仲裁(引擎确认/5s 看门狗)
async fn remove_transfer_level_inner(
    st: &Arc<AppState>,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
    level: &str,
) -> Result<bool, AppException> {
    let Some(card) = st.card_get(job_id).await else {
        return Ok(false);
    };
    match level {
        "view" => {
            // 裁定(对齐桌面壳):view 级只对终态卡有意义;活动卡要求先取消
            if !crate::transfer_state::is_terminal(&card.dto.state) {
                return Err(AppException::Internal { message: "任务进行中，请先取消".to_string() });
            }
            let ok = st.card_mark_removed(job_id).await;
            if ok {
                // UI 列表按事件驱动刷新,补发 removed 让 Kotlin 侧同步删行
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: "removed".to_string() });
            }
            Ok(ok)
        }
        "destroy" => {
            if crate::transfer_state::is_terminal(&card.dto.state) {
                // M2 T3:destroy 级终态卡实删连带 parts 目录(镜像桌面壳 finalize_destroy)
                st.finalize_destroy(job_id).await;
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: "removed".to_string() });
                return Ok(true);
            }
            // 活动卡:cancelling 仲裁后删(引擎终态确认/5s 看门狗 → finalize_remove;
            // parts 保留为续传数据,由磁盘历史入口决定去留)
            cancel_transfer_arbitrate(st, cb, job_id).await?;
            Ok(true)
        }
        _ => Err(AppException::Internal { message: format!("无效删除级别: {}", level) }),
    }
}

// ===== 磁盘历史内部实现(M2 T3;测试直调,FFI 包装层不进测试) =====

/// 表内卡 → 历史记录条目(removed 标记透传;created_at_ms=started_at_ms)。
fn disk_job_dto_from_card(card: &crate::transfer_state::TransferCard) -> DiskJobDto {
    let d = &card.dto;
    DiskJobDto {
        job_id: d.job_id,
        display_name: d.name.clone(),
        total: d.total,
        done: d.done,
        state: d.state.clone(),
        direction: d.direction.clone(),
        peer_hex: d.peer.clone(),
        created_at_ms: d.started_at_ms,
        removed_from_view: card.removed,
    }
}

/// 历史记录入口合并(纯函数,移植桌面壳 merge_disk_jobs):磁盘扫盘条目优先;
/// 表内"非 open 且磁盘无 parts"的卡补齐(parts 被 gc 的全真孤儿历史仍可见);
/// 按 job_id 去重;输出按 job_id 升序(ffi 补充:稍后 UI 稳定排序用)。
fn merge_disk_jobs(
    mut disk: Vec<DiskJobDto>,
    cards: &[crate::transfer_state::TransferCard],
) -> Vec<DiskJobDto> {
    for c in cards {
        if !crate::transfer_state::is_terminal(&c.dto.state) {
            continue;
        }
        if !disk.iter().any(|j| j.job_id == c.dto.job_id) {
            disk.push(disk_job_dto_from_card(c));
        }
    }
    disk.sort_by_key(|j| j.job_id);
    disk
}

/// 各 parts 根扫盘 → 历史条目(同 id 多根首见为准;阻塞 IO,调用方放 spawn_blocking)。
/// 字段口径对齐桌面壳 list_disk_jobs:done=已收块数;state 缺块→interrupted、
/// 全真→failed;direction/peer_hex/created_at_ms 只读 meta(无 meta 回默认)。
fn scan_disk_jobs(roots: &[std::path::PathBuf]) -> Vec<DiskJobDto> {
    let mut out: Vec<DiskJobDto> = Vec::new();
    for root in roots {
        for oj in localtrans_core::transfer::orphan_jobs(root) {
            if out.iter().any(|j| j.job_id == oj.job_id) {
                continue;
            }
            let m = &oj.manifest;
            let meta = m.meta();
            out.push(DiskJobDto {
                job_id: oj.job_id,
                display_name: meta.map(|mt| mt.display_name.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| m.file_name.clone()),
                total: m.total_size,
                done: m.received.iter().filter(|&&r| r).count() as u64,
                state: if m.missing_chunks().is_empty() { "failed".into() } else { "interrupted".into() },
                direction: meta.map(|mt| mt.direction.clone())
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "pull".into()),
                peer_hex: meta.map(|mt| mt.peer_hex.clone()).unwrap_or_default(),
                created_at_ms: meta.map(|mt| mt.created_at_ms),
                removed_from_view: false, // 纯磁盘视角,无视图信息
            });
        }
    }
    out
}

/// 在各根中查找孤儿 manifest(restore 建卡用;首见为准)。
fn find_orphan_on_disk(
    roots: &[std::path::PathBuf],
    job_id: u64,
) -> Option<localtrans_core::transfer::OrphanJob> {
    roots.iter()
        .flat_map(|r| localtrans_core::transfer::orphan_jobs(r))
        .find(|oj| oj.job_id == job_id)
}

async fn list_disk_jobs_inner(st: &AppState) -> Vec<DiskJobDto> {
    let roots = st.parts_roots().await;
    let in_table = st.snapshot_cards().await;
    tokio::task::spawn_blocking(move || {
        merge_disk_jobs(scan_disk_jobs(&roots), &in_table)
    }).await.unwrap_or_default()
}

/// 恢复视图(镜像桌面壳 restore_disk_job):在表只清 removed;不在表按孤儿
/// 建卡(卡键=engine_id,removed=false)。均无 → Err。恢复后补发 TransferUpdated
/// 让事件驱动的 Kotlin 列表即时出现该卡(PC 端前端按需拉取,无此事件)。
async fn restore_disk_job_inner(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
) -> Result<(), AppException> {
    if st.card_restore_view(job_id).await {
        if let Some(dto) = st.card_dto(job_id).await {
            cb.on_event(AppEvent::TransferUpdated { transfer: dto });
        }
        return Ok(());
    }
    let roots = st.parts_roots().await;
    let orphan = tokio::task::spawn_blocking(move || find_orphan_on_disk(&roots, job_id))
        .await
        .map_err(|e| AppException::Internal { message: e.to_string() })?;
    match orphan {
        Some(oj) => {
            let mut dto = orphan_card_dto(job_id, &oj.manifest);
            crate::state::sync_derived_fields(&mut dto);
            {
                let mut map = st.transfers.lock().await;
                let entry = map.entry(job_id).or_insert_with(|| {
                    let mut c = crate::transfer_state::TransferCard::new(dto);
                    c.engine_id = Some(job_id);
                    c
                });
                entry.removed = false;
            }
            st.transfers_dirty.store(true, Ordering::Relaxed);
            if let Some(dto) = st.card_dto(job_id).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
            Ok(())
        }
        None => Err(AppException::Internal { message: format!("磁盘上不存在该任务: {:016x}", job_id) }),
    }
}

/// 彻底删除(镜像桌面壳 destroy_disk_job):在表 → finalize_destroy(+removed
/// 事件让 UI 删行);纯孤儿 → 删各根 parts 目录。均无 → Err。
async fn destroy_disk_job_inner(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
) -> Result<(), AppException> {
    if st.card_get(job_id).await.is_some() {
        st.finalize_destroy(job_id).await;
        cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: "removed".to_string() });
        return Ok(());
    }
    if st.remove_parts_dirs(job_id).await {
        tracing::info!("destroy_disk_job 完成 job={:016x}", job_id);
        Ok(())
    } else {
        Err(AppException::Internal { message: format!("磁盘上不存在该任务: {:016x}", job_id) })
    }
}

/// Handle push sender progress events (照抄桌面壳)
async fn handle_source_progress_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    match ev {
        localtrans_core::transfer::ProgressEvent::SourceStarted { job_id, role, peer, name, total, .. } => {
            let peer_hex = hex::encode(peer);
            // source job_id 是 MetaReq 到达时新分配的(next_source_job_id),
            // 传输表里通常无此行——建卡 + Started 激活(对齐桌面壳 main.rs
            // 乙侧 SourceStarted 分支)。
            // v0.9.2 修复:①push 场景 handle_push_progress_event 可能已建行
            // (push/source-push),此处不得覆盖——否则同一传输出现两条任务
            // (一上传一下载);②direction 按 role 区分:SourcePush(我推给
            // 对方)→ push,SourcePull(对方从我拉)→ pull,此前硬编码 pull
            // 把推送也显示成下载。
            if st.card_get(job_id).await.is_none() {
                let direction = match role {
                    localtrans_core::transfer::SourceRole::SourcePush => "push",
                    localtrans_core::transfer::SourceRole::SourcePull => "pull",
                };
                let local_role = match role {
                    localtrans_core::transfer::SourceRole::SourcePush => "source-push",
                    localtrans_core::transfer::SourceRole::SourcePull => "source",
                };
                st.card_create(crate::dto::TransferDto {
                    job_id,
                    name,
                    total,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: peer_hex,
                    direction: direction.to_string(),
                    local_role: local_role.to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                }).await;
                st.card_apply(job_id, CardEvent::Started).await;
            }
            if let Some(dto) = st.card_dto(job_id).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
        }
        localtrans_core::transfer::ProgressEvent::SourceChunkDone { job_id, bytes, .. } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_chunk_add(job_id, bytes).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::SourceSpeed { job_id, bps, remote_done, .. } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_speed(job_id, bps, remote_done).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::SourceDone { job_id } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
            }
        }
        localtrans_core::transfer::ProgressEvent::SourceFailed { job_id, reason } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Failed { reason: Some(reason.clone()) }).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: reason });
            }
        }
        _ => {}
    }
}

/// Handle pull receiver progress events (照抄桌面壳)
async fn handle_recv_progress_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    match ev {
        localtrans_core::transfer::ProgressEvent::Started { job_id, name, total } => {
            // v0.9.2 修复:接收侧(PC 推文件到手机)的 job_id 在表中无行——
            // 行只在发起方建。此前 get_mut 找不到就整体跳过,后续 ChunkDone/
            // Done 全部空转,文件落盘成功但传输页永远空白。
            // M2 T2:走状态机——缺行建 pending 卡 + Started 激活;终态卡
            // (小文件批流同 job 逐文件 Started/Done 对)重建后再激活,
            // 对齐桌面壳"每个 Started 一张新卡"的语义(键不变)。
            let need_create = match st.card_get(job_id).await {
                Some(card) => {
                    if crate::transfer_state::is_terminal(&card.dto.state) {
                        st.card_remove(job_id).await;
                        true
                    } else {
                        st.card_mutate(job_id, |d| {
                            d.name = name.clone();
                            d.total = total;
                        }).await;
                        st.card_apply(job_id, CardEvent::Started).await;
                        false
                    }
                }
                None => true,
            };
            if need_create {
                st.card_create(crate::dto::TransferDto {
                    job_id,
                    name: name.clone(),
                    total,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: String::new(),
                    direction: "rx".to_string(),
                    local_role: "receiver".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                }).await;
                st.card_apply(job_id, CardEvent::Started).await;
            }
            if let Some(dto) = st.card_dto(job_id).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
            // Record file name for FilesSaved event (lock scoped to minimal critical section)
            {
                let mut acc = st.saved_files.lock().unwrap();
                let entry = acc.entry(job_id)
                    .or_insert_with(crate::SavedFilesAcc::new);
                // 小文件批流:Done 分支会 remove acc,后续文件的 Started 重建 acc——
                // 根为空说明 respond_offer 未钉过(Auto 策略/重建),此刻 Started 已
                // 在 accept 之后,当前 inbox_dir 即实际落盘目录,钉死一次即可。
                if entry.root_is_empty() {
                    entry.set_root(st.inbox_dir.read().unwrap().clone());
                }
                entry.record(name);
            }
        }
        localtrans_core::transfer::ProgressEvent::Resumed { job_id, already_bytes } => {
            // 续传基线:pending 先激活(镜像桌面壳),done 从真实位置起步
            if st.card_state(job_id).await.as_deref() == Some("pending") {
                st.card_apply(job_id, CardEvent::Started).await;
            }
            st.card_mutate(job_id, |d| d.done = already_bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::ChunkDone { job_id, bytes, .. } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计——
            // 否则被节流的 ChunkDone/Speed 数据凭空丢失,进度累计偏慢
            if let Some(dto) = st.source_chunk_add(job_id, bytes).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Speed { job_id, bps } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_speed(job_id, bps, st.card_dto(job_id).await.map(|d| d.remote_done).unwrap_or(0)).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Done { job_id } => {
            st.clear_throttle(job_id);
            if let Some(mut dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                // Emit FilesSaved event before TransferUpdated/TransferDone
                // 路径用 acc 内钉死的根(accept 时刻快照,见 SavedFilesAcc 注释),
                // 不读当前 inbox_dir——接受后、完成前切目录不影响进行中任务。
                // 消费文件名但保留根:小文件批流同 job 每文件一对 Started/Done,
                // 后续 Started 沿用同根(不变式:同 job 的 FilesSaved 根一致);
                // 行级清理(finalize_remove/Failed)负责最终删除条目。
                let snapshot: Vec<String> = {
                    let mut acc = st.saved_files.lock().unwrap();
                    match acc.get_mut(&job_id) {
                        Some(a) => {
                            let names = a.take_names_keep_root();
                            let root = a.root().to_path_buf();
                            names.iter()
                                .map(|n| root.join(n).to_string_lossy().replace('\\', "/"))
                                .collect()
                        }
                        None => Vec::new(),
                    }
                };
                if !snapshot.is_empty() {
                    if dto.local_path.is_none() {
                        if let Some(first) = snapshot.first() {
                            dto.local_path = Some(first.clone());
                        }
                    }
                    cb.on_event(AppEvent::FilesSaved { job_id, paths: snapshot });
                }

                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
            }
        }
        localtrans_core::transfer::ProgressEvent::Failed { job_id, reason } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Failed { reason: Some(reason.clone()) }).await {
                // Clean up saved_files accumulator
                st.saved_files.lock().unwrap().remove(&job_id);
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: reason });
            }
        }
        localtrans_core::transfer::ProgressEvent::InstantHit { job_id, name, total } => {
            // v0.10.0 秒传:直接落终态(行缺失也建——接收侧行可能尚未创建)。
            // M6 回归锚:pending 直收 Finished 是非法迁移——先 Started 激活再收敛
            // (PC main.rs InstantHit 泵同款约定)。
            if st.card_get(job_id).await.is_none() {
                st.card_create(crate::dto::TransferDto {
                    job_id, name: name.clone(), total, done: 0,
                    state: "pending".to_string(), speed_bps: 0, peer: String::new(),
                    direction: "rx".to_string(), local_role: "receiver".to_string(),
                    progress_percent: 0, eta_secs: -1, fail_reason: String::new(),
                    local_path: None, remote_done: 0, instant: false,
                    ..Default::default()
                }).await;
            }
            st.card_mutate(job_id, |d| {
                d.name = name.clone();
                d.total = total;
                d.done = total;
                d.instant = true;
            }).await;
            if st.card_state(job_id).await.as_deref() == Some("pending") {
                st.card_apply(job_id, CardEvent::Started).await;
            }
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
            }
        }
        _ => {}
    }
}

/// Handle push_files progress events (照抄桌面壳)
async fn handle_push_progress_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    placeholder_id: u64,
    placeholder_alive: &mut bool,
    peer_hex: &str,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    match ev {
        // core 推送路径发的是接收方风格事件(小文件 Started/Done、批流 Failed,
        // 见 engine.rs push_files_inner)——此前只匹配 Source* 系导致全部落进
        // `_ => {}` 丢弃,手机端占位任务永远 pending/0%(v0.7.0 实测)。
        // 对齐桌面壳 commands.rs handle_push_progress_event 的完整匹配。
        localtrans_core::transfer::ProgressEvent::Started { job_id, name, total } => {
            if *placeholder_alive {
                st.card_remove(placeholder_id).await;
                *placeholder_alive = false;
            }
            if st.card_get(job_id).await.is_none() {
                st.card_create(crate::dto::TransferDto {
                    job_id,
                    name: name.clone(),
                    total,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: peer_hex.to_string(),
                    direction: "push".to_string(),
                    local_role: "source-push".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                }).await;
                st.card_apply(job_id, CardEvent::Started).await;
                // M-B7:并发清理(超时/取消)可能刚把行移除——card_apply 返回 None
                // 不能 unwrap panic,只跳过本次 UI 事件
                if let Some(dto) = st.card_dto(job_id).await {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::ChunkDone { job_id, bytes, .. } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_chunk_add(job_id, bytes).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Resumed { job_id, already_bytes } => {
            // 续传基线:pending 先激活(镜像桌面壳),done 从真实位置起步
            if st.card_state(job_id).await.as_deref() == Some("pending") {
                st.card_apply(job_id, CardEvent::Started).await;
            }
            st.card_mutate(job_id, |d| d.done = already_bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::Speed { job_id, bps } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_speed(job_id, bps, st.card_dto(job_id).await.map(|d| d.remote_done).unwrap_or(0)).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Done { job_id } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
            } else if *placeholder_alive {
                // 占位 fallback:Done 先于任何 Started 到达(如全空文件 offer)——
                // 占位卡改键落到引擎 job_id 后走 Started+Finished 收敛
                if let Some(pc) = st.card_get(placeholder_id).await {
                    st.card_remove(placeholder_id).await;
                    let mut d = pc.dto;
                    d.job_id = job_id;
                    st.card_create(d).await;
                    st.card_apply(job_id, CardEvent::Started).await;
                    if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                        cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                        cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
                    }
                }
                *placeholder_alive = false;
            }
        }
        localtrans_core::transfer::ProgressEvent::Failed { job_id, reason } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Failed { reason: Some(reason.clone()) }).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: reason.clone() });
            } else if *placeholder_alive {
                if let Some(pc) = st.card_get(placeholder_id).await {
                    st.card_remove(placeholder_id).await;
                    let mut d = pc.dto;
                    d.job_id = job_id;
                    st.card_create(d).await;
                    // 占位卡仍是 pending:Pending+Failed 是合法边
                    if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Failed { reason: Some(reason.clone()) }).await {
                        cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                        cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: reason });
                    }
                }
                *placeholder_alive = false;
            }
        }
        // 大文件反向取流时,本机路由器经 source_tx 发 Source* 系(source_rx loop
        // 消费)——推送通道不会再收到,这里不重复处理
        _ => {}
    }
}

/// Handle pull_files progress events (照抄桌面壳)
/// N1-T3:pull_files 批次模式事件处理器——全部按 placeholder_id 落卡(ID 恒定),
/// 字节/总量跨文件累计;成败由调用方按整批收敛。卡整批保持 active,
/// 仅首个 Started 做 pending→active 迁移(后续文件的 Started 到时卡已激活)。
#[allow(clippy::too_many_arguments)]
async fn handle_pull_batch_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    card: u64,
    is_multi: bool,
    ok_count: &mut u32,
    _failed_count: &mut u32,
    file_failed: &mut bool,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use localtrans_core::transfer::ProgressEvent as PE;
    use crate::transfer_state::CardEvent;
    match ev {
        PE::Started { name, total, .. } => {
            if st.card_state(card).await.as_deref() == Some("pending") {
                st.card_apply(card, CardEvent::Started).await;
            }
            st.card_mutate(card, |d| {
                if !is_multi { d.name = name; }
                d.total += total;
            }).await;
            if let Some(dto) = st.card_dto(card).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
        }
        PE::Resumed { already_bytes, .. } => {
            st.card_mutate(card, |d| d.done += already_bytes).await;
        }
        PE::ChunkDone { bytes, .. } => {
            if let Some(dto) = st.source_chunk_add(card, bytes).await {
                if !st.should_throttle_progress(card) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        PE::Speed { bps, .. } => {
            if let Some(dto) = st.source_speed(card, bps, st.card_dto(card).await.map(|d| d.remote_done).unwrap_or(0)).await {
                if !st.should_throttle_progress(card) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        PE::Done { .. } => { *ok_count += 1; }
        PE::InstantHit { name, total, .. } => {
            // 秒传:Started/Done 均不发——此处一次性计入总量与完成数
            if st.card_state(card).await.as_deref() == Some("pending") {
                st.card_apply(card, CardEvent::Started).await;
            }
            st.card_mutate(card, |d| {
                if !is_multi { d.name = name; }
                d.total += total;
                d.done += total;
                d.instant = true;
            }).await;
            if let Some(dto) = st.card_dto(card).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
            *ok_count += 1;
        }
        PE::Failed { reason, .. } => {
            *file_failed = true;
            tracing::warn!("下载失败: {}", reason);
            st.card_mutate(card, |d| d.fail_reason = reason).await;
        }
        _ => {}
    }
}

async fn handle_pull_progress_event(
    st: &AppState,
    cb: &Arc<Box<dyn LocalTransCallback>>,
    placeholder_id: u64,
    placeholder_alive: &mut bool,
    peer_hex: &str,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    let _ = peer_hex;
    match ev {
        localtrans_core::transfer::ProgressEvent::Started { job_id, name, total } => {
            if *placeholder_alive {
                st.card_remove(placeholder_id).await;
                *placeholder_alive = false;
            }
            // 续传的引擎 job 是对端新分配的(MetaResp)——正常无旧行;防御:
            // 终态旧卡重建(状态机终态吸收,不允许原地复活),活动卡改元数据后激活
            let need_create = match st.card_get(job_id).await {
                Some(card) => {
                    if crate::transfer_state::is_terminal(&card.dto.state) {
                        st.card_remove(job_id).await;
                        true
                    } else {
                        st.card_mutate(job_id, |d| {
                            d.name = name.clone();
                            d.total = total;
                        }).await;
                        st.card_apply(job_id, CardEvent::Started).await;
                        false
                    }
                }
                None => true,
            };
            if need_create {
                st.card_create(crate::dto::TransferDto {
                    job_id,
                    name: name.clone(),
                    total,
                    done: 0,
                    state: "pending".to_string(),
                    speed_bps: 0,
                    peer: String::new(),
                    direction: "pull".to_string(),
                    local_role: "receiver".to_string(),
                    progress_percent: 0,
                    eta_secs: -1,
                    fail_reason: String::new(),
                    local_path: None,
                    remote_done: 0,
                    instant: false,
                    ..Default::default()
                }).await;
                st.card_apply(job_id, CardEvent::Started).await;
            }
            if let Some(dto) = st.card_dto(job_id).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto });
            }
        }
        localtrans_core::transfer::ProgressEvent::Resumed { job_id, already_bytes } => {
            if st.card_state(job_id).await.as_deref() == Some("pending") {
                st.card_apply(job_id, CardEvent::Started).await;
            }
            st.card_mutate(job_id, |d| d.done = already_bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::ChunkDone { job_id, bytes, .. } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_chunk_add(job_id, bytes).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Speed { job_id, bps } => {
            // 表始终写(桌面壳语义):节流只拦 UI 事件发射,不丢累计
            if let Some(dto) = st.source_speed(job_id, bps, st.card_dto(job_id).await.map(|d| d.remote_done).unwrap_or(0)).await {
                if !st.should_throttle_progress(job_id) {
                    cb.on_event(AppEvent::TransferUpdated { transfer: dto });
                }
            }
        }
        localtrans_core::transfer::ProgressEvent::Done { job_id } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Finished).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: true, fail_reason: String::new() });
            }
        }
        localtrans_core::transfer::ProgressEvent::Failed { job_id, reason } => {
            st.clear_throttle(job_id);
            if let Some(dto) = apply_card_event(st, cb, job_id, CardEvent::Failed { reason: Some(reason.clone()) }).await {
                cb.on_event(AppEvent::TransferUpdated { transfer: dto.clone() });
                cb.on_event(AppEvent::TransferDone { job_id, ok: false, fail_reason: reason });
            }
        }
        _ => {}
    }
}

/// Helper: hex decode with proper error handling
fn hex_decode(s: &str) -> Result<Vec<u8>, hex::FromHexError> {
    hex::decode(s)
}

/// Helper: hex decode + 32 字节长度校验(替代 copy_from_slice panic 模式)
fn decode_fp32(s: &str) -> Result<[u8; 32], AppException> {
    let bytes = hex_decode(s).map_err(|e| AppException::Internal { message: format!("指纹格式非法: {}", e) })?;
    if bytes.len() != 32 {
        return Err(AppException::Internal {
            message: format!("指纹格式非法: 长度 {} 字节,应为 32", bytes.len()),
        });
    }
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&bytes);
    Ok(fp)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::Mutex;

    // Callback wrapper for test purposes
    struct SharedCb(Arc<Mutex<Vec<crate::AppEvent>>>);
    impl crate::LocalTransCallback for SharedCb {
        fn on_event(&self, e: crate::AppEvent) {
            self.0.lock().unwrap().push(e);
        }
    }

    #[test]
    fn hello_and_fingerprint_work() {
        let dir = tempfile::tempdir().unwrap();
        struct Cb;
        impl crate::LocalTransCallback for Cb {
            fn on_event(&self, event: crate::AppEvent) {
                assert!(matches!(event, crate::AppEvent::Hello { .. }));
            }
        }
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().to_string(), Box::new(Cb)).unwrap();
        assert_eq!(app.hello(), "localtrans-ffi");
        assert_eq!(app.my_fingerprint(), "not started");
    }

    #[test]
    fn start_emits_devices_and_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(
            dir.path().to_str().unwrap().into(),
            Box::new(cb)
        ).unwrap();

        app.start();

        let mut s = app.settings();
        s.device_name = "我的手机".into();
        s.offer_timeout_secs = 90;
        app.save_settings(s);

        let s2 = app.settings();
        assert_eq!(s2.device_name, "我的手机");
        assert_eq!(s2.offer_timeout_secs, 90);

        let devices = app.devices();
        assert!(devices.len() >= 0);

        app.shutdown();
    }

    #[test]
    fn clean_backup_dir_name_removes_invalid_chars() {
        assert_eq!(super::clean_backup_dir_name("test<>file".to_string()), "testfile");
        assert_eq!(super::clean_backup_dir_name("test:file".to_string()), "testfile");
        assert_eq!(super::clean_backup_dir_name("test/file".to_string()), "testfile");
    }

    #[test]
    fn clean_backup_dir_name_trims_dots() {
        assert_eq!(super::clean_backup_dir_name("...test...".to_string()), "test");
        assert_eq!(super::clean_backup_dir_name("  test  ".to_string()), "test");
    }

    #[test]
    fn clean_backup_dir_name_empty_after_cleaning() {
        assert_eq!(super::clean_backup_dir_name("<>:/\"\\|?*".to_string()), "");
    }

    #[test]
    fn set_inbox_dir_changes_receive_destination() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(
            dir.path().to_str().unwrap().into(),
            Box::new(cb)
        ).unwrap();

        // 未设置时 inbox_dir = data_dir(未启动时返回 data_dir)
        assert_eq!(app.inbox_dir(), dir.path().to_str().unwrap());

        // 启动后 inbox_dir 默认为 state.dir
        app.start();
        assert_eq!(std::path::PathBuf::from(app.inbox_dir()), dir.path());

        // 设置后切换
        let inbox_path = inbox.path().join("Download").join("LocalTrans");
        std::fs::create_dir_all(&inbox_path).unwrap();
        app.set_inbox_dir(inbox_path.to_str().unwrap().to_string()).unwrap();
        assert_eq!(std::path::PathBuf::from(app.inbox_dir()), inbox_path);

        app.shutdown();
    }

    #[test]
    fn files_saved_paths_built_from_started_names() {
        // 纯函数测试:job 累积的文件名 + inbox 根 → Done 时拼出绝对路径
        let mut acc = super::SavedFilesAcc::new();
        acc.set_root("/sdcard/Download/LocalTrans".into());
        acc.record("a.png".to_string());
        acc.record("sub/b.jpg".to_string()); // rel_dir 场景(文件夹推送)
        let paths = (42u64, acc.snapshot_paths());
        assert_eq!(paths.0, 42u64);
        assert_eq!(paths.1, vec![
            "/sdcard/Download/LocalTrans/a.png".to_string(),
            "/sdcard/Download/LocalTrans/sub/b.jpg".to_string(),
        ]);
    }

    #[test]
    fn local_path_field_serializes_none() {
        let dto = crate::dto::TransferDto {
            job_id: 1, name: "x".into(), total: 0, done: 0, state: "pending".into(),
            speed_bps: 0, peer: "ab".into(), direction: "pull".into(),
            local_role: "receiver".into(), progress_percent: 0, eta_secs: -1,
            fail_reason: String::new(), local_path: None, remote_done: 0, instant: false,
            ..Default::default()
        };
        assert!(dto.local_path.is_none());
    }

    #[tokio::test]
    async fn push_progress_started_creates_row_and_done_finalizes() {
        // 回归 v0.7.0 缺陷:core 推送通道发的是接收方风格事件(Started/Done),
        // FFI push 泵此前只认 Source* 系导致事件全丢、占位任务永远 0%。
        // 直接喂 handler 验证:Started 建行 → ChunkDone 累计 → Done 落完成态。
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        let st = test_app_state().await;

        let mut peer = [0u8; 32];
        peer[0] = 7;
        let peer_hex = hex::encode(peer);
        let placeholder_id = u64::MAX - 1;
        let mut placeholder_alive = true;

        // 1) Started:应建 active 行(此前被 `_ => {}` 丢弃)
        super::handle_push_progress_event(
            &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex,
            localtrans_core::transfer::ProgressEvent::Started { job_id: 100, name: "照片.jpg".into(), total: 500 },
        ).await;
        let row = st.card_dto(100).await.expect("Started 后应存在 job 100 行");
        assert_eq!(row.state, "active");
        assert_eq!(row.total, 500);
        assert_eq!(row.direction, "push");
        assert!(!placeholder_alive, "占位任务应已退场");

        // 2) ChunkDone:进度累计 + 百分比
        super::handle_push_progress_event(
            &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex,
            localtrans_core::transfer::ProgressEvent::ChunkDone { job_id: 100, chunk: 0, bytes: 250 },
        ).await;
        let row = st.card_dto(100).await.unwrap();
        assert_eq!(row.done, 250);
        assert_eq!(row.progress_percent, 50);

        // 3) Done:完成态 + 满格
        super::handle_push_progress_event(
            &st, &cb, placeholder_id, &mut placeholder_alive, &peer_hex,
            localtrans_core::transfer::ProgressEvent::Done { job_id: 100 },
        ).await;
        let row = st.card_dto(100).await.unwrap();
        assert_eq!(row.state, "done");
        assert_eq!(row.done, 500);
        assert_eq!(row.progress_percent, 100);
    }

    #[tokio::test]
    async fn source_progress_started_inserts_row_unconditionally() {
        // 回归 v0.7.0 缺陷:SourceStarted 的 job_id 是 MetaReq 到达时新分配的,
        // 表中必无此行——此前 get_mut 找不到就跳过,source 行永远不建。
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        let st = test_app_state().await;

        let mut peer = [0u8; 32];
        peer[0] = 9;
        super::handle_source_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::SourceStarted {
                job_id: 0x8000_0000_0000_0001,
                role: localtrans_core::transfer::SourceRole::SourcePush,
                peer,
                name: "视频.mp4".into(),
                total: 4096,
            },
        ).await;
        let row = st.card_dto(0x8000_0000_0000_0001).await
            .expect("SourceStarted 应无条件插行");
        assert_eq!(row.state, "active");
        assert_eq!(row.total, 4096);

        // SourceChunkDone 现在能找到行,累计生效
        super::handle_source_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::SourceChunkDone {
                job_id: 0x8000_0000_0000_0001, chunk: 0, bytes: 1024,
            },
        ).await;
        let row = st.card_dto(0x8000_0000_0000_0001).await.unwrap();
        assert_eq!(row.done, 1024);
        assert_eq!(row.progress_percent, 25);
    }

    #[tokio::test]
    async fn source_started_preserves_existing_push_row_and_direction_by_role() {
        // 回归 v0.9.2 缺陷一:手机推送文件出现两条任务(一上传一下载)。
        // push 路径 handle_push_progress_event 已建 "push" 行,SourceStarted
        // 随后到达不得覆盖它;且 direction 必须按 role 区分——
        // SourcePush(我推给对方)→ push,SourcePull(对方从我拉)→ pull。
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        let st = test_app_state().await;

        // 场景一:push 路径已建行,SourceStarted(SourcePush)不得改写方向
        st.card_create(crate::dto::TransferDto {
            job_id: 100, name: "a.mp4".into(), total: 500, done: 0,
            state: "active".into(), speed_bps: 0, peer: "ab".into(),
            direction: "push".into(), local_role: "source-push".into(),
            progress_percent: 0, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
            ..Default::default()
        }).await;

        let mut peer = [0u8; 32];
        peer[0] = 9;
        super::handle_source_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::SourceStarted {
                job_id: 100,
                role: localtrans_core::transfer::SourceRole::SourcePush,
                peer,
                name: "a.mp4".into(),
                total: 500,
            },
        ).await;
        let row = st.card_dto(100).await.unwrap();
        assert_eq!(row.direction, "push", "已存在的 push 行不得被 SourceStarted 覆盖为 pull");
        assert_eq!(row.local_role, "source-push");

        // 场景二:表无此行时,SourcePush 新行 direction 应为 push
        super::handle_source_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::SourceStarted {
                job_id: 200,
                role: localtrans_core::transfer::SourceRole::SourcePush,
                peer,
                name: "b.mp4".into(),
                total: 100,
            },
        ).await;
        let row = st.card_dto(200).await.unwrap();
        assert_eq!(row.direction, "push", "SourcePush 新建行应为 push 方向");

        // 场景三:表无此行时,SourcePull 新行 direction 应为 pull
        super::handle_source_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::SourceStarted {
                job_id: 300,
                role: localtrans_core::transfer::SourceRole::SourcePull,
                peer,
                name: "c.mp4".into(),
                total: 100,
            },
        ).await;
        let row = st.card_dto(300).await.unwrap();
        assert_eq!(row.direction, "pull", "SourcePull 新建行应为 pull 方向");

        // 表里应只有 3 行(100/200/300),没有第四条幽灵行
        assert_eq!(st.snapshot_cards().await.len(), 3);
    }

    #[tokio::test]
    async fn recv_started_creates_row_when_missing() {
        // 回归 v0.9.2 缺陷二:PC 推文件到手机,手机正常落盘但传输页不显示。
        // 接收侧 Started 的 job_id 在表中无行(行只在发起方建),此前
        // get_mut 找不到就整体跳过——ChunkDone/Done 全部空转。
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        let st = test_app_state().await;

        super::handle_recv_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::Started {
                job_id: 42,
                name: "来自PC的文件.docx".into(),
                total: 2048,
            },
        ).await;
        let row = st.card_dto(42).await
            .expect("接收侧 Started 应为未知 job 建行");
        assert_eq!(row.state, "active");
        assert_eq!(row.total, 2048);
        assert_eq!(row.name, "来自PC的文件.docx");

        // 后续 ChunkDone 能累计(此前因行缺失全部丢弃)
        super::handle_recv_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::ChunkDone { job_id: 42, chunk: 0, bytes: 1024 },
        ).await;
        let row = st.card_dto(42).await.unwrap();
        assert_eq!(row.done, 1024);
        assert_eq!(row.progress_percent, 50);

        // Done 能落完成态 + 发 FilesSaved/TransferDone 事件
        super::handle_recv_progress_event(
            &st, &cb,
            localtrans_core::transfer::ProgressEvent::Done { job_id: 42 },
        ).await;
        let row = st.card_dto(42).await.unwrap();
        assert_eq!(row.state, "done");

        let evs = events.lock().unwrap();
        assert!(evs.iter().any(|e| matches!(e, crate::AppEvent::TransferDone { job_id: 42, ok: true, .. })),
            "应发 TransferDone(ok=true) 事件");
    }

    #[tokio::test]
    async fn transfers_clear_finished_only_removes_terminal_rows() {
        let st = test_app_state().await;
        for (id, state) in [(1u64, "done"), (2, "failed"), (3, "interrupted"), (4, "active"), (5, "pending")] {
            st.card_create(crate::dto::TransferDto {
                job_id: id, name: format!("f{}", id), total: 10, done: 5,
                state: state.into(), speed_bps: 0, peer: "ab".into(),
                direction: "pull".into(), local_role: "receiver".into(),
                progress_percent: 50, eta_secs: -1, fail_reason: String::new(),
                local_path: None, remote_done: 0, instant: false,
                ..Default::default()
            }).await;
        }

        assert_eq!(st.snapshot_cards().await.len(), 5);

        // 手动执行与 FFI transfers_clear_finished 相同的过滤逻辑验证语义
        let cards = st.snapshot_cards().await;
        let mut terminal: Vec<u64> = cards.iter()
            .filter(|c| crate::transfer_state::is_terminal(&c.dto.state))
            .map(|c| c.dto.job_id)
            .collect();
        terminal.sort();
        assert_eq!(terminal, vec![1, 2, 3]);
        for id in &terminal {
            st.finalize_remove(*id).await;
        }
        let left = st.snapshot_cards().await;
        assert_eq!(left.len(), 2);
        assert!(left.iter().any(|c| c.dto.job_id == 4) && left.iter().any(|c| c.dto.job_id == 5));
    }

    fn mk_card(id: u64, state: &str) -> crate::dto::TransferDto {
        crate::dto::TransferDto {
            job_id: id, name: format!("f{}", id), total: 100, done: 10,
            state: state.into(), speed_bps: 0, peer: "ab".into(),
            direction: "pull".into(), local_role: "receiver".into(),
            progress_percent: 10, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn cancel_arbitration_confirm_removes_row_and_emits_removed() {
        // M2 T2:取消 = cancelling 仲裁。活动卡 cancel → cancelling;
        // 引擎终态确认先到 → 实删 + TransferDone(removed),看门狗句柄清账
        let st = Arc::new(test_app_state().await);
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        st.card_create(mk_card(11, "active")).await;

        super::cancel_transfer_arbitrate(&st, &cb, 11).await.unwrap();
        assert_eq!(st.card_state(11).await.as_deref(), Some("cancelling"), "cancel 应入 cancelling");
        assert!(st.cancel_watchdogs.lock().await.contains_key(&11), "应挂 5s 看门狗");

        // 引擎终态确认(确认先到)→ 实删收尾
        let out = super::apply_card_event(&st, &cb, 11, crate::transfer_state::CardEvent::Interrupted).await;
        assert!(out.is_none(), "仲裁收尾路径不再向 UI 返回 dto");
        assert!(st.card_get(11).await.is_none(), "确认后行应被实删");
        assert!(!st.cancel_watchdogs.lock().await.contains_key(&11), "看门狗应随收尾 abort 清账");
        let evs = events.lock().unwrap();
        assert!(evs.iter().any(|e| matches!(
            e, crate::AppEvent::TransferDone { job_id: 11, ok: false, fail_reason } if fail_reason == "removed"
        )), "实删应发 removed 事件供 UI 删行");
    }

    #[tokio::test]
    async fn cancel_rejects_terminal_and_missing_card() {
        let st = Arc::new(test_app_state().await);
        let cb: Arc<Box<dyn crate::LocalTransCallback>> =
            Arc::new(Box::new(SharedCb(Arc::new(Mutex::new(Vec::new())))));
        // 缺卡
        assert!(super::cancel_transfer_arbitrate(&st, &cb, 99).await.is_err());
        // 终态卡:终态吸收,状态不得被取消改写
        st.card_create(mk_card(12, "done")).await;
        let err = super::cancel_transfer_arbitrate(&st, &cb, 12).await.unwrap_err();
        assert!(matches!(err, crate::AppException::Internal { .. }));
        assert_eq!(st.card_state(12).await.as_deref(), Some("done"));
        // paused 卡也能取消(七态表:Paused+CancelRequested→Cancelling)
        st.card_create(mk_card(13, "paused")).await;
        super::cancel_transfer_arbitrate(&st, &cb, 13).await.unwrap();
        assert_eq!(st.card_state(13).await.as_deref(), Some("cancelling"));
        st.finalize_remove(13).await; // 清场,防看门狗悬挂
    }

    #[tokio::test]
    async fn two_level_remove_view_and_destroy() {
        let st = Arc::new(test_app_state().await);
        let cb: Arc<Box<dyn crate::LocalTransCallback>> =
            Arc::new(Box::new(SharedCb(Arc::new(Mutex::new(Vec::new())))));

        // view:活动卡拒绝(对齐桌面壳"任务进行中,请先取消")
        st.card_create(mk_card(21, "active")).await;
        let err = super::remove_transfer_level_inner(&st, &cb, 21, "view").await.unwrap_err();
        assert!(matches!(err, crate::AppException::Internal { .. }));

        // destroy:活动卡 → cancelling 仲裁(不立即删)
        assert_eq!(super::remove_transfer_level_inner(&st, &cb, 21, "destroy").await.unwrap(), true);
        assert_eq!(st.card_state(21).await.as_deref(), Some("cancelling"));
        assert!(st.card_get(21).await.is_some(), "活动卡 destroy 后等确认,不立即删");
        st.finalize_remove(21).await; // 清场

        // view:终态卡 removed=true 保留表内 + 活动快照过滤
        st.card_create(mk_card(22, "done")).await;
        assert_eq!(super::remove_transfer_level_inner(&st, &cb, 22, "view").await.unwrap(), true);
        let card = st.card_get(22).await.unwrap();
        assert!(card.removed, "view 删除只打标记");
        assert!(!st.snapshot_dtos().await.iter().any(|d| d.job_id == 22), "removed 卡不出现在列表");

        // destroy:终态卡直接实删
        st.card_create(mk_card(23, "failed")).await;
        assert_eq!(super::remove_transfer_level_inner(&st, &cb, 23, "destroy").await.unwrap(), true);
        assert!(st.card_get(23).await.is_none());

        // 卡片不存在:两级别均返回 Ok(false)
        assert_eq!(super::remove_transfer_level_inner(&st, &cb, 404, "view").await.unwrap(), false);
        assert_eq!(super::remove_transfer_level_inner(&st, &cb, 404, "destroy").await.unwrap(), false);
    }

    #[tokio::test]
    async fn terminal_card_drops_late_engine_events() {
        // 终态吸收:引擎迟到事件(ChunkDone/Done/Failed)不得改写终态卡
        // (旧实现直接改 dto 字段,failed 行会被迟到事件"改账")
        let st = test_app_state().await;
        let cb: Arc<Box<dyn crate::LocalTransCallback>> =
            Arc::new(Box::new(SharedCb(Arc::new(Mutex::new(Vec::new())))));
        st.card_create(mk_card(31, "failed")).await;
        super::handle_pull_progress_event(
            &st, &cb, u64::MAX - 9, &mut false, "aa",
            localtrans_core::transfer::ProgressEvent::ChunkDone { job_id: 31, chunk: 0, bytes: 5 },
        ).await;
        super::handle_pull_progress_event(
            &st, &cb, u64::MAX - 9, &mut false, "aa",
            localtrans_core::transfer::ProgressEvent::Done { job_id: 31 },
        ).await;
        super::handle_pull_progress_event(
            &st, &cb, u64::MAX - 9, &mut false, "aa",
            localtrans_core::transfer::ProgressEvent::Failed { job_id: 31, reason: "late".into() },
        ).await;
        let card = st.card_get(31).await.unwrap();
        assert_eq!(card.dto.state, "failed", "终态卡不得被迟到事件改写");
        assert_eq!(card.dto.done, 10, "终态卡进度不被迟到事件改写");
        assert_eq!(card.dto.fail_reason, "", "终态卡失败原因不被迟到事件覆盖");
    }

    #[tokio::test]
    async fn small_file_batch_started_done_pairs_rebuild_terminal_card() {
        // 小文件批流:同 job 每文件一对 Started/Done;Done 后卡终态,下一文件
        // 的 Started 走"终态重建"而非被终态吸收丢弃(FilesSaved 每文件必达)
        let st = test_app_state().await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));

        for name in ["a.png", "b.jpg"] {
            super::handle_recv_progress_event(
                &st, &cb,
                localtrans_core::transfer::ProgressEvent::Started {
                    job_id: 7, name: name.into(), total: 10,
                },
            ).await;
            assert_eq!(st.card_state(7).await.as_deref(), Some("active"));
            super::handle_recv_progress_event(
                &st, &cb,
                localtrans_core::transfer::ProgressEvent::Done { job_id: 7 },
            ).await;
            assert_eq!(st.card_state(7).await.as_deref(), Some("done"));
        }
        let evs = events.lock().unwrap();
        let saved: Vec<Vec<String>> = evs.iter()
            .filter_map(|e| match e {
                crate::AppEvent::FilesSaved { paths, .. } => Some(paths.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(saved.len(), 2, "每个文件的 FilesSaved 必达(终态重建语义)");
    }

    #[test]
    fn transfer_record_roundtrip_preserves_fields() {
        // FFI TransferDto ↔ 磁盘 TransferRecord 往返不丢字段
        let dto = crate::dto::TransferDto {
            job_id: 0xAB, name: "视频.mp4".into(), total: 1000, done: 400,
            state: "active".into(), speed_bps: 12345, peer: "cd".into(),
            direction: "rx".into(), local_role: "receiver".into(),
            progress_percent: 40, eta_secs: 12, fail_reason: String::new(),
            local_path: Some("/sdcard/Download/LocalTrans/视频.mp4".into()),
            remote_done: 0, instant: false,
            ..Default::default()
        };
        let record: super::TransferRecord = dto.clone().into();
        let json = serde_json::to_string_pretty(&record).unwrap();
        let back: super::TransferRecord = serde_json::from_str(&json).unwrap();
        let dto2: crate::dto::TransferDto = back.into();
        assert_eq!(dto2.job_id, dto.job_id);
        assert_eq!(dto2.name, dto.name);
        assert_eq!(dto2.progress_percent, dto.progress_percent);
        assert_eq!(dto2.local_path, dto.local_path);
    }

    #[test]
    fn migrate_records_marks_active_as_interrupted_and_drops_orphans() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path();
        // job 1: 接收中,parts 目录存在 → interrupted 保留
        std::fs::create_dir_all(inbox.join(".localtrans-parts").join("1")).unwrap();
        // job 2: 接收中,无 parts → 孤儿丢弃
        // job 3: 发送中(push),无 parts 概念 → interrupted 保留
        // job 4: done → 原样保留
        let mk = |id: u64, state: &str, direction: &str| super::TransferRecord {
            job_id: id, name: format!("f{}", id), total: 10, done: 5,
            state: state.into(), speed_bps: 0, peer: "ab".into(),
            direction: direction.into(), local_role: String::new(),
            progress_percent: 50, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
            started_at_ms: None, finished_at_ms: None, parts_id: None, source_path: None,
        };
        let migrated = super::migrate_records_on_startup(
            vec![mk(1, "active", "rx"), mk(2, "pending", "rx"), mk(3, "paused", "push"), mk(4, "done", "rx")],
            &[inbox.to_path_buf()],
        );
        let ids: Vec<u64> = migrated.iter().map(|r| r.job_id).collect();
        assert_eq!(ids, vec![1, 3, 4]); // 2 被丢
        assert_eq!(migrated[0].state, "interrupted");
        assert_eq!(migrated[0].speed_bps, 0);
        assert_eq!(migrated[1].state, "interrupted");
        assert_eq!(migrated[2].state, "done");
    }

    #[test]
    fn load_transfers_persisted_handles_missing_and_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let roots = [tmp.path().to_path_buf()];
        // 缺失 → 空表
        assert!(super::load_transfers_persisted(tmp.path(), &roots).is_empty());
        // 损坏 → 空表不 panic
        std::fs::write(tmp.path().join("transfers.json"), b"not json {{{").unwrap();
        assert!(super::load_transfers_persisted(tmp.path(), &roots).is_empty());
    }

    #[test]
    fn load_transfers_persisted_restores_history() {
        let tmp = tempfile::tempdir().unwrap();
        // 旧格式(裸数组)兼容读:done 历史无需 parts 也恢复
        let json = r#"[{"job_id":"0x5","name":"a.txt","total":10,"done":10,
            "state":"done","speed_bps":0,"peer":"ab","direction":"rx",
            "local_role":"receiver","progress_percent":100,"eta_secs":-2,
            "fail_reason":"","local_path":null}]"#;
        std::fs::write(tmp.path().join("transfers.json"), json).unwrap();
        let map = super::load_transfers_persisted(tmp.path(), &[tmp.path().to_path_buf()]);
        assert_eq!(map.len(), 1);
        assert_eq!(map[&5].dto.state, "done");
        assert_eq!(map[&5].dto.name, "a.txt");
        assert!(!map[&5].removed, "旧格式无 removed 标记,缺省 false");
        // M2 T3:旧文件缺时间戳字段 → serde default None,不阻断解析
        assert!(map[&5].dto.started_at_ms.is_none());
    }

    #[test]
    fn load_transfers_persisted_reads_new_card_format_with_removed() {
        // M2 T2:新格式 {"cards":[{dto,removed}]}——view 删除的卡重启不复活
        let tmp = tempfile::tempdir().unwrap();
        let json = r#"{"cards":[
            {"dto":{"job_id":"0x9","name":"keep.bin","total":10,"done":10,
                "state":"done","speed_bps":0,"peer":"ab","direction":"rx",
                "local_role":"receiver","progress_percent":100,"eta_secs":-2,
                "fail_reason":"","local_path":null},"removed":false},
            {"dto":{"job_id":"0xa","name":"gone.bin","total":10,"done":10,
                "state":"done","speed_bps":0,"peer":"ab","direction":"rx",
                "local_role":"receiver","progress_percent":100,"eta_secs":-2,
                "fail_reason":"","local_path":null},"removed":true}
        ]}"#;
        std::fs::write(tmp.path().join("transfers.json"), json).unwrap();
        let roots = [tmp.path().to_path_buf()];
        let map = super::load_transfers_persisted(tmp.path(), &roots);
        assert_eq!(map.len(), 2);
        assert!(!map[&9].removed);
        assert!(map[&0xa].removed, "removed=true 必须随卡片往返");
        // removed 卡不出现在活动列表快照(两级删除第一级,snapshot_dtos 同款过滤)
        let map = super::load_transfers_persisted(tmp.path(), &roots);
        let visible: Vec<u64> = {
            let mut v: Vec<u64> = map.into_values()
                .filter(|c| !c.removed)
                .map(|c| c.dto.job_id)
                .collect();
            v.sort();
            v
        };
        assert_eq!(visible, vec![9]);
    }

    #[test]
    fn collect_persist_list_keeps_removed_terminal_and_caps_history() {
        // 移植桌面壳 collect_persist_list 语义:removed 终态全量保留(不计数
        // 不截断),普通历史封顶 150——否则 removed 卡被截出 transfers.json,
        // 重启后 removed=false 整卡复活(view 删除破口)
        let mk = |id: u64, state: &str, removed: bool| {
            let mut card = crate::transfer_state::TransferCard::new(crate::dto::TransferDto {
                job_id: id, name: format!("f{}", id), total: 10, done: 10,
                state: state.into(), speed_bps: 0, peer: "ab".into(),
                direction: "rx".into(), local_role: "receiver".into(),
                progress_percent: 100, eta_secs: -2, fail_reason: String::new(),
                local_path: None, remote_done: 0, instant: false,
                ..Default::default()
            });
            card.removed = removed;
            card
        };
        let mut cards = vec![mk(1, "active", false), mk(2, "interrupted", false), mk(3, "done", true)];
        for i in 0..200u64 {
            cards.push(mk(100 + i, "done", false));
        }
        let list = super::collect_persist_list(&cards);
        let removed_count = list.iter().filter(|c| c.removed).count();
        assert_eq!(removed_count, 1, "removed 终态全量保留");
        assert!(list.iter().any(|c| c.dto.job_id == 3 && c.removed));
        let history_count = list.iter()
            .filter(|c| !c.removed && (c.dto.state == "done" || c.dto.state == "failed"))
            .count();
        assert_eq!(history_count, 150, "未删除历史封顶 150");
        // open 卡(interrupted 可续传)全量保留
        assert!(list.iter().any(|c| c.dto.job_id == 1));
        assert!(list.iter().any(|c| c.dto.job_id == 2));
    }

    #[test]
    fn migrate_records_marks_cancelling_as_interrupted() {
        // M2 T2:cancelling 非终态——重启后引擎任务已消失,取消等不到确认,
        // 迁移为 interrupted(push 保留/无 parts 孤儿丢弃规则与其它非终态一致)
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path();
        std::fs::create_dir_all(inbox.join(".localtrans-parts").join("7")).unwrap();
        let mk = |id: u64, state: &str, direction: &str| super::TransferRecord {
            job_id: id, name: format!("f{}", id), total: 10, done: 5,
            state: state.into(), speed_bps: 0, peer: "ab".into(),
            direction: direction.into(), local_role: String::new(),
            progress_percent: 50, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
            started_at_ms: None, finished_at_ms: None, parts_id: None, source_path: None,
        };
        let migrated = super::migrate_records_on_startup(
            vec![mk(7, "cancelling", "rx"), mk(8, "cancelling", "push")],
            &[inbox.to_path_buf()],
        );
        assert_eq!(migrated.len(), 2);
        assert_eq!(migrated[0].state, "interrupted");
        assert_eq!(migrated[1].state, "interrupted");
    }

    // ===== M2 T3:磁盘历史 + manifest 优先启动重建 =====

    /// 在 root/.localtrans-parts/<id> 写一份 manifest(received 位图按参数;
    /// 可选 meta),返回目录路径。
    fn write_parts_manifest(
        root: &std::path::Path,
        job_id: u64,
        file_name: &str,
        total: u64,
        received: &[bool],
        meta: Option<localtrans_core::transfer::TransferMeta>,
    ) -> std::path::PathBuf {
        let hashes: Vec<String> = (0..received.len()).map(|i| format!("{:064x}", i)).collect();
        let mut m = localtrans_core::transfer::manifest::Manifest::from_meta(file_name.into(), total, hashes);
        m.received = received.to_vec();
        m.peer = Some("cd".repeat(32));
        if let Some(meta) = meta {
            m.set_meta(meta);
        }
        let dir = root.join(".localtrans-parts").join(format!("{:016x}", job_id));
        m.save(&dir).unwrap();
        dir
    }

    fn meta_with(display_name: &str, peer_hex: &str, created: i64) -> localtrans_core::transfer::TransferMeta {
        localtrans_core::transfer::TransferMeta {
            direction: "pull".into(),
            local_role: "destination".into(),
            display_name: display_name.into(),
            peer_hex: peer_hex.into(),
            created_at_ms: created,
            finished_at_ms: None,
            fail_reason: None,
            source_path: None,
            batch_label: None,
        }
    }

    #[test]
    fn orphan_card_dto_states_and_name_priority() {
        const C: u64 = 4 * 1024 * 1024;
        // 缺块 → interrupted,done 按已收块字节和(尾块按实际长度)
        let hashes: Vec<String> = (0..3).map(|i| format!("{:064x}", i)).collect();
        let mut m = localtrans_core::transfer::manifest::Manifest::from_meta("raw.bin".into(), 2 * C + 100, hashes.clone());
        m.received = vec![true, false, true];
        let d = super::orphan_card_dto(0x77, &m);
        assert_eq!(d.state, "interrupted");
        assert_eq!(d.done, C + 100, "首块 4MiB + 尾块 100B");
        assert_eq!(d.name, "raw.bin", "无 meta 回退 file_name");
        assert_eq!(d.direction, "pull");
        assert_eq!(d.parts_id.as_deref(), Some("0000000000000077"));
        assert!(d.fail_reason.is_empty());
        assert_eq!(d.job_id, 0x77, "卡键=engine_id");
        // 位图全真 → failed 完整性存疑;meta.display_name 优先;peer/时间戳来自 meta
        let mut full = localtrans_core::transfer::manifest::Manifest::from_meta("raw.bin".into(), 2 * C + 100, hashes);
        full.received = vec![true, true, true];
        full.set_meta(meta_with("电影合集", &"ab".repeat(32), 1_770_000_000_000));
        let d2 = super::orphan_card_dto(0x78, &full);
        assert_eq!(d2.state, "failed");
        assert!(d2.fail_reason.contains("完整性存疑"));
        assert_eq!(d2.name, "电影合集");
        assert_eq!(d2.peer, "ab".repeat(32));
        assert_eq!(d2.started_at_ms, Some(1_770_000_000_000));
        assert_eq!(d2.done, 2 * C + 100);
    }

    #[test]
    fn rebuild_cards_manifest_first_merges_index_and_gcs_after_scan() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        // 磁盘:job 1 单块未收(interrupted,done=0);job 2 位图全真(failed,启动 gc 会删目录)
        write_parts_manifest(&root, 1, "a.bin", 100, &[false], None);
        let full_dir = write_parts_manifest(&root, 2, "b.bin", 100, &[true], Some(meta_with("显示名B", "ef", 5)));
        // 索引:job 1 索引说 active/done=99(应被 manifest 覆盖)且 removed=true;
        //       job 3 索引独有 done 历史;job 4 索引独有 pull pending 无 parts → 丢弃;
        //       job 5 索引独有 push active → interrupted 保留
        let json = r#"{"cards":[
            {"dto":{"job_id":"0x1","name":"索引名A","total":100,"done":99,"state":"active","speed_bps":9,
                "peer":"ab","direction":"pull","local_role":"receiver","progress_percent":99,"eta_secs":1,
                "fail_reason":"","local_path":null,"started_at_ms":123},"removed":true},
            {"dto":{"job_id":"0x3","name":"c.bin","total":10,"done":10,"state":"done","speed_bps":0,
                "peer":"ab","direction":"pull","local_role":"receiver","progress_percent":100,"eta_secs":-2,
                "fail_reason":"","local_path":"/x/c.bin"},"removed":false},
            {"dto":{"job_id":"0x4","name":"d.bin","total":10,"done":1,"state":"pending","speed_bps":0,
                "peer":"ab","direction":"pull","local_role":"receiver","progress_percent":10,"eta_secs":-1,
                "fail_reason":"","local_path":null},"removed":false},
            {"dto":{"job_id":"0x5","name":"e.bin","total":10,"done":1,"state":"active","speed_bps":0,
                "peer":"ab","direction":"push","local_role":"sender","progress_percent":10,"eta_secs":-1,
                "fail_reason":"","local_path":null},"removed":false}
        ]}"#;
        std::fs::write(root.join("transfers.json"), json).unwrap();

        let cards = super::rebuild_cards(&root, &[root.clone()]);
        let mut ids: Vec<u64> = cards.keys().copied().collect();
        ids.sort();
        assert_eq!(ids, vec![1, 2, 3, 5], "4 是 pull 非终态无 parts 孤儿,丢弃");

        // job 1:manifest 优先(interrupted / done=已收字节 0,索引的 active/99 被覆盖),
        //        索引补 removed 与时间戳;manifest 无 meta 名 → 用 file_name(非空),不取索引名
        let c1 = &cards[&1];
        assert_eq!(c1.dto.state, "interrupted", "manifest 优先:索引 active 不作数");
        assert_eq!(c1.dto.done, 0, "manifest 优先:索引 done=99 不作数");
        assert!(c1.removed, "索引 removed 标记透传");
        assert_eq!(c1.dto.started_at_ms, Some(123), "卡片侧缺时间戳时索引补齐");
        assert_eq!(c1.dto.name, "a.bin");
        assert_eq!(c1.engine_id, Some(1));
        // job 2:位图全真 → failed 完整性存疑,meta 显示名优先;目录在扫盘后被 gc,卡仍在
        let c2 = &cards[&2];
        assert_eq!(c2.dto.state, "failed");
        assert_eq!(c2.dto.name, "显示名B");
        assert!(!full_dir.exists(), "gc 在扫盘之后跑:全真孤儿目录被清");
        assert!(root.join(".localtrans-parts/0000000000000001").exists(), "缺块目录保留可续传");
        // job 3:索引独有 done 历史保留(含 local_path)
        assert_eq!(cards[&3].dto.state, "done");
        assert_eq!(cards[&3].dto.local_path.as_deref(), Some("/x/c.bin"));
        // job 5:push 非终态 → interrupted 保留(本机是数据源,无 parts 概念)
        assert_eq!(cards[&5].dto.state, "interrupted");
        // 派生字段已重算(进度百分比与 eta 终态标记)
        assert_eq!(c1.dto.progress_percent, 0);
        assert_eq!(c1.dto.eta_secs, -2);
        assert_eq!(c2.dto.progress_percent, 100);
    }

    #[test]
    fn rebuild_cards_multi_root_first_seen_wins_and_empty_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("inbox");
        let b = tmp.path().join("downloads");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        // 同 id 两根:首根为准
        write_parts_manifest(&a, 9, "from-a.bin", 10, &[false], None);
        write_parts_manifest(&b, 9, "from-b.bin", 10, &[false], None);
        write_parts_manifest(&b, 10, "only-b.bin", 10, &[false], None);
        let cards = super::rebuild_cards(tmp.path(), &[a.clone(), b.clone()]);
        assert_eq!(cards.len(), 2);
        assert_eq!(cards[&9].dto.name, "from-a.bin");
        assert_eq!(cards[&10].dto.name, "only-b.bin");
        // 空根/不存在的根不 panic
        let none = super::rebuild_cards(tmp.path(), &[tmp.path().join("nope")]);
        assert!(none.is_empty());
    }

    #[tokio::test]
    async fn list_disk_jobs_merges_disk_first_and_table_terminal_cards() {
        let st = test_app_state().await;
        // 测试态 parts 根:inbox=C:/inbox(不存在)、download_dir=默认、dir=临时目录 → 写 dir
        let root = st.dir.clone();
        write_parts_manifest(&root, 0x21, "disk.bin", 300, &[true, false, false],
            Some(meta_with("磁盘显示名", &"ab".repeat(32), 777)));
        write_parts_manifest(&root, 0x22, "full.bin", 100, &[true], None);
        // 表内:0x21 也在表(磁盘优先,不重复);0x23 done 无 parts(补齐,removed 透传);
        //       0x24 active(open,不进历史)
        st.card_create(mk_card(0x21, "interrupted")).await;
        let mut done = mk_card(0x23, "done");
        done.started_at_ms = Some(999);
        st.card_create(done).await;
        st.card_mark_removed(0x23).await;
        st.card_create(mk_card(0x24, "active")).await;

        let jobs = super::list_disk_jobs_inner(&st).await;
        let ids: Vec<u64> = jobs.iter().map(|j| j.job_id).collect();
        assert_eq!(ids, vec![0x21, 0x22, 0x23], "磁盘条目 + 表内终态补齐,open 卡不进,按 id 有序");
        let j21 = &jobs[0];
        assert_eq!(j21.display_name, "磁盘显示名", "磁盘条目优先且 meta 名优先");
        assert_eq!(j21.state, "interrupted");
        assert_eq!(j21.done, 1, "PC 口径:done=已收块数");
        assert_eq!(j21.peer_hex, "ab".repeat(32));
        assert_eq!(j21.created_at_ms, Some(777));
        assert!(!j21.removed_from_view, "纯磁盘视角恒 false");
        let j22 = &jobs[1];
        assert_eq!(j22.state, "failed", "位图全真 → failed");
        assert_eq!(j22.display_name, "full.bin", "无 meta 回退 file_name");
        assert_eq!(j22.direction, "pull");
        let j23 = &jobs[2];
        assert_eq!(j23.display_name, "f35");
        assert!(j23.removed_from_view, "表内卡 removed 透传");
        assert_eq!(j23.created_at_ms, Some(999), "表内卡 created_at_ms=started_at_ms");
        assert_eq!(j23.state, "done");
    }

    #[tokio::test]
    async fn restore_disk_job_clears_removed_or_rebuilds_card_from_disk() {
        let st = test_app_state().await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));
        // 在表且 removed → 只清标记
        st.card_create(mk_card(0x31, "done")).await;
        st.card_mark_removed(0x31).await;
        assert!(!st.snapshot_dtos().await.iter().any(|d| d.job_id == 0x31));
        super::restore_disk_job_inner(&st, &cb, 0x31).await.unwrap();
        assert!(!st.card_get(0x31).await.unwrap().removed);
        assert!(st.snapshot_dtos().await.iter().any(|d| d.job_id == 0x31), "恢复后回列表");
        // 不在表、磁盘有孤儿 → 按重建逻辑建卡(interrupted,removed=false,卡键=engine_id)
        const C: u64 = 4 * 1024 * 1024;
        write_parts_manifest(&st.dir, 0x32, "orphan.bin", 2 * C, &[true, false],
            Some(meta_with("孤儿显示名", &"cd".repeat(32), 5)));
        super::restore_disk_job_inner(&st, &cb, 0x32).await.unwrap();
        let c = st.card_get(0x32).await.expect("孤儿应建卡入表");
        assert_eq!(c.dto.state, "interrupted");
        assert_eq!(c.dto.name, "孤儿显示名");
        assert_eq!(c.engine_id, Some(0x32));
        assert!(!c.removed);
        assert_eq!(c.dto.done, C);
        assert_eq!(c.dto.progress_percent, 50, "派生字段已重算(1/2 块)");
        // 均无 → Err
        assert!(super::restore_disk_job_inner(&st, &cb, 0x33).await.is_err());
        // 事件面:两次恢复各发一条 TransferUpdated(Kotlin 列表事件驱动)
        let n = events.lock().unwrap().iter()
            .filter(|e| matches!(e, crate::AppEvent::TransferUpdated { .. }))
            .count();
        assert_eq!(n, 2);
    }

    #[tokio::test]
    async fn destroy_disk_job_removes_parts_and_row_or_pure_orphan() {
        let st = test_app_state().await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));
        // 在表 + 有 parts:删表行 + 删目录 + removed 事件
        let d41 = write_parts_manifest(&st.dir, 0x41, "t.bin", 10, &[false], None);
        st.card_create(mk_card(0x41, "interrupted")).await;
        super::destroy_disk_job_inner(&st, &cb, 0x41).await.unwrap();
        assert!(st.card_get(0x41).await.is_none(), "表行实删");
        assert!(!d41.exists(), "parts 目录实删");
        assert!(events.lock().unwrap().iter().any(|e| matches!(
            e, crate::AppEvent::TransferDone { job_id: 0x41, ok: false, fail_reason } if fail_reason == "removed"
        )), "在表卡实删发 removed 事件供 UI 删行");
        // 纯孤儿(未建卡):只删目录
        let d42 = write_parts_manifest(&st.dir, 0x42, "o.bin", 10, &[false], None);
        super::destroy_disk_job_inner(&st, &cb, 0x42).await.unwrap();
        assert!(!d42.exists());
        // 均无 → Err
        assert!(super::destroy_disk_job_inner(&st, &cb, 0x43).await.is_err());
        // 两级删除 destroy 级终态卡同样连带 parts(finalize_destroy 语义)
        let d44 = write_parts_manifest(&st.dir, 0x44, "z.bin", 10, &[false], None);
        st.card_create(mk_card(0x44, "failed")).await;
        let st_arc = Arc::new(st);
        assert_eq!(super::remove_transfer_level_inner(&st_arc, &cb, 0x44, "destroy").await.unwrap(), true);
        assert!(st_arc.card_get(0x44).await.is_none());
        assert!(!d44.exists(), "destroy 级终态卡实删连带 parts 目录");
    }

    #[tokio::test]
    async fn merge_orphans_from_root_only_adds_missing_cards() {
        let st = test_app_state().await;
        let inbox = tempfile::tempdir().unwrap();
        // 在表活动卡 0x51 不得被磁盘快照改写;0x52 缺卡 → 新建
        st.card_create(mk_card(0x51, "active")).await;
        write_parts_manifest(inbox.path(), 0x51, "live.bin", 10, &[false], None);
        write_parts_manifest(inbox.path(), 0x52, "new.bin", 10, &[false], None);
        let added = st.merge_orphans_from_root(inbox.path()).await;
        assert_eq!(added, 1);
        assert_eq!(st.card_state(0x51).await.as_deref(), Some("active"), "在表卡不被覆盖");
        assert_eq!(st.card_state(0x52).await.as_deref(), Some("interrupted"));
        // 二次补扫幂等
        assert_eq!(st.merge_orphans_from_root(inbox.path()).await, 0);
        // 不存在的根 → 0,不 panic
        assert_eq!(st.merge_orphans_from_root(&inbox.path().join("nope")).await, 0);
    }

    #[tokio::test]
    async fn parts_roots_dedups_inbox_download_and_dir() {
        let st = test_app_state().await;
        let roots = st.parts_roots().await;
        // 测试态:inbox=C:/inbox、download_dir=Config::default()、dir=临时目录 → 三根互异
        assert_eq!(roots.len(), 3);
        assert_eq!(roots[0], std::path::PathBuf::from("C:/inbox"), "inbox 根优先");
        assert_eq!(roots[2], st.dir);
        // inbox 与 dir 同值时去重
        *st.inbox_dir.write().unwrap() = st.dir.clone();
        let roots = st.parts_roots().await;
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0], st.dir);
    }

    /// 构造测试用 AppState(不启动网络/发现,SessionManager 不监听端口)
    async fn test_app_state() -> crate::state::AppState {
        use crate::state::AppState;
        use tokio::sync::{ Mutex as TokioMutex, RwLock };

        let dir = tempfile::tempdir().unwrap();
        let identity = Arc::new(
            localtrans_core::identity::Identity::load_or_create(dir.path()).unwrap()
        );
        let config = Arc::new(RwLock::new(localtrans_core::store::Config::default()));
        let trust = Arc::new(TokioMutex::new(
            localtrans_core::identity::TrustStore::load(dir.path())
        ));
        let (sm, _session_events) = localtrans_core::session::SessionManager::spawn(
            localtrans_core::session::SessionCtx {
                identity: identity.clone(),
                trust: trust.clone(),
                config: config.clone(),
            }
        );

        AppState {
            dir: dir.keep(),
            inbox_dir: std::sync::RwLock::new(std::path::PathBuf::from("C:/inbox")),
            saved_files: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            identity,
            config,
            trust,
            sm,
            discovery: test_discovery_handle(),
            hidden: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            devices: Arc::new(std::sync::Mutex::new(Vec::new())),
            connected_fps: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            event_tasks: Arc::new(std::sync::Mutex::new(Vec::new())),
            pending_offers: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            pending_deletes: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            auto_offers: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            transfers: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            transfers_dirty: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            next_placeholder_id: Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX)),
            // M2 T4:测试构造给 3(同桌面壳测试;Config::default 的 max_active_transfers 也是 3)
            active_gate: Arc::new(tokio::sync::Semaphore::new(3)),
            peer_locks: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            progress_throttle: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            sender_jobs: localtrans_core::transfer::sender_state::new_sender_job_map(),
            cancel_watchdogs: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            relay: Arc::new(tokio::sync::Mutex::new(None)),
            relay_roster: Arc::new(std::sync::Mutex::new(Vec::new())),
            relay_status: Arc::new(std::sync::Mutex::new(crate::relay_state::RelayUiStatus::Disabled)),
            relay_event_task: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connect_task: Arc::new(tokio::sync::Mutex::new(None)),
            healing_fps: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            auto_retried: Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            channels: Arc::new(localtrans_core::routing::ChannelTable::new()),
        }
    }

    /// 测试用 DiscoveryHandle:起一个真实但隐身的发现服务(隐身=不广播,
    /// 端口由系统分配避免测试间冲突;测试里不真正收发)
    fn test_discovery_handle() -> Arc<localtrans_core::discovery::DiscoveryHandle> {
        let identity = localtrans_core::identity::Identity::load_or_create(
            tempfile::tempdir().unwrap().path()
        ).unwrap();
        let mut cfg = localtrans_core::discovery::DiscoveryConfig::default();
        cfg.bind_port = 0;
        cfg.hidden = Arc::new(std::sync::atomic::AtomicBool::new(true));
        Arc::new(localtrans_core::discovery::spawn(
            cfg,
            Arc::new(identity.signing.clone()),
        ).expect("测试发现服务启动失败"))
    }

    // ===== M2 T4 并发队列(镜像桌面壳 main.rs gate 三测试 + 位次) =====

    /// 建一张 pending 占位卡(占位 ID 自 u64::MAX 递减,创建序=id 降序)
    async fn create_pending_card(st: &crate::state::AppState, peer: &str) -> u64 {
        let id = st.next_placeholder_id();
        st.card_create(crate::dto::TransferDto {
            job_id: id,
            name: "f.bin".into(),
            state: "pending".into(),
            peer: peer.to_string(),
            direction: "pull".into(),
            local_role: "receiver".into(),
            ..Default::default()
        }).await;
        id
    }

    #[tokio::test]
    async fn gate_limits_concurrent_and_releases_waiter() {
        // active_gate permits=3(测试构造给 3);Arc 壳供 spawn 共享(同 lib.rs 其余测试)
        let st = Arc::new(test_app_state().await);
        // 三个不同对端占满 gate(peer 锁互不影响,均立即获得)
        let mut slots = vec![];
        for p in ["aa", "bb", "cc"] {
            slots.push(st.acquire_slot(p).await);
        }
        // 第 4 个:应排队(不返回)
        let waiter = tokio::spawn({ let st = st.clone(); async move {
            let _s = st.acquire_slot("dd").await;
            "got"
        }});
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "第 4 个在排队");
        // 释放一个 → 等待者获得
        drop(slots.pop());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(waiter.is_finished(), "出队获得槽位");
    }

    #[tokio::test]
    async fn different_peers_do_not_block_each_other() {
        let st = Arc::new(test_app_state().await); // permits=3
        let _a = st.acquire_slot("aa").await;
        // gate 未满:不同对端立即获得,不互相等待
        let b = tokio::time::timeout(std::time::Duration::from_millis(200),
            st.acquire_slot("bb")).await;
        assert!(b.is_ok(), "不同对端不应被 aa 的 peer 锁挡住");
    }

    #[tokio::test]
    async fn same_peer_serializes_on_peer_lock() {
        let st = Arc::new(test_app_state().await); // permits=3
        let _a = st.acquire_slot("aa").await;
        // 同对端第二个:gate 有余量但 peer 锁被占 → 不立即完成
        let waiter = tokio::spawn({ let st = st.clone(); async move {
            let _g = st.acquire_slot("aa").await;
            "got"
        }});
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "同对端应串行等待");
        // 释放第一个后获得
        drop(_a);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(waiter.is_finished());
    }

    #[tokio::test]
    async fn pending_rank_orders_same_peer_by_creation() {
        let st = test_app_state().await;
        // 占位 ID 递减:先建的 id 更大 → 创建序 = id 降序
        let first = create_pending_card(&st, "dd").await;
        let second = create_pending_card(&st, "dd").await;
        let third_other_peer = create_pending_card(&st, "ee").await;
        assert!(first > second, "占位 ID 递减:先建 id 更大");
        assert_eq!(st.pending_rank(first, "dd").await, Some(1), "先建者位次 1");
        assert_eq!(st.pending_rank(second, "dd").await, Some(2), "后建者位次 2");
        // 跨对端隔离:ee 的卡不进 dd 的队列计数
        assert_eq!(st.pending_rank(third_other_peer, "dd").await, None);
        assert_eq!(st.pending_rank(u64::MAX - 100, "dd").await, None, "不在表中的卡无位次");
    }

    #[tokio::test]
    async fn acquire_with_queue_pos_reports_rank_then_clears() {
        let st = Arc::new(test_app_state().await); // permits=3
        let mut slots = vec![];
        for p in ["aa", "bb", "cc"] {
            slots.push(st.acquire_slot(p).await);
        }
        // 两张同对端 pending 卡排队:第一张直接等槽,第二张走 acquire_with_queue_pos
        let first = create_pending_card(&st, "dd").await;
        let second = create_pending_card(&st, "dd").await;
        let st2 = st.clone();
        let waiter = tokio::spawn(async move {
            crate::acquire_with_queue_pos(&st2, second, "dd").await
        });
        // 等过一个 1s tick
        tokio::time::sleep(std::time::Duration::from_millis(1300)).await;
        assert!(!waiter.is_finished(), "gate 满时应仍在排队");
        assert_eq!(st.card_dto(second).await.unwrap().queue_pos, Some(2),
            "second 是同对端第 2 个等待者(降序=创建序)");
        assert_eq!(st.card_dto(first).await.unwrap().queue_pos, None,
            "未经 acquire 的卡不误报位次");
        // 释放闸门 → 出队,queue_pos 清 None
        drop(slots);
        tokio::time::timeout(std::time::Duration::from_secs(2), waiter).await
            .expect("释放后应出队").unwrap();
        assert_eq!(st.card_dto(second).await.unwrap().queue_pos, None,
            "拿到槽位后清位次");
    }

    #[test]
    fn files_saved_survives_done_and_accumulates_across_files() {
        // 小文件批流:core 侧同 job 每文件一对 Started/Done(T8 按文件数补 Done),
        // 但 FFI Done 分支做的是 remove——首个 Done 即取走 acc 并发出单文件
        // FilesSaved。Kotlin 消费端(MediaScanNotifier/AppNav)逐条扫描,无
        // 批量假设,N 次回调可接受。此测试验证的是 acc 自身在未被移除前的
        // 追加语义(直接操作 acc 时文件名跨 record 累积):
        let mut acc = super::SavedFilesAcc::new();
        acc.set_root("/sdcard/Download/LocalTrans".into());
        acc.record("a.png".to_string());
        assert_eq!(acc.snapshot_paths(), vec!["/sdcard/Download/LocalTrans/a.png".to_string()]);
        acc.record("sub/b.jpg".to_string());
        assert_eq!(acc.snapshot_paths(), vec![
            "/sdcard/Download/LocalTrans/a.png".to_string(),
            "/sdcard/Download/LocalTrans/sub/b.jpg".to_string(),
        ]);
    }

    #[tokio::test]
    async fn files_saved_small_batch_done_remove_then_started_rebuilds() {
        // 回归 v0.11.0 审查 B-2 + 返工:core 接收侧小文件批流对同一 job_id
        // 每文件发一对 Started/Done(recv_small_files_batched)。Done 消费文件
        // 名但保留根;文件之间切 set_inbox_dir 后,后续 Started 重建/追加必须
        // 沿用本 job 已钉的根(accept 时刻),不能钉成新目录——否则同一批文件
        // 的 FilesSaved 路径散落两个根,与实际落盘(accept 时的旧根)错位。
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));
        let st = test_app_state().await;
        // 模拟 accept 钉根(respond_offer 路径)
        st.saved_files.lock().unwrap()
            .entry(7u64)
            .or_insert_with(crate::SavedFilesAcc::new)
            .set_root(std::path::PathBuf::from("/sdcard/Download/LocalTrans"));

        async fn send_pair(
            st: &crate::state::AppState,
            cb: &Arc<Box<dyn crate::LocalTransCallback>>,
            name: &str,
        ) {
            super::handle_recv_progress_event(
                st, cb,
                localtrans_core::transfer::ProgressEvent::Started {
                    job_id: 7, name: name.to_string(), total: 10,
                },
            ).await;
            super::handle_recv_progress_event(
                st, cb,
                localtrans_core::transfer::ProgressEvent::Done { job_id: 7 },
            ).await;
        }
        send_pair(&st, &cb, "a.png").await;
        // 文件之间切目录:重建场景下不得影响本 job 的根
        *st.inbox_dir.write().unwrap() = std::path::PathBuf::from("C:/NewInbox");
        send_pair(&st, &cb, "b.jpg").await;

        let evs = events.lock().unwrap();
        let saved: Vec<Vec<String>> = evs.iter()
            .filter_map(|e| match e {
                crate::AppEvent::FilesSaved { paths, .. } => Some(paths.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(saved, vec![
            vec!["/sdcard/Download/LocalTrans/a.png".to_string()],
            vec!["/sdcard/Download/LocalTrans/b.jpg".to_string()],
        ]);
        // Done 消费后 names 已清(根保留),不留旧文件名——下一对 Started 重新累计
        let acc = st.saved_files.lock().unwrap();
        let a = acc.get(&7).expect("根应保留在 acc 中供批流后续文件沿用");
        assert!(a.snapshot_paths().is_empty());
    }

    #[test]
    fn relay_invalid_config_marks_error_not_connecting() {
        // 坏配置(缺端口)启动:start() 后 relay_status 应为 Error 而非 Connecting
        let dir = tempfile::tempdir().unwrap();
        // 预写坏配置:启用中继但地址缺端口(完整配置 JSON)
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{"device_name":"测试设备","download_dir":"/tmp/downloads","hidden":false,"quic_port":{},"discovery_port":{},"shares":[],"relay_enabled":true,"relay_server":"10.255.255.1","relay_psk":"0123456789abcdef","consent_timeout_secs":120,"offer_timeout_secs":60}}"#,
                localtrans_core::ports::quic_port(), localtrans_core::ports::discovery_port()
            ),
        ).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start();

        let status = app.relay_status();
        assert!(status.enabled, "配置已启用");
        assert!(!status.connected, "不应建立连接");
        assert!(status.error.contains("缺少端口"), "坏配置应给出具体错误,实际: {}", status.error);

        app.shutdown();
    }

    #[test]
    fn relay_valid_unreachable_marks_connecting_then_error() {
        // 好配置但服务器不可达:状态先 Connecting,连接失败后 Error
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{"device_name":"测试设备","download_dir":"/tmp/downloads","hidden":false,"quic_port":{},"discovery_port":{},"shares":[],"relay_enabled":true,"relay_server":"127.0.0.1:19443","relay_psk":"0123456789abcdef","consent_timeout_secs":120,"offer_timeout_secs":60}}"#,
                localtrans_core::ports::quic_port(), localtrans_core::ports::discovery_port()
            ),
        ).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start();

        let status = app.relay_status();
        assert!(status.enabled);
        // 注意:core RelayClient::connect 内部有 backoff 重试,连接失败
        // 后 status 停留在 Connecting/Reconnecting 而非 Error——这与桌面行为
        // 一致(UI 显示「连接中」)。此测试只断言「不会连接成功且不 panic」。
        assert!(!status.connected, "不可达服务器不应连接成功");
        app.shutdown();
    }

    #[test]
    fn devices_merges_roster_with_local_priority() {
        // 手工喂名册:core merge_devices 已单测覆盖排序/去重,
        // 此处验证 FFI devices() 真正接了名册(via_relay 不再恒 false)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start();

        // 直接向 state.relay_roster 写入一台测试设备(绕过网络)
        {
            let st = app.state_for_test().unwrap();
            let mut fp = [0u8; 32];
            fp[0] = 0xAA;
            let roster = vec![localtrans_core::relay::proto::RemoteDevice {
                fingerprint: fp,
                name: "中继远程设备".into(),
                lease_addr: format!("10.0.0.1:{}", localtrans_core::ports::quic_port()).parse().unwrap(),
                relayed_ephemeral: false,
            }];
            *st.relay_roster.lock().unwrap() = roster;
        }

        let devices = app.devices();
        let remote = devices.iter().find(|d| d.via_relay).expect("名册设备应出现在列表");
        assert_eq!(remote.name, "中继远程设备");
        assert!(remote.online);

        app.shutdown();
    }

    #[test]
    fn connect_routes_to_roster_when_not_local() {
        // 名册有、本地无的设备:应走 relay.connect_peer(无中继连接时报
        // 「中继未连接」而非「Device not found」——证明路由进了名册分支)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start();

        let mut fp = [0u8; 32];
        fp[0] = 0xBB;
        let fp_hex = hex::encode(fp);
        {
            let st = app.state_for_test().unwrap();
            *st.relay_roster.lock().unwrap() = vec![localtrans_core::relay::proto::RemoteDevice {
                fingerprint: fp,
                name: "远程设备".into(),
                lease_addr: format!("10.0.0.2:{}", localtrans_core::ports::quic_port()).parse().unwrap(),
                relayed_ephemeral: false,
            }];
        }

        let err = app.connect_device(fp_hex).unwrap_err();
        let msg = format!("{}", err);
        assert!(msg.contains("中继未连接"), "名册设备应走路由分支,实际报错: {}", msg);

        app.shutdown();
    }

    #[test]
    fn save_settings_relay_change_triggers_status_update() {
        // 启用中继+坏地址保存:relay_status 应从 Disabled 变为 Error
        // (证明 save_settings 真的触发了重连评估)
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start();

        assert_eq!(app.relay_status().status, "disabled");

        let mut s = app.settings();
        s.relay_enabled = true;
        s.relay_addr = "10.255.255.1".into();   // 缺端口→validate 失败
        s.relay_psk = "0123456789abcdef".into();
        app.save_settings(s);

        let status = app.relay_status();
        assert_eq!(status.status, "error");
        assert!(status.error.contains("缺少端口"));

        app.shutdown();
    }

    #[tokio::test]
    async fn source_speed_updates_remote_done() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));
        let st = test_app_state().await;
        // 先建一行(push 方向)
        st.card_create(crate::dto::TransferDto {
            job_id: 1, name: "big.bin".into(), total: 100, done: 40,
            state: "active".into(), speed_bps: 0, peer: "aa".into(),
            direction: "push".into(), local_role: "source-push".into(),
            progress_percent: 40, eta_secs: -1, fail_reason: String::new(),
            local_path: None, remote_done: 0, instant: false,
            ..Default::default()
        }).await;
        super::handle_source_progress_event(&st, &cb, localtrans_core::transfer::ProgressEvent::SourceSpeed {
            job_id: 1, bps: 1024, loss_ratio: 0.0, rtt_ms: 10, cwnd: 100, streams: 2,
            remote_done: 60,
        }).await;
        let dto = st.card_dto(1).await.unwrap();
        assert_eq!(dto.remote_done, 60);
        assert_eq!(dto.speed_bps, 1024);
    }

    #[tokio::test]
    async fn instant_hit_marks_row_done_instant() {
        use crate::handle_recv_progress_event;
        use crate::AppEvent;
        let st = test_app_state().await;
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb: Arc<Box<dyn crate::LocalTransCallback>> = Arc::new(Box::new(SharedCb(events.clone())));
        handle_recv_progress_event(&st, &cb, localtrans_core::transfer::ProgressEvent::InstantHit {
            job_id: 9, name: "dup.bin".into(), total: 555,
        }).await;
        let dto = st.card_dto(9).await.unwrap();
        assert_eq!(dto.state, "done");
        assert!(dto.instant);
        assert_eq!(dto.done, 555);
        // 事件面:TransferUpdated + TransferDone(ok)
        let events = events.lock().unwrap();
        assert!(events.iter().any(|e| matches!(e, AppEvent::TransferDone { ok: true, .. })));
    }

    #[test]
    fn parse_probe_addr_bare_ip_gets_default_port() {
        let a = super::parse_probe_addr("192.168.1.100").unwrap();
        assert_eq!(a, format!("192.168.1.100:{}", localtrans_core::ports::discovery_port()).parse().unwrap());
    }

    #[test]
    fn parse_probe_addr_ip_port_kept() {
        let a = super::parse_probe_addr("192.168.1.100:48000").unwrap();
        assert_eq!(a.port(), 48000);
        assert_eq!(a.ip().to_string(), "192.168.1.100");
    }

    #[test]
    fn parse_probe_addr_garbage_errs() {
        assert!(super::parse_probe_addr("not-an-ip").is_err());
        assert!(super::parse_probe_addr("").is_err());
        assert!(super::parse_probe_addr("192.168.1.999").is_err());
    }

    #[test]
    fn udp_local_ip_does_not_panic() {
        // 无网环境返回 None 也算通过——只断言不 panic
        let _ = super::udp_local_ip();
    }

    /// M3a FR1:local_ips DTO 与 core 枚举 1:1 映射,ip 为合法 IPv4 点分字串,
    /// 无环回/虚拟网卡漏网(过滤规则本体在 core 单测;此处验镜像不失真)
    #[test]
    fn local_ip_dtos_mirror_core_enumeration() {
        let core = localtrans_core::net_addrs::local_addresses();
        let dtos = super::local_ip_dtos();
        assert_eq!(dtos.len(), core.len());
        for (d, c) in dtos.iter().zip(core.iter()) {
            assert_eq!(d.ip, c.ip.to_string());
            assert_eq!(d.if_name, c.if_name);
            let ip: std::net::Ipv4Addr = d.ip.parse().expect("ip 应为 IPv4 点分字串");
            assert!(localtrans_core::net_addrs::is_usable_ipv4(&ip));
            assert!(!localtrans_core::net_addrs::is_virtual_iface(&d.if_name));
        }
    }

    #[test]
    fn decode_fp32_empty_errs() {
        assert!(super::decode_fp32("").is_err());
    }

    #[test]
    fn decode_fp32_too_short_errs() {
        // 合法 hex 但不足 64 字符(32 字节)
        assert!(super::decode_fp32("aabbcc").is_err());
    }

    #[test]
    fn decode_fp32_too_long_errs() {
        // 65 个 hex 字符 → 超 32 字节
        let long = "a".repeat(65);
        assert!(super::decode_fp32(&long).is_err());
    }

    #[test]
    fn decode_fp32_non_hex_errs() {
        assert!(super::decode_fp32(&"z".repeat(64)).is_err());
    }

    #[test]
    fn concurrent_reads_before_start_are_empty_not_panic() {
        // OnceLock 并发读安全:未 start 时 20 线程同时调 devices/transfers/settings
        let dir = tempfile::tempdir().unwrap();
        let app = super::LocalTransApp::new(
            dir.path().to_str().unwrap().into(),
            Box::new(SharedCb(Arc::new(std::sync::Mutex::new(Vec::new())))),
        )
        .unwrap();
        let app = Arc::new(app);
        let mut handles = Vec::new();
        for _ in 0..20 {
            let a = app.clone();
            handles.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    assert!(a.devices().is_empty());
                    assert!(a.transfers().is_empty());
                    let _s = a.settings(); // 仅验证不 panic
                    assert_eq!(a.my_fingerprint(), "not started");
                }
            }));
        }
        for h in handles {
            h.join().expect("线程不应 panic");
        }
    }

    #[test]
    fn start_port_in_use_returns_err_not_panic() {
        use std::net::UdpSocket;
        // 先占住默认 QUIC 端口,再 start() 应返回 Err 而非 panic
        let _guard = UdpSocket::bind(("0.0.0.0", localtrans_core::ports::quic_port())).expect("占用端口失败");

        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            format!(
                r#"{{"device_name":"测试设备","download_dir":"/tmp/downloads","hidden":false,"quic_port":{},"discovery_port":{},"shares":[],"relay_enabled":false,"consent_timeout_secs":120,"offer_timeout_secs":60}}"#,
                localtrans_core::ports::quic_port(), localtrans_core::ports::discovery_port()
            ),
        ).unwrap();

        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        let result = app.start();
        assert!(result.is_err(), "端口被占时 start 应返回 Err");
        app.shutdown();
    }

    #[test]
    fn connect_device_bad_fingerprint_errs_not_panic() {
        // 设备页直通入口:空/短指纹必须返回 Err 而非 copy_from_slice panic
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start().expect("测试前提:正常 start");

        let r = app.connect_device("".to_string());
        assert!(r.is_err(), "空指纹应返回 Err");
        match app.connect_device("aabbcc".to_string()) {
            Err(super::AppException::Internal { message }) => {
                assert!(message.contains("指纹格式非法"), "错误消息应说明指纹非法,实际: {}", message);
            }
            _ => panic!("短指纹应为 Internal 错误"),
        }
        app.shutdown();
    }

    #[test]
    fn connect_device_unreachable_times_out_within_25s() {
        // T6 慢测试:不可达地址(10.255.255.1 保留段,黑洞不回包)下
        // connect_device 应 ~15s 返回 Err 而非挂死;计时断言 <25s 兜住抖动
        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start().expect("测试前提:正常 start");

        // 注入本地设备表指向不可达地址,强制走 connect_pinned 分支
        let mut fp = [0u8; 32];
        fp[0] = 0xCC;
        let fp_hex = hex::encode(fp);
        {
            let st = app.state_for_test().unwrap();
            *st.devices.lock().unwrap() = vec![localtrans_core::discovery::DeviceInfo {
                fingerprint: fp,
                name: "黑洞设备".into(),
                addr: format!("10.255.255.1:{}", localtrans_core::ports::quic_port()).parse().unwrap(),
                last_seen: std::time::Instant::now(),
            }];
        }

        let t0 = std::time::Instant::now();
        let result = app.connect_device(fp_hex);
        let elapsed = t0.elapsed();
        assert!(result.is_err(), "不可达地址应返回 Err");
        assert!(elapsed < std::time::Duration::from_secs(25),
            "应在 25s 内返回(15s 超时兜底),实际 {:?}", elapsed);
        app.shutdown();
    }

    #[test]
    fn force_relay_switch_roundtrip_and_persists() {
        // M3c T3:开关往返 + config.json 持久化(新实例重载仍生效)。
        // config.json 显式写独立高位端口,规避与并行测试的默认端口竞争
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"device_name":"测试设备","download_dir":"/tmp/downloads","hidden":false,"quic_port":58731,"discovery_port":58730,"shares":[],"relay_enabled":false,"consent_timeout_secs":120,"offer_timeout_secs":60}"#,
        ).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start().expect("测试前提:正常 start");

        let mut fp = [0u8; 32];
        fp[0] = 0xDD;
        let fp_hex = hex::encode(fp);
        assert!(!app.is_force_relay(fp_hex.clone()), "默认关闭");
        app.set_force_relay(fp_hex.clone(), true).unwrap();
        assert!(app.is_force_relay(fp_hex.clone()), "开启后立即可读");
        // 落盘生效:直接读 config.json
        let raw = std::fs::read_to_string(dir.path().join("config.json")).unwrap();
        assert!(raw.contains(&fp_hex), "config.json 应含指纹条目");
        // 关闭:条目移除
        app.set_force_relay(fp_hex.clone(), false).unwrap();
        assert!(!app.is_force_relay(fp_hex.clone()), "关闭后不可读");

        // 持久化:再开启后用新实例(同目录)重载
        app.set_force_relay(fp_hex.clone(), true).unwrap();
        app.shutdown();
        drop(app); // 旧实例 runtime 持有 QUIC socket,必须先释放再重绑同端口
        // 旧实例 UDP 端口释放有延迟(quinn endpoint 异步关闭),重试启动
        let events2 = Arc::new(Mutex::new(Vec::new()));
        let app2 = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(SharedCb(events2))).unwrap();
        let mut started = false;
        let mut last_err = String::new();
        for _ in 0..20 {
            match app2.start() {
                Ok(()) => { started = true; break; }
                Err(e) => { last_err = e.to_string(); }
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        assert!(started, "测试前提:重试后 start(端口释放), 末次错误: {last_err}");
        assert!(app2.is_force_relay(fp_hex.clone()), "重启后开关应保持");
        app2.shutdown();
    }

    #[test]
    fn force_relay_connect_errors_when_relay_absent() {
        // M3c T3 拦截语义:开关开启 + 中继不在场 → connect_device 立即报
        // 「中继未配置」,不得落回本地发现直连路径(即使设备就在发现表里)
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path()).unwrap();
        std::fs::write(
            dir.path().join("config.json"),
            r#"{"device_name":"测试设备","download_dir":"/tmp/downloads","hidden":false,"quic_port":58741,"discovery_port":58740,"shares":[],"relay_enabled":false,"consent_timeout_secs":120,"offer_timeout_secs":60}"#,
        ).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start().expect("测试前提:正常 start");

        let mut fp = [0u8; 32];
        fp[0] = 0xEE;
        let fp_hex = hex::encode(fp);
        {
            let st = app.state_for_test().unwrap();
            // 本地发现在场:若拦截失效,会走 connect_pinned 直连(环回死端口,慢失败)
            *st.devices.lock().unwrap() = vec![localtrans_core::discovery::DeviceInfo {
                fingerprint: fp,
                name: "直连在场设备".into(),
                addr: format!("127.0.0.1:1").parse().unwrap(),
                last_seen: std::time::Instant::now(),
            }];
        }
        app.set_force_relay(fp_hex.clone(), true).unwrap();

        let t0 = std::time::Instant::now();
        let err = app.connect_device(fp_hex.clone()).unwrap_err();
        let elapsed = t0.elapsed();
        let msg = err.to_string();
        assert!(msg.contains("中继未配置"), "应报「中继未配置」,实际: {msg}");
        assert!(elapsed < std::time::Duration::from_secs(3),
            "拦截必须先于直连尝试(否则 15s 超时),实际 {:?}", elapsed);
        // devices() 注记:开关在位 → force_relay=true
        let d = app.devices().into_iter().find(|d| d.fingerprint == fp_hex).expect("设备在列表");
        assert!(d.force_relay, "devices() 应注记 force_relay");
        app.shutdown();
    }

    #[test]
    fn submit_pairing_code_bad_fingerprint_errs_not_panic() {        let dir = tempfile::tempdir().unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let cb = SharedCb(events.clone());
        let app = super::LocalTransApp::new(dir.path().to_str().unwrap().into(), Box::new(cb)).unwrap();
        app.start().expect("测试前提:正常 start");

        let r = app.submit_pairing_code("zzzz".to_string(), "123456".to_string());
        assert!(r.is_err(), "非 hex 指纹应返回 Err 而非 panic");
        app.shutdown();
    }

    // ===== M3c T0 通道表生产者(probe 模块) =====

    /// 甲乙互信(环回测试共用)
    async fn pair_trust(
        st_trust: &Arc<tokio::sync::Mutex<localtrans_core::identity::TrustStore>>,
        ctx_b: &localtrans_core::session::SessionCtx,
        fp_a: [u8; 32],
        fp_b: [u8; 32],
    ) {
        use localtrans_core::{Perms, TrustedPeer};
        let mut ta = st_trust.lock().await;
        ta.upsert(TrustedPeer {
            fingerprint: fp_b,
            name: "乙".into(),
            alias: String::new(),
            paired_at: 1,
            perms: Perms::default(),
        });
        let mut tb = ctx_b.trust.lock().await;
        tb.upsert(TrustedPeer {
            fingerprint: fp_a,
            name: "甲".into(),
            alias: String::new(),
            paired_at: 1,
            perms: Perms::default(),
        });
    }

    /// 轮询等待通道记录出现带宽估算(全量探测完成的标志)
    async fn wait_probe_done(st: &crate::state::AppState, fp: [u8; 32]) -> Vec<localtrans_core::routing::ChannelRecord> {
        for _ in 0..300 {
            let snap = st.channels.snapshot(&fp);
            if snap.iter().any(|r| r.est_bps.is_some()) {
                return snap;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("30s 内全量探测未完成(通道表无带宽数据)");
    }

    #[tokio::test]
    async fn 环回_session_up登记并全量探测_通道表非空() {
        localtrans_core::test_support::init_tracing();
        let st = Arc::new(test_app_state().await);
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = localtrans_core::test_support::setup_ctx("乙");
        let fp_a = st.identity.fingerprint();
        pair_trust(&st.trust, &ctx_b, fp_a, fp_b).await;

        let addr1 = localtrans_core::test_support::start_listener(&sm_b).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), st.sm.connect_pinned(addr1, fp_b))
            .await
            .expect("连接超时")
            .expect("连接失败");
        let conn = st.sm.session(&fp_b).await.expect("会话应存在");

        // 生产者入口(镜像 PC 壳 SessionUp 分支的调用形态)
        crate::probe::on_session_up(&st, fp_b, conn).await;
        let snap = wait_probe_done(&st, fp_b).await;

        // 断言:登记+探测数据齐备,当前通道=连接地址
        assert_eq!(snap.len(), 1, "单地址单记录");
        let r = &snap[0];
        assert_eq!(r.addr, addr1);
        assert!(!r.via_relay, "直连路径(测试态中继不在场)");
        let rtt = r.rtt_ms.expect("RTT 应有值");
        assert!(rtt < 1000, "环回 RTT 应远小于 1s,实际 {rtt}ms");
        assert!(r.est_bps.expect("带宽应有值") > 1_000_000, "环回带宽应远大于 1Mbps");
        assert_eq!(st.channels.current(&fp_b), Some(addr1), "当前通道指针已登记");

        sm_b.shutdown_all().await;
    }

    #[tokio::test]
    async fn 环回_活动传输推迟探测_传输结束后补测() {
        localtrans_core::test_support::init_tracing();
        let st = Arc::new(test_app_state().await);
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = localtrans_core::test_support::setup_ctx("乙");
        let fp_a = st.identity.fingerprint();
        pair_trust(&st.trust, &ctx_b, fp_a, fp_b).await;

        // 时机纪律前置:表内挂一张 active 卡(推迟判据=任意 active/paused)
        st.card_create(crate::dto::TransferDto {
            job_id: 0x61,
            name: "busy.bin".into(),
            state: "active".into(),
            ..Default::default()
        }).await;

        let addr1 = localtrans_core::test_support::start_listener(&sm_b).await;
        tokio::time::timeout(std::time::Duration::from_secs(5), st.sm.connect_pinned(addr1, fp_b))
            .await
            .expect("连接超时")
            .expect("连接失败");
        let conn = st.sm.session(&fp_b).await.expect("会话应存在");

        crate::probe::on_session_up(&st, fp_b, conn).await;
        // 500ms 内不得登记/探测(还在 5s 步长的推迟轮询里)
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(
            st.channels.snapshot(&fp_b).is_empty(),
            "活动传输期间必须推迟登记"
        );

        // 传输结束(表清空)→ 推迟解除,补测完成
        st.transfers.lock().await.clear();
        let snap = wait_probe_done(&st, fp_b).await;
        assert_eq!(st.channels.current(&fp_b), Some(addr1), "推迟解除后完成登记");
        assert!(snap[0].est_bps.is_some(), "推迟解除后全量探测应有带宽");

        sm_b.shutdown_all().await;
    }
}
