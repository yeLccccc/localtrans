# 跨端取消通知与 source 任务清理 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 取消/失败的传输在数据源侧也落终态——source 行不再永久 active(计时不停、进度泵不降频)。

**Architecture:** 两个机制:①复用既有 `TransferCtl{job_id, Cancel}` 网络消息,下载方取消时发给数据源,其路由器清理 sender 任务并发 `SourceFailed`;②`SenderJobState` 记录关联 `offer_id`,`push_files_rel` 失败返回前按 offer_id 反查清理关联 sender 任务。sender_jobs 注册表以参数穿透(非全局 static,保持测试隔离)。

**Tech Stack:** Rust(tokio/quinn),cargo workspace,core crate 为主 + Tauri 壳层一行接线。

**对 spec 的两处实现级修订**(spec: docs/superpowers/specs/2026-08-23-job-cancel-notify-design.md):
1. **不新增 JobCancel 消息**——复用既有 `TransferCtl`(session.rs 已把它归入 inbound_ctrl 转发类;engine.rs 路由器已有其分支,Throttle 已按 sender 侧处理,Cancel 对称扩展)。零协议面增量。
2. **sender_jobs 用参数穿透而非进程级 static**——spawn_rpc_router 的 map 由调用方创建(壳层 AppState/ffi/tests 各持一份),清理必须操作同一 map;全局 static 会破坏多实例测试隔离。
3. 测试计划对应调整:原 T5(新消息序列化)取消(无新消息);原 T4(暂停回归)并入 Task 1 的守护注释——探针自杀路径明确不动。

## Global Constraints

- 提交信息中文,前缀 `feat:`/`fix:`/`chore:`/`docs:`;代码注释中文,风格与周边一致
- TDD:每个任务先写测试跑红,再实现跑绿,再提交
- 新增 tracing 日志只打 job_id/offer_id 与数量,**不打文件名/路径**
- 协议两端同版本升级,无混跑;不引入版本协商
- 命令行是 Windows Git Bash;仓库根 `C:/Users/<user>/Desktop/work/localTrans`,路径用正斜杠
- 测试基线:core 118 通过;壳层(localtrans)19 通过;**存量失败与本项目无关,不要去修**:vitest 3 个 TransferItem 文案失败(历史遗留);ffi `start_emits_devices_and_settings_roundtrip` 端口占用失败(本机运行实例占端口,环境问题)
- 探针 60s 空闲自杀**保持只停探针、不清任务表**(长暂停后 RecvAck 仍需找到任务发 SourceDone;清理注册表会破坏暂停/恢复)
- YAGNI:不动 ffi(安卓壳)、不动 UI、不加版本号

---

### Task 1: 路由器处理对端取消(TransferCtl Cancel → sender 清理)

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(TransferCtl 分支,~1884 行;测试加在 `push_large_file_recv_ack_completes_source_side` 之后)
- Test: 同文件 `mod tests`

**Interfaces:**
- Consumes: 既有 `spawn_rpc_router(sm, ctx, reg, ctrl_rx, ask_tx, sender_jobs, source_event_tx)`(7 参);既有 `fire_probe_stop(&SenderJobState)`;`ProgressEvent::SourceFailed { job_id, reason }`
- Produces: 路由器行为——`TransferCtl{Cancel}` 命中 sender_jobs 时:remove + fire_probe_stop + 发 `SourceFailed{reason:"对方已取消"}`;未命中幂等跳过。后续任务依赖此语义。

- [ ] **Step 1: 写失败测试**

在 engine.rs 测试模块(`push_large_file_recv_ack_completes_source_side` 测试之后)加入:

