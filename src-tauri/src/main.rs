// Copyright (c) 2024 LocalTrans contributors.
// Task 16: Tauri 应用壳

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod events;
mod firewall;
mod probe;
mod reconnect;
mod transfer_state;
// Task M0: 自动化测试 API 服务（spec §7.1），仅 test-api 构建才编译，
// 正式构建整个模块不进产物
#[cfg(feature = "test-api")]
mod test_api;

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tauri::{Emitter, Manager};
use tokio::sync::{Mutex, RwLock, mpsc};

use localtrans_core::*;

/// v0.5.0 待答推送：respond + 顺延信号
pub struct PendingOffer {
    pub respond: tokio::sync::oneshot::Sender<Option<std::path::PathBuf>>,
    pub extend: std::sync::Arc<tokio::sync::Notify>,
}

/// 应用全局状态
#[derive(Clone)]
struct AppState {
    /// 数据目录
    dir: std::path::PathBuf,
    /// 设备身份
    identity: Arc<identity::Identity>,
    /// 配置（RwLock 用于运行时修改）
    config: Arc<RwLock<store::Config>>,
    /// 信任列表
    trust: Arc<Mutex<identity::TrustStore>>,
    /// 会话管理器
    sm: Arc<session::SessionManager>,
    /// 发现句柄
    discovery: Arc<discovery::DiscoveryHandle>,
    /// 共享区注册表
    reg: Arc<share::ShareRegistry>,
    /// 是否隐藏（与 DiscoveryConfig 共享）
    hidden: Arc<AtomicBool>,
    /// v0.5.0 待答表 (job_id -> PendingOffer)
    pending_offers: Arc<Mutex<std::collections::HashMap<u64, PendingOffer>>>,
    /// v0.5.0 Auto 档接收完成通知（job_id -> (peer_hex, file_count)）
    auto_offers: Arc<Mutex<std::collections::HashMap<u64, (String, usize)>>>,
    /// P0-2c 待答删除表 (ask_id -> oneshot::Sender<bool>)
    pending_deletes: Arc<Mutex<std::collections::HashMap<u64, tokio::sync::oneshot::Sender<bool>>>>,
    /// 待配对表 (fingerprint hex -> own_code)
    /// name 已在事件中，own_code 由 PairingCodeShown 事件填入
    pending_pairing: Arc<Mutex<std::collections::HashMap<String, String>>>,
    /// 当前设备列表
    devices: Arc<Mutex<Vec<discovery::DeviceInfo>>>,
    /// 传输任务表（Task 4:TransferCard 状态机表,单写者 card_apply 入口)
    transfers: Arc<Mutex<std::collections::HashMap<u64, transfer_state::TransferCard>>>,
    /// 引擎真实 job_id → card_id(事件桥翻译;ID 恒定映射核心)
    engine_to_card: Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    /// 下一卡片 ID(独立计数器,从 1 起;不再用 u64::MAX 递减占位)
    next_card_id: Arc<std::sync::atomic::AtomicU64>,
    /// 传输表脏标记（驱动 data/transfers.json 落盘）
    transfers_dirty: Arc<AtomicBool>,
    /// Task 7:全局并发闸门(permits=max_active_transfers,跨对端放开)
    /// + 对端串行锁表(同对端维持串行,替代全局 xfer_lock)。
    /// active_gate 在 AppState 构造点已有 config(L947 提前 read),直接用真值;
    /// 测试构造默认 3。
    active_gate: Arc<tokio::sync::Semaphore>,
    peer_locks: Arc<Mutex<std::collections::HashMap<String, Arc<Mutex<()>>>>>,
    /// v0.11.x BUG01:占位任务的取消信号表(placeholder_id → cancel 标志)。
    /// 对端离线等 OfferResp 期间真实注册表查不到占位 ID,取消按钮此前
    /// 必报"任务不在运行"——此表补上占位期控制面。
    placeholder_cancels: Arc<Mutex<std::collections::HashMap<u64, std::sync::Arc<std::sync::atomic::AtomicBool>>>>,
    /// 已建立 QUIC 会话的对端指纹集合（SessionUp/Down 维护）。
    /// 发现层的 online 只代表广播可见；这里才是"连接真的活着"，
    /// QUIC 层自带 5s keep-alive + 60s idle 超时作为心跳
    connected_fps: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 进程级 sender 任务表（T12 transfer_throttle 消费）
    sender_jobs: localtrans_core::transfer::sender_state::SenderJobMap,
    /// 中继客户端（可选，当启用中继时存在）
    relay: Arc<Mutex<Option<Arc<localtrans_core::relay::client::RelayClient>>>>,
    /// 中结名册（RemoteDevice 列表）
    relay_roster: Arc<Mutex<Vec<localtrans_core::relay::proto::RemoteDevice>>>,
    /// 中继事件桥任务句柄（用于生命周期管理）
    relay_event_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// M-B3: 中继 connect 任务句柄(重连时 abort 旧连接任务)
    relay_connect_task: Arc<tokio::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    /// 回路 1 去重:正在自愈的指纹(SessionDown 触发时 insert,结束移除)
    healing_fps: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 回路 2 一次性守卫:已自动续传过的 job_id(永不二次自动续传)
    auto_retried: Arc<Mutex<std::collections::HashSet<u64>>>,
    /// Task 5:删除仲裁看门狗(card_id → JoinHandle)。
    /// destroy 活动卡时挂 5s sleep 任务;引擎终态确认先到则 abort。
    cancel_watchdogs: Arc<Mutex<std::collections::HashMap<u64, tokio::task::JoinHandle<()>>>>,
    /// Task 8:推送多文件子任务挂接表(engine job_id → 父卡 card_id)。
    /// 推送编排(push_files/push_files_rel)建父卡后登记每个预期子文件的事件归属;
    /// source 桥 SourceStarted 到达时查此表:命中→child_upsert 挂父卡(不裂变),
    /// 未命中→按原逻辑独立建卡。拉取方向不用此表(start_download_dir 编排本地可控)。
    pending_children: Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    /// 修复轮 P4①:批次父卡 → 共享 offer engine job(小文件批流无单文件
    /// engine job,整批 pause/cancel 需打到 offer job 的 push_control 上)。
    batch_offer_jobs: Arc<Mutex<std::collections::HashMap<u64, u64>>>,
    /// M3a FR5 连接记忆:用户主动连过(成功会话)的设备,持久化 data/connect_memory.json。
    /// 数据/退避纯函数在 core::connect_memory;编排在本壳 reconnect 模块。
    connect_memory: Arc<Mutex<localtrans_core::connect_memory::ConnectMemory>>,
    /// M3a FR5:每设备自动重连任务注册表(fp_hex → JoinHandle)。
    /// 防重入判重 + 任务自摘;任务因"已在会话/失忆/失信任"自行退出。
    reconnect_tasks: Arc<Mutex<std::collections::HashMap<String, tokio::task::JoinHandle<()>>>>,
    /// M3b FR1 通道记录表:每设备 {地址,RTT,估速,稳定性,时间戳},内存态
    /// 不持久化。数据/探测原语/评分决策在 core::routing;编排在本壳 probe 模块。
    channels: Arc<localtrans_core::routing::ChannelTable>,
}

/// 当前 Unix 毫秒(T12: 从 commands.rs 移至根模块，供多处使用)
/// 修复轮 1 C2：卡片 ID 高位段基址——引擎 job_id 从 1 递增（小整数空间），
/// 卡片 ID 从 2^46 起分配，两空间永不撞号
pub(crate) const CARD_ID_BASE: u64 = 0x4000_0000_0000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 指纹 hex 截断显示（前 8 字符，用于设备未发现时的降级显示）
fn peer_hex_chars(hex: &str) -> String {
    hex.get(0..8).unwrap_or(hex).to_string()
}

