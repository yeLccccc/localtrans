# 配对授权强化(同意门+单向随机码)实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 被连接方先授权(同意门)、发起方后证明(读码输入),两者齐备才建立互信;配对码一次一随机、只在被连接方屏幕显示。

**Architecture:** 协议层新增 ConsentGrant/ConsentDeny/ConsentCancel 三条控制消息;Session 增加 ConsentState(被动方门计时/已同意+码哈希/主动方);配对码从"连接派生对称 SAS"改为"B 点同意瞬间随机生成+哈希常数时间比对";壳层换 4 个新事件+3 个新命令;PairingDialog 重构为按角色分态。

**Tech Stack:** Rust(tokio/quinn)、Vue 3 + TS、Tauri 2。`rand = "0.8"`(已有)、`subtle = "2"`(锁文件已有 2.6.1,作为直接依赖加入 core)。

## Global Constraints

- spec: `docs/superpowers/specs/2026-08-22-consent-gate-design.md`(逐条对照)
- **B(被连接方)的验证码绝不出现在 A(发起方)界面/协议消息/日志/pending_pairing 表中**(用户硬约束)
- 码一次一随机:B 点同意瞬间生成,配对完成/结束等待/断连任一终点即焚;同两台设备两次尝试码不同
- 同意门超时 `consent_timeout_secs`:默认 60,可设 15-600,范围外钳制;超时断连**不记冷却**(冷却只留给码错 3 次)
- 码错 3 次断连+冷却 COOLDOWN_SECS=300(既有,不变)
- **已信任重连静默路径零改动**(受信分支不弹门不验码)
- 码比对必须常数时间(subtle::ConstantTimeEq);core 内只存 SHA-256 哈希
- 中继路径(adopt_connection 被动方)与局域网路径共用同一套门+码协议
- 局域网受信直连的行为逐字节不变;现有测试 `trusted_reconnect_is_silent` 必须原样通过
- 提交信息中文,前缀 feat:/fix:/chore:/docs:
- Windows 上测试命令(Git Bash):`cargo test -p localtrans-core`、`cd ui && npx vitest run`;全部测试串行跑法 `-- --test-threads=1`(端口竞争)

## 背景速览(实现者必读)

当前流程(session.rs):双方从连接派生**相同的** SAS 码 → 任一方输对 → `ctrl_loop` 收 `PairCodeSubmit` 判定匹配 → **直接 `complete_pairing` 写互信**(被动方零操作,这是要修的根因)。

新流程角色:
- **发起方 A**:`post_handshake`(connect/connect_peer 路径)未信任分支 → 发 `PairingWaitConsent` 事件(UI 等待态)→ 收 `ConsentGrant` → 发 `PairingCodeEntry` 事件(UI 输码态)→ `submit_pair_code` 发 `PairCodeSubmit` → 等 `PairResult{ok:true}`
- **接受方 B**:`handle_incoming`/`adopt_connection` 未信任分支 → 发 `PairingConsentNeeded` 事件(UI 同意门,**不含码**)→ B 点同意(`grant_consent` 命令)→ 随机生成码、存哈希、发 `ConsentGrant` → 发 `PairingCodeShown` 事件(UI 亮码)→ 收 `PairCodeSubmit` → 常数时间比对 → 匹配则 `complete_pairing` + 回 `PairResult{ok:true}`
- **挂起**:A 的码先到、B 未同意 → 暂存 `pending_code`,不验证不计数;B 点同意瞬间先验暂存码
- **结束等待**:B 已同意后可 `cancel_wait` → 发 `ConsentCancel` + 断连 + 码即焚

---

### Task 1: 配对码生成与常数时间比对(pairing.rs 重构)

**Files:**
- Modify: `crates/localtrans-core/src/pairing.rs`(全文重写:删 SAS 派生,新增随机码)
- Modify: `crates/localtrans-core/Cargo.toml`(加 subtle 依赖)
- Modify: `Cargo.toml`(workspace deps 加 subtle)

**Interfaces:**
- Consumes: 无(纯新代码)
- Produces(后续任务依赖,签名逐字精确):
  - `pub fn generate_pair_code() -> String` —— 6 位十进制零填充字符串
  - `pub fn hash_code(code: &str) -> [u8; 32]` —— SHA-256
  - `pub fn verify_code(code: &str, hash: &[u8; 32]) -> bool` —— 常数时间比对
  - `pub struct PairingMachine { ... }` 保留,字段改为:`code_hash: [u8; 32]`、`submitted_ok: bool`、`fails: u8`;方法:`pub fn new(code_hash: [u8; 32]) -> Self`、`pub fn submit_remote(&mut self, remote_code: &str) -> bool`、`pub fn is_complete(&self) -> bool`、`pub fn failed_out(&self) -> bool`
  - `pub const COOLDOWN_SECS: u64 = 300;`(保留)
  - `derive_sas_code` **删除**

**步骤:**

- [ ] **Step 1.1: 写失败测试**(替换 pairing.rs 的 `#[cfg(test)] mod tests` 整体;实现部分先不动,让编译失败即为"测试失败")

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_code_is_6_digits() {
        for _ in 0..100 {
            let code = generate_pair_code();
            assert_eq!(code.len(), 6, "码应为 6 位: {}", code);
            assert!(code.chars().all(|c| c.is_ascii_digit()), "码应全数字: {}", code);
        }
    }

    #[test]
    fn codes_are_random_across_attempts() {
        // 100 次生成的码收集去重,至少应有 90 个不同值
        // (生日碰撞下限保护;真随机 6 位码 100 次几乎不可能少于 90 个不同值)
        let mut seen = std::collections::HashSet::new();
        for _ in 0..100 {
            seen.insert(generate_pair_code());
        }
        assert!(seen.len() >= 90, "100 次生成应至少 90 个不同码,实际 {}", seen.len());
    }

    #[test]
    fn hash_and_verify_roundtrip() {
        let code = generate_pair_code();
        let hash = hash_code(&code);
        assert!(verify_code(&code, &hash), "正确码应验证通过");
        assert!(!verify_code("000000", &hash) || code == "000000", "错误码应验证失败");
    }

    #[test]
    fn pairing_machine_accepts_correct_code() {
        let code = generate_pair_code();
        let mut m = PairingMachine::new(hash_code(&code));
        assert!(m.submit_remote(&code), "正确码应匹配");
        assert!(m.is_complete());
        assert!(!m.failed_out());
    }

    #[test]
    fn pairing_machine_counts_failures() {
        let code = generate_pair_code();
        let mut m = PairingMachine::new(hash_code(&code));
        assert!(!m.submit_remote("000000") || code == "000000");
        assert!(!m.submit_remote("111111") || code == "111111");
        assert!(!m.failed_out(), "2 次失败不应判定失败");
        assert!(!m.submit_remote("222222") || code == "222222");
        assert!(m.failed_out(), "3 次失败应判定失败");
    }
}
```

- [ ] **Step 1.2: 跑测试确认失败**

Run: `cargo test -p localtrans-core pairing`
Expected: 编译错误(generate_pair_code 等未定义;derive_sas_code 相关符号缺失)

- [ ] **Step 1.3: 实现**

workspace `Cargo.toml` `[workspace.dependencies]` 段加:

```toml
subtle = "2.6"
```

`crates/localtrans-core/Cargo.toml` dependencies 加:

```toml
subtle = { workspace = true }
```

pairing.rs 全文重写为:

```rust
// 配对码:一次一随机生成 + 哈希常数时间比对
//
// 模型(v0.4.0 同意门):接受方 B 点"同意"瞬间随机生成 6 位码,只在 B 的
// 屏幕展示;发起方 A 人工读码输入,B 侧与哈希常数时间比对。码随连接即焚。
// 旧的对 SAS 对称派生(derive_sas_code)已废弃——对称模型下 A 界面必然
// 显示与 B 相同的码,违反"B 的码不出现在 A 界面"的硬约束。