```rust
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
            sender_jobs_b.clone(), Some(source_tx));

        // 甲连接乙,发 MetaReq 让乙建 sender 任务
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("连接应成功").unwrap();

        let mut resp_rx = sm_a.take_inbound_resp_rx().await.expect("甲响应通道未被占用");
        sm_a.send_ctrl(&fp_b, ControlMsg::MetaReq {
            share_id: "share1".to_string(),
            path: "cancel_test.bin".to_string(),
        }).await.unwrap();

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

        // 等 MetaResp 到甲并归还响应通道(防泄漏影响后续用例)
        timeout(Duration::from_secs(5), async {
            loop {
                match resp_rx.recv().await {
                    Some((_, ControlMsg::MetaResp { .. })) => return,
                    Some(_) => continue,
                    None => panic!("resp 通道不应关闭"),
                }
            }
        }).await.expect("应收到 MetaResp");
        sm_a.return_inbound_resp_rx(resp_rx).await;

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
```

- [ ] **Step 2: 跑红**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib cancel_pull_notifies_source_side -- --nocapture
```
Expected: FAIL——5s 超时收不到 SourceFailed(现状:Cancel 落到 task_controls 分支,查不到任务仅 warn 日志)。

- [ ] **Step 3: 实现(路由器 TransferCtl 分支扩展)**

engine.rs `ControlMsg::TransferCtl { job_id, action }` 分支(~1884 行),在 Throttle 前置分支之后、task_controls 转发之前插入:

```rust
                ControlMsg::TransferCtl { job_id, action } => {
                    // T7: Throttle 作用于 sender 侧任务(源限速),不经接收端控制通道
                    if let crate::protocol::TransferAction::Throttle { max_streams } = action {
                        // ……现有 Throttle 处理保持不变……
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
                            tracing::info!("收到对端取消: job {} 清理 sender 任务", job_id);
                            continue;
                        }
                    }

                    // T15: 查全局任务控制注册表,转发到对应传输任务的控制通道
                    // ……现有 task_controls 转发保持不变……
```

(只插入中间的 `if action == Cancel {...}` 块,现有 Throttle 与 task_controls 代码原样保留。)

- [ ] **Step 4: 跑绿**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib cancel_pull_notifies_source_side -- --nocapture
```
Expected: PASS。

- [ ] **Step 5: 回归**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib
```
Expected: 119 passed(118 + 新 1)。

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(transfer): 路由器处理对端取消——TransferCtl{Cancel} 清理 sender 任务并发 SourceFailed"
```

---

### Task 2: push 失败清理关联 sender 任务(offer_id 关联 + 注册表穿透)

**Files:**
- Modify: `crates/localtrans-core/src/transfer/sender_state.rs`(offer_id 字段)
- Modify: `crates/localtrans-core/src/transfer/engine.rs`(MetaReq 赋值;cleanup_push_senders;push_files/push_files_rel 加参 + 内部重构;全部调用点)
- Modify: `crates/localtrans-relay/tests/e2e.rs`(~1106 行调用点)
- Modify: `src-tauri/src/commands.rs`(~703、~1003 行调用点)
- Modify: `crates/localtrans-ffi/src/lib.rs`(~693、~1427 行调用点)
- Test: engine.rs `mod tests`

**Interfaces:**
- Consumes: Task 1 的路由器行为;`SenderJobMap = Arc<RwLock<HashMap<u64, Arc<SenderJobState>>>>`
- Produces(后续任务/壳层依赖的精确签名):
  - `SenderJobState.offer_id: Option<u64>`(普通 pull 为 None)
  - `pub async fn cleanup_push_senders(sender_jobs: &SenderJobMap, offer_id: u64, reason: &str)`
  - `pub async fn push_files(sm: &SessionManager, peer: &Fingerprint, files: Vec<PathBuf>, sender_jobs: &SenderJobMap, progress: mpsc::Sender<ProgressEvent>) -> Result<u64, EngineError>`
  - `pub async fn push_files_rel(sm: &SessionManager, peer: &Fingerprint, files: Vec<(PathBuf, String)>, sender_jobs: &SenderJobMap, progress: mpsc::Sender<ProgressEvent>) -> Result<u64, EngineError>`

- [ ] **Step 1: 写失败测试**

engine.rs 测试模块(`cancel_pull_notifies_source_side` 之后)加入:

```rust
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
            ctrl_rx_b, ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("甲入站通道未被占用");
        let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, sender_jobs_a.clone(), Some(source_tx));

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
```

- [ ] **Step 2: 跑红**

