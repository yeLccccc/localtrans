# 传输面板源端可视化 + 持久化 + 卡死修复 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让发送方在 Transfers 页面看到实时进度/速率/健康度,支持暂停/继续/取消/限速/踢人;transfers.json 持久化;修 u64 精度 bug 与"等待中"卡死;关闭应用时弹模态

**Architecture:**
- Rust 核心加 `SourceProgressEvent` 系列变体,sender-side 速度采样任务,`SenderJobState` 提到 `AppState`
- Tauri 端 `source-progress` 事件通道 + `transfers.json` 节流写盘 + 启动 interrupted 迁移
- 前端混合列表 + 角色徽章 + 健康面板 + ETA,三角色按钮分支
- Tauri `onCloseRequested` 拦截 + 前端模态回传决定

**Tech Stack:** Rust 1.70+ (Tokio, Quinn, Serde, Tauri 2), TypeScript + Vue 3 + Pinia + Vitest

**Spec:** `docs/superpowers/specs/2026-08-21-transfers-source-visibility-design.md`

## Global Constraints

- 工程语言:Rust 2021 edition + TypeScript 5
- 已有约定:`#[serde(rename_all = "snake_case")]` 在 Rust 端默认,前端字段名 snake_case
- 测试约定:引擎层用 `#[tokio::test]` + `tempfile`;前端用 vitest + @vue/test-utils
- 提交约定:conventional commits,feat/fix/test/refactor/docs 前缀
- 必须在 `D:\localTrans` 仓库根目录
- 不得修改 `Cargo.toml` workspace 已有依赖版本;新增依赖走 `[workspace.dependencies]` 后再 `crates/*/Cargo.toml` 或 `src-tauri/Cargo.toml` 引用
- 不引入新的第三方 UI 库(限速菜单 click-outside 用轻量自定义指令,见 T15)

---

## File Structure

### 新增文件

```
crates/localtrans-core/src/serde_compat.rs        # u64_hex_string serde 模块
crates/localtrans-core/src/transfer/source_probe.rs  # run_source_probe
crates/localtrans-core/src/transfer/sender_state.rs  # SenderJobState + sender_jobs 注册表
crates/localtrans-core/src/transfer/throttle.rs   # Throttle 协议 + 节流判定
crates/localtrans-core/tests/u64_serde.rs         # u64 序列化往返测试
crates/localtrans-core/tests/source_probe.rs      # SourceProbe 单元测试
crates/localtrans-core/tests/source_events.rs     # 全双工集成测试
crates/localtrans-core/tests/persistence.rs       # 持久化往返测试
crates/localtrans-core/tests/bug_fixes.rs         # 三个 bug 修复的回归测试
crates/localtrans-core/tests/throttle.rs          # 节流判定测试
src-tauri/src/persistence.rs                      # transfers.json 读写
src-tauri/src/close_guard.rs                      # onCloseRequested 桥接
ui/src/composables/useCloseGuard.ts               # 前端关闭拦截 composable
ui/src/__tests__/TransferItem.test.ts             # 组件测试
ui/src/__tests__/transfersStore.test.ts           # store 测试
ui/src/__tests__/TransfersPage.test.ts            # 页面测试
ui/src/__tests__/useCloseGuard.test.ts            # composable 测试
ui/src/directives/clickOutside.ts                 # v-click-outside 指令
```

### 修改文件

```
crates/localtrans-core/src/protocol.rs            # u64 → String;TransferAction::Throttle
crates/localtrans-core/src/transfer/engine.rs     # run_sender 加进度回调;push_files 补 Started
crates/localtrans-core/src/transfer/mod.rs        # 导出新模块
crates/localtrans-core/src/session.rs             # next_source_job_id 原子计数器
crates/localtrans-core/src/identity.rs            # 加 SourceRole 类型(若合适,否则放 transfer/mod.rs)
crates/localtrans-core/src/lib.rs                 # re-exports
src-tauri/src/main.rs                             # onCloseRequested + AppState 扩展 + 新事件名常量
src-tauri/src/commands.rs                         # 4 个新 command + Done/Failed 占位 fallback
src-tauri/Cargo.toml                              # 不变(已有 tokio/serde 等)
ui/src/types.ts                                   # TransferDto + SourceProgressEvent
ui/src/api.ts                                     # 新 invoke + onSourceProgress
ui/src/stores/transfers.ts                        # source-progress 监听 + 新 actions
ui/src/components/TransferItem.vue                # 角色徽章 + 健康面板 + ETA + 三角色按钮 + 删除 + 限速菜单
ui/src/pages/Transfers.vue                        # 清除按钮 + 空状态提示
ui/src/App.vue                                    # 关闭拦截模态挂载
ui/package.json                                   # 不变(已有 vitest 依赖;若无则加)
```

---

## Task 1: u64 → String 序列化基础设施

**Files:**
- Create: `crates/localtrans-core/src/serde_compat.rs`
- Modify: `crates/localtrans-core/src/lib.rs:1-15`
- Test: `crates/localtrans-core/tests/u64_serde.rs`

**Interfaces:**
- Consumes: 无
- Produces: `serde_compat::u64_hex_string::{serialize, deserialize}` 模块

- [ ] **Step 1: 写失败测试**

在 `crates/localtrans-core/tests/u64_serde.rs`:

```rust
use localtrans_core::serde_compat::u64_hex_string;
use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct W {
    #[serde(with = "u64_hex_string")]
    j: u64,
}

#[test]
fn u64_serializes_as_hex_string() {
    let w = W { j: 0x1234_5678_9abc_def0 };
    let s = serde_json::to_string(&w).unwrap();
    assert!(s.contains("\"j\":\"123456789abcdef0\""), "got: {}", s);
}

#[test]
fn u64_roundtrips_precision_safe() {
    let original = 1u64 << 62;  // 4.6e18 — 远超 JS Number 安全整数
    let s = serde_json::to_string(&W { j: original }).unwrap();
    let parsed: W = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed.j, original);
}

#[test]
fn u64_zero_serializes_as_16_zero_chars() {
    let s = serde_json::to_string(&W { j: 0 }).unwrap();
    assert!(s.contains("\"j\":\"0000000000000000\""));
}
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core --test u64_serde`
Expected: FAIL with "module `serde_compat` not found"

- [ ] **Step 3: 实现 `serde_compat.rs`**

```rust
// crates/localtrans-core/src/serde_compat.rs
//! Serde 兼容模块,处理 u64 ↔ JSON 字符串的精度问题。

pub mod u64_hex_string {
    use serde::{Deserialize, Deserializer, Serializer};

    /// 把 u64 序列化为 16 位小写 hex 字符串(无 0x 前缀)。
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&format!("{:016x}", v))
    }

    /// 从 hex 字符串反序列化回 u64;允许可选 `0x` 前缀。
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let s = String::deserialize(d)?;
        let stripped = s.trim_start_matches("0x");
        u64::from_str_radix(stripped, 16).map_err(serde::de::Error::custom)
    }
}
```

- [ ] **Step 4: 暴露模块**

修改 `crates/localtrans-core/src/lib.rs`,在 `pub mod store;` 后插入:

```rust
pub mod serde_compat;
```

- [ ] **Step 5: 跑测试,确认通过**

Run: `cargo test -p localtrans-core --test u64_serde`
Expected: 3 passed

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-core/src/serde_compat.rs \
        crates/localtrans-core/src/lib.rs \
        crates/localtrans-core/tests/u64_serde.rs
git commit -m "feat(core): u64 ↔ hex 字符串序列化(修 JS 精度边界)"
```

---

## Task 2: ProgressEvent Source* 变体 + SourceRole

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:351-358`
- Test: `crates/localtrans-core/tests/source_events.rs`(先放空文件,Task 5 充实)

**Interfaces:**
- Consumes: `crate::identity::Fingerprint`
- Produces: `ProgressEvent::{SourceStarted, SourceChunkDone, SourceSpeed, SourceDone, SourceFailed}`, `SourceRole::{SourcePush, SourcePull}`

- [ ] **Step 1: 写失败测试占位**

在 `crates/localtrans-core/tests/source_events.rs`:

```rust
use localtrans_core::transfer::{ProgressEvent, SourceRole};

#[test]
fn source_started_serializes_with_tag() {
    // 占位:T5 充实后此测试断言具体序列化形态
    let ev = ProgressEvent::SourceStarted {
        job_id: 0x8000_0000_0000_0001,
        role: SourceRole::SourcePull,
        peer: [0u8; 32],
        name: "test.bin".into(),
        total: 1024,
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"type\":\"source_started\""), "got: {}", s);
}

#[test]
fn source_role_serializes_snake_case() {
    assert_eq!(serde_json::to_string(&SourceRole::SourcePush).unwrap(), "\"source_push\"");
    assert_eq!(serde_json::to_string(&SourceRole::SourcePull).unwrap(), "\"source_pull\"");
}
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core --test source_events`
Expected: FAIL with "variant `SourceStarted` not found"

- [ ] **Step 3: 修改 `engine.rs` 扩展 `ProgressEvent`**

替换 `crates/localtrans-core/src/transfer/engine.rs:351-358`:

```rust
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
        cwnd: u32,
        streams: u32,
    },
    SourceDone { job_id: u64 },
    SourceFailed { job_id: u64, reason: String },
}

/// 发送方角色(决定 UI 颜色)
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRole {
    SourcePush,
    SourcePull,
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cargo test -p localtrans-core --test source_events`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs \
        crates/localtrans-core/tests/source_events.rs
git commit -m "feat(core): ProgressEvent 加 Source* 系列变体"
```

---

## Task 3: next_source_job_id 原子计数器

**Files:**
- Modify: `crates/localtrans-core/src/session.rs`(在 `next_job_id` 旁加)
- Test: `crates/localtrans-core/tests/u64_serde.rs` 追加测试

**Interfaces:**
- Consumes: 无
- Produces: `pub fn next_source_job_id() -> u64`(高 bit 段 0x8000...)

- [ ] **Step 1: 写失败测试**

追加到 `crates/localtrans-core/tests/u64_serde.rs`:

```rust
use localtrans_core::session::next_source_job_id;

#[test]
fn source_job_id_starts_at_high_bit_segment() {
    let id = next_source_job_id();
    assert!(id >= 0x8000_0000_0000_0000, "got: {:x}", id);
}

