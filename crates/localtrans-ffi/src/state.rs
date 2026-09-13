// Internal application state for FFI layer
// Trimmed version of desktop shell's AppState (main.rs)
//
// M2 T2:传输表从裸 HashMap<u64, TransferDto> 升级为 TransferCard 状态机表
// (移植桌面壳 main.rs Task 4 口径):所有状态变化经 card_apply 单写者入口,
// 非法迁移拒绝;终态吸收。ffi 无 engine_to_card 复杂映射——job_id 即卡键。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Mutex;
use std::collections::{HashSet, HashMap};
use std::time::Instant;
use tokio::sync::{RwLock, Mutex as TokioMutex, oneshot};
use localtrans_core::*;

use crate::transfer_state::{self, CardEvent, TransferCard};

/// Pending offer for response handling
pub struct PendingOffer {
    pub respond: oneshot::Sender<Option<std::path::PathBuf>>,
    pub extend: Arc<tokio::sync::Notify>,
    pub deadline: i64,
}

/// 当前 Unix 毫秒(卡片时间戳盖戳用;对齐桌面壳 now_ms)
pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 由 done/total/speed/state 重算 UI 派生字段(progress_percent / eta_secs)。
/// 对齐原 transfer_update 盖戳语义:终态 eta_secs=-2(完成时间戳指示),
/// 非终态 total>done 且 speed>0 时 eta=剩余/速度,否则 -1。
pub(crate) fn sync_derived_fields(dto: &mut crate::dto::TransferDto) {
    let terminal = transfer_state::is_terminal(&dto.state);
    dto.progress_percent = if dto.total > 0 {
        ((dto.done as f64 / dto.total as f64) * 100.0) as u8
    } else {
        0
    };
    dto.eta_secs = if terminal {
        -2
    } else if dto.total > dto.done && dto.speed_bps > 0 {
        ((dto.total - dto.done) / dto.speed_bps) as i64
    } else {
        -1
    };
}

/// Internal application state (trimmed from desktop shell main.rs)
pub struct AppState {
    /// Data directory
    pub dir: std::path::PathBuf,
    /// User-visible receive destination (inbox). Defaults to dir (private dir, same as old behavior).
    /// Android side will set_inbox_dir to Download/LocalTrans at startup.
    /// Note: Resume manifests/parts still use dir — internal state doesn't pollute user directory.
    pub inbox_dir: std::sync::RwLock<std::path::PathBuf>,
    /// 接收侧 per-job 文件名累积(Started 事件喂入,Done 时生成 FilesSaved)
    pub saved_files: std::sync::Arc<std::sync::Mutex<HashMap<u64, crate::SavedFilesAcc>>>,
    /// Device identity
    pub identity: Arc<identity::Identity>,
    /// Configuration (RwLock for runtime modification)
    pub config: Arc<RwLock<store::Config>>,
    /// Trust list
    pub trust: Arc<TokioMutex<identity::TrustStore>>,
    /// Session manager
    pub sm: Arc<session::SessionManager>,
    /// Discovery handle
    pub discovery: Arc<discovery::DiscoveryHandle>,
    /// Whether hidden (shared with DiscoveryConfig)
    pub hidden: Arc<AtomicBool>,
    /// Current device list
    pub devices: Arc<Mutex<Vec<discovery::DeviceInfo>>>,
    /// Connected fingerprints set (SessionUp/Down maintained)
    pub connected_fps: Arc<Mutex<HashSet<String>>>,
    /// Event bridge tasks join handles (for shutdown)
    pub event_tasks: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,