先给 `push_files` 加上 `sender_jobs: &SenderJobMap` 参数让测试**编译通过**(此时本测试内调用已传参,其余调用点下一步统一改),然后:

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib push_failure_cleans_associated_source_jobs -- --nocapture
```
Expected: FAIL——收不到 SourceFailed(现状:push 失败不清理 sender 任务)。

- [ ] **Step 3: 实现**

**3a. sender_state.rs**——`SenderJobState` 加字段,构造函数补 `offer_id: None,`:

```rust
pub struct SenderJobState {
    // ……现有字段保持不变……
    /// 关联的推送 offer_id(大文件推送反向取流时由 push: 前缀解析)。
    /// None = 普通 pull。push_files 失败路径按它反查清理。
    pub offer_id: Option<u64>,
}
```

**3b. engine.rs MetaReq 分支**——现有 FIX B 桥接块(`if let Some(hex_id) = share_id.strip_prefix("push:")`,~1494 行)内加一行赋值:

```rust
                    // FIX B: Bridge push controls for large-file push (push: prefix share_id)
                    if let Some(hex_id) = share_id.strip_prefix("push:") {
                        if let Ok(offer_id) = u64::from_str_radix(hex_id, 16) {
                            // v0.6.x:记录关联 offer_id——push 失败路径按它反查清理
                            state.offer_id = Some(offer_id);
                            if let Some(pc) = get_push_control(offer_id) {
                                // ……现有桥接保持不变……
                            }
                        }
                    }
```

**3c. engine.rs**——`push_jobs()` 系列 helper 旁新增清理函数:

```rust
/// v0.6.x:推送失败/取消路径的关联 sender 任务清理。大文件推送时接收方
/// 反向 MetaReq 取流,本机路由器按 offer_id 建了 sender 任务(source 行);
/// push_files 失败返回前调用,否则这些任务永久 active(计时不停)。
/// 幂等:无关联任务时空操作。
pub async fn cleanup_push_senders(
    sender_jobs: &SenderJobMap,
    offer_id: u64,
    reason: &str,
) {
    let mut jobs = sender_jobs.write().await;
    let matched: Vec<u64> = jobs.iter()
        .filter(|(_, s)| s.offer_id == Some(offer_id))
        .map(|(id, _)| *id)
        .collect();
    for id in matched {
        if let Some(state) = jobs.remove(&id) {
            fire_probe_stop(&state);
            let _ = state.progress_tx.send(ProgressEvent::SourceFailed {
                job_id: id,
                reason: reason.to_string(),
            }).await;
            tracing::info!("推送失败清理关联 sender 任务: offer {:016x} job {:016x}", offer_id, id);
        }
    }
}
```

**3d. engine.rs**——`push_files`/`push_files_rel` 加参,主体挪入 `push_files_inner`,失败统一清理:

```rust
pub async fn push_files(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: Vec<PathBuf>,
    sender_jobs: &SenderJobMap,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    // 单文件/平铺模式：rel_dir 全空
    push_files_rel(sm, peer, files.iter().map(|p| (p.clone(), String::new())).collect(), sender_jobs, progress).await
}