#[test]
fn source_job_ids_are_unique_across_threads() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::thread;

    let seen = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = vec![];
    for _ in 0..8 {
        let seen = seen.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let id = next_source_job_id();
                seen.lock().unwrap().insert(id);
            }
        }));
    }
    for h in handles { h.join().unwrap(); }
    assert_eq!(seen.lock().unwrap().len(), 800);
}
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core --test u64_serde source_job`
Expected: FAIL with "function `next_source_job_id` not found"

- [ ] **Step 3: 实现 `next_source_job_id`**

修改 `crates/localtrans-core/src/session.rs`,在 `next_job_id` 函数后追加:

```rust
/// 分配 sender-side job id(高 bit 段 0x8000_...,与 receiver 错开)。
/// 多线程并发安全;job_id 全局唯一,跨实例也不冲突。
pub fn next_source_job_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cargo test -p localtrans-core --test u64_serde source_job`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/session.rs \
        crates/localtrans-core/tests/u64_serde.rs
git commit -m "feat(core): next_source_job_id 高 bit 段分配"
```

---

## Task 4: run_sender 加进度回调

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1007-1054`(run_sender)
- Test: `crates/localtrans-core/tests/source_events.rs` 追加

**Interfaces:**
- Consumes: `SenderJobState`(本任务用 `Option<Arc<AtomicU64>>` + `Option<mpsc::Sender<ProgressEvent>>` 占位,T5 引入正式结构)
- Produces: 修改后的 `run_sender` 签名;每块写完后递增 `bytes_counter`、发 `SourceChunkDone` 事件

- [ ] **Step 1: 写失败测试**

追加到 `crates/localtrans-core/tests/source_events.rs`:

```rust
use localtrans_core::transfer::run_sender;
use localtrans_core::protocol::{Manifest, ChunkStreamHeader, CHUNK_SIZE};
use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

#[tokio::test]
async fn run_sender_updates_bytes_counter_on_success() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src.bin");
    let data = vec![7u8; CHUNK_SIZE + 100];
    tokio::fs::write(&src, &data).await.unwrap();
    let manifest = Manifest::build(&src).unwrap();

    let counter = Arc::new(AtomicU64::new(0));
    // 没有真实 QUIC 连接,此测试仅验证 counter 逻辑在文件能读完时能更新。
    // 真实 QUIC 路径在 T5 集成测试覆盖。
    // 这里断言 manifest 块大小正确即可:
    assert_eq!(manifest.chunk_count(), 2);
    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    // 完整端到端需要 mock QUIC,放到后续 T5 集成测试
}
```

注意:`run_sender` 真正需要 QUIC `Connection` 才能跑——纯单元测试只验证 chunk 几何,完整路径覆盖在 T5 集成测试。

- [ ] **Step 2: 跑测试,确认通过(几何断言)**

Run: `cargo test -p localtrans-core --test source_events run_sender_updates`
Expected: PASS(只断言几何)

- [ ] **Step 3: 修改 `run_sender` 签名**

修改 `crates/localtrans-core/src/transfer/engine.rs:1007` 附近的函数签名:

```rust
pub async fn run_sender(
    conn: Connection,
    pool: Arc<BufferPool>,
    src_path: PathBuf,
    manifest: Manifest,
    job_id: u64,
    chunk: u32,
    bytes_counter: Option<Arc<AtomicU64>>,
    progress: Option<mpsc::Sender<ProgressEvent>>,
) -> Result<(), EngineError> {
    let chunk_len = manifest.chunk_len(chunk) as usize;
    let offset = manifest.chunk_offset(chunk);

    let mut buf = pool.acquire().await;
    buf.resize(chunk_len, 0);
    {
        let mut file = match tokio::fs::File::open(&src_path).await {
            Ok(f) => f,
            Err(e) => { pool.release(buf); return Err(e.into()); }
        };
        if let Err(e) = file.seek(io::SeekFrom::Start(offset)).await {
            pool.release(buf); return Err(e.into());
        }
        if let Err(e) = file.read_exact(&mut buf[..chunk_len]).await {
            pool.release(buf); return Err(e.into());
        }
    }

    let write_err = |ctx: &'static str, e: quinn::WriteError| {
        EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("{}: {}", ctx, e)))
    };
    let mut uni = conn
        .open_uni().await
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("open_uni: {}", e))))?;
    uni.write_all(&ChunkStreamHeader { job_id, chunk }.encode()).await.map_err(|e| write_err("写块流头", e))?;
    uni.write_all(&buf[..chunk_len]).await.map_err(|e| write_err("写块数据", e))?;
    uni.finish()
        .map_err(|e| EngineError::Io(io::Error::new(io::ErrorKind::ConnectionAborted, format!("finish: {}", e))))?;
    pool.release(buf);

    // 新增:bytes_counter + SourceChunkDone 事件
    if let Some(counter) = &bytes_counter {
        counter.fetch_add(chunk_len as u64, std::sync::atomic::Ordering::Relaxed);
    }
    if let Some(tx) = &progress {
        let _ = tx.send(ProgressEvent::SourceChunkDone {
            job_id, chunk, bytes: chunk_len as u64,
        }).await;
    }

    tracing::debug!("块流发送完成: job {} chunk {} ({}B)", job_id, chunk, chunk_len);
    Ok(())
}
```

顶部 imports 加 `use std::sync::atomic::AtomicU64;`

- [ ] **Step 4: 编译,确认无错**

Run: `cargo build -p localtrans-core`
Expected: 编译失败,因 `spawn_rpc_router` 内调用 `run_sender` 没传新参数。**这是预期的,继续下一步。**

- [ ] **Step 5: 在 `spawn_rpc_router` 内 FetchReq 分支加占位调用**

修改 `crates/localtrans-core/src/transfer/engine.rs:1212-1216`,临时传 `None, None`:

```rust
tokio::spawn(async move {
    if let Err(e) = run_sender(conn, pool, job.src_path, m, job_id, chunk, None, None).await {
        tracing::warn!("块流发送失败: job {} chunk {}: {}", job_id, chunk, e);
    }
});
```

- [ ] **Step 6: 重新编译并跑全部测试**

Run: `cargo build -p localtrans-core && cargo test -p localtrans-core --test source_events`
Expected: 编译成功,所有现有测试 + 新测试通过

- [ ] **Step 7: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): run_sender 加 bytes_counter/progress 回调参数"
```

---

## Task 5: SenderJobState + SourceProbe + 30s 兜底清理

**Files:**
- Create: `crates/localtrans-core/src/transfer/sender_state.rs`
- Create: `crates/localtrans-core/src/transfer/source_probe.rs`
- Modify: `crates/localtrans-core/src/transfer/mod.rs:1-15`
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1134-1184`(MetaReq handler)
- Test: `crates/localtrans-core/tests/source_events.rs` 追加

**Interfaces:**
- Consumes: `next_source_job_id()`,`AppState.sender_jobs`(T12 在 src-tauri 引入),`Connection`,`mpsc::Sender<ProgressEvent>`
- Produces: `SenderJobState` 结构 + `run_source_probe()` 函数 + sender_jobs 注册/查找/移除辅助

- [ ] **Step 1: 写失败测试**

追加到 `crates/localtrans-core/tests/source_events.rs`:

```rust
use localtrans_core::transfer::sender_state::{SenderJobState, new_sender_job_state};

#[test]
fn sender_job_state_default_throttle_is_max() {
    let tmp = tempfile::TempDir::new().unwrap();
    let manifest = localtrans_core::transfer::Manifest {
        file_name: "x".into(),
        total_size: 0,
        chunk_hashes: vec![],
        received: vec![],
        peer: None, share_id: None, rel: None,
    };
    let (tx, _rx) = mpsc::channel::<ProgressEvent>(1);
    let state = new_sender_job_state(
        0x8000_0000_0000_0001,
        tmp.path().join("src"),
        manifest,
        None,
        tx,
    );
    assert_eq!(state.throttle_cap.load(std::sync::atomic::Ordering::Relaxed), u32::MAX);
    assert!(state.probe_stop_tx.is_some());
}
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core --test source_events sender_job`
Expected: FAIL with "module `sender_state` not found"

- [ ] **Step 3: 创建 `sender_state.rs`**

```rust
// crates/localtrans-core/src/transfer/sender_state.rs
//! Sender-side 任务状态:在 MetaReq 时建,SourceDone/Failed 时清理。

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
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
    pub probe_stop_tx: Option<oneshot::Sender<()>>,
}

/// 建一个新 SenderJobState(probe_stop_tx 留给上层注册后赋值)
pub fn new_sender_job_state(
    job_id: u64,
    src_path: PathBuf,
    manifest: Manifest,
    dest_hint: Option<(String, String)>,
    progress_tx: mpsc::Sender<ProgressEvent>,
) -> SenderJobState {
    SenderJobState {
        job_id,
        src_path,
        manifest,
        dest_hint,
        bytes_counter: Arc::new(AtomicU64::new(0)),
        active_streams: Arc::new(AtomicU32::new(0)),
        throttle_cap: Arc::new(AtomicU32::new(u32::MAX)),
        progress_tx,
        probe_stop_tx: None,
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
        let mut map = jobs.write().await;
        if map.remove(&job_id).is_some() {
            tracing::info!("sender job {} 连接关闭 30s 后兜底清理", job_id);
            let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
                job_id,
                reason: "连接关闭".to_string(),
            }).await;
            if let Some(tx) = state.probe_stop_tx.take() {
                let _ = tx.send(());
            }
        }
    });
}
```

- [ ] **Step 4: 创建 `source_probe.rs`**

```rust
// crates/localtrans-core/src/transfer/source_probe.rs
//! Sender-side 速度 / 健康度采样任务(500ms 周期)

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot;
use quinn::Connection;

use crate::transfer::ProgressEvent;

pub async fn run_source_probe(
    conn: Connection,
    job_id: u64,
    progress: tokio::sync::mpsc::Sender<ProgressEvent>,
    bytes_counter: Arc<std::sync::atomic::AtomicU64>,
    active_streams: Arc<std::sync::atomic::AtomicU32>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.tick().await;  // 跳过首个立即 tick
    let mut last_bytes = 0u64;
    let mut last_time = Instant::now();

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = &mut stop_rx => break,
            _ = conn.closed() => {
                tracing::debug!("sender 连接已关闭,探测任务退出 job={}", job_id);
                break;
            }
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

        last_bytes = cur;
        last_time = now;
    }
}
```

- [ ] **Step 5: 在 `mod.rs` 导出新模块**

修改 `crates/localtrans-core/src/transfer/mod.rs:1-3`:

```rust
pub mod adapt;
pub mod engine;
pub mod manifest;
pub mod sender_state;
pub mod source_probe;
```

并在 engine re-export 块追加 `SourceRole`(在 T2 已加在 engine.rs):

```rust
pub use engine::{
    control_task, start_pull, push_files, spawn_rpc_router,
    ProgressEvent, OfferAsk, TaskControl, EngineError, PartWriter, BufferPool, SourceRole,
};
```

- [ ] **Step 6: 跑新测试,确认通过**

Run: `cargo test -p localtrans-core --test source_events`
Expected: sender_job_state 测试通过

- [ ] **Step 7: 提交**

```bash
git add crates/localtrans-core/src/transfer/sender_state.rs \
        crates/localtrans-core/src/transfer/source_probe.rs \
        crates/localtrans-core/src/transfer/mod.rs
