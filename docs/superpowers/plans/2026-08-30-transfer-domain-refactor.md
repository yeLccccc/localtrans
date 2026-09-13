# 传输域重构实现计划(单卡片状态机/磁盘真相源/并发队列/父子卡片)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 落实 specs/2026-08-30-transfer-page-design.md——1 任务 1 卡片的状态机、manifest 磁盘真相源+两级删除+历史记录、有限并发队列、多文件父卡片、双进度/别名/分区显示,修复卡片异常/删除不同步/复活幽灵行等结构性缺陷。

**Architecture:** 三层推进:①core 层扩展 Manifest 元数据(卡片元数据并入磁盘真相)、收窄 GC、新增孤儿扫描;②PC 壳层引入 `TransferCard` 状态机(单写者 apply(event)),卡片 ID 恒定(壳层分配 card_id,维护 engine_id→card_id 映射,消除占位→实件替换),删除走仲裁(cancelling→引擎确认/5s),启动时 manifest 优先重建;③前端父子卡片+双进度+活动/历史分区+历史记录入口。Android(FFI/Kotlin)不在本计划,后续批次单独做。

**Tech Stack:** Rust(tokio/quinn/serde)+ Tauri 2 + Vue 3/Pinia。测试:cargo test(core/壳层单测+E2E)、Vitest(UI store)。

**版本:** v0.12.0(收尾任务统一升版本号)。

## Global Constraints

- 提交信息:中文前缀(`feat(core):`/`fix(壳):`/`feat(ui):` 等)+空行+`Co-Authored-By: Claude <noreply@anthropic.com>`。
- main 分支直接工作(项目惯例,无 PR 流程)。
- **日志红线**:明文文件名/路径/IP 不进 tracing 日志;指纹只打前 8 hex;秒传日志只打 hash 前 8 位;PSK/配对码不进日志。
- **错误信息不回显内部绝对路径**(fail_reason 走既有 friendlyError 脱敏体系)。
- manifest/索引不含超出既有的敏感明文(指纹 hex、文件名已在盘,无新暴露面)。
- 状态机之外的任务事件:丢弃+warn 日志,不复活(spec §1.3)。
- 传输协议 wire 格式(控制面 JSON)本计划**不动**——全部改动在引擎之上的壳层与 manifest 本地结构(serde `#[serde(default)]` 向后兼容旧 manifest)。
- 事件驱动原则:不新增轮询;进度仍走 4Hz 聚合泵(FAST 250ms/SLOW 1s)。
- 每个 Task:TDD(先写失败测试再实现)、跑绿后原子提交;`cargo test -p localtrans-core` / `cargo test --manifest-path src-tauri/Cargo.toml` / `cd ui && npx vitest run` 按任务范围选用。
- 并发纪律:所有 transfers 表写操作必须在锁内完成、不跨 await(单写者 apply);4Hz 泵只读快照。
- 旧数据兼容:v0.11 的 transfers.json 与 .localtrans-parts/manifest.json(无 meta 字段)必须能无损加载(`#[serde(default)]`),重启不丢历史卡片。

## 现状锚点(实现者必读)

- `crates/localtrans-core/src/transfer/manifest.rs`:`Manifest{file_name,total_size,chunk_hashes,received,peer,share_id,rel}`,save/load 已有。
- `crates/localtrans-core/src/transfer/mod.rs`:`pending_jobs(parts_root)` 只返回有缺失块的任务;`gc_stale_parts` 含 7 天 mtime 盲删+位图全真孤儿删。
- `src-tauri/src/main.rs`:`AppState.transfers: Mutex<HashMap<u64, TransferDto>>`;`transfer_update/transfer_remove/transfer_get_mut`(get_mut 克隆→改→insert 跨 await,丢更新根因);`next_placeholder_id`(u64::MAX 递减);`migrate_on_startup`;`load_transfers_history`;source 事件桥(PE::SourceStarted/ChunkDone/Speed/Done/Failed);4Hz 聚合泵;1s 持久化泵(150 条封顶)。
- `src-tauri/src/commands.rs`:`remove_transfer`(只 remove+可选删 parts,无仲裁,L2305);`clear_completed_transfers`(L2289);`has_parts`(L2344);推送进度 `handle_push_progress_event`(Started 删占位插实件,L591 附近);`start_download_dir` 聚合占位行(L812 附近);`xfer_lock: Arc<tokio::sync::Mutex<()>>` 全局串行。
- `ui/src/types.ts` `TransferDto`;`ui/src/stores/transfers.ts`(4Hz 防抖+乐观更新+lastRequest);`ui/src/components/TransferItem.vue`(角色×状态操作矩阵;remote_done 未显示);`ui/src/pages/Transfers.vue`(刷新按钮+平铺列表)。
- 进度链:Rust 聚合泵→`transfer-progress` 全量快照→Store shallowRef 250ms 防抖→CSS 补间。**事件是全量整表替换,前端免合并。**

---

### Task 1: core — Manifest 元数据扩展 + 状态迁移历史

**Files:**
- Modify: `crates/localtrans-core/src/transfer/manifest.rs`
- Modify: `crates/localtrans-core/src/transfer/mod.rs`(re-export)
- Test: `crates/localtrans-core/src/transfer/manifest.rs` 测试模块内新增

**Interfaces:**
- Produces(后续任务依赖,签名逐字):
  - `pub struct TransferMeta { pub direction: String, pub local_role: String, pub display_name: String, pub peer_hex: String, pub created_at_ms: i64, pub finished_at_ms: Option<i64>, pub fail_reason: Option<String>, pub source_path: Option<String>, pub batch_label: Option<String> }`(全部 `#[serde(default)]`,struct 另 derive `Default`)
  - `impl Manifest { pub fn meta(&self) -> Option<&TransferMeta>; pub fn set_meta(&mut self, m: TransferMeta); }`
  - `pub fn patch_manifest_meta(parts_root: &Path, job_id: u64, f: impl FnOnce(&mut TransferMeta)) -> bool`(load→改→save 原子往返;manifest 不存在返回 false;**只在终态后调用**,活动期间 PartWriter 节流写会覆盖)
  - `pub fn append_history(parts_root: &Path, job_id: u64, ev: &HistoryEvent) -> std::io::Result<()>`(写 `<parts>/.localtrans-parts/{job:016x}/history.jsonl` 追加一行)
  - `pub struct HistoryEvent { pub ts_ms: i64, pub from: String, pub to: String, pub reason: Option<String> }`(Serialize+Deserialize+Clone+Debug)
  - `pub fn load_history(parts_root: &Path, job_id: u64) -> Vec<HistoryEvent>`(损坏行跳过)

- [ ] **Step 1: 写失败测试**(manifest.rs 测试模块追加)

