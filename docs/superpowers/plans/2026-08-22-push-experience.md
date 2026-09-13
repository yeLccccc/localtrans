# 推送体验完善实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 双入口主动推送任意位置文件（设备卡片按钮 + 全局向导 + 拖放保留），接收方确认弹窗带可配置倒计时与拒绝原因传播（denied/timeout），拒绝/超时任务可一键重发，Auto 档完成后系统通知。

**Architecture:** 沿用"core 状态机 / 壳桥接 / UI 展示"三层。core：`OfferResp` 加可选 `reason` 字段；接收方 Ask 档确认 deadline 改为读配置、支持单次顺延；发送方兜底超时随配置伸缩。壳层：`OfferAsk` 扩展 extend 通道与 deadline；`TransferDto` 加 `fail_reason`；注册 notification 插件与 Auto 档 hook 泵。UI：PushWizard 组件、DeviceCard 推送菜单项、App.vue 弹窗倒计时/折叠/总大小、TransferItem 重发。

**Tech Stack:** Rust (tokio/quinn/serde) + Tauri 2 (tauri-plugin-notification/dialog) + Vue 3 + Pinia + vitest。

**Spec:** `docs/superpowers/specs/2026-08-22-push-experience-design.md`

## Global Constraints

- 超时配置：`offer_timeout_secs` 默认 60；core 使用处 `clamp(1, 600)`；壳层 `save_settings` 钳制 `clamp(15, 600)`
- 拒绝原因 serde snake_case：`"denied"` / `"timeout"`；`accepted=false` 时必给 reason（老对端无 reason 按 denied 处理）
- 明文路径/文件名不进日志（tracing 只打数量与 job_id）
- UI 文案全部中文；按钮加 `data-testid` 便于测试
- 提交信息中文，格式 `feat:` / `fix:` / `chore:` / `docs:` / `test:`
- 测试命令：Rust 在仓库根 `cargo test -p localtrans-core` / `cargo test -p localtrans`；UI 在 `ui/` 目录 `npx vitest run`
- 现有拖放路径行为不变（拖放直接推、无确认步）
- 版本目标 0.5.0，但本计划只写 CHANGELOG `[未发布]` 段；版本号对齐与打包属发版收尾，不在本计划
- **实现修订（相对 spec §5）**：文件选择对话框由前端直接调用 `@tauri-apps/plugin-dialog`（与 App.vue"另存到…"、Settings 选目录同模式），不新增壳层包装命令——vitest 可 vi.mock 该模块，spec 的"便于测试"理由不成立，且少两个命令注册

---

### Task 1: 协议 reason 字段

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs:100-136`（OfferResp）、`:297-308`（roundtrip 测试样例）、`:404-415`（snake_case 测试附近）
- Test: 同文件 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces（后续任务依赖的精确类型）:
  ```rust
  pub enum OfferDenyReason { Denied, Timeout }   // serde snake_case
  // ControlMsg::OfferResp 变为:
  // OfferResp { accepted: bool, save_dir: Option<String>,
  //             #[serde(skip_serializing_if = "Option::is_none", default)] reason: Option<OfferDenyReason> }
  ```
- 兼容性：老 JSON 无 reason 字段 → 反序列化为 None（`#[serde(default)]` 必须加，否则旧对端消息解析失败）

- [ ] **Step 1: 写失败测试**

在 `protocol.rs` tests 模块末尾追加：

```rust
    #[test]
    fn offer_resp_reason_snake_case_roundtrip() {
        // 拒绝带原因：序列化必须是小写 "timeout"/"denied"
        let msg = ControlMsg::OfferResp {
            accepted: false,
            save_dir: None,
            reason: Some(OfferDenyReason::Timeout),
        };
        let enc = encode_control(&msg);
        let json = String::from_utf8_lossy(&enc[4..]).to_string();
        assert!(json.contains("\"reason\":\"timeout\""), "实际: {}", json);
        assert_eq!(decode_control(&enc).unwrap(), msg);

        let msg2 = ControlMsg::OfferResp {
            accepted: false,
            save_dir: None,
            reason: Some(OfferDenyReason::Denied),
        };
        let enc2 = encode_control(&msg2);
        assert!(String::from_utf8_lossy(&enc2[4..]).contains("\"reason\":\"denied\""));
        assert_eq!(decode_control(&enc2).unwrap(), msg2);
    }

    #[test]
    fn offer_resp_reason_omitted_when_none() {
        // 接受时 reason 必须不出现在 JSON 里；且老格式（无 reason 键）能解析
        let msg = ControlMsg::OfferResp {
            accepted: true,
            save_dir: Some("D:/dl".into()),
            reason: None,
        };
        let enc = encode_control(&msg);
        let json = String::from_utf8_lossy(&enc[4..]).to_string();
        assert!(!json.contains("reason"), "实际: {}", json);

        let old_json = r#"{"type":"offer_resp","accepted":true,"save_dir":"D:/dl"}"#;
        let decoded: ControlMsg = serde_json::from_str(old_json).unwrap();
        assert_eq!(decoded, msg);
    }
```

同时更新现有 `control_roundtrip_all_variants` 里的 OfferResp 样例（`:305-308`）为：

```rust
            ControlMsg::OfferResp {
                accepted: false,
                save_dir: None,
                reason: Some(OfferDenyReason::Denied),
            },
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib protocol`
Expected: 编译错误 `cannot find type OfferDenyReason` / `no field reason`

- [ ] **Step 3: 最小实现**

`protocol.rs` 在 `ControlMsg` 定义前加：

```rust
/// v0.5.0 推送拒绝原因（OfferResp.accepted=false 时必给；老对端缺省按 Denied 处理）
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OfferDenyReason {
    /// 对方手动拒绝（或策略为 Deny）
    Denied,
    /// 对方确认超时未响应
    Timeout,
}
```

`ControlMsg::OfferResp` 改为：

```rust
    OfferResp {
        accepted: bool,
        save_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<OfferDenyReason>,
    },
```

- [ ] **Step 4: 跑测试确认通过（含全 crate 编译）**