impl AppState {
    /// 单写者入口：事件落到卡片（锁内 apply，不跨 await）。
    /// 非法迁移（含终态吸收）丢弃并 warn。
    pub async fn card_apply(&self, card_id: u64, ev: transfer_state::CardEvent) {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&card_id) else {
            tracing::warn!("card_apply: 卡片不存在 card={:016x}", card_id);
            return;
        };
        let was_cancelling = card.dto.state == "cancelling";
        if !card.apply(ev, now_ms()) {
            tracing::warn!("card_apply: 非法迁移丢弃 card={:016x} state={}", card_id, card.dto.state);
            return;
        }
        // 取走迁移记录并清零（last_transition 是粘性的，自环不追加 history）
        let transition = card.take_last_transition();
        let (direction, engine_id) = (card.dto.direction.clone(), card.engine_id);
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);

        // Task 5:删除仲裁——cancelling 卡收到引擎终态事件(确认先到)
        // → abort 看门狗 + 实删收尾(看门狗/事件两路径共用 finalize_destroy)
        if was_cancelling {
            let now_terminal = matches!(transition, Some((_, ref to))
                if matches!(to.as_str(), "done" | "failed" | "interrupted"));
            if now_terminal {
                tracing::info!("cancelling 卡确认终态,执行 destroy 收尾 card={:016x}", card_id);
                self.finalize_destroy(card_id).await;
                return;
            }
        }

        // 迁移历史：from≠to 且 pull 方向有引擎任务 → spawn_blocking 追加（尽力而为）
        if let Some((from, to)) = transition {
            if from != to && direction == "pull" {
                if let Some(eid) = engine_id {
                    let parts_root = self.download_parts_root().await;
                    let parts_root_hist = parts_root.clone();
                    // Task 6(简化裁定):只在终态后一次性补写 meta(finished_at/fail_reason)。
                    // brief 原规格含"创建期绑定后即 patch direction 等"——但 PartWriter
                    // 活动期节流写 manifest 会覆盖,受控例外风险高;改为终态后唯一一次
                    // patch(创建期元数据由 rebuild 从引擎写入的溯源字段兜底)。
                    if matches!(to.as_str(), "done" | "failed" | "interrupted") {
                        let dto_now = self.card_dto(card_id).await;
                        tokio::spawn(async move {
                            let _ = tokio::task::spawn_blocking(move || {
                                localtrans_core::transfer::patch_manifest_meta(&parts_root, eid, |m| {
                                    m.finished_at_ms = Some(now_ms());
                                    m.fail_reason = dto_now.as_ref().and_then(|d| d.fail_reason.clone());
                                    // 顺手补全创建期元数据(终态时卡片上字段已齐全)
                                    if let Some(d) = dto_now {
                                        m.direction = d.direction;
                                        m.local_role = d.local_role;
                                        m.display_name = d.name;
                                        m.peer_hex = d.peer;
                                        if m.created_at_ms == 0 {
                                            m.created_at_ms = d.started_at_ms.unwrap_or(0);
                                        }
                                        m.source_path = m.source_path.clone().or(d.source_path);
                                    }
                                })
                            }).await;
                        });
                    }
                    tokio::spawn(async move {
                        let hev = localtrans_core::transfer::HistoryEvent {
                            ts_ms: now_ms(),
                            from,
                            to,
                            reason: None,
                        };
                        if let Err(e) = tokio::task::spawn_blocking(move || {
                            localtrans_core::transfer::append_history(&parts_root_hist, eid, &hev)
                        }).await.map(|r| r) {
                            tracing::debug!("history 追加失败(忽略): {:?}", e);
                        }
                    });
                }
            }
        }
    }

    /// 引擎事件入口：engine_id 翻译后落卡（未登记映射时按 job_id 直查——
    /// 无映射说明卡片也不存在，card_apply 会 warn+丢弃，不复活）
    pub async fn engine_event(&self, engine_id: u64, ev: transfer_state::CardEvent) {
        let card_id = self.card_id_of(engine_id).await;
        self.card_apply(card_id, ev).await;
    }

    /// Task 5:卡片完整快照（TransferCard 克隆;删除路径读 removed/cancelling 用）
    pub async fn card_get(&self, card_id: u64) -> Option<transfer_state::TransferCard> {
        self.transfers.lock().await.get(&card_id).cloned()
    }

    /// Task 5:view 级删除标记(removed=true,卡片保留在表内)
    pub async fn card_mark_removed(&self, card_id: u64) -> bool {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&card_id) else { return false };
        card.removed = true;
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// Task 5:destroy 实删收尾（引擎终态确认/5s 看门狗共用）。
    /// 删表行 + parts 目录(manifest+分块;日志只打 job_id hex 不打文件路径)。
    pub async fn finalize_destroy(&self, card_id: u64) {
        // abort 看门狗（若有）——事件先到时看门狗不再触发
        if let Some(h) = self.cancel_watchdogs.lock().await.remove(&card_id) {
            h.abort();
        }
        // parts 目录名:engine_id 优先,回退 parts_id / card_id(先取,删表后读不到)
        let parts_id = self.card_dto(card_id).await.and_then(|d| d.parts_id);
        let pid = self.engine_id_of(card_id).await
            .map(|e| format!("{:016x}", e))
            .or(parts_id)
            .unwrap_or_else(|| format!("{:016x}", card_id));
        if !self.card_remove(card_id).await {
            return; // 已被并发收尾
        }
        let parts_dir = self.config.read().await.download_dir
            .join(format!(".localtrans-parts/{}", pid));
        let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(&parts_dir)).await;
        tracing::info!("destroy 实删完成 card={:016x}", card_id);
    }

    /// engine→card 映射查询（None=未登记）
    pub async fn engine_card_of(&self, engine_id: u64) -> Option<u64> {
        self.engine_to_card.lock().await.get(&engine_id).copied()
    }

    /// engine→card 翻译（无映射回退 engine_id 本身；调用方保证已建卡绑定）
    pub async fn card_id_of(&self, engine_id: u64) -> u64 {
        self.engine_card_of(engine_id).await.unwrap_or(engine_id)
    }

    /// 登记 engine_id→card_id（创建路径拿到真实 ID 后调用），并写 parts_id
    pub async fn bind_engine_id(&self, engine_id: u64, card_id: u64) {
        self.engine_to_card.lock().await.insert(engine_id, card_id);
        let mut map = self.transfers.lock().await;
        if let Some(card) = map.get_mut(&card_id) {
            card.engine_id = Some(engine_id);
            card.dto.parts_id = Some(format!("{:016x}", engine_id));
        }
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// N1-T1b:被动绑定——只登记 engine_to_card 事件路由,不顶 card.engine_id/parts_id。
    /// 单文件推送的 source job(大文件回拉)用:控制命令必须仍路由首个绑定的
    /// offer job(T16 控制桥共享 Arc 级联暂停/取消),否则取消打到 source 上、
    /// offer 编排在 JobDone 上挂满 1800s 占住并发槽(僵尸 active 卡根因)。
    pub async fn bind_engine_id_passive(&self, engine_id: u64, card_id: u64) {
        self.engine_to_card.lock().await.insert(engine_id, card_id);
    }

    /// 新建卡片（保留传入 dto 的全部字段，仅把 job_id 改为恒定 card_id），返回 card_id。
    /// 修复轮 1：next_card_id 从 0x4000_0000_0000 高位段起——与引擎 job_id
    /// （core next_job_id 从 1 递增）彻底隔离，防撞号。
    pub async fn card_create(&self, mut dto: TransferDto) -> u64 {
        let card_id = self.next_card_id.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        dto.job_id = card_id;
        self.transfers.lock().await.insert(card_id, transfer_state::TransferCard::new(dto));
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        card_id
    }

    /// 快照：全表卡片克隆（持久化泵用,含 removed）
    pub async fn snapshot_cards(&self) -> Vec<transfer_state::TransferCard> {
        self.transfers.lock().await.values().cloned().collect()
    }

    /// 快照：活动+历史 DTO 列表（removed 过滤；4Hz 泵与 list_transfers 用）
    pub async fn snapshot_dtos(&self) -> Vec<TransferDto> {
        self.transfers.lock().await.values()
            .filter(|c| !c.removed)
            .map(|c| c.dto.clone())
            .collect()
    }

    /// card→engine 解析（控制命令用；None=无引擎任务）
    pub async fn engine_id_of(&self, card_id: u64) -> Option<u64> {
        self.transfers.lock().await.get(&card_id)
            .and_then(|c| c.engine_id)
    }

    /// 卡片 dto 快照（控制命令/resume 读字段用）
    pub async fn card_dto(&self, card_id: u64) -> Option<TransferDto> {
        self.transfers.lock().await.get(&card_id).map(|c| c.dto.clone())
    }

    /// 卡片当前状态字符串
    pub async fn card_state(&self, card_id: u64) -> Option<String> {
        self.transfers.lock().await.get(&card_id).map(|c| c.dto.state.clone())
    }

    /// 按状态过滤读快照（内部辅助）
    async fn table_read(&self) -> Vec<TransferDto> {
        self.snapshot_dtos().await
    }

    /// 检查卡片存在
    async fn card_exists(&self, card_id: u64) -> bool {
        self.transfers.lock().await.contains_key(&card_id)
    }

    /// 移除卡片（删除命令用），返回是否存在
    pub async fn card_remove(&self, card_id: u64) -> bool {
        let removed = self.transfers.lock().await.remove(&card_id).is_some();
        if removed {
            self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        removed
    }

    /// 清理终态卡片（clear_completed 用）
    pub async fn card_remove_terminal(&self) -> usize {
        let mut map = self.transfers.lock().await;
        let before = map.len();
        map.retain(|_, c| !matches!(c.dto.state.as_str(), "done" | "failed" | "interrupted"));
        let n = before - map.len();
        drop(map);
        if n > 0 {
            self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        n
    }

    /// 一次性锁内改写卡片（聚合行 done 累加等非事件路径），未找到返回 false。
    /// 修复轮 1 I2：改写同时清掉 last_transition——card_mutate 是非事件路径
    /// 手工改写（如 resume 复活），残留旧迁移会误报进 history
    pub async fn card_mutate<F: FnOnce(&mut TransferDto)>(&self, card_id: u64, f: F) -> bool {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&card_id) else { return false };
        f(&mut card.dto);
        card.take_last_transition();
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        true
    }

    /// parts 根目录（history 追加用；尽力而为，读不到默认空）
    async fn download_parts_root(&self) -> std::path::PathBuf {
        self.config.read().await.download_dir.clone()
    }

    /// ChunkDone 增量累加（brief 裁定:单锁内读改发,不拆两次锁）。
    /// 卡片不存在时静默忽略（终态/未建卡事件丢弃）。
    pub async fn source_chunk_add(&self, card_id: u64, bytes: u64) {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&card_id) else { return };
        let (old_done, total) = (card.dto.done, card.dto.total);
        let speed = card.dto.speed_bps;
        let remote = card.dto.remote_done;
        let _ = card.apply(transfer_state::CardEvent::Progress {
            done: old_done + bytes, total, speed_bps: speed, remote_done: remote, health: None,
        }, now_ms());
    }

    /// Speed 事件:done/total 不变,只更新 speed/remote_done/health(单锁内)
    pub async fn source_speed(&self, card_id: u64, bps: u64, remote_done: u64,
                              health: Option<HealthDto>) {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&card_id) else { return };
        let (done, total) = (card.dto.done, card.dto.total);
        let _ = card.apply(transfer_state::CardEvent::Progress {
            done, total, speed_bps: bps, remote_done, health,
        }, now_ms());
    }

    /// Task 7:获取一个传输槽位 = 全局 permit + 对端串行锁。
    /// 先排全局闸门(permits=max_active_transfers,跨对端共享),
    /// 再拿对端锁(同对端串行,跨对端互不阻塞)。
    /// 返回 (permit, peer_lock_guard):调用方持有至传输编排结束,Drop 自动释放。
    /// 已知特性(修复轮 1 注):同对端批量任务会占住 gate 名额等 peer 锁,
    /// 跨对端在此期间被挡——brief 裁定的先 gate 后 peer 序的固有行为。
    pub async fn acquire_slot(&self, peer_hex: &str)
        -> (tokio::sync::OwnedSemaphorePermit, tokio::sync::OwnedMutexGuard<()>)
    {
        let permit = self.active_gate.clone().acquire_owned().await
            .expect("active_gate 信号量不会关闭");
        let peer_lock = {
            let mut locks = self.peer_locks.lock().await;
            locks.entry(peer_hex.to_string())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = peer_lock.lock_owned().await;
        (permit, guard)
    }

    /// Task 7:排队位次近似——同对端 pending 卡按 card_id(单调≈创建序)排位。
    /// 泵式近似,不要求跨锁原子;None=已不在排队(拿到槽位/终态)。
    pub async fn pending_rank(&self, card_id: u64, peer_hex: &str) -> Option<u32> {
        let map = self.transfers.lock().await;
        let mut ids: Vec<u64> = map.values()
            .filter(|c| c.dto.state == "pending" && c.dto.peer == peer_hex)
            .map(|c| c.dto.job_id)
            .collect();
        ids.sort_unstable();
        ids.iter().position(|&id| id == card_id).map(|p| p as u32 + 1)
    }

    // ===== Task 8: 多文件统一父卡片 =====

    /// 子项挂接:upsert 进父卡 children(存在替换,不存在追加),
    /// 同步累加父卡 total/done。锁内完成,不跨 await。
    /// 修复轮 P1:去重键 = (job_id, name) 二元组——job_id 非空按引擎身份
    /// 匹配(引擎 job 唯一);job_id 为空(小文件批流/拉取子项无 per-file
    /// engine job)按 name 匹配,避免 N 个子项互相覆盖坍缩成 1 条。
    /// job_id 非空时若按 job_id 未命中,退而匹配同名空 job_id 占位项
    /// (批流子项升级为真实引擎子项/重试按名替换),不产生重复行。
    /// 修复轮 P2:替换时父卡 done 只前进不减(max)。
    pub async fn child_upsert(&self, parent_card_id: u64, child: crate::ChildDto) {
        let mut map = self.transfers.lock().await;
        let Some(card) = map.get_mut(&parent_card_id) else { return };
        let slot = if child.job_id.is_empty() {
            card.dto.children.iter().position(|c| c.name == child.name)
        } else {
            card.dto.children.iter().position(|c| c.job_id == child.job_id)
                .or_else(|| card.dto.children.iter()
                    .position(|c| c.job_id.is_empty() && c.name == child.name))
        };
        if let Some(i) = slot {
            let old_done = card.dto.children[i].done;
            let new_done = child.done.max(old_done);
            let mut child = child;
            child.done = new_done;
            card.dto.children[i] = child;
            card.dto.done = card.dto.done.saturating_sub(old_done).saturating_add(new_done);
        } else {
            card.dto.total = card.dto.total.saturating_add(child.total);
            card.dto.done = card.dto.done.saturating_add(child.done);
            card.dto.children.push(child);
        }
        drop(map);
        self.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Task 8:子项全终态 → 计算父卡终态。有失败子项 → failed(reason="N 项失败");
    /// 全成功 → done。非全终态不动父卡。
    /// Task 12 修复轮 1:父卡经此路径从非终态转入 done(全部子项成功)时返回
    /// Some((name, direction)),供调用方补发完成 toast(批次推送/文件夹拉取
    /// 此前无 toast);终态吸收/失败收敛返回 None。
    pub async fn recheck_parent_terminal(&self, parent_card_id: u64)
        -> Option<(String, String)>
    {
        let (all_terminal, failed_count, has_children) = {
            let map = self.transfers.lock().await;
            let Some(card) = map.get(&parent_card_id) else { return None };
            let ch = &card.dto.children;
            (
                !ch.is_empty() && ch.iter().all(|c| matches!(c.state.as_str(), "done" | "failed")),
                ch.iter().filter(|c| c.state == "failed").count(),
                !ch.is_empty(),
            )
        };
        if !has_children || !all_terminal {
            return None;
        }
        // 修复轮 P3:批次父卡终态 → 移除该 peer 的挂接登记(清账,防串扰泄漏)。
        let peer_hex = self.card_dto(parent_card_id).await.map(|d| d.peer);
        if let Some(p) = peer_hex.as_deref().and_then(|h| hex::decode(h).ok())
            .filter(|b| b.len() == 32) {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&p);
            let key = crate::commands::batch_peer_key(&arr);
            self.pending_children_remove(key, parent_card_id).await;
        }
        // 修复轮 P4②:父卡已终态(如整批取消先落 failed"已取消")时
        // 不再改写——终态吸收语义,防止子项回执覆盖取消原因。
        if matches!(
            self.card_state(parent_card_id).await.as_deref(),
            Some("done") | Some("failed") | Some("interrupted")
        ) {
            return None;
        }
        if failed_count > 0 {
            // 修复轮 P4③:失败也走状态机单写者(Pending+Failed 在迁移表内)
            self.card_apply(parent_card_id, transfer_state::CardEvent::Failed {
                reason: Some(format!("{} 项失败", failed_count)),
            }).await;
            None
        } else {
            // pending 直收 Finished 是非法迁移——先 Started 激活再收敛到 done
            let st_now = self.card_state(parent_card_id).await.unwrap_or_default();
            if st_now == "pending" {
                self.card_apply(parent_card_id, transfer_state::CardEvent::Started).await;
            }
            self.card_apply(parent_card_id, transfer_state::CardEvent::Finished).await;
            // Task 12 修复轮 1:非终态→done 边,返回 (name, direction) 供 toast
            self.card_dto(parent_card_id).await
                .map(|d| (d.name, d.direction))
        }
    }

    /// Task 8:登记/查询推送子任务归属(engine job_id → 父卡 card_id)。
    pub async fn pending_children_bind(&self, engine_id: u64, parent_card_id: u64) {
        self.pending_children.lock().await.insert(engine_id, parent_card_id);
    }

    /// Task 8:查子任务归属(不移除——后续 SourceChunkDone/SourceDone 还要查)。
    pub async fn pending_children_parent_of(&self, engine_id: u64) -> Option<u64> {
        self.pending_children.lock().await.get(&engine_id).copied()
    }

    /// 修复轮 P3:移除某挂接键的登记(批次终态/取消后清账,防串扰与泄漏)。
    /// 仅当当前登记仍指向 parent_card_id 时移除——防止把后来批次/retry
    /// 的新登记误删。
    pub async fn pending_children_remove(&self, key: u64, parent_card_id: u64) {
        let mut reg = self.pending_children.lock().await;
        if reg.get(&key).copied() == Some(parent_card_id) {
            reg.remove(&key);
        }
    }

    /// 修复轮 P4①:登记批次父卡 → 共享 offer engine job(SourceStarted
    /// 批次分支;小文件批流子项共享此 offer job,整批控制需触达它)。
    pub async fn batch_offer_job_bind(&self, parent_card_id: u64, engine_id: u64) {
        self.batch_offer_jobs.lock().await.insert(parent_card_id, engine_id);
    }

    /// 修复轮 P4①:查批次父卡的共享 offer job。
    pub async fn batch_offer_job_of(&self, parent_card_id: u64) -> Option<u64> {
        self.batch_offer_jobs.lock().await.get(&parent_card_id).copied()
    }

    /// Task 8:父卡的全部活动子 engine_id(children 中 state=active 的 job_id 反解)。
    /// 整批 pause/cancel 传播用。
    pub async fn active_child_engine_ids(&self, parent_card_id: u64) -> Vec<u64> {
        let map = self.transfers.lock().await;
        map.get(&parent_card_id).map(|c|
            c.dto.children.iter()
                .filter(|c| matches!(c.state.as_str(), "active" | "pending" | "paused"))
                .filter_map(|c| u64::from_str_radix(&c.job_id, 16).ok())
                .collect()
        ).unwrap_or_default()
    }

    /// 修复轮 P4①:父卡是否仍有活动子项(含空 job_id 的小文件批流项)。
    pub async fn has_active_children(&self, parent_card_id: u64) -> bool {
        let map = self.transfers.lock().await;
        map.get(&parent_card_id).map(|c|
            c.dto.children.iter()
                .any(|c| matches!(c.state.as_str(), "active" | "pending" | "paused"))
        ).unwrap_or(false)
    }

    /// Task 8:子项终态更新(child_upsert 别名,语义化入口)——终态后由调用方
    /// recheck_parent_terminal 收敛父卡。
    pub async fn child_finish(&self, parent_card_id: u64, child: crate::ChildDto) {
        self.child_upsert(parent_card_id, child).await;
        self.recheck_parent_terminal(parent_card_id).await;
    }
}

/// Task 12 修复轮 1:父卡经 recheck 转 done 的完成 toast 文案(纯函数,可测)。
/// push 批次 → "推送完成";pull 文件夹 → "下载完成";其余方向 None(不 toast)。
pub(crate) fn done_toast_text(direction: &str, name: &str) -> Option<String> {
    match direction {
        "push" => Some(format!("推送完成: {}", name)),
        "pull" => Some(format!("下载完成: {}", name)),
        _ => None,
    }
}

/// Task 6:持久化卡片条目(透明包装,removed 随卡片落盘)。
/// 新格式 transfers.json 为 `{"cards":[...]}` 包装对象——旧格式是裸数组,
/// 一判即中,保证 Removed 标记可往返(view 删除的卡重启不复活)。
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub(crate) struct TransferCardSerde {
    pub dto: TransferDto,
    #[serde(default)]
    pub removed: bool,
}

/// 新格式顶层包装
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub(crate) struct TransfersFile {
    pub cards: Vec<TransferCardSerde>,
}

/// 读 transfers.json:先试新格式 {"cards":[...]},失败回退旧格式裸数组
/// Vec<TransferDto>(removed 缺省 false);全失败 warn 后返回空(可丢弃缓存,
/// 磁盘孤儿全量重建兜底)。纯函数,便于测试。
pub(crate) fn load_cards_file(dir: &std::path::Path) -> Vec<TransferCardSerde> {
    let path = dir.join("transfers.json");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    if let Ok(file) = serde_json::from_str::<TransfersFile>(&text) {
        return file.cards;
    }
    match serde_json::from_str::<Vec<TransferDto>>(&text) {
        Ok(list) => list
            .into_iter()
            .map(|dto| TransferCardSerde { dto, removed: false })
            .collect(),
        Err(e) => {
            tracing::warn!("transfers.json 解析失败,启动后从磁盘重建: {}", e);
            Vec::new()
        }
    }
}

