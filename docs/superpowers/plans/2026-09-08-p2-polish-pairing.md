# P2 执行计划：打磨·配对流程健壮性

> Spec: specs/2026-09-07-p2-polish-pairing.md
> 环境注意：huss_laptop 不可达——失败矩阵场景用 PC↔手机（当前已配对）或环回/条件跳过；双机部分标注遗留。

## 任务分解

### T1 失败分支梳理（评审文档先行）
- [x] 枚举配对全部失败分支写入本计划卡（见下表）

### T1 失败分支梳理表（2026-09-08 实读 core session.rs/pairing.rs + src-tauri commands.rs/main.rs 事件桥 + PairingDialog.vue）

| # | 分支 | 链路（core 事实） | 当前行为 | 评估 → 改动 |
|---|---|---|---|---|
| 1 | 对端拒绝（B 点拒绝） | B deny_consent 发 ConsentDeny → A ctrl_loop 发 `PairingResult{ok:false,"对方拒绝连接"}` + 断连；B 本地收 `{"已拒绝对方连接"}` | A: toast「配对失败: 对方拒绝连接」+弹窗关；B: toast「已拒绝配对请求」+关 | 行为正确 → 文案打磨：reason→建议文案映射表（拒绝≠系统错误）；锚 Vue 测试 |
| 2 | 同意门超时（60s 无响应） | core 门任务超时断连（不记冷却）：B 收 `{"同意超时"}`，A 收 `{"连接已断开"}`+SessionDown | A/B: toast「配对失败: …」+关；无可操作建议。**缺陷**：B 本地倒计时归零时自行调 deny → 与 core 超时竞态弹「拒绝失败: 会话不存在」 | 改：B 倒计时归零只显示「已自动拒绝」，动作交给 core（事件为唯一事实源）；超时类失败进「失败态」对话框（建议文案+重新发起按钮=重试直达） |
| 3 | 错码第 1/2 次（可重输） | B Mismatch → wire `PairResult{ok:false}`（连接保持）→ **A 侧 ctrl_loop 把它误标为 `{"对端拒绝"}`**（真拒绝走 ConsentDeny，另一文案） | A: isMismatch(含「不匹配/码错误」)不命中 → toast+弹窗关 —— **重输流程实际断裂（P2 最大缺口）**；B: 无事件、亮码不变（正确） | 改：core A 侧 `PairResult{ok:false}` 事件 reason 改「配对码不匹配」（3 处，内部事件文案非协议）；Vue 回 entry + 本地失败计数显示「码不匹配，还可重试 N 次」（N=3-已错次数，A 本地计数与 B 计数天然同源） |
| 4 | 错码第 3 次（冷却 5 分钟） | B FailedOut → 记 cooldown[A]=+300s → wire `PairResult{ok:false}` → 断连（close reason "Pairing failed"）；B 本地收 `{"配对码错误超过 3 次"}` | A: 与第 1/2 次表现相同（无法区分终态）；B: toast+关 | 改：Vue 本地计数达 3 → 终态：关弹窗 + toast「配对码连续错误 3 次，配对终止（对方设备冷却约 5 分钟）」；连接断开事件兜底收尾 |
| 5 | 冷却期内再次发起 | 持冷却方主动 connect：post_handshake 直接 `SessionError::Cooldown(remaining)`；对端持冷却时：握手后连接被断（b"cooldown"）→ A 报连接类错误 | 壳层 connect 命令 `e.to_string()` 原样透传 → 前端 toast「连接设备失败: 连接处于冷却期，剩余 295 秒」；无禁用态/倒计时 | 改：壳层 connect 识别 Cooldown 变体 → 结构化错误 `pairing_cooldown:{secs}`；UI 统一 formatConnectError →「对方设备处于配对冷却（剩余 X 秒）…」+ 设备卡冷却倒计时禁用徽章（UI 侧记忆 300s） |
| 6 | 输码会话断线（输码中对端离线） | ctrl EOF → cleanup → A（未信任且无终止结果）收 `{"连接已断开"}`+SessionDown | toast「配对失败: 连接已断开」+关（文案无建议） | 改：Vue 增 connection-state{up:false} 监听兜底（waiting/entry/submitted 中断连 → 关弹窗+建议文案）；reason 映射补建议 |
| 7 | 双盲并发（A、B 同时互点） | 会话表按 fp 单条目、后插者覆盖（M-B1 代次防误杀）。交错好：存活条目同属一条连接（一侧 Awaiting）→ 一边胜出完成；交错坏：双方表都剩自己主动连接（均 Initiator）→ 双方 grant 报「会话不处于待同意状态」，任一方重连一次必收敛 | 现有行为：无死锁、可恢复，但坏交错时用户看到报错文案生硬 | 锚：core 环回单测×2（顺序互连确定性收敛 + 并发互连有界恢复≤1 次重连）；Vue grant 失败文案可操作化（「配对请求已失效，请稍后重试」） |
| 8 | 对已信任设备重复发起 | post_handshake is_trusted → 直接 SessionUp（幂等，connect_inner 已在会话跳过） | 正常，无配对流程 | 合理 → 既有 `trusted_reconnect_is_silent` 已锚定，无改动 |
| 9 | 码输入非数字/超长 | Vue handleCodeInput `replace(/\D/g,'').slice(0,6)` + maxlength=6 + 6 位才可提交；core 常数时间比对 | 前端已防住 | 合理 → 补 vitest 输入过滤锚，无改动 |
| 10 | 边界：A 先输码 B 后同意（pending mismatch） | B 同意瞬间判暂存码 → Mismatch → 仅 B 收「配对码不匹配」事件；wire 上无回包给 A | **既有缺陷**：A 停在 submitted 无反馈 | 改：Vue submitted 阶段 15s 看护超时 → 回 entry 提示「对方尚未判定…」；记录为已知边界（协议不动的前提下无法根治） |