Run: `cargo test -p localtrans-core --lib protocol && cargo check -p localtrans-core`
Expected: protocol 测试 PASS；**engine.rs 编译报错**（`OfferResp` 构造缺 reason 字段）——这是预期的跨文件影响，在 Task 3 修复。为保持本任务可独立提交，临时在 `engine.rs:1663` 与 `engine.rs:807` 的两处 `ControlMsg::OfferResp { ... }` 补 `reason: None,`（engine.rs:807 的模式匹配处补 `..` 不行——结构枚举需列全：`OfferResp { accepted, save_dir, reason: _ }`），`cargo check -p localtrans-core` 过即可。

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/protocol.rs crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): OfferResp 拒绝原因字段 denied/timeout(向后兼容缺省)"
```

---

### Task 2: offer_timeout_secs 配置

**Files:**
- Modify: `crates/localtrans-core/src/store.rs:8-49`（Config）
- Test: 同文件 tests 模块

**Interfaces:**
- Produces: `Config.offer_timeout_secs: u64`（serde default 60）；`SessionManager` 新访问器（session.rs）：
  ```rust
  impl SessionManager {
      /// 推送确认超时（core 钳制 1-600；发送方兜底 = 此值 + 30s）
      pub async fn offer_timeout_secs(&self) -> u64;
  }
  ```

- [ ] **Step 1: 写失败测试**

`store.rs` tests 追加：

```rust
    #[test]
    fn config_offer_timeout_roundtrip_and_default() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        assert_eq!(cfg.offer_timeout_secs, 60, "默认 60s");

        let mut cfg2 = Config::default();
        cfg2.offer_timeout_secs = 120;
        save_config(dir.path(), &cfg2).unwrap();
        let loaded = load_config(dir.path());
        assert_eq!(loaded.offer_timeout_secs, 120);
    }

    #[test]
    fn config_offer_timeout_old_file_defaults() {
        // 老配置文件没有该字段 → serde default 兜底 60
        let dir = tempdir().unwrap();
        let old = r#"{"device_name":"n","download_dir":".","hidden":false,"quic_port":1,"discovery_port":2,"shares":[]}"#;
        std::fs::write(dir.path().join("config.json"), old).unwrap();
        let cfg = load_config(dir.path());
        assert_eq!(cfg.offer_timeout_secs, 60);
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib store`
Expected: 编译错误 `no field offer_timeout_secs`

- [ ] **Step 3: 最小实现**

`store.rs` Config 加字段（consent_timeout_secs 之后）：

```rust
    #[serde(default = "default_offer_timeout_secs")]
    pub offer_timeout_secs: u64,
```

加默认函数与 Default 分支：

```rust
fn default_offer_timeout_secs() -> u64 {
    60
}
```

`impl Default for Config` 的构造里加 `offer_timeout_secs: 60,`。

`session.rs`（`impl SessionManager` 内，`consent_timeout` 读取处 `:676` 附近）加访问器：

```rust
    /// v0.5.0 推送确认超时（core 钳制 1-600；发送方兜底 = 此值 + 30s）
    pub async fn offer_timeout_secs(&self) -> u64 {
        self.ctx.config.read().await.offer_timeout_secs.clamp(1, 600)
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-core --lib store && cargo check -p localtrans-core`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/store.rs crates/localtrans-core/src/session.rs
git commit -m "feat(core): offer_timeout_secs 配置(默认 60,core 钳制 1-600)"
```

---

### Task 3: core 接收方 deadline/顺延 + 发送方 reason 映射

**Files:**
- Modify: `crates/localtrans-core/src/transfer/engine.rs:29-67`（EngineError）、`:402-409`（OfferAsk）、`:756-826`（push_files_rel 等待 resp）、`:1622-1667`（OfferReq 处理）
- Test: 同文件 tests 模块（`:2952` 附近的双机会话测试区）

**Interfaces:**
- Consumes: Task 1 `OfferDenyReason`、`OfferResp.reason`；Task 2 `sm.offer_timeout_secs().await`
- Produces:
  ```rust
  // EngineError 新变体（OfferRejected 保留 = 手动拒绝/Deny/老对端未知；OfferTimeout = 对方超时未确认）
  pub enum EngineError { ..., OfferRejected, OfferTimeout, ... }

  // OfferAsk 扩展为:
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

  // Auto 档 hook（进程级；壳层注册后 Auto 接受时收到通知用于完成系统通知）
  pub struct AutoOfferInfo { pub job_id: u64, pub peer: Fingerprint, pub file_count: usize }
  pub fn set_auto_offer_hook(tx: mpsc::Sender<AutoOfferInfo>);
  ```

- [ ] **Step 1: 写失败测试**

engine.rs tests 模块（`2952` 双机会话测试旁）追加。公共脚手架沿用该区现有模式（`setup_ctx`/`start_listener`/`take_inbound_ctrl_rx`/`spawn_rpc_router`），注意乙侧 ctx 用 `crate::test_support` 现有能力直接改 `config.write().await.offer_timeout_secs`：

```rust
    #[tokio::test]
    async fn offer_ask_timeout_returns_timeout_reason_and_sender_gets_offer_timeout() {
        // 乙 Ask 档 + 1s 超时 + 无人应答 → 乙回 reason=timeout，甲收 OfferTimeout（非 OfferRejected）
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, ctx_a) = crate::test_support::setup_ctx(dir_a.path());
        let (sm_b, mut ctx_b) = crate::test_support::setup_ctx(dir_b.path());
        let fp_a = ctx_a.identity.fingerprint();
        let fp_b = ctx_b.identity.fingerprint();

        // 互信 + 乙 Ask 档 + 1s 确认超时
        {
            let mut trust = ctx_b.trust.lock().await;
            let entry = trust.entry(fp_a).or_default();
            entry.perms.push = crate::identity::PushPolicy::Ask;
        }
        ctx_b.config.write().await.offer_timeout_secs = 1;
        {
            let mut trust = ctx_a.trust.lock().await;
            trust.entry(fp_b).or_default();
        }

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, mut ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);

        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, crate::transfer::sender_state::new_sender_job_map(), None);

        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        // 乙侧收到 Ask 但永不应答（drop respond 即拒绝——这里保持持有不答）
        let mut held: Option<OfferAsk> = None;
        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);

        let push = push_files(&sm_a, &fp_b, vec![src], None, progress_tx);
        tokio::pin!(push);
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
        let res = timeout(Duration::from_secs(10), &mut push).await.unwrap();
        assert!(matches!(res, Err(EngineError::OfferTimeout)), "实际: {:?}", res);
    }

    #[tokio::test]
    async fn offer_ask_extend_resets_deadline_once() {
        // 1s 超时；0.4s 时 extend 一次（重置为完整 1s）；0.8s 时应答接受 → 推送成功
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, ctx_a) = crate::test_support::setup_ctx(dir_a.path());
        let (sm_b, ctx_b) = crate::test_support::setup_ctx(dir_b.path());
        let fp_a = ctx_a.identity.fingerprint();
        let fp_b = ctx_b.identity.fingerprint();

        {
            let mut trust = ctx_b.trust.lock().await;
            trust.entry(fp_a).or_default().perms.push = crate::identity::PushPolicy::Ask;
        }
        ctx_b.config.write().await.offer_timeout_secs = 1;
        { let mut trust = ctx_a.trust.lock().await; trust.entry(fp_b).or_default(); }

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();
        let save_dir = dir_b.path().join("dl");
        std::fs::create_dir_all(&save_dir).unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, mut ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, crate::transfer::sender_state::new_sender_job_map(), None);
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
            push_files(&sm_a, &fp_b, vec![src], None, progress_tx)).await.unwrap();
        assert!(res.is_ok(), "顺延后应答应成功, 实际: {:?}", res.err());
    }

    #[tokio::test]
    async fn auto_offer_hook_fires_with_job_and_peer() {
        // 乙 Auto 档 → hook 收到 AutoOfferInfo(job_id/peer/file_count)
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let (sm_a, ctx_a) = crate::test_support::setup_ctx(dir_a.path());
        let (sm_b, ctx_b) = crate::test_support::setup_ctx(dir_b.path());
        let fp_a = ctx_a.identity.fingerprint();
        let fp_b = ctx_b.identity.fingerprint();

        { let mut trust = ctx_b.trust.lock().await;
          trust.entry(fp_a).or_default().perms.push = crate::identity::PushPolicy::Auto; }
        ctx_b.config.write().await.download_dir = dir_b.path().join("dl");
        std::fs::create_dir_all(dir_b.path().join("dl")).unwrap();
        { let mut trust = ctx_a.trust.lock().await; trust.entry(fp_b).or_default(); }

        let (auto_tx, mut auto_rx) = mpsc::channel::<AutoOfferInfo>(8);
        set_auto_offer_hook(auto_tx);

        let src = dir_a.path().join("t.txt");
        std::fs::write(&src, b"hello").unwrap();

        let b_addr = crate::test_support::start_listener(&sm_b).await;
        let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_b, _ask_rx_b) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_b.clone(), ctx_b.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_b, ask_tx_b, crate::transfer::sender_state::new_sender_job_map(), None);
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
        let (ask_tx_a, _ask_rx_a) = mpsc::channel::<OfferAsk>(8);
        spawn_rpc_router(sm_a.clone(), ctx_a.clone(), Arc::new(ShareRegistry::new(vec![])),
            ctrl_rx_a, ask_tx_a, crate::transfer::sender_state::new_sender_job_map(), None);
        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

        let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);
        let res = timeout(Duration::from_secs(15),
            push_files(&sm_a, &fp_b, vec![src], None, progress_tx)).await.unwrap();
        assert!(res.is_ok(), "Auto 接受应成功: {:?}", res.err());

        let info = timeout(Duration::from_secs(5), auto_rx.recv()).await.unwrap().unwrap();
        assert_eq!(info.peer, fp_a);
        assert_eq!(info.file_count, 1);
    }
```

注意：若 `setup_ctx` 返回签名与此不同（返回 tuple 或需要别的参数），以 `2952` 现有测试的实际用法为准调整脚手架行；断言不变。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib offer_`
Expected: 编译错误（OfferAsk 无 extend/deadline 字段、无 OfferTimeout、无 set_auto_offer_hook）

- [ ] **Step 3: 最小实现**

**3a. EngineError**（`:29-50`）加变体 + Display：

```rust
    /// 对方确认超时未响应（v0.5.0 与"拒绝"区分）
    OfferTimeout,
```

Display（`:63` OfferRejected 行后）：

```rust
            EngineError::OfferTimeout => write!(f, "对方超时未确认"),
```

**3b. OfferAsk 扩展**（`:402-409`）：

```rust
/// OfferAsk 事件（T16 UI 消费；v0.5.0 加顺延信号与 deadline）
pub struct OfferAsk {
    pub from: Fingerprint,
    pub job_id: u64,
    pub files: Vec<crate::protocol::OfferFile>,
    /// 响应通道：Some(save_dir) = 接受, None = 拒绝
    pub respond: tokio::sync::oneshot::Sender<Option<PathBuf>>,
    /// v0.5.0 "另存中"顺延信号——UI 打开目录选择器前 notify，deadline 重置一次（每 job 仅一次）
    pub extend: std::sync::Arc<tokio::sync::Notify>,
    /// v0.5.0 确认截止时刻（epoch 毫秒）
    pub deadline_epoch_ms: i64,
}

/// v0.5.0 Auto 档接受通知（壳层据此在完成时发系统通知）
pub struct AutoOfferInfo {
    pub job_id: u64,
    pub peer: Fingerprint,
    pub file_count: usize,
}

/// v0.5.0 Auto 档 hook（进程级；测试/未注册时为 None，行为同旧版）
pub fn set_auto_offer_hook(tx: mpsc::Sender<AutoOfferInfo>) {
    *auto_offer_hook().lock().unwrap() = Some(tx);
}

fn auto_offer_hook() -> &'static std::sync::Mutex<Option<mpsc::Sender<AutoOfferInfo>>> {
    static HOOK: std::sync::OnceLock<std::sync::Mutex<Option<mpsc::Sender<AutoOfferInfo>>>> =
        std::sync::OnceLock::new();
    HOOK.get_or_init(|| std::sync::Mutex::new(None))
}
```

（若 `set_inbound_recv_hook` 的既有实现模式不同——例如返回 guard 或用别的容器——照它的模式写，保持一致。）

**3c. 接收方 Ask 分支**（`:1636-1659` 整块替换）：

```rust
                        crate::identity::PushPolicy::Ask => {
                            // v0.5.0：超时读配置（1-600s），支持"另存中"单次顺延；
                            // 超时/拒绝分别带 reason 回发
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

                            let mut extended = false;
                            let mut deadline = tokio::time::Instant::now()
                                + Duration::from_secs(secs);
                            let mut respond_rx = respond_rx;
                            let mut answer: (bool, Option<String>, Option<crate::protocol::OfferDenyReason>) =
                                (false, None, Some(crate::protocol::OfferDenyReason::Denied));
                            loop {
                                tokio::select! {
                                    biased;
                                    r = &mut respond_rx => {
                                        answer = match r {
                                            Ok(Some(save_dir)) =>
                                                (true, Some(save_dir.display().to_string()), None),
                                            Ok(None) | Err(_) =>
                                                (false, None, Some(crate::protocol::OfferDenyReason::Denied)),
                                        };
                                        break;
                                    }
                                    _ = extend.notified(), if !extended => {
                                        extended = true;
                                        deadline = tokio::time::Instant::now()
                                            + Duration::from_secs(secs);
                                    }
                                    _ = tokio::time::sleep_until(deadline) => {
                                        answer = (false, None, Some(crate::protocol::OfferDenyReason::Timeout));
                                        break;
                                    }
                                }
                            }
                            answer
                        }
```

外层 `let (accepted, save_dir) = match perms.push {...}` 改为 `let (accepted, save_dir, deny_reason) = match perms.push {...}`，Deny 分支给 `(false, None, Some(OfferDenyReason::Denied))`，Auto 分支给 `(true, Some(dir...), None)` 并在 accepted 后追加 hook 通知（放在 `:1674` `if accepted {` 块内第一行）：

```rust
                        if accepted {
                            // v0.5.0 Auto/Ask 接受都通知壳层（壳层只对 Auto 发系统通知）
                            if let Some(tx) = auto_offer_hook().lock().unwrap().clone() {
                                let _ = tx.try_send(AutoOfferInfo {
                                    job_id,
                                    peer: fingerprint,
                                    file_count: files.len(),
                                });
                            }
```

（hook 对 Ask 也发——壳层用"该 job 是否走过 pending_offers"区分 Ask/Auto，见 Task 4。）

回发 OfferResp（`:1662-1667`）带 reason：

```rust
                    if let Err(e) = sm
                        .send_ctrl(&fingerprint, ControlMsg::OfferResp {
                            accepted,
                            save_dir: save_dir.clone(),
                            reason: if accepted { None } else { deny_reason },
                        })
                        .await
                    {
                        tracing::warn!("发送 OfferResp 失败: {}", e);
                    }
```

**3d. 发送方**（`:799-826`）：等待 resp 的 timeout 由硬编码 60 改为 `sm.offer_timeout_secs().await + 30`；模式匹配取 reason 并映射错误：

```rust
    // 等待 OfferResp（v0.5.0 兜底 = 接收方确认超时 + 30s 宽限；对端掉线不永久挂起）
    let offer_wait_secs = sm.offer_timeout_secs().await + 30;
    let mut resp_rx = sm.take_inbound_resp_rx().await.ok_or_else(|| {
        EngineError::Rpc("入站响应通道已被占用".to_string())
    })?;

    let resp = tokio::time::timeout(Duration::from_secs(offer_wait_secs), async {
        loop {
            match resp_rx.recv().await {
                Some((from, ControlMsg::OfferResp { accepted, save_dir, reason })) if from == *peer => {
                    return Ok((accepted, save_dir, reason));
                }
                Some(_) => continue,
                None => return Err(()),
            }
        }
    })
    .await;
    sm.return_inbound_resp_rx(resp_rx).await;
    let (accepted, _save_dir, deny_reason) = resp
        .map_err(|_| EngineError::Timeout)?
        .map_err(|_| EngineError::Rpc("等待 OfferResp 超时".to_string()))?;

    if !accepted {
        return Err(match deny_reason {
            Some(crate::protocol::OfferDenyReason::Timeout) => EngineError::OfferTimeout,
            _ => EngineError::OfferRejected,
        });
    }
```

同时删除 Task 1 在 engine.rs 打的两处临时补丁中 `:807` 的 `reason: _` 改为上面的解构（`:1663` 已被 3c 覆盖）。

- [ ] **Step 4: 跑测试确认通过 + 全量回归**

Run: `cargo test -p localtrans-core`
Expected: 全部 PASS（含既有 `push_files` Ask 拒绝测试——它断言 `OfferRejected`，手动拒绝路径仍映射 OfferRejected，不需改）

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-core/src/transfer/engine.rs
git commit -m "feat(core): 推送确认超时可配+单次顺延+拒绝原因传播(denied/timeout)"
```

---

### Task 4: 壳层桥接（事件/命令/fail_reason/系统通知）

**Files:**
- Modify: `src-tauri/src/main.rs:234-259`（TransferDto）、`:276-287`（ConfigDto）、`:379-380`（插件注册）、`:441`/`:868-893`（ask 通道与事件桥）、`:808+`（recv 事件泵 Done 分支）
- Modify: `src-tauri/src/commands.rs:626-752`（push_files）、`:931-1042`（push_files_rel）、`:1160-1183`（respond_offer）、`:1511-1566`（config dto/save_settings）
- Modify: `src-tauri/capabilities/default.json`
- Test: `src-tauri/src/commands.rs` tests 模块（`:2069` 附近）

**Interfaces:**
- Consumes: Task 3 `OfferAsk.extend/deadline_epoch_ms`、`AutoOfferInfo`、`set_auto_offer_hook`、`EngineError::OfferTimeout`
- Produces（前端/Task 5 依赖）:
  - 事件 `offer-request` payload 增 `"deadline_epoch_ms": i64`
  - 新命令 `offer_extend(job_id: u64) -> Result<(), String>`
  - `TransferDto` 增 `fail_reason: Option<String>`（serde default）
  - `ConfigDto` 增 `offer_timeout_secs: u64`（default 60）；save_settings `clamp(15, 600)`
  - 系统通知：Auto 档接收完成时发（失败降级 toast）

- [ ] **Step 1: 写失败测试**

commands.rs tests 模块追加：

```rust
    #[test]
    fn save_settings_clamps_offer_timeout() {
        let (st, _dir) = test_state().unwrap(); // 既有测试脚手架,名字以现状为准
        let mut dto = base_settings_dto(&st);   // 既有脚手架:取当前设置转 dto
        dto.offer_timeout_secs = 5;
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(async {
            save_settings_inner(&st, dto.clone()).await.unwrap();
            assert_eq!(st.config.read().await.offer_timeout_secs, 15, "低于 15 钳到 15");
            let mut dto2 = dto.clone();
            dto2.offer_timeout_secs = 9999;
            save_settings_inner(&st, dto2).await.unwrap();
            assert_eq!(st.config.read().await.offer_timeout_secs, 600, "高于 600 钳到 600");
        });
    }
```

（若 save_settings 现签名是 `State<'_, AppState>` 不能直测：把钳制逻辑抽成纯函数 `pub fn clamp_offer_timeout(v: u64) -> u64 { v.clamp(15, 600) }`，save_settings 调它，测试直测纯函数——二选一，跟随 `consent_timeout_secs` 现有测试 `:2069` 的既有模式。）

fail_reason 序列化测试：

```rust
    #[test]
    fn transfer_dto_fail_reason_defaults_none() {
        // 老 transfers.json 无 fail_reason 字段 → 反序列化 None
        let old = r#"{"job_id":"0x1","name":"a","total":1,"done":0,"state":"failed",
                     "speed_bps":0,"peer":"aa","direction":"push"}"#;
        let dto: crate::TransferDto = serde_json::from_str(old).unwrap();
        assert!(dto.fail_reason.is_none());
    }
```

（TransferDto 若为 main.rs 私有，把测试放 main.rs tests 或将 dto 挪 pub(crate)——跟随现有 `:1155`/`:1296` 测试的位置。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans`
Expected: 编译错误（无 offer_timeout_secs / fail_reason / clamp 函数）

- [ ] **Step 3: 最小实现**

**3a. TransferDto**（main.rs `:258` 后）：

```rust
    /// v0.5.0 失败原因文案（"推送请求被对方拒绝"/"对方超时未确认"等;前端直接展示）
    #[serde(default)]
    fail_reason: Option<String>,
```

所有构造 TransferDto 的地方补 `fail_reason: None`（编译器逐一指出，约 10 处）。

**3b. ConfigDto**（main.rs `:283` 后）：

```rust
    #[serde(default = "default_offer_timeout_secs")]
    offer_timeout_secs: u64,
```

加 `fn default_offer_timeout_secs() -> u64 { 60 }`；`config_to_dto`（commands.rs `:1526`）补 `offer_timeout_secs: config.offer_timeout_secs,`；save_settings（`:1549`）加：

```rust
    config.offer_timeout_secs = dto.offer_timeout_secs.clamp(15, 600);
```

**3c. pending_offers 升级 + offer_extend**：AppState 的 `pending_offers` 值类型改为结构体（main.rs 定义处）：

```rust
/// v0.5.0 待答推送：respond + 顺延信号
pub struct PendingOffer {
    pub respond: tokio::sync::oneshot::Sender<Option<std::path::PathBuf>>,
    pub extend: std::sync::Arc<tokio::sync::Notify>,
}
```

ask_rx 循环（`:871-892`）insert 改 `st.pending_offers.lock().await.insert(ask.job_id, PendingOffer {
    respond: ask.respond, extend: ask.extend });`，事件 payload 加：

```rust
                        let _ = app_handle_clone.emit("offer-request", serde_json::json!({
                            "job_id": ask.job_id,
                            "peer": fp_hex,
                            "files": files,
                            "deadline_epoch_ms": ask.deadline_epoch_ms
                        }));
```

respond_offer（commands.rs `:1160-1183`）改为：

```rust
#[tauri::command]
pub async fn respond_offer(
    state: State<'_, AppState>,
    job_id: u64,
    accepted: bool,
    save_dir: Option<String>,
) -> Result<(), String> {
    let pending = state.pending_offers.lock().await.remove(&job_id)
        .ok_or_else(|| format!("未找到待答任务: {}", job_id))?;

    let save = if accepted {
        if let Some(dir) = save_dir {
            Some(PathBuf::from(dir))
        } else {
            Some(state.config.read().await.download_dir.clone())
        }
    } else {
        None
    };

    pending.respond.send(save)
        .map_err(|_| "该请求已超时或已结束".to_string())
}

/// v0.5.0 "另存中"顺延：UI 打开目录选择器前调用，确认 deadline 重置一次
#[tauri::command]
pub async fn offer_extend(state: State<'_, AppState>, job_id: u64) -> Result<(), String> {
    let pending = state.pending_offers.lock().await.get(&job_id)
        .ok_or_else(|| format!("未找到待答任务: {}", job_id))?;
    pending.extend.notify_one();
    Ok(())
}
```

main.rs invoke_handler 注册 `commands::offer_extend`。

**3d. fail_reason 写入**：push_files（`:724-737`）与 push_files_rel（`:1016-1029`）的 `if let Err(e) = res` 分支，把 dto 更新改为：

```rust
                        if placeholder_alive {
                            if let Some(mut dto) = st.transfer_get_mut(placeholder_id).await {
                                dto.state = "failed".into();
                                dto.fail_reason = Some(e.to_string());
                                st.transfer_update(placeholder_id, dto).await;
                            }
                            placeholder_alive = false;
                        }
```

（`e.to_string()` 经 EngineError Display 得"推送请求被对方拒绝"/"对方超时未确认"。）

**3e. 系统通知**：
- main.rs `:380` 后加 `.plugin(tauri_plugin_notification::init())`（Cargo.toml 已有依赖）
- capabilities/default.json permissions 数组加 `"notification:default"`
- ask 通道创建处（`:441` 旁）加 auto hook 通道并注册：

```rust
                let (auto_tx, mut auto_rx) = mpsc::channel::<localtrans_core::transfer::AutoOfferInfo>(16);
                localtrans_core::transfer::set_auto_offer_hook(auto_tx);
```

- AppState 加 `pub auto_offers: tokio::sync::Mutex<std::collections::HashMap<u64, (String, usize)>>`（job_id → (peer 指纹 hex, file_count)）；
- auto_rx 泵：Ask 的 job 会先入 pending_offers（respond 后被 remove）——用"收到 AutoOfferInfo 时该 job 是否仍在 pending_offers"不可靠（时序），改为：auto_rx 泵只 insert；Ask 应答时 respond_offer 里 `state.auto_offers.lock().await.remove(&job_id);`（Ask 用户已确认过，不需要通知）。
- recv 事件泵 Done 分支（main.rs `:840` 附近，`PE::Done` 处理 dto 完成后）追加：

```rust
                                PE::Done { job_id } => {
                                    // ...现有 dto 完成逻辑不动...
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
                                        use tauri::Manager;
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
```

（`peer_hex_chars` 是 8 字截断显示的辅助；`notification()` 需 `tauri::Manager` trait 与插件的 extension trait——以 `use tauri_plugin_notification::NotificationExt;` 为准，编译器会指正确切导入。`st.devices` 的元素类型以 discovery::DeviceInfo 现状为准，fingerprint 字段可能是 [u8;32] 或 String——按现状写。）

- respond_offer 里补 `state.auto_offers.lock().await.remove(&job_id);`（Ask 应答清除，避免 Ask 完成也通知）。

- [ ] **Step 4: 跑测试确认通过 + 壳层全量**

Run: `cargo test -p localtrans && cargo check -p localtrans`
Expected: 全 PASS（含既有 16 个壳层测试）

- [ ] **Step 5: 提交**

```bash
git add src-tauri/src/main.rs src-tauri/src/commands.rs src-tauri/capabilities/default.json
git commit -m "feat(壳): 推送确认倒计时桥接+offer_extend+fail_reason+Auto 完成系统通知"
```

---

### Task 5: UI（向导/卡片入口/弹窗升级/重发/设置）

**Files:**
- Create: `ui/src/components/PushWizard.vue`、`ui/src/composables/useOfferCountdown.ts`
- Modify: `ui/src/pages/Devices.vue`（顶部按钮+向导挂载+DeviceCard 事件）、`ui/src/components/DeviceCard.vue`（菜单项+角标）、`ui/src/App.vue`（弹窗升级）、`ui/src/components/TransferItem.vue`（文案/重发）、`ui/src/pages/Settings.vue`（推送确认超时输入）、`ui/src/api.ts`、`ui/src/types.ts`、`ui/src/stores/transfers.ts`
- Test: `ui/src/__tests__/PushWizard.test.ts`、`ui/src/__tests__/useOfferCountdown.test.ts`、扩展 `ui/src/__tests__/TransferItem.test.ts`

**Interfaces:**
- Consumes: Task 4 事件 `offer-request`（+`deadline_epoch_ms`）、命令 `offer_extend`、`TransferDto.fail_reason`、`ConfigDto.offer_timeout_secs`
- Produces:
  - `PushWizard.vue`：props `{ presetFingerprint?: string }`（卡片入口预选设备直入步 2）；emit `close`
  - `useOfferCountdown(deadlineEpochMs: Ref<number | null>)` → `{ remainingMs: Ref<number>, expired: Ref<boolean>, urgent: Ref<boolean>, reset(deadlineMs: number): void }`（500ms 心跳；urgent = 剩余 ≤10s；expired 自动停表）
  - `transfersStore.recordPushRelRequest(job_id, fp, items: [string, string][])`；`retryTransfer` 支持 `type: 'push-rel'`
  - `api.browse.offerExtend(jobId: number)`

- [ ] **Step 1: 写失败测试**

`ui/src/__tests__/useOfferCountdown.test.ts`：

```ts
/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest'
import { ref } from 'vue'
import { useOfferCountdown } from '../composables/useOfferCountdown'

describe('useOfferCountdown', () => {
  beforeEach(() => { vi.useFakeTimers() })
  afterEach(() => { vi.useRealTimers() })

  it('counts down and flags urgent under 10s', () => {
    const deadline = ref(Date.now() + 12_000)
    const { remainingMs, urgent, expired } = useOfferCountdown(deadline)
    expect(remainingMs.value).toBe(12_000)
    vi.advanceTimersByTime(2500)
    expect(remainingMs.value).toBeLessThanOrEqual(9500)
    expect(urgent.value).toBe(true)
    expect(expired.value).toBe(false)
  })

  it('expires at zero and stops', () => {
    const deadline = ref(Date.now() + 1000)
    const { remainingMs, expired } = useOfferCountdown(deadline)
    vi.advanceTimersByTime(1500)
    expect(expired.value).toBe(true)
    expect(remainingMs.value).toBe(0)
  })

  it('reset re-arms with a new deadline', () => {
    const deadline = ref(Date.now() + 1000)
    const { remainingMs, reset } = useOfferCountdown(deadline)
    vi.advanceTimersByTime(800)
    reset(Date.now() + 10_000)
    expect(remainingMs.value).toBeGreaterThan(9000)
  })
})
```

`ui/src/__tests__/PushWizard.test.ts`：

```ts
/** @vitest-environment jsdom */
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount } from '@vue/test-utils'
import { setActivePinia, createPinia } from 'pinia'
import PushWizard from '../components/PushWizard.vue'

vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn().mockResolvedValue(['C:/a.txt', 'C:/b.txt']),
}))
vi.mock('../api', () => ({
  api: {
    browse: {
      expandLocalPaths: vi.fn().mockResolvedValue([
        ['C:/a.txt', ''], ['C:/b.txt', ''],
      ]),
      pushFilesRel: vi.fn().mockResolvedValue('0x64'),
      connectDevice: vi.fn(),
    },
  },
}))
vi.mock('../stores/devices', () => ({
  useDevicesStore: () => ({
    devices: [
      { fingerprint: 'f1', name: '电脑甲', online: true },
      { fingerprint: 'f2', name: '电脑乙', online: false },
    ],
  }),
}))
vi.mock('../stores/settings', () => ({
  useSettingsStore: () => ({
    trustedPeers: [
      { fingerprint: 'f1', perms: {} },
      { fingerprint: 'f2', perms: {} },
    ],
  }),
}))
vi.mock('../stores/toast', () => ({
  useToastStore: () => ({ push: vi.fn() }),
}))

