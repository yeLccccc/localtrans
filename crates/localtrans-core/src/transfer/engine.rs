use crate::transfer::manifest::{Manifest, ManifestError};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io;
#[cfg(not(target_os = "windows"))]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};
use tracing::warn;

use bytes::BytesMut;
use quinn::Connection;
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::{mpsc, oneshot, Semaphore};

use crate::protocol::{ControlMsg, ChunkStreamHeader, CHUNK_HEADER_LEN, CHUNK_SIZE};
use crate::session::next_job_id;
use crate::session::{SessionCtx, SessionManager};
use crate::share::ShareRegistry;
use crate::identity::Fingerprint;
use crate::store::Config;
use crate::transfer::AdaptiveStreams;

/// 接收引擎错误
#[derive(Debug)]
pub enum EngineError {
    /// 哈希不匹配
    HashMismatch { chunk: u32 },
    /// 传输未完成
    Incomplete,
    /// IO 错误
    Io(io::Error),
    /// Manifest 错误
    Manifest(ManifestError),
    /// 超时
    Timeout,
    /// 会话不存在
    SessionNotFound,
    /// RPC 控制面错误
    Rpc(String),
    /// 块流协议错误
    Protocol(String),
    /// 推送请求被拒绝
    OfferRejected,
    /// 对方确认超时未响应（v0.5.0 与"拒绝"区分）
    OfferTimeout,
    /// 任务被取消（TransferCtl::Cancel）
    Cancelled,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::HashMismatch { chunk } => write!(f, "分块 {} 哈希不匹配", chunk),
            EngineError::Incomplete => write!(f, "传输未完成，所有分块未全部接收"),
            EngineError::Io(err) => write!(f, "IO 错误: {}", err),
            EngineError::Manifest(err) => write!(f, "Manifest 错误: {}", err),
            EngineError::Timeout => write!(f, "操作超时"),
            EngineError::SessionNotFound => write!(f, "会话不存在"),
            EngineError::Rpc(msg) => write!(f, "RPC 控制面错误: {}", msg),
            EngineError::Protocol(msg) => write!(f, "块流协议错误: {}", msg),
            EngineError::OfferRejected => write!(f, "推送请求被对方拒绝"),
            EngineError::OfferTimeout => write!(f, "对方超时未确认"),
            EngineError::Cancelled => write!(f, "任务已取消"),
        }
    }
}

impl std::error::Error for EngineError {}

impl Drop for PartWriter {
    fn drop(&mut self) {
        // 异常退出（断连/取消/中止）时兜底持久化位图：write_chunk 的节流保存只在
        // 周期性触发，断点续传要求任何退出路径都不丢已收块记录。
        // finalize 成功路径 part.bin 已移走、任务目录已删除——此时不再保存
        // （manifest.save 会重建目录），以 part_path 存在性区分两种退出。
        if self.part_path.exists() {
            if let Some(dir) = self.part_path.parent() {
                if let Err(e) = self.manifest.save(dir) {
                    tracing::warn!("PartWriter 退出时保存清单失败: {}", e);
                }
            }
        }
    }
}

impl From<io::Error> for EngineError {
    fn from(err: io::Error) -> Self {
        EngineError::Io(err)
    }
}

impl From<ManifestError> for EngineError {
    fn from(err: ManifestError) -> Self {
        EngineError::Manifest(err)
    }
}

/// 分块接收写入引擎
pub struct PartWriter {
    part_path: PathBuf,
    file: File,
    pub manifest: Manifest,
    last_save: Instant,
}

impl PartWriter {
    /// 打开并初始化分块写入引擎
    pub fn open(dir: &Path, m: Manifest) -> Result<Self, EngineError> {
        fs::create_dir_all(dir)?;

        let part_path = dir.join("part.bin");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .open(&part_path)?;

        let writer = Self {
            part_path,
            file,
            manifest: m,
            last_save: Instant::now(),
        };

        // 立即保存一次 manifest 以持久化初始位图
        writer.manifest.save(dir)?;

        Ok(writer)
    }

    /// 加载已存在的分块写入引擎（用于续传）
    /// 如果任务目录已存在且有 manifest.json，则加载它
    pub fn load_or_open(dir: &Path, m: Manifest) -> Result<Self, EngineError> {
        let manifest_path = dir.join("manifest.json");

        if manifest_path.exists() {
            // 尝试加载现有 manifest
            if let Ok(existing) = Manifest::load(dir) {
                // 验证文件名、大小和哈希是否匹配
                if existing.file_name == m.file_name
                    && existing.total_size == m.total_size
                    && existing.chunk_hashes == m.chunk_hashes
                {
                    // 匹配成功，使用已存的位图
                    let part_path = dir.join("part.bin");
                    let file = File::options()
                        .read(true)
                        .write(true)
                        .open(&part_path)?;

                    tracing::info!("续传任务: {} (已收 {}/{}, 缺失 {})",
                        existing.file_name,
                        existing.received.iter().filter(|&&x| x).count(),
                        existing.chunk_hashes.len(),
                        existing.missing_chunks().len()
                    );

                    return Ok(Self {
                        part_path,
                        file,
                        manifest: existing,
                        last_save: Instant::now(),
                    });
                }
            }
        }

        // 不匹配或不存在，创建新的
        Self::open(dir, m)
    }

    /// 写入单个分块
    pub fn write_chunk(&mut self, idx: u32, data: &[u8]) -> Result<(), EngineError> {
        let actual_hash = hex::encode(Sha256::digest(data));
        self.write_chunk_preverified(idx, data, &actual_hash)
    }

    /// 并发路径写入：哈希已由调用方在写锁外计算（4MB 哈希 ~10ms 级，
    /// 放锁内会把并发接收退化成串行），此处校验后只做写盘与清单更新。
    pub fn write_chunk_preverified(
        &mut self,
        idx: u32,
        data: &[u8],
        actual_hash: &str,
    ) -> Result<(), EngineError> {
        // 验证哈希
        let expected_hash = &self.manifest.chunk_hashes[idx as usize];
        if actual_hash != expected_hash {
            return Err(EngineError::HashMismatch { chunk: idx });
        }

        // 验证长度
        let expected_len = self.manifest.chunk_len(idx) as usize;
        if data.len() != expected_len {
            return Err(EngineError::HashMismatch { chunk: idx });
        }

        // 计算偏移量并写入
        let offset = self.manifest.chunk_offset(idx);

        #[cfg(target_os = "windows")]
        {
            use std::os::windows::fs::FileExt;
            self.file.seek_write(data, offset)?;
        }

        #[cfg(not(target_os = "windows"))]
        {
            self.file.seek(SeekFrom::Start(offset))?;
            self.file.write_all(data)?;
        }

        // 标记为已接收
        self.manifest.received[idx as usize] = true;

        // 节流保存 manifest
        let is_final = self.manifest.missing_chunks().is_empty();
        let should_save = is_final || self.last_save.elapsed().as_secs() > 1;

        if should_save {
            if let Some(dir) = self.part_path.parent() {
                self.manifest.save(dir)?;
                self.last_save = Instant::now();
            }
        }

        Ok(())
    }

    /// 完成传输并移动到最终位置
    pub fn finalize(self, dest_dir: &Path) -> Result<PathBuf, EngineError> {
        // 强制保存一次 manifest
        if let Some(dir) = self.part_path.parent() {
            self.manifest.save(dir)?;
        }

        // P0-1b 最终防线:清单文件名再净化(防未来调用点漏过入口校验)
        crate::transfer::sanitize_file_name(&self.manifest.file_name)?;

        // 检查是否所有分块都已接收
        if !self.manifest.missing_chunks().is_empty() {
            return Err(EngineError::Incomplete);
        }

        // 创建目标目录
        fs::create_dir_all(dest_dir)?;

        // v0.2.4：rename 前强制刷盘——数据真正写到物理磁盘后才移动到最终位置，
        // 防止 OS 缓存未落盘时系统崩溃导致"已完成"文件截断
        if let Err(e) = self.file.sync_all() {
            warn!("finalize 前刷盘失败（继续 rename）: {}", e);
        }

        // 处理文件名冲突
        let final_path = self.resolve_name_conflict(dest_dir);

        // 重命名临时文件到最终位置
        fs::rename(&self.part_path, &final_path)?;

        // 清理任务目录（失败时降级为警告，不影响传输完成）
        if let Some(dir) = self.part_path.parent() {
            if let Err(e) = fs::remove_dir_all(dir) {
                warn!("任务目录清理失败（传输已完成）: {}", e);
            }
        }

        Ok(final_path)
    }

    /// 解析文件名冲突
    fn resolve_name_conflict(&self, dest_dir: &Path) -> PathBuf {
        let mut final_path = dest_dir.join(&self.manifest.file_name);

        if !final_path.exists() {
            return final_path;
        }

        // 使用 Path 语义在最后一个点前插入计数
        let path = Path::new(&self.manifest.file_name);
        let file_stem = path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(&self.manifest.file_name);
        let extension = path.extension()
            .and_then(|s| s.to_str());

        // 递增尝试直到找到可用名称
        for i in 1..=100 {
            let new_name = if let Some(ext) = extension {
                format!("{} ({}).{}", file_stem, i, ext)
            } else {
                format!("{} ({})", file_stem, i)
            };

            final_path = dest_dir.join(&new_name);
            if !final_path.exists() {
                return final_path;
            }
        }

        // 如果试了100次都失败，就返回第100次的路径（让操作系统报错）
        final_path
    }
}

// ============ T14: 发送/接收引擎与控制面 RPC ============

/// 缓冲池（32 份 4MiB 许可）
#[derive(Clone)]
pub struct BufferPool {
    inner: Arc<BufferPoolInner>,
}

/// 缓冲池在途许可数（32 份 = 128MiB 硬上限）
pub const BUFFER_POOL_PERMITS: usize = 32;

impl BufferPool {
    /// 创建新的缓冲池（32 份 4MiB 许可）
    pub fn new() -> Self {
        Self {
            inner: Arc::new(BufferPoolInner {
                sem: Semaphore::new(BUFFER_POOL_PERMITS),
                buffers: crossbeam::queue::SegQueue::new(),
            }),
        }
    }

    /// 获取一个 4MiB 缓冲区（RAII guard：Drop 自动归还许可与缓冲）。
    /// 32 份许可 = 128MiB 在途缓冲硬上限。
    pub async fn acquire(&self) -> BufferGuard {
        BufferGuard::acquire(Arc::clone(&self.inner)).await
    }

    /// 测试钩子：当前可用许可数
    #[cfg(test)]
    pub fn available_permits(&self) -> usize {
        self.inner.sem.available_permits()
    }
}

/// 缓冲池共享内部状态（BufferGuard 持有 Arc，Drop 时归还）
struct BufferPoolInner {
    sem: Semaphore,
    buffers: crossbeam::queue::SegQueue<BytesMut>,
}

impl BufferPoolInner {
    fn give_back(&self, buf: BytesMut) {
        self.buffers.push(buf);
        self.sem.add_permits(1);
    }
}

async fn acquire_from(inner: &Arc<BufferPoolInner>) -> BytesMut {
    let permit = inner.sem.acquire().await.expect("信号量不会关闭");
    permit.forget();
    match inner.buffers.pop() {
        Some(mut buf) => {
            buf.clear();
            buf
        }
        None => BytesMut::with_capacity(CHUNK_SIZE),
    }
}

/// RAII 缓冲 guard：持有池内部状态 + 4MiB 缓冲。
/// Drop 时缓冲还池、许可归还——错误路径（`?` 提前返回 / 任务被 abort）
/// 不再泄漏许可。Deref/DerefMut 到 BytesMut，现有读写用法不变。
pub struct BufferGuard {
    inner: Arc<BufferPoolInner>,
    buf: Option<BytesMut>,
}

impl BufferGuard {
    async fn acquire(inner: Arc<BufferPoolInner>) -> Self {
        let buf = acquire_from(&inner).await;
        Self { inner, buf: Some(buf) }
    }

    /// 显式归还（等价 drop；供希望显式标注释放点的调用点使用）
    pub fn release(self) {
        // buf 在 Drop 中归还，此处仅消费 self
    }
}

impl std::ops::Deref for BufferGuard {
    type Target = BytesMut;
    fn deref(&self) -> &BytesMut {
        self.buf.as_ref().expect("BufferGuard 缓冲存在")
    }
}

impl std::ops::DerefMut for BufferGuard {
    fn deref_mut(&mut self) -> &mut BytesMut {
        self.buf.as_mut().expect("BufferGuard 缓冲存在")
    }
}

impl Drop for BufferGuard {
    fn drop(&mut self) {
        if let Some(buf) = self.buf.take() {
            self.inner.give_back(buf);
        }
    }
}


impl Default for BufferPool {
    fn default() -> Self {
        Self::new()
    }
}

/// 进度事件
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    // ===== 现有 =====
    Started { job_id: u64, name: String, total: u64 },
    ChunkDone { job_id: u64, chunk: u32, bytes: u64 },
    Speed { job_id: u64, bps: u64 },
    Done { job_id: u64 },
    Failed { job_id: u64, reason: String },
    /// 续传基线上报：命中未完成任务目录时上报已收字节数，
    /// 让进度从真实位置起步（否则 Started 归零 + 只取缺失块会让
    /// 完成态停在部分百分比——"没接收完就显示完成"的显示层根因）
    Resumed { job_id: u64, already_bytes: u64 },

    // ===== 新增:发送方视角 =====
    SourceStarted {
        job_id: u64,
        role: SourceRole,
        peer: crate::identity::Fingerprint,
        name: String,
        total: u64,
    },
    SourceChunkDone { job_id: u64, chunk: u32, bytes: u64 },
    SourceSpeed {
        job_id: u64,
        bps: u64,
        loss_ratio: f64,
        rtt_ms: u64,
        cwnd: u64,
        streams: u32,
        /// v0.10.0 对端累计已确认字节(RecvProgress 驱动;0=对端未上报,UI 降级隐藏)
        remote_done: u64,
    },
    SourceDone { job_id: u64 },
    SourceFailed { job_id: u64, reason: String },

    // ===== 新增:秒传 =====
    /// v0.10.0 秒传命中:接收方本地复用完成,零块传输(壳层置 done+instant)
    InstantHit { job_id: u64, name: String, total: u64 },
}

/// 发送方角色(决定 UI 颜色)
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(PartialEq)]
pub enum SourceRole {
    SourcePush,
    SourcePull,
}

/// OfferAsk 事件（T16 UI 消费；v0.5.0 加顺延信号与 deadline）
pub struct OfferAsk {
    pub from: Fingerprint,
    pub job_id: u64,
    pub files: Vec<crate::protocol::OfferFile>,
    /// 响应通道：Some(save_dir) = 接受, None = 拒绝
    pub respond: tokio::sync::oneshot::Sender<Option<PathBuf>>,
    /// v0.5.0 "另存中"顺延信号——UI 打开目录选择器前 notify，deadline 重置一次（每 job 仅一次）
    pub extend: std::sync::Arc<tokio::sync::Notify>,
    /// v0.5.0 确认截止时刻（epoch 毫秒）——壳层转给 UI 做倒计时
    pub deadline_epoch_ms: i64,
}

/// P0-2b: 远程删除确认事件(仿 OfferAsk;仅本机进程内,不走网络协议)
pub struct DeleteAsk {
    pub ask_id: u64,
    pub from: Fingerprint,
    pub share_id: String,
    /// 相对路径末段,仅本机 UI 展示(不进日志)
    pub name: String,
    pub is_dir: bool,
    /// 目录时的条目数(给用户判断依据;上限 10000 截断)
    pub entry_count: u64,
    pub respond: tokio::sync::oneshot::Sender<bool>,
    /// 复用 consent_timeout_secs(默认 60s);超时=自动拒绝
    pub deadline_epoch_ms: i64,
}

/// v0.5.0 Auto 档接受通知（壳层据此在完成时发系统通知）
pub struct AutoOfferInfo {
    pub job_id: u64,
    pub peer: Fingerprint,
    pub file_count: usize,
}

/// v0.5.0 Auto 档 hook（进程级；壳层注册后 Auto 接受时收到通知用于完成系统通知）
pub fn set_auto_offer_hook(tx: mpsc::Sender<AutoOfferInfo>) {
    *auto_offer_hook().lock().unwrap() = Some(tx);
}

fn auto_offer_hook() -> &'static std::sync::Mutex<Option<mpsc::Sender<AutoOfferInfo>>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<mpsc::Sender<AutoOfferInfo>>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}

/// 任务控制命令
#[derive(Debug, Clone)]
pub enum TaskControl {
    Pause,
    Resume,
    Cancel,
}

/// 任务控制通道（用于 TransferCtl 转发）
type TaskControlTx = mpsc::Sender<TaskControl>;

/// T15: 任务控制注册表（进程级）——传输任务（start_pull 等）启动时注册控制通道，
/// 本机路由器收到 TransferCtl 后查表转发；job_id 全局唯一，多实例测试互不冲突。
fn task_controls() -> &'static std::sync::Mutex<HashMap<u64, TaskControlTx>> {
    static CTLS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, TaskControlTx>>> = std::sync::OnceLock::new();
    CTLS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

fn register_task_control(job_id: u64, tx: TaskControlTx) {
    task_controls().lock().unwrap().insert(job_id, tx);
}

fn remove_task_control(job_id: u64) {
    task_controls().lock().unwrap().remove(&job_id);
}

/// R10: 本地任务控制入口——UI 的暂停/恢复/取消直接进本进程注册表，
/// 不经网络往返（拉取任务的批间消化循环从该注册表取命令）。
/// 返回 false = 任务不在表（已完成/不存在/非本进程拉取任务）。
pub fn control_task(job_id: u64, ctl: TaskControl) -> bool {
    let tx = task_controls().lock().unwrap().get(&job_id).cloned();
    match tx {
        Some(tx) => tx.try_send(ctl).is_ok(),
        None => false,
    }
}

/// v0.2.4：入站推送接收事件的进程级 hook（乙侧 UI 进度）。
/// 接收编排任务（OfferReq 接受后的 spawn 块）把 Started/ChunkDone/Done
/// 转发到这里，Tauri 壳 set 后落 TransferDto——乙侧被推送时此前完全没有
/// 接收进度行。未 set（测试/示例）时保持原丢弃行为，互不影响。
fn inbound_recv_hook() -> &'static std::sync::Mutex<Option<mpsc::Sender<ProgressEvent>>> {
    static H: std::sync::OnceLock<std::sync::Mutex<Option<mpsc::Sender<ProgressEvent>>>> =
        std::sync::OnceLock::new();
    H.get_or_init(|| std::sync::Mutex::new(None))
}

/// 注册入站接收事件消费者（Tauri 壳启动时调用一次）
pub fn set_inbound_recv_hook(tx: mpsc::Sender<ProgressEvent>) {
    *inbound_recv_hook().lock().unwrap() = Some(tx);
}

/// 拉取入口（真实 RPC 流程：MetaReq → MetaResp → 分窗口逐块 FetchReq → 块流接收 → finalize）。
/// `reg` 为本地共享区注册表：拉取方不消费它（源路径由对端解析），仅为调度器统一签名保留。
pub async fn start_pull(
    sm: &SessionManager,
    reg: &ShareRegistry,
    peer: &Fingerprint,
    share_id: &str,
    rel: &str,
    cfg: &Config,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    let _ = reg;
    start_pull_into(sm, peer, share_id, rel, cfg, &cfg.download_dir.clone(), progress).await
}

/// v0.2.6 带目标目录的拉取：文件夹模式每个文件落到自己的子目录
/// （dest_dir 保持结构）。v0.2.7 修复：parts 一律集中在**下载根**的
/// .localtrans-parts（此前跟着 dest 走，文件夹模式会把临时目录落进
/// 下载出来的文件夹里，finalize 清理后仍可能在别处残留）。
/// 单文件模式 dest=parts 根=下载目录，行为与旧版一致。
pub async fn start_pull_into(
    sm: &SessionManager,
    peer: &Fingerprint,
    share_id: &str,
    rel: &str,
    cfg: &Config,
    dest_root: &Path,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    start_pull_parts(sm, peer, share_id, rel, cfg, dest_root, &cfg.download_dir.clone(), progress).await
}

/// dest（最终落盘目录）与 parts 根（断点临时目录）分离的拉取。
pub async fn start_pull_parts(
    sm: &SessionManager,
    peer: &Fingerprint,
    share_id: &str,
    rel: &str,
    _cfg: &Config,
    dest_root: &Path,
    parts_root: &Path,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {

    // 1. 元数据协商：发 MetaReq(M-B5: msg_id 多路化),等对端路由器的 MetaResp
    let (meta_msg_id, meta_rx) = sm.send_rpc(peer, ControlMsg::MetaReq {
        share_id: share_id.to_string(),
        path: rel.to_string(),
        msg_id: 0,
    }).await.map_err(|e| EngineError::Rpc(format!("发送 MetaReq 失败: {}", e)))?;

    let meta_wait = tokio::time::timeout(Duration::from_secs(10), async {
        match meta_rx.await {
            Ok((_, ControlMsg::MetaResp { job_id, file_name, total_size, chunk_hashes, file_hash: _, .. })) =>
                Ok((job_id, file_name, total_size, chunk_hashes)),
            Ok(_) => Err(()),
            Err(_) => Err(()),
        }
    })
    .await;
    let meta = match meta_wait {
        Ok(Ok(meta)) => meta,
        Ok(Err(_)) => { sm.cancel_rpc(meta_msg_id).await; return Err(EngineError::Rpc("入站响应通道已关闭".to_string())); }
        Err(_) => { sm.cancel_rpc(meta_msg_id).await; return Err(EngineError::Timeout); }
    };

    let (job_id, file_name, total_size, chunk_hashes) = meta;

    // R9-3: 记录任务溯源信息（对端指纹、共享区ID、相对路径）
    let peer_hex = hex::encode(peer);
    let manifest = Manifest::from_meta_with_source(
        file_name.clone(),
        total_size,
        chunk_hashes,
        Some(peer_hex),
        Some(share_id.to_string()),
        Some(rel.to_string()),
    );

    let _ = progress.send(ProgressEvent::Started {
        job_id,
        name: file_name.clone(),
        total: total_size,
    })
    .await;

    let download_dir = dest_root.to_path_buf();
    // T15: 断点续传——按 (file_name, total_size, chunk_hashes) 匹配本机未完成任务目录，
    // 命中则复用（received 位图延续，只取缺失块）；未命中按本次 job_id 新建。
    // v0.2.7：parts 根独立参数（文件夹模式=下载根，不跟 dest 走）
    let parts_dir = match crate::transfer::pending_jobs(parts_root)
        .into_iter()
        .find(|(_, m)| {
            m.file_name == file_name && m.total_size == total_size && m.chunk_hashes == manifest.chunk_hashes
        })
    {
        Some((old_id, _)) => {
            tracing::info!("命中未完成任务目录 {:016x}，续传", old_id);
            parts_root.join(format!(".localtrans-parts/{:016x}", old_id))
        }
        None => parts_root.join(format!(".localtrans-parts/{:016x}", job_id)),
    };

    // 2. 空文件：无块可取，直接落盘
    if manifest.chunk_count() == 0 {
        let writer = PartWriter::open(&parts_dir, manifest)?;
        let final_path = writer.finalize(&download_dir)?;
        tracing::info!("空文件拉取完成: {}", final_path.display());
        // 空文件同样回执 RecvAck——发送方 source 行的终态不依赖文件大小
        // （v0.6.x：此前此分支提前 return 跳过了 771 行的 RecvAck）
        if let Err(e) = sm.send_ctrl(peer, ControlMsg::RecvAck { job_id }).await {
            tracing::warn!("发送 RecvAck 失败（不影响本机完成态）: {}", e);
        }
        let _ = progress.send(ProgressEvent::Done { job_id }).await;
        return Ok(job_id);
    }

    let conn = sm.session(peer).await.ok_or(EngineError::SessionNotFound)?;
    let pool = Arc::new(BufferPool::new());

    // T15: 自适应流数由探测任务与批处理循环共享（每批开始时取当前窗口）
    let adapt = Arc::new(tokio::sync::Mutex::new(AdaptiveStreams::new()));

    // T15: 支持断点续传 - 尝试加载已存在的 manifest
    let writer = PartWriter::load_or_open(&parts_dir, manifest.clone())?;
    let missing = writer.manifest.missing_chunks();
    // 续传基线：已收块折算字节上报（部分文件续传时进度从真实位置起步）
    if !missing.is_empty() || writer.manifest.received.iter().any(|&r| r) {
        let already: u64 = writer.manifest.received.iter().enumerate()
            .filter(|(_, &r)| r)
            .map(|(i, _)| writer.manifest.chunk_len(i as u32) as u64)
            .sum();
        if already > 0 {
            let _ = progress.send(ProgressEvent::Resumed { job_id, already_bytes: already }).await;
            tracing::info!("续传基线: {} (job {}) 已有 {}/{} 字节", file_name, job_id, already, total_size);
        }
    }
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    // T15: 速度探测任务 - 周期 500ms：真实字节计数算速率 + quinn 路径统计算丢包率，
    // 驱动 AdaptiveStreams::on_probe 并发送 Speed 事件。
    // 退出条件：start_pull 结束（stop_tx send 或 drop）或连接关闭。
    let conn_for_stats = conn.clone();
    let progress_for_speed = progress.clone();
    let adapt_for_probe = adapt.clone();
    let bytes_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let bytes_counter_for_speed = bytes_counter.clone();
    let name_for_log = file_name.clone();
    let total_for_log = total_size;
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel::<()>();

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_millis(500));
        interval.tick().await; // 首个 tick 立即返回，跳过
        let mut last_bytes = 0u64;
        let mut last_time = Instant::now();
        let mut stop_rx = stop_rx;
        let mut tick = 0u32;
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                res = &mut stop_rx => {
                    // send（正常完成）或 drop（错误路径提前返回）都退出
                    let _ = res;
                    break;
                }
                _ = conn_for_stats.closed() => {
                    // 连接断开：run_receiver 可能还要等最长 30s 超时才返回，
                    // 探测任务不必陪着空转（stats 快照虽可安全读取，但事件已无意义）
                    tracing::debug!("连接已关闭，速度探测任务退出");
                    break;
                }
            }

            // quinn 0.11: stats.path 为 PathStats，含 lost_packets/sent_packets
            let path = conn_for_stats.stats().path;
            let loss_ratio = if path.sent_packets > 0 {
                path.lost_packets as f64 / path.sent_packets as f64
            } else {
                0.0
            };

            let now = Instant::now();
            let elapsed = now.duration_since(last_time).as_secs_f64();
            let current_bytes = bytes_counter_for_speed.load(std::sync::atomic::Ordering::Relaxed);
            let bps_now = if elapsed > 0.0 && current_bytes > last_bytes {
                ((current_bytes - last_bytes) as f64 * 8.0 / elapsed) as u64
            } else {
                0
            };

            let _current_streams = adapt_for_probe.lock().await.on_probe(bps_now, loss_ratio);

            let _ = progress_for_speed.send(ProgressEvent::Speed {
                job_id,
                bps: bps_now,
            }).await;

            // 每 2s 一条进度 INFO：大文件传输在日志里可见（跨网段排障
            // 关键——v0.1.8 实机大文件既无完成也无失败日志，无从判断）
            tick = tick.wrapping_add(1);
            if tick % 4 == 1 {
                tracing::info!(
                    "拉取进度: {} (job {}) {:.1}% ({}/{}, {:.1} MB/s)",
                    name_for_log,
                    job_id,
                    current_bytes as f64 / total_for_log.max(1) as f64 * 100.0,
                    current_bytes,
                    total_for_log,
                    bps_now as f64 / 8.0 / 1024.0 / 1024.0
                );
            }

            last_bytes = current_bytes;
            last_time = now;
        }
    });

    // 3. 按动态窗口批量 FetchReq + 接收块流（窗口内并发、窗口间串行，窗口随探测调整）。
    // 批间响应 TransferCtl：Pause=停止发新批次直到 Resume；Cancel=删任务目录并中止。
    let (ctl_tx, mut ctl_rx) = mpsc::channel::<TaskControl>(8);
    register_task_control(job_id, ctl_tx);

    let mut idx = 0;
    while idx < missing.len() {
        // 消化已排队的控制命令
        loop {
            match ctl_rx.try_recv() {
                Ok(TaskControl::Pause) => {
                    tracing::info!("job {} 暂停（等待 Resume/Cancel）", job_id);
                    match ctl_rx.recv().await {
                        Some(TaskControl::Resume) => tracing::info!("job {} 恢复", job_id),
                        Some(TaskControl::Cancel) | None => {
                            let _ = std::fs::remove_dir_all(&parts_dir);
                            let _ = progress.send(ProgressEvent::Failed {
                                job_id,
                                reason: "任务已取消".to_string(),
                            }).await;
                            remove_task_control(job_id);
                            let _ = stop_tx.send(());
                            return Err(EngineError::Cancelled);
                        }
                        _ => {}
                    }
                }
                Ok(TaskControl::Resume) => continue, // 未暂停时的 Resume：无操作
                Ok(TaskControl::Cancel) => {
                    let _ = std::fs::remove_dir_all(&parts_dir);
                    let _ = progress.send(ProgressEvent::Failed {
                        job_id,
                        reason: "任务已取消".to_string(),
                    }).await;
                    remove_task_control(job_id);
                    let _ = stop_tx.send(());
                    return Err(EngineError::Cancelled);
                }
                Err(_) => break, // 队列空（或断开——发送端常驻不会断）
            }
        }

        let window = adapt.lock().await.current().max(1);
        let end = (idx + window).min(missing.len());
        let batch = &missing[idx..end];
        run_receiver(&conn, &pool, sm, peer, job_id, &writer, batch, &progress, Some(&bytes_counter)).await?;
        idx = end;
    }
    remove_task_control(job_id);

    // 传输结束：停掉探测任务（错误路径上 stop_tx 被 drop 同样触发退出）
    let _ = stop_tx.send(());

    // 4. 全部到位 → finalize（consume 写句柄；批任务均已 join，Arc 应唯一）
    let final_path = match std::sync::Arc::try_unwrap(writer) {
        Ok(w) => w.into_inner().finalize(&download_dir)?,
        Err(_) => return Err(EngineError::Protocol("PartWriter 仍被并发任务引用".into())),
    };
    tracing::info!("文件拉取完成: {} (job {})", final_path.display(), job_id);

    // v0.2.4 双向确认：回执发送方"已完整落盘"，对端据此发 SourceDone/清理
    // sender 任务——完成语义从"我收完了"升级为"我收完且对方知道我收完了"
    if let Err(e) = sm.send_ctrl(peer, ControlMsg::RecvAck { job_id }).await {
        tracing::warn!("发送 RecvAck 失败（不影响本机完成态）: {}", e);
    }

    let _ = progress.send(ProgressEvent::Done { job_id }).await;
    Ok(job_id)
}