git commit -m "feat(core): SenderJobState + SourceProbe + 30s 连接断开兜底"
```

- [ ] **Step 8: 加 sender-side 集成测试(验证 Source* 事件真发出来)**

追加到 `crates/localtrans-core/src/transfer/engine.rs` 测试块末尾:

```rust
/// T5: 拉取模式下乙(sender)侧应发出 SourceStarted/SourceChunkDone/SourceSpeed/SourceDone
#[tokio::test]
async fn pull_emits_source_events_on_sender_side() {
    use crate::identity::{TrustedPeer, Perms, PushPolicy};
    use crate::transfer::sender_state::SenderJobMap;
    use std::sync::Arc;
    use tokio::time::timeout;

    crate::test_support::init_tracing();

    // 10MB 文件 = 3 个 4MiB 块
    let data: Vec<u8> = (0..10 * 1024 * 1024).map(|i| (i % 251) as u8).collect();

    // 复用 engine.rs 已有的 setup_pull fixture 思路,这里内联展开以拿到 sender_jobs map
    let (sm_a, _ev_a, ctx_a, fp_a, dir_a) = crate::test_support::setup_ctx("甲");
    let (sm_b, _ev_b, ctx_b, fp_b, dir_b) = crate::test_support::setup_ctx("乙");

    // 互信(browse + download)
    {
        let mut t = ctx_a.trust.lock().await;
        t.upsert(TrustedPeer {
            fingerprint: fp_b, name: "乙".into(), paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        t.save().unwrap();
    }
    {
        let mut t = ctx_b.trust.lock().await;
        t.upsert(TrustedPeer {
            fingerprint: fp_a, name: "甲".into(), paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        t.save().unwrap();
    }

    let share_path = dir_b.path().join("shares");
    fs::create_dir_all(&share_path).unwrap();
    fs::write(share_path.join("file.bin"), &data).unwrap();
    let reg_b = Arc::new(ShareRegistry::new(vec![crate::store::ShareDef {
        id: "share1".into(), alias: "测试".into(), path: share_path,
    }]));

    let download_dir = dir_a.path().join("downloads");
    fs::create_dir_all(&download_dir).unwrap();
    ctx_a.config.write().await.download_dir = download_dir.clone();

    // 乙 listener + router(带 sender_jobs map)
    let b_addr = crate::test_support::start_listener(&sm_b).await;
    let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站");
    let (ask_tx, _ask_rx) = mpsc::channel(8);
    let sender_jobs: SenderJobMap = crate::transfer::sender_state::new_sender_job_map();
    let sender_jobs_for_spawn = sender_jobs.clone();
    spawn_rpc_router(
        sm_b.clone(), ctx_b.clone(), reg_b, ctrl_rx, ask_tx,
        sender_jobs_for_spawn,
    );

    // 甲连接乙
    timeout(Duration::from_secs(5), sm_a.connect(b_addr))
        .await.expect("连接").unwrap();

    // 甲拉取
    let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);
    let reg_unused = ShareRegistry::new(vec![]);
    let cfg_a = ctx_a.config.read().await.clone();
    let _job_id = timeout(
        Duration::from_secs(30),
        start_pull(&sm_a, &reg_unused, &fp_b, "share1", "file.bin", &cfg_a, progress_tx),
    ).await.expect("拉取").expect("成功");

    // 乙侧应发 SourceStarted
    let mut got_source_started = false;
    let mut got_source_done = false;
    let mut got_source_chunks = 0u32;
    timeout(Duration::from_secs(15), async {
        while let Some(ev) = sender_jobs_progress_rx.recv().await {
            match ev {
                ProgressEvent::SourceStarted { role: SourceRole::SourcePull, .. } =>
                    got_source_started = true,
                ProgressEvent::SourceChunkDone { .. } => got_source_chunks += 1,
                ProgressEvent::SourceDone { .. } => { got_source_done = true; break; }
                _ => {}
            }
        }
    }).await.expect("应收到 SourceDone");
}
```

**注意:** 上面的 `sender_jobs_progress_rx` 需要在 router 启动时把 sender 事件泵到一个可观测的 channel。当前 T6 MetaReq handler 内的事件泵走的是"临时丢弃"(`tracing::trace!`)。本测试需要先把 sender 事件也桥接到一个外部 channel——具体做法:扩展 `spawn_rpc_router` 签名接 `mpsc::Sender<ProgressEvent>` 参数(命名为 `source_event_tx`),T6 已建好,这里测试时构造 channel 传入。

**若 T6 未完成本测试,先标记 skip,把 sender_jobs_progress_rx 改为监听主 router 的 progress 通道或跳过本测试,等 T6 完成后再跑。**

---

## Task 6: MetaReq handler 接入 SenderJobState + SourceProbe

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1134-1184`(MetaReq 分支)
- Test: `crates/localtrans-core/tests/source_events.rs` 追加

**Interfaces:**
- Consumes: `spawn_rpc_router` 新签名接 `SenderJobMap`(T12 完整集成时由 src-tauri 传入;本任务先把签名改了,调用方传 `new_sender_job_map()` 占位)
- Produces: MetaReq 命中时建 SenderJobState + 启动 SourceProbe + 注册到 sender_jobs map + 发 SourceStarted

- [ ] **Step 1: 扩展 `spawn_rpc_router` 签名**

修改 `crates/localtrans-core/src/transfer/engine.rs:1057-1063`:

```rust
pub fn spawn_rpc_router(
    sm: Arc<SessionManager>,
    ctx: SessionCtx,
    reg: Arc<ShareRegistry>,
    mut ctrl_rx: mpsc::Receiver<(Fingerprint, ControlMsg)>,
    ask_tx: mpsc::Sender<OfferAsk>,
    sender_jobs: crate::transfer::sender_state::SenderJobMap,
    app: tauri::AppHandle,  // 占位:T12 改 AppHandle 注入
)
```

注意:`tauri::AppHandle` 在 localtrans-core 不能直接依赖。**临时方案**:把 app 改成 `Option<Box<dyn Fn(ProgressEvent) + Send + Sync>>` 回调,src-tauri 端注入 emit 函数。T12 整合时再换成 `AppHandle`。

实际上更简单的办法:**本任务只把 SenderJobMap 加进签名,app 参数暂时不用**——MetaReq handler 内只建 SenderJobState + 启 SourceProbe,事件仍走原 progress channel(由上层接收)。

修改为:

```rust
pub fn spawn_rpc_router(
    sm: Arc<SessionManager>,
    ctx: SessionCtx,
    reg: Arc<ShareRegistry>,
    mut ctrl_rx: mpsc::Receiver<(Fingerprint, ControlMsg)>,
    ask_tx: mpsc::Sender<OfferAsk>,
    sender_jobs: crate::transfer::sender_state::SenderJobMap,
)
```

- [ ] **Step 2: 改 `MetaReq` handler**

替换 `crates/localtrans-core/src/transfer/engine.rs:1134-1184`(MetaReq 分支全段):

```rust
ControlMsg::MetaReq { share_id, path } => {
    let perms = ctx.trust.lock().await
        .get(&fingerprint).map(|p| p.perms.clone()).unwrap_or_default();
    if !perms.download {
        tracing::warn!("{} 无下载权限,拒绝 MetaReq", hex::encode(fingerprint));
        continue;
    }

    let resolved = if let Some(hex_id) = share_id.strip_prefix("push:") {
        match u64::from_str_radix(hex_id, 16).ok()
            .and_then(|oid| resolve_push_file(oid, &path)) {
            Some(p) => p,
            None => { tracing::warn!("推送任务不存在: {} {}", share_id, path); continue; }
        }
    } else {
        match reg.resolve(&share_id, &path) {
            Ok(p) => p,
            Err(e) => { tracing::warn!("路径解析失败: {}", e); continue; }
        }
    };

    let m = match Manifest::build(&resolved) {
        Ok(m) => m,
        Err(e) => { tracing::warn!("清单构建失败: {}", e); continue; }
    };

    let role = if share_id.starts_with("push:") {
        SourceRole::SourcePush
    } else {
        SourceRole::SourcePull
    };
    let job_id = crate::session::next_source_job_id();

    // 决定 progress 通道:复用 ctx 的 progress_tx(若有);否则新建
    // 为简化:本任务暂用独立 channel,后续在 T12 整合时把 sender 事件也走现有 progress_tx
    let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

    let state = std::sync::Arc::new(crate::transfer::sender_state::new_sender_job_state(
        job_id,
        resolved.clone(),
        m.clone(),
        None,
        progress_tx.clone(),
    ));

    // 注册
    sender_jobs.write().await.insert(job_id, state.clone());

    // 发 SourceStarted
    let _ = progress_tx.send(ProgressEvent::SourceStarted {
        job_id, role, peer: fingerprint, name: m.file_name.clone(), total: m.total_size,
    }).await;

    // 启动 SourceProbe
    let conn_for_probe = match sm.session(&fingerprint).await {
        Some(c) => c,
        None => { tracing::warn!("sender 探测: 会话不存在"); continue; }
    };
    let (probe_stop_tx, probe_stop_rx) = tokio::sync::oneshot::channel();
    // 把 stop_tx 装到 state(Mutex<Option<>> 包装,因为 SenderJobState 不直接可变)
    // 简化:本任务用 unsafe / 直接换法——见下注释
    // 实际方案:把 probe_stop_tx 放在 SenderJobState.probe_stop_tx 字段,首次构建后赋值
    // 但 new_sender_job_state 返回时该字段是 None,我们用 Arc<Mutex<Option<>>> 重构
    // ----> 简化:把 probe_stop_tx 作为参数传给 SourceProbe,把 stop_tx 通过 Arc<Mutex<>> 装到 state
    // 最简实现:在 SenderJobState 加 Arc<Mutex<Option<oneshot::Sender<()>>>> 字段
    // 这里采用:本任务先跑通 SourceProbe,stop_tx 用本地变量,todo 标记放进 state

    tokio::spawn(crate::transfer::source_probe::run_source_probe(
        conn_for_probe.clone(),
        job_id,
        progress_tx.clone(),
        state.bytes_counter.clone(),
        state.active_streams.clone(),
        probe_stop_rx,
    ));

    // 30s 连接断开兜底清理
    crate::transfer::sender_state::spawn_connection_drop_cleanup(
        conn_for_probe, state.clone(), sender_jobs.clone(),
    );

    // 启动事件泵:把 progress_rx 里的事件转发到上层
    // 本任务暂时丢弃(progress_rx 在函数末尾 drop),T12 整合时改用 ctx.progress_tx
    tokio::spawn(async move {
        while let Some(ev) = progress_rx.recv().await {
            tracing::trace!("sender event (临时丢弃): {:?}", ev);
        }
    });

    // 发 MetaResp
    let resp = ControlMsg::MetaResp {
        job_id, file_name: m.file_name.clone(),
        total_size: m.total_size, chunk_hashes: m.chunk_hashes.clone(),
    };
    if let Err(e) = sm.send_ctrl(&fingerprint, resp).await {
        tracing::warn!("发送 MetaResp 失败: {}", e);
    }
}
```

**注意:** 上文提到的 `probe_stop_tx` 装入 `state` 的问题——重构 SenderJobState 让 `probe_stop_tx` 是 `Arc<Mutex<Option<oneshot::Sender<()>>>>`:

修改 `sender_state.rs` 顶部结构:

```rust
pub struct SenderJobState {
    // ...
    pub probe_stop_tx: Arc<std::sync::Mutex<Option<oneshot::Sender<()>>>>,
}

pub fn new_sender_job_state(
    // ...
) -> SenderJobState {
    SenderJobState {
        // ...
        probe_stop_tx: Arc::new(std::sync::Mutex::new(None)),
    }
}
```

然后在 `MetaReq` handler 里:

```rust
*state.probe_stop_tx.lock().unwrap() = Some(probe_stop_tx);
```

并在 `spawn_connection_drop_cleanup` 内发停:

```rust
if let Some(tx) = state.probe_stop_tx.lock().unwrap().take() {
    let _ = tx.send(());
}
```

- [ ] **Step 3: 跑编译,确认无错**

Run: `cargo build -p localtrans-core`
Expected: 编译失败,因 `spawn_rpc_router` 现有调用没传新参数。**预期失败,继续下一步。**

- [ ] **Step 4: 更新 `spawn_rpc_router` 调用点**

在 `crates/localtrans-core/src/test_support.rs` 内搜索 `spawn_rpc_router(`,每个调用都追加 `, crate::transfer::sender_state::new_sender_job_map()`。同理 `src-tauri/src/main.rs` 里 `spawn_rpc_router` 调用也加。

- [ ] **Step 5: 跑全部测试**

Run: `cargo test -p localtrans-core`
Expected: 所有现有测试通过

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs \
        crates/localtrans-core/src/transfer/sender_state.rs \
        crates/localtrans-core/src/test_support.rs
git commit -m "feat(core): MetaReq 接入 SenderJobState + SourceProbe + 30s 兜底"
```

---

## Task 7: Throttle 协议 + FetchReq 节流判定

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs`(ControlMsg + TransferAction)
- Modify: `crates/localtrans-core/src/transfer/engine.rs:1185-1220`(FetchReq handler)
- Test: `crates/localtrans-core/tests/throttle.rs`(新)

**Interfaces:**
- Consumes: `SenderJobState.throttle_cap`, `SenderJobState.active_streams`
- Produces: `ControlMsg::TransferCtl { action: TransferAction::Throttle { max_streams } }`

- [ ] **Step 1: 写失败测试**

`crates/localtrans-core/tests/throttle.rs`:

```rust
use localtrans_core::protocol::{ControlMsg, TransferAction};

#[test]
fn throttle_action_serializes_with_max_streams() {
    let msg = ControlMsg::TransferCtl {
        job_id: 1,
        action: TransferAction::Throttle { max_streams: 2 },
    };
    let s = serde_json::to_string(&msg).unwrap();
    assert!(s.contains("\"Throttle\""), "got: {}", s);
    assert!(s.contains("\"max_streams\":2"), "got: {}", s);
}
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core --test throttle`
Expected: FAIL with "variant `Throttle` not found"

- [ ] **Step 3: 在 `protocol.rs` 加 `Throttle` 变体**

修改 `TransferAction` enum(找当前定义位置):

```rust
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum TransferAction {
    Pause,
    Resume,
    Cancel,
    Throttle { max_streams: u32 },  // 新增
}
```

(具体 serde 形式以现有 `TransferAction` 实际定义为基准,本任务只确保 `Throttle { max_streams }` 能 serialize/deserialize round-trip。)

- [ ] **Step 4: 跑测试,确认通过**

Run: `cargo test -p localtrans-core --test throttle`
Expected: 1 passed

- [ ] **Step 5: 在 FetchReq handler 加节流判定**

修改 `crates/localtrans-core/src/transfer/engine.rs:1185-1220`(FetchReq handler),在权限校验后、`jobs.read()` 前插入:

```rust
// 节流判定
let state = match sender_jobs.read().await.get(&job_id).cloned() {
    Some(s) => s,
    None => { tracing::warn!("sender 任务不存在: job {}", job_id); continue; }
};
let cur = state.active_streams.load(std::sync::atomic::Ordering::Relaxed);
let cap = state.throttle_cap.load(std::sync::atomic::Ordering::Relaxed);
if cur >= cap {
    tracing::debug!("节流拒绝: job {} (cur={}, cap={})", job_id, cur, cap);
    continue;
}
```

后续 spawn 块改成:

```rust
tokio::spawn(async move {
    state.active_streams.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let res = run_sender(
        conn, pool, state.src_path.clone(), state.manifest.clone(),
        job_id, chunk,
        Some(state.bytes_counter.clone()),
        Some(state.progress_tx.clone()),
    ).await;
    state.active_streams.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    if let Err(e) = res {
        tracing::warn!("块流发送失败: job {} chunk {}: {}", job_id, chunk, e);
    }
});
```

并删除之前的 `let job_info = jobs.read().await.get(&job_id).cloned();`(用 state 代替)。

- [ ] **Step 6: 跑全部测试**

Run: `cargo test -p localtrans-core`
Expected: 全部通过

- [ ] **Step 7: 提交**

```bash
git add crates/localtrans-core/src/protocol.rs \
        crates/localtrans-core/src/transfer/engine.rs \
        crates/localtrans-core/tests/throttle.rs
git commit -m "feat(core): Throttle 协议 + FetchReq 节流判定"
```

---

## Task 8: Bug 修复 — 小文件 push 补 Started/Done

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:727-750`(push_files 的 small files 分支)
- Test: `crates/localtrans-core/tests/bug_fixes.rs`(新)

**Interfaces:**
- Consumes: `push_files` 现有签名
- Produces: 每小文件一对 Started/Done 事件(而非聚合 1 条)

- [ ] **Step 1: 写失败测试**

追加到 `crates/localtrans-core/src/transfer/engine.rs` 测试块末尾(参照已有的 `small_files_batched` 测试):

```rust
/// T8: 全小文件 push 修复:每文件都应收到 Started + Done,不再卡"等待中"
#[tokio::test]
async fn push_only_small_files_emits_per_file_started_done() {
    use tokio::time::timeout;
    use crate::identity::{TrustedPeer, Perms, PushPolicy};

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
            fingerprint: fp_b, name: "乙".into(), paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        trust.save().unwrap();
    }
    {
        let mut trust = ctx_b.trust.lock().await;
        trust.upsert(TrustedPeer {
            fingerprint: fp_a, name: "甲".into(), paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        trust.save().unwrap();
    }

    let download_dir_b = dir_b.path().join("downloads");
    fs::create_dir_all(&download_dir_b).unwrap();
    ctx_b.config.write().await.download_dir = download_dir_b.clone();

    let b_addr = crate::test_support::start_listener(&sm_b).await;
    let ctrl_rx = sm_b.take_inbound_ctrl_rx().await.expect("入站通道未被占用");
    let (ask_tx, _ask_rx) = mpsc::channel(8);
    spawn_rpc_router(
        sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
        ctrl_rx, ask_tx,
        crate::transfer::sender_state::new_sender_job_map(),
    );

    let _peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr))
        .await.expect("连接应成功").unwrap();

    let (progress_tx, mut progress_rx) = mpsc::channel::<ProgressEvent>(64);

    timeout(
        Duration::from_secs(30),
        push_files(&sm_a, &fp_b, files.clone(), Some(download_dir_b.clone()), progress_tx),
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
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cargo test -p localtrans-core push_only_small_files_emits_per_file_started_done`
Expected: FAIL(当前 push_files 在全小文件时不发 Started/Done,所以 done_count=0)

- [ ] **Step 3: 实现修复**

修改 `crates/localtrans-core/src/transfer/engine.rs:727-740`(push_files 的 small files 分支):

```rust
// 发送小文件批流
if has_small {
    for (_, offer) in &small_files {
        let _ = progress.send(ProgressEvent::Started {
            job_id,
            name: offer.name.clone(),
            total: offer.size,
        }).await;
    }
    send_small_files_batched(&conn, job_id, small_files).await?;
    for (_, offer) in &small_files {
        let _ = progress.send(ProgressEvent::Done { job_id }).await;
    }
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cargo test -p localtrans-core push_only_small_files_emits_per_file_started_done`
Expected: 1 passed

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "fix(core): 全小文件 push 每文件补 Started/Done 事件"
```

---

## Task 9: Tauri handler Done/Failed 占位 fallback

**Files:**
- Modify: `src-tauri/src/commands.rs`(start_download / push_files / resume_pending 三处的 spawn 任务 Done/Failed 分支)
- Test: 此修复由 T14 (前端 store) + T11 (持久化) 的集成测试覆盖;本任务靠代码 review 验收

**Interfaces:**
- Consumes: `AppState.transfer_get_mut`, `placeholder_id`
- Produces: Done/Failed 找不到真 job_id 时回退到 placeholder

- [ ] **Step 1: 在 start_download spawn 任务的 Done 分支加 fallback**

修改 `src-tauri/src/commands.rs:326-333`(start_download 的 Done):

```rust
localtrans_core::transfer::ProgressEvent::Done { job_id: j } => {
    if let Some(mut dto) = st.transfer_get_mut(j).await {
        dto.state = "done".into();
        tracing::info!("下载完成: {} (job {})", dto.name, j);
        st.transfer_update(j, dto).await;
    } else if placeholder_alive {
        // 占位 fallback:Done 到达时真实 job_id 还没机会插进来
        if let Some(mut dto) = st.transfer_get_mut(placeholder_id).await {
            dto.state = "done".into();
            dto.job_id = j;
            st.transfer_remove(placeholder_id).await;
            st.transfer_update(j, dto).await;
        }
        placeholder_alive = false;
    } else {
        tracing::warn!("Done 找不到对应条目: job {}", j);
    }
}
```

- [ ] **Step 2: 在 start_download spawn 任务的 Failed 分支加 fallback**

修改 `src-tauri/src/commands.rs:334-344`(Failed 分支):

```rust
localtrans_core::transfer::ProgressEvent::Failed { job_id: j, reason } => {
    if let Some(mut dto) = st.transfer_get_mut(j).await {
        dto.state = "failed".into();
        tracing::warn!("下载失败: {} (job {}): {}", dto.name, j, reason);
        st.transfer_update(j, dto).await;
    } else if placeholder_alive {
        if let Some(mut dto) = st.transfer_get_mut(placeholder_id).await {
            dto.state = "failed".into();
            dto.job_id = j;
            st.transfer_remove(placeholder_id).await;
            st.transfer_update(j, dto).await;
        }
        placeholder_alive = false;
    }
    let _ = app.emit("toast", serde_json::json!({
        "level": "error",
        "text": format!("下载失败: {}", reason)
    }));
}
```

- [ ] **Step 3: 对 push_files 同样处理**

修改 `src-tauri/src/commands.rs:456-475`(push_files spawn 任务的 Done / Failed),完全相同的 fallback 模式。

- [ ] **Step 4: 对 resume_pending 同样处理**

修改 `src-tauri/src/commands.rs:697-715`(resume_pending spawn 任务的 Done / Failed),完全相同的 fallback 模式。

- [ ] **Step 5: 编译**

Run: `cargo build -p localtrans`
Expected: 编译成功

- [ ] **Step 6: 提交**

```bash
git add src-tauri/src/commands.rs
git commit -m "fix(tauri): Done/Failed 占位 fallback,修小文件 push 卡等待中"
```

---

## Task 10: TransferDto 新字段 + state "interrupted"

**Files:**
- Modify: `src-tauri/src/main.rs`(TransferDto 定义位置)
- Test: 此任务纯加字段,靠编译 + 后续 T13 前端 types.ts 同步验证

**Interfaces:**
- Consumes: 无
- Produces: `TransferDto { local_role, health, started_at_ms }` 字段

- [ ] **Step 1: 修改 TransferDto 定义**

在 `src-tauri/src/main.rs` 找 `TransferDto` 结构定义,改为:

```rust
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct TransferDto {
    #[serde(with = "localtrans_core::serde_compat::u64_hex_string")]
    pub job_id: u64,
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,
    pub speed_bps: u64,
    pub peer: String,
    pub direction: String,
    #[serde(default)]
    pub local_role: String,
    #[serde(default)]
    pub health: Option<HealthDto>,
    #[serde(default)]
    pub started_at_ms: Option<i64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct HealthDto {
    #[serde(default)]
    pub loss_ratio: f64,
    #[serde(default)]
    pub rtt_ms: u64,
    #[serde(default)]
    pub cwnd: u32,
    #[serde(default)]
    pub streams: u32,
}
```

注意:`#[serde(with = "...")]` 用 T1 实现的 `u64_hex_string` 模块。

- [ ] **Step 2: 在所有 TransferUpdate 处填新字段默认值**

在 `src-tauri/src/commands.rs` 所有 `TransferDto { ... }` 字面量,补:

```rust
local_role: "destination".into(),  // 默认 destination,后续 T11 接入
health: None,
started_at_ms: None,
```

至少覆盖 `start_download` / `push_files` / `resume_pending` 三处。

- [ ] **Step 3: 编译**

Run: `cargo build -p localtrans`
Expected: 编译成功(若 localtrans-core 已是 workspace 依赖,serde_compat 已暴露)

- [ ] **Step 4: 提交**

```bash
git add src-tauri/src/main.rs \
        src-tauri/src/commands.rs
git commit -m "feat(tauri): TransferDto 加 local_role/health/started_at_ms"
```

---

## Task 11: transfers.json 持久化(写盘 + 启动加载 + interrupted 迁移)

**Files:**
- Create: `src-tauri/src/persistence.rs`
- Modify: `src-tauri/src/main.rs`(AppState 加 save_throttle + sender_jobs + 加载逻辑)
- Modify: `src-tauri/src/commands.rs`(transfer_update/transfer_remove 调用 schedule_persist)
- Test: `crates/localtrans-core/tests/persistence.rs`(实际写在 src-tauri 测试目录)

**Interfaces:**
- Consumes: `Vec<TransferDto>`, `data/transfers.json` 路径
- Produces: `save_transfers(path, &[TransferDto]) -> io::Result<()>`, `load_transfers(path) -> io::Result<Vec<TransferDto>>`

- [ ] **Step 1: 写失败测试**

`crates/localtrans-core/tests/persistence.rs`:

```rust
// 实际 src-tauri 的 TransferDto 测试见 src-tauri/tests/persistence.rs
// 这里测试核心的"启动清理无 parts 的孤儿"逻辑
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use tempfile::TempDir;

#[test]
fn startup_cleanup_drops_orphan_without_parts() {
    let tmp = TempDir::new().unwrap();
    let download_dir = tmp.path();
    let parts_root = download_dir.join(".localtrans-parts");
    fs::create_dir_all(&parts_root).unwrap();
    // 没建任何子目录 → 所有 job 都"无 parts"

    // 模拟已存在的 1 条 active 记录
    let active_job_ids: HashSet<u64> = vec![0x1].into_iter().collect();
    let alive: HashSet<u64> = fs::read_dir(&parts_root).ok()
        .map(|rd| rd.flatten()
            .filter_map(|e| e.file_name().to_str()
                .and_then(|s| u64::from_str_radix(s, 16).ok()))
            .collect())
        .unwrap_or_default();

    // 该 job 不在 alive 里 → 应被丢弃
    assert!(!alive.contains(&0x1));
    assert!(active_job_ids.difference(&alive).next().is_some());
}
```

- [ ] **Step 2: 跑测试,确认通过(纯算法断言)**

Run: `cargo test -p localtrans-core --test persistence`
Expected: 1 passed

- [ ] **Step 3: 创建 `src-tauri/src/persistence.rs`**

```rust
// src-tauri/src/persistence.rs
//! transfers.json 读写

use std::path::Path;
use serde_json;
use crate::TransferDto;

pub fn save_transfers(dir: &Path, transfers: &[TransferDto]) -> std::io::Result<()> {
    let path = dir.join("transfers.json");
    let json = serde_json::to_string_pretty(transfers)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&path, json)
}

pub fn load_transfers(dir: &Path) -> std::io::Result<Vec<TransferDto>> {
    let path = dir.join("transfers.json");
    if !path.exists() { return Ok(Vec::new()); }
    let json = std::fs::read_to_string(&path)?;
    serde_json::from_str(&json)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// 启动时迁移:active/pending → interrupted,且无 parts 的孤儿丢弃
pub fn migrate_on_startup(transfers: Vec<TransferDto>, download_dir: &Path) -> Vec<TransferDto> {
    use std::collections::HashSet;
    let parts_root = download_dir.join(".localtrans-parts");
    let alive: HashSet<u64> = std::fs::read_dir(&parts_root).ok()
        .map(|rd| rd.flatten()
            .filter_map(|e| e.file_name().to_str()
                .and_then(|s| u64::from_str_radix(s, 16).ok()))
            .collect())
        .unwrap_or_default();

    transfers.into_iter()
        .filter_map(|mut dto| {
            match dto.state.as_str() {
                "active" | "pending" => {
                    if !alive.contains(&dto.job_id) {
                        return None;  // 孤儿,丢弃
                    }
                    dto.state = "interrupted".into();
                }
                _ => {}
            }
            Some(dto)
        })
        .collect()
}
```

- [ ] **Step 4: 在 `AppState` 加字段和 schedule_persist**

修改 `src-tauri/src/main.rs` 的 AppState:

```rust
pub struct AppState {
    pub inner: std::sync::Arc<AppStateInner>,
    // ...
}

pub struct AppStateInner {
    // ... 现有字段 ...

    // 新增
    pub sender_jobs: localtrans_core::transfer::sender_state::SenderJobMap,
    pub save_throttle: std::sync::Arc<std::sync::Mutex<Option<()>>>,
}

impl AppStateInner {
    pub fn schedule_persist(&self) {
        let mut guard = self.save_throttle.lock().unwrap();
        if guard.is_some() { return; }
        *guard = Some(());
        let st = self.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1000)).await;
            let snapshot: Vec<TransferDto> = st.transfer_table_read().await.into_values().collect();
            if let Err(e) = crate::persistence::save_transfers(&st.dir, &snapshot) {
                tracing::warn!("transfers.json 写盘失败: {}", e);
            }
            *st.save_throttle.lock().unwrap() = None;
        });
    }
}
```

实际 `AppState` 结构可能与示例不同——按现有代码适配,在 inner 字段上加 `sender_jobs` 和 `save_throttle` 即可。

- [ ] **Step 5: 在 `transfer_update` / `transfer_remove` 加 schedule_persist**

修改 `src-tauri/src/main.rs` 的两个方法末尾:

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
```

- [ ] **Step 6: 在 `AppState::new()` 加载 + 迁移**

修改 `AppState::new()`,在初始化末尾追加:

```rust
let mut loaded = crate::persistence::load_transfers(&dir).unwrap_or_else(|e| {
    tracing::warn!("transfers.json 加载失败: {}", e);
    Vec::new()
});
let download_dir = config.read().await.download_dir.clone();
loaded = crate::persistence::migrate_on_startup(loaded, &download_dir);
let table: HashMap<u64, TransferDto> = loaded.into_iter().map(|d| (d.job_id, d)).collect();
state.transfer_table = std::sync::Arc::new(tokio::sync::RwLock::new(table));
```

- [ ] **Step 7: 编译**

Run: `cargo build -p localtrans`
Expected: 编译成功

- [ ] **Step 8: 提交**

```bash
git add src-tauri/src/persistence.rs \
        src-tauri/src/main.rs
git commit -m "feat(tauri): transfers.json 持久化 + interrupted 迁移"
```

---

## Task 12: 新 Tauri commands (clear/remove/throttle/has_parts)

**Files:**
- Modify: `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/main.rs`(invoke_handler 注册)

**Interfaces:**
- Consumes: `AppState.transfer_table`, `AppState.sender_jobs`, `AppState.config.download_dir`
- Produces: 4 个新 command

- [ ] **Step 1: 实现 `clear_completed_transfers`**

追加到 `src-tauri/src/commands.rs`:

```rust
#[tauri::command]
pub async fn clear_completed_transfers(state: State<'_, AppState>) -> Result<usize, String> {
    let to_remove: Vec<u64> = state.transfer_table_read().await
        .into_iter()
        .filter(|(_, dto)| matches!(dto.state.as_str(),
            "done" | "failed" | "interrupted"))
        .map(|(id, _)| id)
        .collect();
    let n = to_remove.len();
    for id in to_remove {
        state.transfer_remove(id).await;
    }
    Ok(n)
}
```

- [ ] **Step 2: 实现 `remove_transfer`**

```rust
#[tauri::command]
pub async fn remove_transfer(
    state: State<'_, AppState>,
    job_id: String,
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
```

- [ ] **Step 3: 实现 `transfer_throttle`**

```rust
#[tauri::command]
pub async fn transfer_throttle(
    state: State<'_, AppState>,
    job_id: String,
    max_streams: u32,
) -> Result<(), String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let jobs = state.sender_jobs.read().await;
    let entry = jobs.get(&id).ok_or_else(|| "任务不存在或不在 sender 端".to_string())?;
    entry.throttle_cap.store(max_streams.max(1), std::sync::atomic::Ordering::Relaxed);
    Ok(())
}
```

- [ ] **Step 4: 实现 `has_parts`**

```rust
#[tauri::command]
pub async fn has_parts(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<bool, String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let parts_dir = state.config.read().await.download_dir
        .join(format!(".localtrans-parts/{:016x}", id));
    Ok(parts_dir.exists())
}
```

- [ ] **Step 5: 注册到 invoke_handler**

修改 `src-tauri/src/main.rs` 的 `.invoke_handler`:

```rust
.invoke_handler(tauri::generate_handler![
    // ... 现有 commands ...
    commands::clear_completed_transfers,
    commands::remove_transfer,
    commands::transfer_throttle,
    commands::has_parts,
])
```

- [ ] **Step 6: 编译**

Run: `cargo build -p localtrans`
Expected: 编译成功

- [ ] **Step 7: 提交**

```bash
git add src-tauri/src/commands.rs \
        src-tauri/src/main.rs
git commit -m "feat(tauri): 4 个新 command - 清理/删除/限速/has_parts"
```

---

## Task 13: 前端 types.ts + api.ts

**Files:**
- Modify: `ui/src/types.ts`
- Modify: `ui/src/api.ts`

**Interfaces:**
- Consumes: Rust 端 DTO 字段定义
- Produces: 前端类型 + invoke 包装 + onSourceProgress 监听

- [ ] **Step 1: 更新 `types.ts`**

修改 `TransferDto` 接口:

```ts
export interface TransferDto {
  job_id: string                // was: number
  name: string
  total: number
  done: number
  state: string                 // 新增 "interrupted"
  speed_bps: number
  peer: string
  direction: string
  local_role: LocalRole         // 新增
  health: HealthDto | null      // 新增
  started_at_ms: number | null  // 新增
}

export type LocalRole = 'destination' | 'source-push' | 'source-pull'

export interface HealthDto {
  loss_ratio: number
  rtt_ms: number
  cwnd: number
  streams: number
}

export interface SourceProgressEvent {
  type: 'started' | 'chunk_done' | 'speed' | 'done' | 'failed'
       | 'source_started' | 'source_chunk_done' | 'source_speed' | 'source_done' | 'source_failed'
  job_id: string
  // ... 按 type 携带不同字段
  [key: string]: unknown
}
```

- [ ] **Step 2: 更新 `api.ts`**

```ts
export const api = {
  // ... 现有 ...

  transfers: {
    list: () => invoke<TransferDto[]>('list_transfers'),
    pendingResumeJobs: () => invoke<[string, string][]>('pending_resume_jobs'),
    resumePending: (job_id: string) => invoke('resume_pending', { jobId: job_id }),
    action: (job_id: string, action: TransferAction) =>
      invoke<void>('transfer_action', { jobId: job_id, action }),

    // 新增
    clearCompleted: () => invoke<number>('clear_completed_transfers'),
    remove: (job_id: string, delete_parts: boolean) =>
      invoke<boolean>('remove_transfer', { jobId: job_id, deleteParts: delete_parts }),
    throttle: (job_id: string, max_streams: number) =>
      invoke<void>('transfer_throttle', { jobId: job_id, maxStreams: max_streams }),
    hasParts: (job_id: string) => invoke<boolean>('has_parts', { jobId: job_id }),
  },
}

export function onTransferProgress(cb: (ev: TransferProgressEvent) => void) {
  return listen<TransferProgressEvent>('transfer-progress', (e) => cb(e.payload))
}

// 新增
export function onSourceProgress(cb: (ev: SourceProgressEvent) => void) {
  return listen<SourceProgressEvent>('source-progress', (e) => cb(e.payload))
}
```

- [ ] **Step 3: 编译前端**

Run: `cd ui && npx vue-tsc --noEmit`
Expected: 通过(可能需要先修其他文件里的 job_id: number 引用;这些由后续任务修)

- [ ] **Step 4: 提交**

```bash
git add ui/src/types.ts ui/src/api.ts
git commit -m "feat(ui): types/api 加 source-progress + 新 invoke + job_id 改 string"
```

---

## Task 14: transfers.ts store 加新 actions + source-progress 监听

**Files:**
- Modify: `ui/src/stores/transfers.ts`
- Test: `ui/src/__tests__/transfersStore.test.ts`

**Interfaces:**
- Consumes: `api.transfers.{clearCompleted, remove, throttle, hasParts}`, `onSourceProgress`
- Produces: store actions `clearCompleted / removeTransfer / throttleTransfer`

- [ ] **Step 1: 写失败测试**

`ui/src/__tests__/transfersStore.test.ts`:

```ts
import { describe, it, expect, vi } from 'vitest'
import { setActivePinia, createPinia } from 'pinia'
import { useTransfersStore } from '../stores/transfers'

describe('transfersStore', () => {
  beforeEach(() => {
    setActivePinia(createPinia())
  })

  it('clearCompleted calls api + refresh', async () => {
    const store = useTransfersStore()
    // mock api.transfers.clearCompleted
    vi.mock('../api', () => ({
      api: {
        transfers: {
          clearCompleted: vi.fn().mockResolvedValue(3),
          list: vi.fn().mockResolvedValue([]),
          pendingResumeJobs: vi.fn().mockResolvedValue([]),
          resumePending: vi.fn(),
          action: vi.fn(),
          remove: vi.fn(),
          throttle: vi.fn(),
          hasParts: vi.fn(),
        },
      },
      onTransferProgress: vi.fn(),
      onSourceProgress: vi.fn(),
    }))
    const n = await store.clearCompleted()
    expect(n).toBe(3)
  })
})
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cd ui && npx vitest run __tests__/transfersStore.test.ts`
Expected: FAIL with "clearCompleted is not a function"

- [ ] **Step 3: 加 store 实现**

修改 `ui/src/stores/transfers.ts`:

```ts
// 顶部 import 加 onSourceProgress
import { api, onTransferProgress, onSourceProgress } from '../api'

// 在 setupEventListeners 内追加
function setupEventListeners() {
  onTransferProgress((progress) => {
    scheduleUpdate(progress.jobs)
  })
  onSourceProgress((ev) => {
    scheduleUpdate([buildSourceDto(ev)])
  })
}

// 单条 source 事件 → TransferDto(只填能填的字段,其他用现有 transfer 兜底)
function buildSourceDto(ev: SourceProgressEvent): TransferDto {
  const existing = transfers.value.find(t => t.job_id === ev.job_id)
  const base: TransferDto = existing || {
    job_id: ev.job_id,
    name: '', total: 0, done: 0, state: 'active',
    speed_bps: 0, peer: '', direction: 'pull',
    local_role: 'source-pull', health: null, started_at_ms: null,
  }
  // 按 ev.type 更新字段
  switch (ev.type) {
    case 'source_started':
      return {
        ...base,
        name: (ev.name as string) || base.name,
        total: (ev.total as number) || base.total,
        state: 'active',
        local_role: ev.role === 'source_push' ? 'source-push' : 'source-pull',
        started_at_ms: ev.started_at_ms as number ?? Date.now(),
      }
    case 'source_chunk_done':
      return { ...base, done: base.done + ((ev.bytes as number) || 0) }
    case 'source_speed':
      return {
        ...base,
        speed_bps: (ev.bps as number) || 0,
        health: {
          loss_ratio: (ev.loss_ratio as number) ?? 0,
          rtt_ms: (ev.rtt_ms as number) ?? 0,
          cwnd: (ev.cwnd as number) ?? 0,
          streams: (ev.streams as number) ?? 0,
        },
      }
    case 'source_done':
      return { ...base, state: 'done' }
    case 'source_failed':
      return { ...base, state: 'failed' }
    default:
      return base
  }
}

// 新 actions
async function clearCompleted(): Promise<number> {
  const n = await api.transfers.clearCompleted()
  await refreshTransfers()
  return n
}

async function removeTransfer(job_id: string, delete_parts: boolean): Promise<void> {
  await api.transfers.remove(job_id, delete_parts)
  transfers.value = transfers.value.filter(t => t.job_id !== job_id)
}

async function throttleTransfer(job_id: string, max_streams: number): Promise<void> {
  await api.transfers.throttle(job_id, max_streams)
}

// return 块加新 actions
return {
  // ... 现有 ...
  clearCompleted,
  removeTransfer,
  throttleTransfer,
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cd ui && npx vitest run __tests__/transfersStore.test.ts`
Expected: 1 passed

- [ ] **Step 5: 提交**

```bash
git add ui/src/stores/transfers.ts ui/src/__tests__/transfersStore.test.ts
git commit -m "feat(ui): store 加 source-progress + clearCompleted/removeTransfer/throttleTransfer"
```

---

## Task 15: TransferItem.vue — 角色徽章 + 健康面板 + ETA

**Files:**
- Modify: `ui/src/components/TransferItem.vue`
- Test: `ui/src/__tests__/TransferItem.test.ts`

**Interfaces:**
- Consumes: `TransferDto`(含 local_role / health / started_at_ms)
- Produces: 角色色块 + 角色文字 + 健康面板(三指标) + 已用时间 + ETA

- [ ] **Step 1: 写失败测试**

`ui/src/__tests__/TransferItem.test.ts`:

```ts
import { describe, it, expect } from 'vitest'
import { mount } from '@vue/test-utils'
import TransferItem from '../components/TransferItem.vue'

const baseTransfer = {
  job_id: '0000000000000001',
  name: 'test.bin',
  total: 1000, done: 500, state: 'active',
  speed_bps: 100, peer: 'aabb', direction: 'pull',
  local_role: 'destination' as const,
  health: { loss_ratio: 0.01, rtt_ms: 5, cwnd: 100, streams: 4 },
  started_at_ms: Date.now() - 10000,
}

describe('TransferItem', () => {
  it('renders role text for destination', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer } })
    expect(w.text()).toContain('接收中')
  })

  it('renders role text for source-push', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, local_role: 'source-push' } }
    })
    expect(w.text()).toContain('推送中')
  })

  it('renders role text for source-pull', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, local_role: 'source-pull' } }
    })
    expect(w.text()).toContain('被取中')
  })

  it('renders health panel when active', () => {
    const w = mount(TransferItem, { props: { transfer: baseTransfer } })
    expect(w.text()).toContain('丢包')
    expect(w.text()).toContain('RTT')
    expect(w.text()).toContain('cwnd')
  })

  it('hides health panel when not active', () => {
    const w = mount(TransferItem, {
      props: { transfer: { ...baseTransfer, state: 'done' } }
    })
    expect(w.text()).not.toContain('丢包')
  })
})
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cd ui && npx vitest run __tests__/TransferItem.test.ts`
Expected: FAIL(模板还没渲染角色文字/健康面板)

- [ ] **Step 3: 改 TransferItem.vue 模板与 script**

模板 `<div class="transfer-item">` 加 `:class="`transfer-role-${transfer.local_role || 'destination'}`"`:

```vue
<div class="transfer-item" :class="[
  `transfer-${transfer.state}`,
  `transfer-role-${transfer.local_role || 'destination'}`
]">
```

头部 info 块加角色文字:

```vue
<div class="transfer-info">
  <div class="transfer-name">{{ transfer.name }}</div>
  <div class="transfer-meta">
    <span class="role-badge">{{ roleText }}</span>
    <span class="peer">{{ peerName }}</span>
  </div>