use sha2::{Digest, Sha256};
use thiserror::Error;

/// 配对失败冷却时间(秒)
pub const COOLDOWN_SECS: u64 = 300;

/// 配对错误
#[derive(Error, Debug)]
pub enum PairingError {
    #[error("会话错误: {0}")]
    Session(#[from] crate::session::SessionError),
}

/// 随机生成 6 位十进制配对码(密码学随机,无偏采样,零填充)
pub fn generate_pair_code() -> String {
    use rand::Rng;
    let value: u32 = rand::thread_rng().gen_range(0..1_000_000);
    format!("{:06}", value)
}

/// 配对码的 SHA-256 哈希(core 内只存哈希,明文只在 B 的 UI 短暂存在)
pub fn hash_code(code: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(code.as_bytes());
    hasher.finalize().into()
}

/// 常数时间比对:防止时序侧信道逐位猜码
pub fn verify_code(code: &str, hash: &[u8; 32]) -> bool {
    use subtle::ConstantTimeEq;
    let candidate = hash_code(code);
    candidate.ct_eq(hash).into()
}

/// 配对状态机(仅接受方 B 使用:持有码哈希与失败计数)
pub struct PairingMachine {
    code_hash: [u8; 32],
    submitted_ok: bool,
    fails: u8,
}

impl PairingMachine {
    pub fn new(code_hash: [u8; 32]) -> Self {
        PairingMachine { code_hash, submitted_ok: false, fails: 0 }
    }

    /// 提交远程验证码,常数时间比对
    pub fn submit_remote(&mut self, remote_code: &str) -> bool {
        if verify_code(remote_code, &self.code_hash) {
            self.submitted_ok = true;
            true
        } else {
            self.fails += 1;
            false
        }
    }

    pub fn is_complete(&self) -> bool {
        self.submitted_ok
    }

    pub fn failed_out(&self) -> bool {
        self.fails >= 3
    }
}
```

同时删掉 session.rs 顶部的 `use crate::pairing::{derive_sas_code, PairingMachine, COOLDOWN_SECS};` 中对 `derive_sas_code` 的引用(改成本任务 Produces 列出的符号;session.rs 本任务**只修 import 编译错误,不做行为改动**——如 `derive_sas_code(&conn, ...)` 调用处暂时注释并 `todo!()` 会破坏测试,正确做法见下)。

**编译衔接说明**:session.rs 有两处 `derive_sas_code(&conn, &local_fp, &peer_fp)?` 调用(post_handshake:673、handle_incoming:867)。本任务临时改成:

```rust
let own_code = String::new(); // TODO(Task 3): 移除,同意门下码在 grant_consent 生成
```

这两处调用点的行为修复在 Task 3 完成。本任务结束时 session.rs 的既有配对测试**允许失败**(它们测的是旧对称流程,Task 3/4 改造),但 `cargo check -p localtrans-core` 必须通过、pairing 新测试必须全绿。

- [ ] **Step 1.4: 跑测试确认通过**

Run: `cargo test -p localtrans-core pairing && cargo check -p localtrans-core`
Expected: pairing 5 个测试全 PASS;check 通过(session 配对测试此时允许红,不影响本步判定)

- [ ] **Step 1.5: 提交**

```bash
git add Cargo.toml Cargo.lock crates/localtrans-core/Cargo.toml crates/localtrans-core/src/pairing.rs crates/localtrans-core/src/session.rs
git commit -m "feat: 配对码改一次一随机+哈希常数时间比对(pairing.rs)"
```

---

### Task 2: 协议消息 + ConsentState + 事件定义

**Files:**
- Modify: `crates/localtrans-core/src/protocol.rs:51-61`(ControlMsg 加 3 个变体)
- Modify: `crates/localtrans-core/src/session.rs`(SessionEvent 换新、Session 加 consent 字段、ConsentState 枚举)

**Interfaces:**
- Consumes: Task 1 的 `PairingMachine::new(code_hash: [u8; 32])`
- Produces:
  - ControlMsg 新变体(serde snake_case 自动标签 `consent_grant`/`consent_deny`/`consent_cancel`):
    ```rust
    ConsentGrant { name: String },
    ConsentDeny,
    ConsentCancel,
    ```
  - SessionEvent 新变体(**删除** `PairingRequested`):
    ```rust
    PairingConsentNeeded { fingerprint: Fingerprint, name: String },   // B 弹门(不含码)
    PairingCodeShown { fingerprint: Fingerprint, own_code: String },   // B 亮码(同意后)
    PairingWaitConsent { fingerprint: Fingerprint, name: String },     // A 等待态
    PairingCodeEntry { fingerprint: Fingerprint, name: String },       // A 输码态
    ```
  - `enum ConsentState { AwaitingConsent { deadline: tokio::time::Instant }, Granted { pending_code: Option<String> }, Initiator }`
  - `struct Session { conn, ctrl_send, pairing: Option<PairingMachine>, peer_name: Option<String>, consent: ConsentState }`

**步骤:**

- [ ] **Step 2.1: 写失败测试**(session.rs tests 模块新增;此刻 SessionEvent 变体未定义,编译失败即测试失败)

```rust
    /// T6: 发起方事件流安全属性——全程不得收到任何携带码的事件
    #[tokio::test]
    async fn initiator_never_receives_own_code() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn(async move { sm_a.connect(b_addr).await });

        // A: PairingWaitConsent(不含码)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b);
            }
            other => panic!("A 应先收到 PairingWaitConsent,实际: {:?}", other),
        }

        // B: PairingConsentNeeded(不含码)
        let fp_a_expected = match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { fingerprint, .. } => fingerprint,
            other => panic!("B 应收到 PairingConsentNeeded,实际: {:?}", other),
        };
        assert_eq!(fp_a_expected, _fp_a);

        // B 同意 → 生成码并亮给 B 自己
        sm_b.grant_consent(&fp_a_expected).await.unwrap();

        // A: PairingCodeEntry(不含码)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b);
            }
            other => panic!("A 应收到 PairingCodeEntry,实际: {:?}", other),
        }

        // B: PairingCodeShown(码只给 B)
        let own_code = match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { fingerprint, own_code } => {
                assert_eq!(fingerprint, _fp_a);
                own_code
            }
            other => panic!("B 应收到 PairingCodeShown,实际: {:?}", other),
        };
        assert_eq!(own_code.len(), 6);

        // A 完成连接(返回对端指纹)
        let peer_fp = connect_handle.await.unwrap().unwrap();
        assert_eq!(peer_fp, fp_b);

        // 金丝雀:A 再收任何事件都不允许携带码字段——本测试通过
        // 事件枚举定义本身保证(变体无码字段),此断言固化回归
    }