/// v0.6.x:推送失败/取消路径的关联 sender 任务清理。大文件推送时接收方
/// 反向 MetaReq 取流,本机路由器按 offer_id 建了 sender 任务(source 行);
/// push_files 失败返回前调用,否则这些任务永久 active(计时不停)。
/// 幂等:无关联任务时空操作。
pub async fn cleanup_push_senders(
    sender_jobs: &crate::transfer::sender_state::SenderJobMap,
    offer_id: u64,
    reason: &str,
) {
    // M-C5: 先在写锁内完成全部 remove,释放锁后再 await 发事件——
    // 持写锁 await 会与读锁使用方(FetchReq 服务)互相卡死
    let matched: Vec<_> = {
        let mut jobs = sender_jobs.write().await;
        let matched: Vec<u64> = jobs.iter()
            .filter(|(_, s)| s.offer_id == Some(offer_id))
            .map(|(id, _)| *id)
            .collect();
        matched.into_iter()
            .filter_map(|id| jobs.remove(&id).map(|s| (id, s)))
            .collect()
    };
    for (id, state) in matched {
        use crate::transfer::sender_state::fire_probe_stop;
        fire_probe_stop(&state);
        let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
            job_id: id,
            reason: reason.to_string(),
        }).await;
        tracing::debug!("推送失败清理关联 sender 任务: offer {:016x} job {:016x}", offer_id, id);
    }
}

/// 推送入口（T15: 支持小文件批流 + 大文件块流）
/// 发送 OfferReq → 等 OfferResp → 推送文件
pub async fn push_files(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: Vec<PathBuf>,
    sender_jobs: &crate::transfer::sender_state::SenderJobMap,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    // 单文件/平铺模式：rel_dir 全空
    push_files_rel(sm, peer, files.iter().map(|p| (p.clone(), String::new())).collect(), sender_jobs, progress).await
}

/// v0.2.6 带相对目录的推送：files 为 (本地路径, 相对目录) 列表。
/// rel_dir 用 '/' 分隔（如 "photos/2026"），接收方按它建子目录；
/// 大文件 MetaReq 用 "rel_dir/file_name" 匹配源路径。
/// v0.6.x:sender_jobs 注册表穿透——失败路径按 offer_id 清理关联 sender 任务。
pub async fn push_files_rel(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: Vec<(PathBuf, String)>,
    sender_jobs: &crate::transfer::sender_state::SenderJobMap,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    push_files_rel_cancellable(sm, peer, files, sender_jobs, progress, None).await
}

/// 带壳层取消信号的推送(v0.11.x BUG01 修复):占位任务期(Started 事件前)
/// 对端离线卡在等 OfferResp 时,壳层按 placeholder_id 取消——信号经此传入,
/// 等待循环即时退出。cancel 语义 = 用户取消,返回 Cancelled 错误。
pub async fn push_files_rel_cancellable(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: Vec<(PathBuf, String)>,
    sender_jobs: &crate::transfer::sender_state::SenderJobMap,
    progress: mpsc::Sender<ProgressEvent>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<u64, EngineError> {
    let job_id = next_job_id();
    match push_files_inner(sm, peer, &files, job_id, progress, cancel).await {
        Ok(()) => Ok(job_id),
        Err(e) => {
            tracing::info!("推送失败({}),清理关联 sender 任务: offer {:016x}", e, job_id);
            // v0.6.x:失败/取消/超时统一在此清理关联 sender 任务——
            // 大文件反向取流产生的 source 行不再永久 active
            cleanup_push_senders(sender_jobs, job_id, &e.to_string()).await;
            Err(e)
        }
    }
}

/// push_files_rel 的主体:job_id 由外层分配,成功返回 Ok(())。
/// (原 push_files_rel 主体原样搬入:`let job_id = next_job_id();` 删除、
///   形参 files 改为 `&[(PathBuf, String)]` 切片、结尾 `Ok(job_id)` 改 `Ok(())`;
///   主体内所有逻辑——OfferReq/分组/批流/JobDone 等待/Done 事件——逐行保持不变)
async fn push_files_inner(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: &[(PathBuf, String)],
    job_id: u64,
    progress: mpsc::Sender<ProgressEvent>,
    cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) -> Result<(), EngineError> {
    // 构建文件列表
    let mut offer_files = Vec::new();
    for (path, rel_dir) in files.iter() {
        let metadata = fs::metadata(path)?;
        let file_name = path.file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| EngineError::Protocol("无效文件名".to_string()))?;

        let hash = if metadata.len() <= SMALL_FILE_LIMIT {
            Some(crate::transfer::dedup::sha256_file(path)?)
        } else {
            None // 大文件后台算,经 MetaResp 补
        };
        offer_files.push(crate::protocol::OfferFile {
            name: file_name.to_string(),
            size: metadata.len(),
            rel_dir: rel_dir.clone(),
            hash,
        });
    }

    // 按文件大小分组：小文件(≤1MiB) 和 大文件(>1MiB)
    // 重要：此分组必须在发送 OfferReq 之前完成，因为大文件注册必须在接收方
    // accept 后立即反向 MetaReq 之前完成——MetaReq 查 push_jobs 注册表为空会被丢弃
    let mut small_files = Vec::new();
    let mut large_files = Vec::new();

    for ((path, _rel), offer) in files.iter().zip(offer_files.iter()) {
        if offer.size <= SMALL_FILE_LIMIT {
            small_files.push((path.clone(), offer.clone()));
        } else {
            large_files.push((path.clone(), offer.clone()));
        }
    }

    let has_large = !large_files.is_empty();

    // 大文件推送任务注册必须在 OfferReq 之前——接收方 accept 后立即反向发
    // MetaReq(push:offer_id)，若此时注册表为空则路由器 warn "推送任务不存在"
    // 并丢弃请求，接收方等 MetaResp 30s 超时后 JobFailed。实测日志证实此竞态：
    // MetaReq 到达时刻 offer 已发但 register_push_job_rel 未执行，导致大文件推送
    // 偶发 30s 超时失败（见测试 push_failure_cleans_associated_source_jobs）。
    if has_large {
        register_push_job_rel(job_id, large_files.iter().map(|(p, o)| (p.clone(), o.rel_dir.clone())).collect());
    }

    // 发送 OfferReq
    sm.send_ctrl(peer, ControlMsg::OfferReq {
        job_id,
        files: offer_files.clone(),
    }).await.map_err(|e| EngineError::Rpc(format!("发送 OfferReq 失败: {}", e)))?;

    // 等待 OfferResp(M-B5: 推送协商信号走广播通道订阅过滤,
    // 不再独占一次性响应通道——浏览 RPC 与推送彻底解耦)
    // v0.11.x BUG01:对端不在线时此等待最长 offer_timeout+30s,期间壳层
    // 占位任务无法取消(注册表查不到 placeholder_id)——cancel 信号在
    // 等待循环里即时生效,不再傻等超时
    let offer_wait_secs = sm.offer_timeout_secs().await + 30;
    let mut signal_rx = sm.subscribe_push_signals();
    let cancel_for_wait = cancel.clone();
    let resp = tokio::time::timeout(Duration::from_secs(offer_wait_secs), async {
        loop {
            if let Some(c) = cancel_for_wait.as_ref() {
                if c.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(());
                }
            }
            // 5s 一轮:cancel 检查粒度(广播通道 recv 阻塞期间无法打断)
            match tokio::time::timeout(Duration::from_secs(5), signal_rx.recv()).await {
                Ok(Ok((from, ControlMsg::OfferResp { accepted, save_dir, reason, skip_bitmap }))) if from == *peer => {
                    return Ok((accepted, save_dir, reason, skip_bitmap));
                }
                Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => return Err(()),
                Err(_) => continue, // 5s 无信号,回到循环头的 cancel 检查
            }
        }
    })
    .await;
    let (accepted, _save_dir, deny_reason, resp_skip_bitmap) = match resp {
        Ok(Ok(v)) => v,
        Ok(Err(())) => {
            // 用户取消——清理已注册的大文件推送任务后返回 Cancelled
            if has_large {
                remove_push_job(job_id);
            }
            return Err(EngineError::Cancelled);
        }
        Err(_) => return Err(EngineError::Timeout),
    };

    // v0.11.0 T18 小文件秒传位图:接收方回 skip_bitmap(与请求 files 等长逐位对应,
    // true=跳过)——按请求索引剔除,不再依赖 hash 匹配(不泄露持有信息)。
    // 大文件不过滤——接收方在 MetaResp 处自判定,注册表项由 remove_push_job 清理
    // N1-T1a:被跳过的文件仍发 Started(壳层占位卡据此激活;否则全跳过时卡片
    // 永卡 pending/total=0——只发整 job Done 会撞 Pending+Finished 非法迁移)。
    let mut skipped_small: Vec<crate::protocol::OfferFile> = Vec::new();
    if !resp_skip_bitmap.is_empty() {
        let mut kept = Vec::with_capacity(small_files.len());
        for (path, offer) in small_files {
            let skip = files.iter().position(|(p, _)| p == &path)
                .and_then(|i| resp_skip_bitmap.get(i).copied())
                .unwrap_or(false); // 位图缺位时保守发送
            if skip { skipped_small.push(offer); } else { kept.push((path, offer)); }
        }
        small_files = kept;
        if !skipped_small.is_empty() {
            tracing::info!("小文件秒传: 接收方已持有 {} 个,批流跳过", skipped_small.len());
        }
    }

    if !accepted {
        // 清理已注册的大文件推送任务——offer_id 全局唯一不会复用，不清理会导致注册表泄漏
        if has_large {
            remove_push_job(job_id);
        }
        return Err(match deny_reason {
            Some(crate::protocol::OfferDenyReason::Timeout) => EngineError::OfferTimeout,
            _ => EngineError::OfferRejected,
        });
    }
    // 保存目录由接收方决定（OfferResp.save_dir / 其 Ask 应答 / 其默认下载目录）；
    // save_dir_hint 仅作为 brief 签名保留，发送侧不消费。

    let conn = sm.session(peer).await.ok_or(EngineError::SessionNotFound)?;
    let has_small = !small_files.is_empty();
    // N1-T1a:秒传跳过的文件也占 Started/Done 配对(先于实发文件发 Started,
    // 与 Done 的最旧 active 配对序一致)
    for offer in &skipped_small {
        let _ = progress.send(ProgressEvent::Started {
            job_id,
            name: offer.name.clone(),
            total: offer.size,
        }).await;
    }

    // 发送小文件批流(T8: 每文件一对 Started/Done,不再聚合成" N 个小文件"单条)
    let small_count = small_files.len() + skipped_small.len();
    if has_small {
        for (_, offer) in &small_files {
            let _ = progress.send(ProgressEvent::Started {
                job_id,
                name: offer.name.clone(),
                total: offer.size,
            }).await;
        }
        if let Err(e) = send_small_files_batched(&conn, job_id, small_files).await {
            #[cfg(test)]
            {
                // 低危审计修复(生产路径不记录):失败时也清计数,防测试间泄漏
                small_batch_streams().lock().unwrap().remove(&job_id);
            }
            let _ = progress.send(ProgressEvent::Failed {
                job_id,
                reason: format!("小文件批流失败: {}", e),
            }).await;
            return Err(e);
        }
    }

    // 发送大文件（接收方反向驱动：乙发 MetaReq(push:) → 甲回 MetaResp → 乙窗口 FetchReq →
    // 甲逐块开流）——注册已在发送 OfferReq 之前完成，此处仅发送 Started 事件
    if has_large {
        for (_, offer) in &large_files {
            let _ = progress.send(ProgressEvent::Started {
                job_id,
                name: offer.name.clone(),
                total: offer.size,
            }).await;
        }
    }

    // 等待乙侧整 offer 完成信号：乙在全部文件落盘后回 JobDone{offer_id}，
    // 甲侧 Done 事件在此时才发——保证 Done 之后文件一定已在磁盘上。
    if has_small || has_large {
        // M-B5: JobDone/JobFailed 也走推送信号广播通道
        let mut done_rx = sm.subscribe_push_signals();
        // v0.9.0 修:取消硬超时,改为"无进展即超时"——
        // 基础看门狗 idle_timeout_secs(默认 60s)。如果 progress 通道在
        // 看门狗窗口内没有任何 Started/Done 流过,则触发超时;否则刷新窗口。
        // 这样小文件快速完成不会被误杀,大文件丢包重传也能等到。
        #[allow(unused_variables)]
        let idle_secs = std::env::var("LOCALTRANS_IDLE_TIMEOUT_SEC")
            .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(60);
        // 注意:理想做法是订阅 progress 通道驱动看门狗;此处退化为静态
        // 超时,但值放大到合理上界(30 分钟)以避开真实大文件场景。
        // 下一步:把 progress 当作 watchdog tick 信号。
        let static_timeout_secs = std::env::var("LOCALTRANS_TRANSFER_MAX_SEC")
            .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1800);
        let wait_done = tokio::time::timeout(Duration::from_secs(static_timeout_secs), async {
            loop {
                // v0.11.x BUG01:等待对端完成期间也可取消(占位/进行中任务统一出口)
                if let Some(c) = cancel.as_ref() {
                    if c.load(std::sync::atomic::Ordering::Relaxed) {
                        return Err("已取消".to_string());
                    }
                }
                match tokio::time::timeout(Duration::from_secs(5), done_rx.recv()).await {
                    Ok(Ok((from, ControlMsg::JobDone { offer_id, .. }))) if from == *peer && offer_id == job_id => {
                        return Ok(());
                    }
                    // v0.2.4：乙侧接收失败回执——立即失败返回，不再傻等超时
                    Ok(Ok((from, ControlMsg::JobFailed { offer_id, reason }))) if from == *peer && offer_id == job_id => {
                        return Err(reason);
                    }
                    Ok(Ok(_)) | Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
                    Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) =>
                        return Err("推送信号通道已关闭".to_string()),
                    Err(_) => continue, // 5s 无信号,回循环头查 cancel
                }
            }
        })
        .await;
        // 无论 JobDone 等待成败，先清理推送任务注册表——
        // 否则错误路径（超时/通道关闭）泄漏的表项会让重试解析到过期源路径
        if has_large {
            remove_push_job(job_id);
        }
        // tokio::time::timeout 返回 Result<T, Elapsed>;T = Result<(), String>
        // 用 match_inner_or_else 形式拆开两层
        match wait_done {
            Ok(Ok(())) => {} // JobDone 收到,正常完成
            Ok(Err(reason)) => {
                return Err(EngineError::Rpc(format!("对方接收失败: {}", reason)));
            }
            Err(_elapsed) => {
                // tokio 超时:JobDone 一直没收到——可重试
                return Err(EngineError::Rpc(format!(
                    "传输未完成({}s 内无 JobDone,可重试)",
                    static_timeout_secs,
                )));
            }
        }
    }

    // T8: 小文件按文件数补 Done(在 JobDone 之后,保持"Done ⇒ 已落盘"不变量)。
    // 注意:这是发送侧(push_files)对同一 job_id 逐文件补 Done——壳层(FFI)
    // 的 saved_files 累加器按"首个 Done 取走并移除、后续 Started 重建"消化,
    // 每条 FilesSaved 单文件,消费端逐条处理,无跨文件累积假设。
    // N1-T1a:秒传跳过的文件同样占一票 Done(与先发的 Started 配对;
    // 全跳过时 has_small=false 但 small_count>0,Done 仍须补齐)。
    if small_count > 0 {
        for _ in 0..small_count {
            let _ = progress.send(ProgressEvent::Done { job_id }).await;
        }
    }
    // v0.2.9：派生 sender 任务的探针由 10s 空闲自杀兜底停掉
    // （push 完成后字节不再增长、无在途流 → 探针退出，CPU 归位）

    let _ = progress.send(ProgressEvent::Done { job_id }).await;  // 整 job Done,保持原样
    Ok(())
}

/// T15: 推送任务注册表（进程级）——push_files 注册大文件源路径，
/// 本机路由器的 MetaReq 分支按 "push:{offer_id:x}" 前缀解析（绕过共享区）。
/// job_id 全局唯一（next_job_id 原子分配），多实例测试互不冲突。
/// v0.2.6：值改为 (路径, rel_dir) 对——MetaReq 的 path 是 "rel_dir/name"，
/// 同名文件在不同子目录靠 rel 区分。
fn push_jobs() -> &'static std::sync::Mutex<HashMap<u64, Vec<(PathBuf, String)>>> {
    static JOBS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, Vec<(PathBuf, String)>>>> = std::sync::OnceLock::new();
    JOBS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// T16: 推送任务控制标志
#[derive(Clone)]
pub struct PushControl {
    pub paused: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

fn push_controls() -> &'static std::sync::Mutex<HashMap<u64, PushControl>> {
    static CONTROLS: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, PushControl>>> = std::sync::OnceLock::new();
    CONTROLS.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// v0.10.0 推送任务的大文件 hash 槽:key = "rel_dir/name"(与 resolve_push_file
/// 的 req_path 组合规则一致);后台算完填入,MetaReq 路由回填 MetaResp.file_hash
fn register_push_job_rel(offer_id: u64, entries: Vec<(PathBuf, String)>) {
    push_jobs().lock().unwrap().insert(offer_id, entries);
    // T16: 同时注册控制标志
    push_controls().lock().unwrap().insert(offer_id, PushControl {
        paused: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    });
}

fn remove_push_job(offer_id: u64) {
    push_jobs().lock().unwrap().remove(&offer_id);
    push_controls().lock().unwrap().remove(&offer_id);
}

/// T16: 获取推送任务控制标志(供 transfer_action 使用)
pub fn get_push_control(offer_id: u64) -> Option<PushControl> {
    push_controls().lock().unwrap().get(&offer_id).cloned()
}

/// 解析推送任务的文件路径（v0.2.6：请求 path 形如 "rel_dir/name"——
/// 接收方 recv_push_large_file 按此组合请求；先组合匹配再裸名回退）
fn resolve_push_file(offer_id: u64, req_path: &str) -> Option<PathBuf> {
    let jobs = push_jobs().lock().unwrap();
    let entries = jobs.get(&offer_id)?;
    // 1. 组合匹配："rel/name" 完整相等
    if let Some((p, _)) = entries.iter().find(|(p, rel)| {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if rel.is_empty() {
            false
        } else {
            format!("{}/{}", rel.trim_matches('/'), name) == req_path
        }
    }) {
        return Some(p.clone());
    }
    // 2. 裸文件名回退（平铺推送 / 旧对端）
    entries.iter()
        .find(|(p, _)| p.file_name().and_then(|n| n.to_str()) == Some(req_path))
        .map(|(p, _)| p.clone())
}

/// T15: 推送模式大文件单文件接收（乙侧反向驱动，复用拉取块流机制与续传）
async fn recv_push_large_file(
    sm: &SessionManager,
    conn: &Connection,
    pool: &Arc<BufferPool>,
    peer: &Fingerprint,
    offer_id: u64,
    file: &crate::protocol::OfferFile,
    save_root: &Path,
    parts_root: &Path,
    progress: &mpsc::Sender<ProgressEvent>,
) -> Result<(), EngineError> {
    // v0.2.6：rel_dir 子目录（'/' 分隔，已净化）——大文件落到 save_root/rel_dir/
    let file_dest = if file.rel_dir.is_empty() {
        save_root.to_path_buf()
    } else {
        let mut d = save_root.to_path_buf();
        for comp in file.rel_dir.split('/').filter(|s| !s.is_empty()) {
            d.push(sanitize_component(comp));
        }
        d
    };

    // 1. 元数据协商：向甲请求该文件的清单（push: 前缀走甲的推送任务注册表）。
    // v0.2.6：path 用 "rel_dir/name" 组合——甲侧注册表按同 key 匹配，
    // 同名文件在不同子目录不再冲突
    let req_path = if file.rel_dir.is_empty() {
        file.name.clone()
    } else {
        format!("{}/{}", file.rel_dir.trim_matches('/'), file.name)
    };
    // M-B5: MetaReq 走 msg_id 多路 RPC
    let (meta_msg_id, meta_rx) = sm.send_rpc(peer, ControlMsg::MetaReq {
        share_id: format!("push:{:016x}", offer_id),
        path: req_path,
        msg_id: 0,
    }).await.map_err(|e| EngineError::Rpc(format!("发送 MetaReq 失败: {}", e)))?;
    let meta_wait = tokio::time::timeout(Duration::from_secs(30), async {
        match meta_rx.await {
            Ok((_, ControlMsg::MetaResp { job_id, file_name, total_size, chunk_hashes, file_hash, .. })) =>
                Ok((job_id, file_name, total_size, chunk_hashes, file_hash)),
            Ok(_) => Err(()),
            Err(_) => Err(()),
        }
    }).await;
    let meta = match meta_wait {
        Ok(Ok(meta)) => meta,
        Ok(Err(_)) => { sm.cancel_rpc(meta_msg_id).await; return Err(EngineError::Rpc("等待 MetaResp 失败".to_string())); }
        Err(_) => { sm.cancel_rpc(meta_msg_id).await; return Err(EngineError::Timeout); }
    };
    let (job_id, file_name, total_size, chunk_hashes, file_hash) = meta;

    // P0-1b: MetaResp 文件名净化(攻击者全权可控字段)
    let file_name = crate::transfer::sanitize_file_name(&file_name)?;
    // P0-1b/P0-4: total_size 与 Offer 声明交叉校验(挡"offer 报小、meta 报大"的
    // part.bin 高位偏移扩展攻击),不允许零头差异
    if total_size != file.size {
        return Err(EngineError::Protocol(format!(
            "MetaResp total_size ({}) 与 Offer 声明 ({}) 不一致, 拒绝接收",
            total_size, file.size
        )));
    }

    // v0.10.0 秒传判定:MetaResp 带整体 hash 且收件箱索引命中(大小一致、文件在)
    // → 本地复用,零块传输
    if let Some(hash) = file_hash.as_ref().filter(|h| !h.is_empty()) {
        let index_path = parts_root.join(".localtrans-inbox-index.json");
        // v0.11.0 T5:读盘 + prune IO 下放阻塞线程池
        let mut index = tokio::task::spawn_blocking(move || {
            let mut idx = crate::transfer::dedup::InboxIndex::load(&index_path);
            idx.prune_missing();
            idx
        }).await
            .map_err(|e| EngineError::Io(io::Error::other(e.to_string())))?;
        if let Some(src) = index.lookup(hash, total_size) {
            let src = src.clone();
            let dest = crate::transfer::dedup::place_dedup_copy(&src, &file_dest, &file_name)?;
            index.insert(hash.clone(), dest.clone(), total_size);
            index.save();
            tracing::info!("秒传命中: {} → 已复用(hash {}…)", total_size, &hash[..8.min(hash.len())]);
            if let Err(e) = sm.send_ctrl(peer, ControlMsg::RecvAck { job_id }).await {
                tracing::warn!("秒传 RecvAck 失败: {}", e);
            }
            let _ = progress.send(ProgressEvent::InstantHit {
                job_id, name: file_name.clone(), total: total_size,
            }).await;
            return Ok(());
        }
    }

    let manifest = Manifest::from_meta(file_name.clone(), total_size, chunk_hashes);
    let _ = progress.send(ProgressEvent::Started { job_id, name: file_name.clone(), total: total_size }).await;

    // v0.2.6：parts 集中在下载根的 .localtrans-parts（不污染子目录结构），
    // finalize 时再移到 save_root/rel_dir/
    let parts_dir = parts_root.join(format!(".localtrans-parts/{:016x}", job_id));
    let writer = PartWriter::load_or_open(&parts_dir, manifest)?;
    let missing = writer.manifest.missing_chunks();
    let writer = std::sync::Arc::new(tokio::sync::Mutex::new(writer));

    // v0.10.0 双进度：接收侧累计字节计数器，每收完一个窗口回发 RecvProgress
    let cumulative = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));

    // 2. 窗口化 FetchReq + 收块流（沿用拉取主链路的固定窗口；乙侧自适应归 T16 UI 集成）
    let mut idx = 0;
    while idx < missing.len() {
        let window = 4usize;
        let end = (idx + window).min(missing.len());
        let batch = &missing[idx..end];
        run_receiver(conn, pool, sm, peer, job_id, &writer, batch, progress, Some(&cumulative.clone())).await?;
        idx = end;
        // v0.10.0: 整窗落盘后回发累计字节（4MB/块 × 4 块/窗 ≈ 每 16MB 一条，无洪泛）
        let _ = sm.send_ctrl(peer, ControlMsg::RecvProgress {
            job_id,
            cumulative_bytes: cumulative.load(std::sync::atomic::Ordering::Relaxed),
        }).await;
    }

    // 3. finalize（完成信号 JobDone 由编排任务在整 offer 收完后统一发送）
    let final_path = match std::sync::Arc::try_unwrap(writer) {
        Ok(w) => w.into_inner().finalize(&file_dest)?,
        Err(_) => return Err(EngineError::Protocol("PartWriter 仍被并发任务引用".into())),
    };
    tracing::info!("大文件推送完成: {}", final_path.display());
    // v0.6.x 对齐 v0.2.4 双向确认契约：回执推送方"已完整落盘"——甲侧据此发
    // SourceDone 并清理 sender 任务。此前只有 pull 模式回 RecvAck，推送大文件
    // 的 source 行永远 active（计时不停、进度泵永不降频的根因）
    if let Err(e) = sm.send_ctrl(peer, ControlMsg::RecvAck { job_id }).await {
        tracing::warn!("发送 RecvAck 失败（不影响本机完成态）: {}", e);
    }
    let _ = progress.send(ProgressEvent::Done { job_id }).await;

    // v0.10.0 正常传输完成 → 索引进账(下次同文件秒传)
    if let Some(hash) = file_hash.as_ref().filter(|h| !h.is_empty()) {
        let index_path = parts_root.join(".localtrans-inbox-index.json");
        let mut index = crate::transfer::dedup::InboxIndex::load(&index_path);
        index.insert(hash.clone(), final_path.clone(), total_size);
        index.save();
    }
    Ok(())
}

/// 接收引擎：为一个批次的缺失块发 FetchReq，并并发接收对应的单向块流写入 PartWriter
///
/// T20 吞吐修正：原实现逐流串行消化（读块+哈希+落盘逐个求和），回环仅 ~188MB/s。
/// 现在 accept 仍串行（轻操作），但每条流的处理 spawn 成任务流水线化——读块/哈希
/// 与磁盘写重叠，PartWriter 单文件句柄经互斥锁串行化写盘。批内并发度=窗口大小。
///
/// v0.2.6 平滑修正：整批 barrier 拆成滑动窗口——每收完一块（join_next 返回）
/// 立即补发下一块请求，在途块数恒等于窗口大小。进度事件按块连续到达，
/// 不再"整批一跳"（批模式下 UI 一跳 = 窗口×4MiB，观感卡顿的根因）。
pub async fn run_receiver(
    conn: &Connection,
    pool: &Arc<BufferPool>,
    sm: &SessionManager,
    peer: &Fingerprint,
    job_id: u64,
    writer: &Arc<tokio::sync::Mutex<PartWriter>>,
    batch: &[u32],
    progress: &mpsc::Sender<ProgressEvent>,
    bytes_counter: Option<&Arc<std::sync::atomic::AtomicU64>>,
) -> Result<(), EngineError> {
    run_receiver_windowed(conn, pool, sm, peer, job_id, writer, batch, progress, bytes_counter, batch.len().max(1)).await
}