/// 孤儿 manifest → 卡片 dto(纯函数,rebuild 与 restore_disk_job 共用)。
/// 缺块→interrupted;位图全真→failed(完整性存疑)。meta 有 display_name
/// 用之,无则回退 manifest.file_name。card_id=engine_id(裁定:免映射)。
pub(crate) fn orphan_card_dto(job_id: u64, manifest: &localtrans_core::transfer::manifest::Manifest) -> TransferDto {
    use localtrans_core::protocol::CHUNK_SIZE;
    let missing = manifest.missing_chunks();
    let (state, fail_reason) = if missing.is_empty() {
        ("failed", Some("数据完整性存疑,建议重新拉取".to_string()))
    } else {
        ("interrupted", None)
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
        job_id, // 启动重建卡 card_id=engine_id,免映射
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
        direction: meta.map(|m| m.direction.clone()).unwrap_or_else(|| "pull".into()),
        local_role: meta.map(|m| m.local_role.clone()).unwrap_or_else(|| "destination".into()),
        health: None,
        started_at_ms: meta.map(|m| m.created_at_ms),
        finished_at_ms: meta.and_then(|m| m.finished_at_ms),
        source_path: meta.and_then(|m| m.source_path.clone()),
        fail_reason,
        remote_done: 0, instant: false,
        queue_pos: None, batch_id: None, children: vec![],
        parts_id: Some(format!("{job_id:016x}")),
    }
}

/// Task 6:manifest 优先启动重建(纯函数,便于测试)。
/// 1) orphan_jobs 扫盘建卡(card_id=engine_id);2) transfers.json 补缺/合并;
/// 3) gc_stale_parts 收尾(在扫盘之后跑,全真孤儿的卡已在表,历史可见)。
/// 冲突(同 engine_id):manifest 侧为准(状态/进度),索引侧补 display_name
/// 与 removed 标记。索引独有条目:终态(done/failed)与 push interrupted 保留,
/// pull 非终态无 parts 丢弃(无数据可续)。
pub(crate) fn rebuild_cards(
    data_dir: &std::path::Path,
    download_dir: &std::path::Path,
) -> Vec<transfer_state::TransferCard> {
    // 1. 磁盘孤儿建卡(先扫后 gc——顺序是历史记录可见性的前提)
    let mut cards: Vec<transfer_state::TransferCard> =
        localtrans_core::transfer::orphan_jobs(download_dir)
            .into_iter()
            .map(|oj| {
                let mut c = transfer_state::TransferCard::new(orphan_card_dto(oj.job_id, &oj.manifest));
                c.engine_id = Some(oj.job_id);
                c
            })
            .collect();

    // 2. transfers.json 索引合并/补缺
    for entry in load_cards_file(data_dir) {
        let mut dto = entry.dto;
        let removed = entry.removed;
        // 旧格式迁移盖戳:非终态→interrupted(push 无 parts 保留)
        if !matches!(dto.state.as_str(), "done" | "failed" | "interrupted" | "cancelling") {
            if dto.direction != "push" && !cards.iter().any(|c| c.engine_id == Some(dto.job_id)) {
                continue; // pull 非终态且磁盘无 parts:孤儿,丢弃
            }
            dto.state = "interrupted".into();
            dto.speed_bps = 0;
            if dto.finished_at_ms.is_none() {
                dto.finished_at_ms = Some(now_ms());
            }
        }
        if let Some(c) = cards.iter_mut().find(|c| c.engine_id == Some(dto.job_id)) {
            // 冲突:manifest 侧(已有卡)为准;索引补显示名、removed 与
            // 卡片侧缺失的时间戳/fail_reason(修复轮 1)
            if c.dto.name.is_empty() {
                c.dto.name = dto.name;
            }
            if c.removed != removed {
                c.removed = removed;
            }
            if c.dto.started_at_ms.is_none() {
                c.dto.started_at_ms = dto.started_at_ms;
            }
            if c.dto.finished_at_ms.is_none() {
                c.dto.finished_at_ms = dto.finished_at_ms;
            }
            if c.dto.fail_reason.is_none() {
                c.dto.fail_reason = dto.fail_reason;
            }
        } else {
            // 索引独有(终态 done/failed、push interrupted 等)直接建卡
            let mut c = transfer_state::TransferCard::new(dto);
            c.removed = removed;
            if c.dto.direction == "pull" {
                c.engine_id = Some(c.dto.job_id);
            }
            cards.push(c);
        }
    }

    // 3. gc 位图全真孤儿目录(卡已建,历史记录仍可见)
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let gc = localtrans_core::transfer::gc_stale_parts(download_dir, now);
    if gc > 0 {
        tracing::info!("启动清理位图全真孤儿 parts 目录 {} 个", gc);
    }
    cards
}

/// brief 测试入口:数据目录与下载目录同根的单参版
pub(crate) fn rebuild_cards_from_disk(
    dir: &std::path::Path,
) -> Vec<transfer_state::TransferCard> {
    rebuild_cards(dir, dir)
}

/// Task 6/修复轮 1:持久化落盘列表收集(纯函数,便于测试)。
/// open 卡(非终态)全量;终态卡分两池:removed=true 全量保留(不计数
/// 不截断),removed=false 封顶 150 条——否则 removed 卡被截出 transfers.json,
/// 重启孤儿重建时 removed=false 整卡复活(硬验收破口)。
pub(crate) fn collect_persist_list(cards: &[transfer_state::TransferCard]) -> Vec<TransferCardSerde> {
    let mut open_jobs: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| !matches!(c.dto.state.as_str(), "done" | "failed"))
        .cloned()
        .map(|c| TransferCardSerde { dto: c.dto, removed: c.removed })
        .collect();
    let removed_terminal: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| matches!(c.dto.state.as_str(), "done" | "failed") && c.removed)
        .cloned()
        .map(|c| TransferCardSerde { dto: c.dto, removed: true })
        .collect();
    let mut history: Vec<TransferCardSerde> = cards.iter()
        .filter(|c| matches!(c.dto.state.as_str(), "done" | "failed") && !c.removed)
        .cloned()
        .map(|c| TransferCardSerde { dto: c.dto, removed: false })
        .collect();
    history.truncate(150);
    open_jobs.extend(history);
    open_jobs.extend(removed_terminal);
    open_jobs
}

/// 设备信息 DTO（前端使用）
#[derive(serde::Serialize, Clone, Debug)]
struct DeviceDto {
    fingerprint: String,
    name: String,
    addr: String,
    online: bool,
    /// QUIC 会话已建立（SessionUp 后为 true；发现层 online 只代表广播可见）
    connected: bool,
    /// 是否通过中继连接（本地发现的设备为 false，仅中继名册中的设备为 true）
    #[serde(default)]
    via_relay: bool,
    /// M3c T3:强制走中继开关在位（config.json force_relay_map；卡片角标+菜单勾选态）
    #[serde(default)]
    force_relay: bool,
}

/// 从 localtrans_core::DeviceInfo 转换（connected 由调用方按会话表补齐）
impl From<&localtrans_core::discovery::DeviceInfo> for DeviceDto {
    fn from(d: &localtrans_core::discovery::DeviceInfo) -> Self {
        DeviceDto {
            fingerprint: hex::encode(d.fingerprint),
            name: d.name.clone(),
            addr: d.addr.to_string(),
            online: d.last_seen.elapsed().as_secs() < 30,
            connected: false,
            via_relay: false, // 本地发现的设备默认非中继
            force_relay: false,
        }
    }
}

/// 目录列表响应 DTO（ListResp 是 ControlMsg 变体，不能直接作返回类型）
#[derive(serde::Serialize, Clone)]
struct ListRespDto {
    entries: Vec<protocol::FileEntry>,
    next_cursor: Option<u64>,
}

/// 配对信息 DTO
#[derive(serde::Serialize, Clone)]
struct PairingDto {
    fingerprint: String,
    name: String,
    own_code: String,
}

/// 同意配对返回 DTO
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct GrantConsentDto {
    own_code: String,
}

/// 传输任务 DTO
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
struct TransferDto {
    #[serde(with = "localtrans_core::serde_compat::u64_hex_string")]
    job_id: u64,
    name: String,
    total: u64,
    done: u64,
    state: String, // "pending", "active", "paused", "interrupted", "done", "failed"
    speed_bps: u64,
    peer: String,
    direction: String, // "pull" | "push"
    #[serde(default)]
    local_role: String, // "destination" | "source-push" | "source-pull"
    #[serde(default)]
    health: Option<HealthDto>,
    #[serde(default)]
    started_at_ms: Option<i64>,
    /// 终态时间戳（done/failed/interrupted 盖戳）——前端据此冻结"已用"计时，
    /// 不再出现"已完成却继续走表"
    #[serde(default)]
    finished_at_ms: Option<i64>,
    /// v0.2.8 文件夹任务的恢复参数（"share_id|dir_path"）——重启后
    /// resume_pending 据此重新走 start_pull_dir 编排。单文件任务为 None
    /// （恢复参数在 manifest 的溯源字段里）
    #[serde(default)]
    source_path: Option<String>,
    /// v0.5.0 失败原因文案（"推送请求被对方拒绝"/"对方超时未确认"等;前端直接展示）
    #[serde(default)]
    fail_reason: Option<String>,
    /// v0.10.0 push 方向:对端累计已确认字节(前端显示 backlog,先落字段)
    #[serde(default)]
    remote_done: u64,
    /// v0.10.0 秒传命中标记
    #[serde(default)]
    instant: bool,
    /// v0.11.0 排队位置(队列卡片展示;None=不在排队)
    #[serde(default)]
    queue_pos: Option<u32>,
    /// v0.11.0 批次 id(同一次多选传输归属同批,折叠展示)
    #[serde(default)]
    batch_id: Option<String>,
    /// v0.11.0 文件夹任务的子文件列表(父卡片聚合展示)
    #[serde(default)]
    children: Vec<ChildDto>,
    /// v0.11.0 分块续传任务归属的 parts 目录名
    #[serde(default)]
    parts_id: Option<String>,
}

/// v0.11.0 文件夹任务子文件 DTO(父卡片 children);Task 8 pub 化供 commands.rs 用
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct ChildDto {
    pub job_id: String,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,
}

/// 传输健康度 DTO
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default, PartialEq)]
struct HealthDto {
    #[serde(default)]
    loss_ratio: f64,
    #[serde(default)]
    rtt_ms: u64,
    #[serde(default)]
    cwnd: u64,
    #[serde(default)]
    streams: u32,
}

/// 配置 DTO（前端↔后端）
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ConfigDto {
    device_name: String,
    download_dir: String,
    hidden: bool,
    quic_port: u16,
    discovery_port: u16,
    #[serde(default = "default_consent_timeout_secs")]
    consent_timeout_secs: u64,
    #[serde(default = "default_offer_timeout_secs")]
    offer_timeout_secs: u64,
    /// Task 7:全局并发闸门容量(1-8)
    #[serde(default = "default_max_active_transfers")]
    max_active_transfers: u32,
    shares: Vec<ShareDefDto>,
    // v0.9.0 修:v0.8.3 漏掉 relay 三字段,前端 get_settings 永远拿不到
    // → UI 重启后看不到已保存配置。#[serde(default)] 保证旧 config.json 兼容
    #[serde(default)]
    relay_enabled: bool,
    #[serde(default)]
    relay_server: String,
    #[serde(default)]
    relay_psk: String,
}

fn default_consent_timeout_secs() -> u64 {
    60
}

fn default_offer_timeout_secs() -> u64 {
    60
}

fn default_max_active_transfers() -> u32 {
    3
}

/// 共享区定义 DTO
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct ShareDefDto {
    id: String,
    alias: String,
    path: String,
}

/// 信任对等方 DTO
#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct TrustedPeerDto {
    fingerprint: String,
    name: String,
    /// 本地别名(空=未设置,展示层回退 name)
    #[serde(default)]
    alias: String,
    paired_at: u64,
    browse: bool,
    download: bool,
    push: String, // "ask", "auto", "deny"
}

/// WebView2 运行时注册表键（Evergreen Standless 通道，64 位 OS 上的 32 位视图）
#[cfg(target_os = "windows")]
const REG_KEY_WEBVIEW2: &str = r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";

/// WebView2 检测（Windows）
#[cfg(target_os = "windows")]
fn check_webview2() -> Result<(), ()> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

    let result = Command::new("reg")
        .args(["query", REG_KEY_WEBVIEW2, "/v", "pv"])
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW：GUI 子系统下避免闪黑框
        .output();

    match result {
        Ok(output) if output.status.success() => Ok(()),
        _ => {
            let text: Vec<u16> = "LocalTrans 需要安装 Microsoft Edge WebView2 运行时。\n\n\
                点击确定打开下载页，安装后重新启动 LocalTrans。\n\n\
                https://developer.microsoft.com/microsoft-edge/webview2/"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let title: Vec<u16> = "LocalTrans - 缺少 WebView2 运行时"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            unsafe {
                MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0);
            }
            Err(())
        }
    }
}

/// 合并本地/名册/已连接/信任表并推送 device-list(SessionUp 与 PairingResult 共用)
async fn emit_merged_device_list(app: &tauri::AppHandle, st: &AppState) {
    let devices = st.devices.lock().await.clone();
    let roster = st.relay_roster.lock().await.clone();
    let connected = st.connected_fps.lock().await.clone();
    let aliases = crate::commands::alias_map(&*st.trust.lock().await);
    let trusted = crate::commands::trusted_pairs(&*st.trust.lock().await);
    let dtos = crate::commands::merge_devices(&devices, &roster, &connected, &aliases, &trusted);
    let _ = app.emit("device-list", dtos);
}

/// 配对失败原因 → 可操作建议文案(P2 配对健壮性)。
/// 与 UI 侧 PairingDialog.failureAdvice 同源——SessionEvent 只携带 reason
/// 字符串,壳层 toast 与前端弹窗各自映射,文案修改必须两处同步。
fn pairing_failure_advice(reason: &str) -> String {
    if reason.contains("拒绝") {
        "对方拒绝了本次配对请求，请与对方确认后再试".into()
    } else if reason.contains("超时") {
        "同意门超时：未在限时内完成确认，请重新发起配对".into()
    } else if reason.contains("已结束等待") {
        "对方结束了本次配对，可重新发起配对".into()
    } else if reason.contains("断开") {
        "配对连接已断开：对方可能已离线或取消，请重新发起配对".into()
    } else if reason.contains("3 次") {
        "配对码连续错误 3 次，配对已终止。对方设备进入约 5 分钟冷却期，请稍后再试".into()
    } else {
        format!("配对失败: {reason}")
    }
}

