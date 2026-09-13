# 传输面板源端可视化 + 持久化 + 卡死修复 — 设计文档

- **日期**:2026-08-21
- **状态**:待用户审阅
- **上一版**:v0.1.9(2026-08-21)
- **项目路径**:`D:\localTrans`

---

## 1. 概述

v0.1.9 的 Transfers 页只显示**接收方**视角的传输:我拉取的、我收到的推送。发送方(我推出去的、我被拉取的)在 UI 上要么完全没有、要么永远卡在"等待中"、要么无法暂停 / 取消 / 限速 / 踢人。本设计同时解决三个相互纠缠的问题:

1. **源端缺失**——推送方看不到自己的上传进度、速率、健康度;服务方看不到谁在拉自己的哪个文件、用了多少带宽
2. **持久化缺失**——transfer 表全在内存,应用重启后所有传输信息丢失,只剩 `.localtrans-parts/` 目录里的续传痕迹
3. **卡"等待中"的 bug**——`placeholder_id` 与真实 `job_id` 的解耦逻辑有漏洞,导致部分场景下占位条目永远无法被替换或清理

外加两个交互层补强:
4. **u64 job_id 跨 JSON 边界精度丢失**——重启下载时报 `invalid type: floating point 1.8446744073709552e+19, expected u64`
5. **关闭应用时无提醒**——active 任务被静默中断,下次需要手动续传

**核心目标:**
- A. 发送方(推送方 + 服务方)在 Transfers 页看到实时进度 / 速率 / 健康度(丢包 / RTT / cwnd / 并发流)
- B. 发送方能 **暂停 / 继续 / 取消**(推送方)或 **限速 / 踢人**(服务方)
- C. 所有 TransferDto 持久化到 `transfers.json`,重启后恢复
- D. 关闭应用时若仍有 active 任务,弹模态让用户确认
- E. 修 u64 精度 bug + "等待中"卡死 bug

**非目标(明确不做,本轮保功能,下一轮优化交互):**
- 历史传输的远程同步 / 多端共享
- 智能预判(预测完成时间、智能调速算法)
- 任务编排 / 优先级队列
- 限速滑块(本轮用预设档位 1/2/4/不限)
- Tauri 原生模态(本轮复用浏览器原生 confirm;原生模态下一轮)

---

## 2. 已确认的设计决策

| 决策点 | 结论 |
|--------|------|
| TransferDto 角色模型 | 加 `local_role: destination / source-push / source-pull`,单表不分桶 |
| TransferDto state 新值 | 新增 `interrupted`(重启前回 active/pending,重启后无后台任务) |
| 页面布局 | 混合列表 + 角色徽章(左侧色块 + 头部小标签) |
| 健康度语义 | 网络层三指标直出:丢包率 + RTT + 拥塞窗口 + 并发流数 |
| 关闭拦截触发 | 仅 active 状态 |
| 关闭弹窗选项 | 取消 / 仍要关闭 |
| 三件套覆盖范围 | 全部角色(destination + source-push + source-pull) |
| 持久化粒度 | 仅表快照(transfers.json 整体序列化) |
| UI 清理入口 | "清除已完成/失败" 按钮 + 单任务"删除"按钮(带二次确认) |
| u64 序列化修复 | Rust u64 在 JSON 里序列化为字符串(hex),前端存字符串 |
| Throttle 实现 | sender 侧维护 `active_streams` 计数,FetchReq 时若 ≥ cap 直接拒绝(接收方 batch 内重试) |
| 关闭拦截实现 | Tauri `onCloseRequested` 事件 + 前端模态回传决定 |

---

## 3. 数据模型

### 3.1 `ProgressEvent`(engine 侧)

```rust
#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProgressEvent {
    // ===== 现有(保持兼容,语义不变)=====
    Started { job_id: u64, name: String, total: u64 },
    ChunkDone { job_id: u64, chunk: u32, bytes: u64 },
    Speed { job_id: u64, bps: u64 },
    Done { job_id: u64 },
    Failed { job_id: u64, reason: String },

    // ===== 新增:发送方视角(按角色)=====
    SourceStarted {
        job_id: u64,
        role: SourceRole,
        peer: Fingerprint,
        name: String,
        total: u64,
    },
    SourceChunkDone { job_id: u64, chunk: u32, bytes: u64 },
    SourceSpeed {
        job_id: u64,
        bps: u64,
        loss_ratio: f64,
        rtt_ms: u64,
        cwnd: u32,
        streams: u32,
    },
    SourceDone { job_id: u64 },
    SourceFailed { job_id: u64, reason: String },
}

#[derive(Serialize, Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    SourcePush,   // 我推给对端
    SourcePull,   // 对端从我这里拉
}
```

**关键点:**
- 不破坏现有接收方路径,`Started/ChunkDone/Speed/Done/Failed` 不动
- Sender 走独立变体但**走同一个 mpsc 通道**,Tauri 端一个 match 就能分两路
- `SourceSpeed` 一次性带齐健康三指标,避免 UI 拼凑
- `SourceStarted` 带 `role` 是因为同一种 `SourceStarted` 变体既覆盖 source-push 又覆盖 source-pull,Tauri 路由时直接读 role 字段填 `local_role`

### 3.2 `TransferDto`(前后端共享,带 `serde(default)` 兼容老前端)

```rust
#[derive(Serialize, Deserialize, Clone)]
pub struct TransferDto {
    // ===== 现有字段(保持兼容)=====
    pub job_id: u64,                  // 序列化为 hex 字符串(见 3.3)
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,                // "pending" | "active" | "paused" | "done" | "failed" | "interrupted"
    pub speed_bps: u64,
    pub peer: String,
    pub direction: String,            // "pull" | "push"

    // ===== 新增字段(serde default)=====
    #[serde(default)]
    pub local_role: String,           // "destination" | "source-push" | "source-pull"
    #[serde(default)]
    pub health: Option<HealthDto>,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct HealthDto {
    pub loss_ratio: f64,
    pub rtt_ms: u64,
    pub cwnd: u32,
    pub streams: u32,
}
```