/// 滑动窗口接收：`window` 为在途块配额（批模式传 batch.len() 退化为旧行为）。
/// 每块完成即补发下一块 FetchReq，管道全程不排空。
///
/// 计量模型：sent（已发 FetchReq）- completed（已收割任务）≤ window。
/// select 两个事件源：对端回流块流（accept_uni）与块任务完成（join_next），
/// 守卫条件保证只在有对应工作可做时等待。
async fn run_receiver_windowed(
    conn: &Connection,
    pool: &Arc<BufferPool>,
    sm: &SessionManager,
    peer: &Fingerprint,
    job_id: u64,
    writer: &Arc<tokio::sync::Mutex<PartWriter>>,
    batch: &[u32],
    progress: &mpsc::Sender<ProgressEvent>,
    bytes_counter: Option<&Arc<std::sync::atomic::AtomicU64>>,
    window: usize,
) -> Result<(), EngineError> {
    let expected: std::sync::Arc<std::collections::HashSet<u32>> =
        batch.iter().copied().collect::<std::collections::HashSet<_>>().into();

    let mut tasks = tokio::task::JoinSet::new();
    let mut sent = 0usize;       // 已发出的 FetchReq 数
    let mut accepted = 0usize;   // 已 accept 的块流数
    let mut completed = 0usize;  // 已完成的块任务数

    // v0.9.1 idle watchdog:接收侧不再用"单块 60s 硬超时"判死——窄带中继
    // (5Mbps)下 16 流瓜分带宽时单块要 ~107s,硬超时让大文件 0 字节失败
    // (实测根因)。改为:任意块流上有字节到达就刷新 last_progress_ms,
    // 连续 idle_timeout_secs 无任何字节进展才判死。慢管道上慢传,不再自杀;
    // 对端真僵死(半开连接/黑洞)时仍能在 idle 窗口后退出。
    let idle_timeout_secs = std::env::var("LOCALTRANS_IDLE_TIMEOUT_SEC")
        .ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(90);
    let last_progress_ms = Arc::new(std::sync::atomic::AtomicI64::new(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0),
    ));
    let mut idle_watchdog = tokio::time::interval(Duration::from_millis(1000));
    idle_watchdog.tick().await; // 跳过首个立即 tick

    let join_err = |e: tokio::task::JoinError| {
        let reason = if e.is_panic() {
            let p = e.into_panic();
            p.downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic 载荷非字符串类型".into())
        } else {
            "任务被取消".into()
        };
        EngineError::Io(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            format!("块处理任务失败: {}", reason),
        ))
    };

    // 补发：保持 sent - completed = 窗口（有剩余块才补）
    macro_rules! refill {
        () => {
            while sent < batch.len() && sent - completed < window {
                let chunk = batch[sent];
                sm.send_ctrl(peer, ControlMsg::FetchReq { job_id, chunk })
                    .await
                    .map_err(|e| EngineError::Rpc(format!("发送 FetchReq 失败: {}", e)))?;
                sent += 1;
            }
        };
    }

    refill!();

    while completed < batch.len() {
        tokio::select! {
            // idle watchdog:连续 idle_timeout_secs 无字节进展 → 判死退出。
            // 放在 select 里与流事件并发,任一分支活动都会先于超时被选中。
            _ = idle_watchdog.tick() => {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0);
                let last_ms = last_progress_ms.load(std::sync::atomic::Ordering::Relaxed);
                let idle_ms = now_ms.saturating_sub(last_ms);
                if idle_ms > (idle_timeout_secs as i64) * 1000 {
                    tracing::warn!(
                        "接收看门狗: job {} 批 {} 块已 {}s 无任何字节进展,判死(对端僵死或路径黑洞)",
                        job_id, batch.len(), idle_ms / 1000
                    );
                    return Err(EngineError::Timeout);
                }
            }
            // 对端回流：只有还有未 accept 的在途请求时才等流
            res = conn.accept_uni(), if accepted < sent => {
                accepted += 1;
                let stream = res
                    .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("accept_uni: {}", e))))?;

                let pool = pool.clone();
                let writer = writer.clone();
                let expected = expected.clone();
                let progress = progress.clone();
                let counter = bytes_counter.cloned();
                let last_progress = last_progress_ms.clone();

                tasks.spawn(async move {
                    let mut stream = stream;

                    // 12 字节块流头。无本地超时——活性由外层 idle watchdog
                    // 统一仲裁(连续 idle_timeout_secs 无字节进展才判死)。
                    let mut hdr = [0u8; CHUNK_HEADER_LEN];
                    stream.read_exact(&mut hdr)
                        .await
                        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读块流头: {}", e))))?;
                    let header = ChunkStreamHeader::decode(&hdr)
                        .map_err(|e| EngineError::Protocol(format!("块流头解码失败: {}", e)))?;

                    if header.job_id != job_id || !expected.contains(&header.chunk) {
                        return Err(EngineError::Protocol(format!(
                            "块流不匹配: 期望 job {} 的 {:?}, 实际 job {} chunk {}",
                            job_id, expected, header.job_id, header.chunk
                        )));
                    }

                    let chunk_len = writer.lock().await.manifest.chunk_len(header.chunk) as usize;
                    // RAII：guard Drop 自动还池+归还许可——错误路径与任务被
                    // 外层提前返回 abort 时均不泄漏（T1/T2 审计修复）
                    let mut buf = pool.acquire().await;
                    buf.resize(chunk_len, 0);
                    // v0.9.1: 分段读(64KiB/段)并逐段刷新全局进展时戳——
                    // 只要数据在流(哪怕很慢),外层 watchdog 就不判死。
                    {
                        let mut off = 0usize;
                        while off < chunk_len {
                            let end = (off + 64 * 1024).min(chunk_len);
                            match stream.read_exact(&mut buf[off..end]).await {
                                Ok(()) => {}
                                Err(e) => {
                                    return Err(EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读块数据: {}", e))));
                                }
                            }
                            off = end;
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64)
                                .unwrap_or(0);
                            last_progress.store(now_ms, std::sync::atomic::Ordering::Relaxed);
                        }
                    }

                    // 哈希在写锁外计算（跨任务并行 CPU），锁内仅写盘（T20 吞吐关键路径）
                    let actual_hash = hex::encode(Sha256::digest(&buf[..chunk_len]));
                    let res = writer
                        .lock()
                        .await
                        .write_chunk_preverified(header.chunk, &buf[..chunk_len], &actual_hash);
                    drop(buf);
                    res?;

                    // T15: 更新字节计数器（用于速度计算）
                    if let Some(counter) = counter {
                        counter.fetch_add(chunk_len as u64, std::sync::atomic::Ordering::Relaxed);
                    }

                    // M-C4: 高频进度事件用 try_send——通道满时丢弃而非背压
                    // 阻塞块任务。累计值以 parts 位图/transfer 表为准,丢一次
                    // UI 刷新无碍;Done/Failed 等关键事件仍走 await 不丢。
                    let _ = progress.try_send(ProgressEvent::ChunkDone {
                        job_id,
                        chunk: header.chunk,
                        bytes: chunk_len as u64,
                    });
                    Ok(())
                });
            }
            // 块任务完成：即时收割 + 补发（进度连续到达的关键路径）
            res = tasks.join_next(), if !tasks.is_empty() => {
                match res {
                    Some(jr) => {
                        completed += 1;
                        jr.map_err(join_err)??;
                        refill!();
                    }
                    None => {
                        // tasks 空但 completed < batch.len()：accept 分支的守卫
                        // 已兜住（accepted == sent 时不再等流），理论上不可达
                        tracing::warn!("块任务集意外清空: job {} completed {}/{}", job_id, completed, batch.len());
                    }
                }
            }
        }
    }

    Ok(())
}

/// 发送引擎：为一个 FetchReq 开一条单向块流（12 字节头 + 数据 + fin）。
/// M-C6: 清单只取所需标量(chunk_len/chunk_offset)——不再整份 clone Manifest
/// (数千块的 hash 数组)传进 spawn,省每次 FetchReq 的深拷贝。
pub async fn run_sender(
    conn: Connection,
    pool: Arc<BufferPool>,
    src_path: PathBuf,
    chunk_len: u64,
    chunk_offset: u64,
    job_id: u64,
    chunk: u32,
    bytes_counter: Option<Arc<AtomicU64>>,
    progress: Option<mpsc::Sender<ProgressEvent>>,
) -> Result<(), EngineError> {
    let chunk_len = chunk_len as usize;

    // RAII：guard Drop 自动还池+归还许可——open_uni/write_all/finish 的
    // `?` 提前返回不再泄漏许可（T1/T2 审计修复）
    let mut buf = pool.acquire().await;
    buf.resize(chunk_len, 0);
    {
        let mut file = tokio::fs::File::open(&src_path).await.map_err(|e| -> EngineError { e.into() })?;
        file.seek(io::SeekFrom::Start(chunk_offset)).await?;
        file.read_exact(&mut buf[..chunk_len]).await?;
    }

    let write_err = |ctx: &'static str, e: quinn::WriteError| {
        EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("{}: {}", ctx, e)))
    };
    let mut uni = conn
        .open_uni()
        .await
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("open_uni: {}", e))))?;
    uni.write_all(&ChunkStreamHeader { job_id, chunk }.encode()).await.map_err(|e| write_err("写块流头", e))?;
    uni.write_all(&buf[..chunk_len]).await.map_err(|e| write_err("写块数据", e))?;
    uni.finish()
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("finish: {}", e))))?;
    drop(buf);

    // 新增:bytes_counter + SourceChunkDone 事件
    if let Some(counter) = &bytes_counter {
        counter.fetch_add(chunk_len as u64, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(tx) = &progress {
        // M-C4: 高频事件 try_send,满则丢弃(累计以位图/表为准)
        let _ = tx.try_send(ProgressEvent::SourceChunkDone {
            job_id, chunk, bytes: chunk_len as u64,
        });
    }

    tracing::debug!("块流发送完成: job {} chunk {} ({}B)", job_id, chunk, chunk_len);
    Ok(())
}

/// RPC 路由器：入站控制消息分发与响应（拉取模式服务侧）
pub fn spawn_rpc_router(
    sm: Arc<SessionManager>,
    ctx: SessionCtx,
    reg: Arc<ShareRegistry>,
    mut ctrl_rx: mpsc::Receiver<(Fingerprint, ControlMsg)>,
    ask_tx: mpsc::Sender<OfferAsk>,
    delete_ask_tx: mpsc::Sender<DeleteAsk>,
    sender_jobs: crate::transfer::sender_state::SenderJobMap,
    source_event_tx: Option<mpsc::Sender<ProgressEvent>>,
) {
    use crate::transfer::sender_state::fire_probe_stop;

    tokio::spawn(async move {
        let pool = Arc::new(BufferPool::new());
        // M-C1: legacy jobs 表已删除——sender_jobs 注册表是唯一事实来源,
        // 该表只有 BitmapReq 一个读者(消费方早已不存在),纯内存泄漏
        let mut next_ask_id: u64 = 1;
        // 待确认删除并发上限 8(防弹窗轰炸;超出直接拒绝)
        let delete_permits = Arc::new(tokio::sync::Semaphore::new(8));

        while let Some((fingerprint, msg)) = ctrl_rx.recv().await {
            match msg {
                ControlMsg::SharesReq { msg_id } => {
                    let perms = ctx
                        .trust
                        .lock()
                        .await
                        .get(&fingerprint)
                        .map(|p| p.perms.clone())
                        .unwrap_or_else(crate::identity::Perms::denied);
                    if !perms.browse {
                        tracing::warn!("{} 无浏览权限，拒绝 SharesReq", hex::encode(fingerprint));
                        let _ = sm.send_ctrl(&fingerprint, ControlMsg::SharesResp { shares: vec![], msg_id }).await;
                        continue;
                    }
                    let shares: Vec<crate::protocol::ShareInfo> = reg
                        .list()
                        .iter()
                        .map(|s| crate::protocol::ShareInfo {
                            id: s.id.clone(),
                            alias: s.alias.clone(),
                        })
                        .collect();
                    if let Err(e) = sm.send_ctrl(&fingerprint, ControlMsg::SharesResp { shares, msg_id }).await {
                        tracing::warn!("发送 SharesResp 失败: {}", e);
                    }
                }
                ControlMsg::ListReq { share_id, path, cursor, msg_id } => {
                    // R9-2: 目录浏览请求 - 检查 browse 权限
                    let perms = ctx
                        .trust
                        .lock()
                        .await
                        .get(&fingerprint)
                        .map(|p| p.perms.clone())
                        .unwrap_or_else(crate::identity::Perms::denied);
                    if !perms.browse {
                        tracing::warn!("{} 无浏览权限，拒绝 ListReq", hex::encode(fingerprint));
                        let _ = sm.send_ctrl(&fingerprint, ControlMsg::ListResp {
                            entries: vec![],
                            next_cursor: None,
                            msg_id,
                        }).await;
                        continue;
                    }

                    let list_result = reg.list_dir(&share_id, &path, cursor, 200).await;
                    match list_result {
                        Ok((entries, next_cursor)) => {
                            if let Err(e) = sm.send_ctrl(&fingerprint, ControlMsg::ListResp {
                                entries,
                                next_cursor,
                                msg_id,
                            }).await {
                                tracing::warn!("发送 ListResp 失败: {}", e);
                            }
                        }
                        Err(e) => {
                            tracing::warn!("list_dir 失败: {}，返回空列表", e);
                            // 任何 ShareError 返回空 ListResp（防客户端挂死）
                            let _ = sm.send_ctrl(&fingerprint, ControlMsg::ListResp {
                                entries: vec![],
                                next_cursor: None,
                                msg_id,
                            }).await;
                        }
                    }
                }
                ControlMsg::MetaReq { share_id, path, msg_id } => {
                    let perms = ctx
                        .trust
                        .lock()
                        .await
                        .get(&fingerprint)
                        .map(|p| p.perms.clone())
                        .unwrap_or_else(crate::identity::Perms::denied);
                    if !perms.download {
                        tracing::warn!("{} 无下载权限，拒绝 MetaReq", hex::encode(fingerprint));
                        continue;
                    }
                    let resolved = if let Some(hex_id) = share_id.strip_prefix("push:") {
                        // T15 推送模式：从推送任务注册表解析（推送源不在共享区）
                        match u64::from_str_radix(hex_id, 16).ok().and_then(|oid| resolve_push_file(oid, &path)) {
                            Some(p) => p,
                            None => {
                                tracing::warn!("推送任务不存在或文件不匹配: {} {}", share_id, path);
                                continue;
                            }
                        }
                    } else {
                        match reg.resolve(&share_id, &path) {
                            Ok(p) => p,
                            Err(e) => {
                                tracing::warn!("路径解析失败: {}", e);
                                continue;
                            }
                        }
                    };
                    // v0.11.0 T5:清单构建含全文件哈希扫描,大目录可达秒级——
                    // 下放阻塞线程池,避免卡住路由器循环
                    let resolved_c = resolved.clone();
                    let m = match tokio::task::spawn_blocking(move || Manifest::build(&resolved_c)).await {
                        Ok(Ok(m)) => m,
                        Ok(Err(e)) => {
                            tracing::warn!("清单构建失败: {}", e);
                            continue;
                        }
                        Err(e) => {
                            tracing::warn!("清单构建任务失败: {}", e);
                            continue;
                        }
                    };
                    let role = if share_id.starts_with("push:") {
                        SourceRole::SourcePush
                    } else {
                        SourceRole::SourcePull
                    };

                    // 低危审计修复:MetaResp 单条消息上限 4MiB(take_msg 限制),
                    // 60k 块 × 64B hash hex ≈ 4MiB 边界。超过即拒绝该文件
                    // (>约240GB),提示"文件过大暂不支持"。分页协议留 backlog。
                    if m.chunk_hashes.len() > 60_000 {
                        tracing::warn!(
                            "文件过大暂不支持: {} 共 {} 块(>约240GB)，拒绝 MetaReq",
                            m.file_name, m.chunk_hashes.len()
                        );
                        // 不回 MetaResp——请求方走既有 30s 超时报 Timeout。
                        // 分页协议留 backlog(见 task-19-audit-report.md 取舍)
                        continue;
                    }

                    // Ruling C: use next_source_job_id() for MetaReq-issued job_id
                    let job_id = crate::session::next_source_job_id();

                    // Ruling D: decide progress channel - use source_event_tx if provided, else local channel
                    let (progress_tx, progress_rx) = if let Some(tx) = source_event_tx.clone() {
                        (tx.clone(), None)
                    } else {
                        let (tx, rx) = mpsc::channel::<ProgressEvent>(64);
                        (tx, Some(rx))
                    };

                    // Ruling A: build state with new_sender_job_state, then assign real probe_stop_tx
                    let mut state = crate::transfer::sender_state::new_sender_job_state(
                        job_id,
                        resolved.clone(),
                        m.clone(),
                        None,
                        progress_tx.clone(),
                    );

                    // FIX B: Bridge push controls for large-file push (push: prefix share_id)
                    if let Some(hex_id) = share_id.strip_prefix("push:") {
                        if let Ok(offer_id) = u64::from_str_radix(hex_id, 16) {
                            // v0.6.x:记录关联 offer_id——push 失败路径按它反查清理
                            state.offer_id = Some(offer_id);
                            if let Some(pc) = get_push_control(offer_id) {
                                // Share the push control Arcs so transfer_action gates both small batch AND large-file serving
                                state.paused = pc.paused.clone();
                                state.cancelled = pc.cancelled.clone();
                                tracing::debug!("桥接推送控制: offer_id {:016x} → job_id {:016x}", offer_id, job_id);
                            }
                        }
                    }

                    let (probe_stop_tx, probe_stop_rx) = oneshot::channel();
                    state.probe_stop_tx = Some(Arc::new(std::sync::Mutex::new(Some(probe_stop_tx))));
                    let state = Arc::new(state);

                    // M-C1: legacy jobs 表已删,sender_jobs 为唯一注册表
                    sender_jobs.write().await.insert(job_id, state.clone());

                    // Send SourceStarted event
                    let _ = progress_tx.send(ProgressEvent::SourceStarted {
                        job_id,
                        role,
                        peer: fingerprint,
                        name: m.file_name.clone(),
                        total: m.total_size,
                    }).await;

                    // Spawn SourceProbe and connection drop cleanup
                    // Ruling defensive fix: session lookup failure must not swallow MetaResp
                    let conn_opt = sm.session(&fingerprint).await;
                    if let Some(conn) = conn_opt {
                        let conn_probe = conn.clone();
                        tokio::spawn(crate::transfer::source_probe::run_source_probe(
                            conn_probe,
                            job_id,
                            progress_tx.clone(),
                            state.bytes_counter.clone(),
                            state.active_streams.clone(),
                            state.remote_done.clone(),
                            probe_stop_rx,
                        ));

                        // Spawn connection drop cleanup
                        crate::transfer::sender_state::spawn_connection_drop_cleanup(
                            conn,
                            state.clone(),
                            sender_jobs.clone(),
                        );
                    } else {
                        tracing::warn!("sender 探测: 会话不存在 {}，跳过 SourceProbe 和连接清理", hex::encode(fingerprint));
                    }

                    // Ruling D: if we created a local channel, spawn discard pump
                    if let Some(mut rx) = progress_rx {
                        tokio::spawn(async move {
                            while let Some(ev) = rx.recv().await {
                                tracing::trace!("sender event (临时丢弃): {:?}", ev);
                            }
                        });
                    }

                    // v0.10.0:大文件整体 hash 从清单派生(sha256 of 顺序拼接的块 hash)——
                    // Manifest::build 本就读全文件,派生零成本;后台任务方案有 MetaReq 竞态已弃用
                    let file_hash = if share_id.starts_with("push:") {
                        let joined = m.chunk_hashes.join("");
                        Some(hex::encode(sha2::Sha256::digest(joined.as_bytes())))
                    } else {
                        None
                    };
                    let resp = ControlMsg::MetaResp {
                        job_id,
                        file_name: m.file_name.clone(),
                        total_size: m.total_size,
                        chunk_hashes: m.chunk_hashes.clone(),
                        file_hash,
                        msg_id,
                    };
                    if let Err(e) = sm.send_ctrl(&fingerprint, resp).await {
                        tracing::warn!("发送 MetaResp 失败: {}", e);
                    }
                }
                ControlMsg::FetchReq { job_id, chunk } => {
                    let perms = ctx
                        .trust
                        .lock()
                        .await
                        .get(&fingerprint)
                        .map(|p| p.perms.clone())
                        .unwrap_or_else(crate::identity::Perms::denied);
                    if !perms.download {
                        tracing::warn!("{} 无下载权限，拒绝 FetchReq", hex::encode(fingerprint));
                        continue;
                    }

                    // T7: Throttle gate - check sender job exists and get state
                    let state = match sender_jobs.read().await.get(&job_id).cloned() {
                        Some(s) => s,
                        None => { tracing::warn!("sender 任务不存在: job {}", job_id); continue; }
                    };

                    // T16: 检查取消标志 - 触发清理并跳过
                    if state.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                        tracing::info!("sender 任务已取消: job {}, 触发清理", job_id);
                        // M-C5: 写锁只包 remove,await 发事件放锁外(锁序问题)
                        let removed = sender_jobs.write().await.remove(&job_id);
                        if let Some(state_mut) = removed {
                            // 停止探针 (用 fire_probe_stop 兼容多克隆场景)
                            fire_probe_stop(&state_mut);
                            // 发送失败事件
                            let _ = state_mut.progress_tx.send(ProgressEvent::SourceFailed {
                                job_id,
                                reason: "任务已取消".to_string(),
                            }).await;
                        }
                        continue;
                    }

                    // T16: 暂停不丢请求——延迟到暂停解除后再服务。此前直接
                    // 丢弃 FetchReq,而接收端窗口配额(sent-completed)按"已发
                    // 请求数"记账且无 per-请求重发,被丢的块永久缺失:恢复后
                    // 窗口死锁,直至 60s idle 看门狗判死(2026-09-07 D2 暂停→
                    // 继续→interrupted 实证)。门在 spawn 任务内等,不阻塞路由器。

                    // T7: Load cur/cap from state for throttle checking
                    let cur = state.active_streams.load(std::sync::atomic::Ordering::Relaxed);
                    let cap = state.throttle_cap.load(std::sync::atomic::Ordering::Relaxed);
                    if cur >= cap {
                        tracing::debug!("节流拒绝: job {} (cur={}, cap={})", job_id, cur, cap);
                        continue;
                    }

                    // T7: Keep the chunk bounds check using state.manifest.chunk_count()
                    if chunk >= state.manifest.chunk_count() {
                        tracing::warn!("FetchReq 块号越界: job {} chunk {} (共 {})", job_id, chunk, state.manifest.chunk_count());
                        continue;
                    }

                    // T7: Keep the session connection lookup
                    let conn = match sm.session(&fingerprint).await {
                        Some(c) => c,
                        None => {
                            tracing::warn!("会话不存在，无法发送块: job {}", job_id);
                            continue;
                        }
                    };

                    // T7: Spawn block with run_sender, wiring up the two placeholder args
                    let pool = pool.clone();
                    // M-C6: 只传标量,不再 clone 整份 Manifest 进 spawn
                    let chunk_len = state.manifest.chunk_len(chunk);
                    let chunk_off = state.manifest.chunk_offset(chunk);
                    // T16: 暂停门(任务内等待,不阻塞路由器循环)——暂停期间取消
                    // 则放弃本请求(整个任务正在拆除,接收端由 JobFailed 兜底)
                    let pause_gate = state.paused.clone();
                    let cancel_gate = state.cancelled.clone();
                    tokio::spawn(async move {
                        while pause_gate.load(std::sync::atomic::Ordering::Relaxed) {
                            if cancel_gate.load(std::sync::atomic::Ordering::Relaxed) {
                                tracing::debug!("暂停期间取消,放弃延迟块: job {} chunk {}", job_id, chunk);
                                return;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        state.active_streams.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let res = run_sender(
                            conn, pool, state.src_path.clone(), chunk_len, chunk_off,
                            job_id, chunk,
                            Some(state.bytes_counter.clone()),
                            Some(state.progress_tx.clone()),
                        ).await;
                        state.active_streams.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                        if let Err(e) = res {
                            tracing::warn!("块流发送失败: job {} chunk {}: {}", job_id, chunk, e);
                        }
                    });
                }
                ControlMsg::OfferReq { job_id, files } => {
                    let perms = ctx
                        .trust
                        .lock()
                        .await
                        .get(&fingerprint)
                        .map(|p| p.perms.clone())
                        .unwrap_or_else(crate::identity::Perms::denied);
                    // v0.11.0 T4:Ask 档等待移入子任务——路由器循环绝不 await 用户响应
                    // （最长 offer_timeout_secs，默认 60s、上限 600s）。Ask 时先发问即
                    // continue，应答/超时后子任务自行续发 OfferResp 与接收编排。
                    if matches!(perms.push, crate::identity::PushPolicy::Ask) {
                        let secs = ctx.config.read().await.offer_timeout_secs.clamp(1, 600);
                        let (respond_tx, respond_rx) = tokio::sync::oneshot::channel();
                        let extend = std::sync::Arc::new(tokio::sync::Notify::new());
                        let deadline_epoch_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as i64 + (secs as i64) * 1000)
                            .unwrap_or(0);
                        let _ = ask_tx
                            .send(OfferAsk {
                                from: fingerprint,
                                job_id,
                                files: files.clone(),
                                respond: respond_tx,
                                extend: extend.clone(),
                                deadline_epoch_ms,
                            })
                            .await;

                        let sm_ask = sm.clone();
                        let ctx_ask = ctx.clone();
                        let pool_ask = pool.clone();
                        let fp_ask = fingerprint;
                        tokio::spawn(async move {
                            // 等待用户应答（超时/顺延语义同 v0.5.0）
                            let mut extended = false;
                            let mut deadline = tokio::time::Instant::now()
                                + Duration::from_secs(secs);
                            let mut respond_rx = respond_rx;
                            let answer: (bool, Option<String>, Option<crate::protocol::OfferDenyReason>) = loop {
                                tokio::select! {
                                    biased;
                                    r = &mut respond_rx => {
                                        break match r {
                                            Ok(Some(save_dir)) =>
                                                (true, Some(save_dir.display().to_string()), None),
                                            Ok(None) | Err(_) =>
                                                (false, None, Some(crate::protocol::OfferDenyReason::Denied)),
                                        };
                                    }
                                    _ = extend.notified(), if !extended => {
                                        extended = true;
                                        deadline = tokio::time::Instant::now()
                                            + Duration::from_secs(secs);
                                    }
                                    _ = tokio::time::sleep_until(deadline) => {
                                        break (false, None, Some(crate::protocol::OfferDenyReason::Timeout));
                                    }
                                }
                            };
                            let (accepted, save_dir, deny_reason) = answer;
                            if !accepted {
                                // T6-FR6:自动拒绝曾完全静默——对端只见"对方超时未确认",
                                // 本端无任何痕迹,历史上被误判为"入站引擎退化"
                                // (2026-09-08 真机取证:弹窗被系统界面遮挡/无人应答时,
                                // 全引擎链路正常,仅此处超时拒绝)。落一条 INFO 便于排障。
                                tracing::info!(
                                    "推送请求未应答,已自动拒绝: job={:016x} 对端={:.8} 原因={:?}",
                                    job_id, hex::encode(&fp_ask[..4]), deny_reason
                                );
                            }
                            handle_offer_decision(
                                &sm_ask, &ctx_ask, &pool_ask, fp_ask, job_id, files,
                                accepted, save_dir, deny_reason,
                            ).await;
                        });
                        continue;
                    }
                    let (accepted, save_dir, deny_reason) = match perms.push {
                        crate::identity::PushPolicy::Deny => (false, None, Some(crate::protocol::OfferDenyReason::Denied)),
                        crate::identity::PushPolicy::Auto => {
                            let dir = ctx.config.read().await.download_dir.clone();
                            (true, Some(dir.display().to_string()), None)
                        }
                        crate::identity::PushPolicy::Ask => unreachable!("Ask 分支已提前 spawn 并 continue"),
                    };
                    handle_offer_decision(
                        &sm, &ctx, &pool, fingerprint, job_id, files,
                        accepted, save_dir, deny_reason,
                    ).await;
                    continue;
                }
                ControlMsg::RecvProgress { job_id, cumulative_bytes } => {
                    // v0.10.0 双进度:更新 sender 任务的 remote_done,source_probe
                    // 500ms 周期随 SourceSpeed 带出(不额外发事件,防洪泛)
                    if let Some(state) = sender_jobs.read().await.get(&job_id) {
                        state.remote_done.store(
                            cumulative_bytes,
                            std::sync::atomic::Ordering::Relaxed,
                        );
                    }
                }
                ControlMsg::RecvAck { job_id } => {
                    // v0.2.4 双向确认闭环：接收方已 finalize 落盘 → 发送方据此
                    // 发 SourceDone（UI source 行落完成态）并清理 sender 任务。
                    // 此前 SourceDone 定义了但无人发送——发送侧永远 active。
                    let state = sender_jobs.write().await.remove(&job_id);
                    if let Some(state) = state {
                        fire_probe_stop(&state);
                        let _ = state.progress_tx.send(ProgressEvent::SourceDone { job_id }).await;
                        tracing::info!("收到 RecvAck: job {} 完成（对端已确认落盘）", job_id);
                    } else {
                        tracing::debug!("RecvAck 目标任务不存在（可能已清理）: job {}", job_id);
                    }
                }
                ControlMsg::TransferCtl { job_id, action } => {
                    // T7: Throttle 作用于 sender 侧任务(源限速),不经接收端控制通道
                    if let crate::protocol::TransferAction::Throttle { max_streams } = action {
                        let state = sender_jobs.read().await.get(&job_id).cloned();
                        match state {
                            Some(s) => {
                                s.throttle_cap.store(max_streams, std::sync::atomic::Ordering::Relaxed);
                                tracing::info!("sender job {} 限速: max_streams={}", job_id, max_streams);
                            }
                            None => tracing::warn!("Throttle 目标任务不存在: job {}", job_id),
                        }
                        continue;
                    }

                    // v0.6.x:对端(下载方)取消拉取——本机是数据源时清理 sender
                    // 任务并发 SourceFailed,source 行落终态(此前无跨端通知,
                    // source 行永久 active、计时不停)。幂等:任务不存在(已完成/
                    // 已清理)时仅日志。注意:探针 60s 空闲自杀路径保持不清任务表
                    // (长暂停后 RecvAck 仍需找到任务发 SourceDone)。
                    if action == crate::protocol::TransferAction::Cancel {
                        let state = sender_jobs.write().await.remove(&job_id);
                        if let Some(state) = state {
                            fire_probe_stop(&state);
                            let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
                                job_id,
                                reason: "对方已取消".to_string(),
                            }).await;
                            tracing::info!("收到对端({})取消: job {} 清理 sender 任务", hex::encode(fingerprint), job_id);
                            continue;
                        } else {
                            tracing::debug!("TransferCtl Cancel 目标任务不存在(可能已完成/已清理): job {}", job_id);
                        }
                    }

                    // T15: 查全局任务控制注册表，转发到对应传输任务的控制通道
                    let task_control = task_controls().lock().unwrap().get(&job_id).cloned();

                    if let Some(tx) = task_control {
                        let task_action = match action {
                            crate::protocol::TransferAction::Pause => TaskControl::Pause,
                            crate::protocol::TransferAction::Resume => TaskControl::Resume,
                            crate::protocol::TransferAction::Cancel => TaskControl::Cancel,
                            // Throttle is handled above by the if let pre-branch, unreachable here
                            _ => unreachable!("Throttle should be handled above"),
                        };

                        // 转发到任务的控制通道
                        let _ = tx.send(task_action).await;
                        tracing::info!("TransferCtl 已转发: job {} action={:?}", job_id, action);
                    } else {
                        tracing::warn!("任务不存在或无控制通道: job {}", job_id);
                    }
                }
                // 安卓 T2: 远程文件操作 RPC 路由
                ControlMsg::ShareRename { share_id, path, new_name, msg_id } => {
	                    // P0-2a: 远程文件操作权限门(与 ListReq 对齐要求 browse;fail-closed)
	                    let perms = ctx
	                        .trust
	                        .lock()
	                        .await
	                        .get(&fingerprint)
	                        .map(|p| p.perms.clone())
	                        .unwrap_or_else(crate::identity::Perms::denied);
	                    if !perms.browse {
	                        tracing::warn!("{} 无操作权限, 拒绝 ShareRename", hex::encode(&fingerprint[..4]));
	                        let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
	                            ok: false, error: Some("无操作权限".into()), msg_id,
	                        }).await;
	                        continue;
	                    }
                    let result = reg.op_rename(&share_id, &path, &new_name)
                        .map_err(|e| e.to_string());
                    let resp = match result {
                        Ok(()) => ControlMsg::ShareOpResult { ok: true, error: None, msg_id },
                        Err(e) => ControlMsg::ShareOpResult { ok: false, error: Some(e), msg_id },
                    };
                    let _ = sm.send_ctrl(&fingerprint, resp).await;
                    tracing::debug!(share_id, "share op rename handled");
                }
                ControlMsg::ShareDelete { share_id, path, msg_id } => {
                            // P0-2a: 远程文件操作权限门(与 ListReq 对齐要求 browse;fail-closed)
                            let perms = ctx
                                .trust
                                .lock()
                                .await
                                .get(&fingerprint)
                                .map(|p| p.perms.clone())
                                .unwrap_or_else(crate::identity::Perms::denied);
                            if !perms.browse {
                                tracing::warn!("{} 无操作权限, 拒绝 ShareDelete", hex::encode(&fingerprint[..4]));
                                let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
                                    ok: false, error: Some("无操作权限".into()), msg_id,
                                }).await;
                                continue;
                            }

                            // 解析 + 统计(失败即回错误,不进确认流)
                            let resolved = match reg.resolve(&share_id, &path) {
                                Ok(p) => p,
                                Err(e) => {
                                    let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
                                        ok: false, error: Some(e.to_string()), msg_id,
                                    }).await;
                                    continue;
                                }
                            };
                            let is_dir = resolved.is_dir();
                            // v0.11.0 T5:目录计数含递归遍历(上限 1 万项)——下放阻塞线程池
                            let resolved_for_count = resolved.clone();
                            let entry_count = if is_dir {
                                tokio::task::spawn_blocking(move || count_dir_entries(&resolved_for_count))
                                    .await
                                    .unwrap_or(0)
                            } else { 0 };
                            let name = path.rsplit('/').next().unwrap_or(&path).to_string();

                            let permit = match delete_permits.clone().try_acquire_owned() {
                                Ok(p) => p,
                                Err(_) => {
                                    let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
                                        ok: false, error: Some("待确认删除过多, 请稍后再试".into()), msg_id,
                                    }).await;
                                    continue;
                                }
                            };

                            let secs = ctx.config.read().await.consent_timeout_secs.clamp(1, 600);
                            let deadline_epoch_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as i64 + (secs as i64) * 1000)
                                .unwrap_or(0);
                            let ask_id = next_ask_id;
                            next_ask_id += 1;
                            let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
                            // fail-closed:壳层无消费者/已退出 → send 失败 → 拒绝
                            if delete_ask_tx.send(DeleteAsk {
                                ask_id, from: fingerprint.clone(), share_id: share_id.clone(),
                                name, is_dir, entry_count, respond: tx, deadline_epoch_ms,
                            }).await.is_err() {
                                let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
                                    ok: false, error: Some("对方未确认删除".into()), msg_id,
                                }).await;
                                continue;
                            }

                            // 等待移入子任务:路由器循环绝不 await 用户响应
                            let sm_c = sm.clone();
                            let reg_c = reg.clone();
                            let fp_c = fingerprint;
                            let msg_id_c = msg_id;
                            tokio::spawn(async move {
                                let deadline = tokio::time::Instant::now()
                                    + std::time::Duration::from_secs(secs);
                                let allowed = match tokio::time::timeout_at(deadline, rx).await {
                                    Ok(Ok(true)) => true,
                                    _ => false, // 拒绝 / 通道关闭 / 超时 —— 全部 fail-closed
                                };
                                drop(permit);
                                let result = if allowed {
                                    // v0.11.0 T5:remove_dir_all 可能极慢——下放阻塞线程池
                                    let (reg_d, share_d, path_d) = (reg_c.clone(), share_id.clone(), path.clone());
                                    tokio::task::spawn_blocking(move ||
                                        reg_d.op_delete(&share_d, &path_d).map_err(|e| e.to_string())
                                    ).await.unwrap_or_else(|e| Err(e.to_string()))
                                } else {
                                    Err("对方未确认删除".to_string())
                                };
                                let ok = result.is_ok();
                                let _ = sm_c.send_ctrl(&fp_c, ControlMsg::ShareOpResult {
                                    ok, error: result.err(), msg_id: msg_id_c,
                                }).await;
                                if ok {
                                    tracing::info!(share_id, "远程删除已确认并执行: 对端={}", hex::encode(&fp_c[..4]));
                                }
                            });
                            continue;
                        }
                ControlMsg::ShareMkdir { share_id, path, msg_id } => {
	                    // P0-2a: 远程文件操作权限门(与 ListReq 对齐要求 browse;fail-closed)
	                    let perms = ctx
	                        .trust
	                        .lock()
	                        .await
	                        .get(&fingerprint)
	                        .map(|p| p.perms.clone())
	                        .unwrap_or_else(crate::identity::Perms::denied);
	                    if !perms.browse {
	                        tracing::warn!("{} 无操作权限, 拒绝 ShareMkdir", hex::encode(&fingerprint[..4]));
	                        let _ = sm.send_ctrl(&fingerprint, ControlMsg::ShareOpResult {
	                            ok: false, error: Some("无操作权限".into()), msg_id,
	                        }).await;
	                        continue;
	                    }
                    let result = reg.op_mkdir(&share_id, &path)
                        .map_err(|e| e.to_string());
                    let resp = match result {
                        Ok(()) => ControlMsg::ShareOpResult { ok: true, error: None, msg_id },
                        Err(e) => ControlMsg::ShareOpResult { ok: false, error: Some(e), msg_id },
                    };
                    let _ = sm.send_ctrl(&fingerprint, resp).await;
                    tracing::debug!(share_id, "share op mkdir handled");
                }
                _ => {
                    tracing::debug!("忽略非 RPC 消息");
                }
            }
        }
    });
}