```rust
    #[test]
    fn meta_roundtrip_and_default_absent() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![1u8; CHUNK_SIZE + 10]).unwrap();
        let mut m = Manifest::build(&p).unwrap();
        // 旧版 manifest 无 meta 字段 → None,加载不报错
        assert!(m.meta().is_none());

        let mut meta = TransferMeta::default();
        meta.direction = "pull".into();
        meta.local_role = "destination".into();
        meta.display_name = "电影合集".into();
        meta.peer_hex = "aabbccdd11223344".into();
        meta.created_at_ms = 1770000000000i64;
        m.set_meta(meta);

        let dir = tmp.path().join("parts");
        m.save(&dir).unwrap();
        let m2 = Manifest::load(&dir).unwrap();
        assert_eq!(m2.meta().unwrap().display_name, "电影合集");
        assert_eq!(m2.meta().unwrap().direction, "pull");
    }

    #[test]
    fn patch_manifest_meta_updates_in_place() {
        let tmp = TempDir::new().unwrap();
        let p = tmp.path().join("f.bin");
        fs::write(&p, vec![2u8; 5]).unwrap();
        let mut m = Manifest::build(&p).unwrap();
        let mut meta = TransferMeta::default();
        meta.direction = "pull".into();
        meta.created_at_ms = 100;
        m.set_meta(meta);
        let dir = tmp.path().join(".localtrans-parts/00000000000000ff");
        m.save(&dir).unwrap();

        let ok = patch_manifest_meta(tmp.path(), 0xff, |mt| {
            mt.fail_reason = Some("对端拒绝".into());
            mt.finished_at_ms = Some(999);
        });
        assert!(ok);
        let m2 = Manifest::load(&dir).unwrap();
        assert_eq!(m2.meta().unwrap().fail_reason.as_deref(), Some("对端拒绝"));
        assert_eq!(m2.meta().unwrap().finished_at_ms, Some(999));
        assert!(!patch_manifest_meta(tmp.path(), 0xdead, |_| {}), "不存在应返回 false");
    }

    #[test]
    fn history_append_and_load_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000007");
        fs::create_dir_all(&job).unwrap();
        append_history(tmp.path(), 7, &HistoryEvent {
            ts_ms: 1, from: "active".into(), to: "paused".into(), reason: None,
        }).unwrap();
        append_history(tmp.path(), 7, &HistoryEvent {
            ts_ms: 2, from: "paused".into(), to: "failed".into(), reason: Some("对端断开".into()),
        }).unwrap();
        let h = load_history(tmp.path(), 7);
        assert_eq!(h.len(), 2);
        assert_eq!(h[1].reason.as_deref(), Some("对端断开"));
        // 损坏行跳过不炸
        fs::write(job.join("history.jsonl"), "not json\n").unwrap();
        assert!(load_history(tmp.path(), 7).is_empty());
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core transfer::manifest`
Expected: 编译失败(`TransferMeta`/`patch_manifest_meta` 未定义)

- [ ] **Step 3: 实现**

`Manifest` struct 加字段(`received` 之后):
```rust
    /// 卡片元数据(2026-08-30 传输域定案:manifest 唯一真相)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<TransferMeta>,
```
注意:`Manifest::build`/`from_meta`/`from_meta_with_source` 三个构造点补 `meta: None`;struct 定义+访问器+两个自由函数按 Interfaces 签名实现。`transfer/mod.rs` 加:
```rust
pub use manifest::{TransferMeta, HistoryEvent, patch_manifest_meta, append_history, load_history};
```
(save 的原子性沿用现状直接 write,不引入 tmp+rename——与 PartWriter 既有行为一致,不另开先例。)

- [ ] **Step 4: 跑测试转绿**

Run: `cargo test -p localtrans-core transfer::manifest`
Expected: 全 PASS(含既有 3 个测试)

- [ ] **Step 5: 提交** `feat(core): manifest 扩展卡片元数据与状态迁移历史`

---

### Task 2: core — GC 收窄(去 7 天盲删)+ 孤儿全量扫描

**Files:**
- Modify: `crates/localtrans-core/src/transfer/mod.rs`
- Test: 同文件 `gc_parts_tests` 模块改造+新增 `orphan_jobs_tests`

**Interfaces:**
- Produces:
  - `pub fn gc_stale_parts(parts_root: &Path, now_epoch_secs: u64) -> usize` —— 签名不变,**语义收窄:只删"位图全真孤儿"**;7 天 mtime 盲删删除(spec §2.4:改为空间紧张时提示,不静默删用户数据)
  - `pub struct OrphanJob { pub job_id: u64, pub manifest: Manifest }`(pub 字段,Clone)
  - `pub fn orphan_jobs(parts_root: &Path) -> Vec<OrphanJob>` —— 扫描全部含合法 manifest 的任务目录(含位图全真的,与 pending_jobs 区别;pending_jobs 维持原语义不动)

- [ ] **Step 1: 写失败测试**

`gc_parts_tests` 改造(现有测试语义随 GC 收窄调整)+ 新增:

```rust
    #[test]
    fn stale_over_7_days_now_survives() {
        // 2026-08-30 定案:取消 7 天盲删——parts 是用户数据,只能显式删
        let tmp = tempdir().unwrap();
        let job = tmp.path().join(".localtrans-parts/0000000000000042");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("000.part"), b"x").unwrap();
        set_mtime_old(&job);
        assert_eq!(gc_stale_parts(tmp.path(), now()), 0);
        assert!(job.exists(), "超 7 天不再盲删");
    }

    #[test]
    fn orphan_jobs_lists_all_incl_fully_received() {
        use crate::transfer::orphan_jobs;
        let tmp = tempdir().unwrap();
        let root = tmp.path().join(".localtrans-parts");
        // 任务 A:半收(缺块)
        let a = root.join("0000000000000001");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("manifest.json"),
            r#"{"file_name":"a.bin","total_size":5,"chunk_hashes":["00"],"received":[false]}"#).unwrap();
        // 任务 B:位图全真(差一步 finalize)
        let b = root.join("0000000000000002");
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(b.join("manifest.json"),
            r#"{"file_name":"b.bin","total_size":5,"chunk_hashes":["00"],"received":[true]}"#).unwrap();
        let jobs = orphan_jobs(tmp.path());
        assert_eq!(jobs.len(), 2, "全真孤儿也要列出(历史记录入口)");
        assert!(jobs.iter().any(|j| j.job_id == 2));
    }
```
同时改造现有测试 `stale_job_dir_over_7_days_is_removed` → 断言 `removed == 0` 且目录存活(与上面新测试合并成一个,删除旧的);`fully_received_orphan_removed_regardless_of_age` 保持(全真仍删);`pending_jobs` 既有行为测试不动。

- [ ] **Step 2: 跑红** — `cargo test -p localtrans-core gc_parts` Expected: 新测试 FAIL(旧 GC 仍删 7 天目录)

- [ ] **Step 3: 实现** —— `gc_stale_parts` 删除 stale 分支(`MAX_AGE_SECS`/mtime 检查整段移除);新增 `OrphanJob`+`orphan_jobs`(结构参照 `pending_jobs`,去掉 `missing_chunks().is_empty()` 过滤);`now_epoch_secs` 参数保留(签名不变,调用方 main.rs L204-215 不用改,函数内不再使用——加 `let _ = now_epoch_secs;` 保签名)。

- [ ] **Step 4: 跑绿** — `cargo test -p localtrans-core transfer::` 全 PASS

- [ ] **Step 5: 提交** `feat(core): GC 收窄为仅清位图全真孤儿;新增 orphan_jobs 全量扫描`

---

### Task 3: 壳层 — TransferCard 状态机(纯逻辑)

**Files:**
- Create: `src-tauri/src/transfer_state.rs`
- Modify: `src-tauri/src/main.rs`(mod transfer_state; 声明处与其他 mod 并列)
- Test: `src-tauri/src/transfer_state.rs` 内 `#[cfg(test)]`

**Interfaces:**
- Consumes: `crate::TransferDto`(main.rs 现有)
- Produces(Task 4/5/6 依赖,逐字):

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum CardState { Pending, Active, Paused, Cancelling, Done, Failed, Interrupted }