    // ===== Transfer management fields (from desktop shell) =====
    /// Pending offers table (job_id -> PendingOffer)
    pub pending_offers: Arc<TokioMutex<HashMap<u64, PendingOffer>>>,
    /// Pending deletes table (ask_id -> oneshot::Sender<bool>) - P0-2d
    pub pending_deletes: Arc<TokioMutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>>,
    /// Auto offers table (job_id -> (peer_hex, file_count)) - simplified for Android
    pub auto_offers: Arc<TokioMutex<HashMap<u64, (String, usize)>>>,
    /// 传输任务表(M2 T2:TransferCard 状态机表,单写者 card_apply 入口;
    /// job_id 即卡键——占位卡用 u64::MAX 递减 ID,引擎事件按真实 job_id 落卡)
    pub transfers: Arc<TokioMutex<HashMap<u64, TransferCard>>>,
    /// Transfer table dirty flag (drives persistence)
    pub transfers_dirty: Arc<AtomicBool>,
    /// Placeholder task ID allocator (decrementing from u64::MAX)
    pub next_placeholder_id: Arc<AtomicU64>,
    /// M2 T4:全局并发闸门(permits=max_active_transfers,启动按 config 构建 clamp 1-8)。
    /// 信号量容量不可变:运行中改设置不热更新,重启生效——对齐桌面壳"启动构建"语义。
    pub active_gate: Arc<tokio::sync::Semaphore>,
    /// M2 T4:对端串行锁表(peer_hex → 锁,首次使用懒创建):
    /// 同对端任务串行,跨对端互不阻塞(镜像桌面壳 peer_locks)
    pub peer_locks: Arc<TokioMutex<HashMap<String, Arc<TokioMutex<()>>>>>,
    /// Progress throttle tracking (job_id -> last_emit Instant)
    pub progress_throttle: Arc<Mutex<HashMap<u64, Instant>>>,
    /// Sender job map for push control (pause/resume/cancel)
    pub sender_jobs: localtrans_core::transfer::sender_state::SenderJobMap,
    /// 删除/取消仲裁看门狗(job_id → JoinHandle,镜像桌面壳 cancel_watchdogs)。
    /// cancelling 卡挂 5s sleep 任务;引擎终态确认先到则 abort。
    pub cancel_watchdogs: Arc<TokioMutex<HashMap<u64, tokio::task::JoinHandle<()>>>>,

    // ===== 中继(v0.9.0):镜像桌面壳 AppState 的 relay 四件套 =====
    /// 中继客户端(启用且连接成功时存在)
    pub relay: Arc<tokio::sync::Mutex<Option<Arc<localtrans_core::relay::client::RelayClient>>>>,
    /// 中结名册快照(RosterUpdated 事件写入,devices() 合并用)
    pub relay_roster: Arc<Mutex<Vec<localtrans_core::relay::proto::RemoteDevice>>>,
    /// 中继 UI 状态(拉模式,relay_status() 读)
    pub relay_status: Arc<Mutex<crate::relay_state::RelayUiStatus>>,
    /// 中继事件桥任务句柄(重连/禁用时 abort)
    pub relay_event_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// M-B3: 中继 connect 任务句柄(重连时 abort 旧连接任务,防竞态双连)
    pub relay_connect_task: Arc<tokio::sync::Mutex<Option<tokio::task::AbortHandle>>>,

    // ===== 自愈回路守卫(Task 6:镜像桌面壳) =====
    /// 回路 1 去重:正在自愈的指纹
    pub healing_fps: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 回路 2 一次性守卫:已自动续传过的 job_id
    pub auto_retried: Arc<Mutex<std::collections::HashSet<u64>>>,

    // ===== M3b T4:通道记录表(镜像桌面壳 AppState.channels;只读消费面) =====
    /// 每设备 {地址,RTT,估速,稳定性(近10次),时间戳} 内存态表。数据结构/探测
    /// 原语/评分决策全在 core::routing(与 PC 壳同源同版本)。
    /// M3c T0:生产者已接线(probe 模块——SessionUp 登记+全量探测+5min 快检,
    /// 镜像 PC 壳 probe::on_session_up/spawn_scheduler;退化切换不移植,
    /// Android 单通道场景为主,退化切换 PC 优先)。
    pub channels: Arc<localtrans_core::routing::ChannelTable>,
}