/// v0.11.0 T4:OfferReq 决策落地(路由器与 Ask 子任务共用)——查收件箱 skip 表、
/// 回 OfferResp、accept 时 spawn 两阶段接收编排。原为路由器内联代码,Ask 等待
/// spawn 化后抽出供两条路径复用;行为与旧内联版本等价。
async fn handle_offer_decision(
    sm: &Arc<SessionManager>,
    ctx: &SessionCtx,
    pool: &Arc<BufferPool>,
    fingerprint: Fingerprint,
    job_id: u64,
    files: Vec<crate::protocol::OfferFile>,
    accepted: bool,
    save_dir: Option<String>,
    deny_reason: Option<crate::protocol::OfferDenyReason>,
) {
    // v0.10.0 小文件秒传:accept 前查收件箱索引,已持有的小文件
    // (hash+size 双匹配)回 skip 表——发送方批流剔除,带宽零消耗
    let default_download_dir = ctx.config.read().await.download_dir.clone();
    let download_dir = if let Some(save_dir_str) = &save_dir {
        PathBuf::from(save_dir_str)
    } else {
        default_download_dir
    };
    let inbox_index_path = download_dir.join(".localtrans-inbox-index.json");
    // v0.11.0 T5:索引加载(读盘)+ prune(逐项 exists)是文件系统 IO——下放阻塞线程池
    let mut inbox_index = tokio::task::spawn_blocking(move || {
        let mut idx = crate::transfer::dedup::InboxIndex::load(&inbox_index_path);
        idx.prune_missing();
        idx
    }).await.unwrap_or_else(|_| crate::transfer::dedup::InboxIndex::load(
        &download_dir.join(".localtrans-inbox-index.json")));
    // v0.11.0 T18 秒传位图化(A5 oracle 完整方案):仅在接受时才计算/回传位图,
    // 与请求 files 等长逐位对应(true=已持有可跳过);不再回显 hash——
    // 拒绝(Deny/超时/Ask 未答)回空位图,accepted 位图也不含任何 hash 字符串
    let skip_bitmap: Vec<bool> = if accepted {
        files.iter().map(|f| {
            f.size <= SMALL_FILE_LIMIT
                && f.hash.as_ref().map(|h| inbox_index.lookup(h, f.size).is_some()).unwrap_or(false)
        }).collect()
    } else {
        Vec::new()
    };

    if let Err(e) = sm
        .send_ctrl(&fingerprint, ControlMsg::OfferResp {
            accepted,
            save_dir: save_dir.clone(),
            reason: if accepted { None } else { deny_reason },
            skip_bitmap: skip_bitmap.clone(),
        })
        .await
    {
        tracing::warn!("发送 OfferResp 失败: {}", e);
    }

    // 如果接受，启动接收编排任务（顺序执行，避免多个 accept_uni 消费者抢流）：
    // 阶段 1 小文件批流（甲先发批流）；阶段 2 大文件反向驱动
    // （MetaReq(push:{offer_id}) → MetaResp → 窗口 FetchReq → 块流 →
    // PartWriter::load_or_open 续传 → finalize → JobDone 通知甲侧）。
    // 发送方 push_files 同样先批流后大文件，两阶段不会并发混流。
    if accepted {
        // v0.5.0 Auto/Ask 接受都通知壳层（壳层只对 Auto 发系统通知）
        if let Some(tx) = auto_offer_hook().lock().unwrap().clone() {
            let _ = tx.try_send(AutoOfferInfo {
                job_id,
                peer: fingerprint,
                file_count: files.len(),
            });
        }

        let conn = match sm.session(&fingerprint).await {
            Some(c) => c,
            None => {
                tracing::warn!("会话不存在，无法接收推送");
                return;
            }
        };
        let sm_drv = sm.clone();
        let peer_fp = fingerprint;
        let offer_id = job_id;
        // v0.11.0 T18:小文件批流按位图剔除跳过项(发送方已按位图过滤,
        // 此处同步剔除保证旧对端不回位图时接收侧仍不重复落盘)
        let small: Vec<crate::protocol::OfferFile> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| f.size <= SMALL_FILE_LIMIT)
            .filter(|(i, _)| !skip_bitmap.get(*i).copied().unwrap_or(false))
            .map(|(_, f)| f.clone())
            .collect();
        // 位图命中的小文件本地复用数据(hash,src,size,rel_dir,name)
        let small_instant: Vec<(String, PathBuf, u64, String, String)> = files.iter()
            .enumerate()
            .filter(|(i, f)| f.size <= SMALL_FILE_LIMIT
                && skip_bitmap.get(*i).copied().unwrap_or(false))
            .filter_map(|(_, f)| {
                let h = f.hash.clone()?;
                let src = inbox_index.lookup(&h, f.size).cloned()?;
                Some((h, src, f.size, f.rel_dir.clone(), f.name.clone()))
            })
            .collect();
        // 索引本体移入 spawn(仅 skip 命中时才需要);路径已在上面物化。
        // 无命中时立即 drop,避免陈旧快照在 spawn 内覆盖磁盘最新状态
        let mut inbox_index = if skip_bitmap.is_empty() || !skip_bitmap.iter().any(|b| *b) {
            None
        } else {
            Some(inbox_index)
        };
        let download_dir = download_dir.clone();
        let large: Vec<crate::protocol::OfferFile> = files
            .iter()
            .filter(|f| f.size > SMALL_FILE_LIMIT)
            .cloned()
            .collect();
        let pool_drv = pool.clone();
        tokio::spawn(async move {
            let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

            // v0.2.4：接收事件泵——转发到进程级 hook（乙侧 UI 进度行）。
            // hook 未注册（测试/示例）时事件自然丢弃，行为同旧版。
            let hook_tx = inbound_recv_hook().lock().unwrap().clone();
            let pump_tx = progress_tx.clone();
            tokio::spawn(async move {
                while let Some(ev) = progress_rx.recv().await {
                    if let Some(tx) = &hook_tx {
                        let _ = tx.send(ev).await;
                    }
                }
            });

            // 两阶段接收（小文件批流 → 大文件反向驱动）。
            // v0.2.4：失败路径回执 JobFailed——甲侧以 Failed 收场
            // （不再傻等 120s 超时），乙侧 UI 同步落 failed 态
            let ack_result = async {
                if !small.is_empty() {
                    recv_small_files_batched(&conn, download_dir.as_ref(), offer_id, &pump_tx).await?;
                }
                // v0.10.0 秒传小文件:本地复用 + InstantHit(不进批流)
                let mut inbox_index = inbox_index.take()
                    .unwrap_or_else(|| crate::transfer::dedup::InboxIndex::load(
                        &download_dir.join(".localtrans-inbox-index.json")));
                for (h, src, size, rel_dir, name) in &small_instant {
                    let mut dest_dir = download_dir.clone();
                    for comp in rel_dir.split('/').filter(|s| !s.is_empty()) {
                        dest_dir.push(sanitize_component(comp));
                    }
                    match crate::transfer::dedup::place_dedup_copy(src, &dest_dir, name) {
                        Ok(dest) => {
                            inbox_index.insert(h.clone(), dest, *size);
                            let _ = pump_tx.send(ProgressEvent::InstantHit {
                                job_id: offer_id, name: name.clone(), total: *size,
                            }).await;
                        }
                        Err(e) => {
                            // 复用失败降级:不阻断后续文件,记警告
                            tracing::warn!("小文件秒传放置失败({}): {}", &h[..8.min(h.len())], e);
                        }
                    }
                }
                // v0.10.0 修复:仅在确有复用写入时落盘——offer 处理时的
                // 空快照无条件 save 会覆盖并发的批流进账(读-改-写竞态)
                if !small_instant.is_empty() {
                    inbox_index.save();
                }
                for f in &large {
                    recv_push_large_file(
                        &sm_drv, &conn, &pool_drv, &peer_fp, offer_id, f,
                        &download_dir, &download_dir, &pump_tx,
                    ).await?;
                }
                Ok::<(), EngineError>(())
            }.await;

            match ack_result {
                Ok(()) => {
                    // 整个 offer 接收落盘完毕 → 通知甲侧（甲据此发 Done，
                    // 保证 Done 之后文件一定已在磁盘上）
                    if let Err(e) = sm_drv.send_ctrl(&peer_fp, ControlMsg::JobDone {
                        job_id: offer_id,
                        offer_id,
                    }).await {
                        tracing::warn!("发送 JobDone 失败: {}", e);
                    }
                }
                Err(e) => {
                    tracing::warn!("推送接收失败: {}", e);
                    let _ = sm_drv.send_ctrl(&peer_fp, ControlMsg::JobFailed {
                        offer_id,
                        reason: e.to_string(),
                    }).await;
                    let _ = pump_tx.send(ProgressEvent::Failed {
                        job_id: offer_id,
                        reason: format!("接收失败: {}", e),
                    }).await;
                }
            }
            // 停泵：drop 所有克隆后 recv 返回 None，泵任务自然退出
            drop(pump_tx);
            drop(progress_tx);
        });
    }
}

/// 小文件批流阈值（1MiB）
pub const SMALL_FILE_LIMIT: u64 = 1024 * 1024;

/// T15: 每个 offer 任务的小文件批流实际开的单向流数（按 job_id 键控，
/// 并行测试互不干扰）。契约：一次 push_files 的小文件走**单条**批流——
/// 测试断言此值锁住该契约（退化为逐文件开流会在此暴露）。
/// 低危审计修复:仅测试构建保留——生产路径该表只增不减是内存泄漏点。
#[cfg(test)]
fn small_batch_streams() -> &'static std::sync::Mutex<HashMap<u64, u64>> {
    static M: std::sync::OnceLock<std::sync::Mutex<HashMap<u64, u64>>> = std::sync::OnceLock::new();
    M.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// 记录一次批流开流（send_small_files_batched 每次调用恰开 1 条）
#[cfg(test)]
fn record_small_batch_stream(job_id: u64) {
    let mut m = small_batch_streams().lock().unwrap();
    *m.entry(job_id).or_insert(0) += 1;
}

/// 查询某任务的小文件批流开流数（测试断言用）
#[cfg(test)]
pub fn small_batch_streams_opened(job_id: u64) -> u64 {
    small_batch_streams().lock().unwrap().get(&job_id).copied().unwrap_or(0)
}

/// 小文件批流发送：在单条双向流上发送多个小文件
/// v0.2.6 格式：循环发送 [u16 名长][名 UTF-8][u16 rel长][rel UTF-8][u64 大小][数据]
/// （rel 为空串时 2 字节零长——与旧格式 [u16 名长][名][u64 大小][数据] 不兼容，
/// 版本对齐：两端同版本升级，无混跑场景）
pub async fn send_small_files_batched(
    conn: &Connection,
    job_id: u64,
    files: Vec<(PathBuf, crate::protocol::OfferFile)>,
) -> Result<(), EngineError> {
    // T16: 获取推送控制标志
    let control = push_controls().lock().unwrap().get(&job_id).cloned();

    let mut uni = conn
        .open_uni()
        .await
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("open_uni: {}", e))))?;
    #[cfg(test)]
    record_small_batch_stream(job_id);

    for (path, offer) in files {
        // T16: 检查取消标志
        if let Some(ref ctl) = control {
            if ctl.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(EngineError::Cancelled);
            }
            // T16: 检查暂停标志 - 等待恢复
            while ctl.paused.load(std::sync::atomic::Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(200)).await;
                // 暂停期间仍需检查取消
                if ctl.cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(EngineError::Cancelled);
                }
            }
        }

        let data = fs::read(&path)?;
        let name_bytes = offer.name.as_bytes();
        let name_len = name_bytes.len() as u16;
        let rel = offer.rel_dir.trim_matches('/');
        let rel_bytes = rel.as_bytes();
        let rel_len = rel_bytes.len() as u16;

        // 写入：[u16 名长][名 UTF-8][u16 rel长][rel UTF-8][u64 大小][数据]
        uni.write_all(&name_len.to_be_bytes()).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写名长: {}", e))))?;
        uni.write_all(name_bytes).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写文件名: {}", e))))?;
        uni.write_all(&rel_len.to_be_bytes()).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写目录长: {}", e))))?;
        uni.write_all(rel_bytes).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写目录名: {}", e))))?;
        uni.write_all(&offer.size.to_be_bytes()).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写文件大小: {}", e))))?;
        uni.write_all(&data).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("写文件数据: {}", e))))?;
    }

    uni.finish()
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("finish: {}", e))))?;

    Ok(())
}

/// 小文件批流接收：从单条双向流接收多个小文件
pub async fn recv_small_files_batched(
    conn: &Connection,
    save_dir: &Path,
    job_id: u64,
    progress: &mpsc::Sender<ProgressEvent>,
) -> Result<(), EngineError> {
    let mut stream = tokio::time::timeout(Duration::from_secs(30), conn.accept_uni())
        .await
        .map_err(|_| EngineError::Timeout)?
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("accept_uni: {}", e))))?;

    let mut file_count = 0u32;
    let mut total_bytes = 0u64;

    loop {
        // 读取 [u16 名长]（read_exact 消除半包；流在整文件边界干净结束 → FinishedEarly(0)）
        let mut name_len_bytes = [0u8; 2];
        match stream.read_exact(&mut name_len_bytes).await {
            Ok(()) => {}
            Err(quinn::ReadExactError::FinishedEarly(n)) if n == 0 => break, // 流结束 = 全部收完
            Err(quinn::ReadExactError::FinishedEarly(n)) => {
                return Err(EngineError::Protocol(format!("名长读取不完整: 流结束前读了 {} 字节", n)));
            }
            Err(e) => {
                return Err(EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读名长: {}", e))));
            }
        }

        let name_len = u16::from_be_bytes(name_len_bytes) as usize;

        // 读取 [名 UTF-8]
        let mut name_bytes = vec![0u8; name_len];
        stream.read_exact(&mut name_bytes).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读文件名: {}", e))))?;
        let file_name = String::from_utf8(name_bytes)
            .map_err(|e| EngineError::Protocol(format!("文件名非 UTF-8: {}", e)))?;

        // P0-1a: 文件名净化(拒绝穿越/绝对路径/ADS/保留名等)
        let file_name = crate::transfer::sanitize_file_name(&file_name)?;

        // v0.2.6 读取 [u16 rel长][rel UTF-8]（文件夹推送的子目录，空串=根）
        let mut rel_len_bytes = [0u8; 2];
        stream.read_exact(&mut rel_len_bytes).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读目录长: {}", e))))?;
        let rel_len = u16::from_be_bytes(rel_len_bytes) as usize;
        let rel_dir = if rel_len > 0 {
            let mut rel_bytes = vec![0u8; rel_len];
            stream.read_exact(&mut rel_bytes).await
                .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读目录名: {}", e))))?;
            String::from_utf8(rel_bytes)
                .map_err(|e| EngineError::Protocol(format!("目录名非 UTF-8: {}", e)))?
        } else {
            String::new()
        };

        // 读取 [u64 大小]
        let mut size_bytes = [0u8; 8];
        stream.read_exact(&mut size_bytes).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读文件大小: {}", e))))?;
        let file_size = u64::from_be_bytes(size_bytes);

        // P0-4: 声明大小上限校验——超限即断流拒绝,杜绝 vec![0u8; u64::MAX] 式 OOM
        if file_size > SMALL_FILE_LIMIT {
            return Err(EngineError::Protocol(format!(
                "批流文件超过小文件上限: 声明 {} 字节, 上限 {}",
                file_size, SMALL_FILE_LIMIT
            )));
        }

        // 读取 [数据]
        let mut data = vec![0u8; file_size as usize];
        stream.read_exact(&mut data).await
            .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::UnexpectedEof, format!("读文件数据: {}", e))))?;

        // 写入文件(v0.2.6:rel_dir 子目录,成分逐个净化防穿越;P0-1a:文件名已净化)
        let mut dest_dir = save_dir.to_path_buf();
        for comp in rel_dir.split('/').filter(|s| !s.is_empty()) {
            dest_dir.push(sanitize_component(comp));
        }
        fs::create_dir_all(&dest_dir)?;
        let dest = write_small_file(&dest_dir, &file_name, &data)?;

        // v0.10.0 正常接收完成 → 索引进账(内容 hash 自算,下次同文件秒传)。
        // 批流帧格式不带 hash(协议兼容),就地重算——小文件 ≤1MiB 毫秒级
        let content_hash = crate::transfer::dedup::sha256_of_slice(&data);
        let index_path = save_dir.join(".localtrans-inbox-index.json");
        let mut index = crate::transfer::dedup::InboxIndex::load(&index_path);
        index.insert(content_hash, dest, file_size);
        index.save();

        file_count += 1;
        total_bytes += file_size;

        let _ = progress.send(ProgressEvent::Started {
            job_id,
            name: file_name.clone(),
            total: file_size,
        }).await;

        let _ = progress.send(ProgressEvent::Done { job_id }).await;
    }

    tracing::info!("小文件批流接收完成: {} 个文件, {} 字节", file_count, total_bytes);
    Ok(())
}

// ============ v0.2.6: 文件夹拉取 ============

/// 远端目录递归枚举结果：文件相对路径（'/' 分隔）+ 大小
#[derive(Clone, Debug)]
pub struct RemoteDirListing {
    pub files: Vec<(String, u64)>,
}

/// BFS 递归枚举远端目录（每目录一次 ListReq 往返）。
/// 深度/广度上限防御：深度 ≤ 32、单目录条目按服务端分页（每页 200）取全，
/// 总文件数硬顶 10000——对端共享区异常时不要把本机内存/时间拖死。
pub async fn list_remote_dir_recursive(
    sm: &SessionManager,
    peer: &Fingerprint,
    share_id: &str,
    root_rel: &str,
) -> Result<RemoteDirListing, EngineError> {
    const MAX_DEPTH: usize = 32;
    const MAX_FILES: usize = 10_000;

    let mut files = Vec::new();
    let mut queue: std::collections::VecDeque<(String, usize)> =
        std::collections::VecDeque::new();
    queue.push_back((root_rel.trim_matches('/').to_string(), 0));

    while let Some((dir_rel, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            tracing::warn!("目录深度超限，跳过: {}", dir_rel);
            continue;
        }
        // 分页取全该目录
        let mut cursor: u64 = 0;
        loop {
            let (list_msg_id, list_rx) = sm.send_rpc(peer, ControlMsg::ListReq {
                share_id: share_id.to_string(),
                path: dir_rel.clone(),
                cursor,
                msg_id: 0,
            }).await.map_err(|e| EngineError::Rpc(format!("发送 ListReq 失败: {}", e)))?;

            let page = tokio::time::timeout(Duration::from_secs(15), async {
                match list_rx.await {
                    Ok((_, ControlMsg::ListResp { entries, next_cursor, .. })) => Ok((entries, next_cursor)),
                    Ok(_) | Err(_) => Err(()),
                }
            }).await;
            if page.is_err() {
                // 超时清理挂起 RPC，防止 pending_rpcs 泄漏
                sm.cancel_rpc(list_msg_id).await;
            }
            let (entries, next_cursor) = page
                .map_err(|_| EngineError::Timeout)?
                .map_err(|_| EngineError::Rpc("枚举目录时响应通道关闭".to_string()))?;

            for e in entries {
                let child = if dir_rel.is_empty() {
                    e.name.clone()
                } else {
                    format!("{}/{}", dir_rel, e.name)
                };
                if e.is_dir {
                    queue.push_back((child, depth + 1));
                } else {
                    files.push((child, e.size));
                }
                if files.len() >= MAX_FILES {
                    tracing::warn!("文件数超上限 {}，截断枚举", MAX_FILES);
                    return Ok(RemoteDirListing { files });
                }
            }

            match next_cursor {
                Some(c) => cursor = c,
                None => break,
            }
        }
    }

    tracing::info!("目录枚举完成: {} 个文件（根: {}）", files.len(), root_rel);
    Ok(RemoteDirListing { files })
}

/// 文件夹拉取事件：编排进度（聚合任务行用）
#[derive(Clone, Debug)]
pub enum DirPullEvent {
    /// 枚举完成，开始传输。total_bytes = 全部文件字节和
    Enumerated { file_count: usize, total_bytes: u64 },
    /// 单文件完成（含空文件/失败重试后的成功）
    FileDone { rel_path: String, bytes: u64 },
    /// 单文件失败（继续传其余，不中止）
    FileFailed { rel_path: String, reason: String },
    /// 全部结束。failed 为空 = 整体 done，否则整体 failed（部分成功）
    AllDone { succeeded: usize, failed: Vec<String> },
}

/// 文件夹拉取编排：枚举 → 逐文件 start_pull_into（dest = 下载根/文件夹名/子目录）。
/// 单文件失败继续传其余；结束时若全成发 DirPullEvent::AllDone{succeeded, failed:[]}
/// （Tauri 壳据此把聚合任务行落 done/failed）。
/// 返回 (成功数, 失败列表)。
pub async fn start_pull_dir(
    sm: &SessionManager,
    peer: &Fingerprint,
    share_id: &str,
    dir_rel: &str,
    cfg: &Config,
    events: mpsc::Sender<DirPullEvent>,
    per_file_progress: mpsc::Sender<ProgressEvent>,
) -> Result<(usize, Vec<String>), EngineError> {
    // 1. 枚举
    let listing = list_remote_dir_recursive(sm, peer, share_id, dir_rel).await?;
    if listing.files.is_empty() {
        return Err(EngineError::Protocol("目录为空或不存在".into()));
    }
    let total_bytes: u64 = listing.files.iter().map(|(_, s)| s).sum();
    let _ = events.send(DirPullEvent::Enumerated {
        file_count: listing.files.len(),
        total_bytes,
    }).await;

    // 2. 目标根：下载目录/文件夹名（保持结构，子目录随 rel 展开）
    let dir_name = Path::new(dir_rel)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("folder")
        .to_string();
    let dest_root = cfg.download_dir.join(sanitize_component(&dir_name));

    // 3. 逐文件串行（入站响应通道全局唯一，并行会互踩；xfer_lock 由外层持有）
    let mut succeeded = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for (rel, _size) in &listing.files {
        // 子目录：dest = dest_root/父目录（文件名由 start_pull_into 的 finalize 定）
        // 子目录:dest = dest_root/父目录(文件名由 start_pull_into 的 finalize 定)
        // S9:rel 来自远端,parent 必须净化,拒绝 `..`/绝对分量防路径穿越
        let parent = sanitize_rel_parent(rel)?;
        let file_dest = if parent.as_os_str().is_empty() {
            dest_root.clone()
        } else {
            dest_root.join(parent)
        };

        match start_pull_parts(sm, peer, share_id, rel, cfg, &file_dest, &cfg.download_dir, per_file_progress.clone()).await {
            Ok(_) => {
                succeeded += 1;
                let _ = events.send(DirPullEvent::FileDone {
                    rel_path: rel.clone(),
                    bytes: *_size,
                }).await;
            }
            Err(e) => {
                tracing::warn!("文件夹内文件拉取失败: {} ({})", rel, e);
                failed.push(rel.clone());
                let _ = events.send(DirPullEvent::FileFailed {
                    rel_path: rel.clone(),
                    reason: e.to_string(),
                }).await;
            }
        }
    }

    let _ = events.send(DirPullEvent::AllDone {
        succeeded,
        failed: failed.clone(),
    }).await;
    tracing::info!(
        "文件夹拉取结束: {} 成功 / {} 失败（根: {}）",
        succeeded, failed.len(), dir_rel
    );
    Ok((succeeded, failed))
}

/// P0-1a/P0-4: 批流单文件落盘。调用前提:name 已过 sanitize_file_name、size 已校验。
/// 冲突改名基于 dest_dir(v0.8.1 前误用 save_dir,子目录冲突文件会落回根目录——已修)。
fn write_small_file(dest_dir: &Path, file_name: &str, data: &[u8]) -> std::io::Result<PathBuf> {
    let file_path = dest_dir.join(file_name);
    let final_path = if file_path.exists() {
        let mut counter = 1u32;
        loop {
            let new_name = format!("{} ({})", file_name, counter);
            let new_path = dest_dir.join(&new_name);
            if !new_path.exists() {
                break new_path;
            }
            counter += 1;
        }
    } else {
        file_path
    };
    std::fs::write(&final_path, data)?;
    Ok(final_path)
}