**state 字段值集合:**
- `pending`: 占位任务,排队 / 元数据协商中
- `active`: 进行中
- `paused`: 已暂停(本端操作)
- `done`: 完成
- `failed`: 失败
- `interrupted`: **新增**——重启前回 active/pending,重启后无后台任务接管

### 3.3 u64 job_id 序列化为 hex 字符串

修 `1.8446744073709552e+19` 这类 floating point 精度 bug。

**Rust 侧(自定义 serde 模块):**

```rust
// crates/localtrans-core/src/serde_compat.rs (新增)
pub mod u64_hex_string {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{:016x}", v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        u64::from_str_radix(s.trim_start_matches("0x"), 16)
            .map_err(serde::de::Error::custom)
    }
}
```

**应用到以下字段(全部加 `#[serde(with = "u64_hex_string")]`):**
- `TransferDto::job_id`(src-tauri 主 DTO)
- `protocol::ControlMsg::BitmapReq { job_id }`
- `protocol::ControlMsg::BitmapResp { job_id }`
- `protocol::ControlMsg::TransferCtl { job_id }`
- `protocol::ControlMsg::JobDone { job_id, offer_id }`

**前端 `types.ts` 同步:**
```ts
export interface TransferDto {
  job_id: string  // was: number
  // ...
}
```

**Tauri command 参数解析:** Tauri 2 自动 camelCase → snake_case 转换,前端 `invoke('xxx', { jobId: '0000000000000001' })` 后端收 `job_id: "0000000000000001"`,再用 `u64_hex_string::deserialize` 解析回 u64。

**兼容性:** 老前端不知道 job_id 是字符串,会把字符串 `===` 数字比较失败——这是**强制迁移**,文档明确告诉用户升级后必须用新前端。

### 3.4 Sender-side 状态(`SenderJobState`)

```rust
pub struct SenderJobState {
    pub job_id: u64,
    pub src_path: PathBuf,
    pub manifest: Manifest,
    pub dest_hint: Option<(String, String)>,  // 推送模式用

    // 进度反馈
    pub bytes_counter: Arc<AtomicU64>,
    pub active_streams: Arc<AtomicU32>,

    // 控制
    pub throttle_cap: Arc<AtomicU32>,  // 默认 u32::MAX,Throttle 时改写
    pub progress_tx: mpsc::Sender<ProgressEvent>,

    // 探测任务生命周期
    pub probe_stop_tx: Option<oneshot::Sender<()>>,  // SourceDone/Failed 时 send() 退出探测任务
}
```

**存储位置:** `AppState::sender_jobs: Arc<RwLock<HashMap<u64, Arc<SenderJobState>>>>`

**生命周期:**
- 创建: `MetaReq` 命中后(spawn_rpc_router 收到 MetaReq)→ 分配 `next_source_job_id()` → 建 state → 启动 SourceProbe 任务 → 注册到 AppState.sender_jobs
- 清理: `SourceDone` / `SourceFailed` 发出后 `probe_stop_tx.send(())` 关探测任务 + `sender_jobs.remove(job_id)`
- 连接断开兜底:每个 SenderJobState 启动时同时 spawn 一个 30s 计时任务,监听 `conn.closed()`,连接关闭后等 30s 让上层收齐残余事件,然后无条件 `SourceFailed { reason: "连接关闭" }` + 清理(防止 sender_jobs 无限堆积)

---

## 4. Engine 改动

### 4.1 Sender-side 进度事件

**新增 SourceProbe 任务**(与接收方 `start_pull` 内的探测任务对偶):

```rust
async fn run_source_probe(
    conn: Connection,
    job_id: u64,
    progress: mpsc::Sender<ProgressEvent>,
    bytes_counter: Arc<AtomicU64>,
    active_streams: Arc<AtomicU32>,
    throttle_cap: Arc<AtomicU32>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(500));
    let mut last_bytes = 0u64;
    let mut last_time = Instant::now();
    loop {
        tokio::select! {
            _ = interval.tick() {}
            _ = &mut stop_rx => break,
            _ = conn.closed() => break,
        }
        let stats = conn.stats().path;
        let now = Instant::now();
        let elapsed = now.duration_since(last_time).as_secs_f64();
        let cur = bytes_counter.load(Ordering::Relaxed);
        let bps = if elapsed > 0.0 && cur > last_bytes {
            ((cur - last_bytes) as f64 * 8.0 / elapsed) as u64
        } else { 0 };
        let loss_ratio = if stats.sent_packets > 0 {
            stats.lost_packets as f64 / stats.sent_packets as f64
        } else { 0.0 };
        let _ = progress.send(ProgressEvent::SourceSpeed {
            job_id, bps, loss_ratio,
            rtt_ms: stats.rtt.as_millis() as u64,
            cwnd: stats.cwnd,
            streams: active_streams.load(Ordering::Relaxed),
        }).await;
        last_bytes = cur; last_time = now;
    }
}
```

**`run_sender` 加进度回调参数:**

```rust
pub async fn run_sender(
    conn: Connection,
    pool: Arc<BufferPool>,
    src_path: PathBuf,
    manifest: Manifest,
    job_id: u64,
    chunk: u32,
    bytes_counter: Option<Arc<AtomicU64>>,  // 新增
    progress: Option<mpsc::Sender<ProgressEvent>>,  // 新增
) -> Result<(), EngineError> {
    // ... 现有读块/写流逻辑 ...
    if let Some(counter) = &bytes_counter {
        counter.fetch_add(chunk_len as u64, Ordering::Relaxed);
    }
    if let Some(tx) = &progress {
        let _ = tx.send(ProgressEvent::SourceChunkDone {
            job_id, chunk, bytes: chunk_len as u64,
        }).await;
    }
    // ...
}
```

**`spawn_rpc_router` 的 `FetchReq` handler 改造:**