impl AppState {
    /// Create new AppState
    pub fn new(
        dir: std::path::PathBuf,
        identity: Arc<identity::Identity>,
        config: Arc<RwLock<store::Config>>,
        trust: Arc<TokioMutex<identity::TrustStore>>,
        sm: Arc<session::SessionManager>,
        discovery: Arc<discovery::DiscoveryHandle>,
        hidden: Arc<AtomicBool>,
        pending_offers: Arc<TokioMutex<std::collections::HashMap<u64, PendingOffer>>>,
        pending_deletes: Arc<TokioMutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<bool>>>>,
        auto_offers: Arc<TokioMutex<std::collections::HashMap<u64, (String, usize)>>>,
        transfers: Arc<TokioMutex<std::collections::HashMap<u64, TransferCard>>>,
        transfers_dirty: Arc<AtomicBool>,
        next_placeholder_id: Arc<AtomicU64>,
        active_gate: Arc<tokio::sync::Semaphore>,
        peer_locks: Arc<TokioMutex<std::collections::HashMap<String, Arc<TokioMutex<()>>>>>,
        progress_throttle: Arc<Mutex<std::collections::HashMap<u64, Instant>>>,
        sender_jobs: localtrans_core::transfer::sender_state::SenderJobMap,
    ) -> Self {
        Self {
            inbox_dir: std::sync::RwLock::new(dir.clone()),
            dir,
            identity,
            config,
            trust,
            sm,
            discovery,
            hidden,
            devices: Arc::new(Mutex::new(Vec::new())),
            connected_fps: Arc::new(Mutex::new(HashSet::new())),
            event_tasks: Arc::new(Mutex::new(Vec::new())),
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
            cancel_watchdogs: Arc::new(TokioMutex::new(std::collections::HashMap::new())),
            saved_files: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            relay: Arc::new(tokio::sync::Mutex::new(None)),
            relay_roster: Arc::new(Mutex::new(Vec::new())),
            relay_status: Arc::new(Mutex::new(crate::relay_state::RelayUiStatus::Disabled)),
            relay_event_task: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connect_task: Arc::new(tokio::sync::Mutex::new(None)),
            healing_fps: Arc::new(Mutex::new(std::collections::HashSet::new())),
            auto_retried: Arc::new(Mutex::new(std::collections::HashSet::new())),
            channels: Arc::new(localtrans_core::routing::ChannelTable::new()),
        }
    }

    /// Register an event bridge task for later shutdown
    pub fn register_event_task(&self, handle: tokio::task::JoinHandle<()>) {
        self.event_tasks.lock().unwrap().push(handle);
    }

    /// Abort all event bridge tasks
    pub fn abort_event_tasks(&self) {
        let mut tasks = self.event_tasks.lock().unwrap();
        for task in tasks.drain(..) {
            task.abort();
        }
    }

    // ===== 传输卡状态机表(M2 T2,镜像桌面壳 main.rs AppState 卡片入口) =====

    /// 单写者入口:事件落卡(锁内 apply,不跨 await)。
    /// 返回 Some(dto)=接受(调用方可发 UI 事件);None=卡片不存在/非法迁移(内部 warn)。
    /// 接受后重算 progress_percent/eta_secs 派生字段并置脏。
    pub async fn card_apply(&self, job_id: u64, ev: CardEvent) -> Option<crate::dto::TransferDto> {
        let mut map = self.transfers.lock().await;
        let card = map.get_mut(&job_id)?;
        if !card.apply(ev, now_ms()) {
            tracing::warn!("card_apply: 非法迁移丢弃 job={:016x} state={}", job_id, card.dto.state);
            return None;
        }
        sync_derived_fields(&mut card.dto);
        let dto = card.dto.clone();
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        Some(dto)
    }