describe('PushWizard', () => {
  beforeEach(() => { setActivePinia(createPinia()) })

  it('step1 lists only trusted+online devices', async () => {
    const w = mount(PushWizard)
    expect(w.text()).toContain('电脑甲')
    expect(w.text()).not.toContain('电脑乙')
  })

  it('presetFingerprint skips to file step', async () => {
    const w = mount(PushWizard, { props: { presetFingerprint: 'f1' } })
    expect(w.find('[data-testid="btn-pick-files"]').exists()).toBe(true)
  })
})
```

TransferItem.test.ts 扩展（追加用例）：

```ts
  it('source-push failed with deny reason shows 重发 when request recorded', async () => {
    const { useTransfersStore } = await import('../stores/transfers')
    const store = useTransfersStore()
    store.lastRequest.set('0000000000000001', {
      type: 'push-rel' as any, fp: 'aabb', items: [['C:/a.txt', '']],
    })
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer,
          state: 'failed',
          local_role: 'source-push' as const,
          direction: 'push' as const,
          fail_reason: '推送请求被对方拒绝',
        } as any,
      },
      ...mountOptions,
    })
    expect(w.text()).toContain('推送请求被对方拒绝')
    const btn = w.find('[data-testid="btn-resend"]')
    expect(btn.exists()).toBe(true)
    await btn.trigger('click')
    const { api } = await import('../api')
    expect(api.browse.pushFilesRel).toHaveBeenCalledWith('aabb', [['C:/a.txt', '']])
  })

  it('source-push failed without record shows no 重发', () => {
    const w = mount(TransferItem, {
      props: {
        transfer: {
          ...baseTransfer, state: 'failed',
          local_role: 'source-push' as const, direction: 'push' as const,
          fail_reason: '对方超时未确认',
        } as any,
      },
      ...mountOptions,
    })
    expect(w.find('[data-testid="btn-resend"]').exists()).toBe(false)
  })