#[cfg_attr(target_os = "windows", allow(dead_code))]
fn main() {
    // 日志先于一切初始化：写入 data/logs/localtrans.log.YYYY-MM-DD（按天滚动，
    // 跟着 exe 走）。release 版是 GUI 子系统没有控制台，不落盘等于没有日志。
    // 需要更详细日志时设环境变量 LOCALTRANS_LOG=debug（默认 info）
    let log_dir = localtrans_core::store::data_dir()
        .expect("数据目录初始化失败")
        .join("logs");
    let file_appender = tracing_appender::rolling::daily(&log_dir, "localtrans.log");
    // Task M1: 改为 Registry 组装（fmt layer + 可选环形缓冲 layer + EnvFilter），
    // 滚动文件写入与 EnvFilter 解析行为（lossy，非法指令忽略）与原
    // fmt().with_env_filter().init() 完全一致。环形缓冲 layer 仅 test-api
    // 构建挂载（logs/tail 数据面，spec §7.1.7）；非 test 构建传 None 空层，零开销。
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_ansi(false);
    #[cfg(feature = "test-api")]
    let ring_layer = test_api::ring::layer();
    #[cfg(not(feature = "test-api"))]
    let ring_layer: Option<tracing_subscriber::layer::Identity> = None;
    tracing_subscriber::registry()
        .with(fmt_layer)
        .with(ring_layer)
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("LOCALTRANS_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .init();
    tracing::info!("LocalTrans 启动，日志目录: {}", log_dir.display());

    // Windows WebView2 检测
    #[cfg(target_os = "windows")]
    if let Err(()) = check_webview2() {
        std::process::exit(1);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.set_focus();
            }
        }))
        // 前端要用 dialog 选目录、opener 打开下载目录——Rust 侧必须注册，
        // 且 capabilities/default.json 需授予权限（Tauri 2 ACL）
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            // 异步核心初始化
            tauri::async_runtime::block_on(async {
                // 1. 数据目录
                let dir = localtrans_core::store::data_dir()
                    .expect("数据目录初始化失败");

                // 2. 设备身份
                let identity = Arc::new(localtrans_core::identity::Identity::load_or_create(&dir)
                    .expect("身份初始化失败"));

                // 3. 配置
                let config = Arc::new(RwLock::new(localtrans_core::store::load_config(&dir)));

                // 4. 信任列表
                let trust = Arc::new(Mutex::new(localtrans_core::identity::TrustStore::load(&dir)));

                // 5. 是否隐藏
                let hidden = Arc::new(AtomicBool::new(config.read().await.hidden));

                // 6. 发现服务
                let discovery_cfg = localtrans_core::discovery::DiscoveryConfig {
                    bind_port: config.read().await.discovery_port,
                    hidden: hidden.clone(),
                    name: config.read().await.device_name.clone(),
                    quic_port: config.read().await.quic_port,
                    fingerprint: identity.fingerprint(),
                    // 重探目标持久化到 data/，重启后跨网段对端自动找回
                    data_dir: Some(dir.clone()),
                    ..Default::default()
                };

                let discovery = Arc::new(localtrans_core::discovery::spawn(
                    discovery_cfg,
                    Arc::new(identity.signing.clone()),
                ).expect("发现服务启动失败"));

                // 7. 会话管理器
                let ctx = localtrans_core::session::SessionCtx {
                    identity: identity.clone(),
                    trust: trust.clone(),
                    config: config.clone(),
                };

                let (sm, mut session_events) = localtrans_core::session::SessionManager::spawn(ctx);

                // 8. QUIC 监听器
                let quic_port = config.read().await.quic_port;
                sm.start_listener(quic_port).await
                    .expect("QUIC 监听器启动失败");

                // 9. 共享区注册表
                let reg = Arc::new(localtrans_core::share::ShareRegistry::new(
                    config.read().await.shares.clone()
                ));

                // 10. RPC 路由器
                let ctrl_rx = sm.take_inbound_ctrl_rx().await
                    .expect("入站控制通道已被占用");

                let (ask_tx, mut ask_rx) = mpsc::channel(8);

                // v0.5.0 Auto 档完成通知通道
                let (auto_tx, mut auto_rx) = mpsc::channel::<localtrans_core::transfer::AutoOfferInfo>(16);
                localtrans_core::transfer::set_auto_offer_hook(auto_tx);

                // P0-2c 删除确认通道
                let (delete_ask_tx, mut delete_ask_rx) = mpsc::channel::<localtrans_core::transfer::DeleteAsk>(8);

                // 10.1 创建进程级 sender 任务表（T12 transfer_throttle 消费）
                let sender_jobs = localtrans_core::transfer::sender_state::new_sender_job_map();

                // 10.2 创建 source 事件桥（T12 Ruling A）
                let (source_tx, mut source_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

                // v0.2.4 乙侧接收事件通道：被推送时接收引擎的 Started/ChunkDone/
                // Done/Failed 转发到这里落 TransferDto——此前乙侧完全没有接收进度行
                let (recv_tx, mut recv_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
                localtrans_core::transfer::set_inbound_recv_hook(recv_tx);

                // 已连接对端指纹集合（SessionUp/Down 维护；watchdog 推送消费）
                let connected_fps = Arc::new(Mutex::new(std::collections::HashSet::<String>::new()));

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
                    sender_jobs.clone(),
                    Some(source_tx),
                );

                // 10.4 入站 SharesChanged 事件泵：对端 watchdog 推送 → 前端事件
                {
                    let app_n = app.handle().clone();
                    let mut notify_rx = sm.subscribe_notify();
                    tokio::spawn(async move {
                        loop {
                            match notify_rx.recv().await {
                                Ok((fp, localtrans_core::protocol::ControlMsg::SharesChanged { share_id })) => {
                                    let _ = app_n.emit("remote-shares-changed", serde_json::json!({
                                        "fingerprint": hex::encode(fp),
                                        "share_id": share_id,
                                    }));
                                }
                                Ok(_) => {}
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::debug!("通知通道滞后丢弃 {} 条", n);
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                            }
                        }
                    });
                }

                // 管理状态
                let app_handle = app.handle().clone();
                let download_dir = config.read().await.download_dir.clone();
                // Task 6:manifest 优先启动重建(孤儿卡 card_id=engine_id;
                // next_card_id 抬底覆盖所有载入 ID,新建高位段卡不撞)
                #[allow(unused_assignments)]
                let mut loaded_ids: Vec<u64> = Vec::new();
                let state = AppState {
                    dir: dir.clone(),
                    identity: identity.clone(),
                    config: config.clone(),
                    trust: trust.clone(),
                    sm: sm.clone(),
                    discovery: discovery.clone(),
                    reg: reg.clone(),
                    hidden: hidden.clone(),
                    pending_offers: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    auto_offers: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    pending_deletes: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    pending_pairing: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    devices: Arc::new(Mutex::new(Vec::new())),
                    transfers: Arc::new(Mutex::new({
                        // Task 6:manifest 优先重建——磁盘孤儿 + transfers.json 合并
                        let cards = rebuild_cards(&dir, &download_dir);
                        loaded_ids = cards.iter().map(|c| c.dto.job_id).collect();
                        cards.into_iter()
                            .map(|c| (c.dto.job_id, c))
                            .collect()
                    })),
                    engine_to_card: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    // 修复轮 1 C2/C3：高位段起始 + 载入卡 job_id 抬底（fetch_max），
                    // 新建卡片不会覆盖已加载行(重建卡 ID=engine_id 小整数也在内)。
                    // 装机修复 BUG-B 第二层:孤儿卡 ID=engine_id(0x8000 段 source id)
                    // 参与抬底会把计数器抬进 0x8000 段 → 新建 card_id 与 engine_id
                    // 同空间,resolve_engine_id 回退无法区分 → 续传"未找到任务"。
                    // 抬底只认 0x4000 段(card 空间);0x8000 段孤儿 ID 不进计数器,
                    // HashMap 键唯一性不受影响(加载表本身防覆盖的是同 ID 行)。
                    next_card_id: {
                        let c = std::sync::atomic::AtomicU64::new(CARD_ID_BASE);
                        for id in loaded_ids {
                            if id < 0x8000_0000_0000_0000 {
                                c.fetch_max(id + 1, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                        Arc::new(c)
                    },
                    transfers_dirty: Arc::new(AtomicBool::new(false)),
                    placeholder_cancels: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    active_gate: Arc::new(tokio::sync::Semaphore::new(
                        config.read().await.max_active_transfers.clamp(1, 8) as usize)),
                    peer_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    connected_fps: connected_fps.clone(),
                    sender_jobs: sender_jobs.clone(),
                    relay: Arc::new(Mutex::new(None)),
                    relay_roster: Arc::new(Mutex::new(Vec::new())),
                    relay_event_task: Arc::new(tokio::sync::Mutex::new(None)),
                    relay_connect_task: Arc::new(tokio::sync::Mutex::new(None)),
                    healing_fps: Arc::new(Mutex::new(std::collections::HashSet::new())),
                    auto_retried: Arc::new(Mutex::new(std::collections::HashSet::new())),
                    cancel_watchdogs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    pending_children: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    batch_offer_jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
                    connect_memory: Arc::new(Mutex::new(
                        localtrans_core::connect_memory::ConnectMemory::load(&dir),
                    )),
                    channels: Arc::new(localtrans_core::routing::ChannelTable::new()),
                    reconnect_tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
                };

                // 创建 state_clone 用于中继事件任务(state 被移动后无法访问)
                let state_clone = state.clone();
                app.manage(state);

                // M3a FR5 连接记忆:启动扫描——对已记忆且仍受信、未在会话的设备
                // 逐台调度自动重连(退避节奏与断线路径同款;记忆为空则零开销)
                reconnect::spawn_startup(app_handle.clone());

                // M3b FR2 通道探测:5min 周期快检(掉 50% 升级全量;活动传输推迟)
                probe::spawn_scheduler(app_handle.clone());

                // 10.5 中继启动：如果配置启用且服务器地址非空，启动中继连接
                {
                    let config = config.read().await;
                    let enabled = config.relay_enabled;
                    let server_str = config.relay_server.clone();
                    let psk = config.relay_psk.clone();
                    let device_name = config.device_name.clone();
                    let hidden = config.hidden;
                    drop(config);

                    if enabled && !server_str.is_empty() {
                        // 配置验证:失败打 WARN 并拒绝启动连接(不再静默"连接中")
                        if let Err(reason) = localtrans_core::relay::validate_relay_config(true, &server_str, &psk) {
                            tracing::warn!("中继配置无效, 已跳过连接: {}", reason);
                            let _ = app.handle().emit("relay-state", serde_json::json!({
                                "status": "ConfigError",
                                "error": reason,
                            }));
                        } else {
                            let Ok(server_addr) = server_str.parse::<std::net::SocketAddr>() else {
                                unreachable!("validate 通过但解析失败");
                            };
                            let relay_config = localtrans_core::relay::client::RelayClientConfig {
                                server_addr,
                                psk,
                                device_name,
                                hidden,
                            };
                            let identity_clone = identity.clone();
                            let app_handle_clone = app.handle().clone();

                            tokio::spawn(async move {
                                match localtrans_core::relay::client::RelayClient::connect(
                                    relay_config,
                                    identity_clone,
                                ).await {
                                    Ok((client, mut event_rx)) => {
                                        tracing::info!("中继连接成功");

                                        // v0.9.0 修:PunchIncoming 分支需要从 state 取 client
                                        // 之前用 `_client` 下划线丢弃,导致 PunchIncoming 时
                                        // st.relay 为 None 走 continue,accept_peer 永远不触发,
                                        // 对端 punch 进不来 → 内层握手超时
                                        *state_clone.relay.lock().await = Some(client.clone());

                                        // 简化的事件桥任务（暂不处理 PunchIncoming）
                                        let app_for_events = app_handle_clone.clone();
                                        let app_for_state = app_handle_clone.clone();

                                        let event_task = tokio::spawn(async move {
                                            while let Some(event) = event_rx.recv().await {
                                                match event {
                                                    localtrans_core::relay::client::RelayEvent::RosterUpdated(roster) => {
                                                        // 暂不处理名册更新
                                                        tracing::debug!("名册更新: {} 台设备", roster.len());
                                                    }
                                                    localtrans_core::relay::client::RelayEvent::StatusChanged(status) => {
                                                        let _ = app_for_events.emit("relay-state", serde_json::json!({
                                                            "status": format!("{:?}", status),
                                                        }));
                                                    }
                                                    localtrans_core::relay::client::RelayEvent::PunchIncoming { from_fp, session_addr } => {
                                                        tracing::info!("收到中继 punch: from={}, session={}", hex::encode(from_fp), session_addr);
                                                        // 远程对端主动连我们：自动 accept 并 adopt 进会话管理器
                                                        // 互信早已建立（名册只含注册设备；未配对对端在 mTLS/信任检查被拒）
                                                        let Some(st) = app_for_state.try_state::<AppState>() else { continue };
                                                        let Some(relay) = st.relay.lock().await.clone() else { continue };
                                                        let sm = st.sm.clone();
                                                        tokio::spawn(async move {
                                                            match relay.accept_peer(session_addr, from_fp).await {
                                                                Ok(conn) => {
                                                                    if let Err(e) = sm.adopt_connection(conn).await {
                                                                        tracing::warn!("中继入站连接 adopt 失败: {}", e);
                                                                    }
                                                                }
                                                                Err(e) => tracing::warn!("中继入站 accept_peer 失败: {}", e),
                                                            }
                                                        });
                                                    }
                                                }
                                            }
                                        });

                                        // 存储事件任务句柄
                                        *state_clone.relay_event_task.lock().await = Some(event_task);

                                        let _ = app_handle_clone.emit("relay-state", serde_json::json!({
                                            "status": "Registered",
                                        }));
                                    }
                                    Err(e) => {
                                        tracing::warn!("中继连接失败: {}", e);
                                        let _ = app_handle_clone.emit("toast", serde_json::json!({
                                            "level": "warning",
                                            "text": format!("中继连接失败: {}", e),
                                        }));
                                    }
                                }
                            });
                        }
                    }
                }

                // 10.3 共享目录 watchdog：指纹变化 → 本机 emit + 推送
                // SharesChanged 给所有已连接对端（对端浏览页自动刷新）
                {
                    let reg_w = (*reg).clone();
                    let sm_w = sm.clone();
                    let app_w = app.handle().clone();
                    let connected_w = connected_fps.clone();
                    localtrans_core::share_watch::spawn_share_watcher(
                        reg_w,
                        std::time::Duration::from_secs(2),
                        move |share_id| {
                            let share_id = share_id.clone();
                            let sm = sm_w.clone();
                            let app = app_w.clone();
                            let connected_set = connected_w.clone();
                            tokio::spawn(async move {
                                // 本机事件（前端本地共享管理用）
                                let _ = app.emit("shares-changed", serde_json::json!({
                                    "share_id": share_id,
                                }));

                                // 推给所有已连接对端（对端浏览页自动刷新）
                                let connected = connected_set.lock().await.clone();
                                for fp_hex in connected {
                                    if let Ok(fp_bytes) = hex::decode(&fp_hex) {
                                        if fp_bytes.len() == 32 {
                                            let mut fp = [0u8; 32];
                                            fp.copy_from_slice(&fp_bytes);
                                            if let Err(e) = sm.send_ctrl(&fp, protocol::ControlMsg::SharesChanged {
                                                share_id: share_id.clone(),
                                            }).await {
                                                tracing::debug!("推送 SharesChanged 失败({}): {}", fp_hex, e);
                                            }
                                        }
                                    }
                                }
                            });
                        },
                    );
                }

                // 启动自检：防火墙开着但缺 LocalTrans 放行规则时直接提醒——
                // 真机排障中"UAC 被取消导致规则没加上"是最常见的坑，不提醒没人知道
                {
                    let app_handle = app.handle().clone();
                    tokio::spawn(async move {
                        let (exists, enabled) = firewall::rule_status_async().await;
                        if !(exists && enabled) {
                            let _ = app_handle.emit("toast", serde_json::json!({
                                "level": "warning",
                                "text": "未检测到防火墙放行规则（UDP 47600-47601）——对方可能发现不了你。请到 设置→防火墙 添加。"
                            }));
                        }
                    });
                }

                // 11. 事件泵任务

                // 会话事件循环
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    while let Some(ev) = session_events.recv().await {
                        match ev {
                            localtrans_core::session::SessionEvent::PairingConsentNeeded { fingerprint, name } => {
                                let fp_hex = hex::encode(fingerprint);

                                let _ = app_handle_clone.emit("pairing-consent-needed", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "name": name
                                }));
                            }
                            localtrans_core::session::SessionEvent::PairingCodeShown { fingerprint, own_code } => {
                                let fp_hex = hex::encode(fingerprint);

                                // 记入待配对表（own_code 用于壳重启后恢复显示）
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.pending_pairing.lock().await.insert(
                                        fp_hex.clone(),
                                        own_code.clone()
                                    );
                                }

                                let _ = app_handle_clone.emit("pairing-code-shown", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "own_code": own_code
                                }));
                            }
                            localtrans_core::session::SessionEvent::PairingWaitConsent { fingerprint, name } => {
                                let fp_hex = hex::encode(fingerprint);

                                let _ = app_handle_clone.emit("pairing-wait-consent", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "name": name
                                }));
                            }
                            localtrans_core::session::SessionEvent::PairingCodeEntry { fingerprint, name } => {
                                let fp_hex = hex::encode(fingerprint);

                                // 记入待配对表（仅 fingerprint，own_code 稍后由 PairingCodeShown 填充）
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.pending_pairing.lock().await.insert(
                                        fp_hex.clone(),
                                        String::new() // own_code 此时未知，PairingCodeShown 时填充
                                    );
                                }

                                let _ = app_handle_clone.emit("pairing-code-entry", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "name": name
                                }));
                            }
                            localtrans_core::session::SessionEvent::PairingResult { fingerprint, ok, reason } => {
                                let fp_hex = hex::encode(fingerprint);

                                // 配对结束（成功或失败）都移出待配对表，避免弹窗残留
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.pending_pairing.lock().await.remove(&fp_hex);
                                }

                                if ok {
                                    // M3a FR5 配对成功自动连接一次:配对本身运行在已建立的
                                    // QUIC 连接上,complete_pairing 写互信后即发 SessionUp——
                                    // "自动连接一次"由该连接天然达成,此处无需再 connect。
                                    // 同时把对端记入连接记忆(双方用户都为此配对点过同意)
                                    if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                        st.connected_fps.lock().await.insert(fp_hex.clone());
                                        {
                                            let mut mem = st.connect_memory.lock().await;
                                            mem.record(&fingerprint);
                                            if let Err(e) = mem.save() {
                                                tracing::warn!("连接记忆落盘失败: {}", e);
                                            }
                                        }
                                        emit_merged_device_list(&app_handle_clone, &*st).await;
                                    }
                                    let _ = app_handle_clone.emit("connection-state", serde_json::json!({
                                        "fingerprint": fp_hex,
                                        "up": true
                                    }));
                                }

                                let _ = app_handle_clone.emit("pairing-result", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "ok": ok,
                                    "reason": reason
                                }));

                                // P2 配对健壮性:失败 toast 分层——
                                // a.「配对码不匹配」(未满 3 次)不弹 toast:可重输,
                                //    反馈由 PairingDialog 内嵌错误条承载(还剩 N 次文案),
                                //    逐次弹 toast 是噪音;
                                // b. 其余失败原因映射为可操作建议文案(与 UI 侧
                                //    PairingDialog.failureAdvice 同源,两处文案须同步改)。
                                if let Some(r) = &reason {
                                    if r != "配对码不匹配" {
                                        let advice = pairing_failure_advice(r);
                                        let _ = app_handle_clone.emit("toast", serde_json::json!({
                                            "level": "warning",
                                            "text": advice
                                        }));
                                    }
                                }
                            }
                            localtrans_core::session::SessionEvent::SessionUp { fingerprint, conn, .. } => {
                                let fp_hex = hex::encode(fingerprint);

                                // M3b FR2:通道登记 + 全量探测(会话建立触发;活动
                                // 传输时后台任务自行推迟,事件泵不阻塞)
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    crate::probe::on_session_up(&st, fingerprint, conn).await;
                                }

                                // 维护会话状态集合并即时重发设备列表（connected 徽章数据源）
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.connected_fps.lock().await.insert(fp_hex.clone());
                                    emit_merged_device_list(&app_handle_clone, &*st).await;
                                }

                                let _ = app_handle_clone.emit("connection-state", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "up": true
                                }));

                                // 回路 2:对端恢复在线 → 自动续传 failed 且有 parts 的任务(每任务仅 1 次)
                                {
                                    let st = app_handle_clone.try_state::<AppState>().unwrap();
                                    let config_arc = st.config.clone();
                                    let transfers_arc = st.transfers.clone();
                                    let retried_arc = st.auto_retried.clone();
                                    let app2 = app_handle_clone.clone();
                                    let fp_hex2 = fp_hex.clone();
                                    tokio::spawn(async move {
                                        let download_dir = config_arc.read().await.download_dir.clone();
                                        let pending: Vec<u64> = localtrans_core::transfer::pending_jobs(&download_dir)
                                            .into_iter().map(|(id, _)| id).collect();
                                        let candidates = {
                                            let transfers = transfers_arc.lock().await;
                                            let retried = retried_arc.lock().await;
                                            localtrans_core::relay::autoheal::auto_resumable_jobs(
                                                transfers.iter().map(|(id, c)| (*id, c.dto.state.clone(), c.dto.peer.clone())).collect(),
                                                &fp_hex2, &pending, &retried,
                                            )
                                        };
                                        for job_id in candidates {
                                            retried_arc.lock().await.insert(job_id);
                                            let _ = app2.emit("toast", serde_json::json!({
                                                "level": "info",
                                                "text": "连接恢复,已自动续传"
                                            }));
                                            let state_for_resume = app2.state::<AppState>();
                                            let _ = crate::commands::resume_pending(
                                                state_for_resume, app2.clone(), format!("{:x}", job_id),
                                            ).await;
                                        }
                                    });
                                }
                            }
                            localtrans_core::session::SessionEvent::SessionDown { fingerprint } => {
                                let fp_hex = hex::encode(fingerprint);

                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.connected_fps.lock().await.remove(&fp_hex);
                                    let devices = st.devices.lock().await.clone();
                                    let roster = st.relay_roster.lock().await.clone();
                                    let connected = st.connected_fps.lock().await.clone();
                                    let aliases = crate::commands::alias_map(&*st.trust.lock().await);
                                    let trusted = crate::commands::trusted_pairs(&*st.trust.lock().await);
                                    let dtos = crate::commands::merge_devices(&devices, &roster, &connected, &aliases, &trusted);
                                    let _ = app_handle_clone.emit("device-list", dtos);
                                }

                                let _ = app_handle_clone.emit("connection-state", serde_json::json!({
                                    "fingerprint": fp_hex,
                                    "up": false
                                }));

                                // 回路 1:中继会话自愈(本地发现无此设备 && 名册有)。
                                // M3a FR6:信任已不成立时(对端发 TrustBroken 移除了对本机的
                                // 信任,或本机用户刚 remove_trusted)豁免——自动重连只会给
                                // 对端弹配对同意门(骚扰);重配对必须由用户重新发起。
                                {
                                    let st = app_handle_clone.try_state::<AppState>().unwrap();
                                    let is_local = st.devices.lock().await.iter()
                                        .any(|d| d.fingerprint == fingerprint);
                                    let in_roster = st.relay_roster.lock().await.iter()
                                        .any(|d| d.fingerprint == fingerprint);
                                    let still_trusted = st.trust.lock().await.is_trusted(&fingerprint);
                                    if !is_local && in_roster && still_trusted {
                                        // 去重:已在自愈中则跳过(insert 返回 false 表示已存在)
                                        if st.healing_fps.lock().await.insert(fp_hex.clone()) {
                                            let relay = st.relay.lock().await.clone();
                                            let sm = st.sm.clone();
                                            let healing = st.healing_fps.clone();
                                            let fp_hex_ui = fp_hex.clone();
                                            let app_ui = app_handle_clone.clone();
                                            tokio::spawn(async move {
                                                let _ = app_ui.emit("peer-reconnecting", serde_json::json!({
                                                    "fingerprint": fp_hex_ui, "active": true
                                                }));
                                                let ok = match relay {
                                                    Some(client) => localtrans_core::relay::autoheal::auto_reconnect(&client, &sm, fingerprint).await,
                                                    None => false,
                                                };
                                                if !ok {
                                                    let _ = app_ui.emit("peer-reconnecting", serde_json::json!({
                                                        "fingerprint": fp_hex_ui, "active": false
                                                    }));
                                                }
                                                // 成功时 SessionUp 事件驱动 UI 清态;此处统一清守卫
                                                healing.lock().await.remove(&fp_hex_ui);
                                            });
                                        }
                                    }
                                }

                                // 回路 3(M3a FR5):连接记忆自动重连——已记忆且仍受信的设备
                                // 进入退避重连(2/4/8/16s 封顶 30s,5 败回落;静默,仅成败各一条 toast)。
                                // 与回路 1 互补:本回路只走本地发现表地址,中继独有设备由回路 1 兜底。
                                reconnect::schedule_on_down(&app_handle_clone, fingerprint).await;
                            }
                            localtrans_core::session::SessionEvent::TrustBroken { fingerprint, peer_name } => {
                                // M3a FR6:对端已移除对本机的信任。core 侧收到通知即删除了
                                // 本端信任条目(双盲对称重配,徽章自动降级为待配对);这里
                                // 收尾:清 connected 集合、清连接记忆(防回路 3 按记忆重连)、
                                // 刷新设备列表、toast 告知。紧随其后的 SessionDown 事件
                                // 走既有断连展示,回路 1/3 因信任与记忆均已清而自然豁免。
                                let fp_hex = hex::encode(fingerprint);
                                if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                    st.connected_fps.lock().await.remove(&fp_hex);
                                    {
                                        let mut mem = st.connect_memory.lock().await;
                                        if mem.remove(&fingerprint) {
                                            if let Err(e) = mem.save() {
                                                tracing::warn!("TrustBroken 后连接记忆清除落盘失败: {}", e);
                                            }
                                        }
                                    }
                                    emit_merged_device_list(&app_handle_clone, &*st).await;
                                }
                                let _ = app_handle_clone.emit("toast", serde_json::json!({
                                    "level": "warning",
                                    "text": match peer_name {
                                        Some(n) if !n.is_empty() => format!("{} 已移除对你的信任，连接已断开", n),
                                        _ => "对方已移除对你的信任，连接已断开".to_string(),
                                    }
                                }));
                            }
                        }
                    }
                });

                // 发现设备 watch
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    let mut rx = discovery.devices.clone();
                    loop {
                        if rx.changed().await.is_ok() {
                            let devices: Vec<discovery::DeviceInfo> = rx.borrow().clone();

                            // 更新状态（connected 从会话集合补齐后再发 DTO）
                            if let Some(st) = app_handle_clone.try_state::<AppState>() {
                                *st.devices.lock().await = devices;
                                let devices = st.devices.lock().await.clone();
                                let roster = st.relay_roster.lock().await.clone();
                                let connected = st.connected_fps.lock().await.clone();
                                let aliases = crate::commands::alias_map(&*st.trust.lock().await);
                                let trusted = crate::commands::trusted_pairs(&*st.trust.lock().await);
                                let dtos = crate::commands::merge_devices(&devices, &roster, &connected, &aliases, &trusted);
                                let _ = app_handle_clone.emit("device-list", dtos);
                            }
                        }
                    }
                });

                // v0.2.4 乙侧接收事件泵：hook 通道 → TransferDto（direction=pull、
                // local_role=destination——本机是被推送的接收方）
                {
                    let app_handle_clone = app_handle.clone();
                    tokio::spawn(async move {
                        while let Some(ev) = recv_rx.recv().await {
                            let Some(st) = app_handle_clone.try_state::<AppState>() else { continue };
                            use localtrans_core::transfer::ProgressEvent as PE;
                            match ev {
                                PE::Started { job_id, name, total } => {
                                    // 修复轮 1 C1：建卡/绑定/事件三分离——card_id 由
                                    // card_create 分配（高位段），引擎 job_id 经映射绑定；
                                    // 后续事件全部走 engine_event 翻译，不再裸用 job_id
                                    let cid = st.card_create(TransferDto {
                                        job_id: 0, name, total, done: 0,
                                        state: "pending".into(),
                                        speed_bps: 0,
                                        // 乙侧不知道对端指纹（编排任务未传）——用
                                        // connected 集合的第一个兜底，前端只显示名
                                        peer: String::new(),
                                        direction: "pull".into(),
                                        local_role: "destination".into(),
                                        health: None,
                                        started_at_ms: None,
                                        finished_at_ms: None, source_path: None, fail_reason: None,
                                        remote_done: 0, instant: false,
                                        queue_pos: None, batch_id: None, children: vec![], parts_id: Some(format!("{:016x}", job_id)),
                                    }).await;
                                    st.bind_engine_id(job_id, cid).await;
                                    st.card_apply(cid, transfer_state::CardEvent::Started).await;
                                }
                                PE::Resumed { job_id, already_bytes } => {
                                    st.engine_event(job_id, transfer_state::CardEvent::Started).await;
                                    let cid = st.card_id_of(job_id).await;
                                    st.card_mutate(cid, |d| d.done = already_bytes).await;
                                }
                                PE::ChunkDone { job_id, bytes, .. } => {
                                    let cid = st.card_id_of(job_id).await;
                                    st.source_chunk_add(cid, bytes).await;
                                    // done 累加推进从 pending 提前到 active 的场景由
                                    // Started 先行保证;此处仅 Progress 自环
                                }
                                PE::Done { job_id } => {
                                    // Task 12:仅活动→终态边触发完成 toast(启动重建/恢复不触发——
                                    // 终态吸收使重复 Finished 走不到迁移,name 不进 tracing 只进 emit json)
                                    let cid = st.card_id_of(job_id).await;
                                    let was_terminal = st.card_state(cid).await
                                        .map(|s| matches!(s.as_str(), "done" | "failed" | "interrupted"))
                                        .unwrap_or(true);
                                    st.engine_event(job_id, transfer_state::CardEvent::Finished).await;
                                    if !was_terminal {
                                        if let Some(name) = st.card_dto(cid).await.map(|d| d.name) {
                                            let _ = app_handle_clone.emit("toast", serde_json::json!({
                                                "level": "success",
                                                "text": format!("下载完成: {}", name)
                                            }));
                                        }
                                    }
                                    // v0.5.0 Auto 档完成系统通知（降级 toast）
                                    if let Some((peer_hex, count)) =
                                        st.auto_offers.lock().await.remove(&job_id)
                                    {
                                        let name = st.devices.lock().await.iter()
                                            .find(|d| hex::encode(&d.fingerprint) == peer_hex)
                                            .map(|d| d.name.clone())
                                            .unwrap_or_else(|| peer_hex_chars(&peer_hex));
                                        let dir = st.config.read().await.download_dir.display().to_string();
                                        let body = format!("{} 推送了 {} 个文件，已存入 {}", name, count, dir);
                                        use tauri_plugin_notification::NotificationExt;
                                        let notify = app_handle_clone.notification().builder()
                                            .title("LocalTrans 已接收文件")
                                            .body(&body);
                                        if let Err(err) = notify.show() {
                                            tracing::warn!("系统通知失败(降级 toast): {}", err);
                                            let _ = app_handle_clone.emit("toast", serde_json::json!({
                                                "level": "info", "text": body
                                            }));
                                        }
                                    }
                                }
                                PE::Failed { job_id, reason } => {
                                    st.engine_event(job_id, transfer_state::CardEvent::Failed {
                                        reason: Some(reason.clone()),
                                    }).await;
                                    let _ = app_handle_clone.emit("toast", serde_json::json!({
                                        "level": "error",
                                        "text": format!("接收失败: {}", reason)
                                    }));
                                }
                                PE::InstantHit { job_id, name, total } => {
                                    // 秒传命中:补建卡(若 Started 未到)后直接 Finished（C1 同源）
                                    let cid = match st.engine_card_of(job_id).await {
                                        Some(cid) => cid,
                                        None => {
                                            let cid = st.card_create(TransferDto {
                                                job_id: 0, name: name.clone(), total, done: total,
                                                state: "pending".into(), speed_bps: 0,
                                                peer: String::new(), direction: "pull".into(),
                                                local_role: "destination".into(), health: None,
                                                started_at_ms: None, finished_at_ms: None,
                                                source_path: None, fail_reason: None,
                                                remote_done: 0, instant: true,
                                                queue_pos: None, batch_id: None, children: vec![],
                                                parts_id: Some(format!("{:016x}", job_id)),
                                            }).await;
                                            st.bind_engine_id(job_id, cid).await;
                                            cid
                                        }
                                    };
                                    st.card_mutate(cid, |d| {
                                        d.name = name.clone(); d.total = total; d.done = total;
                                        d.instant = true;
                                    }).await;
                                    // 秒传卡常以 pending 落地（Started 未到，本分支补建），
                                    // 而 pending 直收 Finished 是非法迁移——会被状态机丢弃，
                                    // 卡片永远卡"等待中"（M6 pc-pc 场景重复推送实测踩中）。
                                    // 与 recheck_parent_terminal 同款处理：先 Started 激活再收敛。
                                    if st.card_state(cid).await.as_deref() == Some("pending") {
                                        st.card_apply(cid, transfer_state::CardEvent::Started).await;
                                    }
                                    st.card_apply(cid, transfer_state::CardEvent::Finished).await;
                                }
                                _ => {}
                            }
                        }
                    });
                }

                // ask_rx 循环
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    while let Some(ask) = ask_rx.recv().await {
                        let fp_hex = hex::encode(ask.from);

                        // 存入待答表
                        let job = ask.job_id;
                        let deadline = ask.deadline_epoch_ms;
                        if let Some(st) = app_handle_clone.try_state::<AppState>() {
                            st.pending_offers.lock().await.insert(job, PendingOffer {
                                respond: ask.respond,
                                extend: ask.extend,
                            });

                            // 超时清理看门狗：sleep 到 deadline+3s 醒来后清理残留条目
                            // 权衡说明：若有顺延，真实 deadline 晚于 deadline_epoch_ms，看门狗可能早醒。
                            // 顺延场景用户正在选目录，紧接着会 respond(remove 掉)；若顺延后仍超时，
                            // 条目由看门狗清理。误删概率=用户恰在 deadline+3s 边缘还没选完目录——可接受。
                            let app_handle_wd = app_handle_clone.clone();
                            tokio::spawn(async move {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default();
                                let target_ms = deadline as u64 + 3000;
                                let duration_ms = target_ms.saturating_sub(now.as_millis() as u64);
                                tokio::time::sleep(std::time::Duration::from_millis(duration_ms)).await;
                                if let Some(st) = app_handle_wd.try_state::<AppState>() {
                                    let mut pending = st.pending_offers.lock().await;
                                    // 仍存在 = 没人应答过(respond_offer/remove 都会拿走) → 超时残留,清理
                                    if pending.contains_key(&job) {
                                        pending.remove(&job);
                                        drop(pending);
                                        st.auto_offers.lock().await.remove(&job);
                                    }
                                }
                            });
                        }

                        let files: Vec<serde_json::Value> = ask.files.iter()
                            .map(|f| serde_json::json!({
                                "name": f.name,
                                "size": f.size,
                                "rel_dir": f.rel_dir
                            }))
                            .collect();

                        let _ = app_handle_clone.emit("offer-request", serde_json::json!({
                            "job_id": ask.job_id,
                            "peer": fp_hex,
                            "files": files,
                            "deadline_epoch_ms": ask.deadline_epoch_ms
                        }));
                    }
                });

                // auto_rx 泵：Auto 档接收时记录，完成时发系统通知
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    while let Some(info) = auto_rx.recv().await {
                        if let Some(st) = app_handle_clone.try_state::<AppState>() {
                            let peer_hex = hex::encode(info.peer);
                            st.auto_offers.lock().await.insert(info.job_id, (peer_hex, info.file_count));
                        }
                    }
                });

                // delete_ask_rx 循环:远程删除确认 → 前端模态
                let app_handle_d = app_handle.clone();
                let trust_for_d = trust.clone();
                tokio::spawn(async move {
                    while let Some(ask) = delete_ask_rx.recv().await {
                        if let Some(st) = app_handle_d.try_state::<AppState>() {
                            st.pending_deletes.lock().await.insert(ask.ask_id, ask.respond);
                            // 超时看门狗:deadline+3s 清残留(镜像 offer 看门狗)
                            let wd = app_handle_d.clone();
                            let (aid, dl) = (ask.ask_id, ask.deadline_epoch_ms);
                            tokio::spawn(async move {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                                let wait = (dl as u64 + 3000).saturating_sub(now.as_millis() as u64);
                                tokio::time::sleep(std::time::Duration::from_millis(wait)).await;
                                if let Some(st) = wd.try_state::<AppState>() {
                                    st.pending_deletes.lock().await.remove(&aid);
                                }
                            });
                        }
                        let peer_name = trust_for_d.lock().await
                            .get(&ask.from)
                            .map(|p| if p.alias.is_empty() { p.name.clone() } else { p.alias.clone() })
                            .unwrap_or_else(|| "未知设备".into());
                        let _ = app_handle_d.emit("delete-request", serde_json::json!({
                            "ask_id": ask.ask_id,
                            "fingerprint": hex::encode(ask.from),
                            "peer_name": peer_name,
                            "share_id": ask.share_id,
                            "name": ask.name,
                            "is_dir": ask.is_dir,
                            "entry_count": ask.entry_count,
                            "deadline_epoch_ms": ask.deadline_epoch_ms,
                        }));
                    }
                });

                // source 事件桥（T12 Ruling A）
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    while let Some(ev) = source_rx.recv().await {
                        let Some(st) = app_handle_clone.try_state::<AppState>() else { continue };
                        use localtrans_core::transfer::ProgressEvent as PE;
                        match ev {
                            PE::SourceStarted { job_id, role, peer, name, total } => {
                                // Task 8:批次/重试挂接(子项挂父卡,不裂变);
                                // N1-T1b:单文件推送挂接——source job 被动绑回占位卡,
                                // 不建第二张卡(修复一次推送 done×2;取消经 offer job
                                // 级联,修复僵尸 active 卡占并发槽)。
                                // 修复轮 P3:活动批次(batch_peer_key)优先,子项重试
                                // (retry_peer_key)其次——retry 不顶掉活动批次登记。
                                if matches!(role, localtrans_core::transfer::SourceRole::SourcePush)
                                    && commands::source_push_attach(&st, job_id, &peer, &name, total).await
                                {
                                    continue;
                                }
                                // ID 恒定：推送方向占位卡已建（engine_to_card 有映射）→
                                // 绑定 + Started；无映射（source-pull 被取方被动任务）→ 建卡
                                let local_role = match role {
                                    localtrans_core::transfer::SourceRole::SourcePush => "source-push",
                                    localtrans_core::transfer::SourceRole::SourcePull => "source-pull",
                                };
                                let direction = if matches!(role, localtrans_core::transfer::SourceRole::SourcePush) { "push" } else { "pull" };
                                let mapped = st.engine_card_of(job_id).await.is_some();
                                if !mapped {
                                    let cid = st.card_create(TransferDto {
                                        job_id: 0, name, total, done: 0,
                                        state: "pending".into(),
                                        speed_bps: 0,
                                        peer: hex::encode(peer),
                                        direction: direction.into(),
                                        local_role: local_role.into(),
                                        health: None,
                                        started_at_ms: None,
                                        finished_at_ms: None, source_path: None, fail_reason: None,
                                        remote_done: 0, instant: false,
                                        queue_pos: None, batch_id: None, children: vec![], parts_id: None,
                                    }).await;
                                    st.bind_engine_id(job_id, cid).await;
                                }
                                st.engine_event(job_id, transfer_state::CardEvent::Started).await;
                            }
                            PE::SourceChunkDone { job_id, bytes, .. } => {
                                // Task 8:批次子任务 → 字节累到子项,不动父卡计数
                                if let Some(pid) = st.pending_children_parent_of(job_id).await {
                                    crate::commands::batch_child_chunk(&st, pid, job_id, bytes).await;
                                    continue;
                                }
                                let cid = st.card_id_of(job_id).await;
                                st.source_chunk_add(cid, bytes).await;
                            }
                            PE::SourceSpeed { job_id, bps, remote_done, loss_ratio, rtt_ms, cwnd, streams } => {
                                let cid = st.card_id_of(job_id).await;
                                st.source_speed(cid, bps, remote_done,
                                    Some(HealthDto { loss_ratio, rtt_ms, cwnd, streams })).await;
                            }
                            PE::SourceDone { job_id } => {
                                // Task 8:批次子任务 → 子项落 done + 父终态收敛
                                if let Some(pid) = st.pending_children_parent_of(job_id).await {
                                    // Task 12 修复轮 1:父卡经 recheck 转 done 时补发完成 toast
                                    if let Some((name, direction)) =
                                        crate::commands::batch_child_finish(&st, pid, job_id, "done", None).await
                                    {
                                        if let Some(text) = crate::done_toast_text(&direction, &name) {
                                            let _ = app_handle_clone.emit("toast", serde_json::json!({
                                                "level": "success",
                                                "text": text
                                            }));
                                        }
                                    }
                                    continue;
                                }
                                // 完成态满格兜底（对端 RecvAck 已确认逐块校验落盘）
                                // Task 12:仅活动→终态边触发完成 toast(重建/恢复不触发;name 不进 tracing)
                                let cid = st.card_id_of(job_id).await;
                                let was_terminal = st.card_state(cid).await
                                    .map(|s| matches!(s.as_str(), "done" | "failed" | "interrupted"))
                                    .unwrap_or(true);
                                st.engine_event(job_id, transfer_state::CardEvent::Finished).await;
                                if !was_terminal {
                                    if let Some(name) = st.card_dto(cid).await.map(|d| d.name) {
                                        let _ = app_handle_clone.emit("toast", serde_json::json!({
                                            "level": "success",
                                            "text": format!("推送完成: {}", name)
                                        }));
                                    }
                                }
                            }
                            PE::SourceFailed { job_id, reason } => {
                                if let Some(pid) = st.pending_children_parent_of(job_id).await {
                                    let _ = crate::commands::batch_child_finish(&st, pid, job_id, "failed", Some(reason.clone())).await;
                                    let _ = app_handle_clone.emit("toast", serde_json::json!({
                                        "level": "error",
                                        "text": format!("推送失败: {}", reason)
                                    }));
                                    continue;
                                }
                                st.engine_event(job_id, transfer_state::CardEvent::Failed {
                                    reason: Some(reason.clone()),
                                }).await;
                                // toast 同现有 handler 风格
                                let _ = app_handle_clone.emit("toast", serde_json::json!({
                                    "level": "error",
                                    "text": format!("推送失败: {}", reason)
                                }));
                            }
                            _ => {}
                        }
                    }
                });

                // v0.2.9 传输进度聚合：脏检查 + 动态频率。
                // 此前 4Hz 无条件全表克隆+序列化+emit——空闲时（无任何任务）
                // 也在跑，任务表含 150 条历史时每秒白克隆 4 次，常驻 CPU 大头。
                // 现在：有任何 open 任务（active/pending/paused）→ 4Hz；
                //       全部终态 → 降到 1Hz 且只在表内容变化时发（空闲几乎零开销）。
                // （本 tokio 版本 Interval 无 period_set——频率切换时重建 interval）
                let app_handle_clone = app.handle().clone();
                tokio::spawn(async move {
                    use std::collections::hash_map::DefaultHasher;
                    use std::hash::{Hash, Hasher};

                    const FAST: tokio::time::Duration = tokio::time::Duration::from_millis(250);
                    const SLOW: tokio::time::Duration = tokio::time::Duration::from_secs(1);
                    let mut interval = tokio::time::interval(FAST);
                    let mut fast_mode = true;
                    let mut last_sig: Option<u64> = None; // 表内容签名（空闲脏检查用）

                    loop {
                        interval.tick().await;

                        let Some(st) = app_handle_clone.try_state::<AppState>() else { continue };
                        let jobs = st.snapshot_dtos().await;

                        let has_open = jobs.iter().any(|d|
                            matches!(d.state.as_str(), "active" | "pending" | "paused"));

                        if has_open {
                            if !fast_mode {
                                interval = tokio::time::interval(FAST);
                                fast_mode = true;
                            }
                            last_sig = None;
                        } else {
                            if fast_mode {
                                interval = tokio::time::interval(SLOW);
                                fast_mode = false;
                            }
                            // 空闲脏检查：签名没变连克隆序列化都不做
                            let mut h = DefaultHasher::new();
                            let mut sorted: Vec<&TransferDto> = jobs.iter().collect();
                            sorted.sort_by_key(|d| d.job_id);
                            for d in sorted {
                                (d.job_id, d.state.as_str(), d.done, d.name.as_str()).hash(&mut h);
                            }
                            let sig = h.finish();
                            if last_sig == Some(sig) {
                                continue;
                            }
                            last_sig = Some(sig);
                        }

                        let _ = app_handle_clone.emit("transfer-progress", serde_json::json!({
                            "jobs": jobs
                        }));
                    }
                });

                // 传输任务持久化：脏标记驱动的 1s 落盘（data/transfers.json，
                // 临时文件+rename 原子替换）。终态历史封顶 150 条防无限增长。
                let app_handle_clone = app_handle.clone();
                tokio::spawn(async move {
                    let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(1));

                    loop {
                        interval.tick().await;

                        let Some(st) = app_handle_clone.try_state::<AppState>() else { continue };
                        if !st.transfers_dirty.swap(false, std::sync::atomic::Ordering::Relaxed) {
                            continue;
                        }

                        // Task 6:序列化改为 TransferCardSerde{dto, removed}——
                        // view 删除的卡(removed=true)落盘标记,重启不整卡复活;
                        // 修复轮 1:removed 卡豁免 150 条终态封顶(见 collect_persist_list)
                        let all = st.snapshot_cards().await;
                        let file = TransfersFile { cards: collect_persist_list(&all) };

                        let path = st.dir.join("transfers.json");
                        let tmp = path.with_extension("json.tmp");
                        match serde_json::to_string_pretty(&file)
                            .map_err(|e| e.to_string())
                            .and_then(|s| std::fs::write(&tmp, s).map_err(|e| e.to_string()))
                            .and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()))
                        {
                            Ok(_) => {}
                            Err(e) => tracing::warn!("传输任务持久化失败: {}", e),
                        }
                    }
                });

                // Task M0: 测试 API 服务启动。编译期 feature 门（本 cfg）+
                // 运行期 LOCALTRANS_TEST_API=1 环境变量门（函数内部）双闸；
                // 端口绑定失败在函数内部兜底，不拖垮主应用
                #[cfg(feature = "test-api")]
                test_api::start_test_api(app.handle().clone());

                Ok(())
            })
        })
        .invoke_handler(tauri::generate_handler![
            // 设备命令
            commands::list_devices,
            commands::set_hidden,
            commands::probe_now,
            commands::add_manual_device,
            // M3a T3 名片体系：生成/粘贴添加（解析→多地址入重探）
            commands::get_business_card,
            commands::add_by_card,
            commands::connect,

            // 配对命令
            commands::get_pairing_pending,
            commands::submit_pair_code,
            commands::reject_pairing,
            commands::grant_consent,
            commands::deny_consent,
            commands::cancel_pairing_wait,

            // 浏览/传输命令
            commands::list_shares_remote,
            commands::list_dir_remote,
            commands::start_download,
            commands::push_files,
            commands::push_files_rel,
            commands::start_download_dir,
            commands::start_download_batch,
            commands::expand_local_paths,
            commands::prepare_shutdown,
            commands::respond_offer,
            commands::offer_extend,
            commands::transfer_action,
            commands::retry_child,
            commands::respond_delete,

            // 队列命令
            commands::list_transfers,
            commands::pending_resume_jobs,
            commands::resume_pending,

            // 传输表清理命令（T12）
            commands::clear_completed_transfers,
            commands::remove_transfer,
            commands::transfer_throttle,
            commands::has_parts,

            // 历史记录命令（Task 6）
            commands::list_disk_jobs,
            commands::restore_disk_job,
            commands::destroy_disk_job,

            // 设置命令
            commands::get_settings,
            commands::save_settings,
            commands::add_share,
            commands::remove_share,
            commands::list_trusted,
            commands::set_perms,
            commands::remove_trusted,
            commands::set_alias,

            // 中继命令
            commands::set_relay_config,
            commands::relay_status,

            // 系统命令
            commands::add_firewall_rule,
            commands::get_device_fingerprint,
            commands::get_network_status,
            // M3b T4 通道表只读查询（探测选路观测面；test_api invoke ReadOnly 同源）
            commands::list_channels,
            commands::probe_now_peer,
            commands::set_force_relay,
            commands::open_logs_dir,
            commands::open_download_dir,

            // 前端观测（Task M1）：logBridge 上报入口，无条件注册（见命令注释）
            commands::ui_log,

            // 测试通道桥（Task M2）：无条件注册，函数体 feature 门控
            commands::test_bridge_hello,
            commands::test_bridge_result,
        ])
        .run(tauri::generate_context!())
        .expect("LocalTrans 启动失败");
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use std::sync::Arc;

    /// Task 4 表级测试辅助:AppState 逐字段构造。
    /// transfers/engine_to_card/next_card_id 真实化,其余字段最小离线构造
    /// (Identity/TrustStore 走临时目录;SessionManager/Discovery 离线 spawn,
    /// 端口 0;测试只触碰 transfers 域)。
    pub async fn test_app_state() -> AppState {
        let dir = tempfile::TempDir::new().unwrap().keep();
        let identity = Arc::new(identity::Identity::load_or_create(&dir).unwrap());
        let config: Arc<RwLock<store::Config>> = Arc::new(RwLock::new(Default::default()));
        let trust = Arc::new(Mutex::new(identity::TrustStore::load(&dir)));
        let connect_memory = Arc::new(Mutex::new(
            localtrans_core::connect_memory::ConnectMemory::load(&dir),
        ));
        let (sm, _ev_rx) = session::SessionManager::spawn(session::SessionCtx {
            identity: identity.clone(),
            trust: trust.clone(),
            config: config.clone(),
        });
        let discovery = Arc::new(discovery::spawn(discovery::DiscoveryConfig {
            bind_port: 0,
            hidden: Arc::new(AtomicBool::new(false)),
            name: "test".into(),
            quic_port: 0,
            fingerprint: identity.fingerprint(),
            data_dir: None,
            ..Default::default()
        }, Arc::new(identity.signing.clone())).expect("测试发现服务启动失败"));
        AppState {
            dir,
            identity,
            config,
            trust,
            sm,
            discovery,
            reg: Arc::new(share::ShareRegistry::new(vec![])),
            hidden: Arc::new(AtomicBool::new(false)),
            pending_offers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            auto_offers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_deletes: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_pairing: Arc::new(Mutex::new(std::collections::HashMap::new())),
            devices: Arc::new(Mutex::new(Vec::new())),
            transfers: Arc::new(Mutex::new(std::collections::HashMap::new())),
            engine_to_card: Arc::new(Mutex::new(std::collections::HashMap::new())),
            next_card_id: Arc::new(std::sync::atomic::AtomicU64::new(crate::CARD_ID_BASE)),
            transfers_dirty: Arc::new(AtomicBool::new(false)),
            placeholder_cancels: Arc::new(Mutex::new(std::collections::HashMap::new())),
            active_gate: Arc::new(tokio::sync::Semaphore::new(3)),
            peer_locks: Arc::new(Mutex::new(std::collections::HashMap::new())),
            connected_fps: Arc::new(Mutex::new(std::collections::HashSet::new())),
            sender_jobs: localtrans_core::transfer::sender_state::new_sender_job_map(),
            relay: Arc::new(Mutex::new(None)),
            relay_roster: Arc::new(Mutex::new(Vec::new())),
            relay_event_task: Arc::new(tokio::sync::Mutex::new(None)),
            relay_connect_task: Arc::new(tokio::sync::Mutex::new(None)),
            healing_fps: Arc::new(Mutex::new(std::collections::HashSet::new())),
            auto_retried: Arc::new(Mutex::new(std::collections::HashSet::new())),
            cancel_watchdogs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            pending_children: Arc::new(Mutex::new(std::collections::HashMap::new())),
            batch_offer_jobs: Arc::new(Mutex::new(std::collections::HashMap::new())),
            connect_memory,
            channels: Arc::new(localtrans_core::routing::ChannelTable::new()),
            reconnect_tasks: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_app_state;

    #[test]
    fn done_toast_text_by_direction() {
        // Task 12 修复轮 1:方向→文案纯函数
        assert_eq!(crate::done_toast_text("push", "照片 3 个文件").as_deref(),
            Some("推送完成: 照片 3 个文件"));
        assert_eq!(crate::done_toast_text("pull", "[文件夹] docs").as_deref(),
            Some("下载完成: [文件夹] docs"));
        assert_eq!(crate::done_toast_text("", "x"), None);
        assert_eq!(crate::done_toast_text("other", "x"), None);
    }

    #[tokio::test]
    async fn recheck_to_done_returns_toast_info_once() {
        // Task 12 修复轮 1:父卡经 recheck 从非终态转 done 恰好返回一次
        // (name, direction);终态吸收(第二次 recheck)返回 None 不重复 toast
        let st = test_app_state().await;
        let parent = st.card_create(crate::TransferDto {
            job_id: 0, name: "批次卡".into(), total: 0, done: 0,
            state: "pending".into(), speed_bps: 0, peer: "aa".into(),
            direction: "push".into(), local_role: "source-push".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: Some("b".into()), children: vec![], parts_id: None,
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "a.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "b.txt".into(),
            total: 10, done: 0, state: "active".into(),
        }).await;
        // 未全终态:None,父卡不迁移
        assert_eq!(st.recheck_parent_terminal(parent).await, None);
        assert_eq!(st.card_state(parent).await.as_deref(), Some("pending"));
        // 最后一子落 done:第一次收敛 → Some((name, direction))
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "b.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        let info = st.recheck_parent_terminal(parent).await;
        assert_eq!(info.as_ref().map(|(n, d)| (n.as_str(), d.as_str())),
            Some(("批次卡", "push")), "仅首次转 done 返回 toast 信息");
        assert_eq!(st.card_state(parent).await.as_deref(), Some("done"));
        // 终态吸收:再次 recheck 不返回 → 不重复 toast
        assert_eq!(st.recheck_parent_terminal(parent).await, None);
    }

    #[tokio::test]
    async fn gate_limits_concurrent_and_reports_queue_pos() {
        let st = test_app_state().await; // active_gate permits=3(测试构造给 3)
        // 三个不同对端占满 gate(peer 锁互不影响,均立即获得)
        let mut permits = vec![];
        for p in ["aa", "bb", "cc"] {
            permits.push(st.acquire_slot(p).await);
        }
        // 第 4 个:应排队(不返回)
        let waiter = tokio::spawn({ let st = st.clone(); async move {
            let _p = st.acquire_slot("dd").await;
            "got"
        }});
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "第 4 个在排队");
        // 释放一个 → 等待者获得
        drop(permits.pop());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(waiter.is_finished(), "出队获得槽位");
    }

    #[tokio::test]
    async fn different_peers_do_not_block_each_other() {
        let st = test_app_state().await; // permits=3
        let _a = st.acquire_slot("aa").await;
        // gate 未满:不同对端立即获得,不互相等待
        let b = tokio::time::timeout(std::time::Duration::from_millis(200),
            st.acquire_slot("bb")).await;
        assert!(b.is_ok(), "不同对端不应被 aa 的 peer 锁挡住");
    }

    #[tokio::test]
    async fn same_peer_serializes_on_peer_lock() {
        let st = test_app_state().await; // permits=3
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
    async fn card_lifecycle_single_writer_no_replace() {
        let st = test_app_state().await;
        let card_id = st.card_create(TransferDto {
            job_id: 0, name: "批".into(), total: 10, done: 0, state: "pending".into(),
            speed_bps: 0, peer: "aa".repeat(32), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: None,
            finished_at_ms: None, source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        }).await;
        assert_ne!(card_id, 0);
        // 引擎 Started 到达:先绑定再事件,ID 不变
        st.bind_engine_id(0xdead, card_id).await;
        st.engine_event(0xdead, transfer_state::CardEvent::Started).await;
        st.engine_event(0xdead, transfer_state::CardEvent::Progress {
            done: 5, total: 10, speed_bps: 1, remote_done: 0, health: None }).await;
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].job_id, card_id, "卡片 ID 恒定");
        assert_eq!(snap[0].state, "active");
        assert_eq!(snap[0].done, 5);
        assert_eq!(snap[0].parts_id.as_deref(), Some("000000000000dead"));
    }

    #[tokio::test]
    async fn receiver_side_bind_and_translate() {
        // 修复轮 1 C1 锁定:接收侧 card_create→bind(engine_id≠card_id)→
        // engine_event(Started/Progress) 全程经映射翻译,ID 恒定
        let st = test_app_state().await;
        let engine_id: u64 = 7; // 引擎 job_id(小整数空间)
        let cid = st.card_create(TransferDto {
            job_id: 0, name: "收".into(), total: 10, done: 0, state: "pending".into(),
            speed_bps: 0, peer: String::new(), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: None,
            finished_at_ms: None, source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        }).await;
        assert_ne!(cid, engine_id, "card_id 不等于引擎 job_id");
        st.bind_engine_id(engine_id, cid).await;
        st.engine_event(engine_id, transfer_state::CardEvent::Started).await;
        st.engine_event(engine_id, transfer_state::CardEvent::Progress {
            done: 6, total: 10, speed_bps: 2, remote_done: 0, health: None }).await;
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap.len(), 1, "只有一张卡,无幽灵行");
        assert_eq!(snap[0].job_id, cid, "ID=card_create 返回值");
        assert_eq!(snap[0].state, "active");
        assert_eq!(snap[0].done, 6, "done 经映射更新");
        assert_eq!(snap[0].parts_id.as_deref(), Some("0000000000000007"));
    }

    #[tokio::test]
    async fn resume_pending_card_id_resolves_to_engine_id() {
        // 修复轮 1 锁定:resume_pending 收到 card_id(0x4000 高位段)时,
        // resolve_engine_id 必须翻译回引擎 job_id;无映射(旧引擎直传)回退原值
        let st = crate::test_support::test_app_state().await;
        let engine_id: u64 = 7;
        let cid = st.card_create(TransferDto {
            job_id: 0, name: "续".into(), total: 10, done: 0, state: "interrupted".into(),
            speed_bps: 0, peer: String::new(), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: None,
            finished_at_ms: None, source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        }).await;
        st.bind_engine_id(engine_id, cid).await;

        use crate::commands::resolve_engine_id;
        assert_eq!(resolve_engine_id(&st, cid).await, engine_id,
            "card_id 翻译回 engine_id");
        assert_eq!(resolve_engine_id(&st, 42).await, 42,
            "无映射回退原值(旧 engine job_id 直传兼容)");
    }

    #[tokio::test]
    async fn orphan_ids_do_not_pollute_card_counter() {
        // 装机修复 BUG-B 第二层:启动抬底只认 0x4000 段(card 空间)。
        // 磁盘孤儿卡 ID=engine_id(0x8000 段 source id,如 0x8000...002),
        // 若参与 fetch_max 会把 next_card_id 抬进 0x8000 段 → 新建 card_id
        // 与 engine_id 同空间 → resolve_engine_id 回退无法区分 → 续传
        // "未找到任务"(装机日志 card=8000000000000003 即此污染产物)。
        let c = std::sync::atomic::AtomicU64::new(crate::CARD_ID_BASE);
        let loaded_ids: Vec<u64> = vec![5, 0x4000_0000_0000_0042, 0x8000_0000_0000_0002];
        for id in loaded_ids {
            if id < 0x8000_0000_0000_0000 {
                c.fetch_max(id + 1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let next = c.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(next, 0x4000_0000_0000_0043, "抬到 card 段最大+1");
        assert!(next < 0x8000_0000_0000_0000, "计数器永不进 0x8000 段");
    }

    #[tokio::test]
    async fn card_create_does_not_shadow_loaded_history() {
        // 修复轮 1 C3 锁定:预置历史行 job_id=5 → 计数器抬底 → 新卡 ID≠5 且旧行仍在
        let st = test_app_state().await;
        // 模拟启动加载:表里已有 job_id=5 的历史行
        {
            let mut map = st.transfers.lock().await;
            let mut d = TransferDto {
                job_id: 5, name: "旧".into(), total: 9, done: 9, state: "done".into(),
                speed_bps: 0, peer: "c".repeat(32), direction: "pull".into(),
                local_role: "destination".into(), health: None, started_at_ms: Some(1),
                finished_at_ms: Some(2), source_path: None, fail_reason: None,
                remote_done: 0, instant: false, queue_pos: None, batch_id: None,
                children: vec![], parts_id: None,
            };
            d.parts_id = Some(format!("{:016x}", 5));
            map.insert(5, transfer_state::TransferCard::new(d));
        }
        st.next_card_id.fetch_max(6, std::sync::atomic::Ordering::Relaxed);

        let cid = st.card_create(TransferDto {
            job_id: 0, name: "新".into(), total: 1, done: 0, state: "pending".into(),
            speed_bps: 0, peer: String::new(), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: None,
            finished_at_ms: None, source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        }).await;
        assert_ne!(cid, 5, "新卡不覆盖历史行 ID");
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap.len(), 2, "旧行仍在");
        assert!(snap.iter().any(|d| d.job_id == 5 && d.name == "旧"));
        assert!(snap.iter().any(|d| d.job_id == cid && d.name == "新"));
    }

    // ===== 修复轮 1:持久化截断豁免 removed 卡 =====

    #[test]
    fn persist_truncate_exempts_removed_cards() {
        // >150 条终态卡,其中若干 removed=true:removed 卡全量保留,
        // truncate(150) 只作用于 removed=false 池
        let mk = |job_id: u64, removed: bool| {
            let mut c = transfer_state::TransferCard::new(TransferDto {
                job_id, name: "x".into(), total: 1, done: 1, state: "done".into(),
                speed_bps: 0, peer: "aa".into(), direction: "pull".into(),
                local_role: "destination".into(), health: None, started_at_ms: None,
                finished_at_ms: None, source_path: None, fail_reason: None,
                remote_done: 0, instant: false, queue_pos: None, batch_id: None,
                children: vec![], parts_id: None,
            });
            c.removed = removed;
            c
        };
        let mut cards: Vec<transfer_state::TransferCard> = Vec::new();
        for i in 0..160 {
            cards.push(mk(i as u64, i % 40 == 0)); // 4 张 removed + 156 张正常
        }
        let list = collect_persist_list(&cards);
        let removed_in: Vec<_> = list.iter().filter(|s| s.removed).map(|s| s.dto.job_id).collect();
        assert_eq!(removed_in.len(), 4, "removed 卡全部落盘");
        for id in removed_in {
            assert!(cards.iter().any(|c| c.dto.job_id == id));
        }
        let normal = list.iter().filter(|s| !s.removed).count();
        assert_eq!(normal, 150, "截断只作用于 removed=false 池");
        assert_eq!(list.len(), 154);
    }

    // ===== 修复轮 1:重建冲突补索引侧元数据 =====

    #[test]
    fn rebuild_conflict_fills_index_metadata() {
        // 冲突(同 engine_id):manifest 侧为准,但索引行的 started_at_ms/
        // finished_at_ms/fail_reason 在卡片侧缺时补上
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000007");
        std::fs::create_dir_all(&job).unwrap();
        // 磁盘 meta 无时间戳、无 fail_reason
        std::fs::write(job.join("manifest.json"),
            r#"{"file_name":"g.bin","total_size":5,"chunk_hashes":["00"],"received":[false]}"#).unwrap();
        std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
            "job_id": "7", "name": "旧名", "total": 5, "done": 0,
            "state": "failed", "speed_bps": 0, "peer": "aa", "direction": "pull",
            "started_at_ms": 111, "finished_at_ms": 222,
            "fail_reason": "对端断开"
        }]).to_string()).unwrap();
        let cards = rebuild_cards(tmp.path(), tmp.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].dto.started_at_ms, Some(111), "补索引 started_at_ms");
        assert_eq!(cards[0].dto.finished_at_ms, Some(222), "补索引 finished_at_ms");
        assert_eq!(cards[0].dto.fail_reason.as_deref(), Some("对端断开"), "补索引 fail_reason");
    }

    #[tokio::test]
    async fn unknown_engine_event_dropped_not_revived() {
        let st = test_app_state().await;
        // 终态卡再收事件:不复活不炸
        let card_id = st.card_create(TransferDto {
            job_id: 0, name: "x".into(), total: 1, done: 1, state: "failed".into(),
            speed_bps: 0, peer: "b".repeat(32), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: Some(1),
            finished_at_ms: Some(2), source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        }).await;
        st.engine_event(card_id, transfer_state::CardEvent::Started).await;
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap[0].state, "failed");
    }

    // ===== Task 6: manifest 优先启动重建 + 持久化改造 =====

    #[test]
    fn startup_rebuild_prefers_manifest_meta() {
        // 磁盘有 meta 的孤儿 + 索引有同 ID 旧记录 → 用 meta 的 display_name
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000003");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("manifest.json"), serde_json::json!({
            "file_name": "视频.mp4", "total_size": 5,
            "chunk_hashes": ["00"], "received": [false],
            "meta": { "direction": "pull", "local_role": "destination",
                      "display_name": "我的视频", "peer_hex": "aabb",
                      "created_at_ms": 123 }
        }).to_string()).unwrap();
        // 索引记录 display_name 缺失/陈旧
        std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
            "job_id": "3", "name": "旧名", "total": 5, "done": 0,
            "state": "done", "speed_bps": 0, "peer": "aabb", "direction": "pull"
        }]).to_string()).unwrap();

        let cards = rebuild_cards_from_disk(tmp.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].dto.name, "我的视频");
        assert_eq!(cards[0].dto.state, "interrupted", "缺块孤儿→interrupted");
    }

    #[test]
    fn fully_received_orphan_is_failed_with_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000004");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("manifest.json"),
            r#"{"file_name":"b.bin","total_size":5,"chunk_hashes":["00"],"received":[true]}"#).unwrap();
        let cards = rebuild_cards_from_disk(tmp.path());
        assert_eq!(cards[0].dto.state, "failed");
        assert!(cards[0].dto.fail_reason.as_deref().unwrap().contains("完整性"));
    }

    #[test]
    fn transfers_json_new_format_round_trips_removed() {
        // 新格式 {"cards":[...]}:removed=true 往返保留,重启不复活
        let tmp = tempfile::tempdir().unwrap();
        let dto = TransferDto {
            job_id: 3, name: "x".into(), total: 1, done: 1, state: "done".into(),
            speed_bps: 0, peer: "aa".into(), direction: "pull".into(),
            local_role: "destination".into(), health: None, started_at_ms: None,
            finished_at_ms: None, source_path: None, fail_reason: None,
            remote_done: 0, instant: false, queue_pos: None, batch_id: None,
            children: vec![], parts_id: None,
        };
        let file = TransfersFile { cards: vec![TransferCardSerde { dto: dto.clone(), removed: true }] };
        std::fs::write(tmp.path().join("transfers.json"),
            serde_json::to_string(&file).unwrap()).unwrap();
        let cards = load_cards_file(tmp.path());
        assert_eq!(cards.len(), 1);
        assert!(cards[0].removed, "removed 标记往返保留");
        assert_eq!(cards[0].dto.job_id, 3);
    }

    #[test]
    fn transfers_json_old_bare_array_format_still_loads() {
        // 旧格式裸数组兼容读取(removed 缺省 false)
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
            "job_id": "3", "name": "旧名", "total": 5, "done": 0,
            "state": "done", "speed_bps": 0, "peer": "aabb", "direction": "pull"
        }]).to_string()).unwrap();
        let cards = load_cards_file(tmp.path());
        assert_eq!(cards.len(), 1);
        assert!(!cards[0].removed);
    }

    #[test]
    fn corrupted_transfers_json_rebuilds_from_disk() {
        // 损坏文件:warn 后丢弃索引,磁盘孤儿照常建卡(可丢弃缓存)
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("transfers.json"), b"not json {{").unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000005");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("manifest.json"),
            r#"{"file_name":"c.bin","total_size":5,"chunk_hashes":["00"],"received":[false]}"#).unwrap();
        let cards = rebuild_cards(tmp.path(), tmp.path());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].dto.state, "interrupted");
    }

    #[test]
    fn view_removed_card_not_revived_after_rebuild() {
        // 硬验收:removed=true 的 pull interrupted 卡(磁盘有 parts)重建后仍 removed
        let tmp = tempfile::tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000006");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("manifest.json"),
            r#"{"file_name":"d.bin","total_size":5,"chunk_hashes":["00"],"received":[false]}"#).unwrap();
        let file = TransfersFile { cards: vec![TransferCardSerde {
            dto: TransferDto {
                job_id: 6, name: "d.bin".into(), total: 5, done: 0,
                state: "interrupted".into(), speed_bps: 0, peer: "aa".into(),
                direction: "pull".into(), local_role: "destination".into(),
                health: None, started_at_ms: None, finished_at_ms: Some(1),
                source_path: None, fail_reason: None, remote_done: 0, instant: false,
                queue_pos: None, batch_id: None, children: vec![], parts_id: Some("0000000000000006".into()),
            },
            removed: true,
        }] };
        std::fs::write(tmp.path().join("transfers.json"),
            serde_json::to_string(&file).unwrap()).unwrap();
        let cards = rebuild_cards(tmp.path(), tmp.path());
        assert_eq!(cards.len(), 1);
        assert!(cards[0].removed, "视图删除的卡不得整卡复活");
    }

    #[test]
    #[cfg(target_os = "windows")]
    fn webview2_registry_key_is_stable() {
        // 键路径与 GUID 必须与 Edge WebView2 Evergreen 官方一致——
        // 拼错则每台未装运行时的机器会误报/漏报
        assert!(REG_KEY_WEBVIEW2.starts_with(r"HKLM\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\"));
        assert!(REG_KEY_WEBVIEW2.ends_with("{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}"));
    }

    mod persistence_tests {
        use super::*;
        use std::fs;
        use std::path::Path;

        /// Task 6:写最小 manifest 的辅助(缺块,interrupted 语义)
        fn write_manifest(download_dir: &Path, job_id: u64, file_name: &str) {
            let job = download_dir.join(".localtrans-parts").join(format!("{job_id:016x}"));
            fs::create_dir_all(&job).unwrap();
            fs::write(job.join("manifest.json"), serde_json::json!({
                "file_name": file_name, "total_size": 5,
                "chunk_hashes": ["00"], "received": [false]
            }).to_string()).unwrap();
        }

        #[test]
        fn rebuild_converts_indexed_active_to_interrupted() {
            // 索引行 active + 磁盘有 manifest → interrupted 盖戳(manifest 优先)
            let tmp = tempfile::TempDir::new().unwrap();
            write_manifest(tmp.path(), 0x1, "a.bin");
            std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
                "job_id": "1", "name": "a.bin", "total": 5, "done": 0,
                "state": "active", "speed_bps": 100, "peer": "aa", "direction": "pull"
            }]).to_string()).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0].dto.state, "interrupted");
            assert_eq!(cards[0].dto.speed_bps, 0);
        }

        #[test]
        fn rebuild_drops_orphan_index_row_without_parts() {
            // 索引行 active 且磁盘无 parts → 丢弃(无数据可续)
            let tmp = tempfile::TempDir::new().unwrap();
            std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
                "job_id": "1", "name": "a.bin", "total": 5, "done": 0,
                "state": "active", "speed_bps": 100, "peer": "aa", "direction": "pull"
            }]).to_string()).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 0);
        }

        #[test]
        fn rebuild_push_orphan_kept_as_interrupted() {
            // push 无 parts 概念 → 只改状态保留
            let tmp = tempfile::TempDir::new().unwrap();
            std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
                "job_id": "2", "name": "b.bin", "total": 5, "done": 0,
                "state": "active", "speed_bps": 100, "peer": "aa", "direction": "push"
            }]).to_string()).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0].dto.state, "interrupted");
        }

        #[test]
        fn rebuild_keeps_terminal_rows_without_parts() {
            let tmp = tempfile::TempDir::new().unwrap();
            std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([
                {"job_id": "1", "name": "a", "total": 5, "done": 0, "state": "done",
                 "speed_bps": 0, "peer": "aa", "direction": "pull"},
                {"job_id": "2", "name": "b", "total": 5, "done": 0, "state": "failed",
                 "speed_bps": 0, "peer": "aa", "direction": "pull"}
            ]).to_string()).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 2);
        }

        #[test]
        fn rebuild_manifest_wins_conflict() {
            // 冲突:索引 done + 磁盘缺块 → manifest 侧状态为准(interrupted)
            let tmp = tempfile::TempDir::new().unwrap();
            write_manifest(tmp.path(), 3, "c.bin");
            std::fs::write(tmp.path().join("transfers.json"), serde_json::json!([{
                "job_id": "3", "name": "旧名", "total": 5, "done": 5,
                "state": "done", "speed_bps": 0, "peer": "aa", "direction": "pull"
            }]).to_string()).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 1, "不出现双卡");
            assert_eq!(cards[0].dto.state, "interrupted");
        }

        #[test]
        fn rebuild_gc_removes_fully_received_dir_but_card_survives() {
            // 位图全真:gc 删目录,但卡已建(历史可见,failed+完整性存疑)
            let tmp = tempfile::TempDir::new().unwrap();
            let job = tmp.path().join(".localtrans-parts/0000000000000009");
            fs::create_dir_all(&job).unwrap();
            fs::write(job.join("manifest.json"),
                r#"{"file_name":"e.bin","total_size":5,"chunk_hashes":["00"],"received":[true]}"#).unwrap();
            let cards = rebuild_cards(tmp.path(), tmp.path());
            assert_eq!(cards.len(), 1);
            assert_eq!(cards[0].dto.state, "failed");
            assert!(!job.exists(), "gc 删了目录");
        }

        #[test]
        fn load_cards_file_handles_missing_file() {
            let tmp = tempfile::TempDir::new().unwrap();
            assert_eq!(load_cards_file(tmp.path()).len(), 0);
        }

        #[test]
        fn transfer_dto_fail_reason_defaults_none() {
            // 老 transfers.json 无 fail_reason 字段 → 反序列化 None
            let old = r#"{"job_id":"0x1","name":"a","total":1,"done":0,"state":"failed",
                         "speed_bps":0,"peer":"aa","direction":"push"}"#;
            let dto: TransferDto = serde_json::from_str(old).unwrap();
            assert!(dto.fail_reason.is_none());
        }
    }
}