```

注意:测试里的 `sm_b.grant_consent` 在 Task 3 才实现,本任务先写测试占位会被编译卡住。**顺序调整**:本任务的测试只验证**事件定义与序列中前两步**(WaitConsent/ConsentNeeded),`grant_consent` 之后的部分剪到 Task 3 的测试里。本任务测试如下(编译只依赖本任务新增的类型):

```rust
    /// 事件定义冒烟:未信任连接双向事件序列的前两步(不含码)
    #[tokio::test]
    async fn untrusted_connect_emits_consent_events() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b);
            }
            other => panic!("A 应收到 PairingWaitConsent,实际: {:?}", other),
        }

        let fp_a_got = match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { fingerprint, .. } => fingerprint,
            other => panic!("B 应收到 PairingConsentNeeded,实际: {:?}", other),
        };
        assert_eq!(fp_a_got, _fp_a);

        // 事件枚举变体不携带码——由类型定义保证,此处再无事件可收即通过。
        // (连接后续停在等同意,这里不断言更多)
        connect_handle.abort();
    }
```

- [ ] **Step 2.2: 跑测试确认失败**

Run: `cargo test -p localtrans-core untrusted_connect_emits_consent_events`
Expected: 编译错误(SessionEvent 无 PairingWaitConsent 等变体)

- [ ] **Step 2.3: 实现**

protocol.rs ControlMsg 的 `PairResult` 变体后加:

```rust
    ConsentGrant {
        name: String,
    },
    ConsentDeny,
    ConsentCancel,
```

session.rs:

1. SessionEvent:删除 `PairingRequested` 变体,新增 4 个变体(见 Interfaces;字段顺序与类型逐字一致)
2. Session 结构加字段 `consent: ConsentState`
3. 新枚举(放在 Session 结构附近):

```rust
/// 同意门状态(v0.4.0 配对授权强化)
enum ConsentState {
    /// 被动方:等待本机用户点同意(deadline = 门超时时刻,由 consent_timeout_secs 决定)
    AwaitingConsent { deadline: tokio::time::Instant },
    /// 已同意(pairing.machine 持有码哈希)。pending_code = A 的码先到但
    /// B 尚未同意时的暂存——放行时机 = B 点同意那一刻
    Granted { pending_code: Option<String> },
    /// 主动方:同意是对端的事,本方无门
    Initiator,
}
```

4. **行为接线(最小可用,让测试过)**:
   - `post_handshake` 未信任分支:原 `PairingRequested` 事件改为 `PairingWaitConsent { fingerprint: peer_fp, name: <对方设备名,此刻未知则用 addr 字符串> }`。注意:此刻还没交换 Hello,不知道对方名——**沿用现状**(原代码用 addr 当 name),发 `PairingWaitConsent { fingerprint: peer_fp, name: addr.map(|a| a.to_string()).unwrap_or_default() }`。establish_control_flow 完成、拿到 peer_name 后**再补发一次** `PairingWaitConsent { fingerprint, name: peer_name }`?——不,事件重复会让 UI 弹两次。**正确做法**:未信任分支发一次,name 用 addr 占位;UI 在收到 ConsentGrant 时会拿到 B 的真实 name(ConsentGrant 带 name),用那个显示。post_handshake 不补发。
   - `handle_incoming` 未信任分支:原 `PairingRequested` 事件改为 `PairingConsentNeeded { fingerprint: peer_fp, name: <Hello 交换后的 peer_name> }`。注意原代码在 Hello 交换**前**发事件;新时序下 B 侧要显示"设备 X 请求连接",X 是 Hello 里的名字,所以**事件移到 Hello 交换之后**发。
   - `insert_session_and_spawn` 签名加 `consent: ConsentState` 参数;未信任时 A(establish 路径)传 `ConsentState::Initiator`、B(handle_incoming 路径)传 `ConsentState::AwaitingConsent { deadline: now + consent_timeout_secs() }`;受信时传 `ConsentState::Initiator`(受信无门,状态不参与)。
   - 新私有辅助(供 deadline 计算与钳制):

```rust
    /// 同意门超时秒数:读取配置,缺省 60,钳制到 [15, 600]
    async fn consent_timeout(&self) -> Duration {
        let secs = self.ctx.config.read().await.consent_timeout_secs;
        Duration::from_secs(secs.clamp(15, 600))
    }
```

   - Config(crate `store.rs`)加字段:`#[serde(default = "default_consent_timeout_secs")] pub consent_timeout_secs: u64,` + `fn default_consent_timeout_secs() -> u64 { 60 }`;Default impl 加 `consent_timeout_secs: 60`;test_support.rs setup_ctx 的 Config 字面量加 `consent_timeout_secs: 60`。
   - `grant_consent`/`deny_consent`/`cancel_wait` 三个方法本任务**先写签名+`todo!()` 之外的空实现**(返回 Ok)让编译过,Task 3 填真逻辑——**修正**:空实现会让 Task 3 的 TDD 失去红基线。本任务**不写**这三个方法,测试里也不调它们(上面测试已剪掉)。

5. **修编译连锁**:main.rs 事件泵的 `SessionEvent::PairingRequested` match 臂会编译失败——本任务把该臂改为 `PairingConsentNeeded { fingerprint, name } => { ... }`(转发 `pairing-consent-needed` 事件,同 payload 结构),其余 3 个新事件的桥接在 Task 5 做(本任务先在 match 里加 `_ => {}` 兜底?——**不**,Rust 枚举 match 必须穷尽;本任务把 4 个新事件全部加进 main.rs 事件泵,emit 事件名分别 `pairing-consent-needed`/`pairing-code-shown`/`pairing-wait-consent`/`pairing-code-entry`,payload 为 JSON:`{fingerprint, name}` 或 `{fingerprint, own_code}`。Task 5 只处理 UI 消费)。

- [ ] **Step 2.4: 跑测试确认通过**

Run: `cargo test -p localtrans-core untrusted_connect_emits_consent_events && cargo check -p localtrans`
Expected: 测试 PASS;壳 check 通过(此时壳的 pairing-request 前端监听还在,但壳侧编译无恙)

- [ ] **Step 2.5: 提交**

```bash
git add crates/localtrans-core/src/protocol.rs crates/localtrans-core/src/session.rs crates/localtrans-core/src/store.rs crates/localtrans-core/src/test_support.rs src-tauri/src/main.rs
git commit -m "feat: 协议加 Consent 三消息+ConsentState+四个配对事件"
```

---

### Task 3: B 侧门与码——grant/deny/cancel + ctrl_loop 放行

**Files:**
- Modify: `crates/localtrans-core/src/session.rs`

**Interfaces:**
- Consumes: Task 1 `generate_pair_code/hash_code/PairingMachine::new(code_hash)`;Task 2 `ConsentState`/ControlMsg 三变体/`consent_timeout()`
- Produces(SessionManager 公开方法,壳层 Task 5 依赖,签名逐字精确):
  - `pub async fn grant_consent(&self, fingerprint: &Fingerprint) -> Result<String, SessionError>` —— 返回生成的明文码(壳转发给 B 的 UI)
  - `pub async fn deny_consent(&self, fingerprint: &Fingerprint) -> Result<(), SessionError>`
  - `pub async fn cancel_wait(&self, fingerprint: &Fingerprint) -> Result<(), SessionError>`