#[derive(Debug, Clone)]
pub enum CardEvent {
    Started,                                   // 引擎/对端开始传输
    Progress { done: u64, total: u64, speed_bps: u64, remote_done: u64,
               health: Option<crate::HealthDto> },
    Paused, Resumed,
    Failed { reason: Option<String> },
    Finished,                                  // done
    Interrupted,
    CancelRequested,                           // 用户删除/取消活动任务
    CancelConfirmed,                           // 引擎回执(或 5s 超时兜底)
    QueueTimeout,                              // pending 60s 未启动
}
impl CardState { pub fn as_str(&self) -> &'static str }   // "pending"/"active"/...
pub fn state_from_str(s: &str) -> Option<CardState>

#[derive(Debug, Clone)]
pub struct TransferCard {
    pub dto: TransferDto,
    /// 引擎真实 job_id(None=尚未拿到/无引擎任务;parts 目录名用它)
    pub engine_id: Option<u64>,
    /// 视图移除(两级删除第一级):true 时不出现在活动列表,历史记录可见
    pub removed: bool,
    /// 删除级别跟踪:CancelRequested 后进入 cancelling,等待确认
    pub cancelling_since_ms: Option<i64>,
}
impl TransferCard {
    pub fn new(dto: TransferDto) -> Self;
    /// 单写者入口:应用事件,返回是否接受。非法迁移返回 false(调用方 warn+丢弃)。
    pub fn apply(&mut self, ev: CardEvent, now_ms: i64) -> bool;
    /// 状态迁移成功后返回 (from_state, to_state) 供 history 追加——由 apply 内部记录
    pub fn last_transition(&self) -> Option<(String, String)>;
}
```

**状态机迁移表(apply 的全部合法边,实现即此表,表外一律 false):**

| from | event | to | 附加动作 |
|---|---|---|---|
| Pending | Started | Active | dto.started_at_ms=now |
| Pending | QueueTimeout | Failed | fail_reason=Some("排队超时") |
| Pending | Failed | Failed | 记 reason |
| Pending | CancelRequested | Cancelling | cancelling_since=now(占位任务无引擎,由命令层直接 CancelConfirmed) |
| Active | Progress | Active | 更新 done/total/speed/remote_done/health |
| Active | Paused | Paused | speed=0 |
| Active | Failed | Failed | 记 reason,finished_at=now |
| Active | Finished | Done | done=total 兜底,finished_at=now |
| Active | Interrupted | Interrupted | speed=0,finished_at=now |
| Active | CancelRequested | Cancelling | cancelling_since=now;**不立即改 state**(等待 CancelConfirmed 才离开——spec §1.3) |
| Paused | Resumed | Active | — |
| Paused | Failed/Interrupted/Finished | 同名终态 | — |
| Paused | CancelRequested | Cancelling | 同上 |
| Cancelling | CancelConfirmed | (调用方删除卡片/转 removed) | apply 返回 true 且置 dto.state 仍为 cancelling;删除动作在命令层 |
| Cancelling | Failed/Interrupted/Finished | 同名终态 | 引擎先回终态也算确认,cancelling_since 清 None |
| Done/Failed/Interrupted | (任何事件) | 不变 | **终态吸收:全部返回 false**(不复活,spec §1.4) |

例外(合法复活,显式列出):`resume_pending`/`retry` 走**新建卡片**(新 card_id),不复用终态卡——`apply` 不提供复活边。

- [ ] **Step 1: 写失败测试**(transfer_state.rs,≥10 个用例覆盖上表关键边+非法边)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransferDto;

    fn dto(state: &str) -> TransferDto {
        TransferDto {
            job_id: 1, name: "t.bin".into(), total: 100, done: 0,
            state: state.into(), speed_bps: 0, peer: "aa".repeat(32),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }
    }
    const NOW: i64 = 1_770_000_000_000;

    #[test]
    fn pending_to_active_to_done_happy_path() {
        let mut c = TransferCard::new(dto("pending"));
        assert!(c.apply(CardEvent::Started, NOW));
        assert_eq!(c.dto.state, "active");
        assert_eq!(c.dto.started_at_ms, Some(NOW));
        assert!(c.apply(CardEvent::Progress { done: 40, total: 100, speed_bps: 10, remote_done: 0, health: None }, NOW));
        assert_eq!(c.dto.done, 40);
        assert!(c.apply(CardEvent::Finished, NOW + 5));
        assert_eq!(c.dto.state, "done");
        assert_eq!(c.dto.finished_at_ms, Some(NOW + 5));
    }

    #[test]
    fn terminal_absorbs_everything_no_revive() {
        let mut c = TransferCard::new(dto("failed"));
        assert!(!c.apply(CardEvent::Started, NOW));
        assert!(!c.apply(CardEvent::Progress { done: 1, total: 1, speed_bps: 1, remote_done: 0, health: None }, NOW));
        assert_eq!(c.dto.state, "failed", "终态不复活");
    }

    #[test]
    fn active_cancel_keeps_state_until_confirmed() {
        let mut c = TransferCard::new(dto("active"));
        assert!(c.apply(CardEvent::CancelRequested, NOW));
        assert_eq!(c.dto.state, "cancelling");
        assert!(c.cancelling_since_ms.is_some());
        assert!(c.apply(CardEvent::CancelConfirmed, NOW));
        // 确认后由命令层负责移除;状态保持 cancelling
        assert_eq!(c.dto.state, "cancelling");
    }

    #[test]
    fn engine_terminal_also_clears_cancelling() {
        let mut c = TransferCard::new(dto("active"));
        c.apply(CardEvent::CancelRequested, NOW);
        assert!(c.apply(CardEvent::Interrupted, NOW));
        assert_eq!(c.dto.state, "interrupted");
        assert!(c.cancelling_since_ms.is_none());
    }

    #[test]
    fn paused_resume_rejected_from_pending() {
        let mut c = TransferCard::new(dto("pending"));
        assert!(!c.apply(CardEvent::Resumed, NOW), "pending 不能 resume");
        let mut p = TransferCard::new(dto("paused"));
        assert!(p.apply(CardEvent::Resumed, NOW));
        assert_eq!(p.dto.state, "active");
    }

    #[test]
    fn queue_timeout_fails_pending() {
        let mut c = TransferCard::new(dto("pending"));
        assert!(c.apply(CardEvent::QueueTimeout, NOW));
        assert_eq!(c.dto.state, "failed");
        assert_eq!(c.dto.fail_reason.as_deref(), Some("排队超时"));
    }

    #[test]
    fn failed_records_reason_and_freezes_time() {
        let mut c = TransferCard::new(dto("active"));
        assert!(c.apply(CardEvent::Failed { reason: Some("对端拒绝".into()) }, NOW));
        assert_eq!(c.dto.fail_reason.as_deref(), Some("对端拒绝"));
        assert_eq!(c.dto.finished_at_ms, Some(NOW));
    }

    #[test]
    fn last_transition_reports_edge() {
        let mut c = TransferCard::new(dto("pending"));
        c.apply(CardEvent::Started, NOW);
        assert_eq!(c.last_transition().unwrap(), ("pending".to_string(), "active".to_string()));
    }

    #[test]
    fn state_str_roundtrip() {
        for s in ["pending","active","paused","cancelling","done","failed","interrupted"] {
            assert_eq!(state_from_str(s).unwrap().as_str(), s);
        }
        assert!(state_from_str("weird").is_none());
    }

    #[test]
    fn progress_updates_remote_done_for_sender() {
        let mut c = TransferCard::new(dto("active"));
        c.dto.local_role = "source-push".into();
        assert!(c.apply(CardEvent::Progress { done: 80, total: 100, speed_bps: 5, remote_done: 60, health: None }, NOW));
        assert_eq!(c.dto.remote_done, 60);
    }
}
```

- [ ] **Step 2: 跑红** — `cargo test --manifest-path src-tauri/Cargo.toml transfer_state` Expected: 编译失败(模块不存在)

- [ ] **Step 3: 实现** —— 按 Interfaces+迁移表实现 `transfer_state.rs`(~200 行)。`TransferDto` 需同步加 4 个字段(见 Task 4 Step 3 的字段定义,本任务先加齐以通过编译:`queue_pos/batch_id/children/parts_id`,全部 `#[serde(default)]`)。

- [ ] **Step 4: 跑绿** — `cargo test --manifest-path src-tauri/Cargo.toml transfer_state` 全 PASS

- [ ] **Step 5: 提交** `feat(壳): TransferCard 单卡片状态机(迁移表+终态吸收+cancelling)`

---

### Task 4: 壳层 — AppState 切换状态机表 + 事件单写者接线 + ID 恒定映射

**Files:**
- Modify: `src-tauri/src/main.rs`(AppState+事件桥)
- Modify: `src-tauri/src/commands.rs`(全部 transfer_update/transfer_get_mut/transfer_remove 调用点)
- Test: `src-tauri/src/main.rs` 测试模块新增(表级集成测试)

**Interfaces:**
- Consumes: Task 3 全部;Task 1 `patch_manifest_meta/append_history`
- Produces(Task 5/6/7/8 依赖):

```rust
// AppState 字段替换:
pub transfers: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, TransferCard>>>,
/// 引擎真实 job_id → card_id(事件桥翻译;ID 恒定核心)
pub engine_to_card: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, u64>>>,
/// 下一卡片 ID(独立计数器,从 1 起;不再用 u64::MAX 递减占位)
pub next_card_id: std::sync::atomic::AtomicU64,

impl AppState {
    /// 单写者入口:事件落到卡片(锁内 apply,不跨 await)
    pub async fn card_apply(&self, card_id: u64, ev: transfer_state::CardEvent);
    /// 引擎事件入口:engine_id 翻译后落卡(未登记映射时按 job_id 直查卡片)
    pub async fn engine_event(&self, engine_id: u64, ev: transfer_state::CardEvent);
    /// 登记 engine_id→card_id(创建路径拿到真实 ID 后调用)
    pub async fn bind_engine_id(&self, engine_id: u64, card_id: u64);
    /// 新建卡片(pending),返回 card_id
    pub async fn card_create(&self, dto: TransferDto) -> u64;
    /// 快照:活动+历史 DTO 列表(removed 过滤;4Hz 泵与 list_transfers 用)
    pub async fn snapshot_dtos(&self) -> Vec<TransferDto>;
    /// card→engine 解析(控制命令用;None=无引擎任务)
    pub async fn engine_id_of(&self, card_id: u64) -> Option<u64>;
}
```

**改造要点(implementer 按此执行,逐点核对):**

1. **TransferDto 扩 4 字段**(main.rs struct 定义处,全部 `#[serde(default)]`):
```rust
    #[serde(default)]
    pub queue_pos: Option<u32>,      // 排队位次(None=不在队列)
    #[serde(default)]
    pub batch_id: Option<String>,    // 所属批次(card_id hex;子项填,父卡片自身不填)
    #[serde(default)]
    pub children: Vec<ChildDto>,     // 子文件明细(父卡片填;单文件空)
    #[serde(default)]
    pub parts_id: Option<String>,    // parts 目录名(engine_id hex;续传/删parts用;None=同 job_id)
```
```rust
#[derive(serde::Serialize, serde::Deserialize, Clone, Debug, Default)]
pub struct ChildDto {
    pub job_id: String,   // engine job_id hex(子项控制用)
    pub name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,    // active/done/failed/pending
}
```
**全仓补齐字面量**:所有 `TransferDto { ... }` 构造点(约 15 处:source 桥/main.rs 事件处理/commands.rs/测试)补 `queue_pos: None, batch_id: None, children: vec![], parts_id: None`。编译器逐一指出,不许漏。

2. **删除旧三件套**:`transfer_update/transfer_remove/transfer_get_mut` 删除,调用点全部改走 `card_apply/engine_event/card_create`。典型映射:
   - source 桥 `PE::SourceStarted` → 若 `engine_to_card` 有映射(推送占位已建卡):`engine_event(id, Started)`;**无映射才 `card_create`**(被取方被动任务,现状"无条件插行"保留为首次建卡)。
   - `PE::SourceChunkDone` → `engine_event(id, Progress{done 增量累加…})`——**增量累加逻辑移入 card_apply 调用方**:先读卡片 done,再发 `Progress{done: old+bytes}`(锁内一次完成,不再 get→改→insert 两段)。
   - `PE::SourceSpeed` → `engine_event(id, Progress{done 不变, speed/remote_done/health 更新})`。
   - `PE::SourceDone/SourceFailed` → `engine_event(id, Finished/Failed{reason})`;Failed 时 reason 进日志只打前 8 位指纹无关部分(reason 本身是文案,安全)。
   - main.rs 接收侧事件(TransferStarted/Progress/Paused/... 现有 handle_* 分支)同样逐分支改 `card_apply`。
   - **迁移历史**:card_apply 内检测 `last_transition()` 有值且卡片有 parts(pull 方向)→ `tokio::spawn_blocking` 追加 `append_history`(失败仅 debug 日志,不阻塞)。

3. **ID 恒定(占位消除)**:
   - 所有占位创建点(推送 offer 占位/下载占位/文件夹聚合占位)改 `card_create`(next_card_id 从 1 递增)。
   - 拿到引擎真实 job_id 后 `bind_engine_id(engine_id, card_id)`(推送=SourceStarted 首次到达时按 offer 关联;下载=start_pull 返回 job_id 处)。**删除 handle_push_progress_event 的"删占位插实件"逻辑**(L591 附近)与 start_download 的替换逻辑——改为绑定+字段更新。
   - `dto.parts_id = Some(format!("{:016x}", engine_id))` 在绑定时写入。
   - 控制命令(pause/resume/cancel/throttle/resume_pending/has_parts/remove_transfer)入口统一先 `engine_id_of(card_id)` 翻译再查引擎/parts 目录;前端传的 job_id 一律是 card_id。

4. **4Hz 泵/持久化泵改读** `snapshot_dtos()`(锁内 clone,不持锁序列化)。

- [ ] **Step 1: 写失败测试**(main.rs tests;AppState 构造参照现有测试用法)

```rust
    #[tokio::test]
    async fn card_lifecycle_single_writer_no_replace() {
        let st = test_app_state().await; // 测试辅助:现有测试若无可参照 commands::tests 的构造方式新建
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
```
(若无现成 `test_app_state` 辅助,在本测试模块新建:构造 AppState 需要的字段中 transfers/engine_to_card/next_card_id 真实化,其余字段用 Default/空值——参照 AppState 定义逐字段给 `Arc::new(Mutex::new(...))` 空值。)

- [ ] **Step 2: 跑红** → **Step 3: 按改造要点 1-4 实现** → **Step 4: 跑绿+全量回归**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: 全 PASS(既有测试中直接操作 transfers 表的需同步适配为 card API——适配测试不算改坏,语义等价即可)

- [ ] **Step 5: 提交** `feat(壳): transfers 表切换 TransferCard 状态机+ID恒定映射(消占位替换竞态)`

---

### Task 5: 壳层 — 删除仲裁 + 两级删除命令

**Files:**
- Modify: `src-tauri/src/commands.rs`(remove_transfer/clear_completed_transfers/has_parts)
- Modify: `src-tauri/src/main.rs`(cancelling 5s 看门狗 spawn)
- Test: commands.rs tests 新增

**Interfaces:**
- Consumes: Task 4 `card_apply/engine_id_of`;Task 1 `patch_manifest_meta`
- Produces:
  - `remove_transfer(state, job_id: String, level: String) -> Result<bool, String>` —— `level: "view" | "destroy"`(** breaking:UI Task 9 同步改**;`delete_parts: bool` 参数删除)
  - 内部函数 `pub async fn destroy_transfer(state: &State<'_, AppState>, card_id: u64) -> Result<(), String>`
  - 看门狗:`AppState` 加 `cancel_watchdogs: Arc<tokio::sync::Mutex<std::collections::HashMap<u64, tokio::task::JoinHandle<()>>>>`(防重复挂表)

**行为规格:**

- `level="view"`(移除视图):卡片 `removed=true` + 表保留(历史记录可见);transfers.json 索引该条**保留**(带 removed 标记)——持久化结构改为序列化 `TransferCard`(dto+removed,engine_id 不存盘,启动重绑)。manifest+parts 原样。
- `level="destroy"`(彻底删除):
  - 终态卡:直接删 manifest+parts 目录(按 parts_id)+ 表 remove + 索引消失。
  - 活动卡(pending/active/paused):`card_apply(CancelRequested)` → 对 engine_id 发既有取消路径(transfer_action cancel / placeholder_cancels / sender_jobs 清理,复用 commands.rs 现有 cancel 分支逻辑)→ 卡片进入 cancelling;**引擎终态事件到达(Task 4 已接线)或 5s 看门狗超时** → 执行 destroy 实删。
  - 看门狗 spawn:`card_apply(CancelRequested)` 后 `tokio::spawn(sleep(5s) → 若卡片仍 cancelling → destroy实删)`,句柄入 cancel_watchdogs,确认先到则 abort。
- `clear_completed_transfers`:仅对终态卡调 `remove_transfer(level="view")`(spec §2.3:"清除已完成"=移除视图),逐条走状态机路径不绕过(spec §1.3)。
- `has_parts`:按 `engine_id_of` 翻译后查 parts 目录(逻辑不变,入口加翻译)。
- **续传键修正**:`resume_pending` 命令接收 card_id→翻译 engine_id→走现有 pending_jobs 编排(pending_jobs 返回 engine_id)。

- [ ] **Step 1: 写失败测试**(commands.rs tests;测试构造参照 Task 4 test_app_state)

```rust
    #[tokio::test]
    async fn view_remove_keeps_card_and_parts() {
        // 终态卡 view 删除:removed=true,表里还在,parts 不动
        let st = test_app_state().await;
        let card_id = st.card_create(done_dto("done")).await;
        let ok = remove_transfer_inner(&st, card_id, "view").await.unwrap();
        assert!(ok);
        let card = st.card_get(card_id).await.unwrap();
        assert!(card.removed);
        assert_eq!(st.snapshot_dtos().await.len(), 0, "活动列表消失");
    }

    #[tokio::test]
    async fn destroy_active_goes_through_cancelling() {
        let st = test_app_state().await;
        let card_id = st.card_create(active_dto()).await;
        st.bind_engine_id(0x77, card_id).await;
        // destroy 活动卡:立即变 cancelling,不直接消失
        destroy_transfer(&st, card_id).await.unwrap();
        let card = st.card_get(card_id).await.unwrap();
        assert_eq!(card.dto.state, "cancelling");
        // 引擎终态到达 → 实删
        st.engine_event(0x77, transfer_state::CardEvent::Interrupted, ).await;
        tokio::time::sleep(Duration::from_millis(100)).await; // destroy 异步收尾
        assert!(st.card_get(card_id).await.is_none(), "确认后删除");
    }

    #[tokio::test]
    async fn clear_completed_is_view_level() {
        let st = test_app_state().await;
        st.card_create(done_dto("done")).await;
        st.card_create(done_dto("failed")).await;
        st.card_create(active_dto()).await;
        let n = clear_completed_inner(&st).await.unwrap();
        assert_eq!(n, 2, "只清终态");
        assert_eq!(st.snapshot_dtos().await.len(), 1);
    }
```
(inner 辅助函数抽出来供测试直调,`#[tauri::command]` 包装层不进测试。done_dto/active_dto 为构造辅助。`card_get` 若 Task 4 未提供则本任务补:`pub async fn card_get(&self, id: u64) -> Option<TransferCard>`。)

- [ ] **Step 2: 跑红** → **Step 3: 实现**(行为规格逐条;引擎取消复用现有 cancel 分支,不重写协议) → **Step 4: 跑绿+回归** `cargo test --manifest-path src-tauri/Cargo.toml`

- [ ] **Step 5: 提交** `feat(壳): 删除仲裁( cancelling→引擎确认/5s看门狗)+两级删除`

---

### Task 6: 壳层 — 启动重建(manifest 优先)+ 持久化改造 + 历史记录命令

**Files:**
- Modify: `src-tauri/src/main.rs`(load_transfers_history/migrate_on_startup/持久化泵)
- Modify: `src-tauri/src/commands.rs`(新命令)
- Test: main.rs tests + commands.rs tests

**Interfaces:**
- Consumes: Task 2 `orphan_jobs`;Task 1 `TransferMeta/patch_manifest_meta`
- Produces:
  - `list_disk_jobs(state) -> Result<Vec<DiskJobDto>, String>`(历史记录入口:全部磁盘 manifest 含已移除视图的)
  - `restore_disk_job(state, job_id: String) -> Result<(), String>`(恢复视图:removed=false)
  - `destroy_disk_job(state, job_id: String) -> Result<(), String>`(按 engine job_id 彻底删;用于孤儿)
  - `pub struct DiskJobDto { pub job_id: String, pub display_name: String, pub total: u64, pub done: u64, pub state: String, pub direction: String, pub peer_hex: String, pub created_at_ms: Option<i64>, pub removed_from_view: bool }`
  - 注册进 `tauri::generate_handler![...]`。

**行为规格:**

1. **启动重建顺序**(load_transfers_history 重写):
   - `orphan_jobs(download_dir)` → 每个孤儿一张卡:state=interrupted(缺块)或 failed(位图全真,标注 fail_reason="数据完整性存疑,建议重新拉取");meta 有 display_name 用之,无则 manifest.file_name;parts_id=engine_id hex;**卡片 job_id=engine_id**(启动重建的卡直接用引擎 ID 作 card_id,免映射——只对磁盘孤儿这样,next_card_id 起点设为 max(孤儿ID)+1 防撞)。
   - transfers.json(现持久化)中的终态记录(done/failed)与 push 方向 interrupted(无 parts):直接建卡(此为唯一记录源)。
   - 冲突(两边都有同 engine_id):manifest 侧为准(状态/进度),索引侧的 display_name 等元数据字段补缺。
   - `migrate_on_startup` 逻辑并入(非终态→interrupted 盖戳),孤儿丢弃逻辑删除(孤儿=历史记录可见,不再丢)。
2. **持久化泵改造**:序列化对象改为 `Vec<TransferCardSerde>{dto, removed}`(新增透明结构,字段 `#[serde(default)]`);损坏时**从磁盘孤儿全量重建**(spec §2.1:可丢弃缓存)——加载路径已天然如此(先扫盘)。
3. **卡片创建时写 meta**:pull 方向拿到 engine_id 绑定后,`spawn_blocking(patch_manifest_meta)` 填 direction/local_role/peer_hex/display_name/created_at(source_path 有则填)。终态时(Finished/Failed/Interrupted 落卡后)再 patch finished_at_ms/fail_reason。push 方向无 parts,跳过。

- [ ] **Step 1: 写失败测试**

```rust
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
```
(`rebuild_cards_from_disk(parts_root_or_data_dir) -> Vec<TransferCard>` 抽为 pub 纯函数便于测试;list_disk_jobs/restore/destroy 的测试用 test_app_state 走命令级断言,模式同 Task 5。)

- [ ] **Step 2: 跑红** → **Step 3: 实现** → **Step 4: 跑绿+回归+手动冒烟**:`cargo run` 一次,确认旧 transfers.json+parts 目录启动后卡片完整回归(名字/状态/续传按钮在)。

- [ ] **Step 5: 提交** `feat(壳): manifest 优先启动重建+两级删除持久化+历史记录命令`

---

### Task 7: 壳层 — 全局并发闸门 + 队列调度

**Files:**
- Modify: `src-tauri/src/main.rs`(AppState:xfer_lock 改造)
- Modify: `src-tauri/src/commands.rs`(创建任务排队点)
- Modify: `src-tauri/src/main.rs`(ConfigDto/store Config 若无字段则加 `max_active_transfers: u32` default 3)
- Test: commands.rs/main.rs tests

**Interfaces:**
- Produces:
  - AppState:`pub peer_locks: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Arc<tokio::sync::Mutex<()>>>>>`(替换全局 `xfer_lock`——**同对端串行维持,跨对端放开**)
  - AppState:`pub active_gate: Arc<tokio::sync::Semaphore>`(permits=max_active_transfers,启动按配置构建)
  - `pub async fn acquire_slot(&self, peer_hex: &str) -> (tokio::sync::OwnedSemaphorePermit, Arc<tokio::sync::Mutex<()>>)`(先排队拿全局 permit,再拿对端锁;返回给传输编排,Drop 自动释放)
  - 排队位次:卡片 `dto.queue_pos` 在等待 permit 期间周期更新(1s tick 重新计算 waiting 序=按 card_create 时间排序的名次);启动后置 None。**queue_pos 只存 dto,TransferCard 顶层不再有同名字段**(Task 3 的 `TransferCard.queue_pos` 定义取消,统一走 `dto.queue_pos`,避免双源)。

**行为规格:**

- 创建任务(下载/推送/续传)流程改为:建卡(pending)→ `acquire_slot` 等待 → `card_apply(Started)` → 走现有编排(start_pull/push_files)→ 释放。
- 等待期间:queue_pos 显示"排队中·第 N 位";pending 60s 超时逻辑维持(QueueTimeout→failed)。
- 释放触发出队:Semaphore 天然唤醒下一个等待者(FIFO),无需显式队列结构——位次计算=同 peer 等待者中按创建时间排序。
- `max_active_transfers` 进 Config(store.rs `#[serde(default = "default_max_active")]`,fn 返回 3;范围钳制 1-8)+ ConfigDto 透传 + set_settings 持久化(设置页 UI 在 Task 12)。
- **xfer_lock 所有现有持有点**(commands.rs 搜索 `xfer_lock`)改 `acquire_slot(peer)`:下载/推送/续传各入口,锁 guard 生命周期不变。

- [ ] **Step 1: 写失败测试**

```rust
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
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "第 4 个在排队");
        // 释放一个 → 等待者获得
        drop(permits.pop());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(waiter.is_finished(), "出队获得槽位");
    }

    #[tokio::test]
    async fn different_peers_do_not_block_each_other() {
        let st = test_app_state().await; // permits=3
        let _a = st.acquire_slot("aa").await;
        // gate 未满:不同对端立即获得,不互相等待
        let b = tokio::time::timeout(Duration::from_millis(200),
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
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "同对端应串行等待");
        // 释放第一个后获得
        drop(_a);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(waiter.is_finished());
    }
```

- [ ] **Step 2: 跑红** → **Step 3: 实现** → **Step 4: 跑绿+回归**(重点回归:既有多任务 E2E、"通道被占用"相关测试若断言全局串行需按新语义适配——同对端仍串行,断言应仍通过)

- [ ] **Step 5: 提交** `feat(壳): 全局并发闸门+排队位次(同对端串行维持,跨对端放开)`

---

### Task 8: 壳层+协议编排 — 多文件统一父卡片(推送不裂变/拉取聚合对齐/子项重试)

**Files:**
- Modify: `src-tauri/src/main.rs`(source 桥 SourceStarted 分支/接收侧 Started 分支)
- Modify: `src-tauri/src/commands.rs`(推送编排:批次卡创建+子项挂接)
- Test: commands.rs tests

**Interfaces:**
- Consumes: Task 4 `children: Vec<ChildDto>/batch_id`
- Produces:
  - 推送多文件:一张父卡(card_id=批次 ID,创建时定,name="N 个文件"或首文件名+"等 N 项");每个子文件引擎 Started 事件到达时**不再新建卡**,而是 `child_upsert(parent_card_id, ChildDto)`(AppState 新方法,锁内更新 children 数组);
  - 子文件终态:children[i].state 更新(active→done/failed);**全部子项终态→父卡 Finished/Failed**(有失败子项→failed+fail_reason="N 项失败";全成功→done)。
  - 拉取文件夹:现状聚合卡(start_download_dir)保留为父卡,`children` 从 DirPullEvent(FileDone 逐文件)累积填充(每文件一项,engine job_id 记录以便单文件重试)。
  - 子项单独重试:`retry_child(state, parent_card_id: String, child_job_id: String)` 命令——失败子项按 lastRequest 参数(父卡记录)重发单文件;子项新引擎事件挂回父卡 children 替换。
  - 父卡整批控制:pause/cancel 传播——父卡 CancelRequested 时对全部活动子 engine_id 逐个走取消路径(复用 Task 5 destroy_transfer 逻辑)。

- [ ] **Step 1: 写失败测试**

```rust
    #[tokio::test]
    async fn push_batch_stays_one_card_children_upsert() {
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("3 个文件")).await; // push 批次占位
        // 引擎子文件1 Started:不裂变,挂 child
        st.child_upsert(parent, ChildDto {
            job_id: format!("{:x}", 0x101), name: "a.txt".into(),
            total: 10, done: 0, state: "active".into(),
        }).await;
        st.child_upsert(parent, ChildDto {
            job_id: format!("{:x}", 0x102), name: "b.txt".into(),
            total: 20, done: 20, state: "done".into(),
        }).await;
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap.len(), 1, "恒一张父卡");
        assert_eq!(snap[0].children.len(), 2);
        // 子1完成 → 全终态 → 父 done
        st.child_upsert(parent, ChildDto {
            job_id: format!("{:x}", 0x101), name: "a.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        assert_eq!(st.card_get(parent).await.unwrap().dto.state, "done");
    }

    #[tokio::test]
    async fn parent_failed_when_any_child_failed() {
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("2 个文件")).await;
        st.child_upsert(parent, ChildDto {
            job_id: format!("{:x}", 0x201), name: "bad.txt".into(),
            total: 10, done: 0, state: "failed".into(),
        }).await;
        st.child_upsert(parent, ChildDto {
            job_id: format!("{:x}", 0x202), name: "good.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.state, "failed");
        assert!(card.dto.fail_reason.as_deref().unwrap().contains("1 项"), "reason 应含失败数");
    }
```
(`recheck_parent_terminal(card_id)`:子项全终态时计算父终态——pub async fn,AppState 方法。)

- [ ] **Step 2: 跑红** → **Step 3: 实现**(source 桥 SourceStarted 分支改挂 child:推送方向 engine 事件若 job 的 offer 对应批次卡→child_upsert,判定方式=engine_to_card 里 offer 根 job 已绑父卡;子文件 job 与父的关联通过推送编排时预登记 `pending_children: HashMap<engine_id, parent_card_id>`(AppState 新字段)) → **Step 4: 跑绿+回归**

- [ ] **Step 5: 提交** `feat(壳): 多文件统一父卡片+子项挂接+整批控制+子项重试`

---

### Task 9: UI — types/api/store 对接新字段与命令

**Files:**
- Modify: `ui/src/types.ts`
- Modify: `ui/src/api.ts`
- Modify: `ui/src/stores/transfers.ts`
- Test: `ui/src/stores/__tests__/transfers.test.ts`(新建;若无此目录参照现有测试位置 `ui/src/**/__tests__` 或 `*.test.ts` 就近)

**Interfaces:**
- Consumes: Task 4/5/6/8 的 DTO 与命令(level 参数/DiskJobDto/children/queue_pos)
- Produces:

```ts
// types.ts 追加
export interface ChildDto {
  job_id: string
  name: string
  total: number
  done: number
  state: string
}
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
// TransferDto 追加(queue_pos?: number | null; batch_id?: string | null;
//   children?: ChildDto[]; parts_id?: string | null; remote_done?: number; instant?: boolean)
```
```ts
// api.ts transfersApi 改造
removeTransfer: (job_id: string, level: 'view' | 'destroy'): Promise<boolean> =>
  invokeCommand('remove_transfer', { jobId: job_id, level }),
listDiskJobs: (): Promise<DiskJobDto[]> => invokeCommand('list_disk_jobs'),
restoreDiskJob: (job_id: string): Promise<void> =>
  invokeCommand('restore_disk_job', { jobId: job_id }),
destroyDiskJob: (job_id: string): Promise<void> =>
  invokeCommand('destroy_disk_job', { jobId: job_id }),
retryChild: (parent_id: string, child_job_id: string): Promise<void> =>
  invokeCommand('retry_child', { parentCardId: parent_id, childJobId: child_job_id }),
```
store 改造:`removeTransfer(job_id, level)` 透传;新增 `diskJobs` 状态+`refreshDiskJobs()`(打开历史区时拉取)+`restoreDiskJob/destroyDiskJob` 包装(操作后 refreshDiskJobs);乐观更新块 `transfers.value.filter` 保留(view 级别由 4Hz 事件自然收敛,乐观过滤仅 destroy)。

- [ ] **Step 1: 写失败测试**(Vitest;store 单测:mock api 模块——参照现有 store 测试的 mock 手法,若项目无先例则用 `vi.mock('../api')`)

```ts
import { describe, it, expect, vi, beforeEach } from 'vitest'

vi.mock('../api', () => ({
  api: {
    transfers: {
      list: vi.fn().mockResolvedValue([]),
      clearCompleted: vi.fn().mockResolvedValue(0),
      removeTransfer: vi.fn().mockResolvedValue(true),
      listDiskJobs: vi.fn().mockResolvedValue([
        { job_id: '3', display_name: '老任务', total: 5, done: 2,
          state: 'interrupted', direction: 'pull', peer_hex: 'aabb',
          created_at_ms: 1, removed_from_view: true },
      ]),
      restoreDiskJob: vi.fn().mockResolvedValue(undefined),
      destroyDiskJob: vi.fn().mockResolvedValue(undefined),
      pendingResumeJobs: vi.fn().mockResolvedValue([]),
      resumePending: vi.fn(),
      transferThrottle: vi.fn(),
    },
    browse: { transferAction: vi.fn(), startDownload: vi.fn(), pushFiles: vi.fn(), pushFilesRel: vi.fn() },
  },
  onTransferProgress: vi.fn(() => () => {}),
  friendlyError: vi.fn((s: string) => s),
}))

import { useTransfersStore } from '../transfers'

describe('transfers store 新接口', () => {
  beforeEach(() => { vi.clearAllMocks() })

  it('removeTransfer 传 level 而非 deleteParts', async () => {
    const st = useTransfersStore()
    st.transfers = [{ job_id: '9', name: 'x', total: 1, done: 0, state: 'done',
      speed_bps: 0, peer: 'a', direction: 'pull', local_role: 'destination',
      health: null, started_at_ms: 1, finished_at_ms: 2 } as any]
    await st.removeTransfer('9', 'view')
    const { api } = await import('../api')
    expect(api.transfers.removeTransfer).toHaveBeenCalledWith('9', 'view')
  })

  it('refreshDiskJobs 拉取并缓存磁盘历史', async () => {
    const st = useTransfersStore()
    await st.refreshDiskJobs()
    expect(st.diskJobs.length).toBe(1)
    expect(st.diskJobs[0].removed_from_view).toBe(true)
  })
})
```

- [ ] **Step 2: 跑红** — `cd ui && npx vitest run` → **Step 3: 实现** → **Step 4: 跑绿**(`npx vitest run` 全量+`npx vue-tsc --noEmit` 若项目在用)

- [ ] **Step 5: 提交** `feat(ui): 对接两级删除/历史记录/父子卡片新接口`

---

### Task 10: UI — TransferItem 父卡片子展开+双进度+别名+排队位次

**Files:**
- Modify: `ui/src/components/TransferItem.vue`
- Test: `ui/src/components/__tests__/TransferItem.test.ts`(新建,挂载测试用 @vue/test-utils——若项目未装该依赖,改为对纯计算函数的单元测试:把新逻辑抽到 `ui/src/lib/transferDisplay.ts` 导出纯函数测之,**不新增依赖**)

**Interfaces:**
- Consumes: Task 9 types
- Produces: `ui/src/lib/transferDisplay.ts` 纯函数(测试载体):

```ts
export interface PeerNameSource { fingerprint: string; alias: string; name: string }[]
/** D9:别名>广播名;无信任记录回退指纹缩写 aa9988..ff12 */
export function peerDisplayName(peerHex: string, peers: PeerNameSource): string
/** D11:发送方主进度=remote_done;接收方=done。返回 {mainDone, sentDone, backlog} */
export function progressPair(t: { local_role?: string; done: number; remote_done?: number; total: number }):
  { mainDone: number; sentDone: number; backlogPct: number }
/** backlog>20% 提示积压;满格未确认→"等待确认"态 */
export function senderDisplayState(t: { done: number; remote_done?: number; total: number; state: string }):
  'normal' | 'backlog' | 'awaiting-confirm'
/** 排队文案:"排队中 · 第 N 位" */
export function queueText(queuePos: number | null | undefined, direction: string): string
```

**模板改造(TransferItem.vue):**

1. **别名**:peerName computed 改用 `peerDisplayName(transfer.peer, settingsStore.trustedPeers)`(import useSettingsStore;trustedPeers 已有 alias/name/fingerprint 字段)。指纹缩写移入 title tooltip。
2. **双进度(D11)**:local_role=source-push 时:进度条 width 按 `progressPair().mainDone`(=remote_done);进度数字主显示 `mainDone/total`;`sentDone>mainDone` 时辅助小字"已发 X";`senderDisplayState()=='backlog'` 时显示黄色"网络积压"角标;`=='awaiting-confirm'` 时进度条加 `.awaiting` 类(条纹动画,已有 CSS 体系的降饱和处理:repeating-linear-gradient)。
3. **排队位次**:state=pending 且 queue_pos≠null 时 pendingHint 用 `queueText()`(替代现有固定文案;无 queue_pos 回退现文案)。
4. **父卡片子展开**:transfer.children 非空时卡片底部展开区(`v-if="expanded"`,卡片头部加 ▸/▾ 切换钮):每子项一行(名/大小/状态徽章/进度细条);失败子项行尾"重试"按钮→`transfersStore.retryChild(parentId, child.job_id)`。父卡操作矩阵不变(整批控制)。
5. **删除两级**:handleRemove 改造——终态:弹选择"移除(保留磁盘数据,可在历史记录找回)/彻底删除(不可恢复)"(用现有 confirmDialog 两次问或自绘小菜单,取简:confirmDialog 文案含两选项说明,彻底删除二次确认);活动:直接 destroy 流程(取消+彻底删,复用现有确认文案)。

- [ ] **Step 1: 写失败测试**(transferDisplay.test.ts——纯函数四组用例,逐函数正反例,内容从上面 docstring 推导:如 `progressPair({local_role:'source-push',done:100,remote_done:60,total:100})` → `{mainDone:60,sentDone:100,backlogPct:40}`;接收方向 mainDone=done;`senderDisplayState` 满格 done=total 但 remote_done<total→'awaiting-confirm' 等)

- [ ] **Step 2: 跑红** → **Step 3: 实现纯函数+模板改造** → **Step 4: 跑绿+`npx vitest run`**

- [ ] **Step 5: 提交** `feat(ui): 父卡片子展开+双进度+别名+排队位次`

---

### Task 11: UI — 传输页活动/历史分区 + 历史记录入口 + 删刷新按钮

**Files:**
- Modify: `ui/src/pages/Transfers.vue`
- Test: `ui/src/lib/transferDisplay.ts` 加分区纯函数+测试

**Interfaces:**
- Consumes: Task 9/10
- Produces: `splitActiveHistory(jobs: TransferDto[]): { active: TransferDto[]; history: TransferDto[] }`(active=active/pending/paused/cancelling;history=done/failed/interrupted;active 内排序:active>cancelling>pending(按 queue_pos)>paused;history 按 finished_at_ms 降序)

**页面改造:**

1. **删刷新按钮**(header 的"刷新"按钮+handleRefresh 移除;onMounted 仅初始 refreshTransfers 一次——事件驱动已覆盖,spec 砍掉项)。"清除已完成/失败"保留(语义=移除视图)。
2. **分区**:上半"进行中"(active 列表,空时显示现有空态文案);下半"历史"(默认折叠,`v-if="historyExpanded"` 切换,标头显示历史条数);两区各自 v-for TransferItem。
3. **历史记录入口**:header 加"历史记录"按钮 → 抽屉/展开区(取简:内嵌第三区"磁盘历史(含已移除)"),onOpen 时 `refreshDiskJobs()`;每行:display_name/状态/大小/时间+两个操作:"恢复到列表"(restoreDiskJob:removed 卡片回活动视图——后端 removed=false)+“彻底删除”(destroyDiskJob,confirmDialog 确认);DiskJob 行不渲染 TransferItem(轻量行即可)。
4. **样式**:复用既有 token(var(--gray-*)/var(--space-*));历史区折叠按钮样式参照现有 btn-refresh 类改。

- [ ] **Step 1: 写失败测试**(splitActiveHistory 用例:混合状态分组/排序正确/空输入)
- [ ] **Step 2: 跑红** → **Step 3: 实现** → **Step 4: 跑绿+手测**(`npm run dev` 打开传输页核对三区渲染)
- [ ] **Step 5: 提交** `feat(ui): 传输页活动/历史分区+磁盘历史入口+删刷新按钮`

---

### Task 12: 收尾 — 完成 toast 全覆盖 + 设置页并发配置 + 全量回归 + 版本

**Files:**
- Modify: `src-tauri/src/main.rs`(完成事件 toast:接收侧已有则核对的发送侧 SourceDone 分支——终态落卡且非 cancelling 时 emit toast "推送完成: {name}";拉取侧 Finished 同理"下载完成: {name}";**仅活动→终态的边触发**,历史恢复/启动重建不触发——判定:card_apply 前状态非终态且事件为 Finished)
- Modify: `ui/src/pages/Settings.vue`(连接安全卡加"并发任务数"数字输入 1-8,保存走 set_settings 的 max_active_transfers)
- Modify: `ui/src/types.ts`(ConfigDto 加 `max_active_transfers?: number`)
- Modify: `src-tauri/src/main.rs` ConfigDto + `crates/localtrans-core/src/store.rs` Config(若 Task 7 未含)
- Modify: `CHANGELOG.md` + 版本号(`src-tauri/tauri.conf.json` version、`Cargo.toml` 三处 workspace 版本)→ **0.12.0**
- Test: 回归命令

**验收核对(spec §9 全 9 条,逐条手测+记录):**

1. 传输中删除任务:卡片先变 cancelling(显示"取消中"),引擎停止后消失;无幽灵行。
2. 强杀进程(taskkill)后重启:任务带完整元数据(名字/对端/角色/时间)回归 interrupted,续传可用。
3. 清除已完成→历史记录入口可见磁盘 manifest,可恢复视图或彻底删除;彻底删除后 parts 目录消失(资源管理器核对)。
4. 多文件推送(拖文件夹到设备卡):全程一张父卡片,展开见子文件;整批取消有效。
5. 发送方进度:主数字与对端屏幕一致(remote_done);人为限速对端制造积压→提示出现;满格未确认→"等待确认"态。
6. 并发:设置并发 3,同时发起 4 个不同对端任务→第 4 个排队显示位次;队首完成自动出队。(单对端场景:同对端仍串行——验证第 2 个显示排队)
7. 手动删 parts 目录后重启:对应卡片显示 failed("数据丢失/完整性存疑")而非可续传。
8. 列表:默认活动优先;历史折叠;对端显示别名(设备页起过别名后)。
9. 拉取+推送完成均有 toast;切到其他页也能收到(全局 toast 体系)。

- [ ] **Step 1: 设置页写失败测试**(若有设置 store 测试先例则加 max_active_transfers 保存用例;无则跳过测试直改——设置页为纯表单接线)
- [ ] **Step 2: 实现 toast/设置页/版本** → **Step 3: 全量回归**:
  - `cargo test`(workspace 全部)
  - `cargo build --release --manifest-path src-tauri/Cargo.toml`(编译通过)
  - `cd ui && npx vitest run && npm run build`
- [ ] **Step 4: 手测验收清单** 逐条记录结果(通过/不适用+原因)写入提交信息或 CHANGELOG
- [ ] **Step 5: 提交** `feat: v0.12.0 传输域重构收尾(并发配置+完成通知+版本)`

---

## 执行顺序与依赖

```
T1 → T2 (core 地基,先行)
T3 → T4 → T5 → T6 (壳层状态机链,严格串行)
T7 (并发,依赖 T4 的卡片表;可与 T6 并行审但实现串行)
T8 (父子,依赖 T4/T5)
T9 → T10 → T11 (UI 链,依赖壳层全部)
T12 (收尾)
```

## 风险与回退

- **最大风险 T4**(调用面广):若全量接线后既有 E2E 失败,允许临时注释掉非核心路径的接线(逐个 card_apply 化)分两提交,但终态必须全部走状态机。
- T7 的 xfer_lock 改造若引发"通道被占用"回归,回退方案:保留全局锁,仅加 queue_pos 显示(并发上限退化为 1)——**需用户裁定**,不自行降级。
- 数据迁移不可逆点:T6 启动重建(读旧 transfers.json+manifest)。提交前必须手测旧数据目录启动一次。transfers.json 结构升级后旧版本读不回——**提交信息中注明升级不可回退**(v0.12.0 单向)。