```

（TransferItem.test.ts 现有 mock 的 api.browse 里要补 `pushFilesRel: vi.fn()`。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cd ui && npx vitest run`
Expected: 新文件找不到模块/组件；TransferItem 新用例 fail

- [ ] **Step 3: 最小实现**

**3a. types.ts**：`TransferDto` 加 `fail_reason?: string | null`；`ConfigDto` 加 `offer_timeout_secs: number`；offer-request 事件类型加 `deadline_epoch_ms: number`。transfers store 的 lastRequest 值类型加 `'push-rel'` 与 `items?: [string, string][]`。

**3b. api.ts** browse 加：

```ts
  offerExtend: (job_id: number): Promise<void> =>
    invokeCommand('offer_extend', { jobId: job_id }),
```

**3c. composables/useOfferCountdown.ts**：

```ts
import { ref, watch, onUnmounted, type Ref } from 'vue'

/** v0.5.0 接收确认倒计时（500ms 心跳；≤10s urgent；到 0 expired 停表） */
export function useOfferCountdown(deadlineEpochMs: Ref<number | null>) {
  const remainingMs = ref(0)
  const expired = ref(false)
  const urgent = ref(false)
  let timer: number | null = null

  function tick() {
    if (deadlineEpochMs.value == null) return
    const left = deadlineEpochMs.value - Date.now()
    remainingMs.value = Math.max(0, left)
    urgent.value = left > 0 && left <= 10_000
    if (left <= 0) {
      expired.value = true
      stop()
    }
  }

  function stop() {
    if (timer !== null) { window.clearInterval(timer); timer = null }
  }

  function reset(deadlineMs: number) {
    expired.value = false
    deadlineEpochMs.value = deadlineMs
    tick()
  }

  watch(deadlineEpochMs, (v) => {
    stop()
    if (v == null) return
    expired.value = false
    tick()
    timer = window.setInterval(tick, 500)
  }, { immediate: true })

  onUnmounted(stop)

  return { remainingMs, expired, urgent, reset }
}
```