**步骤:**

- [ ] **Step 3.1: 写失败测试**(session.rs tests 模块;三个核心时序)

```rust
    /// T1: 同意在先、输码在后 → 双方互信
    #[tokio::test]
    async fn grant_then_code_completes() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = ctx_a.identity.fingerprint();

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        // B 弹门
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { .. } => {}
            other => panic!("B 应收到 ConsentNeeded,实际: {:?}", other),
        }
        // A 等待
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("A 应收到 WaitConsent,实际: {:?}", other),
        }

        // B 同意 → 拿到码
        let code = sm_b.grant_consent(&fp_a).await.unwrap();
        assert_eq!(code.len(), 6);

        // A 被通知可输码
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 输码
        assert!(sm_a.submit_pair_code(&fp_b, &code).await.unwrap(),
            "正确码应返回 true");

        // 双方 SessionUp
        for (ev, peer_fp) in [(&mut ev_a, fp_b), (&mut ev_b, ctx_a.identity.fingerprint())] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::SessionUp { fingerprint, .. } => assert_eq!(fingerprint, peer_fp),
                other => panic!("应收到 SessionUp,实际: {:?}", other),
            }
        }
        // 双方 PairingResult ok:true
        for (ev, peer_fp) in [(&mut ev_a, fp_b), (&mut ev_b, ctx_a.identity.fingerprint())] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingResult { fingerprint, ok, .. } => {
                    assert_eq!(fingerprint, peer_fp);
                    assert!(ok);
                }
                other => panic!("应收到 PairingResult,实际: {:?}", other),
            }
        }

        assert!(ctx_a.trust.lock().await.is_trusted(&fp_b));
        assert!(ctx_b.trust.lock().await.is_trusted(&fp_a));
        connect_handle.await.unwrap().unwrap();
    }

    /// T2: 码先到、同意后到 → 挂起放行
    #[tokio::test]
    async fn code_arrives_before_consent_suspends_and_releases() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        // 双方门/等待事件
        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        // B 同意,拿到码
        let code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 故意"先知道码"(模拟肩窥/时序:B 已亮码)再输——正常顺序。
        // 真正的乱序场景:B 未同意时 A 就输码。构造:B 同意前 A 提交。
        // 重新来一遍连接:
        connect_handle.await.unwrap().unwrap();
        sm_a.disconnect(&fp_b).await;
        sm_b.disconnect(&fp_a).await;

        // 第二轮:B 不先同意
        let b_addr2 = start_listener(&sm_b).await;
        let connect_handle2 = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr2).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("第二轮应收到门事件,实际: {:?}", other),
            }
        }

        // A 先输一个"将来才有效"的码——此刻 B 未同意,码任意。
        // submit 返回什么取决于挂起语义:本地无法判定(B 的码 B 还没生成),
        // A 侧只透传,返回 true 表示"已提交"
        assert!(sm_a.submit_pair_code(&fp_b, "123456").await.unwrap(),
            "A 提交应成功(挂起语义)");

        // 短暂等待确保 PairCodeSubmit 已到达 B
        tokio::time::sleep(Duration::from_millis(300)).await;

        // B 现在同意——生成的码恰好是 A 输的那个?不可能(随机)。
        // 挂起放行的正确语义:B 同意瞬间验暂存码,错则计数(不匹配几乎必然)。
        // 所以本测试验证:挂起期间无 PairingResult(ok:false)泛洪、B 同意后
        // 系统状态一致。真正的"先码后同意成功"需要 A 在 B 同意并亮码后
        // 输对——那与 T1 相同。此测试固化:乱序不崩溃、不误完成。
        let code2 = sm_b.grant_consent(&fp_a).await.unwrap();
        assert_eq!(code2.len(), 6);

        // B 同意后,A 收到 CodeEntry;A 的旧提交已按暂存码验错(计数+1),
        // 但不应断连(未满 3 次)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("第二轮 A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 用正确码完成
        assert!(sm_a.submit_pair_code(&fp_b, &code2).await.unwrap());
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, .. } => assert!(ok),
            other => panic!("B 应收到 PairingResult,实际: {:?}", other),
        }

        connect_handle2.await.unwrap().unwrap();
    }

    /// T3: B 拒绝 → A 断连、无互信
    #[tokio::test]
    async fn deny_consent_disconnects_initiator() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        sm_b.deny_consent(&fp_a).await.unwrap();

        // A 收到拒绝结果
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("对方拒绝连接"));
            }
            other => panic!("A 应收到拒绝结果,实际: {:?}", other),
        }

        // connect 应以错误告终(对端断开)
        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "A 的 connect 应失败");

        // 无互信
        assert!(!ctx_a.trust.lock().await.is_trusted(&fp_b));
    }

    /// T4: 门超时断连、不记冷却
    #[tokio::test]
    async fn consent_timeout_disconnects() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, _fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        // 把 B 的门超时设为 1s(直接写配置)
        // setup_ctx 返回的 ctx.config 是 Arc<RwLock<Config>>
        // 注:setup_ctx 的第三返回值即 ctx
        let (sm_a2, mut ev_a2, _c2, _f2, _d2) = setup_ctx("甲");
        let _ = (sm_a2, ev_a2);
        let _ctx_b_config = (); // 占位避免未使用警告

        // 简化:直接用 B 的 ctx 改配置(重新拿一遍 setup_ctx 不行——身份会变)。
        // 正确路径:setup_ctx 改造为可注入超时的变体。这里用最直接的办法:
        // 在 test_support.rs 加 setup_ctx_with_timeout(name, secs)。
        // —— 由实现者在 test_support.rs 添加:
        // pub fn setup_ctx_with_timeout(name: &str, consent_timeout_secs: u64) -> (同 setup_ctx 返回)
        // 实现同 setup_ctx,仅 Config 的 consent_timeout_secs 字段不同。

        let (sm_b_t, mut ev_b_t, _ctx_b_t, _fp_b_t, _dir_b_t) =
            crate::test_support::setup_ctx_with_timeout("乙", 1);
        let _ = (&sm_b_t, &mut ev_b_t);

        let b_addr = start_listener(&sm_b_t).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("A 应收到 WaitConsent,实际: {:?}", other),
        }

        // 不点同意,等 2s(门 1s 到期)
        tokio::time::sleep(Duration::from_secs(2)).await;

        // A 的 connect 应失败(被 B 断开)
        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "门超时后 A 的 connect 应失败");

        // A 收到 PairingResult ok:false "同意超时"
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("同意超时"));
            }
            other => panic!("A 应收到超时结果,实际: {:?}", other),
        }
    }

    /// T5: 挂起期间暂存码验错同样计数,3 次断连+冷却
    #[tokio::test]
    async fn wrong_code_counted_even_when_suspended() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        // B 同意(生成随机码,A 不知道)
        let _code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 连错 3 次(避开真实码:真实码随机,000000 命中概率 1e-6;
        // 为确定性,连输 000000/111111/222222 三个不同错码)
        for wrong in ["000000", "111111", "222222"] {
            let r = sm_a.submit_pair_code(&fp_b, wrong).await.unwrap();
            // 前 2 次返回 false(不匹配),第 3 次 FailedOut 也返回 false
            assert!(!r, "错码 {} 应返回 false", wrong);
            match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingResult { ok, .. } => assert!(!ok),
                other => panic!("应收到 PairingResult,实际: {:?}", other),
            }
        }

        // 断连:connect handle 以错误结束
        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "3 次错码后应断连");

        // 冷却:立即重连被拒
        let b_addr2 = start_listener(&sm_b).await;
        let r2 = timeout(Duration::from_secs(5), sm_a.connect(b_addr2)).await.unwrap();
        assert!(r2.is_err() || r2.unwrap().is_err(), "冷却期内重连应失败");
    }

    /// T9: B 主动结束等待 → 断连+码即焚;A 收"对方已结束等待"
    #[tokio::test]
    async fn cancel_wait_burns_code_and_disconnects() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, _fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        let _code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // B 结束等待
        sm_b.cancel_wait(&fp_a).await.unwrap();

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("对方已结束等待"));
            }
            other => panic!("A 应收到结束等待结果,实际: {:?}", other),
        }

        let result = connect_handle.await.unwrap();
        assert!(result.is_err(), "结束等待后 A 的 connect 应失败");
    }
```