```rust
ControlMsg::FetchReq { job_id, chunk } => {
    // 权限校验(现有)
    if !perms.download { continue; }

    // 拿到 sender job state
    let state = match sender_jobs.read().await.get(&job_id).cloned() {
        Some(s) => s,
        None => { tracing::warn!("sender 任务不存在: job {}", job_id); continue; }
    };

    // Throttle: 当前活跃流 ≥ cap → 拒绝本次
    let cur = state.active_streams.load(Ordering::Relaxed);
    let cap = state.throttle_cap.load(Ordering::Relaxed);
    if cur >= cap {
        tracing::debug!("节流拒绝: job {} (cur={}, cap={})", job_id, cur, cap);
        continue;  // 接收方 batch 内 60s 超时报错兜底;后续可优化为显式 TransferCtl
    }

    let conn = sm.session(&fingerprint).await.ok_or(...);
    tokio::spawn(async move {
        state.active_streams.fetch_add(1, Ordering::Relaxed);
        let res = run_sender(
            conn, pool, state.src_path.clone(),
            state.manifest.clone(), job_id, chunk,
            Some(state.bytes_counter.clone()),
            Some(state.progress_tx.clone()),
        ).await;
        state.active_streams.fetch_sub(1, Ordering::Relaxed);
        if let Err(e) = res {
            tracing::warn!("块流发送失败: job {} chunk {}: {}", job_id, chunk, e);
        }
    });
}
```

**`spawn_rpc_router` 的 `MetaReq` handler 改造:**

```rust
ControlMsg::MetaReq { share_id, path } => {
    // 权限校验(现有)
    if !perms.download { continue; }

    // 解析路径(现有 push:/普通 逻辑)
    let resolved = ...;

    let m = match Manifest::build(&resolved) { ... };
    let job_id = next_source_job_id();  // 新原子计数器,不复用 receiver 的 next_job_id
    let mut m_with_source = m.clone();
    m_with_source.peer = Some(hex::encode(fingerprint));
    m_with_source.share_id = Some(share_id.clone());
    m_with_source.rel = Some(path.clone());

    // 决定 role
    let role = if share_id.starts_with("push:") {
        SourceRole::SourcePush
    } else {
        SourceRole::SourcePull
    };

    // 建 SenderJobState
    let bytes_counter = Arc::new(AtomicU64::new(0));
    let active_streams = Arc::new(AtomicU32::new(0));
    let throttle_cap = Arc::new(AtomicU32::new(u32::MAX));
    let (probe_stop_tx, probe_stop_rx) = oneshot::channel();
    let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

    let state = Arc::new(SenderJobState {
        job_id,
        src_path: resolved,
        manifest: m.clone(),
        dest_hint: None,
        bytes_counter: bytes_counter.clone(),
        active_streams: active_streams.clone(),
        throttle_cap: throttle_cap.clone(),
        progress_tx: progress_tx.clone(),
    });

    // 注册到 AppState.sender_jobs
    sender_jobs.write().await.insert(job_id, state.clone());

    // 启动 SourceProbe
    let conn_for_probe = sm.session(&fingerprint).await
        .ok_or_else(|| "会话不存在,无法启动 sender 探测")?;
    let progress_for_probe = progress_tx.clone();
    tokio::spawn(run_source_probe(
        conn_for_probe, job_id,
        progress_for_probe,
        bytes_counter.clone(),
        active_streams.clone(),
        throttle_cap.clone(),
        probe_stop_rx,
    ));

    // 转发进度事件到上层(由 AppState 把 progress_tx 传进来;细节见 §5)
    let state_for_pump = state.clone();
    let app = ...; // AppHandle,从 spawn_rpc_router 签名扩展
    tokio::spawn(async move {
        while let Some(ev) = progress_rx.recv().await {
            let _ = app.emit("source-progress", &ev);  // 单独事件通道,避免污染 transfer-progress 4Hz 节流
        }
    });

    // 保留 probe_stop_tx 以便 SourceDone 后清理
    state.probe_stop_tx = Some(probe_stop_tx);  // SenderJobState 加字段

    // 发 MetaResp(现有)
    sm.send_ctrl(&fingerprint, ControlMsg::MetaResp { ... }).await;
}
```

**新增原子计数器:**

```rust
// crates/localtrans-core/src/session.rs
pub fn next_source_job_id() -> u64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
```

高 bit 段与 receiver 的 `next_job_id` 错开,避免理论上的 id 冲突,方便调试区分。

### 4.2 Sender-side 控制注册表 + Throttle 协议

**复用现有 `task_controls()` 注册表的 sender 版:**

```rust
// engine.rs
fn sender_task_controls() -> &'static std::sync::Mutex<HashMap<u64, TaskControlTx>> {
    static CTLS: OnceLock<Mutex<HashMap<u64, TaskControlTx>>> = OnceLock::new();
    CTLS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn control_sender_task(job_id: u64, ctl: TaskControl) -> bool {
    let tx = sender_task_controls().lock().unwrap().get(&job_id).cloned();
    match tx {
        Some(tx) => tx.try_send(ctl).is_ok(),
        None => false,
    }
}
```

Sender 任务的 control channel 什么时候注册?**当前不注册**——sender-side 的 Pause/Cancel 由 throttle_cap=0 + 关连接实现。Throttle 通过直接写 `throttle_cap` 实现,不进 control channel。

**Pause 语义(side sender):**
- Throttle cap 设为 0 → 后续 FetchReq 全拒绝 → 接收方 batch 超时报错 → 进 failed
- 但这是 fail 不是 pause,语义不准
- **本轮简化:** source-pull 不支持 Pause,只有"踢人"(Cancel 语义)+ "限速"(throttle_cap)
- **source-push 支持 Pause/Resume:** 在 push_files 内的 SenderJobState 注册 control channel,接收 TransferCtl 转发

**协议扩 Throttle 动作:**

```rust
// crates/localtrans-core/src/protocol.rs
pub enum TransferAction {
    Pause,
    Resume,
    Cancel,
    Throttle { max_streams: u32 },  // 新增
}
```

接收方收到 Throttle 是误用——`spawn_rpc_router` 看到 Throttle 走 sender 路径而不是 task_controls 路径,按 job_id 找 sender-side SenderJobState,写 throttle_cap;找不到则 warn。

### 4.3 Bug 修复 A:小文件 push 补 Started

