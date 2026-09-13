//! Sender-side 任务状态:在 MetaReq 时建,SourceDone/Failed 时清理。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Duration;

use crate::transfer::manifest::Manifest;
use crate::transfer::ProgressEvent;

/// Sender 任务状态
pub struct SenderJobState {
    pub job_id: u64,
    pub src_path: PathBuf,
    pub manifest: Manifest,
    pub dest_hint: Option<(String, String)>,

    pub bytes_counter: Arc<AtomicU64>,
    pub active_streams: Arc<AtomicU32>,
    pub throttle_cap: Arc<AtomicU32>,
    pub progress_tx: mpsc::Sender<ProgressEvent>,
    /// probe 任务停止信号:默认构造时已建好一个 fresh oneshot,
    /// 上层可在 register 时用 .take() 取走替换为可观测的 stop channel。
    /// 用 Arc<Mutex<>> 包装以便从任意 Arc 克隆调用(解决 Arc::get_mut 在多克隆时失败问题)
    pub probe_stop_tx: Option<Arc<Mutex<Option<oneshot::Sender<()>>>>>,
    /// 暂停标志:transfer_action 设置为 true 时,FetchReq 处理器跳过发送
    pub paused: Arc<AtomicBool>,
    /// 取消标志:transfer_action 设置为 true 时,FetchReq 处理器触发清理并跳过
    pub cancelled: Arc<AtomicBool>,
    /// 关联的推送 offer_id(大文件推送反向取流时由 push: 前缀解析)。
    /// None = 普通 pull。push_files 失败路径按它反查清理。
    pub offer_id: Option<u64>,
    /// v0.10.0 对端累计已收字节(RecvProgress 更新,source_probe 500ms 读取)
    pub remote_done: Arc<AtomicU64>,
}

/// 建一个新 SenderJobState(probe_stop_tx 默认建好一个 fresh oneshot)
pub fn new_sender_job_state(
    job_id: u64,
    src_path: PathBuf,
    manifest: Manifest,
    dest_hint: Option<(String, String)>,
    progress_tx: mpsc::Sender<ProgressEvent>,
) -> SenderJobState {
    // 提前建好默认的 stop channel,让上层不必关心 None 场景。
    // 用 Arc<Mutex<>> 包装以便从任意 Arc 克隆调用
    let (probe_stop_tx, _probe_stop_rx) = oneshot::channel();
    SenderJobState {
        job_id,
        src_path,
        manifest,
        dest_hint,
        bytes_counter: Arc::new(AtomicU64::new(0)),
        active_streams: Arc::new(AtomicU32::new(0)),
        throttle_cap: Arc::new(AtomicU32::new(u32::MAX)),
        progress_tx,
        probe_stop_tx: Some(Arc::new(Mutex::new(Some(probe_stop_tx)))),
        paused: Arc::new(AtomicBool::new(false)),
        cancelled: Arc::new(AtomicBool::new(false)),
        offer_id: None,
        remote_done: Arc::new(AtomicU64::new(0)),
    }
}

/// 取消/断连清理共用:停掉 SourceProbe(可从任意 Arc 克隆调用,天然防双发)
pub fn fire_probe_stop(state: &SenderJobState) {
    if let Some(m) = &state.probe_stop_tx {
        if let Some(tx) = m.lock().unwrap().take() {
            let _ = tx.send(());
        }
    }
}

/// SenderJobState 注册表(进程级;跨实例 job_id 不冲突,见 next_source_job_id)
pub type SenderJobMap = std::sync::Arc<tokio::sync::RwLock<std::collections::HashMap<u64, Arc<SenderJobState>>>>;

pub fn new_sender_job_map() -> SenderJobMap {
    std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new()))
}

/// 30s 连接关闭兜底清理:监听 conn.closed(),等 30s 后发 SourceFailed + remove
pub fn spawn_connection_drop_cleanup(
    conn: quinn::Connection,
    state: Arc<SenderJobState>,
    jobs: SenderJobMap,
) {
    tokio::spawn(async move {
        let _ = conn.closed().await;
        tokio::time::sleep(Duration::from_secs(30)).await;

        let job_id = state.job_id;
        // M-C5: 写锁作用域只包 remove,await 发事件放在锁外——持写锁
        // await 会阻塞 FetchReq 读锁路径(锁序问题)
        let removed = {
            let mut map = jobs.write().await;
            map.remove(&job_id).is_some()
        };
        if removed {
            tracing::info!("sender job {} 连接关闭 30s 后兜底清理", job_id);
            // 统一用 fire_probe_stop,不再依赖 Arc::get_mut(在多克隆时会失败)
            fire_probe_stop(&state);
            let _ = state
                .progress_tx
                .send(ProgressEvent::SourceFailed {
                    job_id,
                    reason: "连接关闭".to_string(),
                })
                .await;
        }
    });
}
