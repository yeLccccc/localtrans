# 配对授权强化(同意门 + 单向展示码)设计

日期:2026-08-22
状态:已与用户逐节确认

## 1. 背景与问题

当前配对是 SAS 双向验码模型:双方从同一加密连接派生**相同的 6 位码**,任意一台输入正确 → 双方同时完成互信。被动方(被连接的 B)的 `ctrl_loop` 收到 `PairCodeSubmit` 判定匹配后**直接 `complete_pairing` 写互信**(session.rs),B 的用户全程无需任何操作——配对框被事件自动关闭。

问题:**B 的主人没有授权过这次信任**。任何知道码的人(含肩窥者)发起连接并输对码,B 就被动互信了。

## 2. 目标模型

**被连接方先授权(同意门),发起方后证明(读码输入),两者齐备才建立互信。**

| 角色 | 判定时机 | 界面 | 职责 |
|---|---|---|---|
| 接受方 B(被动) | `handle_incoming`/`adopt_connection` 收到连接 | ①同意/拒绝门 → ②纯展示码 | 授权 + 亮码 |
| 发起方 A(主动) | `connect`/`connect_peer`(中继) | 不显示码,只有输入框 | 读 B 屏幕的码并输入 |

核心规则(全部落在 core 协议层):

1. **未同意不亮码**:B 的码只在 B 点"同意"后对其本机 UI 可见,对 A 永不可见
2. **同意是必要条件**:A 输对码但 B 未同意 → A 挂起"等待对方确认";B 点同意瞬间放行
3. **码是充分证明**:B 已同意 + A 输对码 → 立即完成配对,B 无需再操作
4. **已信任重连静默**:受信路径零改动,不弹门不验码
5. **拒绝/超时/3 错**:拒绝→断连;门超时(默认 60s,可配 15-600s)未点→断连;码错 3 次→断连+冷却 5min(既有冷却机制不变)
6. **码一次一随机**:配对码不再从连接派生(B 点"同意"瞬间本地密码学随机生成 6 位),生命周期 = 同意→配对完成/结束等待/断连,任一终点即焚;同两台设备每次配对尝试码都不同,旧码作废后不可重放
7. **B 可主动结束等待**:B 已同意、亮码等 A 输入期间,B 可随时点"结束等待"→ 断连 + 当前码即焚;A 侧收到"对方已结束等待"提示

用户硬约束:**B(被连接方)的验证码绝不出现在 A(发起方)界面上**。推论:B 侧验码输入框取消(无码可读),验码纯单向(A 读 B 的码)。B 的安全由同意门保障。

中继场景同规则:远程设备 punch 后同样分发起(`connect_peer`)/接受(`adopt_connection`)角色,与局域网共用同一套门+码协议。

## 3. 协议变更

`ControlMsg` 新增两条(protocol.rs):

```rust
/// 接受方点击"同意连接"后发给发起方
ConsentGrant {
    /// 设备名,便于 A 核对在输谁家的码
    name: String,
}

/// 接受方点击"拒绝"后发给发起方
ConsentDeny,

/// 接受方同意后、等待验码期间主动结束等待
ConsentCancel,
```

未发布过,直接改协议,不留旧路径、不做版本协商。

## 4. 状态机

### 4.1 Session 结构扩展

```rust
struct Session {
    conn: Connection,
    ctrl_send: mpsc::Sender<ControlMsg>,
    pairing: Option<PairingMachine>,   // 既有:码验证
    peer_name: Option<String>,
    consent: ConsentState,             // 新增
}

enum ConsentState {
    /// 被动方:等待本机用户点同意(门计时从此起算)
    AwaitingConsent { deadline: Instant },
    /// 已同意。code_hash = B 点同意瞬间随机生成码的 SHA-256;
    /// pending_code = A 的码先到但尚未验证的暂存
    Granted { code_hash: [u8; 32], pending_code: Option<String> },
    /// 主动方:同意是对端的事,本方无门
    Initiator,
}
```

### 4.2 A 侧(发起方)流程

```
connect 完成(mTLS 握手 + Hello 交换)
    ↓
UI 事件: PairingWaitConsent { fingerprint, peer_name }   ← 新事件,替代原 PairingRequested
界面: "等待对方同意连接…"(无码、无输入框)
    ↓ 收到 ConsentGrant
UI 事件: PairingCodeEntry { fingerprint, peer_name }      ← 新事件
界面: 亮出输入框 "请输入对方屏幕上显示的 6 位码"
    ↓ A 输码 → submit_pair_code(不变)
    ├─ 码错 → PairResult{ok:false},计数+1(3 次断连冷却,不变)
    ├─ 码对 + B 已同意 → PairResult{ok:true} → 双方 complete_pairing
    └─ 码对 + B 未同意 → 挂起(A UI 停在"已提交,等待对方确认…")
    ↓ 收到 ConsentDeny
断连 + 提示"对方拒绝连接"
```