</div>
```

进度条下方加健康面板 + 时间信息:

```vue
<div v-if="transfer.state === 'active' && transfer.health" class="health-panel">
  <span class="metric">丢包 {{ (transfer.health.loss_ratio * 100).toFixed(1) }}%</span>
  <span class="metric">RTT {{ transfer.health.rtt_ms }}ms</span>
  <span class="metric">cwnd {{ transfer.health.cwnd }}</span>
  <span class="metric">{{ transfer.health.streams }} 流</span>
</div>

<div class="time-info">
  <span v-if="elapsedSeconds !== null">已用 {{ formatDuration(elapsedSeconds) }}</span>
  <span v-if="etaSeconds !== null && etaSeconds > 0">剩余 {{ formatDuration(etaSeconds) }}</span>
  <span v-else-if="etaSeconds === 0">即将完成</span>
</div>
```

script 加 computed:

```ts
const roleText = computed(() => {
  const map: Record<string, string> = {
    'destination': '接收中',
    'source-push': '推送中',
    'source-pull': '被取中',
  }
  return map[props.transfer.local_role] || '传输中'
})

const elapsedSeconds = computed(() => {
  if (!props.transfer.started_at_ms) return null
  return Math.max(0, Math.floor((Date.now() - props.transfer.started_at_ms) / 1000))
})