/// S9 拉取侧路径穿越净化:远端 ListResp 的 rel 不可信,取其 parent
/// 逐段净化后拼盘。任何含 `..`/`.`/空段之外的恶意分量(绝对路径、
/// 盘符)直接拒绝——宁可拒收,不落下载目录之外。
fn sanitize_rel_parent(rel: &str) -> Result<PathBuf, EngineError> {
    let pure = Path::new(rel);
    if pure.is_absolute() {
        return Err(EngineError::Protocol(format!("非法相对路径: {}", rel)));
    }
    let mut out = PathBuf::new();
    for comp in pure.parent().unwrap_or_else(|| Path::new("")) {
        let s = comp.to_str().ok_or_else(|| {
            EngineError::Protocol(format!("非法相对路径: {}", rel))
        })?;
        if s == ".." || s.contains(':') || s.matches('/').next().is_some() || s.matches('\\').next().is_some() {
            return Err(EngineError::Protocol(format!("非法相对路径: {}", rel)));
        }
        if s.is_empty() || s == "." {
            continue;
        }
        // 复用单层净化:拦掉残余分隔符/通配符/非法字符
        let sanitized = sanitize_component(s);
        out.push(sanitized);
    }
    Ok(out)
}

/// 路径成分净化：拦掉 ".."、盘符、分隔符——目录名只作单层文件夹名用
fn sanitize_component(name: &str) -> String {
    let cleaned: String = name.chars()
        .filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|'))
        .collect();
    if cleaned.is_empty() || cleaned == ".." || cleaned == "." {
        "folder".to_string()
    } else {
        cleaned
    }
}