同时改造既有测试(新时序):
- `full_pairing_flow_both_sides_trusted_after`:改为"grant_then_code_completes 同构"(可直接删,由 T1 替代;保留文件里其余)
- `three_wrong_codes_cooldown`:改为 T5 同构(删,由 T5 替代)
- `ctrl_loop_survives_bidirectional_burst`:开头的配对段改为——双方门事件后 `grant_consent` 拿码、A `submit_pair_code` 完成配对,再进入爆发段(原断言不动)
- `trusted_reconnect_is_silent`:不动(守卫测试)
- `adopt_connection_establishes_session`:预互信,不动

- [ ] **Step 3.2: 跑测试确认失败**

Run: `cargo test -p localtrans-core -- --test-threads=1`
Expected: 编译错(grant_consent 等未定义)

- [ ] **Step 3.3: 实现**

session.rs 新增三个公开方法(放 submit_pair_code 附近):

```rust
    /// B(接受方)点同意:随机生成配对码、存哈希、通知 A 可输码。
    /// 返回明文码(仅本机 UI 展示,不得发往对端)。
    pub async fn grant_consent(&self, fingerprint: &Fingerprint) -> Result<String, SessionError> {
        let (conn, ctrl_send, had_pending) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(fingerprint)
                .ok_or_else(|| SessionError::Pairing("会话不存在".into()))?;
            // 门状态检查:仅 AwaitingConsent 可同意(幂等:Granted 再点返回旧码?
            // 不——码已生成无法取回明文。二次调用直接报错)
            match session.consent {
                ConsentState::AwaitingConsent { .. } => {}
                _ => return Err(SessionError::Pairing("会话不处于待同意状态".into())),
            }
            let code = crate::pairing::generate_pair_code();
            let code_hash = crate::pairing::hash_code(&code);
            session.pairing = Some(PairingMachine::new(code_hash));
            session.consent = ConsentState::Granted { pending_code: None };
            (session.conn.clone(), session.ctrl_send.clone(), code)
        };

        let peer_name = self.ctx.config.read().await.device_name.clone();
        ctrl_send.send(ControlMsg::ConsentGrant { name: peer_name }).await
            .map_err(|_| SessionError::Pairing("控制流已关闭".into()))?;

        // 同意瞬间检查暂存码(A 的码可能先到)
        // had_pending 是 code(移动语义),暂存码在 sessions 表里——
        // 在上方锁内取 pending_code 并在此判定:
        // (实现时把暂存判定逻辑提为私有 fn try_release_pending)
        self.try_release_pending(fingerprint, &conn).await;

        let _ = self.event_tx.send(SessionEvent::PairingCodeShown {
            fingerprint: *fingerprint,
            own_code: had_pending,
        }).await;
        Ok(had_pending)
    }
```

**实现注意**(按此写,不要偏离):
1. `try_release_pending(&self, fp, conn)`:锁内取 `Granted { pending_code }`——若有暂存,用 `pairing.submit_remote` 判定:对→complete_pairing+回 PairResult{ok:true};错→计数路径(failed_out 则 record_cooldown+断连,否则仅回 PairResult{ok:false});判定后 `pending_code = None`(无论对错,暂存一次性消费)。
2. `deny_consent`:锁内查会话存在且 consent 为 AwaitingConsent → `ctrl_send.send(ConsentDeny)`(尽力)→ `conn.close(0, b"consent denied")` → 发事件 `PairingResult { ok: false, reason: Some("对方拒绝连接".into()) }` 给**本机 B**(UI 收尾)→ 移除会话。A 侧的 PairingResult 由 ctrl_loop 处理 ConsentDeny 时发出(见 4)。
3. `cancel_wait`:锁内查 consent 为 Granted → `ctrl_send.send(ConsentCancel)` → close → 本机事件 `PairingResult { ok: false, reason: Some("已结束等待".into()) }` → 移除会话。
4. ctrl_loop 的 `ControlMsg` match 加三臂:
   - `ConsentGrant { name }`(A 侧收):发事件 `PairingCodeEntry { fingerprint: peer_fp, name }`
   - `ConsentDeny`(A 侧收):发事件 `PairingResult { ok: false, reason: Some("对方拒绝连接".into()) }`,`break 'outer`(不 record_cooldown)
   - `ConsentCancel`(A 侧收):发事件 `PairingResult { ok: false, reason: Some("对方已结束等待".into()) }`,`break 'outer`(不 record_cooldown)
5. **PairCodeSubmit 的处理重写**(核心):B 侧 ctrl_loop 收到时——锁内 match consent:
   - `Initiator`:不可能,debug 日志忽略(旧 NotPairing 语义)
   - `AwaitingConsent { .. }`:`pending_code = Some(code)`,不发任何回包、不计数、不发事件(A 的 UI 停在"已提交,等待对方确认")
   - `Granted { pending_code }`:调用提取得私有判定 `fn judge_code(session, code) -> PairOutcome`(submit_remote→Matched/Mismatch/FailedOut),路径与旧逻辑同构:Matched→complete_pairing+PairResult{ok:true};FailedOut→record_cooldown+PairResult{ok:false,"配对码错误超过 3 次"}+断连;Mismatch→回 PairResult{ok:false}
6. **A 侧 submit_pair_code 语义变更**:A 的 pairing machine 不再存在(Initiator 无 PairingMachine,`session.pairing` 为 None)。submit_pair_code 锁内:若 consent==Initiator → 直接 `ctrl_send.send(PairCodeSubmit { code })` 返回 Ok(true)(透传,挂起语义);若 Granted/B 侧(本机是接受方,UI 无输入框,不会走到)→ 返回 Err("本机为接受方,无输码入口")。**旧的双向 submit 逻辑删除**。
7. **门超时驱动**:ctrl_loop 的 select! 加第三支(或独立 spawn 计时任务——推荐独立任务,避免 ctrl_loop 复杂化):