**3d. PushWizard.vue**（要点，样式跟随 PairingDialog 的 modal 结构与 design.css token）：

```vue
<template>
  <div class="modal-backdrop" @click.self="emit('close')">
    <div class="modal-card push-wizard" data-testid="push-wizard">
      <h3>推送文件</h3>

      <!-- 步 1：选设备（仅已配对+在线，单选） -->
      <div v-if="step === 1">
        <p class="wizard-hint">选择要推送到的设备</p>
        <div v-if="selectableDevices.length === 0" class="wizard-empty">
          没有已配对且在线的设备——先在设备页完成配对
        </div>
        <label
          v-for="d in selectableDevices" :key="d.fingerprint"
          class="wizard-device" :class="{ selected: selectedFp === d.fingerprint }"
        >
          <input type="radio" :value="d.fingerprint" v-model="selectedFp" />
          <span>{{ d.name }}</span>
          <span v-if="d.via_relay" class="badge badge-remote">远程</span>
        </label>
        <div class="modal-actions">
          <button class="btn-secondary" @click="emit('close')">取消</button>
          <button class="btn-primary" :disabled="!selectedFp" @click="step = 2">下一步</button>
        </div>
      </div>

      <!-- 步 2：选文件 + 确认 -->
      <div v-else>
        <p class="wizard-hint">推送到 <b>{{ targetName }}</b>，共 {{ items.length }} 个文件（{{ formatSize(totalBytes) }}）</p>
        <div class="wizard-buttons">
          <button class="btn-secondary" data-testid="btn-pick-files" @click="pickFiles">选择文件</button>
          <button class="btn-secondary" data-testid="btn-pick-folder" @click="pickFolder">选择文件夹</button>
        </div>
        <ul class="wizard-files">
          <li v-for="(it, i) in items" :key="it[0]">
            <span class="file-path">{{ it[0] }}</span>
            <button class="link-btn" @click="items.splice(i, 1)">移除</button>
          </li>
        </ul>
        <div v-if="expandError" class="error-message">{{ expandError }}</div>
        <div class="modal-actions">
          <button v-if="!presetFingerprint" class="btn-secondary" @click="step = 1">上一步</button>
          <button class="btn-secondary" @click="emit('close')">取消</button>
          <button class="btn-primary" data-testid="btn-send" :disabled="items.length === 0 || sending" @click="send">
            {{ sending ? '发送中...' : `发送 ${items.length} 个文件` }}
          </button>
        </div>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue'
import { open } from '@tauri-apps/plugin-dialog'
import { api } from '../api'
import { useDevicesStore } from '../stores/devices'
import { useSettingsStore } from '../stores/settings'
import { useToastStore } from '../stores/toast'
import { useTransfersStore } from '../stores/transfers'

const props = defineProps<{ presetFingerprint?: string }>()
const emit = defineEmits<{ (e: 'close'): void }>()

const devicesStore = useDevicesStore()
const settingsStore = useSettingsStore()
const toastStore = useToastStore()
const transfersStore = useTransfersStore()

const step = ref(props.presetFingerprint ? 2 : 1)
const selectedFp = ref(props.presetFingerprint ?? '')
const items = ref<[string, string][]>([])
const expandError = ref('')
const sending = ref(false)

const selectableDevices = computed(() =>
  devicesStore.devices.filter(d =>
    d.online && settingsStore.trustedPeers.some(p => p.fingerprint === d.fingerprint)))

const targetName = computed(() =>
  devicesStore.devices.find(d => d.fingerprint === selectedFp.value)?.name ?? selectedFp.value)

const totalBytes = ref(0) // 选完后由 expand 结果累计（展开前未知则显示个数即可,可先省略大小——见 send 实现）

async function appendPaths(paths: string[] | string | null) {
  if (!paths) return
  const list = Array.isArray(paths) ? paths : [paths]
  if (list.length === 0) return
  expandError.value = ''
  try {
    const expanded = await api.browse.expandLocalPaths(list)
    if (expanded.length === 0) {
      expandError.value = '没有可推送的文件（文件夹可能为空或全是隐藏项）'
      return
    }
    // 去重合并
    const seen = new Set(items.value.map(i => i[0]))
    for (const it of expanded) if (!seen.has(it[0])) items.value.push(it)
  } catch (e) {
    expandError.value = e instanceof Error ? e.message : String(e)
  }
}

async function pickFiles() {
  await appendPaths(await open({ multiple: true, title: '选择要推送的文件' }) as string[] | null)
}

async function pickFolder() {
  await appendPaths(await open({ directory: true, multiple: false, title: '选择要推送的文件夹' }) as string | null)
}

async function send() {
  if (!selectedFp.value || items.value.length === 0) return
  sending.value = true
  try {
    await devicesStore.connectDevice(selectedFp.value)
    const jobId = await api.browse.pushFilesRel(selectedFp.value, items.value)
    transfersStore.recordPushRelRequest(jobId, selectedFp.value, items.value.map(i => [i[0], i[1]]))
    toastStore.push('success', `开始推送 ${items.value.length} 个文件`)
    emit('close')
  } catch (e) {
    toastStore.push('error', '推送失败: ' + (e instanceof Error ? e.message : String(e)))
  } finally {
    sending.value = false
  }
}

function formatSize(bytes: number): string {
  if (bytes === 0) return '0 B'
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let v = bytes, i = 0
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i++ }
  return `${v.toFixed(1)} ${units[i]}`
}
</script>
```