`push_files` 当前在 `has_small` 分支只发 1 条聚合 Started,接收方 UI 看到一条聚合任务但没有 Done。改造:

```rust
if has_small {
    for (path, offer) in &small_files {
        let _ = progress.send(ProgressEvent::Started {
            job_id, name: offer.name.clone(), total: offer.size,
        }).await;
    }
    send_small_files_batched(&conn, job_id, small_files).await?;
    for (_, offer) in &small_files {
        let _ = progress.send(ProgressEvent::Done { job_id }).await;
    }
}
```

### 4.4 Bug 修复 B:Tauri handler Done/Failed 占位 fallback

`start_download` / `push_files` / `resume_pending` 三处的 spawn 任务里,`Done` / `Failed` 分支都加 fallback:

```rust
ProgressEvent::Done { job_id: j } => {
    if let Some(mut dto) = st.transfer_get_mut(j).await {
        dto.state = "done".into();
        st.transfer_update(j, dto).await;
    } else if placeholder_alive {
        // 占位 fallback:按 peer+direction 找到了
        if let Some(mut dto) = st.transfer_get_mut(placeholder_id).await {
            dto.state = "done".into();
            dto.job_id = j;  // 占位 id 换成真 job_id
            st.transfer_remove(placeholder_id).await;
            st.transfer_update(j, dto).await;
        }
        placeholder_alive = false;
    } else {
        tracing::warn!("Done 找不到对应条目: job {}", j);
    }
}
```

`Failed` 同理。

### 4.5 Bug 修复 C:启动清理

`AppState::new()` 加载 `transfers.json` 后:

```rust
// 扫描磁盘 parts 目录
let alive_parts: HashSet<u64> = std::fs::read_dir(&parts_root).ok()
    .map(|rd| rd.flatten()
        .filter_map(|e| e.file_name().to_str()
            .and_then(|s| u64::from_str_radix(s, 16).ok()))
        .collect())
    .unwrap_or_default();

let mut table = HashMap::new();
for mut dto in loaded {
    match dto.state.as_str() {
        "active" | "pending" => {
            // 重启后没后台任务 → interrupted
            dto.state = "interrupted".into();
            // parts 已被外部清理(应用卸载/手动删) → 不入表
            if !alive_parts.contains(&dto.job_id) {
                continue;
            }
        }
        _ => {}
    }
    table.insert(dto.job_id, dto);
}
```

---

## 5. Tauri / AppState 改动

### 5.1 AppState 扩展

```rust
pub struct AppState {
    // ... 现有字段 ...

    // 新增
    pub sender_jobs: Arc<RwLock<HashMap<u64, Arc<SenderJobState>>>>,
    pub save_throttle: Arc<Mutex<Option<()>>>,  // persist 节流标记
}
```

`spawn_rpc_router` 签名扩展接 AppHandle + sender_jobs:

```rust
pub fn spawn_rpc_router(
    sm: Arc<SessionManager>,
    ctx: SessionCtx,
    reg: Arc<ShareRegistry>,
    mut ctrl_rx: mpsc::Receiver<(Fingerprint, ControlMsg)>,
    ask_tx: mpsc::Sender<OfferAsk>,
    sender_jobs: Arc<RwLock<HashMap<u64, Arc<SenderJobState>>>>,  // 新增
    app: AppHandle,  // 新增,用于 emit source-progress 事件
)
```

### 5.2 新事件通道

```rust
// src-tauri/src/main.rs 或独立模块
const EVENT_SOURCE_PROGRESS: &str = "source-progress";  // sender-side 进度
const EVENT_TRANSFER_PROGRESS: &str = "transfer-progress";  // 现有,receiver-side 进度(保持)
```

**前端 `api.ts` 加监听:**

```ts
export function onSourceProgress(cb: (ev: ProgressEvent) => void) {
  return listen<ProgressEvent>(EVENT_SOURCE_PROGRESS, (e) => cb(e.payload))
}
```

**Pinia store 改造:**
- `setupEventListeners` 同时注册 transfer-progress 和 source-progress
- 两路事件走同一个 `scheduleUpdate` 函数(它内部按 job_id 去重/覆盖)

### 5.3 transfer_update / transfer_remove 集成持久化

```rust
pub async fn transfer_update(&self, id: u64, dto: TransferDto) {
    let mut table = self.transfer_table.write().await;
    table.insert(id, dto);
    drop(table);
    self.schedule_persist();
}

pub async fn transfer_remove(&self, id: u64) -> Option<TransferDto> {
    let removed = self.transfer_table.write().await.remove(&id);
    self.schedule_persist();
    removed
}

fn schedule_persist(&self) {
    let mut guard = self.save_throttle.lock().unwrap();
    if guard.is_some() { return; }  // 已有挂起的写任务
    *guard = Some(());
    let st = self.inner().clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let snapshot: Vec<TransferDto> = st.transfer_table_read().await.into_values().collect();
        if let Err(e) = save_transfers(&st.dir, &snapshot) {
            tracing::warn!("transfers.json 写盘失败: {}", e);
        }
        *st.save_throttle.lock().unwrap() = None;
    });
}
```

**写盘路径:** `data/transfers.json`(serde_json::to_string_pretty)。

**加载路径:** `AppState::new()` 时调 `load_transfers(&dir)`,失败 warn + 空表继续。

### 5.4 新 Tauri command

```rust
#[tauri::command]
async fn clear_completed_transfers(state: State<'_, AppState>) -> Result<usize, String> {
    let to_remove: Vec<u64> = state.transfer_table_read().await
        .into_iter()
        .filter(|(_, dto)| matches!(dto.state.as_str(),
            "done" | "failed" | "interrupted"))
        .map(|(id, _)| id).collect();
    let n = to_remove.len();
    for id in to_remove { state.transfer_remove(id).await; }
    Ok(n)
}

#[tauri::command]
async fn remove_transfer(
    state: State<'_, AppState>,
    job_id: String,                  // hex 字符串
    delete_parts: bool,
) -> Result<bool, String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let removed = state.transfer_remove(id).await.is_some();
    if removed && delete_parts {
        let parts_dir = state.config.read().await.download_dir
            .join(format!(".localtrans-parts/{:016x}", id));
        let _ = std::fs::remove_dir_all(&parts_dir);
    }
    Ok(removed)
}

#[tauri::command]
async fn transfer_throttle(
    state: State<'_, AppState>,
    job_id: String,
    max_streams: u32,
) -> Result<(), String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let jobs = state.sender_jobs.read().await;
    let entry = jobs.get(&id).ok_or_else(|| "任务不存在或不在 sender 端".to_string())?;
    entry.throttle_cap.store(max_streams.max(1), Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
async fn has_parts(state: State<'_, AppState>, job_id: String) -> Result<bool, String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let parts_dir = state.config.read().await.download_dir
        .join(format!(".localtrans-parts/{:016x}", id));
    Ok(parts_dir.exists())
}
```