**关键行为差异**:A 本地判定通过后**不再立即自动完成**——必须等 B 的 `PairResult{ok:true}` 回包。

### 4.3 B 侧(接受方)流程

```
收到连接(mTLS + Hello)
    ↓
UI 事件: PairingConsentNeeded { fingerprint, peer_name }  ← 新事件(不含码!)
界面: "设备 X 请求连接" [同意] [拒绝]
    ├─ 点同意 → 本地密码学随机生成 6 位码 + 发 ConsentGrant
    │      → UI 事件 PairingCodeShown { own_code } ← 第二个新事件,此刻才亮码
    │      → 界面切换为大号展示码(无输入框)+ [结束等待] 按钮
    │      ↓ 等 A 的 PairCodeSubmit
    │      ├─ 码对(无论先到后到) → complete_pairing + PairResult{ok:true}
    │      └─ 码错 → PairResult{ok:false} 计数,3 次断连冷却
    │      B 点[结束等待] → 断连 + 码即焚 + A 侧提示"对方已结束等待"
    └─ 点拒绝 → 发 ConsentDeny + 断连
门超时(默认 60s,可配)未点 → 断连
```

**码的生成与比对**:B 点同意瞬间用 `rand` 生成 6 位随机数(`UniformInteger` 无偏采样);core 内以 SHA-256 哈希形态存储,UI 展示明文经壳事件下发;A 的 `PairCodeSubmit` 到达后与哈希**常数时间比对**(subtle crate)。码只在 B 的 UI 短暂存在明文,不出现在任何协议消息、日志、`pending_pairing` 表中。

### 4.3.1 B 主动结束等待

B 已同意、亮码等 A 输入期间,任何时候可点"结束等待"(新壳命令 `cancel_pairing_wait` → core 方法 `cancel_wait`):

```
发 ControlMsg::ConsentCancel(新消息)→ 断连 → 码即焚
A 侧收到 ConsentCancel → 提示"对方已结束等待",关闭输码界面
不记冷却(与门超时同理:正常防御,非攻击)
```

### 4.4 挂起放行机制(ctrl_loop 收到 PairCodeSubmit)

```
收到 PairCodeSubmit { code }
    ├─ 本方是 Initiator → 不可能,防御性忽略
    ├─ AwaitingConsent(B 未同意):
    │      码先到。不验证、不拒绝、不计数——暂存进 pending_code 挂起
    ├─ Granted(B 已同意):
    │      pending_code 有暂存?先验它(放行时机 = B 点同意那一刻)
    │      验当前码 → 对:complete_pairing + 回 PairResult{ok:true}
    │                → 错:计数,3 次断连冷却(暂存码验错同样计数)
    └─ 已配对/受信 → 既有行为(忽略)
```

**B 点同意的动作**(新方法 `grant_consent`,由壳命令调用):

```
本地随机生成 6 位码 → 哈希存入 state → 发 ConsentGrant
    → UI 事件 PairingCodeShown { own_code }(此刻才亮码)
    → 状态 AwaitingConsent → Granted{ code_hash }
    └─ Granted 瞬间检查 pending_code:
           有暂存且验对(与 code_hash 常数时间比对)→ 立即 complete_pairing(A 侧挂起秒解除)
           有暂存且验错 → 计数路径
           无暂存 → 安静等 A 输码
```

**为什么暂存在 B 而不是 A 重发**:重发需要 A 侧加定时器+重试逻辑,且"码已验过"状态在 A 侧重复判定;暂存方案状态单点在 B,时序无关,测试好写。

### 4.5 超时配置

```rust
consent_timeout_secs: u64,   // 默认 60,可设范围 15-600
```

- 默认 60s,设置页(连接安全卡片)可调,范围外钳制到边界
- 持久化进 `data/config.json`,与其他设置项同路
- 门超时到点未点 → 断连;**不记冷却**(冷却只留给码错 3 次)
- B 挂起等待(同意后等 A 输码)无独立超时,跟随连接 keepalive

## 5. 壳层(Tauri)与 UI

### 5.1 壳命令

```rust
#[tauri::command] grant_consent(fingerprint)       // B 点同意 → core.grant_consent(生成随机码+发 ConsentGrant)
#[tauri::command] deny_consent(fingerprint)        // B 点拒绝 → core.deny_consent(发 ConsentDeny+断连)
                                                    // reject_pairing 保留,内部转调 deny_consent
#[tauri::command] cancel_pairing_wait(fingerprint) // B 已同意后主动结束等待 → core.cancel_wait
```

### 5.2 事件桥

| core 事件 | 壳转发 | UI 消费 |
|---|---|---|
| `PairingConsentNeeded { fp, name }` | `pairing-consent-needed` | B 弹同意门(**不含码**) |
| `PairingCodeShown { fp, own_code }` | `pairing-code-shown` | B 亮码(同意后才发) |
| `PairingWaitConsent { fp, name }` | `pairing-wait-consent` | A 显"等待同意" |
| `PairingCodeEntry { fp, name }` | `pairing-code-entry` | A 亮输入框(收到 ConsentGrant 后) |
| `PairingResult`(既有) | `pairing-result` | 双方收尾(不变) |