（totalBytes 若拿不到就删掉汇总显示，只显示个数——不要留死代码。）

**3e. Devices.vue**：header-actions 加按钮：

```html
        <button class="btn btn-primary" @click="pushWizardOpen = true" data-testid="btn-push-wizard">
          推送文件
        </button>
```

（放在"刷新"按钮左侧。）挂载向导：

```html
    <PushWizard v-if="pushWizardOpen" @close="pushWizardOpen = false" />
    <PushWizard v-if="cardPushFp" :preset-fingerprint="cardPushFp" @close="cardPushFp = null" />
```

script：`import PushWizard from '../components/PushWizard.vue'`、`const pushWizardOpen = ref(false)`、`const cardPushFp = ref<string | null>(null)`；DeviceCard 绑定事件：

```html
      <DeviceCard ... @request-push="cardPushFp = $event" />
```

`handlePushFilesToDevice`（`:277-292`）发送成功后补记录（拖放路径同样可重发）：

```ts
    const jobId = await api.browse.pushFilesRel(fingerprint, items)
    transfersStore.recordPushRelRequest(jobId, fingerprint, items)
```

（需 `import { useTransfersStore } from '../stores/transfers'` 并实例化。）

**3f. DeviceCard.vue**：下拉菜单"权限设置"区块前加菜单项：