### 5.5 关闭拦截

```rust
// main.rs 的 Tauri builder
.on_window_event(|window, event| {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();  // 默认阻止,前端决定
        let _ = window.emit("close-requested", ());
    }
})
```

**前端处理流程见 §6.6。**

---

## 6. 前端改动

### 6.1 `types.ts` 更新

```ts
export interface TransferDto {
  job_id: string                    // was: number
  name: string
  total: number
  done: number
  state: string                     // 新增 "interrupted"
  speed_bps: number
  peer: string
  direction: string
  local_role: LocalRole             // 新增
  health: HealthDto | null          // 新增
  started_at_ms: number | null      // 新增
}

export type LocalRole = 'destination' | 'source-push' | 'source-pull'

export interface HealthDto {
  loss_ratio: number
  rtt_ms: number
  cwnd: number
  streams: number
}

export interface ThrottleRequest {
  job_id: string
  max_streams: number
}

export interface SourceProgressEvent {
  // 与 Rust ProgressEvent 的 #[serde(tag = "type", rename_all = "snake_case")] 一致
  type: 'started' | 'chunk_done' | 'speed' | 'done' | 'failed'
       | 'source_started' | 'source_chunk_done' | 'source_speed' | 'source_done' | 'source_failed'
  job_id: string
  // 按 type 携带不同字段(Started/SourceStarted → name+total+[role, peer];
  //                     ChunkDone/SourceChunkDone → chunk+bytes;
  //                     Speed/SourceSpeed → bps+[loss_ratio, rtt_ms, cwnd, streams];
  //                     Done/SourceDone/Failed/SourceFailed → [reason])
}
```

### 6.2 `api.ts` 扩展

```ts
export const transfersApi = {
  list: () => invoke<TransferDto[]>('list_transfers'),
  pendingResumeJobs: () => invoke<[string, string][]>('pending_resume_jobs'),
  resumePending: (job_id: string) => invoke('resume_pending', { jobId: job_id }),
  clearCompleted: () => invoke<number>('clear_completed_transfers'),
  remove: (job_id: string, delete_parts: boolean) =>
    invoke<boolean>('remove_transfer', { jobId: job_id, deleteParts: delete_parts }),
  throttle: (job_id: string, max_streams: number) =>
    invoke<void>('transfer_throttle', { jobId: job_id, maxStreams: max_streams }),
  action: (job_id: string, action: TransferAction) =>
    invoke<void>('transfer_action', { jobId: job_id, action }),
  hasParts: (job_id: string) => invoke<boolean>('has_parts', { jobId: job_id }),
}

export function onSourceProgress(cb: (ev: ProgressEvent) => void) {
  return listen<ProgressEvent>('source-progress', (e) => cb(e.payload))
}
```

### 6.3 `stores/transfers.ts` 更新

```ts
function setupEventListeners() {
  onTransferProgress((progress) => scheduleUpdate(progress.jobs))
  onSourceProgress((ev) => {
    // source-progress 单条事件,转成单元素数组走 scheduleUpdate
    scheduleUpdate(buildSourceDto(ev))
  })
}

async function clearCompleted() {
  const n = await api.transfers.clearCompleted()
  await refreshTransfers()
  return n
}

async function removeTransfer(job_id: string, delete_parts: boolean) {
  await api.transfers.remove(job_id, delete_parts)
  transfers.value = transfers.value.filter(t => t.job_id !== job_id)
}

async function throttleTransfer(job_id: string, max_streams: number) {
  await api.transfers.throttle(job_id, max_streams)
  // toast 在组件层做
}
```

### 6.4 `components/TransferItem.vue` 改造

**角色色块 + 标签:**

```vue
<div class="transfer-item" :class="[
  `transfer-${transfer.state}`,
  `transfer-role-${transfer.local_role || 'destination'}`
]">
```

```css
.transfer-role-destination { border-left: 4px solid #10b981; }
.transfer-role-source-push { border-left: 4px solid #f59e0b; }
.transfer-role-source-pull { border-left: 4px solid #f97316; }
```

**头部小标签:**
```vue
<div class="transfer-info">
  <div class="transfer-name">{{ transfer.name }}</div>
  <div class="transfer-meta">
    <span class="role-badge">{{ roleText }}</span>
    <span class="peer">{{ peerName }}</span>
  </div>
</div>
```

```ts
const roleText = computed(() => {
  return {
    'destination': '接收中',
    'source-push': '推送中',
    'source-pull': '被取中',
  }[props.transfer.local_role] || '传输中'
})
```

**健康面板(active 时显示):**
```vue
<div v-if="transfer.state === 'active' && transfer.health" class="health-panel">
  <span class="metric">丢包 {{ (transfer.health.loss_ratio * 100).toFixed(1) }}%</span>
  <span class="metric">RTT {{ transfer.health.rtt_ms }}ms</span>
  <span class="metric">cwnd {{ transfer.health.cwnd }}</span>
  <span class="metric">{{ transfer.health.streams }} 流</span>
</div>
```

**ETA / 已用时间(active 时显示):**
```ts
const elapsedSeconds = computed(() => {
  if (!props.transfer.started_at_ms) return null
  return Math.floor((Date.now() - props.transfer.started_at_ms) / 1000)
})
const etaSeconds = computed(() => {
  if (!props.transfer.started_at_ms) return null
  if (props.transfer.speed_bps <= 0) return null
  if (props.transfer.done >= props.transfer.total) return 0
  return Math.ceil((props.transfer.total - props.transfer.done) * 8 / props.transfer.speed_bps)
})
```