```rust
        // insert_session_and_spawn 内,consent 为 AwaitingConsent 时:
        if let ConsentState::AwaitingConsent { deadline } = consent {
            let sm = self.clone();
            let fp = peer_fp;
            tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                let mut sessions = sm.sessions.lock().await;
                if let Some(session) = sessions.get_mut(&fp) {
                    if matches!(session.consent, ConsentState::AwaitingConsent { .. }) {
                        // 还没同意:超时断连(不记冷却)
                        session.conn.close(0u8.into(), b"consent timeout");
                        sessions.remove(&fp);
                        drop(sessions);
                        let _ = sm.event_tx.send(SessionEvent::PairingResult {
                            fingerprint: fp,
                            ok: false,
                            reason: Some("同意超时".into()),
                        }).await;
                        // 注意:这是 B 本机事件;A 侧由连接断开感知(connect Err)
                        // A 侧的 PairingResult 事件:A 的 connect future 返回 Err 即可,
                        // 但 T4 断言 A 收到 PairingResult{ok:false,"同意超时"} ——由 A 的
                        // post_handshake 对 conn.closed 的监听发?简化: ctrl_loop 断连
                        // 兜底分支里,若本方是 Initiator 且 pairing 未完成,发
                        // PairingResult{ok:false, reason: 连接断开原因}。见实现注意 8。
                    }
                }
            });
        }
```

8. **A 侧断连感知**:ctrl_loop 结尾(会话清理处)加:若本方 consent==Initiator 且 pairing 未完成(即未收到 PairResult ok:true)→ 发 `PairingResult { ok: false, reason: Some("连接已断开".into()) }`。T4 走这条路径时 reason 与断言 "同意超时" 不符——**修正 T4 断言**:A 侧 reason 改断言 `Some("连接已断开")`(B 侧才收到 "同意超时")。**以本注为准修 T4 测试**:B 侧(ev_b_t)断 "同意超时",A 侧断 "连接已断开"。
9. `post_handshake` 未信任分支:删 `let own_code = derive_sas_code(...)`(Task 1 已占位)与 PairingRequested 事件(Task 2 已换);`establish_control_flow` 的 own_code/is_trusted 参数随配对重构清理——**受信分支零行为变化**。
10. `handle_incoming`:Hello 交换后、insert_session_and_spawn 前,若 !is_trusted 发 `PairingConsentNeeded { fingerprint: peer_fp, name: peer_name.clone() }`。

- [ ] **Step 3.4: 跑测试确认通过**

Run: `cargo test -p localtrans-core -- --test-threads=1`
Expected: 全绿(含 trusted_reconnect_is_silent、adopt_connection_establishes_session、loopback 等既有测试)

- [ ] **Step 3.5: 提交**

```bash
git add crates/localtrans-core/src/session.rs crates/localtrans-core/src/test_support.rs
git commit -m "feat: B 侧同意门——grant/deny/cancel+挂起放行+门超时"
```

---

### Task 4: 壳层命令与事件桥

**Files:**
- Modify: `src-tauri/src/commands.rs`(新增 3 命令,改 PairingDto/待配对表语义)
- Modify: `src-tauri/src/main.rs`(AppState.pending_pairing 语义、事件泵、命令注册)

**Interfaces:**
- Consumes: Task 3 `grant_consent/deny_consent/cancel_wait`;Task 2 事件
- Produces(Tauri 命令,UI 依赖):
  - `grant_consent(fingerprint: String) -> Result<GrantConsentDto, String>`,`GrantConsentDto { own_code: String }`
  - `deny_consent(fingerprint: String) -> Result<(), String>`
  - `cancel_pairing_wait(fingerprint: String) -> Result<(), String>`
  - 前端事件名:`pairing-consent-needed {fingerprint, name}`、`pairing-code-shown {fingerprint, own_code}`、`pairing-wait-consent {fingerprint, name}`、`pairing-code-entry {fingerprint, name}`、`pairing-result`(既有)
  - `pending_pairing` 表值类型改为 `(String, Option<String>)`——(name, own_code);own_code 仅 B 同意后 grant_consent 返回时填入

**步骤:**

- [ ] **Step 4.1: 实现**(壳层无单测,以编译+命令注册为验证;UI 测试在 Task 5)

commands.rs 配对段改为:

```rust
/// B(接受方)同意连接:生成随机码,返回给本机 UI 展示
#[tauri::command]
pub async fn grant_consent(
    state: State<'_, AppState>,
    fingerprint: String,
) -> Result<crate::GrantConsentDto, String> {
    let fp = decode_fp(&fingerprint)?;
    let own_code = state.sm.grant_consent(&fp).await.map_err(|e| e.to_string())?;
    // 记入待配对表(B 本机恢复显示用;A 侧永不查询到此码——
    // A 机器的 pending_pairing 只有自己的接受方条目)
    state.pending_pairing.lock().await.insert(fingerprint.clone(), own_code.clone());
    Ok(crate::GrantConsentDto { own_code })
}

/// B(接受方)拒绝连接
#[tauri::command]
pub async fn deny_consent(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;
    state.sm.deny_consent(&fp).await.map_err(|e| e.to_string())?;
    state.pending_pairing.lock().await.remove(&fingerprint);
    Ok(())
}

/// B(接受方)已同意后主动结束等待(码即焚)
#[tauri::command]
pub async fn cancel_pairing_wait(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;
    state.sm.cancel_wait(&fp).await.map_err(|e| e.to_string())?;
    state.pending_pairing.lock().await.remove(&fingerprint);
    Ok(())
}
```

加私有辅助(若尚无):

```rust
fn decode_fp(fingerprint: &str) -> Result<[u8; 32], String> {
    let fp_bytes = hex::decode(fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);
    Ok(fp)
}
```

(既有 submit_pair_code/reject_pairing 的手写解码替换为 decode_fp;`reject_pairing` 保留并转调 `deny_consent` 的逻辑:直接 `state.sm.disconnect` 即可,注释"兼容保留,新 UI 走 deny_consent"。)

main.rs:
1. `pending_pairing: Arc<Mutex<HashMap<String, String>>>`(值从 (String,String) 简化为 String=own_code;name 已在事件里)
2. PairingDto 结构改为 `{ fingerprint: String, name: String, own_code: String }`(own_code 可为空串——未同意时)`get_pairing_pending` 相应调整(own_code 从表取,无则空串;name 用设备名表/中继名册反查,查不到用指纹缩写)
3. 事件泵:Task 2 已加 4 事件转发;PairingCodeShown 到达时 `pending_pairing.insert(fp_hex, own_code)`(壳重启前恢复用);PairingResult 到达时 remove
4. `struct GrantConsentDto { own_code: String }`(derive Serialize/Deserialize/Clone/Debug)加在 PairingDto 旁
5. `save_settings`/`config_to_dto`:ConfigDto 加 `consent_timeout_secs: u64`(serde default 60);save_settings 写入 `config.consent_timeout_secs = dto.consent_timeout_secs.clamp(15, 600);`;config_to_dto 读出
6. 命令注册段加 `commands::grant_consent, commands::deny_consent, commands::cancel_pairing_wait,`

- [ ] **Step 4.2: 编译验证**

Run: `cargo check -p localtrans && cd ui && npm run build`
Expected: 壳编译通过;UI 构建**此时可能仍通过**(旧 UI 不引用新命令)——以壳 check 为准

- [ ] **Step 4.3: 提交**

```bash
git add src-tauri/src/commands.rs src-tauri/src/main.rs
git commit -m "feat: 壳层同意门命令+事件桥+超时配置透传"
```