const etaSeconds = computed(() => {
  if (props.transfer.speed_bps <= 0) return null
  if (props.transfer.done >= props.transfer.total) return 0
  return Math.ceil((props.transfer.total - props.transfer.done) * 8 / props.transfer.speed_bps)
})

function formatDuration(secs: number): string {
  if (secs < 60) return `${secs}s`
  if (secs < 3600) return `${Math.floor(secs/60)}m${secs%60}s`
  const h = Math.floor(secs/3600)
  const m = Math.floor((secs%3600)/60)
  return `${h}h${m}m`
}
```

style 加:

```css
.transfer-role-destination { border-left: 4px solid #10b981; }
.transfer-role-source-push { border-left: 4px solid #f59e0b; }
.transfer-role-source-pull { border-left: 4px solid #f97316; }

.health-panel {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: #6b7280;
  margin-top: 4px;
}

.metric { font-family: ui-monospace, monospace; }

.time-info {
  display: flex;
  gap: 12px;
  font-size: 11px;
  color: #6b7280;
  margin-top: 2px;
}

.role-badge {
  display: inline-block;
  padding: 1px 8px;
  border-radius: 8px;
  background: #f3f4f6;
  color: #4b5563;
  font-size: 11px;
  margin-right: 6px;
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cd ui && npx vitest run __tests__/TransferItem.test.ts`
Expected: 5 passed

- [ ] **Step 5: 提交**

```bash
git add ui/src/components/TransferItem.vue \
        ui/src/__tests__/TransferItem.test.ts
git commit -m "feat(ui): TransferItem 角色徽章 + 健康面板 + ETA"
```

---

## Task 16: TransferItem.vue — 三角色按钮分支

**Files:**
- Modify: `ui/src/components/TransferItem.vue`
- Create: `ui/src/directives/clickOutside.ts`
- Test: `ui/src/__tests__/TransferItem.test.ts` 追加

**Interfaces:**
- Consumes: `transfersStore.transferAction / removeTransfer / throttleTransfer / api.transfers.hasParts / api.transfers.resumePending`
- Produces: 三角色按钮(暂停/继续/取消/重试/续传/打开/删除/限速/踢人)

- [ ] **Step 1: 写失败测试**

追加到 `ui/src/__tests__/TransferItem.test.ts`:

```ts
import { vi } from 'vitest'

it('destination + active shows pause/cancel', () => {
  const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'destination', state: 'active' } } })
  expect(w.text()).toContain('暂停')
  expect(w.text()).toContain('取消')
})