```vue
<div class="time-info">
  <span v-if="elapsedSeconds !== null">已用 {{ formatDuration(elapsedSeconds) }}</span>
  <span v-if="etaSeconds !== null && etaSeconds > 0">剩余 {{ formatDuration(etaSeconds) }}</span>
</div>
```

**按钮区(按角色 + 状态分支):**

```vue
<div class="transfer-actions">
  <!-- destination:暂停/继续/取消 + 重试/续传/删除 -->
  <template v-if="transfer.local_role === 'destination'">
    <template v-if="transfer.state === 'active'">
      <button @click="handlePause" class="btn btn-warning">暂停</button>
      <button @click="handleCancel" class="btn btn-danger">取消</button>
    </template>
    <template v-else-if="transfer.state === 'paused'">
      <button @click="handleResume" class="btn btn-primary">继续</button>
      <button @click="handleCancel" class="btn btn-danger">取消</button>
    </template>
    <template v-else-if="['failed', 'interrupted'].includes(transfer.state)">
      <button v-if="hasRequestRecord" @click="handleRetry" class="btn btn-primary">重试</button>
      <button @click="handleResumePending" class="btn btn-primary">续传</button>
      <button @click="handleRemove" class="btn btn-secondary">删除</button>
    </template>
    <template v-else-if="transfer.state === 'done'">
      <button @click="handleOpenFolder" class="btn btn-success">打开所在文件夹</button>
      <button @click="handleRemove" class="btn btn-secondary">删除</button>
    </template>
  </template>

  <!-- source-push:暂停/继续/取消 + 删除 -->
  <template v-else-if="transfer.local_role === 'source-push'">
    <template v-if="transfer.state === 'active'">
      <button @click="handlePause" class="btn btn-warning">暂停</button>
      <button @click="handleCancel" class="btn btn-danger">取消</button>
    </template>
    <template v-else-if="transfer.state === 'paused'">
      <button @click="handleResume" class="btn btn-primary">继续</button>
      <button @click="handleCancel" class="btn btn-danger">取消</button>
    </template>
    <template v-else>
      <button @click="handleRemove" class="btn btn-secondary">删除</button>
    </template>
  </template>

  <!-- source-pull:限速 / 踢人 -->
  <template v-else-if="transfer.local_role === 'source-pull'">
    <template v-if="transfer.state === 'active'">
      <button @click="throttleMenuOpen = !throttleMenuOpen" class="btn btn-warning">限速</button>
      <div v-if="throttleMenuOpen" class="throttle-menu" v-on-click-outside="() => throttleMenuOpen = false">
        <button @click="throttleTo(1)">1 流</button>
        <button @click="throttleTo(2)">2 流</button>
        <button @click="throttleTo(4)">4 流</button>
        <button @click="throttleTo(0)">不限</button>
      </div>
      <button @click="handleKick" class="btn btn-danger">踢人</button>
    </template>
    <template v-else>
      <button @click="handleRemove" class="btn btn-secondary">删除</button>
    </template>
  </template>
</div>
```

**handleKick / throttleTo / handleRemove / handleResumePending:**

```ts
async function handleKick() {
  if (!confirm(`踢出对端 ${peerName.value} 的拉取?此操作不可恢复`)) return
  await transfersStore.transferAction(props.transfer.job_id, 'cancel')
  toastStore.push('info', '已踢出对端')
}

async function throttleTo(maxStreams: number) {
  throttleMenuOpen.value = false
  await transfersStore.throttleTransfer(props.transfer.job_id, maxStreams || 0xFFFFFFFF)
  toastStore.push('info', maxStreams ? `已限速到 ${maxStreams} 流` : '已取消限速')
}

async function handleRemove() {
  const hasParts = await api.transfers.hasParts(props.transfer.job_id)
  if (hasParts) {
    if (!confirm('该任务在磁盘上有未完成分块,确定删除吗?此操作不可恢复')) return
    await transfersStore.removeTransfer(props.transfer.job_id, true)
  } else {
    await transfersStore.removeTransfer(props.transfer.job_id, false)
  }
  toastStore.push('info', '任务已删除')
}

async function handleResumePending() {
  // interrupted 任务的"续传"等同于从 pending_jobs 选该 job_id
  await api.transfers.resumePending(props.transfer.job_id)
}
```

### 6.5 `pages/Transfers.vue` 改造

**顶部工具栏加按钮:**
```vue
<div class="page-header">
  <h1>传输任务</h1>
  <div class="header-actions">
    <button @click="handleRefresh" class="btn-refresh" :disabled="loading">刷新</button>
    <button @click="handleClearCompleted" class="btn-secondary">清除已完成/失败</button>
  </div>
</div>
```

```ts
async function handleClearCompleted() {
  const n = await transfersStore.clearCompleted()
  toastStore.push('success', `已清除 ${n} 个历史任务`)
}
```

**空状态文案补强:**
```vue
<div v-else-if="transfers.length === 0" class="empty-state">
  <p>暂无传输任务</p>
  <p class="hint">试试从设备页发起传输,或在浏览页面下载文件</p>
</div>
```

### 6.6 关闭拦截

**新建 `composables/useCloseGuard.ts`:**

```ts
import { ref, watch } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useTransfersStore } from '../stores/transfers'

export function useCloseGuard() {
  const transfersStore = useTransfersStore()
  const guardOpen = ref(false)
  const activeCount = ref(0)

  let unlisten: (() => void) | null = null

  async function init() {
    const win = getCurrentWindow()
    unlisten = await win.onCloseRequested(async (event) => {
      const active = transfersStore.transfers.filter(t => t.state === 'active')
      if (active.length === 0) return  // 放行

      event.preventDefault()
      activeCount.value = active.length
      guardOpen.value = true
      // 等用户决定
      await new Promise<void>((resolve) => {
        const stop = watch(guardOpen, (open) => {
          if (!open) {
            stop()
            resolve()
          }
        })
      })
      if (forceClose.value) {
        await win.destroy()
      }
    })
  }

  function cancelClose() {
    forceClose.value = false
    guardOpen.value = false
  }
  function doForceClose() {
    forceClose.value = true
    guardOpen.value = false
  }

  return { guardOpen, activeCount, init, cancelClose, doForceClose }
}
```