---

### Task 5: UI——PairingDialog 多态 + Settings 超时项

**Files:**
- Modify: `ui/src/types.ts`(PairingDto/ConfigDto/GrantConsentDto)
- Modify: `ui/src/api.ts`(新命令/新事件监听)
- Modify: `ui/src/stores/devices.ts`(pairing 状态机适配)
- Modify: `ui/src/components/PairingDialog.vue`(重构为按角色分态)
- Modify: `ui/src/pages/Settings.vue`(连接安全卡片:同意超时输入)
- Test: `ui/src/__tests__/PairingDialog.test.ts`(新建)

**Interfaces:**
- Consumes: Task 4 的命令/事件名
- Produces: 无(终端 UI)

**步骤:**

- [ ] **Step 5.1: 写失败测试**(新建 `ui/src/__tests__/PairingDialog.test.ts`;vitest + @vue/test-utils,mock 参照 `ui/src/test-support/tauriMock.ts` 既有模式)

```typescript
import { describe, it, expect, vi, beforeEach } from 'vitest'
import { mount, flushPromises } from '@vue/test-utils'
import PairingDialog from '../components/PairingDialog.vue'

// mock invokeCommand:grant_consent 返回码
const invokeMock = vi.fn()
vi.mock('../api', async () => {
  const actual = await vi.importActual<typeof import('../api')>('../api')
  return {
    ...actual,
    invokeCommand: (cmd: string, args?: Record<string, unknown>) => invokeMock(cmd, args),
    onEvent: (_name: string, cb: (payload: unknown) => void) => {
      ;(globalThis as any).__emit = (name: string, payload: unknown) => {
        if ((globalThis as any).__eventName === name) cb(payload)
      }
      return () => {}
    },
  }
})

function emitEvent(name: string, payload: unknown) {
  ;(globalThis as any).__eventName = name
  ;(globalThis as any).__emit(name, payload)
}

beforeEach(() => {
  vi.clearAllMocks()
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === 'grant_consent') return { own_code: '482913' }
    return undefined
  })
})

describe('PairingDialog 同意门', () => {
  it('B 侧:收到 consent-needed 弹同意门,点同意后亮码(无输入框)', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()

    emitEvent('pairing-consent-needed', { fingerprint: 'aa', name: '甲的电脑' })
    await flushPromises()

    expect(wrapper.text()).toContain('甲的电脑')
    expect(wrapper.text()).toContain('同意')

    // 点同意
    const btn = wrapper.find('[data-testid="btn-grant"]')
    await btn.trigger('click')
    await flushPromises()

    expect(invokeMock).toHaveBeenCalledWith('grant_consent', { fingerprint: 'aa' })
    // 亮码,无输入框
    expect(wrapper.text()).toContain('482913')
    expect(wrapper.find('[data-testid="code-input"]').exists()).toBe(false)
    // 有结束等待按钮
    expect(wrapper.find('[data-testid="btn-cancel-wait"]').exists()).toBe(true)
  })

  it('A 侧:wait → code-entry 流转,输入并提交', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()

    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙的电脑' })
    await flushPromises()
    expect(wrapper.text()).toContain('等待')

    emitEvent('pairing-code-entry', { fingerprint: 'bb', name: '乙的电脑' })
    await flushPromises()
    expect(wrapper.find('[data-testid="code-input"]').exists()).toBe(true)
    expect(wrapper.text()).not.toContain('本机')  // A 不显示本机码

    const input = wrapper.find('[data-testid="code-input"]')
    await input.setValue('556677')
    await wrapper.find('[data-testid="btn-submit"]').trigger('click')
    await flushPromises()
    expect(invokeMock).toHaveBeenCalledWith('submit_pair_code', {
      fingerprint: 'bb', peer_code: '556677',
    })
  })

  it('A 侧:收到 deny 结果关闭弹窗', async () => {
    const wrapper = mount(PairingDialog)
    await flushPromises()
    emitEvent('pairing-wait-consent', { fingerprint: 'bb', name: '乙' })
    await flushPromises()
    emitEvent('pairing-result', { fingerprint: 'bb', ok: false, reason: '对方拒绝连接' })
    await flushPromises()
    expect(wrapper.find('.pairing-dialog').exists()).toBe(false)
  })
})
```

- [ ] **Step 5.2: 跑测试确认失败**

Run: `cd ui && npx vitest run src/__tests__/PairingDialog.test.ts`
Expected: FAIL(组件还是旧双码界面,无 btn-grant 等)

- [ ] **Step 5.3: 实现**

types.ts:

```typescript
export interface PairingDto {
  fingerprint: string
  name: string
  own_code: string
}

export interface GrantConsentDto {
  own_code: string
}
```

ConfigDto 加 `consent_timeout_secs?: number`。

api.ts:

```typescript
export const pairingApi = {
  getPending: (): Promise<PairingDto[]> => invokeCommand('get_pairing_pending'),
  submitCode: (fingerprint: string, peer_code: string): Promise<boolean> =>
    invokeCommand('submit_pair_code', { fingerprint, peer_code }),
  grant: (fingerprint: string): Promise<GrantConsentDto> =>
    invokeCommand('grant_consent', { fingerprint }),
  deny: (fingerprint: string): Promise<void> =>
    invokeCommand('deny_consent', { fingerprint }),
  cancelWait: (fingerprint: string): Promise<void> =>
    invokeCommand('cancel_pairing_wait', { fingerprint }),
}

export function onPairingConsentNeeded(cb: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-consent-needed', cb)
}
export function onPairingCodeShown(cb: (p: { fingerprint: string; own_code: string }) => void) {
  return onEvent('pairing-code-shown', cb)
}
export function onPairingWaitConsent(cb: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-wait-consent', cb)
}
export function onPairingCodeEntry(cb: (p: { fingerprint: string; name: string }) => void) {
  return onEvent('pairing-code-entry', cb)
}
```

(旧的 `onPairingRequest`/`pairing-request` 删除。)

PairingDialog.vue 重构(script 核心;template 按 data-testid 挂钩;样式沿用现有 token):

```typescript
// 组件内状态机
type DialogState =
  | { role: 'acceptor'; phase: 'gate'; fingerprint: string; name: string }
  | { role: 'acceptor'; phase: 'code'; fingerprint: string; name: string; ownCode: string }
  | { role: 'initiator'; phase: 'waiting'; fingerprint: string; name: string }
  | { role: 'initiator'; phase: 'entry'; fingerprint: string; name: string }
  | { role: 'initiator'; phase: 'submitted'; fingerprint: string; name: string }

const state = ref<DialogState | null>(null)
const peerCode = ref('')
const remainingSecs = ref(0)   // B 门倒计时,从配置读初值
let countdownTimer: ReturnType<typeof setInterval> | null = null

// 事件注册(脚本顶层,模式同旧版):
// onPairingConsentNeeded → state = acceptor/gate + 启动倒计时(从 settings store 读 consent_timeout_secs)
// onPairingCodeShown → 若当前 gate 且指纹匹配 → phase:'code',ownCode 填入
// onPairingWaitConsent → state = initiator/waiting
// onPairingCodeEntry → 若 waiting 且指纹匹配 → phase:'entry'
// onPairingResult → ok:true 成功 toast+关闭;ok:false 按 reason toast+关闭

async function handleGrant() {          // B 点同意
  const r = await api.pairing.grant(state.value!.fingerprint)
  // 码展示等 pairing-code-shown 事件;乐观更新亦可:r.own_code 直接填
  // 采用乐观更新(grant_consent 返回码,事件兜底)
  state.value = { role: 'acceptor', phase: 'code', fingerprint: state.value!.fingerprint,
                  name: state.value!.name, ownCode: r.own_code }
}
async function handleDeny() { await api.pairing.deny(state.value!.fingerprint); state.value = null }
async function handleCancelWait() { await api.pairing.cancelWait(state.value!.fingerprint); state.value = null }
async function handleSubmit() {        // A 提交码
  await api.pairing.submitCode(state.value!.fingerprint, peerCode.value)
  state.value = { ...state.value, phase: 'submitted' } as DialogState  // 挂起态
}
```