```html
            <div class="dropdown-divider"></div>
            <button class="dropdown-item" data-testid="btn-push-menu"
              :disabled="!device.online" @click="emit('request-push', device.fingerprint)">
              推送文件…
            </button>
```

emit 声明加 `(e: 'request-push', fingerprint: string)`。角标：`device-actions` 里浏览按钮旁加

```html
        <span v-if="hasActivePush" class="push-spinner" title="推送进行中">⏳</span>
```

`hasActivePush` 由 `useTransfersStore().transfers` 计算：存在 `local_role==='source-push' && peer===device.fingerprint && ['pending','active'].includes(state)`。（用 CSS spinner 或 emoji 皆可，跟随 design token 风格。）

**3g. App.vue 弹窗升级**：
- offer-request 处理存 `deadline_epoch_ms`
- `const { remainingMs, expired, urgent, reset } = useOfferCountdown(computed(() => offerRequest.value?.deadline_epoch_ms ?? null))`
- 模板 header 下加倒计时条：

```html
          <div v-if="remainingMs > 0" class="offer-countdown" :class="{ urgent }" data-testid="offer-countdown">
            {{ Math.floor(remainingMs / 60000) }}:{{ String(Math.floor((remainingMs % 60000) / 1000)).padStart(2, '0') }} 后自动拒绝
          </div>
```

- `watch(expired, v => { if (v && offerRequest.value) { offerRequest.value = null; toastStore.push('info', '已超时自动拒绝') } })`
- 文件列表 >8 折叠：`const filesExpanded = ref(false)`，模板 `v-for="(file, index) in (filesExpanded ? offerRequest.files : offerRequest.files.slice(0, 8))"`，列表底加

```html
            <button v-if="offerRequest.files.length > 8" class="link-btn" data-testid="btn-expand-files"
              @click="filesExpanded = !filesExpanded">
              {{ filesExpanded ? '收起' : `共 ${offerRequest.files.length} 个文件（展开查看）` }}
            </button>
```

- 总大小汇总行（文件数量 info-item 旁）：`共 {{ formatSize(offerRequest.files.reduce((s, f) => s + (f.size || 0), 0)) }}`
- `handleSaveAs` 打开对话框前顺延：

```ts
    if (offerRequest.value) {
      try { await api.browse.offerExtend(offerRequest.value.job_id) } catch { /* 已超时则由后续 respond 报错 */ }
      reset(Date.now() + 60_000) // 本地重置展示（core 同规则顺延一次,时长取设置默认 60s 的近似值即可——倒计时仅展示层）
    }
```

（`reset` 的时长：Settings 里的 offer_timeout_secs 可从 settingsStore 取 `settingsStore.config?.offer_timeout_secs ?? 60`。）

**3h. TransferItem.vue**：
- source-push pending 文案（`:76`）`等待对方接收...` → `等待对方确认接收...`
- 状态徽章下失败原因：stateText 计算后,模板 status-badge 后加

```html
        <span v-if="transfer.state === 'failed' && transfer.fail_reason" class="fail-reason">{{ transfer.fail_reason }}</span>
```

- source-push failed 分支（`:89-91` 的 else 前）加：

```html
        <template v-else-if="transfer.state === 'failed'">
          <button v-if="canResend" @click="handleResend" class="btn btn-primary" data-testid="btn-resend">重发</button>
          <button @click="handleRemove" class="btn btn-secondary">删除</button>
        </template>
```

script：

```ts
const canResend = computed(() => {
  if (props.transfer.local_role !== 'source-push' || props.transfer.state !== 'failed') return false
  return transfersStore.lastRequest.get(props.transfer.job_id)?.type === 'push-rel'
})

async function handleResend() {
  try {
    await transfersStore.retryTransfer(props.transfer.job_id)
    toastStore.push('info', '已重新发起推送')
  } catch (e) {
    toastStore.push('error', '重发失败: ' + (e instanceof Error ? e.message : String(e)))
  }
}
```

**3i. transfers.ts**：

```ts
  function recordPushRelRequest(job_id: string, fp: string, items: [string, string][]) {
    lastRequest.value.set(job_id, { type: 'push-rel', fp, items })
  }
```

retryTransfer 的分支加：

```ts
      } else if (request.type === 'push-rel') {
        await api.browse.pushFilesRel(request.fp, request.items!)
      }
```

lastRequest 类型定义改 `{ type: 'pull' | 'push' | 'push-rel'; fp: string; share_id?: string; path?: string; paths?: string[]; items?: [string, string][] }`，return 列表导出 `recordPushRelRequest`。

**3j. Settings.vue** 连接安全卡片（`:191` security-form 内、同意超时输入后）加：

```html
            <div class="identity-item">
              <span class="label">推送确认超时(秒):</span>
              <input
                v-model.number="offerTimeoutSecs"
                type="number" min="15" max="600"
                class="form-input"
              />
            </div>
            <div class="security-hint">对方推送文件时,你有多少秒时间确认;超时自动拒绝(15-600)</div>
```

script：`const offerTimeoutSecs = ref(60)`；`refreshSettings`/`saveSettings`（跟随 consentTimeoutSecs 的现有读写位置）同步 `offer_timeout_secs` 字段。