**`App.vue` 顶层挂载模态:**
```vue
<Teleport to="body">
  <div v-if="closeGuard.guardOpen.value" class="modal-backdrop">
    <div class="modal-card">
      <h3>有 {{ closeGuard.activeCount.value }} 个传输正在进行</h3>
      <p>关闭窗口会中断这些传输,下次需要手动续传。</p>
      <div class="modal-actions">
        <button @click="closeGuard.cancelClose()" class="btn-secondary">取消关闭</button>
        <button @click="closeGuard.doForceClose()" class="btn-danger">仍要关闭</button>
      </div>
    </div>
  </div>
</Teleport>
```

---

## 7. 错误处理与边界

| 场景 | 处理 |
|------|------|
| SourceSpeed 探测在连接断时退出 | `tokio::select!` 监听 `conn.closed()`,退出前发最后一次 SourceSpeed(可选) |
| Sender 侧 SourceDone 没正常发(连接挂) | SourceFailed { reason: "连接关闭" } 兜底;sender_jobs 在 30s 超时后清理 |
| 接收方 FetchReq 被节流拒绝 | 接收方 start_pull 当前批次循环会因未收齐块流而 60s 超时报错——本轮不优化,后续可发显式 TransferCtl |
| Receiver 收到 Throttle 协议帧 | warn + 忽略,不 panic |
| 节流 cap=0 | 默认最小值 1(代码内 `.max(1)` 兜底) |
| transfers.json 加载失败(损坏) | warn + 空表继续;下次 update 时被覆盖 |
| transfers.json 写盘失败(磁盘满) | warn,不影响内存表,下次重试 |
| 并发 transfer_update 触发并发 persist | schedule_persist 节流 1s 内合并写 |
| transfer_throttle 对非 sender 任务调用 | 返回错误"任务不存在或不在 sender 端" |
| remove_transfer 时 parts 已被外部删除 | ignore,state 表里照样移除 |
| 关闭拦截时用户重复点 X | Tauri onCloseRequested 默认阻止,模态必须选才能走;第二次 onCloseRequested 触发时若模态已开直接 return |
| 限速菜单打开时其他操作 | 菜单点击外部区域关闭(click-outside 指令) |
| 删除带 parts 的任务 | `confirm()` 二次确认;按 §6.4 handleRemove 实现 |
| 传输列表为空时显示什么 | 保留现有 empty-state + 提示"试试从设备页发起传输" |
| Source progress 事件丢失(mpsc 满) | channel size=64;满了就丢,SourceProbe 不会阻塞 |

---

## 8. 测试策略

### 8.1 引擎层单测(crates/localtrans-core)

```rust
#[test] sender_speed_emits_correct_bps_loss_rtt_cwnd_streams
  // mock quinn stats,验证 SourceSpeed 字段值

#[test] run_sender_updates_bytes_counter_on_success

#[test] run_sender_updates_bytes_counter_on_error

#[test] sender_throttle_cap_1_rejects_second_concurrent_fetch
  // 建 SenderJobState,cap=1,启动两个并发 run_sender → 第二个被拒绝

#[test] sender_throttle_cap_max_no_rejection

#[test] u64_job_id_roundtrips_json_without_precision_loss
  // assert json.contains("\"job_id\":\"<hex>\"")
  // 反序列化回 u64 不变

#[test] push_files_only_small_files_emits_started_and_done_per_file

#[test] start_pull_meta_req_timeout_returns_err

#[test] control_sender_task_returns_false_when_not_registered

#[test] control_sender_task_returns_true_when_registered
```

### 8.2 引擎层集成测试

```rust
#[tokio::test] full_duplex_pull_emits_source_events
  // 甲发起 pull,乙(sender)收到 ChunkDone + Speed + Done

#[tokio::test] full_duplex_push_with_large_files_emits_source_events
  // 甲 push 含大文件,甲(sender)看到每大文件 Started + Speed + Done

#[tokio::test] sender_jobs_cleaned_after_source_done
  // 完成 30s 后 sender_jobs 应被清理

#[tokio::test] sender_jobs_cleaned_after_connection_drop_30s
  // 连接断开兜底
```

### 8.3 Tauri 命令层测试

```rust
#[tokio::test] clear_completed_transfers_removes_done_failed_interrupted

#[tokio::test] remove_transfer_without_parts_only_removes_state

#[tokio::test] remove_transfer_with_parts_also_deletes_disk_dir

#[tokio::test] transfer_throttle_writes_to_sender_jobs_throttle_cap

#[tokio::test] transfer_throttle_non_sender_returns_error

#[tokio::test] persistence_roundtrip_100_transfers

#[tokio::test] startup_load_migrates_active_pending_to_interrupted

#[tokio::test] startup_load_drops_active_without_parts

#[tokio::test] startup_load_corrupt_json_continues_with_empty_table
```

### 8.4 前端测试(vitest)

```ts
// TransferItem.test.ts
test('destination + active 显示暂停/取消')
test('destination + paused 显示继续/取消')
test('destination + failed 显示重试/续传/删除')
test('destination + done 显示打开/删除')
test('source-pull + active 显示限速/踢人')
test('source-push + active 显示暂停/取消')
test('健康面板在 active + health 存在时显示三指标')
test('ETA 在 speed_bps > 0 + total > done 时显示秒数')
test('ETA 在 speed_bps = 0 时显示 "—"')
test('限速菜单点击 → throttleTransfer 被调用')
test('踢人按钮 → confirm + transferAction(cancel)')
test('删除按钮 → hasParts + confirm + removeTransfer')

// transfersStore.test.ts
test('transferAction throttle 路由到 throttleTransfer')
test('clearCompleted 调用 api + refresh')
test('removeTransfer(deleteParts=true) 调用 + 本地 filter')
test('sourceProgress 事件触发 scheduleUpdate')

// TransfersPage.test.ts
test('"清除已完成/失败" 按钮存在')
test('空状态文案')
test('续传横幅交互(保留现有)')

// useCloseGuard.test.ts
test('有 active 时阻止关闭并打开模态')
test('无 active 时放行关闭')
test('模态点取消关闭 → 窗口保持')
test('模态点仍要关闭 → window.destroy')
```