红线复核：上表改动全部为内部事件文案/壳层错误传递/UI 层——wire 协议消息（PairResult/ConsentDeny 字段）、冷却语义（COOLDOWN_SECS=300、记冷却时机）零改动；不动 Kotlin、不动既有场景断言。

### T2 实现
- [x] 错码反馈分层："码错(剩 N 次)"与"已冷却(倒计时)"分离显示
- [x] 冷却期发起方按钮态（禁用+倒计时提示）
- [x] 超时文案带可操作建议+重试直达
- [x] 双盲并发：双方同时 connect 的一边胜出语义验证（现有行为确认/最小修复）
### T3 验收
- [x] 失败矩阵场景 pairing-matrix.mjs（条件跳过模式；PC↔手机可用项真机跑）接入 run-all
- [x] vitest/单测/截图

## 执行记录（2026-09-08 完成）

### 改动清单
| 层 | 文件 | 内容 |
|---|---|---|
| core | `crates/localtrans-core/src/session.rs` | A 侧 `PairResult{ok:false}` 事件文案 3 处「对端拒绝」→「配对码不匹配」（内部事件文案，wire 协议零改动）；新增环回单测×3：`wrong_code_result_says_mismatch_not_denied`（错码文案锚）、`mutual_connect_sequential_converges_single_session`（双盲顺序互连确定性收敛+恰好一次 ok:true）、`simultaneous_connect_bounded_recovery`（双盲真并发三种交错落点均 ≤1 次补连收敛，不死锁） |
| 壳层 | `src-tauri/src/commands.rs` | `fmt_connect_err`：`SessionError::Cooldown(secs)` → 结构化 `pairing_cooldown:{secs}`（connect_pinned×2 + adopt_as_initiator 三处接线）+ 单测 |
| 壳层 | `src-tauri/src/main.rs` | PairingResult 失败 toast 分层：「配对码不匹配」（可重输）不弹 toast；其余映射建议文案 `pairing_failure_advice`（与 UI failureAdvice 同源） |
| UI | `ui/src/components/PairingDialog.vue` | 错码分层（本地失败计数→「码不匹配，还可重试 N 次」，第 3 次终态）；失败态对话框（建议文案+关闭/重新发起=重试直达）；connection-state 断连兜底（错码耗尽→冷却终态，第 3 次在途→概率冷却两可文案）；同意门倒计时归零不再抢跑 deny（消「拒绝失败: 会话不存在」竞态）+3s 兜底收敛；submitted 15s 看护（pending-mismatch 边界防挂起）；grant 失败文案可操作化 |
| UI | `ui/src/stores/devices.ts` | 配对冷却表（fp→到期），connect 错误 `pairing_cooldown:N` 解析记入；pairing-result 3 次终态记入/成功清除；设备卡倒计时数据源 |
| UI | `ui/src/components/DeviceCard.vue` | 冷却徽章（倒计时）+ 连接按钮禁用态；连接错误统一 formatConnectError |
| UI | `ui/src/api.ts` | `formatConnectError`：`pairing_cooldown:{secs}` →「对方设备处于配对冷却（剩余 X 秒）…」 |
| UI | Devices/Browse/PushWizard | connect 错误统一走 formatConnectError |
| 测试 | `ui/src/__tests__/PairingDialog.test.ts` | 7→8 用例（deny 失败态、错码 1 次分层、3 次终态+冷却、竞态丢失兜底、输入过滤、断线兜底） |
| 测试 | `ui/src/__tests__/DeviceCard.test.ts` | +冷却徽章/禁用/过期恢复用例 |
| E2E | `tests/e2e/scenarios/pairing-matrix.mjs` | 新场景（PC↔手机真机 11 步），已注册 run-all |