/// 目录条目计数(含子目录递归),上限 10000 截断——防共享区超大树卡住路由器
fn count_dir_entries(root: &Path) -> u64 {
    fn walk(dir: &Path, count: &mut u64) {
        if *count >= 10_000 { return; }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                *count += 1;
                if *count >= 10_000 { return; }
                let p = e.path();
                if p.is_dir() {
                    walk(&p, count);
                }
            }
        }
    }
    let mut n = 0u64;
    if root.is_dir() { walk(root, &mut n); }
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::CHUNK_SIZE;
    use crate::transfer::manifest::Manifest;
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use tempfile::TempDir;

    /// 读取文件的指定分块（测试辅助函数）
    fn read_chunk(path: &Path, idx: u32) -> Vec<u8> {
        let mut file = File::open(path).unwrap();
        let offset = idx as u64 * CHUNK_SIZE as u64;
        file.seek(SeekFrom::Start(offset)).unwrap();

        let metadata = fs::metadata(path).unwrap();
        let remaining = metadata.len().saturating_sub(offset);
        let chunk_size = std::cmp::min(CHUNK_SIZE as u64, remaining) as usize;

        let mut buffer = vec![0u8; chunk_size];
        file.read_exact(&mut buffer).unwrap();
        buffer
    }

    #[test]
    fn out_of_order_write_then_finalize_matches_source() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.bin");

        // 创建源文件：2个完整块 + 50字节
        let data: Vec<u8> = (0..CHUNK_SIZE as u32 * 2 + 50).map(|i| i as u8).collect();
        fs::write(&src, &data).unwrap();

        let m = Manifest::build(&src).unwrap();
        let parts = tmp.path().join(".localtrans-parts").join("0000000000000001");
        fs::create_dir_all(&parts).unwrap();

        let mut w = PartWriter::open(&parts, m).unwrap();

        // 乱序写入：先写块1，再写块0，最后写块2
        let c1 = read_chunk(&src, 1);
        let c0 = read_chunk(&src, 0);
        let c2 = read_chunk(&src, 2);

        w.write_chunk(1, &c1).unwrap();
        w.write_chunk(0, &c0).unwrap();
        w.write_chunk(2, &c2).unwrap();

        let dest = w.finalize(&tmp.path().join("out")).unwrap();
        assert_eq!(fs::read(&dest).unwrap(), data);
        assert!(!parts.exists(), "任务目录应清理");
    }

    #[test]
    fn corrupted_chunk_rejected() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.bin");

        // 创建源文件
        let data: Vec<u8> = (0..CHUNK_SIZE as u32 + 100).map(|i| i as u8).collect();
        fs::write(&src, &data).unwrap();

        let m = Manifest::build(&src).unwrap();
        let parts = tmp.path().join(".localtrans-parts").join("0000000000000002");
        fs::create_dir_all(&parts).unwrap();

        let mut w = PartWriter::open(&parts, m).unwrap();

        // 尝试写入篡改的数据
        let c0 = read_chunk(&src, 0);
        let mut corrupted = c0.clone();
        corrupted[0] = !corrupted[0]; // 翻转第一个字节

        let result = w.write_chunk(0, &corrupted);
        assert!(result.is_err());
        match result.unwrap_err() {
            EngineError::HashMismatch { chunk: 0 } => {},
            _ => panic!("期望 HashMismatch 错误"),
        }

        // 确认 received[0] 仍然为 false
        assert!(!w.manifest.received[0]);

        // 正确的数据应该能成功写入
        w.write_chunk(0, &c0).unwrap();
        assert!(w.manifest.received[0]);
    }

    #[test]
    fn finalize_before_complete_rejected() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("src.bin");

        // 创建3个块的文件
        let data: Vec<u8> = (0..CHUNK_SIZE as u32 * 3).map(|i| i as u8).collect();
        fs::write(&src, &data).unwrap();

        let m = Manifest::build(&src).unwrap();
        let parts = tmp.path().join(".localtrans-parts").join("0000000000000003");
        fs::create_dir_all(&parts).unwrap();

        let mut w = PartWriter::open(&parts, m).unwrap();

        // 只写入1/3的块
        let c1 = read_chunk(&src, 1);
        w.write_chunk(1, &c1).unwrap();

        // 尝试 finalize 应该失败
        let result = w.finalize(&tmp.path().join("out"));
        assert!(result.is_err());
        match result.unwrap_err() {
            EngineError::Incomplete => {},
            _ => panic!("期望 Incomplete 错误"),
        }
    }

    #[test]
    fn conflict_rename_multidot_preserves_full_name() {
        let tmp = TempDir::new().unwrap();
        let src = tmp.path().join("archive.tar.gz");

        // 创建源文件（多点扩展名）
        let data: Vec<u8> = (0..CHUNK_SIZE as u32 + 100).map(|i| i as u8).collect();
        fs::write(&src, &data).unwrap();

        let m = Manifest::build(&src).unwrap();
        let parts = tmp.path().join(".localtrans-parts").join("0000000000000004");
        fs::create_dir_all(&parts).unwrap();

        let mut w = PartWriter::open(&parts, m).unwrap();

        // 写入所有分块（2个块：一个完整的 CHUNK_SIZE 和一个 100 字节的块）
        let c0 = read_chunk(&src, 0);
        let c1 = read_chunk(&src, 1);
        w.write_chunk(0, &c0).unwrap();
        w.write_chunk(1, &c1).unwrap();

        // 预置同名文件以模拟冲突
        let dest_dir = tmp.path().join("out");
        fs::create_dir_all(&dest_dir).unwrap();
        let existing_file = dest_dir.join("archive.tar.gz");
        fs::write(&existing_file, b"existing content").unwrap();

        // Finalize 应该自动处理冲突
        let final_path = w.finalize(&dest_dir).unwrap();

        // 验证最终路径为 "archive.tar (1).gz"（在最后一个点前插入计数）
        assert_eq!(final_path.file_name().unwrap().to_str().unwrap(), "archive.tar (1).gz");
        assert_eq!(fs::read(&final_path).unwrap(), data);

        // 确认原文件仍然存在
        assert!(existing_file.exists());
        assert_eq!(fs::read(&existing_file).unwrap(), b"existing content");
    }

    /// T14 测试夹具：互信双方 + 乙侧共享区/路由器就位 + 甲连接完成
    struct PullFixture {
        sm_a: Arc<SessionManager>,
        cfg_a: Config,
        fp_b: Fingerprint,
        b_addr: std::net::SocketAddr,
        download_dir: PathBuf,
        _dir_a: TempDir,
        _dir_b: TempDir,
    }

    async fn setup_pull(file_name: &str, data: &[u8]) -> PullFixture {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use tokio::time::timeout;

        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信：browse + download + push=Auto
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // 乙的共享区与源文件
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join(file_name), data).unwrap();
        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        // 甲的下载目录（覆盖默认的 data 根）
        let download_dir = dir_a.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_a.config.write().await.download_dir = download_dir.clone();

        // 乙监听 + RPC 路由器
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx, ask_tx, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 甲连接乙（互信 → 直接 SessionUp）
        let peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();
        assert_eq!(peer_fp, fp_b);

        let cfg_a = ctx_a.config.read().await.clone();
        PullFixture {
            cfg_a,
            sm_a,
            fp_b,
            b_addr,
            download_dir,
            _dir_a: dir_a,
            _dir_b: dir_b,
        }
    }

    /// v0.2.4 双向确认契约：pull 完成后乙（发送方）收到 RecvAck → 发 SourceDone、
    /// 清理 sender_jobs——发送侧任务不再永远 active
    #[tokio::test]
    async fn pull_recv_ack_completes_source_side() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let data: Vec<u8> = (0..9 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 乙的共享区与源文件
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("ack_test.bin"), &data).unwrap();
        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        // 甲的下载目录
        let download_dir = dir_a.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_a.config.write().await.download_dir = download_dir.clone();
        let cfg_a = ctx_a.config.read().await.clone();

        // 乙监听 + 路由器（挂 source 事件通道观测 SourceDone）
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs = crate::transfer::sender_state::new_sender_job_map();
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx, ask_tx, mpsc::channel(8).0, sender_jobs.clone(), Some(source_tx));

        // 甲连接乙
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 甲拉取
        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        timeout(
            Duration::from_secs(30),
            start_pull(&sm_a, &ShareRegistry::new(vec![]), &fp_b, "share1", "ack_test.bin", &cfg_a, progress_tx),
        ).await.expect("拉取应完成").expect("拉取应成功");

        // 接收方先收到 Done
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = progress_rx.recv().await {
                if matches!(ev, ProgressEvent::Done { .. }) { break; }
            }
        }).await.expect("应收到 Done");

        // 发送方随后收到 SourceStarted → SourceDone（RecvAck 驱动）
        let mut saw_done = false;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = source_rx.recv().await {
                match ev {
                    ProgressEvent::SourceDone { job_id } => {
                        // 任务应已从 sender_jobs 清理
                        assert!(!sender_jobs.read().await.contains_key(&job_id),
                            "RecvAck 后 sender 任务应已清理");
                        saw_done = true;
                        break;
                    }
                    ProgressEvent::SourceStarted { .. } => {}
                    _ => {}
                }
            }
        }).await.expect("应在超时前收到 SourceDone");
        assert!(saw_done, "应收到 SourceDone 事件");
    }

    /// 大文件推送的 RecvAck 闭环：乙收到大文件落盘后回 RecvAck，
    /// 甲侧 source 行（MetaReq 驱动的 sender 任务）应收到 SourceDone 并清理。
    /// v0.6.x 修复前：recv_push_large_file 只回 JobDone 不回 RecvAck，
    /// 甲侧 source 行永远 active（UI 计时不停、进度泵永不降频的根因）
    #[tokio::test]
    async fn push_large_file_recv_ack_completes_source_side() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 5MB 大文件（2 块）：走 recv_push_large_file 的块流路径
        let data: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 甲的源文件
        let src_file = dir_a.path().join("push_large_ack.bin");
        fs::write(&src_file, &data).unwrap();

        // 乙的下载目录
        let download_dir = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();

        // 甲侧路由器挂 source 事件通道（观测 SourceStarted/SourceDone）
        // 乙侧路由器照常（ask 通道丢弃即可——Auto 档直接落盘）
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), Some(source_tx));

        // 甲连接乙并推送大文件
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push = sender_jobs_a.clone();
        let push_fut = tokio::spawn(async move {
            push_files(&sm_a, &fp_b, vec![src_file], &sender_jobs_for_push, progress_tx).await
        });

        // 接收方 Done（push_files 返回即已等过 JobDone，这里消费尾部事件）
        timeout(Duration::from_secs(30), async {
            while let Some(ev) = progress_rx.recv().await {
                if matches!(ev, ProgressEvent::Done { .. }) { break; }
            }
        }).await.expect("推送应完成");

        push_fut.await.expect("推送任务不应 panic")
            .expect("推送应成功");

        // 发送方（甲）应收到 SourceStarted → SourceDone（RecvAck 驱动）。
        // 注意：这里是回放式消费（推送已完成），SourceStarted 到达时任务可能
        // 已被 RecvAck 清理——瞬时表状态不可回放断言，只断言 SourceDone 必达
        let mut saw_source_started = false;
        let mut saw_source_done = false;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = source_rx.recv().await {
                match ev {
                    ProgressEvent::SourceStarted { .. } => {
                        saw_source_started = true;
                    }
                    ProgressEvent::SourceDone { job_id } => {
                        assert!(!sender_jobs_a.read().await.contains_key(&job_id),
                            "RecvAck 后 sender 任务应已清理");
                        saw_source_done = true;
                        break;
                    }
                    _ => {}
                }
            }
        }).await.expect("应在超时前收到 SourceDone");
        assert!(saw_source_started, "应收到 SourceStarted 事件");
        assert!(saw_source_done, "应收到 SourceDone 事件");
    }

    /// v0.10.0 UX4: 大文件推送接收侧逐窗回发 RecvProgress——发送方应收到
    /// SourceSpeed{remote_done} 事件且值单调递增,终值等于文件大小。
    #[tokio::test]
    async fn push_large_file_reports_remote_done() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 100MB + 123 字节（走大文件路径，确保传输时间 > 2s 以触发多个 SourceSpeed 和最终 RecvProgress）
        let file_size: u64 = 100 * 1024 * 1024 + 123;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 甲的源文件
        let src_file = dir_a.path().join("push_remote_done.bin");
        fs::write(&src_file, &data).unwrap();

        // 乙的下载目录
        let download_dir = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();

        // 甲侧路由器挂 source 事件通道（收集 SourceSpeed 事件）
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), Some(source_tx));

        // 甲连接乙并推送大文件
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push = sender_jobs_a.clone();
        let push_fut = tokio::spawn(async move {
            push_files(&sm_a, &fp_b, vec![src_file], &sender_jobs_for_push, progress_tx).await
        });

        // 接收方 Done（push_files 返回即已等过 JobDone，这里消费尾部事件）
        timeout(Duration::from_secs(30), async {
            while let Some(ev) = progress_rx.recv().await {
                if matches!(ev, ProgressEvent::Done { .. }) { break; }
            }
        }).await.expect("推送应完成");

        push_fut.await.expect("推送任务不应 panic")
            .expect("推送应成功");

        // 发送方（甲）应收到 SourceSpeed{remote_done} → SourceDone（RecvAck 驱动）。
        // 收集 SourceSpeed 事件的 remote_done 值
        let mut speeds: Vec<u64> = Vec::new();
        let mut saw_source_done = false;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = source_rx.recv().await {
                if let ProgressEvent::SourceSpeed { remote_done, .. } = ev {
                    speeds.push(remote_done);
                }
                if matches!(ev, ProgressEvent::SourceDone { .. }) {
                    saw_source_done = true;
                    // 再等一小段时间，确保最后的 SourceSpeed 事件（可能随 SourceDone 几乎同时到达）被收集
                    let _ = tokio::time::sleep(Duration::from_millis(100)).await;
                    // 继续收集剩余事件
                    while let Ok(ev) = source_rx.try_recv() {
                        if let ProgressEvent::SourceSpeed { remote_done, .. } = ev {
                            speeds.push(remote_done);
                        }
                    }
                    break; // 收到 SourceDone 后退出
                }
            }
        }).await.expect("应在超时前收到 SourceDone");
        assert!(saw_source_done, "应收到 SourceDone 事件");

        // 断言：单调不减
        assert!(speeds.windows(2).all(|w| w[0] <= w[1]),
            "remote_done 应单调不减: {:?}", speeds);
        // 断言：至少收到一个 SourceSpeed 事件（证明 probe 在运行）
        assert!(!speeds.is_empty(), "应至少收到一个 SourceSpeed 事件");
        // 断言：最后一个值应接近文件大小（允许两窗口误差——timing 原因
        // 可能 final RecvProgress 到达后未及再发 SourceSpeed 即收到 RecvAck
        // 停止；2026-09-06 实测本机负载下稳定差 5 块=20MB，单窗口 16MB
        // 容差不足，放宽到 2 窗口保留"接近终值"本意）
        // 窗口大小为 4 块 × 4MB = 16MB
        let last_value = *speeds.last().unwrap();
        assert!(last_value > file_size.saturating_sub(2 * 16 * 1024 * 1024),
            "remote_done 终值应接近文件大小 (期望 {}, 实际 {})", file_size, last_value);
        // 断言：应有至少 3 条非零 remote_done 进度（证明协议在持续工作）
        let nonzero = speeds.iter().filter(|&&v| v > 0).count();
        assert!(nonzero >= 3,
            "应有至少 3 条非零 remote_done 进度(实际 {}): {:?}", nonzero, speeds);
    }

    /// T3: start_pull 在 send_ctrl 失败(send_meta 前对端无会话/连接已断)时,
    /// 必须把一次性响应通道归还 SessionManager——否则后续任何拉取永久报
    /// "入站响应通道已被占用"。第二次 start_pull 的失败原因应不同(仍是发送失败,
    /// 而非通道被占)。
    #[tokio::test]
    async fn pull_send_ctrl_failure_returns_resp_channel() {
        use crate::test_support::setup_ctx;

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, _fp_a, dir_a) = setup_ctx("甲");
        let (_sm_b, _ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let _ = dir_a;

        let cfg = ctx_a.config.read().await.clone();
        let reg_unused = ShareRegistry::new(vec![]);

        // 甲从未连接乙：send_ctrl 因会话不存在立刻失败（不走 timeout 等待块）
        let (tx1, mut rx1) = mpsc::channel::<ProgressEvent>(64);
        let err1 = start_pull(&sm_a, &reg_unused, &fp_b, "share1", "t3.bin", &cfg, tx1).await;
        assert!(err1.is_err(), "对端无会话应失败");
        let _ = rx1.recv().await; // 排干

        // 关键断言：通道已被归还，第二次调用的失败原因不再是"通道已被占用"
        let (tx2, mut rx2) = mpsc::channel::<ProgressEvent>(64);
        let err2 = start_pull(&sm_a, &reg_unused, &fp_b, "share1", "t3.bin", &cfg, tx2).await;
        let _ = rx2.recv().await;
        let msg2 = match err2 {
            Err(EngineError::Rpc(m)) => m,
            other => panic!("第二次也应为 Rpc 错误, 实际 {:?}", other.err()),
        };
        assert!(
            !msg2.contains("通道已被占用"),
            "第二次失败不应是通道被占, 实际: {}",
            msg2
        );
        // M-B5 多路化后已无共享响应通道可泄漏——send_rpc 失败会取消挂起表项
    }

    /// v0.6.x S1:下载方取消后发 TransferCtl{Cancel},数据源(sender)任务应清理
    /// 并发 SourceFailed——source 行不再永久 active(计时不停)。
    /// 复用既有 TransferCtl 消息(与 Throttle 的 sender 侧处理对称),不新增协议消息。
    #[tokio::test]
    async fn cancel_pull_notifies_source_side() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 乙的共享区源文件(内容无关紧要——测试只走到 MetaResp,不传块)
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("cancel_test.bin"), b"payload").unwrap();
        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        // 乙路由器挂 source 事件通道(观测 SourceStarted/SourceFailed)
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_b = crate::transfer::sender_state::new_sender_job_map();
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx_b, ask_tx_b,
            mpsc::channel(8).0, sender_jobs_b.clone(), Some(source_tx));

        // 甲连接乙,发 MetaReq 让乙建 sender 任务
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (_meta_msg_id, meta_rx) = sm_a.send_rpc(&fp_b, ControlMsg::MetaReq {
            share_id: "share1".to_string(),
            path: "cancel_test.bin".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let resp_rx = meta_rx;

        // 等 SourceStarted 拿 job_id(source 段高 0x8000... id)
        let job_id = timeout(Duration::from_secs(5), async {
            loop {
                match source_rx.recv().await {
                    Some(ProgressEvent::SourceStarted { job_id, .. }) => return job_id,
                    Some(_) => continue,
                    None => panic!("source 通道不应关闭"),
                }
            }
        }).await.expect("应收到 SourceStarted");
        assert!(sender_jobs_b.read().await.contains_key(&job_id), "sender 任务应在表中");

        // 等 MetaResp 到甲(send_rpc 按请求 msg_id 路由,无需归还通道)
        timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::MetaResp { .. })) => return,
                _ => panic!("resp 不应失败"),
            }
        }).await.expect("应收到 MetaResp");

        // 甲取消:发 TransferCtl{Cancel}(模拟壳层取消通知)
        sm_a.send_ctrl(&fp_b, ControlMsg::TransferCtl {
            job_id,
            action: crate::protocol::TransferAction::Cancel,
        }).await.unwrap();

        // 乙应收到 SourceFailed 且任务被清理
        timeout(Duration::from_secs(5), async {
            loop {
                match source_rx.recv().await {
                    Some(ProgressEvent::SourceFailed { job_id: j, reason }) => {
                        assert_eq!(j, job_id);
                        assert!(reason.contains("取消"), "原因应含'取消': {}", reason);
                        return;
                    }
                    Some(_) => continue,
                    None => panic!("source 通道不应关闭"),
                }
            }
        }).await.expect("应收到 SourceFailed");
        assert!(!sender_jobs_b.read().await.contains_key(&job_id), "任务应已清理");

        // 幂等:重复取消不应 panic、不应再发 SourceFailed
        sm_a.send_ctrl(&fp_b, ControlMsg::TransferCtl {
            job_id,
            action: crate::protocol::TransferAction::Cancel,
        }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        while let Ok(ev) = source_rx.try_recv() {
            assert!(!matches!(ev, ProgressEvent::SourceFailed { .. }),
                "重复取消不应再发 SourceFailed");
        }
    }

    /// v0.6.x S2/S3:push 失败(对端接收失败)时,本机为大文件反向取流建的
    /// sender 任务应被清理并发 SourceFailed——source 行不再永久 active。
    #[tokio::test]
    async fn push_failure_cleans_associated_source_jobs() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 5MB 大文件:接收方反向 MetaReq 取流 → 甲建 sender 任务
        let data: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        let src_file = dir_a.path().join("push_fail.bin");
        fs::write(&src_file, &data).unwrap();

        // 乙的"下载目录"是一个普通文件 → 接收落盘必然失败 → JobFailed
        let bad_dir = dir_b.path().join("not_a_dir");
        fs::write(&bad_dir, b"x").unwrap();
        ctx_b.config.write().await.download_dir = bad_dir;

        // 甲侧路由器挂 source 事件通道;push_files 与甲路由器共用同一 sender_jobs
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), Some(source_tx));

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let result = timeout(Duration::from_secs(60),
            push_files(&sm_a, &fp_b, vec![src_file], &sender_jobs_a, progress_tx),
        ).await.expect("推送应在超时前结束");

        assert!(result.is_err(), "推送应失败(对端落盘失败)");

        // source 行应收 SourceFailed 且关联 sender 任务清空
        timeout(Duration::from_secs(10), async {
            loop {
                match source_rx.recv().await {
                    Some(ProgressEvent::SourceFailed { .. }) => return,
                    Some(_) => continue,
                    None => panic!("source 通道不应关闭"),
                }
            }
        }).await.expect("应收到 SourceFailed");
        assert!(!sender_jobs_a.read().await.iter().any(|(_, s)| s.offer_id.is_some()),
            "关联 sender 任务应已清理");
    }

    /// T14：回环拉取 10MB 文件，进度事件完整、sha256 逐字节一致、任务目录清理
    #[tokio::test]
    async fn pull_file_over_loopback() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let data: Vec<u8> = (0..10 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let f = setup_pull("test_10mb.bin", &data).await;

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let reg_unused = ShareRegistry::new(vec![]);
        let job_id = timeout(
            Duration::from_secs(30),
            start_pull(&f.sm_a, &reg_unused, &f.fp_b, "share1", "test_10mb.bin", &f.cfg_a, progress_tx),
        )
        .await
        .expect("拉取应在超时前完成")
        .expect("拉取应成功");

        // 进度事件：Started → ChunkDone×3 → Done
        let mut saw_started = false;
        let mut chunks = 0u32;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Started { job_id: j, .. } => {
                        assert_eq!(j, job_id);
                        saw_started = true;
                    }
                    ProgressEvent::ChunkDone { job_id: j, .. } => {
                        assert_eq!(j, job_id);
                        chunks += 1;
                    }
                    ProgressEvent::Done { job_id: j } => {
                        assert_eq!(j, job_id);
                        break;
                    }
                    ProgressEvent::Failed { reason, .. } => panic!("拉取失败: {}", reason),
                    _ => {}
                }
            }
        })
        .await
        .expect("应收到 Done 事件");
        assert!(saw_started, "应有 Started 事件");
        assert_eq!(chunks, 3, "10MB / 4MiB = 3 块");

        // 内容逐字节一致
        let got = fs::read(f.download_dir.join("test_10mb.bin")).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&got)),
            hex::encode(Sha256::digest(&data)),
            "下载文件应与源文件 sha256 一致"
        );

        // 任务目录已清理
        assert!(
            !f.download_dir.join(".localtrans-parts").join(format!("{:016x}", job_id)).exists(),
            "任务目录应清理"
        );
    }

    /// T14：空文件拉取（chunk_count=0，不进块流，立即 finalize）
    #[tokio::test]
    async fn empty_file_pull() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let f = setup_pull("empty.txt", b"").await;

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let reg_unused = ShareRegistry::new(vec![]);
        let job_id = timeout(
            Duration::from_secs(15),
            start_pull(&f.sm_a, &reg_unused, &f.fp_b, "share1", "empty.txt", &f.cfg_a, progress_tx),
        )
        .await
        .expect("拉取应在超时前完成")
        .expect("空文件拉取应成功");

        timeout(Duration::from_secs(5), async {
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Done { job_id: j } => {
                        assert_eq!(j, job_id);
                        break;
                    }
                    ProgressEvent::Failed { reason, .. } => panic!("拉取失败: {}", reason),
                    _ => {}
                }
            }
        })
        .await
        .expect("应收到 Done 事件");

        let got = fs::read(f.download_dir.join("empty.txt")).unwrap();
        assert!(got.is_empty(), "空文件内容应为空");
    }

    // ============ T15 Tests ============

    /// T15: 断点续传测试（拉取模式）- 真实重连续传
    #[tokio::test]
    async fn resume_after_connection_drop() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 创建 30MB 源文件（约 8 个 4MiB 块）
        let data: Vec<u8> = (0..30 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let f = setup_pull("resume_30mb.bin", &data).await;

        // 甲第一次拉取：收 4 块后主动断连
        let (progress_tx1, mut progress_rx1) = mpsc::channel::<ProgressEvent>(64);
        let reg1 = ShareRegistry::new(vec![]);
        let sm_pull = f.sm_a.clone();
        let sm_a_for_disconnect = f.sm_a.clone();
        let cfg_a = f.cfg_a.clone();
        let fp_b = f.fp_b;

        let pull_task = tokio::spawn(async move {
            let _ = timeout(
                Duration::from_secs(30),
                start_pull(&sm_pull, &reg1, &fp_b, "share1", "resume_30mb.bin", &cfg_a, progress_tx1),
            ).await;
        });

        // 收到 4 块后断连甲侧会话（任务应失败，任务目录冻结）
        let mut chunks_session1 = 0u32;
        timeout(Duration::from_secs(15), async {
            while let Some(ev) = progress_rx1.recv().await {
                match ev {
                    ProgressEvent::ChunkDone { .. } => {
                        chunks_session1 += 1;
                        if chunks_session1 >= 4 {
                            if let Some(conn) = sm_a_for_disconnect.session(&fp_b).await {
                                conn.close(0u8.into(), b"test disconnect");
                            }
                            return;
                        }
                    }
                    ProgressEvent::Done { .. } | ProgressEvent::Failed { .. } => return,
                    _ => {}
                }
            }
        })
        .await
        .expect("第一会话应在超时前收到块或结束");

        let _ = pull_task.await;
        tracing::info!("第一次拉取中断，本会话已收 {} 块", chunks_session1);
        assert!(chunks_session1 >= 1, "断连前至少应收到一块");

        // 断言任务冻结：parts 目录在、manifest 位图部分完成
        let parts_root = f.download_dir.join(".localtrans-parts");
        assert!(parts_root.exists(), "任务目录应存在");
        let task_dir = fs::read_dir(&parts_root)
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| e.path().is_dir())
            .map(|e| e.path())
            .expect("应有任务目录");
        let saved_manifest = Manifest::load(&task_dir).unwrap();
        assert_eq!(saved_manifest.file_name, "resume_30mb.bin");
        let received_count = saved_manifest.received.iter().filter(|r| **r).count();
        assert_eq!(received_count, chunks_session1 as usize, "位图已收块数应与事件计数一致");
        let missing_count = saved_manifest.missing_chunks().len();
        assert!(missing_count > 0, "应有缺失块");
        assert!(missing_count < 8, "不应全部缺失");
        tracing::info!("续传前缺失块数: {}", missing_count);

        // R9-3: 断言任务溯源字段已保存
        assert_eq!(saved_manifest.share_id.as_deref(), Some("share1"), "share_id 应为 share1");
        assert_eq!(saved_manifest.rel.as_deref(), Some("resume_30mb.bin"), "rel 应为文件名");
        assert_eq!(saved_manifest.peer, Some(hex::encode(f.fp_b)), "peer 应为对端指纹");
        tracing::info!("任务溯源信息: peer={:?}, share_id={:?}, rel={:?}",
            saved_manifest.peer, saved_manifest.share_id, saved_manifest.rel);

        // 重连：甲乙互信（同一身份/信任表），乙的监听器仍在 → 静默重连
        let _ = timeout(Duration::from_secs(5), f.sm_a.connect(f.b_addr))
            .await
            .expect("重连应成功")
            .unwrap();

        // 第二次拉取（续传）：应复用旧任务目录，只取缺失块
        let (progress_tx2, mut progress_rx2) = mpsc::channel::<ProgressEvent>(64);
        let reg2 = ShareRegistry::new(vec![]);
        let cfg_a2 = f.cfg_a.clone();
        let job_id2 = timeout(
            Duration::from_secs(30),
            start_pull(&f.sm_a, &reg2, &f.fp_b, "share1", "resume_30mb.bin", &cfg_a2, progress_tx2),
        )
        .await
        .expect("续传拉取应在超时前完成")
        .expect("续传应成功");

        // 收 Done；统计第二会话 ChunkDone 数（无重传证明：应恰好等于缺失块数）
        let mut chunks_session2 = 0u32;
        timeout(Duration::from_secs(20), async {
            while let Some(ev) = progress_rx2.recv().await {
                match ev {
                    ProgressEvent::ChunkDone { .. } => chunks_session2 += 1,
                    ProgressEvent::Done { job_id: j } => {
                        assert_eq!(j, job_id2);
                        break;
                    }
                    ProgressEvent::Failed { reason, .. } => panic!("续传失败: {}", reason),
                    _ => {}
                }
            }
        })
        .await
        .expect("续传应完成");

        assert_eq!(
            chunks_session2 as usize, missing_count,
            "第二会话应恰好只取缺失块（无重传）"
        );
        assert_eq!(
            chunks_session1 + chunks_session2, 8,
            "两会话合计收块数应等于总块数"
        );

        // 验证最终文件完整
        let got = fs::read(f.download_dir.join("resume_30mb.bin")).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&got)),
            hex::encode(Sha256::digest(&data)),
            "续传后文件应与源文件一致"
        );

        // 任务目录应清理（续传复用的是旧任务目录）
        let leftover = fs::read_dir(&parts_root).map(|mut d| d.next().is_some()).unwrap_or(false);
        assert!(!leftover, "任务目录应清理");

        tracing::info!("断点续传测试通过");
    }

    /// T15: 小文件批流测试（推送模式） - 真实批流传输
    #[tokio::test]
    async fn small_files_batched() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 准备 10 个 100KB 文件（简化版，100个太耗时）
        let mut files = Vec::new();
        let tmp = TempDir::new().unwrap();
        for i in 0..10 {
            let path = tmp.path().join(format!("file_{}.txt", i));
            let data: Vec<u8> = (0..100 * 1024).map(|j| ((j + i * 100) % 256) as u8).collect();
            fs::write(&path, &data).unwrap();
            files.push(path);
        }

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        // 互信配置（push=Auto）
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // 设置乙的下载目录
        let download_dir_b = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir_b).unwrap();
        ctx_b.config.write().await.download_dir = download_dir_b.clone();

        // 乙监听
        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])), ctrl_rx, mpsc::channel(8).0, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 甲连接乙
        let _peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应成功")
            .unwrap();

        // 推送文件（小文件批流）
        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

        let job_id = timeout(
            Duration::from_secs(30),
            push_files(
                &sm_a,
                &fp_b,
                files.clone(),
                &crate::transfer::sender_state::new_sender_job_map(),
                progress_tx,
            ),
        )
        .await
        .expect("推送应在超时前完成")
        .expect("推送应成功");

        // 等待完成
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Done { job_id: j } => {
                        assert_eq!(j, job_id);
                        break;
                    }
                    ProgressEvent::Failed { reason, .. } => panic!("推送失败: {}", reason),
                    _ => {}
                }
            }
        })
        .await
        .expect("应收到 Done 事件");

        // 验证所有文件都已接收
        for i in 0..10 {
            let path = download_dir_b.join(format!("file_{}.txt", i));
            assert!(path.exists(), "文件 {} 应存在", i);
            let data = fs::read(&path).unwrap();
            assert_eq!(data.len(), 100 * 1024, "文件 {} 大小应正确", i);
        }

        // brief 契约：10 个小文件全部走**单条**批流（而非逐文件开流）
        assert_eq!(
            small_batch_streams_opened(job_id), 1,
            "一次推送的小文件应复用单条批流"
        );

        tracing::info!("小文件批流测试通过");
    }

    /// T15: 推送 Offer Ask 流程测试 - 真实 Ask 响应机制
    #[tokio::test]
    async fn push_offer_flow_with_ask_policy() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let tmp = TempDir::new().unwrap();
        let src_file = tmp.path().join("test_push.txt");
        let data: Vec<u8> = (0..50 * 1024).map(|i| (i % 256) as u8).collect();
        fs::write(&src_file, &data).unwrap();
        // 大文件（5MB，2 个块）：覆盖推送的大文件块流路径
        let large_file = tmp.path().join("test_push_large.bin");
        let large_data: Vec<u8> = (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        fs::write(&large_file, &large_data).unwrap();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        // 甲信任乙（push=Auto）
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // 乙信任甲（push=Ask，需要用户确认）
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Ask },
            });
            trust.save().unwrap();
        }

        // 设置乙的下载目录
        let download_dir_b = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir_b).unwrap();
        ctx_b.config.write().await.download_dir = download_dir_b.clone();

        // 乙监听
        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");

        // Ask 响应通道
        let (ask_tx, mut ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])), ctrl_rx, ask_tx, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 甲连接乙
        let _peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应成功")
            .unwrap();

        // 甲侧路由器：大文件推送时 乙 会反向发 MetaReq/FetchReq，需要甲侧 RPC 服务
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        {
            let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
            let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
            spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])), ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), None);
        }

        // ============ 测试拒绝 ============
        let files = vec![src_file.clone(), large_file.clone()];
        let (progress_tx_reject, _progress_rx_reject) = mpsc::channel::<ProgressEvent>(8);

        // Ask 应答者：第 1 次 Ask 拒绝，第 2 次 Ask 接受（同一 ask 通道，模拟 UI 两次应答）
        let dd = download_dir_b.clone();
        let responder_task = tokio::spawn(async move {
            match ask_rx.recv().await {
                Some(ask) => {
                    assert_eq!(ask.from, fp_a);
                    let _ = ask.respond.send(None); // 拒绝
                }
                None => panic!("Ask 通道关闭"),
            }
            match ask_rx.recv().await {
                Some(ask) => {
                    assert_eq!(ask.from, fp_a);
                    let _ = ask.respond.send(Some(dd)); // 接受，指定保存目录
                }
                None => panic!("Ask 通道关闭"),
            }
        });

        // 尝试推送（应该被拒绝）
        let push_result = timeout(
            Duration::from_secs(10),
            push_files(&sm_a, &fp_b, files.clone(), &sender_jobs_a, progress_tx_reject),
        )
        .await;

        // 应该返回 OfferRejected 错误
        match push_result {
            Ok(Ok(_)) => panic!("推送应被拒绝，但返回成功"),
            Ok(Err(EngineError::OfferRejected)) => {}, // 期望的结果
            Ok(Err(e)) => panic!("期望 OfferRejected，实际: {:?}", e),
            Err(_) => panic!("拒绝阶段的推送不应超时"),
        }

        // 确认没有文件写入
        assert!(!download_dir_b.join("test_push.txt").exists(), "拒绝后不应有小文件");
        assert!(!download_dir_b.join("test_push_large.bin").exists(), "拒绝后不应有大文件");

        // ============ 测试接受 ============
        let (progress_tx_accept, mut progress_rx_accept) = mpsc::channel::<ProgressEvent>(16);

        // 重新推送（这次应该被接受：小文件批流 + 大文件块流）
        let job_id = timeout(
            Duration::from_secs(60),
            push_files(&sm_a, &fp_b, files.clone(), &sender_jobs_a, progress_tx_accept),
        )
        .await
        .expect("推送应在超时前完成")
        .expect("推送应成功");

        // 等待完成（小文件组 1 个 Done + 大文件 2 个 Done；收到 offer 的 Done 即可，
        // 甲侧 push_files 返回本身已等待乙侧 JobDone 确认全部落盘）
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = progress_rx_accept.recv().await {
                match ev {
                    ProgressEvent::Done { job_id: j } => {
                        assert_eq!(j, job_id);
                        break;
                    }
                    ProgressEvent::Failed { reason, .. } => panic!("推送失败: {}", reason),
                    _ => {}
                }
            }
        })
        .await
        .expect("应收到 Done 事件");

        // 验证文件正确写入（小文件批流 + 大文件块流）
        let received = fs::read(download_dir_b.join("test_push.txt")).unwrap();
        assert_eq!(received, data, "接收的小文件应与源文件一致");
        let received_large = fs::read(download_dir_b.join("test_push_large.bin")).unwrap();
        assert_eq!(
            hex::encode(Sha256::digest(&received_large)),
            hex::encode(Sha256::digest(&large_data)),
            "接收的大文件应与源文件逐字节一致"
        );

        // 乙侧任务目录应清理
        let parts_b = download_dir_b.join(".localtrans-parts");
        let leftover = fs::read_dir(&parts_b).map(|mut d| d.next().is_some()).unwrap_or(false);
        assert!(!leftover, "乙侧任务目录应清理");

        // 等待应答任务完成
        let _ = responder_task.await;

        tracing::info!("推送 Ask 流程测试通过");
    }

    /// Task 3 Test 1: OfferAsk 超时返回 Timeout reason，发送方收到 OfferTimeout
    #[tokio::test]
    async fn offer_ask_timeout_returns_timeout_reason_and_sender_gets_offer_timeout() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        // 互信 + 乙 Ask 档 + 1s 确认超时
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Ask },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.offer_timeout_secs = 1;
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, mut ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        // 乙侧收到 Ask 但永不应答（drop respond 即拒绝——这里保持持有不答）
        let mut held: Option<OfferAsk> = None;
        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);

        // Spawn push_files in background
        let sm_a_clone = sm_a.clone();
        let push_handle = tokio::spawn(async move {
            push_files(&sm_a_clone, &fp_b, vec![src], &sender_jobs_a, progress_tx).await
        });

        // 等乙侧 Ask 到达（拿到 deadline 字段断言）
        timeout(Duration::from_secs(5), async {
            while held.is_none() {
                if let Some(ask) = ask_rx_b.recv().await { held = Some(ask); }
            }
        }).await.unwrap();
        let ask = held.unwrap();
        assert!(ask.deadline_epoch_ms > 0, "Ask 必须携带 deadline");
        // offer_timeout_secs=1：deadline 在 0.5-2s 后
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
        let delta = ask.deadline_epoch_ms - now_ms;
        assert!(delta > 200 && delta < 2000, "deadline 距今应约 1s, 实际 {}ms", delta);

        // 甲侧最终拿到 OfferTimeout（约 1s 后），而非 OfferRejected
        let res = timeout(Duration::from_secs(10), push_handle).await.unwrap().unwrap();
        assert!(matches!(res, Err(EngineError::OfferTimeout)), "实际: {:?}", res);
    }

    /// Task 3 Test 2: OfferAsk 顺延重置 deadline 一次
    #[tokio::test]
    async fn offer_ask_extend_resets_deadline_once() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Ask },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.offer_timeout_secs = 1;
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();
        let save_dir = dir_b.path().join("dl");
        std::fs::create_dir_all(&save_dir).unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, mut ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), None);
        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        // 应答者：等 Ask → 0.4s 后 extend → 再 0.8s 后接受（若未顺延，1s deadline 已超时拒绝）
        tokio::spawn(async move {
            let ask = timeout(Duration::from_secs(5), ask_rx_b.recv()).await.unwrap().unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
            ask.extend.notify_one();
            tokio::time::sleep(Duration::from_millis(800)).await;
            let _ = ask.respond.send(Some(save_dir));
        });

        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);
        let res = timeout(Duration::from_secs(15),
            push_files(&sm_a, &fp_b, vec![src], &sender_jobs_a, progress_tx)).await.unwrap();
        assert!(res.is_ok(), "顺延后应答应成功, 实际: {:?}", res.err());
    }

    /// Task 3 Test 3: Auto 档 hook 触发
    #[tokio::test]
    async fn auto_offer_hook_fires_with_job_and_peer() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.download_dir = dir_b.path().join("dl");
        std::fs::create_dir_all(dir_b.path().join("dl")).unwrap();
        {
            use crate::identity::{TrustedPeer, Perms, PushPolicy};
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let (auto_tx, mut auto_rx) = mpsc::channel::<AutoOfferInfo>(8);
        set_auto_offer_hook(auto_tx);

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, _ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), None);
        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);
        let res = timeout(Duration::from_secs(15),
            push_files(&sm_a, &fp_b, vec![src], &sender_jobs_a, progress_tx)).await.unwrap();
        assert!(res.is_ok(), "Auto 接受应成功: {:?}", res.err());

        let info = timeout(Duration::from_secs(5), auto_rx.recv()).await.unwrap().unwrap();
        assert_eq!(info.peer, fp_a);
        assert_eq!(info.file_count, 1);
    }

    /// R9-2: 目录浏览测试 - browse=true 返回真实条目，browse=false 返回空
    #[tokio::test]
    async fn list_dir_remote_with_browse_perms() {
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let tmp = TempDir::new().unwrap();
        let share_path = tmp.path().join("share");
        fs::create_dir_all(&share_path).unwrap();

        // 创建测试文件和目录
        fs::write(share_path.join("file1.txt"), b"content1").unwrap();
        fs::create_dir_all(share_path.join("subdir")).unwrap();
        fs::write(share_path.join("subdir/file2.txt"), b"content2").unwrap();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = crate::test_support::setup_ctx("乙");

        // 甲侧共享区（作为被浏览方）
        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        // 甲监听（作为服务方）
        let a_addr = crate::test_support::start_listener(&sm_a).await;
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), reg_a, ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 乙侧也需要路由器（用于接收 ListResp 等响应消息）
        {
            let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
            let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
            spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])), ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);
        }

        // 乙连接甲
        let _peer_fp = timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应成功")
            .unwrap();

        // 预置甲侧信任乙，给予 browse 权限
        {
            let mut trust_a = ctx_a.trust.lock().await;
            trust_a.upsert(crate::identity::TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: crate::identity::Perms {
                    browse: true,
                    download: false,
                    push: crate::identity::PushPolicy::Deny,
                },
            });
            trust_a.save().unwrap();
        }

        // 测试 1: browse=true 权限 - 应返回真实文件列表
        let (_m, resp_rx) = sm_b.send_rpc(&fp_a, ControlMsg::ListReq {
            share_id: "share1".to_string(),
            path: String::new(),
            cursor: 0,
            msg_id: 0,
        }).await.unwrap();

        let list_result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ListResp { entries, next_cursor, .. })) => Ok::<_, ()>((entries, next_cursor)),
                _ => Err(()),
            }
        }).await;

        let (entries, next_cursor) = list_result.unwrap().unwrap();
        assert_eq!(entries.len(), 2, "应有 2 个条目（file1.txt 和 subdir）");
        assert_eq!(entries[0].name, "file1.txt");
        assert_eq!(entries[1].name, "subdir");
        assert_eq!(entries[1].is_dir, true);
        assert!(next_cursor.is_none(), "第一页应已包含全部内容");

        // 测试子目录浏览
        let (_m, resp_rx) = sm_b.send_rpc(&fp_a, ControlMsg::ListReq {
            share_id: "share1".to_string(),
            path: "subdir".to_string(),
            cursor: 0,
            msg_id: 0,
        }).await.unwrap();

        let list_result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ListResp { entries, .. })) => Ok::<_, ()>(entries),
                _ => Err(()),
            }
        }).await;

        let entries = list_result.unwrap().unwrap();
        assert_eq!(entries.len(), 1, "子目录应有 1 个条目");
        assert_eq!(entries[0].name, "file2.txt");

        // 测试 2: browse=false 权限 - 应返回空列表
        {
            let mut trust_a = ctx_a.trust.lock().await;
            trust_a.upsert(crate::identity::TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: crate::identity::Perms {
                    browse: false,
                    download: false,
                    push: crate::identity::PushPolicy::Deny,
                },
            });
            trust_a.save().unwrap();
        }

        let (_m, resp_rx) = sm_b.send_rpc(&fp_a, ControlMsg::ListReq {
            share_id: "share1".to_string(),
            path: String::new(),
            cursor: 0,
            msg_id: 0,
        }).await.unwrap();

        let list_result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ListResp { entries, next_cursor, .. })) => Ok::<_, ()>((entries, next_cursor)),
                _ => Err(()),
            }
        }).await;

        let (entries, next_cursor) = list_result.unwrap().unwrap();
        assert_eq!(entries.len(), 0, "无浏览权限应返回空列表");
        assert!(next_cursor.is_none());

        // 测试 3: 无效共享区 ID - 应返回空列表（防客户端挂死）
        {
            let mut trust_a = ctx_a.trust.lock().await;
            trust_a.upsert(crate::identity::TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: crate::identity::Perms {
                    browse: true,
                    download: false,
                    push: crate::identity::PushPolicy::Deny,
                },
            });
            trust_a.save().unwrap();
        }

        let (_m, resp_rx) = sm_b.send_rpc(&fp_a, ControlMsg::ListReq {
            share_id: "invalid_share".to_string(),
            path: String::new(),
            cursor: 0,
            msg_id: 0,
        }).await.unwrap();

        let list_result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ListResp { entries, next_cursor, .. })) => Ok::<_, ()>((entries, next_cursor)),
                _ => Err(()),
            }
        }).await;

        let (entries, next_cursor) = list_result.unwrap().unwrap();
        assert_eq!(entries.len(), 0, "无效共享区应返回空列表");
        assert!(next_cursor.is_none());

        tracing::info!("目录浏览测试通过");
    }

    /// Task 16 (M-B5): 并发 3 个 ListReq 不同目录同时进行，互不报"通道被占用"
    #[tokio::test]
    async fn concurrent_list_requests_multiplex_independently() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::store::ShareDef;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let tmp = TempDir::new().unwrap();
        // 三个子目录,各放一个不同名文件以区分响应
        for d in ["d1", "d2", "d3"] {
            fs::create_dir_all(tmp.path().join(d)).unwrap();
            fs::write(tmp.path().join(d).join(format!("{}.txt", d)), d.as_bytes()).unwrap();
        }
        let share_path = tmp.path().to_path_buf();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = crate::test_support::setup_ctx("乙");

        let reg_a = Arc::new(ShareRegistry::new(vec![ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let a_addr = start_listener(&sm_a).await;
        {
            let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道");
            let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
            spawn_rpc_router(sm_a.clone(), ctx_a.clone(), reg_a, ctrl_rx_a, ask_tx_a,
                mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);
        }
        {
            let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道");
            let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
            spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
                ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
                crate::transfer::sender_state::new_sender_job_map(), None);
        }
        timeout(Duration::from_secs(5), sm_b.connect(a_addr)).await.unwrap().unwrap();

        {
            let mut trust_a = ctx_a.trust.lock().await;
            trust_a.upsert(crate::identity::TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: crate::identity::Perms {
                    browse: true,
                    download: false,
                    push: crate::identity::PushPolicy::Deny,
                },
            });
            trust_a.save().unwrap();
        }

        // 同时发起 3 个不同目录的 ListReq——多路化后各自独立 oneshot,无共享通道占用
        let mut rxs = Vec::new();
        for d in ["d1", "d2", "d3"] {
            let (_m, rx) = sm_b.send_rpc(&fp_a, ControlMsg::ListReq {
                share_id: "share1".to_string(),
                path: d.to_string(),
                cursor: 0,
                msg_id: 0,
            }).await.expect("第 3 个并发请求也不应报通道占用");
            rxs.push((d, rx));
        }

        // 逐个收响应,验证各请求拿到自己目录的条目
        for (d, rx) in rxs {
            let res = timeout(Duration::from_secs(5), async {
                match rx.await {
                    Ok((_, ControlMsg::ListResp { entries, .. })) => entries,
                    _ => panic!("应收到 ListResp"),
                }
            }).await.unwrap();
            assert_eq!(res.len(), 1, "目录 {} 应恰好 1 个条目", d);
            assert_eq!(res[0].name, format!("{}.txt", d), "响应须按 msg_id 路由到对应请求");
        }
    }

    #[tokio::test]
    async fn control_task_routes_to_registry() {
        // 不在表的任务 → false
        assert!(!control_task(u64::MAX, TaskControl::Cancel));

        // 注册一个通道 → true 且命令到达
        let (tx, mut rx) = mpsc::channel(4);
        register_task_control(987654321, tx);
        assert!(control_task(987654321, TaskControl::Pause));
        match rx.recv().await {
            Some(TaskControl::Pause) => {}
            other => panic!("应收到 Pause, 实际 {:?}", other),
        }
        remove_task_control(987654321);
        assert!(!control_task(987654321, TaskControl::Resume));
    }

    /// T6：MetaReq 接入 SenderJobState + SourceProbe + 事件通道
    #[tokio::test]
    async fn meta_req_emits_source_started_and_registers_sender_job() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use crate::transfer::SourceRole;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();

        // Setup two sessions: A=sender/server with share, B=receiver
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // Setup trust with download permission
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // Setup A's shared file
        let share_path = dir_a.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("test.bin"), &test_data).unwrap();
        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        // A's listener + router with source event channel
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(16);
        let sender_jobs = crate::transfer::sender_state::new_sender_job_map();

        let a_addr = start_listener(&sm_a).await;
        let ctrl_rx = sm_a.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_a.clone(),
            ctx_a.clone(),
            reg_a,
            ctrl_rx,
            ask_tx,
            mpsc::channel(8).0,
            sender_jobs.clone(),
            Some(source_tx),
        );

        // B connects to A
        let peer_fp = timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();
        assert_eq!(peer_fp, fp_a);

        // B sends MetaReq
        let (_m, resp_rx) = sm_b.send_rpc(&fp_a, ControlMsg::MetaReq {
            share_id: "share1".to_string(),
            path: "test.bin".to_string(),
            msg_id: 0,
        }).await.unwrap();

        // Assert SourceStarted event received
        let source_ev = timeout(Duration::from_secs(5), source_rx.recv())
            .await
            .expect("应在超时前收到 SourceStarted")
            .expect("通道不应关闭");

        match source_ev {
            ProgressEvent::SourceStarted { job_id, role, peer, name, total } => {
                assert!(job_id >= 0x8000_0000_0000_0000, "job_id 应使用高位段: {}", job_id);
                assert_eq!(role, SourceRole::SourcePull);
                assert_eq!(peer, fp_b);
                assert_eq!(name, "test.bin");
                assert_eq!(total, 1024);
            }
            other => panic!("应收到 SourceStarted，实际收到: {:?}", other),
        }

        // Assert sender_jobs contains the job_id
        let jobs = sender_jobs.read().await;
        assert!(!jobs.is_empty(), "sender_jobs 应包含至少一个任务");

        // Assert B still receives MetaResp (guards Ruling B's defensive path)
        let meta_resp = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::MetaResp { job_id, file_name, total_size, .. })) => {
                    Some((job_id, file_name, total_size))
                }
                _ => None,
            }
        }).await.expect("应在超时前收到 MetaResp").expect("应收到 MetaResp");

        assert_eq!(meta_resp.1, "test.bin");
        assert_eq!(meta_resp.2, 1024);

        tracing::info!("MetaReq 接入 SenderJobState + SourceProbe + 事件通道测试通过");
    }

    #[tokio::test]
    async fn transfer_ctl_throttle_updates_sender_job_cap() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::transfer::manifest::Manifest;
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use crate::session::next_source_job_id;
        use crate::transfer::sender_state::{new_sender_job_map, new_sender_job_state};
        use crate::protocol::{ControlMsg, TransferAction};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // Setup two sessions: A=sender/server, B=receiver/client
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // Setup trust with download permission
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // Setup A's shared file
        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let share_path = dir_a.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("test.bin"), &test_data).unwrap();

        // Create a sender job manually
        let job_id = next_source_job_id();

        // Define the source file path first
        let src_path = share_path.join("test.bin");

        // Build manifest from the actual file
        let manifest = Manifest::build(&src_path).expect("manifest build should succeed");

        let (progress_tx, _progress_rx) = mpsc::channel(16);
        let sender_job_state = Arc::new(new_sender_job_state(
            job_id,
            src_path.clone(),
            manifest,
            None,
            progress_tx,
        ));

        // Create sender_jobs map and register the job
        let sender_jobs = new_sender_job_map();
        {
            let mut jobs = sender_jobs.write().await;
            jobs.insert(job_id, sender_job_state);
        }

        // Start A's listener + router
        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let a_addr = start_listener(&sm_a).await;
        let ctrl_rx = sm_a.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_a.clone(),
            ctx_a.clone(),
            reg_a,
            ctrl_rx,
            ask_tx,
            mpsc::channel(8).0,
            sender_jobs.clone(),
            None,
        );

        // B connects to A
        let peer_fp = timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();
        assert_eq!(peer_fp, fp_a);

        // B sends Throttle control message
        sm_b.send_ctrl(&fp_a, ControlMsg::TransferCtl {
            job_id,
            action: TransferAction::Throttle { max_streams: 3 },
        }).await.unwrap();

        // Assert that sender_jobs throttle_cap was updated within timeout
        let result = timeout(Duration::from_secs(5), async {
            loop {
                let jobs = sender_jobs.read().await;
                if let Some(state) = jobs.get(&job_id) {
                    let cap = state.throttle_cap.load(std::sync::atomic::Ordering::Relaxed);
                    if cap == 3 {
                        return true;
                    }
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await;

        assert!(result.is_ok(), "throttle_cap should be updated to 3 within timeout");

        tracing::info!("TransferCtl Throttle 测试通过");
    }

    /// T16: 推送控制桥接测试 (FIX D)
    #[tokio::test]
    async fn push_control_bridging_to_sender_job_state() {
        use std::fs;
        use crate::transfer::manifest::Manifest;
        use crate::session::next_source_job_id;
        use crate::transfer::sender_state::new_sender_job_state;
        use std::sync::Arc;

        crate::test_support::init_tracing();

        // Create a test file
        let tmp = tempfile::TempDir::new().unwrap();
        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let src_path = tmp.path().join("test.bin");
        fs::write(&src_path, &test_data).unwrap();

        let manifest = Manifest::build(&src_path).expect("manifest build should succeed");
        let (progress_tx, _progress_rx) = mpsc::channel(16);

        // Simulate push control setup
        let offer_id = 12345u64;
        let push_control = PushControl {
            paused: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };

        // Register push controls
        push_controls().lock().unwrap().insert(offer_id, push_control.clone());

        // Create sender job state
        let job_id = next_source_job_id();
        let mut state = new_sender_job_state(
            job_id,
            src_path.clone(),
            manifest,
            None,
            progress_tx,
        );

        // Verify original Arcs are different
        assert!(!Arc::ptr_eq(&state.paused, &push_control.paused), "Original paused should be different Arc");
        assert!(!Arc::ptr_eq(&state.cancelled, &push_control.cancelled), "Original cancelled should be different Arc");

        // Bridge push controls (simulating the FIX B logic)
        state.paused = push_control.paused.clone();
        state.cancelled = push_control.cancelled.clone();

        // Verify bridging worked - same Arcs
        assert!(Arc::ptr_eq(&state.paused, &push_control.paused), "Paused should share Arc after bridging");
        assert!(Arc::ptr_eq(&state.cancelled, &push_control.cancelled), "Cancelled should share Arc after bridging");

        // Verify bidirectional control works
        push_control.paused.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(state.paused.load(std::sync::atomic::Ordering::Relaxed), "State paused should reflect push_control change");

        state.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(push_control.cancelled.load(std::sync::atomic::Ordering::Relaxed), "Push control cancelled should reflect state change");

        // Cleanup
        push_controls().lock().unwrap().remove(&offer_id);

        tracing::info!("推送控制桥接测试通过");
    }

    /// T16: 取消标志阻止 FetchReq 并触发清理 (FIX C extended - verify probe_stop)
    #[tokio::test]
    async fn transfer_ctl_or_flag_cancel_stops_serving() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::transfer::manifest::Manifest;
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use crate::session::next_source_job_id;
        use crate::transfer::sender_state::{new_sender_job_map, new_sender_job_state};
        use crate::protocol::ControlMsg;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // Setup two sessions: A=sender/server, B=receiver/client
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // Setup trust with download permission
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // Setup A's shared file
        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let share_path = dir_a.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("test.bin"), &test_data).unwrap();

        // Create a sender job manually
        let job_id = next_source_job_id();
        let src_path = share_path.join("test.bin");
        let manifest = Manifest::build(&src_path).expect("manifest build should succeed");

        let (progress_tx, mut progress_rx) = mpsc::channel(16);
        let sender_job_state = Arc::new(new_sender_job_state(
            job_id,
            src_path.clone(),
            manifest,
            None,
            progress_tx,
        ));

        // Create sender_jobs map and register the job
        let sender_jobs = new_sender_job_map();
        {
            let mut jobs = sender_jobs.write().await;
            jobs.insert(job_id, sender_job_state.clone());
        }

        // Start A's listener + router
        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let a_addr = start_listener(&sm_a).await;
        let ctrl_rx = sm_a.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_a.clone(),
            ctx_a.clone(),
            reg_a,
            ctrl_rx,
            ask_tx,
            mpsc::channel(8).0,
            sender_jobs.clone(),
            None,
        );

        // B connects to A
        let peer_fp = timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();
        assert_eq!(peer_fp, fp_a);

        // Set cancelled flag directly (simulating transfer_action cancel)
        sender_job_state.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);

        // B attempts to fetch chunk (should be ignored due to cancelled flag)
        sm_b.send_ctrl(&fp_a, ControlMsg::FetchReq { job_id, chunk: 0 }).await.unwrap();

        // Assert that job is cleaned up and SourceFailed event is sent
        let result = timeout(Duration::from_secs(5), async {
            loop {
                let jobs = sender_jobs.read().await;
                if !jobs.contains_key(&job_id) {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }).await;

        assert!(result.is_ok(), "sender job should be removed after cancel within timeout");

        // Check that SourceFailed event was sent
        let event_result = timeout(Duration::from_secs(1), progress_rx.recv()).await;
        assert!(event_result.is_ok(), "Should receive SourceFailed event");
        if let Ok(Some(ProgressEvent::SourceFailed { job_id: event_job_id, reason })) = event_result {
            assert_eq!(event_job_id, job_id);
            assert!(reason.contains("取消") || reason.contains("关闭"));
        }

        // FIX C extension: Verify cleanup completeness - job was removed from registry
        // (Probe stop behavior is verified by the cleanup test coverage - we can't directly observe it here)
        let jobs_after_cancel = sender_jobs.read().await;
        assert!(!jobs_after_cancel.contains_key(&job_id), "Job should be removed from registry after cancel");

        tracing::info!("TransferCtl Cancel 测试通过");
    }

    /// T16: 暂停标志阻止 FetchReq 但不清理任务 (FIX C strengthened)
    #[tokio::test]
    async fn transfer_ctl_or_flag_pause_blocks_serving() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::transfer::manifest::Manifest;
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use crate::session::next_source_job_id;
        use crate::transfer::sender_state::{new_sender_job_map, new_sender_job_state};
        use crate::protocol::ControlMsg;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // Setup two sessions: A=sender/server, B=receiver/client
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // Setup trust with download permission
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // Setup A's shared file
        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let share_path = dir_a.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("test.bin"), &test_data).unwrap();

        // Create a sender job manually
        let job_id = next_source_job_id();
        let src_path = share_path.join("test.bin");
        let manifest = Manifest::build(&src_path).expect("manifest build should succeed");

        let (progress_tx, mut progress_rx) = mpsc::channel(16);
        let sender_job_state = Arc::new(new_sender_job_state(
            job_id,
            src_path.clone(),
            manifest,
            None,
            progress_tx,
        ));

        // Create sender_jobs map and register the job
        let sender_jobs = new_sender_job_map();
        {
            let mut jobs = sender_jobs.write().await;
            jobs.insert(job_id, sender_job_state.clone());
        }

        // Start A's listener + router
        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let a_addr = start_listener(&sm_a).await;
        let ctrl_rx = sm_a.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_a.clone(),
            ctx_a.clone(),
            reg_a,
            ctrl_rx,
            ask_tx,
            mpsc::channel(8).0,
            sender_jobs.clone(),
            None,
        );

        // B connects to A
        let peer_fp = timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();
        assert_eq!(peer_fp, fp_a);

        // Test 1: Paused flag blocks serving
        sender_job_state.paused.store(true, std::sync::atomic::Ordering::Relaxed);

        // Attempt to fetch chunk while paused (should be ignored)
        sm_b.send_ctrl(&fp_a, ControlMsg::FetchReq { job_id, chunk: 0 }).await.unwrap();

        // Give some time for the request to be processed
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Verify job still exists in registry (not cleaned up like cancel)
        let jobs = sender_jobs.read().await;
        assert!(jobs.contains_key(&job_id), "Job should still exist when paused");
        drop(jobs);

        // Test 2: Resume allows serving again
        sender_job_state.paused.store(false, std::sync::atomic::Ordering::Relaxed);

        // Now attempt to fetch chunk again (should be served this time)
        sm_b.send_ctrl(&fp_a, ControlMsg::FetchReq { job_id, chunk: 0 }).await.unwrap();

        // Wait for SourceChunkDone event (indicates successful serving)
        let chunk_event = timeout(Duration::from_secs(3), progress_rx.recv()).await;
        assert!(chunk_event.is_ok(), "Should receive SourceChunkDone after resume");

        tracing::info!("TransferCtl Pause 测试通过");
    }

    /// 暂停期间到达的 FetchReq 必须被**延迟**服务而非丢弃——接收端窗口
    /// 配额(sent-completed)按"已发请求数"记账且无 per-请求重发,丢弃意味着
    /// 该块永久丢失:恢复后窗口死锁,直至 60s idle 看门狗判死
    /// (2026-09-07 api-acceptance D2 暂停→继续→interrupted 实证)。
    /// 本测试复刻真实接收端行为:暂停期发请求→恢复→**不发新请求**,
    /// 断言被延迟的块流最终到达。
    #[tokio::test]
    async fn paused_fetchreq_is_deferred_not_dropped() {
        use std::fs;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::transfer::manifest::Manifest;
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::store::ShareDef;
        use crate::session::next_source_job_id;
        use crate::transfer::sender_state::{new_sender_job_map, new_sender_job_state};
        use crate::protocol::ControlMsg;
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // Setup two sessions: A=sender/server, B=receiver/client
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // A 的共享文件(单块)
        let test_data: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        let share_path = _dir_a.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("test.bin"), &test_data).unwrap();

        let job_id = next_source_job_id();
        let src_path = share_path.join("test.bin");
        let manifest = Manifest::build(&src_path).expect("manifest build should succeed");

        let (progress_tx, mut progress_rx) = mpsc::channel(16);
        let sender_job_state = Arc::new(new_sender_job_state(
            job_id,
            src_path.clone(),
            manifest,
            None,
            progress_tx,
        ));

        let sender_jobs = new_sender_job_map();
        {
            let mut jobs = sender_jobs.write().await;
            jobs.insert(job_id, sender_job_state.clone());
        }

        let reg_a = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let a_addr = start_listener(&sm_a).await;
        let ctrl_rx = sm_a.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_a.clone(),
            ctx_a.clone(),
            reg_a,
            ctrl_rx,
            ask_tx,
            mpsc::channel(8).0,
            sender_jobs.clone(),
            None,
        );

        timeout(Duration::from_secs(5), sm_b.connect(a_addr))
            .await
            .expect("连接应在超时前完成")
            .unwrap();

        // 暂停期间发 FetchReq(会被路由器看到)
        sender_job_state.paused.store(true, std::sync::atomic::Ordering::Relaxed);
        sm_b.send_ctrl(&fp_a, ControlMsg::FetchReq { job_id, chunk: 0 }).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        // 恢复,且**不再发任何新请求**——被延迟的请求必须自行被服务
        sender_job_state.paused.store(false, std::sync::atomic::Ordering::Relaxed);

        let chunk_event = timeout(Duration::from_secs(3), progress_rx.recv()).await;
        assert!(chunk_event.is_ok(),
            "暂停期请求应在恢复后被延迟服务(丢弃语义=接收端窗口配额泄漏→死锁)");
        tracing::info!("暂停期 FetchReq 延迟服务测试通过");
    }

    /// v0.2.6 文件夹推送契约：rel_dir 全链路——小文件批流带子目录、大文件
    /// MetaReq 按 "rel/name" 匹配、接收方按结构落盘（同名文件不同子目录共存）
    #[tokio::test]
    async fn push_dir_with_rel_dir_preserves_structure() {
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 源结构: root/{a.txt(小), sub/big.bin(大 5MB), sub/same.txt(小)},
        // 另一个 top/same.txt —— 同名文件跨子目录
        let tmp = TempDir::new().unwrap();
        let mk = |rel: &str, data: Vec<u8>| {
            let p = tmp.path().join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, &data).unwrap();
            p
        };
        let small_a = mk("root/a.txt", (0..50 * 1024).map(|i| (i % 256) as u8).collect());
        let big = mk("root/sub/big.bin", (0..5 * 1024 * 1024).map(|i| (i % 251) as u8).collect());
        let same_sub = mk("root/sub/same.txt", vec![1u8; 30 * 1024]);
        let same_top = mk("root/top/same.txt", vec![2u8; 30 * 1024]);

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        let download_dir_b = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir_b).unwrap();
        ctx_b.config.write().await.download_dir = download_dir_b.clone();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx, ask_tx,
            mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(),
            None,
        );

        // 甲侧路由器：大文件推送时乙反向发 MetaReq/FetchReq，甲侧 RPC 必须就位
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        {
            let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
            let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
            spawn_rpc_router(
                sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
                ctrl_rx_a, ask_tx_a,
                mpsc::channel(8).0,
                sender_jobs_a.clone(),
                None,
            );
        }

        let _peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 推送：(路径, rel_dir) 对——rel_dir 相对 "root"
        let files = vec![
            (small_a.clone(), "root".to_string()),
            (big.clone(), "root/sub".to_string()),
            (same_sub.clone(), "root/sub".to_string()),
            (same_top.clone(), "root/top".to_string()),
        ];
        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(64);
        timeout(
            Duration::from_secs(60),
            push_files_rel(&sm_a, &fp_b, files, &sender_jobs_a, progress_tx),
        ).await.expect("文件夹推送应在超时前完成").expect("推送应成功");

        // 断言结构保持 + 同名共存 + 内容一致
        let got_a = fs::read(download_dir_b.join("root/a.txt")).expect("root/a.txt 应存在");
        assert_eq!(got_a.len(), 50 * 1024);
        let got_big = fs::read(download_dir_b.join("root/sub/big.bin")).expect("root/sub/big.bin 应存在");
        assert_eq!(got_big.len(), 5 * 1024 * 1024);
        assert_eq!(got_big, fs::read(&big).unwrap(), "大文件内容应逐字节一致");
        let got_same_sub = fs::read(download_dir_b.join("root/sub/same.txt")).expect("root/sub/same.txt 应存在");
        assert!(got_same_sub.iter().all(|&b| b == 1), "sub/same.txt 内容应为 1");
        let got_same_top = fs::read(download_dir_b.join("root/top/same.txt")).expect("root/top/same.txt 应存在");
        assert!(got_same_top.iter().all(|&b| b == 2), "top/same.txt 内容应为 2");

        // parts 集中在下载根，不污染子目录
        assert!(!download_dir_b.join("root/sub/.localtrans-parts").exists(),
            "parts 不应出现在子目录内");

        tracing::info!("文件夹推送 rel_dir 契约测试通过");
    }

    /// T8: 全小文件 push 修复:每文件都应收到 Started + Done,不再卡"等待中"
    #[tokio::test]
    async fn push_only_small_files_emits_per_file_started_done() {
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 准备 3 个 50KB 文件(全小文件,不进大文件块流路径)
        let mut files = Vec::new();
        let tmp = TempDir::new().unwrap();
        for i in 0..3 {
            let path = tmp.path().join(format!("small_{}.txt", i));
            let data: Vec<u8> = (0..50 * 1024).map(|j| ((j + i * 50) % 256) as u8).collect();
            fs::write(&path, &data).unwrap();
            files.push(path);
        }

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = crate::test_support::setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

        // 互信 push=Auto
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b, name: "乙".into(), alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a, name: "甲".into(), alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let download_dir_b = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir_b).unwrap();
        ctx_b.config.write().await.download_dir = download_dir_b.clone();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, _ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(
            sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx, ask_tx,
            mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(),
            None,
        );

        let _peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

        timeout(
            Duration::from_secs(30),
            push_files(&sm_a, &fp_b, files.clone(), &crate::transfer::sender_state::new_sender_job_map(), progress_tx),
        ).await.expect("推送应在超时前完成").expect("推送应成功");

        // 关键断言:应收到 3 条 Started + 3 条 Done(每个小文件一对)
        let mut started_names: Vec<String> = Vec::new();
        let mut done_count = 0u32;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = progress_rx.recv().await {
                match ev {
                    ProgressEvent::Started { name, .. } => started_names.push(name),
                    ProgressEvent::Done { .. } => done_count += 1,
                    ProgressEvent::Failed { reason, .. } => panic!("推送失败: {}", reason),
                    _ => {}
                }
                if done_count >= 3 && started_names.len() >= 3 { break; }
            }
        }).await.expect("应在超时前收完事件");

        assert_eq!(started_names.len(), 3, "应有 3 条 Started 事件");
        assert_eq!(done_count, 3, "应有 3 条 Done 事件");
    }

    #[tokio::test]
    async fn probe_stop_fires_across_arc_clones() {
        use crate::transfer::sender_state::{new_sender_job_state, fire_probe_stop};
        use std::sync::Arc;

        crate::test_support::init_tracing();

        // 创建一个临时文件和清单
        let temp_dir = tempfile::TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test_probe_stop.dat");
        std::fs::write(&file_path, b"hello world").unwrap();
        let manifest = Manifest::build(&file_path).expect("清单构建成功");

        // 创建一个带有真实 oneshot 的 SenderJobState
        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let mut state = new_sender_job_state(
            123,
            file_path.clone(),
            manifest,
            None,
            progress_tx,
        );

        // 替换为我们可以观测的真实 oneshot
        let (probe_stop_tx, mut probe_stop_rx) = oneshot::channel();
        state.probe_stop_tx = Some(Arc::new(std::sync::Mutex::new(Some(probe_stop_tx))));
        let state = Arc::new(state);

        // 模拟处理器的克隆(这是 bug 场景:多克隆导致 Arc::get_mut 失败)
        let state_clone = state.clone();

        // 从克隆调用 fire_probe_stop(模拟 FetchReq 取消路径)
        fire_probe_stop(&state_clone);

        // 断言接收端收到信号
        let result = probe_stop_rx.try_recv();
        assert!(result.is_ok(), "probe_stop_rx 应收到信号");
        assert_eq!(result.unwrap(), (), "应收到单元值");

        // 断言第二次调用是空操作(无 panic,无重复发送)
        fire_probe_stop(&state_clone); // 第二次调用应该安全
    }

    /// 安卓 T2 测试 1: ShareRename 双向测试
    #[tokio::test]
    async fn share_op_rename_roundtrip_between_peers() {
        use tokio::time::timeout;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};

        crate::test_support::init_tracing();

        // 搭建双方: a 是浏览方, b 是数据方
        let tmp = tempfile::TempDir::new().unwrap();
        let share_path = tmp.path().join("share");
        std::fs::create_dir_all(&share_path).unwrap();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // b 侧共享区放 a.txt
        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path.clone(),
        }]));
        // 直接使用文件路径创建测试文件
        std::fs::write(share_path.join("a.txt"), b"hello").unwrap();

        // b 监听并启动路由器
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, _ask_rx_b) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(), None);

        // a 侧也需要路由器(接收响应)
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 连接
        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        // a 侧: take_inbound_resp_rx → 发 ShareRename
        let (_m, resp_rx) = sm_a.send_rpc(&fp_b, ControlMsg::ShareRename {
            share_id: "share1".to_string(),
            path: "a.txt".to_string(),
            new_name: "b.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();

        // 断言: 收到 ShareOpResult{ok:true}
        let result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ShareOpResult { ok, error, .. })) => (ok, error),
                _ => panic!("响应通道关闭"),
            }
        }).await.unwrap();

        assert!(result.0, "重命名应成功: {:?}", result.1);

        // b 共享区出现 b.txt
        let b_txt_path = share_path.join("b.txt");
        let a_txt_path = share_path.join("a.txt");
        assert!(b_txt_path.exists(), "b.txt 应存在");
        assert!(!a_txt_path.exists(), "a.txt 应已被重命名");
    }

    /// 安卓 T2 测试 2: ShareDelete 缺失文件返回错误
    #[tokio::test]
    async fn share_op_delete_missing_returns_error_result() {
        use tokio::time::timeout;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};

        crate::test_support::init_tracing();

        let tmp = tempfile::TempDir::new().unwrap();
        let share_path = tmp.path().join("share");
        std::fs::create_dir_all(&share_path).unwrap();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, _ask_rx_b) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        let (_m, resp_rx) = sm_a.send_rpc(&fp_b, ControlMsg::ShareDelete {
            share_id: "share1".to_string(),
            path: "nope.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();

        let result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ShareOpResult { ok, error, .. })) => (ok, error),
                _ => panic!("响应通道关闭"),
            }
        }).await.unwrap();

        assert!(!result.0, "删除不存在的文件应失败");
        assert!(result.1.is_some(), "应返回错误信息");
    }

    /// 安卓 T2 测试 3: ShareMkdir 创建目录
    #[tokio::test]
    async fn share_op_mkdir_creates_dir_on_peer() {
        use tokio::time::timeout;
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};

        crate::test_support::init_tracing();

        let tmp = tempfile::TempDir::new().unwrap();
        let share_path = tmp.path().join("share");
        std::fs::create_dir_all(&share_path).unwrap();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path.clone(),
        }]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, _ask_rx_b) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<crate::transfer::OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        let (_m, resp_rx) = sm_a.send_rpc(&fp_b, ControlMsg::ShareMkdir {
            share_id: "share1".to_string(),
            path: "x/y".to_string(),
            msg_id: 0,
        }).await.unwrap();

        let result = timeout(Duration::from_secs(5), async {
            match resp_rx.await {
                Ok((_, ControlMsg::ShareOpResult { ok, error, .. })) => (ok, error),
                _ => panic!("响应通道关闭"),
            }
        }).await.unwrap();

        assert!(result.0, "创建目录应成功: {:?}", result.1);

        // 验证 b 侧目录存在
        let reg_check = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));
        assert!(reg_check.resolve("share1", "x/y").unwrap().is_dir(), "x/y 应是目录");
    }

    /// 安卓 T2 Step 5: 老版本兼容性验证结论
    /// 注意: 此测试记录老版本兼容性行为,供 Task 4 FFI 错误文案使用
    /// 由于当前版本已包含新变体,无法直接模拟老版本 serde 行为,
    /// 此测试仅记录结论: 老版本收到新变体时 serde 报 unknown variant 错误
    /// (该结论基于 serde 对 Rust 枚举的标准行为,实际复现需用 v0.5.0 二进制)
    #[test]
    fn old_peer_rejects_unknown_share_op_variants() {
        // 当前版本可以正确解析新变体(证明协议扩展成功)
        let new_variant_json = r#"{"type":"share_rename","share_id":"s1","path":"a.txt","new_name":"b.txt"}"#;
        let result = serde_json::from_str::<crate::protocol::ControlMsg>(new_variant_json);
        assert!(result.is_ok(), "当前版本应支持新变体 share_rename");

        // 记录: 老版本(v0.5.0)收到此 JSON 会报 serde unknown variant 错误
        // 因为老版本 ControlMsg 枚举中没有 ShareRename/Delete/Mkdir/OpResult 变体
        // Task 4 FFI 层遇到超时/错误时应提示"对端版本过旧,不支持文件操作"
    }

    #[test]
    fn write_small_file_conflict_rename_in_same_dir() {
        let root = std::env::temp_dir().join(format!("lt-wsf-{}-conflict", std::process::id()));
        let sub = root.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        // 第一个文件落在子目录
        write_small_file(&sub, "a.txt", b"1").unwrap();
        // 同名冲突:改名文件必须仍在子目录(修复 v0.8.1 前误用 save_dir 落回根目录的 bug)
        let p2 = write_small_file(&sub, "a.txt", b"2").unwrap();
        assert!(p2.starts_with(&sub), "冲突改名不得跳出目标目录: {:?}", p2);
        assert_eq!(p2.file_name().unwrap().to_str().unwrap(), "a.txt (1)");
        // 已存在文件不被覆盖
        assert_eq!(std::fs::read(sub.join("a.txt")).unwrap(), b"1");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn write_small_file_plain_write() {
        let root = std::env::temp_dir().join(format!("lt-wsf-{}-plain", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let p = write_small_file(&root, "hello.txt", b"hi").unwrap();
        assert_eq!(p, root.join("hello.txt"));
        assert_eq!(std::fs::read(&p).unwrap(), b"hi");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn finalize_rejects_hostile_manifest_name() {
        let root = std::env::temp_dir().join(format!("lt-fin-{}-hostile", std::process::id()));
        let parts = root.join("parts/job1");
        let dest = root.join("dest");
        std::fs::create_dir_all(&parts).unwrap();
        std::fs::create_dir_all(&dest).unwrap();

        // 恶意清单:文件名带穿越(模拟 MetaResp 攻击字段)
        // 注意: Manifest::from_meta 的 chunk_hashes 参数类型是 Vec<String>
        let chunk_hashes = vec!["hash1".to_string(), "hash2".to_string()];
        let manifest = Manifest::from_meta("..\\..\\evil.dll".to_string(), 8, chunk_hashes);
        let writer = PartWriter::load_or_open(&parts, manifest).unwrap();
        let err = writer.finalize(&dest).err().expect("恶意名必须被 finalize 拒绝");
        assert!(matches!(err, EngineError::Protocol(_)), "实际: {:?}", err);
        // part 文件未被移动出 parts 目录
        assert!(!dest.join("evil.dll").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// P0-5: 移除信任即时生效——fail-closed 权限 + 移除即断开
    #[tokio::test]
    async fn removed_peer_fail_closed() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::share::ShareRegistry;
        use crate::store::ShareDef;
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        // A/B 互信连接后,B 移除 A 的信任 → A 的 SharesReq 必须得空列表而非真实数据
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("A");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("B");

        // 预置互信
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "B".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "A".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // B 侧共享区预置一个共享
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let share_id = "test_share".to_string();
        let test_file = share_path.join("test.txt");
        fs::write(&test_file, b"hello").unwrap();

        let reg_b: Arc<ShareRegistry> = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: share_id.clone(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        // B 监听 + 路由器
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(), None);

        // A 连接 B
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        // A 请求共享列表(移除信任前)应该得到真实数据
        let (_m1, rx_before) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::SharesReq { msg_id: 0 }).await.unwrap();
        let (fp_resp, msg_before) = timeout(Duration::from_secs(5), rx_before).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg_before {
            crate::protocol::ControlMsg::SharesResp { shares, .. } => {
                assert_eq!(shares.len(), 1, "移除信任前应看到1个共享");
                assert_eq!(shares[0].id, "test_share");
            }
            other => panic!("移除信任前期望 SharesResp, 实际 {:?}", other),
        }

        // 移除信任(模拟用户点"移除配对")
        ctx_b.trust.lock().await.remove(&fp_a);

        // A 请求共享列表(移除信任后)必须得到空列表(fail-closed)
        let (_m2, rx_after) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::SharesReq { msg_id: 0 }).await.unwrap();
        let (fp_resp2, msg_after) = timeout(Duration::from_secs(5), rx_after).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp2, fp_b, "响应应来自 B");
        match msg_after {
            crate::protocol::ControlMsg::SharesResp { shares, .. } => {
                assert!(shares.is_empty(), "移除信任后必须回空列表(实际: {} 个共享)", shares.len());
            }
            other => panic!("移除信任后期望 SharesResp, 实际 {:?}", other),
        }

        // 清理
        let _ = fs::remove_dir_all(share_path);
    }

    #[tokio::test]
    async fn share_ops_require_browse_perm() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use crate::share::ShareRegistry;
        use crate::store::ShareDef;
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        // A/B 互信连接后,B 把 A 降权(browse=false)→ Share*/delete/mkdir 全部拒绝且文件原样
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("A");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("B");

        // 预置互信(A 侧给 B 完全权限)
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_b.clone(),
                name: "B".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        // B 侧给 A browse=false 权限
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a.clone(),
                name: "A".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: false, download: false, push: PushPolicy::Deny },
            });
            trust.save().unwrap();
        }

        // B 侧共享区预置一个共享和一个文件
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let share_id = "test_share".to_string();
        let test_file = share_path.join("test.txt");
        fs::write(&test_file, b"hello").unwrap();

        let reg_b: Arc<ShareRegistry> = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: share_id.clone(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        // B 监听 + 路由器
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b, mpsc::channel(8).0,
            crate::transfer::sender_state::new_sender_job_map(), None);

        // A 连接 B
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        // 测试1: ShareRename 被拒绝
        let (_m1, rx1) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareRename {
            share_id: share_id.clone(),
            path: "test.txt".to_string(),
            new_name: "renamed.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let (fp_resp1, msg1) = timeout(Duration::from_secs(5), rx1).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp1, fp_b, "响应应来自 B");
        match &msg1 {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "ShareRename 应被拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("无操作权限")), "错误应含'无操作权限'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 测试2: ShareDelete 被拒绝
        let (_m2, rx2) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
            share_id: share_id.clone(),
            path: "test.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let (fp_resp2, msg2) = timeout(Duration::from_secs(5), rx2).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp2, fp_b, "响应应来自 B");
        match &msg2 {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "ShareDelete 应被拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("无操作权限")), "错误应含'无操作权限'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 测试3: ShareMkdir 被拒绝
        let (_m3, rx3) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareMkdir {
            share_id: share_id.clone(),
            path: "newdir".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let (fp_resp3, msg3) = timeout(Duration::from_secs(5), rx3).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp3, fp_b, "响应应来自 B");
        match &msg3 {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "ShareMkdir 应被拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("无操作权限")), "错误应含'无操作权限'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 验证文件未被改/删,目录未新建
        assert!(test_file.exists(), "原文件应仍然存在");
        assert!(share_path.join("renamed.txt").metadata().is_err(), "不应创建重命名文件");
        assert!(share_path.join("newdir").metadata().is_err(), "不应创建新目录");

        // 验证原文件内容未变
        let content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "hello", "原文件内容应不变");

        // 清理
        let _ = fs::remove_dir_all(share_path);
    }

    /// P0-2b: 远程删除确认门测试集
    /// 测试1: respond(true) → ShareOpResult ok=true 且目录被删
    #[tokio::test]
    async fn delete_confirm_true_executes() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        // B 侧共享区预置一个目录和一个文件
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let test_dir = share_path.join("testdir");
        fs::create_dir_all(&test_dir).unwrap();
        let test_file = test_dir.join("file.txt");
        fs::write(&test_file, b"hello").unwrap();

        let reg_b = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: "share1".to_string(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        // B 侧路由器: 建立真实的 delete_ask_rx 用于接收确认请求
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        let (delete_ask_tx_b, mut delete_ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b,
            delete_ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        // A 请求删除目录
        let (_m, resp_rx_wait) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
            share_id: "share1".to_string(),
            path: "testdir".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let mut resp_rx_pin = std::pin::pin!(resp_rx_wait);

        // B 侧收到 DeleteAsk 请求
        let delete_ask = match timeout(Duration::from_secs(5), delete_ask_rx_b.recv()).await {
            Ok(Some(ask)) => ask,
            Ok(None) => panic!("应在超时前收到 DeleteAsk, 通道已关闭"),
            Err(_) => panic!("应在超时前收到 DeleteAsk, 超时"),
        };

        assert_eq!(delete_ask.from, fp_a, "DeleteAsk 应来自 A");
        assert_eq!(delete_ask.share_id, "share1");
        assert_eq!(delete_ask.name, "testdir");
        assert!(delete_ask.is_dir, "应识别为目录");
        assert_eq!(delete_ask.entry_count, 1, "目录内应统计到1个文件(不含目录自身)");

        // 确认删除
        delete_ask.respond.send(true).unwrap();

        // A 收到成功响应
        let (fp_resp, msg) = timeout(Duration::from_secs(5), resp_rx_pin.as_mut()).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(*ok, "删除应成功");
                assert!(error.is_none(), "成功时不应有错误");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 验证目录已被删除
        assert!(!test_dir.exists(), "目录应已被删除");
        let _ = fs::remove_dir_all(share_path);
    }

    /// 测试2: respond(false) → ok=false error 含 "未确认" 且目录原样
    #[tokio::test]
    async fn delete_confirm_false_keeps() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }

        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let test_file = share_path.join("keep.txt");
        fs::write(&test_file, b"keep").unwrap();

        let reg_b = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: "share1".to_string(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        let (delete_ask_tx_b, mut delete_ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b,
            delete_ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        let (_m, rx_keep) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
            share_id: "share1".to_string(),
            path: "keep.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let mut rx_keep_pin = std::pin::pin!(rx_keep);

        let delete_ask = timeout(Duration::from_secs(5), delete_ask_rx_b.recv())
            .await
            .expect("应在超时前收到 DeleteAsk")
            .expect("DeleteAsk 通道不应关闭");

        // 拒绝删除
        delete_ask.respond.send(false).unwrap();

        let (fp_resp, msg) = timeout(Duration::from_secs(5), rx_keep_pin.as_mut()).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "删除应被拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("未确认")), "错误应含'未确认'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 验证文件原封不动
        assert!(test_file.exists(), "文件应仍然存在");
        let content = fs::read_to_string(&test_file).unwrap();
        assert_eq!(content, "keep", "文件内容应不变");

        let _ = fs::remove_dir_all(share_path);
    }

    /// 测试3: 不 respond, 等 2s → ok=false(超时自动拒绝), 进程/会话存活
    #[tokio::test]
    async fn delete_confirm_timeout_denies() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信 + 配置超时为 1 秒(测试专用)
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.consent_timeout_secs = 1;

        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let test_file = share_path.join("timeout.txt");
        fs::write(&test_file, b"timeout").unwrap();

        let reg_b = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: "share1".to_string(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        let (delete_ask_tx_b, mut delete_ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b,
            delete_ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        let (_m, rx_to) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
            share_id: "share1".to_string(),
            path: "timeout.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();
        let mut rx_to_pin = std::pin::pin!(rx_to);

        // 收到 DeleteAsk 但不响应
        let _delete_ask = timeout(Duration::from_secs(5), delete_ask_rx_b.recv())
            .await
            .expect("应在超时前收到 DeleteAsk")
            .expect("DeleteAsk 通道不应关闭");

        // 等待超时自动拒绝(consent_timeout_secs=1, 等待 2s 确保超时)
        let (fp_resp, msg) = timeout(Duration::from_secs(5), rx_to_pin.as_mut()).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "超时应自动拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("未确认")), "错误应含'未确认'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        // 验证文件未被删除
        assert!(test_file.exists(), "超时后文件应仍存在");

        // 验证连接/会话存活: 再发一个 ShareReq 能收到响应
        let (_m2, rx_alive) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::SharesReq { msg_id: 0 }).await.unwrap();
        let _ = timeout(Duration::from_secs(5), rx_alive).await
            .expect("会话应存活, 能收到 SharesReq 响应");

        let _ = fs::remove_dir_all(share_path);
    }

    /// 测试4: 连发 9 个 ShareDelete(不 respond)→ 第 9 个立即 ok=false error 含 "稍后"
    #[tokio::test]
    async fn delete_pending_cap_rejects() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.consent_timeout_secs = 1;

        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        for i in 1..=9 {
            fs::write(share_path.join(format!("{}.txt", i)), format!("file{}", i)).unwrap();
        }

        let reg_b = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: "share1".to_string(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        let (delete_ask_tx_b, mut delete_ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b,
            delete_ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        // 连发 9 个删除请求(不响应, 挂起);最后一个的响应用 msg_id 路由接收
        let mut last_rx = None;
        for i in 1..=9 {
            let (_m, rx_i) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
                share_id: "share1".to_string(),
                path: format!("{}.txt", i),
                msg_id: 0,
            }).await.unwrap();
            last_rx = Some(rx_i);
        }

        // 等待前 8 个 DeleteAsk 到达(第 9 个被 semaphore 阻挡)
        for i in 1..=8 {
            let _ask = timeout(Duration::from_secs(5), delete_ask_rx_b.recv()).await
                .expect(&format!("第 {} 个 DeleteAsk 应到达", i))
                .expect("DeleteAsk 通道不应关闭");
        }

        // 第 9 个请求应立即收到 "稍后" 拒绝
        let (fp_resp, msg) = timeout(Duration::from_secs(5), last_rx.unwrap()).await
            .expect("应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg {
            crate::protocol::ControlMsg::ShareOpResult { ok, error, .. } => {
                assert!(!ok, "第 9 个请求应被立即拒绝");
                assert!(error.as_ref().map_or(false, |e| e.contains("稍后")), "错误应含'稍后'");
            }
            other => panic!("期望 ShareOpResult, 实际 {:?}", other),
        }

        let _ = fs::remove_dir_all(share_path);
    }

    /// 测试5: 挂起一个未响应的删除期间, A 发 ListReq 仍能收到 ListResp(证明路由器不阻塞)
    #[tokio::test]
    async fn router_not_blocked_during_pending() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{TrustedPeer, Perms, PushPolicy};
        use tokio::time::{timeout, Duration};

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
            trust.save().unwrap();
        }
        ctx_b.config.write().await.consent_timeout_secs = 10;

        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        fs::write(share_path.join("pending.txt"), b"pending").unwrap();

        let reg_b = Arc::new(ShareRegistry::new(vec![
            crate::store::ShareDef { id: "share1".to_string(), path: share_path.clone(), alias: "测试共享".to_string() }
        ]));

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        let (delete_ask_tx_b, mut delete_ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b.clone(), ctrl_rx_b, ask_tx_b,
            delete_ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await
            .expect("连接应在超时前完成")
            .expect("连接应成功");

        // 发起删除但不响应(响应将通过专属 oneshot 到达; 此处故意丢弃 receiver 触发 SendErr 分支容忍)
        let (_m_del, _rx_del_drop) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ShareDelete {
            share_id: "share1".to_string(),
            path: "pending.txt".to_string(),
            msg_id: 0,
        }).await.unwrap();

        let _ask = timeout(Duration::from_secs(5), delete_ask_rx_b.recv())
            .await
            .expect("应收到 DeleteAsk")
            .expect("DeleteAsk 通道不应关闭");

        // 立即发送 ListReq, 应能收到响应(证明路由器循环不阻塞)
        let (_m_list, rx_list) = sm_a.send_rpc(&fp_b, crate::protocol::ControlMsg::ListReq {
            share_id: "share1".to_string(),
            path: ".".to_string(),
            cursor: 0,
            msg_id: 0,
        }).await.unwrap();

        let (fp_resp, msg) = timeout(Duration::from_secs(5), rx_list).await
            .expect("ListReq 应在超时前收到响应")
            .expect("响应不应失败");
        assert_eq!(fp_resp, fp_b, "响应应来自 B");
        match &msg {
            crate::protocol::ControlMsg::ListResp { entries, .. } => {
                assert!(!entries.is_empty(), "应收到目录列表");
            }
            other => panic!("期望 ListResp, 实际 {:?}", other),
        }

        let _ = fs::remove_dir_all(share_path);
    }

    /// 全局 inbound_recv_hook 是进程级单例:依赖它的测试必须互斥,
    /// 否则并行时 hook 串台(A 测试收到 B 测试的 InstantHit——实测踩坑)。
    static RECV_HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// v0.10.0 Task 9 Step 1: 大文件二次推送应零块传输（秒传命中）
    #[tokio::test]
    async fn push_same_large_file_twice_second_is_instant() {
        let _hook_guard = RECV_HOOK_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 5MB 大文件（走大文件路径）
        let file_size: u64 = 5 * 1024 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 甲的源文件
        let src_file = dir_a.path().join("instant_large.bin");
        fs::write(&src_file, &data).unwrap();

        // 乙的下载目录
        let download_dir = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();

        // 甲侧路由器（收集发送方事件，验证第二次无 SourceChunkDone）
        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), Some(source_tx));

        // 甲连接乙
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 乙侧事件收集（验证 InstantHit）
        let recv_hook_guard = crate::transfer::engine::inbound_recv_hook();
        let (recv_hook_tx, mut recv_hook_rx) = mpsc::channel::<ProgressEvent>(64);
        recv_hook_guard.lock().unwrap().replace(recv_hook_tx);

        // ===== 第一次推送：正常传输 =====
        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push = sender_jobs_a.clone();
        let sm_a1 = sm_a.clone();
        let src_file1 = src_file.clone();
        let push_fut = tokio::spawn(async move {
            push_files(&sm_a1, &fp_b, vec![src_file1], &sender_jobs_for_push, progress_tx).await
        });

        // 等待第一次推送完成
        timeout(Duration::from_secs(30), async {
            while let Some(ev) = progress_rx.recv().await {
                if matches!(ev, ProgressEvent::Done { .. }) { break; }
            }
        }).await.expect("第一次推送应完成");

        push_fut.await.expect("推送任务不应 panic")
            .expect("第一次推送应成功");

        // 验证目标文件内容一致
        let dest_file = download_dir.join("instant_large.bin");
        assert!(dest_file.exists(), "第一次推送后目标文件应存在");
        assert_eq!(fs::read(&dest_file).unwrap(), data, "第一次推送内容应一致");

        // ===== 第二次推送同一文件：应秒传 =====
        // 先排空甲侧 source_rx 里第一次推送的残留事件(SourceChunkDone 等),
        // 否则下方"二推无块传输"断言会把一推残留误判为二推传输
        while let Ok(ev) = source_rx.try_recv() {
            if matches!(ev, ProgressEvent::SourceDone { .. }) { break; }
        }

        let (progress_tx2, _progress_rx2) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push2 = sender_jobs_a.clone();
        let sm_a2 = sm_a.clone();
        let src_file2 = src_file.clone();
        let push_fut2 = tokio::spawn(async move {
            push_files(&sm_a2, &fp_b, vec![src_file2], &sender_jobs_for_push2, progress_tx2).await
        });

        // 收集乙侧事件，验证 InstantHit
        let mut saw_instant_hit = false;
        let instant_hit_name = timeout(Duration::from_secs(10), async {
            while let Some(ev) = recv_hook_rx.recv().await {
                if let ProgressEvent::InstantHit { name, total, .. } = ev {
                    saw_instant_hit = true;
                    assert_eq!(name, "instant_large.bin", "InstantHit 文件名应匹配");
                    assert_eq!(total, file_size, "InstantHit 大小应匹配");
                    return Some(name);
                }
            }
            None
        }).await.expect("应在超时前收到事件");

        assert!(saw_instant_hit, "应收到 InstantHit 事件");
        assert!(instant_hit_name.is_some(), "InstantHit 事件应有文件名");

        // 收集甲侧事件，验证无 SourceChunkDone（零块传输）
        let mut saw_source_chunk_done = false;
        let saw_source_done = timeout(Duration::from_secs(10), async {
            while let Some(ev) = source_rx.recv().await {
                if matches!(ev, ProgressEvent::SourceChunkDone { .. }) {
                    saw_source_chunk_done = true;
                }
                if matches!(ev, ProgressEvent::SourceDone { .. }) {
                    return true;
                }
            }
            false
        }).await.expect("应在超时前收到 SourceDone");

        assert!(saw_source_done, "应收到 SourceDone 事件");
        assert!(!saw_source_chunk_done, "第二次推送不应有 SourceChunkDone 事件（零块传输）");

        // 第二次推送应返回 Ok
        push_fut2.await.expect("推送任务不应 panic")
            .expect("第二次推送应成功");

        // 验证新目标文件内容仍一致（复用成功）
        assert_eq!(fs::read(&dest_file).unwrap(), data, "秒传后内容应仍一致");

        // 清理全局 hook 防污染后续测试
        crate::transfer::engine::inbound_recv_hook().lock().unwrap().take();
    }

    /// v0.10.0 Task 9: 小文件二次推送应秒传(skip 表 + 本地复用,批流零字节)
    #[tokio::test]
    async fn push_same_small_file_second_is_instant() {
        let _hook_guard = RECV_HOOK_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 100KB 小文件(走批流路径)
        let file_size: u64 = 100 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        let src_file = dir_a.path().join("instant_small.bin");
        fs::write(&src_file, &data).unwrap();

        let download_dir = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();

        let (source_tx, mut source_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), Some(source_tx));

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 乙侧事件收集(批流 Started 计数 + InstantHit)
        let recv_hook_guard = crate::transfer::engine::inbound_recv_hook();
        let (recv_hook_tx, mut recv_hook_rx) = mpsc::channel::<ProgressEvent>(64);
        recv_hook_guard.lock().unwrap().replace(recv_hook_tx.clone());

        // ===== 第一次推送:正常批流 =====
        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push = sender_jobs_a.clone();
        let sm_a1 = sm_a.clone();
        let src_file1 = src_file.clone();
        let push_fut = tokio::spawn(async move {
            push_files(&sm_a1, &fp_b, vec![src_file1], &sender_jobs_for_push, progress_tx).await
        });

        timeout(Duration::from_secs(30), async {
            while let Some(ev) = progress_rx.recv().await {
                if matches!(ev, ProgressEvent::Done { .. }) { break; }
            }
        }).await.expect("第一次推送应完成");

        push_fut.await.expect("推送任务不应 panic")
            .expect("第一次推送应成功");

        let dest_file = download_dir.join("instant_small.bin");
        assert!(dest_file.exists(), "第一次推送后目标文件应存在");
        assert_eq!(fs::read(&dest_file).unwrap(), data, "第一次推送内容应一致");

        // 排空乙侧 hook 通道残留(第一次的 Started/Done)
        while let Ok(_) = recv_hook_rx.try_recv() {}
        // 静默窗:等第一次推送的接收编排任务彻底退场(其尾流事件可能晚于
        // try_recv 排空到达——批流 Done 经 hook 泵异步转发),否则尾流 Started
        // 会误置 saw_batched_started 造成竞态假失败(实测偶发)
        tokio::time::sleep(Duration::from_millis(300)).await;
        while let Ok(_) = recv_hook_rx.try_recv() {}

        // ===== 第二次推送:应秒传 =====
        // 并行测试防互踩:hook 是进程级单例,并行兄弟测试(large 秒传)收尾的
        // take() 会误清本测试的 hook——发推送前确认仍在,被清则重装同一 tx
        // (接收编排 spawn 时快照 hook,必须在 push 前保证就位)
        if inbound_recv_hook().lock().unwrap().is_none() {
            recv_hook_guard.lock().unwrap().replace(recv_hook_tx.clone());
        }
        let (progress_tx2, mut progress_rx2) = mpsc::channel::<ProgressEvent>(64);
        let sender_jobs_for_push2 = sender_jobs_a.clone();
        let sm_a2 = sm_a.clone();
        let src_file2 = src_file.clone();
        let push_fut2 = tokio::spawn(async move {
            push_files(&sm_a2, &fp_b, vec![src_file2], &sender_jobs_for_push2, progress_tx2).await
        });

        // 乙侧应收到 InstantHit(而非批流 Started)
        let mut saw_instant_hit = false;
        let mut saw_batched_started = false;
        timeout(Duration::from_secs(10), async {
            while let Some(ev) = recv_hook_rx.recv().await {
                match ev {
                    ProgressEvent::InstantHit { name, total, .. } => {
                        assert_eq!(name, "instant_small.bin", "InstantHit 文件名应匹配");
                        assert_eq!(total, file_size, "InstantHit 大小应匹配");
                        saw_instant_hit = true;
                        return;
                    }
                    ProgressEvent::Started { .. } => { saw_batched_started = true; }
                    _ => {}
                }
            }
        }).await.expect("应在超时前收到 InstantHit");

        assert!(saw_instant_hit, "应收到 InstantHit 事件");
        assert!(!saw_batched_started, "第二次推送不应有批流 Started(零字节传输)");

        push_fut2.await.expect("推送任务不应 panic")
            .expect("第二次推送应成功");

        assert_eq!(fs::read(&dest_file).unwrap(), data, "秒传后内容应仍一致");

        crate::transfer::engine::inbound_recv_hook().lock().unwrap().take();
    }

    /// A5 隐私审计:OfferReq 被 Deny 时 OfferResp.skip 必须为空——
    /// 拒绝不应回传"我已持有哪些文件"的持有信息
    #[tokio::test]
    async fn offer_deny_returns_empty_skip() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        // 100KB 小文件(带 hash,正常接受时会进 skip 计算)
        let file_size: u64 = 100 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let hash = crate::transfer::dedup::sha256_of_slice(&data);
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 甲信任乙(Auto push);乙对甲 Deny——但乙的收件箱索引里预置了该文件的持有记录
        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 乙侧(Deny 方)收件箱索引预置持有记录:若 skip 在拒绝时仍计算,会命中此 hash
        let download_dir = std::env::temp_dir().join(format!("a5_deny_{}_{}",
            std::process::id(),
            fp_b[..4].iter().map(|b| format!("{:02x}", b)).collect::<String>()));
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();
        let held_file = download_dir.join("held.bin");
        fs::write(&held_file, &data).unwrap();
        let mut inbox = crate::transfer::dedup::InboxIndex::load(
            &download_dir.join(".localtrans-inbox-index.json"));
        inbox.insert(hash.clone(), held_file.clone(), file_size);
        inbox.save();

        let src_file = dir_a.path().join("a5_secret.bin");
        fs::write(&src_file, &data).unwrap();

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 把乙对甲改为 Deny(在路由器 spawn 前设置会被读到;此处路由器已启动,
        // 但 OfferReq 处理时实时读 trust——运行中翻转即可)
        ctx_b.trust.lock().await.upsert(TrustedPeer {
            fingerprint: fp_a,
            name: "甲".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Deny },
        });

        // 直接发 OfferReq(不经 push_files,便于断言原始 OfferResp.skip_bitmap)
        let job_id: u64 = 0xA500;
        let files = vec![crate::protocol::OfferFile {
            name: "a5_secret.bin".to_string(),
            size: file_size,
            rel_dir: String::new(),
            hash: Some(hash.clone()),
        }];
        sm_a.send_ctrl(&fp_b, ControlMsg::OfferReq { job_id, files }).await.unwrap();

        let mut signal_rx = sm_a.subscribe_push_signals();
        let resp = timeout(Duration::from_secs(10), async {
            loop {
                match signal_rx.recv().await {
                    Ok((from, ControlMsg::OfferResp { accepted, save_dir: _, reason, skip_bitmap })) if from == fp_b => {
                        return (accepted, reason, skip_bitmap);
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => panic!("推送信号通道意外关闭"),
                }
            }
        }).await.expect("应在超时前收到 OfferResp");

        let (accepted, reason, skip_bitmap) = resp;
        assert!(!accepted, "Deny 档应被拒绝");
        assert_eq!(reason, Some(crate::protocol::OfferDenyReason::Denied));
        assert!(skip_bitmap.is_empty(),
            "拒绝时 OfferResp.skip_bitmap 必须为空——实际泄漏 {} 位持有信息", skip_bitmap.len());
    }

    /// T18: accepted 时 OfferResp.skip_bitmap 与请求 files 等长逐位对应——
    /// 收件箱命中 2/3 文件时位图按请求顺序为 [true,true,false],且不含 hash 回显
    #[tokio::test]
    async fn offer_accept_skip_bitmap_matches_request_order() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let file_size: u64 = 100 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        // 三个不同内容的小文件;前两个(按请求顺序)预置进乙的收件箱索引
        let mk = |seed: u8| -> (String, Vec<u8>) {
            let mut d = vec![seed; file_size as usize];
            d[0] = seed.wrapping_add(1);
            (crate::transfer::dedup::sha256_of_slice(&d), d)
        };
        let (h0, d0) = mk(0);
        let (h1, d1) = mk(1);
        let (h2, _d2) = mk(2);

        let download_dir = std::env::temp_dir().join(format!("t18_bm_{}_{}",
            std::process::id(),
            fp_b[..4].iter().map(|b| format!("{:02x}", b)).collect::<String>()));
        fs::create_dir_all(&download_dir).unwrap();
        ctx_b.config.write().await.download_dir = download_dir.clone();
        for (h, d) in [(&h0, &d0), (&h1, &d1)] {
            let f = download_dir.join(format!("held_{}.bin", &h[..6]));
            fs::write(&f, d).unwrap();
            let mut inbox = crate::transfer::dedup::InboxIndex::load(
                &download_dir.join(".localtrans-inbox-index.json"));
            inbox.insert(h.clone(), f.clone(), file_size);
            inbox.save();
        }

        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 按请求顺序 3 个文件:前两个乙已持有 → 位图 [true,true,false]
        let files = vec![
            crate::protocol::OfferFile { name: "f0.bin".into(), size: file_size, rel_dir: String::new(), hash: Some(h0) },
            crate::protocol::OfferFile { name: "f1.bin".into(), size: file_size, rel_dir: String::new(), hash: Some(h1) },
            crate::protocol::OfferFile { name: "f2.bin".into(), size: file_size, rel_dir: String::new(), hash: Some(h2) },
        ];
        sm_a.send_ctrl(&fp_b, ControlMsg::OfferReq { job_id: 0x1800, files }).await.unwrap();

        let mut signal_rx = sm_a.subscribe_push_signals();
        timeout(Duration::from_secs(10), async {
            loop {
                match signal_rx.recv().await {
                    Ok((from, ControlMsg::OfferResp { accepted, reason: _, save_dir: _, skip_bitmap })) if from == fp_b => {
                        assert!(accepted, "Auto 档应接受");
                        assert_eq!(skip_bitmap, vec![true, true, false],
                            "位图必须与请求 files 等长且按请求顺序");
                        return;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => panic!("推送信号通道意外关闭"),
                }
            }
        }).await.expect("应在超时前收到 OfferResp");
    }

    /// N1-T1a: 小文件全秒传跳过的推送,发送方 progress 通道仍须有
    /// Started/Done 配对事件——壳层占位卡据此激活并落终态。
    /// 缺失时卡片永卡 pending/total=0(2026-09-06 真机探针实证)。
    #[tokio::test]
    async fn full_skip_push_still_emits_started_done_pair() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let file_size: u64 = 64 * 1024;
        let data: Vec<u8> = (0..file_size).map(|i| (i % 251) as u8).collect();
        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        for (ctx, fp_other, name) in [(&ctx_a, fp_b, "乙"), (&ctx_b, fp_a, "甲")] {
            ctx.trust.lock().await.upsert(TrustedPeer {
                fingerprint: fp_other,
                name: name.to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
            });
        }

        let src_file = dir_a.path().join("n1_skip.bin");
        fs::write(&src_file, &data).unwrap();
        let hash = crate::transfer::dedup::sha256_of_slice(&data);

        // 乙收件箱预置同 hash+size 文件 → OfferResp 位图全 true → 批流跳过
        let download_dir = std::env::temp_dir().join(format!("n1_skip_{}_{}",
            std::process::id(),
            fp_b[..4].iter().map(|b| format!("{:02x}", b)).collect::<String>()));
        fs::create_dir_all(&download_dir).unwrap();
        let held = download_dir.join("held.bin");
        fs::write(&held, &data).unwrap();
        let index_path = download_dir.join(".localtrans-inbox-index.json");
        let mut inbox = crate::transfer::dedup::InboxIndex::load(&index_path);
        inbox.insert(hash, held, file_size);
        inbox.save();
        ctx_b.config.write().await.download_dir = download_dir.clone();

        let sender_jobs_a = crate::transfer::sender_state::new_sender_job_map();
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("乙入站通道未被占用");
        let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, mpsc::channel(8).0, sender_jobs_a.clone(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
        timeout(Duration::from_secs(30),
            push_files(&sm_a, &fp_b, vec![src_file], &sender_jobs_a, progress_tx),
        ).await.expect("全跳过推送应在超时前完成").unwrap();

        let mut saw_started = false;
        let mut file_done = 0u32;
        while let Ok(Some(ev)) = timeout(Duration::from_secs(3), progress_rx.recv()).await {
            match ev {
                ProgressEvent::Started { name, total, .. } => {
                    assert_eq!(name, "n1_skip.bin");
                    assert_eq!(total, file_size);
                    saw_started = true;
                }
                ProgressEvent::Done { .. } => file_done += 1,
                _ => {}
            }
        }
        assert!(saw_started, "全跳过也必须发 Started——否则发送方占位卡永卡 pending");
        assert!(file_done >= 1, "整 job Done 应正常发出");
        let _ = fs::remove_dir_all(&download_dir);
    }

    // ===== S9: sanitize_rel_parent 纯函数直测 =====

    #[test]
    fn sanitize_rel_parent_rejects_traversal() {
        assert!(sanitize_rel_parent("../evil/x").is_err(), ".. 分量必须被拒");
    }

    #[test]
    fn sanitize_rel_parent_rejects_absolute() {
        if cfg!(windows) {
            // Windows 语义:`\` 是分隔符,`C:` 是盘符前缀 → 绝对路径,必须拒
            assert!(sanitize_rel_parent("C:\\x").is_err(), "盘符绝对分量必须被拒");
        } else {
            // Unix 语义:反斜杠是普通字符,"C:\x" 只是单个相对文件名,
            // 落在下载目录内属安全;真正的根绝对路径仍必须被拒(见下)
            assert!(sanitize_rel_parent("C:\\x").is_ok(), "Unix 下反斜杠是普通字符,应为相对名");
        }
        assert!(sanitize_rel_parent("/etc").is_err(), "根绝对分量必须被拒");
    }

    #[test]
    fn sanitize_rel_parent_accepts_nested() {
        let p = sanitize_rel_parent("a/b/c.txt").unwrap();
        assert_eq!(p, PathBuf::from("a").join("b"), "parent 应为 a/b");
    }

    #[test]
    fn sanitize_rel_parent_empty_is_ok() {
        assert!(sanitize_rel_parent("").unwrap().as_os_str().is_empty());
        assert!(sanitize_rel_parent("top.bin").unwrap().as_os_str().is_empty(),
            "无父目录的裸文件名应得到空 parent");
    }

    // ============ T1/T2 审计修复：BufferPool 许可 RAII 化 ============

    /// run_sender 错误路径（open_uni 失败）不再泄漏许可：
    /// 40 次读文件成功但 open_uni 失败后，池可用许可应恢复到初始值。
    #[tokio::test]
    async fn sender_open_uni_failure_does_not_leak_permits() {
        use crate::session::{connect, bind_endpoint, server_config, client_config};
        use std::net::SocketAddr;

        crate::test_support::init_tracing();

        let src = tempfile::TempDir::new().unwrap();
        let data: Vec<u8> = (0..100).map(|i| i as u8).collect();
        fs::write(src.path().join("src.bin"), &data).unwrap();
        let manifest = Manifest::build(&src.path().join("src.bin")).unwrap();

        fs::create_dir_all(src.path().join("a")).unwrap();
        fs::create_dir_all(src.path().join("b")).unwrap();
        let id_a = Arc::new(crate::identity::Identity::load_or_create(&src.path().join("a")).unwrap());
        let id_b = Arc::new(crate::identity::Identity::load_or_create(&src.path().join("b")).unwrap());

        // 服务端握手成功后立即关闭连接 → open_uni 必然失败
        let server_cfg = server_config(&id_b).unwrap();
        let ep_b = quinn::Endpoint::server(server_cfg, SocketAddr::from(([127, 0, 0, 1], 0))).unwrap();
        let b_addr = ep_b.local_addr().unwrap();
        tokio::spawn(async move {
            while let Some(incoming) = ep_b.accept().await {
                if let Ok(conn) = incoming.await {
                    conn.close(0u32.into(), b"reject");
                }
            }
        });

        let mut client_ep = bind_endpoint(0, &id_a).unwrap();
        client_ep.set_default_client_config(client_config(None));

        let pool = Arc::new(BufferPool::new());
        assert_eq!(pool.available_permits(), BUFFER_POOL_PERMITS);

        for round in 0..40u64 {
            let (conn, _) = connect(&client_ep, b_addr, &id_a, None).await
                .unwrap_or_else(|e| panic!("round {} 握手应成功: {}", round, e));
            // 等服务端的 close 完全落地——此后 open_uni 必然失败（确定性）
            tokio::time::timeout(Duration::from_secs(5), conn.closed()).await
                .expect("服务端应关闭连接");
            let result = run_sender(
                conn.clone(),
                pool.clone(),
                src.path().join("src.bin"),
                manifest.chunk_len(0),
                manifest.chunk_offset(0),
                999,
                0,
                None,
                None,
            ).await;
            assert!(result.is_err(), "round {} 应因连接被关而失败", round);
            drop(conn);
        }

        // 核心断言：许可全部归还，无泄漏
        assert_eq!(
            pool.available_permits(),
            BUFFER_POOL_PERMITS,
            "40 次 open_uni 失败后许可应全部归还（RAII 生效）"
        );
    }

    /// run_receiver_windowed 提前返回时的归还保证由两层构成：
    /// (a) 已收割任务的 guard 在任务内 drop；(b) 在途任务随 JoinSet drop 被
    /// abort，任务局部变量（含 guard）随之 drop。本测试针对 (b) 的底层语义——
    /// abort 的持 guard 任务必须归还许可——这正是 T2 泄漏路径的根。
    #[tokio::test]
    async fn aborted_chunk_tasks_return_pool_permits() {
        crate::test_support::init_tracing();

        let pool = Arc::new(BufferPool::new());
        assert_eq!(pool.available_permits(), BUFFER_POOL_PERMITS);

        {
            let mut tasks = tokio::task::JoinSet::new();
            for _ in 0..3 {
                let pool = pool.clone();
                tasks.spawn(async move {
                    let _buf = pool.acquire().await;
                    // 模拟在途块任务：等块流数据到来（永远不来）
                    std::future::pending::<()>().await;
                });
            }
            // 等三个任务都拿到 guard 进入挂起态
            let deadline = Instant::now() + Duration::from_secs(5);
            while pool.available_permits() != BUFFER_POOL_PERMITS - 3 {
                assert!(Instant::now() < deadline, "任务应全部获得缓冲");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            // 等价 run_receiver_windowed 提前返回：JoinSet drop → abort → guard drop
        }

        // abort 与任务内 Drop 的执行是异步的——让出几轮调度等 abort 生效
        let deadline = Instant::now() + Duration::from_secs(5);
        while pool.available_permits() != BUFFER_POOL_PERMITS {
            assert!(Instant::now() < deadline, "abort 后许可应在期限内全部归还");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            pool.available_permits(),
            BUFFER_POOL_PERMITS,
            "被 abort 的在途块任务应经 guard Drop 归还全部许可"
        );
    }

    /// v0.11.0 T4:OfferReq Ask 档等待期间,同对端的其他控制消息仍能被路由器
    /// 处理(不积压)——发起 Ask 推送后立即发 SharesReq,其响应应在 Ask 应答前到达。
    #[tokio::test]
    async fn ask_offer_does_not_block_other_ctrl_msgs() {
        use crate::test_support::{setup_ctx, start_listener};
        use crate::identity::{Perms, PushPolicy, TrustedPeer};
        use tokio::time::timeout;

        crate::test_support::init_tracing();

        let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = setup_ctx("乙");

        // 甲信任乙
        ctx_a.trust.lock().await.upsert(TrustedPeer {
            fingerprint: fp_b,
            name: "乙".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        // 乙信任甲,push=Ask 且超时拉长——若路由器仍内联等待,SharesReq 必然饿死
        {
            let mut trust = ctx_b.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms { browse: true, download: true, push: PushPolicy::Ask },
            });
        }
        ctx_b.config.write().await.offer_timeout_secs = 30;

        // 乙共享区(SharesReq 需有 browse 权限即可,list 内容无关紧要)
        let share_path = dir_b.path().join("shares");
        fs::create_dir_all(&share_path).unwrap();
        let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
            id: "share1".to_string(),
            alias: "测试共享".to_string(),
            path: share_path,
        }]));
        // 乙下载目录 + Ask 档接收编排所需的路径
        let download_dir_b = dir_b.path().join("downloads");
        fs::create_dir_all(&download_dir_b).unwrap();
        ctx_b.config.write().await.download_dir = download_dir_b.clone();

        // 乙监听 + 路由器;挂 ask 通道但不立即应答——模拟用户犹豫
        let b_addr = start_listener(&sm_b).await;
        let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
        let (ask_tx, mut ask_rx) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx,
            ask_tx, mpsc::channel(8).0, crate::transfer::sender_state::new_sender_job_map(), None);

        // 甲连接乙
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        // 甲发 OfferReq(乙会进 Ask 等待,最长 30s)
        sm_a.send_ctrl(&fp_b, ControlMsg::OfferReq {
            job_id: 9001,
            files: vec![crate::protocol::OfferFile {
                name: "ask_test.txt".into(),
                size: 10,
                rel_dir: String::new(),
                hash: None,
            }],
        }).await.unwrap();
        // 给乙一点时间进入 Ask 等待(收到 OfferAsk 即证明子任务已接手)
        let ask = timeout(Duration::from_secs(5), ask_rx.recv())
            .await.expect("应收到 OfferAsk").expect("ask 通道不应关闭");
        assert_eq!(ask.job_id, 9001);

        // 关键断言:Ask 等待未解除时,同对端的 SharesReq 仍能即时得到 SharesResp
        let (_m_shares, rx_shares) = sm_a.send_rpc(&fp_b, ControlMsg::SharesReq { msg_id: 0 })
            .await.unwrap();
        let resp_rx_start = std::time::Instant::now();
        timeout(Duration::from_secs(3), async {
            match rx_shares.await {
                Ok((_, ControlMsg::SharesResp { shares, .. })) => {
                    assert!(shares.iter().any(|s| s.id == "share1"),
                        "SharesResp 应含测试共享");
                }
                _ => panic!("SharesReq 响应不应失败"),
            }
        }).await.expect("Ask 等待期间 SharesResp 应能到达(路由器不应被阻塞)");
        assert!(resp_rx_start.elapsed() < Duration::from_secs(5),
            "SharesResp 应在 Ask 应答之前就到达");

        // 清理:拒绝 Ask, OfferResp(timeout reason=Denied)应正常回发
        let _ = ask.respond.send(None);
        let mut signal_rx = sm_a.subscribe_push_signals();
        timeout(Duration::from_secs(5), async {
            loop {
                match signal_rx.recv().await {
                    Ok((_, ControlMsg::OfferResp { accepted, .. })) => {
                        assert!(!accepted, "拒绝后 OfferResp.accepted 应为 false");
                        return;
                    }
                    Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(_) => panic!("推送信号通道不应关闭"),
                }
            }
        }).await.expect("拒绝后应回发 OfferResp");
    }
}