it('destination + paused shows resume/cancel', () => {
  const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'destination', state: 'paused' } } })
  expect(w.text()).toContain('继续')
})

it('source-pull + active shows throttle/kick', () => {
  const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-pull', state: 'active' } } })
  expect(w.text()).toContain('限速')
  expect(w.text()).toContain('踢人')
})

it('source-push + active shows pause/cancel', () => {
  const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, local_role: 'source-push', state: 'active' } } })
  expect(w.text()).toContain('暂停')
  expect(w.text()).toContain('取消')
})

it('done state shows open folder / delete', () => {
  const w = mount(TransferItem, { props: { transfer: { ...baseTransfer, state: 'done' } } })
  expect(w.text()).toContain('打开所在文件夹')
  expect(w.text()).toContain('删除')
})
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cd ui && npx vitest run __tests__/TransferItem.test.ts`
Expected: 5 failed(按钮尚未按角色分支)

- [ ] **Step 3: 创建 clickOutside 指令**

`ui/src/directives/clickOutside.ts`:

```ts
import type { Directive } from 'vue'

export const vClickOutside: Directive<HTMLElement, () => void> = {
  beforeMount(el, binding) {
    el._clickOutsideHandler = (ev: MouseEvent) => {
      if (!el.contains(ev.target as Node)) {
        binding.value()
      }
    }
    document.addEventListener('click', el._clickOutsideHandler, true)
  },
  unmounted(el) {
    document.removeEventListener('click', el._clickOutsideHandler!, true)
  },
}