/// v0.2.6 带相对目录的推送：files 为 (本地路径, 相对目录) 列表。
/// v0.6.x:sender_jobs 注册表穿透——失败路径按 offer_id 清理关联 sender 任务。
pub async fn push_files_rel(
    sm: &SessionManager,
    peer: &Fingerprint,
    files: Vec<(PathBuf, String)>,
    sender_jobs: &SenderJobMap,
    progress: mpsc::Sender<ProgressEvent>,
) -> Result<u64, EngineError> {
    let job_id = next_job_id();
    match push_files_inner(sm, peer, &files, job_id, progress).await {
        Ok(()) => Ok(job_id),
        Err(e) => {
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
) -> Result<(), EngineError> {
    // ……原主体……(成功路径结尾)
    let _ = progress.send(ProgressEvent::Done { job_id }).await;  // 整 job Done,保持原样
    Ok(())
}
```

注意 `SenderJobMap` 类型需在 engine.rs 可见(检查现有 `use`;spawn_rpc_router 已用该类型则已可见,否则补 `use crate::transfer::sender_state::SenderJobMap;`)。

**3e. 全部调用点补参**(机械改动,每处加一个实参):
- engine.rs 测试(~9 处):`cancel_pull_notifies_source_side` 无 push 调用不用动;`push_large_file_recv_ack_completes_source_side`(~2715)、`push_offer_flow_with_ask_policy`(~3061/3205/3227)、`push_control_bridging...`(~3333)、`offer_ask_timeout...`(~3420)、其它(~3482、4436)与 `push_files_rel` 直接调用(~4353)。**规则:测试里本地路由器用了哪个 map,push 调用就传它的引用**;路由器处 inline 写 `new_sender_job_map()` 的,提为变量 `let sender_jobs = ...;` 同时传给 spawn_rpc_router(clone)与 push(`&sender_jobs`);没有本地路由器的用例传 `&crate::transfer::sender_state::new_sender_job_map()`(临时 Arc 借用,编译器允许)。
- `crates/localtrans-relay/tests/e2e.rs:1106`:传 `&localtrans_core::transfer::sender_state::new_sender_job_map()`。
- `src-tauri/src/commands.rs`:push_files(~703)与 push_files_rel(~1003)传 `&state.sender_jobs`(AppState 字段,与路由器同源——确认 main.rs 路由器 spawn 用的就是 state.sender_jobs)。
- `crates/localtrans-ffi/src/lib.rs`(~693、~1427):传 ffi 侧路由器所用的同一 map(ffi start() 里 `let sender_jobs = new_sender_job_map();`——确认该变量传给 spawn_rpc_router 的同一实例;若 AppState 持有则用之)。

- [ ] **Step 4: 跑绿**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib push_failure_cleans_associated_source_jobs -- --nocapture
```
Expected: PASS。

- [ ] **Step 5: 回归**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib && cargo test -p localtrans-relay --test e2e && cargo check --workspace --tests
```
Expected: core 120 passed;relay e2e 通过(该套件既有基线内);workspace 编译 0 error(存量 warning 忽略)。

- [ ] **Step 6: 提交**

```bash
git add crates/localtrans-core/src/transfer/sender_state.rs crates/localtrans-core/src/transfer/engine.rs crates/localtrans-relay/tests/e2e.rs src-tauri/src/commands.rs crates/localtrans-ffi/src/lib.rs
git commit -m "feat(transfer): push 失败清理关联 sender 任务——offer_id 关联 + 注册表穿透"
```

---

### Task 3: 壳层取消接线 + spec 修订附录 + 全量回归

**Files:**
- Modify: `src-tauri/src/commands.rs`(transfer_action 拉取分支 + peer_fingerprint_of helper + 测试)
- Modify: `docs/superpowers/specs/2026-08-23-job-cancel-notify-design.md`(文末附录)

**Interfaces:**
- Consumes: Task 1 的 TransferCtl Cancel 语义;Task 2 的 push 签名(壳层调用点已在 Task 2 改完,本任务不改 push 相关)
- Produces: `fn peer_fingerprint_of(dto: &TransferDto) -> Option<[u8; 32]>`(私有 helper)

- [ ] **Step 1: 写失败测试**

commands.rs 测试模块加入:

```rust
#[test]
fn peer_fingerprint_of_parses_and_rejects() {
    let dto = |peer: &str| TransferDto {
        job_id: 1, name: "t".into(), total: 1, done: 0,
        state: "active".into(), speed_bps: 0, peer: peer.into(),
        direction: "pull".into(), local_role: "destination".into(),
        health: None, started_at_ms: None, finished_at_ms: None,
        source_path: None, fail_reason: None,
    };
    // 合法 32 字节 hex
    assert!(peer_fingerprint_of(&dto(&"ab".repeat(32))).is_some());
    // 空 peer(乙侧接收行)→ None,调用侧跳过发送
    assert!(peer_fingerprint_of(&dto("")).is_none());
    // 非 hex / 长度不符
    assert!(peer_fingerprint_of(&dto("xyz")).is_none());
    assert!(peer_fingerprint_of(&dto(&"ab".repeat(16))).is_none());
}
```

- [ ] **Step 2: 跑红**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans peer_fingerprint_of -- --nocapture
```
Expected: 编译失败——`peer_fingerprint_of` 未定义。

- [ ] **Step 3: 实现**

commands.rs 加 helper(transfer_action 附近):

```rust
/// 从任务行取对端指纹(取消通知的寻址)。无效/空 peer 返回 None。
fn peer_fingerprint_of(dto: &TransferDto) -> Option<[u8; 32]> {
    let bytes = hex::decode(&dto.peer).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&bytes);
    Some(fp)
}
```

transfer_action 拉取回退分支,`if !transfer::control_task(job_id, ctl) { return Err(...); }` 成功之后、本地状态更新之前插入:

```rust
    // v0.6.x:取消是跨端事件——通知数据源清理 sender 任务(其 source 行落终态,
    // 不再永久 active)。尽力而为:连接已断时发送失败,对端另有 30s 连接关闭
    // 兜底;任务先一步完成时对端幂等忽略。仅拉取分支发(本机是 source 的取消
    // 由 sender_jobs/push_control 分支提前 return,对端行由 JobFailed/机制 B 覆盖)。
    if action == "cancel" {
        if let Some(dto) = state.transfer_get_mut(job_id).await {
            if let Some(fp) = peer_fingerprint_of(&dto) {
                let _ = state.sm.send_ctrl(&fp, protocol::ControlMsg::TransferCtl {
                    job_id,
                    action: protocol::TransferAction::Cancel,
                }).await;
            }
        }
    }
```

(确认 commands.rs 已有 `protocol` 的 use——文件内 232 行起已有 `protocol::ControlMsg::SharesReq` 用法,沿用同路径;`TransferAction` 用全路径 `protocol::TransferAction::Cancel`。)

- [ ] **Step 4: 跑绿**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans --lib
```
Expected: 20 passed(19 + 新 1)。

- [ ] **Step 5: spec 修订附录**

spec 文件末尾追加:

```markdown
## 附录:实现修订(2026-08-23,计划阶段确定)

1. **不新增 JobCancel 消息**:复用既有 `TransferCtl{job_id, action}`——session.rs 已将其归入
   inbound_ctrl 转发类,engine.rs 路由器已有其分支(Throttle 已按 sender 侧处理),Cancel
   对称扩展。协议面零增量,原 3.1/3.2 的"新消息"由 TransferCtl 的 Cancel 分支承担。
2. **sender_jobs 参数穿透**(原 3.5 的实现取舍):注册表非进程级 static——壳层 AppState/
   ffi/测试各建 map 传给 spawn_rpc_router;`push_files`/`push_files_rel` 增加
   `sender_jobs: &SenderJobMap` 参数,清理与路由器操作同一 map,保持多实例测试隔离。
3. 原测试计划 T5(新消息序列化)随消息取消而取消;T4(暂停回归)以 Task 1 实现处的
   守护注释 + 既有 118 测试回归覆盖。
```

- [ ] **Step 6: 全量回归**

```bash
cd "C:/Users/<user>/Desktop/work/localTrans" && cargo test -p localtrans-core --lib && cargo test -p localtrans --lib && cargo check --workspace --tests
```
Expected: core 120;壳层 20;编译 0 error。(ffi 测试因本机端口占用的存量环境失败可忽略,但 `cargo check` 必须过。)

- [ ] **Step 7: 提交**

```bash
git add src-tauri/src/commands.rs docs/superpowers/specs/2026-08-23-job-cancel-notify-design.md
git commit -m "feat(shell): 取消下载时通知数据源(TransferCtl)——source 行跨端落终态"
```

---

## 收尾核查(控制器执行,非任务)

- [ ] 三个任务提交齐;`git log --oneline -4` 可见
- [ ] 手动 E2E 提示(告知用户,不阻塞):两实例 `cargo tauri dev`,A 下载 B 的大文件中途取消 → B 的传输页 source 行应在 1s 内落"失败(对方已取消)"、计时冻结