    /// 一次性锁内改写卡片(非事件路径:建卡元数据/聚合累计等),未找到返回 false。
    /// 对齐桌面壳:改写同时清掉 last_transition(残留旧迁移防误报)。
    pub async fn card_mutate<F: FnOnce(&mut crate::dto::TransferDto)>(&self, job_id: u64, f: F) -> bool {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&job_id) else { return false };
        f(&mut card.dto);
        card.take_last_transition();
        sync_derived_fields(&mut card.dto);
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// 新建卡片(存在则覆盖——占位卡重发同 ID 场景;调用方保证 ID 语义)
    pub async fn card_create(&self, dto: crate::dto::TransferDto) {
        self.transfers.lock().await.insert(dto.job_id, TransferCard::new(dto));
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// 卡片完整快照(删除路径读 removed/cancelling 用)
    pub async fn card_get(&self, job_id: u64) -> Option<TransferCard> {
        self.transfers.lock().await.get(&job_id).cloned()
    }

    /// 卡片 dto 快照(控制命令/事件发射用)
    pub async fn card_dto(&self, job_id: u64) -> Option<crate::dto::TransferDto> {
        self.transfers.lock().await.get(&job_id).map(|c| c.dto.clone())
    }

    /// 卡片当前状态字符串
    pub async fn card_state(&self, job_id: u64) -> Option<String> {
        self.transfers.lock().await.get(&job_id).map(|c| c.dto.state.clone())
    }

    /// ChunkDone 增量累加(单锁内读改 apply Progress 自环;镜像桌面壳 source_chunk_add)。
    /// 卡片不存在/非 active 时静默忽略(状态机拒绝,返回 None)。
    pub async fn source_chunk_add(&self, job_id: u64, bytes: u64) -> Option<crate::dto::TransferDto> {
        let mut map = self.transfers.lock().await;
        let card = map.get_mut(&job_id)?;
        let (done, total) = (card.dto.done, card.dto.total);
        let (speed, remote) = (card.dto.speed_bps, card.dto.remote_done);
        if !card.apply(CardEvent::Progress {
            done: done + bytes, total, speed_bps: speed, remote_done: remote, health: None,
        }, now_ms()) {
            tracing::warn!("source_chunk_add: 非法迁移丢弃 job={:016x} state={}", job_id, card.dto.state);
            return None;
        }
        sync_derived_fields(&mut card.dto);
        let dto = card.dto.clone();
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        Some(dto)
    }

    /// Speed 事件:done/total 不变,只更新 speed/remote_done(单锁内 Progress 自环)
    pub async fn source_speed(&self, job_id: u64, bps: u64, remote_done: u64) -> Option<crate::dto::TransferDto> {
        let mut map = self.transfers.lock().await;
        let card = map.get_mut(&job_id)?;
        let (done, total) = (card.dto.done, card.dto.total);
        if !card.apply(CardEvent::Progress {
            done, total, speed_bps: bps, remote_done, health: None,
        }, now_ms()) {
            tracing::warn!("source_speed: 非法迁移丢弃 job={:016x} state={}", job_id, card.dto.state);
            return None;
        }
        sync_derived_fields(&mut card.dto);
        let dto = card.dto.clone();
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        Some(dto)
    }

    /// 快照:全表卡片克隆(持久化泵用,含 removed)
    pub async fn snapshot_cards(&self) -> Vec<TransferCard> {
        self.transfers.lock().await.values().cloned().collect()
    }

    /// 快照:活动+历史 DTO 列表(removed 过滤;transfers() 列表用)
    pub async fn snapshot_dtos(&self) -> Vec<crate::dto::TransferDto> {
        self.transfers.lock().await.values()
            .filter(|c| !c.removed)
            .map(|c| c.dto.clone())
            .collect()
    }

    /// view 级删除标记(removed=true,卡片保留在表内;两级删除第一级)
    pub async fn card_mark_removed(&self, job_id: u64) -> bool {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&job_id) else { return false };
        card.removed = true;
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// 移除卡片(删除命令/收尾用),返回是否存在
    pub async fn card_remove(&self, job_id: u64) -> bool {
        let removed = self.transfers.lock().await.remove(&job_id).is_some();
        if removed {
            self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        removed
    }

    /// 取消/删除实删收尾(引擎终态确认先到 / 5s 看门狗超时两路径共用):
    /// abort 看门狗 + 删表行 + 清 throttle + saved_files acc 随行删除。
    /// 返回是否本次真正删除(false=已被并发收尾)。
    /// 注:不动 parts 目录——取消后的分块是续传数据,保留给磁盘历史入口
    /// (list_disk_jobs)决定去留;彻底删除走 finalize_destroy。
    pub async fn finalize_remove(&self, job_id: u64) -> bool {
        if let Some(h) = self.cancel_watchdogs.lock().await.remove(&job_id) {
            h.abort();
        }
        if !self.card_remove(job_id).await {
            return false;
        }
        self.clear_throttle(job_id);
        // saved_files acc 随行删除:Done 只消费文件名保留根,行级删除才是
        // 条目生命周期的终点,防 map 泄漏(原 transfer_remove 同款语义)
        self.saved_files.lock().unwrap().remove(&job_id);
        true
    }

    // ===== 并发队列(M2 T4,镜像桌面壳 main.rs acquire_slot / pending_rank) =====

    /// 获取一个传输槽位 = 全局 permit + 对端串行锁。
    /// 先排全局闸门(permits=max_active_transfers,跨对端共享),
    /// 再拿对端锁(同对端串行,跨对端互不阻塞)。
    /// 返回 (permit, peer_lock_guard):调用方持有至传输编排结束,Drop 自动释放。
    /// 已知特性(同桌面壳):同对端批量任务会占住 gate 名额等 peer 锁,
    /// 跨对端在此期间被挡——先 gate 后 peer 序的固有行为。
    pub async fn acquire_slot(&self, peer_hex: &str)
        -> (tokio::sync::OwnedSemaphorePermit, tokio::sync::OwnedMutexGuard<()>)
    {
        let permit = self.active_gate.clone().acquire_owned().await
            .expect("active_gate 信号量不会关闭");
        let peer_lock = {
            let mut locks = self.peer_locks.lock().await;
            locks.entry(peer_hex.to_string())
                .or_insert_with(|| Arc::new(TokioMutex::new(())))
                .clone()
        };
        let guard = peer_lock.lock_owned().await;
        (permit, guard)
    }

    /// 排队位次近似——同对端 pending 卡按 job_id 排位。
    /// ffi 差异:占位卡 ID 自 u64::MAX 递减分配,创建序 = ID 降序
    /// (桌面壳 card_id 递增故升序);引擎事件不建 pending 卡,表内
    /// pending 恒为占位卡,降序即创建序。泵式近似,不要求跨锁原子;
    /// None=已不在排队(拿到槽位/终态)。
    pub async fn pending_rank(&self, job_id: u64, peer_hex: &str) -> Option<u32> {
        let map = self.transfers.lock().await;
        let mut ids: Vec<u64> = map.values()
            .filter(|c| c.dto.state == "pending" && c.dto.peer == peer_hex)
            .map(|c| c.dto.job_id)
            .collect();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        ids.iter().position(|&id| id == job_id).map(|p| p as u32 + 1)
    }

    // ===== 磁盘历史(M2 T3,镜像桌面壳 finalize_destroy / list_disk_jobs 数据源) =====

    /// parts 目录候选根(对齐 core 实际落盘点,ffi 与 PC 的单 download_dir 不同):
    /// - 拉取(start_pull* 系列)→ config.download_dir;
    /// - 推送接收(respond_offer 应答的 save_dir)→ inbox_dir(accept 时刻);
    /// - 兼容/默认 → dir(inbox 未注入前 inbox==dir)。
    /// 去重保序;扫盘/删目录一律遍历全部根。
    pub async fn parts_roots(&self) -> Vec<std::path::PathBuf> {
        let inbox = self.inbox_dir.read().unwrap().clone();
        let download = self.config.read().await.download_dir.clone();
        let mut roots: Vec<std::path::PathBuf> = Vec::with_capacity(3);
        for r in [inbox, download, self.dir.clone()] {
            if !roots.contains(&r) {
                roots.push(r);
            }
        }
        roots
    }

    /// 删除各候选根下该任务的 parts 目录(manifest+分块);返回是否至少删掉一个。
    /// 日志只打 job_id hex,不打文件路径(隐私红线)。阻塞 IO 下放 spawn_blocking。
    pub async fn remove_parts_dirs(&self, job_id: u64) -> bool {
        let roots = self.parts_roots().await;
        let removed = tokio::task::spawn_blocking(move || {
            let mut n = 0usize;
            for root in roots {
                let p = root.join(format!(".localtrans-parts/{:016x}", job_id));
                if p.is_dir() && std::fs::remove_dir_all(&p).is_ok() {
                    n += 1;
                }
            }
            n
        }).await.unwrap_or(0);
        if removed > 0 {
            tracing::info!("parts 目录实删 job={:016x} dirs={}", job_id, removed);
        }
        removed > 0
    }

    /// destroy 级实删收尾(镜像桌面壳 finalize_destroy):finalize_remove
    /// (abort 看门狗+删表行+清 throttle/acc)+ 删 parts 目录。
    /// 返回是否本次真正删了表行(false=已被并发收尾;parts 仍会尝试清理)。
    pub async fn finalize_destroy(&self, job_id: u64) -> bool {
        let row_removed = self.finalize_remove(job_id).await;
        self.remove_parts_dirs(job_id).await;
        if row_removed {
            tracing::info!("destroy 实删完成 job={:016x}", job_id);
        }
        row_removed
    }

    /// 恢复视图(两级删除第一级的逆操作):removed=false;卡不存在返回 false。
    /// 非事件路径,直接改标记并置脏(镜像桌面壳 restore_disk_job 的在表分支)。
    pub async fn card_restore_view(&self, job_id: u64) -> bool {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&job_id) else { return false };
        card.removed = false;
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// 补扫单个 parts 根的孤儿 manifest 建卡(启动时 inbox_dir 尚未注入——
    /// 安卓侧 start() 后才 set_inbox_dir,推送接收的 parts 落在 inbox 根;
    /// 注入时补一次)。只补表内缺失的卡(or_insert),不覆盖任何在表卡
    /// (在表活动卡不能被磁盘快照改写;在表终态卡已在启动重建时按
    /// "manifest 优先"合并过)。返回新建卡数。
    pub async fn merge_orphans_from_root(&self, root: &std::path::Path) -> usize {
        let root_owned = root.to_path_buf();
        let orphans = tokio::task::spawn_blocking(move || {
            localtrans_core::transfer::orphan_jobs(&root_owned)
        }).await.unwrap_or_default();
        if orphans.is_empty() {
            return 0;
        }
        let mut added = 0usize;
        {
            let mut map = self.transfers.lock().await;
            for oj in orphans {
                if map.contains_key(&oj.job_id) {
                    continue;
                }
                let mut dto = crate::orphan_card_dto(oj.job_id, &oj.manifest);
                sync_derived_fields(&mut dto);
                let mut card = TransferCard::new(dto);
                card.engine_id = Some(oj.job_id);
                map.insert(oj.job_id, card);
                added += 1;
            }
        }
        if added > 0 {
            self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("补扫 parts 根新建孤儿卡 {} 张", added);
        }
        added
    }

    /// Allocate next placeholder ID
    pub fn next_placeholder_id(&self) -> u64 {
        self.next_placeholder_id.fetch_sub(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Check if progress should be throttled (500ms threshold)
    pub fn should_throttle_progress(&self, job_id: u64) -> bool {
        let mut throttle = self.progress_throttle.lock().unwrap();
        let now = Instant::now();
        let last = throttle.entry(job_id).or_insert(now);
        let elapsed = now.duration_since(*last).as_millis();
        if elapsed >= 500 {
            *last = now;
            false // Don't throttle
        } else {
            true // Throttle
        }
    }

    /// Clear throttle entry for terminal states (always emit immediately)
    pub fn clear_throttle(&self, job_id: u64) {
        let mut throttle = self.progress_throttle.lock().unwrap();
        throttle.remove(&job_id);
    }
}