### 8.5 手动验证清单

```
- [ ] 双机拉取:甲发起下载,乙的 Transfers 页同时看到橙色"被取中"条目,进度实时滚动
- [ ] 双机推送(含大文件):甲 Transfers 页看到黄色进度条 + ChunkDone + Speed + Done
- [ ] sender-side 限速:甲把对端的 cap 设成 1 流,乙端速率明显下降
- [ ] sender-side 踢人:甲点踢人,乙端 60s 后转 failed,甲端 SourceDone / SourceFailed
- [ ] 全小文件 push:UI 不再卡"等待中",每文件独立显示
- [ ] 应用重启:transfers.json 加载,active → interrupted,UI 顶部横幅提示"续传"
- [ ] 应用重启后清除已完成/失败按钮:interrupted 也被清
- [ ] 关闭应用时有 active 任务:弹模态,取消关闭能拦下
- [ ] u64 job_id 边界:手动制造一个 > 2^53 的 job_id,restart 后操作不报 floating point 错
- [ ] 删除带 parts 的任务:confirm 提示"未完成分块",确认后磁盘目录一并删除
- [ ] 删除无 parts 的任务:直接删,无 confirm
- [ ] 限速菜单点击外部区域关闭
- [ ] sender-side 任务在连接断开 30s 后从表中消失
```

---

## 9. 迁移与兼容

### 9.1 数据格式迁移

- `data/transfers.json`:新增,首次启动后由 AppState 生成
- 旧用户首次升级:启动时找不到 transfers.json → 空表运行,无迁移逻辑
- `data/trust.json` / `data/identity/` 等其他文件:不动

### 9.2 协议兼容性

- 接收方 / 发送方都是同一应用同一版本,无需跨版本协议兼容
- `u64 → String` 序列化是**强制迁移**:老前端不知道新后端会发字符串,会被反序列化错。文档明确"升级必须前后端同步"

### 9.3 任务 ID 空间

- receiver: `next_job_id() → 0x0000_0000_0000_0001` 起
- sender:   `next_source_job_id() → 0x8000_0000_0000_0000` 起
- 高 bit 段错开,避免理论上的 id 冲突

### 9.4 行为变化(用户感知)

| 操作 | 老行为 | 新行为 |
|------|--------|--------|
| 全小文件 push | UI 卡"等待中",无 Done | 每文件独立显示进度 + Done |
| 应用重启后看 transfers 表 | 空 | 恢复历史任务(active→interrupted) |
| 关闭应用 | 直接关 | 有 active 时弹模态 |
| 服务方被拉 | 无可见信息 | 橙色"被取中"卡片 + 速率 + 健康 |
| 服务方要踢人 | 无 | 限速 / 踢人按钮 |
| 删除已完成任务 | 只能等自动清 | 手动按钮 + 单条删除 |

---

## 10. 风险与延期

### 10.1 已知风险

| 风险 | 影响 | 缓解 |
|------|------|------|
| SourceProgress 4Hz 事件量较大 | UI 渲染压力 | 与 receiver 进度合并走同一个 scheduleUpdate,内部按 job_id 去重 |
| Throttle 拒绝后接收方 60s 超时报错 | 体验差但功能正确 | 本轮接受,后续可发显式 TransferCtl 让接收方暂停/恢复 |
| transfers.json 频繁写盘 | SSD 写入放大 | 节流 1s,实测传输期间最多 1 次/秒 |
| 节流菜单 click-outside 指令 | 需新增 v-on-click-outside 指令 | 加一个轻量指令(几十行),避免引入第三方库 |
| 关闭拦截在 Windows 强制关闭时不生效 | taskkill 等绕过 | 兜底靠断点续传,任务可恢复 |

### 10.2 延期到下轮

- 限速可调滑块(本轮用 1/2/4/不限 预设)
- Tauri 原生确认模态(本轮用浏览器 confirm)
- 限流拒绝后显式 TransferCtl 让接收方暂停
- ETA 智能预测(根据历史速率自适应)
- sender_jobs map 加更多 sender 端统计(总流量、峰值速率)
- transfer 表的多端同步(中继功能)

### 10.3 文档与发版

- CHANGELOG 加 v0.2.0 条目
- README "传输"章节更新操作说明
- "故障排除"加 "transfers.json 损坏如何恢复"

---

## 11. 实施范围估算

| 层 | 文件 | 行数估算 |
|----|------|----------|
| Engine | `crates/localtrans-core/src/transfer/engine.rs` | +250 |
| Engine | `crates/localtrans-core/src/transfer/mod.rs` | +10 |
| Engine | `crates/localtrans-core/src/protocol.rs` | +20 |
| Engine | `crates/localtrans-core/src/session.rs` | +20 |
| Engine | `crates/localtrans-core/src/serde_compat.rs` (新) | +20 |
| Engine | `crates/localtrans-core/src/identity.rs` | +5 |
| Tauri | `src-tauri/src/main.rs` | +50 |
| Tauri | `src-tauri/src/commands.rs` | +200 |
| 前端 | `ui/src/types.ts` | +20 |
| 前端 | `ui/src/api.ts` | +30 |
| 前端 | `ui/src/stores/transfers.ts` | +40 |
| 前端 | `ui/src/components/TransferItem.vue` | +200 |
| 前端 | `ui/src/pages/Transfers.vue` | +30 |
| 前端 | `ui/src/composables/useCloseGuard.ts` (新) | +50 |
| 前端 | `ui/src/App.vue` | +30 |
| 测试 | 各 `tests/*.rs` 和 `*.test.ts` | +400 |

**总计:** ~1370 行(含测试)。代码本身 ~770 行。