- [ ] **Step 4: 跑测试确认通过**

Run: `cd ui && npx vitest run && npm run build`
Expected: 全 PASS（含 3 个既有 TransferItem 历史失败用例若仍红——那是 v0.4.0 已知历史遗留,与本任务无关,不修）; vue-tsc 无错

- [ ] **Step 5: 提交**

```bash
git add ui/src
git commit -m "feat(ui): 推送向导双入口+接收弹窗倒计时+拒绝原因展示与一键重发"
```

---

### Task 6: E2E 回归 + 文档

**Files:**
- Modify: `crates/localtrans-relay/tests/e2e.rs`（复用双会话框架加 1 个 Ask 超时用例）
- Modify: `README.md:50-54`（推送流程）、`CHANGELOG.md`（[未发布] 段）
- Test: relay E2E

**Interfaces:**
- Consumes: Task 3 `EngineError::OfferTimeout`、`offer_timeout_secs` 配置、Task 5 完成的全链路

- [ ] **Step 1: 写失败的 E2E**

e2e.rs 复用现有局域网双会话测试模式（找该文件里直连/loopback 的既有用例脚手架），追加（脚手架行从相邻用例复制，三处差异：`policy=Ask`、`offer_timeout_secs=1`、不 spawn 应答者；断言必须是 `Err(EngineError::OfferTimeout)` 而非泛化 error——这是本用例存在的意义）：

```rust
#[tokio::test]
async fn push_ask_timeout_reason_reaches_sender() {
    use localtrans_core::identity::PushPolicy;
    use localtrans_core::transfer::{push_files, spawn_rpc_router, EngineError, ProgressEvent};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    // 复用 e2e.rs 相邻用例的临时目录/SessionManager 脚手架:
    let dir_a = std::env::temp_dir().join(format!("e2e_push_to_a_{}", std::process::id()));
    let dir_b = std::env::temp_dir().join(format!("e2e_push_to_b_{}", std::process::id()));
    std::fs::create_dir_all(&dir_a).unwrap();
    std::fs::create_dir_all(&dir_b).unwrap();

    let (sm_a, ctx_a) = localtrans_core::test_support::setup_ctx(&dir_a);
    let (sm_b, ctx_b) = localtrans_core::test_support::setup_ctx(&dir_b);
    let fp_a = ctx_a.identity.fingerprint();
    let fp_b = ctx_b.identity.fingerprint();

    // 差异 1:乙 Ask 档
    {
        let mut trust = ctx_b.trust.lock().await;
        trust.entry(fp_a).or_default().perms.push = PushPolicy::Ask;
    }
    // 差异 2:1s 确认超时
    ctx_b.config.write().await.offer_timeout_secs = 1;
    { let mut trust = ctx_a.trust.lock().await; trust.entry(fp_b).or_default(); }

    let src = dir_a.join("e2e.txt");
    std::fs::write(&src, b"ask-timeout").unwrap();

    let b_addr = localtrans_core::test_support::start_listener(&sm_b).await;
    let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
    // 差异 3:ask 通道开着但不消费应答(Ask 事件入通道,永不应答)
    let (ask_tx_b, _ask_rx_b) = mpsc::channel(8);
    spawn_rpc_router(sm_b.clone(), ctx_b.clone(),
        Arc::new(localtrans_core::share::ShareRegistry::new(vec![])),
        ctrl_rx_b, ask_tx_b,
        localtrans_core::transfer::sender_state::new_sender_job_map(), None);
    let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
    let (ask_tx_a, _ask_rx_a) = mpsc::channel(8);
    spawn_rpc_router(sm_a.clone(), ctx_a.clone(),
        Arc::new(localtrans_core::share::ShareRegistry::new(vec![])),
        ctrl_rx_a, ask_tx_a,
        localtrans_core::transfer::sender_state::new_sender_job_map(), None);

    timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await.unwrap().unwrap();

    let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);
    // 核心断言:OfferTimeout(而非 OfferRejected/Timeout 泛化)
    let res = timeout(Duration::from_secs(15),
        push_files(&sm_a, &fp_b, vec![src], None, progress_tx)).await.unwrap();
    assert!(matches!(res, Err(EngineError::OfferTimeout)), "实际: {:?}", res.err());
}
```

（实现者注意：`setup_ctx`/`start_listener` 的真实签名以 e2e.rs 相邻用例与 `test_support` 现状为准——若相邻用例用的是别的构造方式，照相邻用例抄脚手架，只保住三处差异与最终断言。）

- [ ] **Step 2: 跑测试确认失败/通过**

Run: `cargo test -p localtrans-relay --test e2e push_ask_timeout`
Expected: 若脚手架对则直接 PASS（本用例是集成既有能力）；若 OfferTimeout 未正确传播则 FAIL——FAIL 时回到 Task 3 的映射逻辑排查

- [ ] **Step 3: 全量回归（双包）**

Run: `cargo test -p localtrans-core && cargo test -p localtrans && cargo test -p localtrans-relay && cd ui && npx vitest run`
Expected: 全绿（UI 3 个历史遗留红用例除外——v0.4.0 已记录在案）

- [ ] **Step 4: 文档**

README.md `:50-54` 推送段改为：

```markdown
**推送文件（主动推送）：**
1. 设备卡片菜单点【推送文件…】选文件；或设备页顶部【推送文件】向导（先选设备再选文件）；或直接拖放文件到设备卡片
2. 对方收到确认弹窗（默认 60s 倒计时，可在设置页调整 15-600s），可选 接收 / 另存到… / 拒绝；超时自动拒绝
3. 被拒绝/超时的任务在传输页可一键【重发】；"自动接受"档接收完成后弹系统通知
```

CHANGELOG.md 顶部加：

```markdown
## [未发布]

### 新增
- 推送双入口：设备卡片【推送文件…】+ 设备页推送向导（选设备→选文件→发送）；拖放保留
- 接收确认弹窗升级：可配置倒计时（默认 60s，15-600 可调）、总大小汇总、长列表折叠、"另存到…"自动顺延
- 拒绝原因传播：对方拒绝/超时在发送方任务上明确标出，拒绝/超时任务支持一键重发
- 自动接受档接收完成后系统通知（不无声被塞文件）
```

- [ ] **Step 5: 提交**

```bash
git add crates/localtrans-relay/tests/e2e.rs README.md CHANGELOG.md
git commit -m "test(e2e): 推送确认超时原因端到端用例 + docs 推送新流程"
```

---

## 执行注意事项（控制器必读）

1. **Task 1 会临时编译破坏 engine.rs**：Step 4 明确"补 `reason: None`/`reason: _` 让 check 过"——这是过渡脚手架，Task 3 3d 会替换 `:807` 处。审查 Task 1 时不要把这两行临时补丁当缺陷。
2. **Task 3 是全计划最高风险任务**（engine.rs 热路径 + tokio::select 语义）：实现者若对 `biased` 顺序或 oneshot `&mut` 在 select 中的用法不确定，控制器亲自验证编译再放行审查。
3. **Task 4 的 `notification()` extension trait 导入**以编译器指正为准（`tauri_plugin_notification::NotificationExt`），plan 中的 `use` 是近似。
4. **UI 测试 3 个历史红用例**（TransferItem "等待对端接收..."相关）是 v0.4.0 已记录的历史遗留，与本计划无关，禁止顺手修（改断言会污染任务 diff）。
5. **手动验证**（全部任务完成后、用户验收前）：双机实推——Ask 接受/拒绝/超时三分支、Auto 完成通知、另存顺延、向导与卡片双入口、拖放回归。