### 验证
- gate core：282 passed / 0 failed；gate shell：83 passed / 0 failed；ui：vitest 202 passed + build ✓
- 真机场景 `node scenarios/pairing-matrix.mjs`：**11/11 PASS**（reports/pairing-matrix-mtsuxnkw-x51/）
  - 分支B 拒绝：PC 失败态文案「对方拒绝了本次配对请求…」✓
  - 分支C 同意门超时（15s）：门自动收敛 + toast 建议文案捕获 ✓
  - 分支D 错码3次：递减文案 ✓ → 冷却 toast ✓ → 设备卡徽章「配对冷却 299s」✓ → 冷却期重连被拒 ✓
  - 分支A 错码重输：「还可重试 2 次」→ 重输 → trusted ✓（P2 修复主锚：旧版此处「对端拒绝」直接关窗、重输断裂）
  - 夹具还原：PC 三件还原 + 手机重配对 trusted ✓（手机截图目检 huss_pc 已连接）
- 目检证据说明：PC 关键态 DOM 断言全部实时通过；像素截图因 Windows 会话 00:03 自动锁屏退化为锁屏图（reports 内 v-*.png）。失败态对话框真实渲染目检见上一轮运行 `reports/pairing-matrix-mtsukqo4-f85/failure-pc.png`（建议文案+关闭/重新发起按钮渲染正确）。锁屏期重跑可补像素证据。

### 实证发现（实现中确认）
1. **P2 主缺口证实**：A 侧错码 1/2 次与终态失败在事件层不可区分（旧文案统一「对端拒绝」），重输流程从未真正可用——本轮以内部事件文案修正+本地计数解决，wire 不动。
2. **FailedOut 竞态**（core 测试早有注释）：第 3 次错码的 PairResult 可能跑不赢连接关闭，发起方只剩 SessionDown——UI 以「本地计数+第 3 次在途」判别做概率冷却终态兜底，文案两可。
3. **配对门只看接受方 is_trusted**：单边残留信任会让门静默跳过（首轮真机实证：手机残留信任→直连无门）。矩阵场景因此用 factoryReset 保证双端零信任。
4. **Android PairingDialogs 为裸 Compose Dialog**，未开 testTagsAsResourceId，e2e 拿不到 btn-grant/btn-deny tag——场景按文案节点点击（remote-rename 同款手法）。
5. 双盲并发坏交错（双方表内均剩主动连接）与交叉双待同意（grant 均成但判定被僵尸忽略）两种落点存在，均为「一次重连即恢复」的有界问题，已单测锚定；彻底解法（连接准入控制/指纹决胜）涉及会话表语义重构，建议单独立卡。

### 遗留
- huss_laptop 不可达：双 PC 版失败矩阵（A/B 互换角色）待其恢复补跑。
- 冷却期发起方精确倒计时依赖 connect 报错刷新；反向（对端持冷却）断连错误是连接层报错，无法携带剩余秒数（wire 不动的前提下）。
- Android FFI 未重编（core 文案修正对安卓发起方生效需下次 APK 构建）。

（进行中）

## P2 关门 ✅（2026-09-09）
- 61f8040：九分支梳理+实现（错码分层反馈"还可重试 N 次"/第 3 次冷却 toast+设备卡冷却徽章/超时带建议文案/双盲并发环回单测锚定/submitted 15s 看护）
- **实测亮点**：真机 pairing-matrix 11/11（PC↔手机，含错码递减/冷却徽章/重输恢复）；单边残留信任使同意门静默跳过的实证（场景改 factoryReset 保证零信任）
- **实证发现（打磨池）**：PairResult FailedOut 可能跑不赢连接关闭（core 已知注释）——UI 概率冷却兜底；Android PairingDialogs 未开 testTagsAsResourceId
- 门禁：core 282 / shell 83 / vitest 202 全绿