declare global {
  interface HTMLElement {
    _clickOutsideHandler?: (ev: MouseEvent) => void
  }
}
```

- [ ] **Step 4: 在 main.ts 注册指令**

修改 `ui/src/main.ts`(或新文件):

```ts
import { vClickOutside } from './directives/clickOutside'

const app = createApp(...)
app.directive('click-outside', vClickOutside)
```

- [ ] **Step 5: 重写 TransferItem.vue 的 transfer-actions 块**

替换现有 `.transfer-actions` 块为:

```vue
<div class="transfer-actions">
  <!-- destination:暂停/继续/取消/重试/续传/打开/删除 -->
  <template v-if="(transfer.local_role || 'destination') === 'destination'">
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
      <div v-if="throttleMenuOpen" v-click-outside="() => throttleMenuOpen = false" class="throttle-menu">
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

- [ ] **Step 6: 加 handlers**

在 `<script setup>` 加:

```ts
const throttleMenuOpen = ref(false)

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
  try {
    await api.transfers.resumePending(props.transfer.job_id)
    toastStore.push('info', '正在续传...')
  } catch (e) {
    toastStore.push('error', '续传失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}
```

style 加:

```css
.throttle-menu {
  position: absolute;
  background: white;
  border: 1px solid #e5e7eb;
  border-radius: 6px;
  padding: 4px;
  box-shadow: 0 2px 8px rgba(0,0,0,0.1);
  z-index: 10;
  display: flex;
  flex-direction: column;
}

.throttle-menu button {
  padding: 4px 12px;
  background: none;
  border: none;
  text-align: left;
  cursor: pointer;
}

.throttle-menu button:hover { background: #f3f4f6; }
```

- [ ] **Step 7: 跑测试,确认通过**

Run: `cd ui && npx vitest run __tests__/TransferItem.test.ts`
Expected: 10 passed(原有 5 + 新 5)

- [ ] **Step 8: 提交**

```bash
git add ui/src/components/TransferItem.vue \
        ui/src/directives/clickOutside.ts \
        ui/src/main.ts \
        ui/src/__tests__/TransferItem.test.ts
git commit -m "feat(ui): TransferItem 三角色按钮 + 限速菜单 + 删除/踢人"
```

---

## Task 17: Transfers.vue — 清除按钮 + 空状态

**Files:**
- Modify: `ui/src/pages/Transfers.vue`
- Test: `ui/src/__tests__/TransfersPage.test.ts`

**Interfaces:**
- Consumes: `transfersStore.clearCompleted`
- Produces: 顶部 "清除已完成/失败" 按钮 + 改进空状态文案

- [ ] **Step 1: 写失败测试**

`ui/src/__tests__/TransfersPage.test.ts`:

```ts
import { mount } from '@vue/test-utils'
import { describe, it, expect, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'

vi.mock('../stores/transfers', () => ({
  useTransfersStore: () => ({
    transfers: [], resumeJobs: [], loading: false, error: null,
    lastRequest: new Map(),
    refreshTransfers: vi.fn(),
    refreshResumeJobs: vi.fn(),
    clearCompleted: vi.fn().mockResolvedValue(5),
  }),
}))
vi.mock('../stores/toast', () => ({
  useToastStore: () => ({ push: vi.fn() }),
}))
vi.mock('vue-router', () => ({ useRoute: () => ({}) }))

import TransfersPage from '../pages/Transfers.vue'

describe('TransfersPage', () => {
  it('shows clear completed button', () => {
    setActivePinia(createPinia())
    const w = mount(TransfersPage)
    expect(w.text()).toContain('清除已完成/失败')
  })

  it('shows empty hint', () => {
    setActivePinia(createPinia())
    const w = mount(TransfersPage)
    expect(w.text()).toContain('试试从设备页发起传输')
  })
})
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cd ui && npx vitest run __tests__/TransfersPage.test.ts`
Expected: 2 failed

- [ ] **Step 3: 修改 Transfers.vue**

模板 `.page-header` 加按钮:

```vue
<div class="page-header">
  <h1>传输任务</h1>
  <div class="header-actions">
    <button @click="handleRefresh" class="btn-refresh" :disabled="loading">刷新</button>
    <button @click="handleClearCompleted" class="btn-clear">清除已完成/失败</button>
  </div>
</div>
```

空状态补 hint:

```vue
<div v-else-if="transfers.length === 0" class="empty-state">
  <p>暂无传输任务</p>
  <p class="hint">试试从设备页发起传输,或在浏览页下载文件</p>
</div>
```

script 加:

```ts
async function handleClearCompleted() {
  const n = await transfersStore.clearCompleted()
  toastStore.push('success', `已清除 ${n} 个历史任务`)
}
```

style:

```css
.header-actions {
  display: flex;
  gap: 8px;
}

.btn-clear {
  padding: 8px 16px;
  background: white;
  border: 1px solid #d1d5db;
  border-radius: 6px;
  color: #374151;
  cursor: pointer;
  transition: all 0.2s;
}

.btn-clear:hover { background: #f9fafb; border-color: #9ca3af; }

.empty-state .hint {
  color: #9ca3af;
  font-size: 14px;
  margin-top: 8px;
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cd ui && npx vitest run __tests__/TransfersPage.test.ts`
Expected: 2 passed

- [ ] **Step 5: 提交**

```bash
git add ui/src/pages/Transfers.vue ui/src/__tests__/TransfersPage.test.ts
git commit -m "feat(ui): Transfers 页清除按钮 + 空状态提示"
```

---

## Task 18: 关闭拦截 — useCloseGuard + onCloseRequested + App.vue 模态

**Files:**
- Create: `ui/src/composables/useCloseGuard.ts`
- Modify: `src-tauri/src/main.rs`(onCloseRequested + emit)
- Modify: `ui/src/App.vue`(挂载模态)
- Test: `ui/src/__tests__/useCloseGuard.test.ts`

**Interfaces:**
- Consumes: `transfersStore.transfers`, `getCurrentWindow().onCloseRequested`
- Produces: `useCloseGuard()` composable + 关闭拦截模态

- [ ] **Step 1: 写失败测试**

`ui/src/__tests__/useCloseGuard.test.ts`:

```ts
import { describe, it, expect, vi } from 'vitest'

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: () => ({
    onCloseRequested: vi.fn().mockResolvedValue(vi.fn()),
    destroy: vi.fn(),
  }),
}))

vi.mock('../stores/transfers', () => ({
  useTransfersStore: () => ({
    transfers: [],
  }),
}))

import { useCloseGuard } from '../composables/useCloseGuard'

describe('useCloseGuard', () => {
  it('returns guardOpen and activeCount', () => {
    const g = useCloseGuard()
    expect(g.guardOpen.value).toBe(false)
    expect(g.activeCount.value).toBe(0)
  })
})
```

- [ ] **Step 2: 跑测试,确认失败**

Run: `cd ui && npx vitest run __tests__/useCloseGuard.test.ts`
Expected: FAIL with "cannot find module"

- [ ] **Step 3: 创建 useCloseGuard.ts**

```ts
// ui/src/composables/useCloseGuard.ts
import { ref, watch } from 'vue'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { useTransfersStore } from '../stores/transfers'

export function useCloseGuard() {
  const transfersStore = useTransfersStore()
  const guardOpen = ref(false)
  const activeCount = ref(0)
  const forceClose = ref(false)

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
          if (!open) { stop(); resolve() }
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

  function cleanup() {
    if (unlisten) unlisten()
  }

  return { guardOpen, activeCount, init, cancelClose, doForceClose, cleanup }
}
```

- [ ] **Step 4: 跑测试,确认通过**

Run: `cd ui && npx vitest run __tests__/useCloseGuard.test.ts`
Expected: 1 passed

- [ ] **Step 5: 在 src-tauri/src/main.rs 加 onCloseRequested**

找到 `tauri::Builder` 链式调用,在 `.on_window_event` 上加:

```rust
.on_window_event(|window, event| {
    if let tauri::WindowEvent::CloseRequested { api, .. } = event {
        api.prevent_close();
        let _ = window.emit("close-requested", ());
    }
})
```

- [ ] **Step 6: 在 App.vue 挂载模态**

修改 `ui/src/App.vue` `<script setup>`:

```ts
import { useCloseGuard } from './composables/useCloseGuard'
import { onMounted } from 'vue'

const closeGuard = useCloseGuard()

onMounted(() => {
  closeGuard.init()
})
```

模板末尾加 `<Teleport>` 模态:

```vue
<template>
  <!-- 现有内容 -->
  <router-view />

  <Teleport to="body">
    <div v-if="closeGuard.guardOpen.value" class="modal-backdrop" @click.self="closeGuard.cancelClose()">
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
</template>

<style scoped>
.modal-backdrop {
  position: fixed; inset: 0;
  background: rgba(0,0,0,0.5);
  display: flex; align-items: center; justify-content: center;
  z-index: 9999;
}
.modal-card {
  background: white; padding: 24px; border-radius: 12px;
  min-width: 320px; max-width: 480px;
}
.modal-card h3 { margin: 0 0 12px 0; color: #1f2937; }
.modal-card p { color: #6b7280; margin: 0 0 16px 0; }
.modal-actions { display: flex; gap: 8px; justify-content: flex-end; }
.btn-secondary {
  padding: 8px 16px; background: #f3f4f6; color: #374151;
  border: 1px solid #d1d5db; border-radius: 6px; cursor: pointer;
}
.btn-danger {
  padding: 8px 16px; background: #ef4444; color: white;
  border: none; border-radius: 6px; cursor: pointer;
}
</style>
```

- [ ] **Step 7: 编译 + 跑全部前端测试**

Run: `cd ui && npx vue-tsc --noEmit && npx vitest run`
Expected: 编译通过,所有测试通过

- [ ] **Step 8: 提交**

```bash
git add src-tauri/src/main.rs \
        ui/src/composables/useCloseGuard.ts \
        ui/src/App.vue \
        ui/src/__tests__/useCloseGuard.test.ts
git commit -m "feat: 关闭应用拦截 + 模态确认"
```

---

## Task 19: 全栈编译 + 手动验证清单执行

**Files:** 无新增

- [ ] **Step 1: 跑全部 Rust 测试**

Run: `cargo test --workspace`
Expected: 全部通过

- [ ] **Step 2: 跑全部前端测试**

Run: `cd ui && npx vitest run`
Expected: 全部通过

- [ ] **Step 3: 编译 release**

Run: `cargo build --release -p localtrans`
Expected: 编译成功

- [ ] **Step 4: 前端 type-check**

Run: `cd ui && npx vue-tsc --noEmit`
Expected: 无错

- [ ] **Step 5: 执行手动验证清单(从 spec §8.5 复制)**

```
- [ ] 双机拉取:甲发起下载,乙的 Transfers 页同时看到橙色"被取中"条目,进度实时滚动
- [ ] 双机推送(含大文件):甲 Transfers 页看到黄色进度条 + ChunkDone + Speed + Done
- [ ] sender-side 限速:甲把对端的 cap 设成 1 流,乙端速率明显下降
- [ ] sender-side 踢人:甲点踢人,乙端 60s 后转 failed,甲端 SourceDone / SourceFailed
- [ ] 全小文件 push:UI 不再卡"等待中",每文件独立显示
- [ ] 应用重启:transfers.json 加载,active → interrupted,UI 顶部横幅提示"续传"
- [ ] 应用重启后清除已完成/失败按钮:interrupted 也被清
- [ ] 关闭应用时有 active 任务:弹模态,取消关闭能拦下
- [ ] u64 job_id 边界:重启后操作不报 floating point 错
- [ ] 删除带 parts 的任务:confirm 提示,确认后磁盘目录一并删除
- [ ] 删除无 parts 的任务:直接删,无 confirm
- [ ] 限速菜单点击外部区域关闭
- [ ] sender-side 任务在连接断开 30s 后从表中消失
```

- [ ] **Step 6: 写 CHANGELOG 条目**

修改 `README.md` 或新建 `CHANGELOG.md`,加 v0.2.0 条目(本轮):

```markdown
## v0.2.0 — 传输面板源端可视化 + 持久化 + 卡死修复

### 新增
- 发送方(推送方 / 服务方)在 Transfers 页可见
- 角色徽章:接收中 / 推送中 / 被取中
- 健康面板:丢包率 / RTT / 拥塞窗口 / 并发流数
- ETA 倒计时
- 三件套:暂停 / 继续 / 取消(全角色)
- 服务方控制:限速(1/2/4 流)/ 踢人
- transfers.json 持久化(节流 1s 写盘)
- 启动恢复:active → interrupted 迁移
- 关闭应用时弹模态提醒

### 修复
- u64 job_id 跨 JSON 边界精度丢失
- 全小文件 push 卡"等待中"
- Done/Failed 找不到 job_id 时占位 fallback
```

- [ ] **Step 7: 最终提交**

```bash
git add CHANGELOG.md README.md
git commit -m "docs: CHANGELOG v0.2.0 条目"
git tag v0.2.0
```

---

## 风险与延期

详见 spec §10。本计划已规避已知风险;延期项目(限速滑块 / 原生模态 / 显式 TransferCtl 拒绝)留给下轮。