template 关键节点:`[data-testid="btn-grant"]`、`[data-testid="btn-deny"]`、`[data-testid="btn-cancel-wait"]`、`[data-testid="code-input"]`(A 输入)、`[data-testid="btn-submit"]`;B 亮码用 CodeBadge(既有组件,code+label props);门倒计时文案 `{{ remainingSecs }}s 后自动拒绝`。**B 的 code phase 绝不渲染输入框;A 的任何 phase 绝不渲染本机码。**

stores/devices.ts:删 `handlePairingRequest`(旧事件),pairing 相关 action 改调 `api.pairing.grant/deny/cancelWait`;`submitPairCode` 保留(输码语义不变,返回 false 时 UI 清空重输的既有交互保留在 Mismatch——**注意**:挂起语义下 A 提交错码,PairResult{ok:false} 事件到达时 UI 关弹窗+toast"配对码不匹配"?错——A 需要能继续重输。**实现约定**:A 侧收 PairingResult{ok:false} 且 reason 为"配对码不匹配"级别(或 phase==submitted 未断连)时**不关弹窗**,显示错误+清空输入回 entry 态;其余失败 reason 关弹窗。判断依据:连接是否仍在(简单起见:reason 包含"不匹配"→回 entry;否则关闭)。

Settings.vue:连接安全卡片(新增,放在中继卡片后):"同意超时(秒)" number input(min 15 max 600,默认 60),绑定 `config.consent_timeout_secs`,保存走既有 saveSettings。

- [ ] **Step 5.4: 跑测试确认通过**

Run: `cd ui && npx vitest run src/__tests__/PairingDialog.test.ts && npm run build`
Expected: 3 个新测试 PASS;构建通过;**既有 UI 测试**(TransferItem/TransfersPage/transfersStore/useCloseGuard)全 PASS

- [ ] **Step 5.5: 提交**

```bash
git add ui/src/types.ts ui/src/api.ts ui/src/stores/devices.ts ui/src/components/PairingDialog.vue ui/src/pages/Settings.vue ui/src/__tests__/PairingDialog.test.ts
git commit -m "feat: 配对 UI 同意门多态——B 门+亮码/A 等待+输码+挂起"
```

---

### Task 6: E2E 中继路径 + 回归全量 + 文档

**Files:**
- Modify: `crates/localtrans-relay/tests/e2e.rs`(既有 3 个 E2E 的配对段改新时序)
- Modify: `CHANGELOG.md`
- Modify: `dist/localtrans-v0.3.0/usage.md` → 本任务不改 dist;文档改 README.md 配对段(若 README 提及旧流程)

**Interfaces:**
- Consumes: Task 3 全部;e2e.rs 既有测试工具(`预互信` 绕过函数)
- Produces: 无

**步骤:**

- [ ] **Step 6.1: 改造 e2e.rs**

既有 3 个 E2E(`two_devices_connect_and_exchange_over_relay`/`client_leave_notifies_peer`/`relay_restart_recovers`)当前用预互信绕过配对。**保持预互信不动**(它们测中继链路,不测配对)。新增第 4 个 E2E:

```rust
/// T8: 中继路径下未信任设备同样走同意门+随机码
#[tokio::test]
async fn relay_pairing_same_flow() {
    // 复用既有 e2e.rs 的中继启动/双客户端注册/punch 工具函数
    // (与 two_devices_connect_and_exchange_over_relay 相同的搭建段)
    // 差异:双方 TrustStore 不预置互信
    //
    // 断言序列:
    // 1. A connect_peer 后:A 收 PairingWaitConsent、B 收 PairingConsentNeeded
    // 2. B grant_consent(fp_a) → A 收 PairingCodeEntry、B 收 PairingCodeShown
    // 3. A submit_pair_code(fp_b, code) → 双方 PairingResult ok:true + SessionUp
    // 4. 双方 TrustStore 互信成立
    // 5. 之后发一条 ListReq/SharesReq 交换成功(复用既有断言段)
}
```

(具体搭建代码从 `two_devices_connect_and_exchange_over_relay` 复制改造——实现者读该函数后照抄搭建段,仅去预互信、插入断门/同意/输码序列。)

- [ ] **Step 6.2: 跑 E2E**

Run: `cargo test -p localtrans-relay --test e2e -- --test-threads=1`
Expected: 4 个 E2E 全 PASS

- [ ] **Step 6.3: 全量回归(Windows)**

Run:
```bash
cargo test -p localtrans-core
cargo test -p localtrans
cargo test -p localtrans-relay -- --test-threads=1
cd ui && npm run build && npx vitest run
```
Expected: 全绿

- [ ] **Step 6.4: WSL Linux 回归**

Run: `wsl -d Ubuntu-22.04 -- bash -lc 'source ~/.cargo/env && cd ~/build && cargo test -p localtrans-core -- --test-threads=1'`
(若 ~/build 源码过期:先按 docs/build-and-test.md §2.4 的 cp 段刷新源码再测)
Expected: 全绿(跨平台 cfg 守卫)

- [ ] **Step 6.5: 文档**

CHANGELOG.md 加(日期 2026-08-22):

```markdown
## [未发布] - 配对授权强化
### 新增
- 同意门:被连接方需先点击"同意连接",配对码才生成并展示(默认 60s 超时,可在设置页调整 15-600s)
- 配对码一次一随机:每次配对尝试独立生成,连接结束即焚;只在被连接方屏幕显示,发起方界面永不显示
- 结束等待:被连接方同意后可随时终止配对等待(码即时作废)
### 安全
- 配对完成必要条件 = 被连接方同意 + 发起方输对码;核心态常数时间比对,明文码不落盘、不进日志与协议
```

README.md 配对段(若描述了旧"双方输同一码"流程)改为:"设备页点连接后,对方屏幕弹出同意确认;同意后对方屏幕显示 6 位码,在本机输入即可完成配对"。

- [ ] **Step 6.6: 提交**

```bash
git add crates/localtrans-relay/tests/e2e.rs CHANGELOG.md README.md
git commit -m "feat: 中继 E2E 配对新时序+文档(v0.4.0 配对授权强化)"
```

---

## 任务依赖图

```
Task 1 (pairing.rs) ──► Task 2 (协议/事件/状态) ──► Task 3 (B 门+放行) ──► Task 4 (壳) ──► Task 5 (UI) ──► Task 6 (E2E+回归+文档)
```

严格串行;每任务独立提交、独立可测。