`own_code` 只随 `PairingCodeShown` 走且仅在 B 同意之后发出(B 自己的 UI)。明文码不落 `pending_pairing` 表、不进日志;**壳不存在任何把 B 的码发给 A 侧界面的路径**。壳重启后正在展示的码丢失 → 会话已断,门与码一起消失,重新连接重新生成(码不恢复,安全优先)。

### 5.3 UI(PairingDialog.vue 重构为多态)

```
B 侧:
  态1 同意门:"设备 X(name+指纹缩写)请求连接"
      [同意] → grant_consent;[拒绝] → deny_consent
      倒计时显示剩余秒(取自配置)
  态2 展示码:大号 6 位码(CodeBadge 复用)+ "把此码告诉对方,或在对方电脑上输入"
      + [结束等待] 按钮 → cancel_pairing_wait(码即焚)
      无输入框,等待配对结果事件

A 侧:
  态1 等待:"等待 X 同意连接…"(转圈);被拒/结束等待 → 提示 + 关闭
  态2 输码:"请输入 X 屏幕上显示的码";6 位输入框(纯数字、错则清空重输)
  态3 挂起:"已提交,等待对方确认…"(B 可能后同意)
      PairResult{ok:true} → 成功提示 + 关闭
```

同一组件按事件类型切态(角色由事件区分,UI 不自行判断)。

**设置页**:连接安全卡片新增"同意超时"数字输入(15-600s,默认 60)。

**Devices.vue 连接按钮**:点击后 A 直接进等待态;按钮逻辑不变。

## 6. 测试矩阵

| # | 测试 | 验证点 |
|---|---|---|
| T1 | `grant_then_code_completes` | B 同意在先,A 输对码 → 双方互信 + 双方 SessionUp |
| T2 | `code_arrives_before_consent_suspends_and_releases` | A 码先到(B 未同意)→ 挂起不计数;B 点同意 → 立即完成 |
| T3 | `deny_consent_disconnects_initiator` | B 拒绝 → A 收 ConsentDeny、连接断、无互信 |
| T4 | `consent_timeout_disconnects` | 门超时(测试 1s)→ 断连、无冷却记录 |
| T5 | `wrong_code_counted_even_when_suspended` | 挂起期间暂存码验错同样计数,3 次断连+冷却 |
| T6 | `initiator_never_receives_own_code` | A 侧事件流断言:全程只有 WaitConsent/CodeEntry,无携带码的事件 |
| T7 | `trusted_reconnect_stays_silent` | 既有测试照跑(受信路径零回归守卫) |
| T8 | `relay_pairing_same_flow` | 中继路径 E2E:远程设备同样走门+码 |
| T9 | `cancel_wait_burns_code_and_disconnects` | B 同意后点结束等待 → 断连+码即焚;A 收"对方已结束等待";同码重放无效 |
| T10 | `code_is_random_per_attempt` | 同两台设备两次配对尝试生成的码不同;码明文不出现在协议消息/日志中 |

既有测试处置:
- session.rs 现有配对测试按新时序改造(原"双方亮码任一方输对即成"流程不再存在)
- `预互信` 测试工具保留(T7/E2E 靠它)
- pairing.rs SAS 派生测试全部不动(密码学未变)

## 7. 边界与防御

1. **旧版 A 连新版 B**:B 等 ConsentGrant 永不到来 → 门超时断连。fail-closed,可接受
2. **重复 ConsentGrant**:Granted 幂等,忽略
3. **A 输码后才收到 ConsentDeny/ConsentCancel**:按对端拒绝/结束处理,UI 收尾
4. **同指纹并发连接**:会话表按指纹键控,后到覆盖,维持现状
5. **壳重启恢复**:会话已断(QUIC 断),门与码一起消失;明文码不持久化,重新连接重新生成(安全优先,不恢复旧码)
6. **码重放**:码随连接即焚,断连后同码再提交 → 会话已不存在,直接失败

## 8. 文档

usage.md 配对说明改为新流程;CHANGELOG 记安全增强。

## 9. 不做的事

- 不动受信重连静默路径
- 不做协议版本协商(未发布,无兼容包袱)
- 不做 B 侧输码(安全上冗余:同意门已保障 B)

## 10. 与原 SAS 方案的密码学变更说明

原设计的 `derive_sas_code`(从 QUIC exporter 派生对称码)与 `PairingMachine` 的双向验码**整体废弃**,被"同意时随机生成 + B 侧哈希常数时间比对"的单向验码取代。原因:用户要求码不固定、每次随机,且 A 侧界面永不显示码——对称派生模型与这两条约束不兼容。pairing.rs 的 SAS 派生测试随实现一并移除;`PairingMachine` 改造为仅 B 侧使用(持有 code_hash + 失败计数)。
