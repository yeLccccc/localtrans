# M3b 执行计划：智能选路·探测与评分

> Spec: specs/2026-09-07-m3b-smart-routing-probe-score.md
> 顺序：FR1 通道记录 → FR2 探测(阶梯+RTT+时机) → FR3 评分选路 → FR4 退化切换 → 接线与场景。
> 环境注意：huss_laptop 不可达——单测/环回场景为主；双机场景条件跳过接入 run-all。

## 任务分解

### T1 FR1+FR2: 通道记录与探测器
- [ ] core 新模块 `src/routing/`（或 channel.rs）：ChannelRecord{addr, rtt_ms, est_bps, loss10(近10次), ts}；ChannelTable（per-fingerprint HashMap，内存态）
- [ ] 阶梯带宽探测：64KB→512KB→4MB 三级 QUIC 流计时（探测用独立 bi 流，复用会话；取最优稳态=中位段速率）；RTT=应用层 Ping 消息×3 中位（ControlMsg 增 Ping/Pong 或复用现有心跳语义——选最小）
- [ ] 时机纪律：会话建立后全量（直连各地址+中继）；5min 周期快检(64KB)；快检掉 50% 升级全量；**活动传输时推迟**（transfer 表有 active/paused 即推迟，探测任务挂起）
- [ ] 单测：阶梯计算/时机纪律判定/记录更新（mock 时间）
- 提交：`feat(core): 通道记录与阶梯探测(时机纪律)`

### T2 FR3: 评分与选路
- [ ] score = rtt_score(50%)+bw_score(35%)+stability_score(15%)，插值表按 spec（<5ms=100, 50ms=60, 200ms=20, >500ms=0 线性；>100Mbps=100, 10Mbps=50, <1Mbps=0 对数；丢包 0%=100, ≥10%=0）——纯函数直测
- [ ] 选路决策：connect 前查表——多通道取最高且领先次优≥15 分；同分误差内直连>中继；地址探测超时 3 次摘除（下次会话补测）
- [ ] 接线：connect/connect_pinned 的地址选择改走决策（单通道直接用=现状不回归）
- [ ] 单测：评分插值/迟滞(49:51 不切)/摘除
- 提交：`feat(core): 评分选路(RTT50/带宽35/稳定15,迟滞>=15)`

### T3 FR4: 退化切换
- [x] 触发：当前通道 RTT 连续 3 次复测翻倍 OR 传输失速 30s（source_probe/看门狗信号）
- [x] 动作：次优地址后台建新 QUIC 会话→就绪→会话表原子替换（generation+1 语义已有）→旧连接关闭；断点续传天然跨通道
- [x] 单测：触发判定/次优选择/替换原子性（mock 双地址环回）
- [x] 用户可见面：仅通道数据变化（UI 在 M3c）；不弹窗
- 提交：`feat(壳): 退化切换(RTT翻倍触发,新传输走上通道;进行中迁移TODO)`

### T4: 接线与场景
- [x] ffi：通道表只读暴露最小 DTO（M3c 用）——regen
- [x] 场景 routing-probe.mjs：双地址环回（或单地址退化模拟）验证探测记录/评分/切换；条件跳过接入 run-all
- [x] 门禁全绿
- 提交：`feat(测试): 探测选路场景(环回)`

### 执行记录
（M3b 代码面 T1~T4 全部完成，2026-09-08；遗留：双机场景补跑待 huss_laptop 恢复、失速触发/进行中迁移 TODO 见 T3、ffi 通道表生产者接线见 T4→M3c）
- T1+T2 完成（2026-09-08，分支 `agent/m3b-t1-t2-probe-score`，两笔 b07a22e + 776d2a0）：core 新模块 `routing/`（mod=ChannelRecord/ChannelTable/时机纪律纯函数；probe=阶梯探测原语+每会话响应端；score=插值表评分+decide 选路）。**wire 兼容定案**：探测消息（ControlMsg 增 Ping/Pong/ProbeReq/ProbeResp）只走既有会话上的**独立 bi 流**、绝不进 ctrl 流——老版本对端无探测流消费者（quinn 不自动拒收），不解析不断连零感知，探测方超时即拉黑（第一次失败即停，register 补测清拉黑，3 次累计摘除），**无需能力协商**；ctrl 流误投探测消息=防御性忽略（新端不因新变体断连）。带宽=出站速率版（发 ProbeReq+size 裸数据，对端读满即丢回 ProbeResp，计时含全程=端到端送达速率；回灌收速率留升级路径，wire 已备好）。响应端挂 insert_session_and_spawn（PC/FFI 对称获得）；调度编排落壳层 probe.rs（SessionUp 全量+5min 快检+掉50%升级+活动传输轮询推迟）；connect 接线=候选≥2 才决策，单候选逐字节维持"发现优先中继兜底"。via_relay 判定=relay 数据面租约地址集（RelayClient::is_relay_data_addr，确定性不猜网段）。gate：core 275 绿（串行口径同绿）+ shell 75 绿 + workspace/ffi check 绿；环回验证=127.0.0.1 双 listener 双地址全量探测记录/评分/决策/ctrl 隔离全过。不动 ffi/Kotlin/UI/广播包；机密扫描 0；未 push。
- T3 完成（2026-09-08，分支 `agent/m3b-t3-failover`，单笔）：**降级交付**——RTT 触发全量落地，失速触发留 TODO。触发：core 侧 `rtt_doubled` 纯函数（基线>0 且 cur≥2×prev，saturating 防溢出）+ ChannelRecord 增 `rtt_double_streak`（record_rtt 翻倍连击/不翻倍清零；register 新会话代重置），连击达 3（`DEGRADE_AFTER_RTT_DOUBLES`）由 `ChannelTable::take_rtt_degradation` 消费性取用；检查点挂 probe.rs 调度器循环（探测推迟循环**之前**——切换不占探测流量，传输中允许）。切换编排 `failover_plan`（复用 decide 迟滞/直连优先；中继不在场剔除经中继记录，与 connect 命令同口径）+ `try_failover`：connect_pinned/中继 connect_peer（限时 10s）→ 成功即两端会话表已原子换代（复用 M-B1 insert 覆盖语义，不另造原语）→ note_connected 换当前指针（SessionUp 泵幂等重登+全量补测）→ 等传输静默后关旧连接（旧代 ctrl_loop 双端 stable_id 判定静默退出，无 SessionDown 假断线）。**安全边界**：建连失败/超时→保持旧通道+目标记录降分（失败样本入稳定窗+超时计数，3 次摘除，下次会话补测恢复）；触发消费后需重新累计 3 次，自带 ≥15min 退避。**失速 30s 触发留 TODO**（M3c/后续）：落点已勘察（run_source_probe 每 500ms SourceSpeed{bps,streams}，bps==0 且 streams>0 持续 60 tick=失速；壳层 main.rs PE::SourceSpeed 消费处挂计数），侵入传输事件桥故独立成任务。**进行中传输不迁移（引擎层限制,TODO）**：接收侧逐块 FetchReq 走会话表（sm.send_ctrl）、对端开块流也查其会话表，表换代后持旧 Connection 克隆的接收循环等不到新会话上的块流（块边界失速→看门狗→失败可续传）；"原子换流"需把 Connection 抽象为可替换句柄，超出单任务工作量——本版收益由"新发起的传输走上新通道"承载。顺带：commands::connect_via_relay 改 `&AppState` 入参+返回租约地址（shell 内两处复用，行为不变）。gate：core 278 绿（275 基线+3，不回退）+ shell 81 绿（75 基线+6）+ workspace check 绿；环回验证=127.0.0.1 双 listener 手写坏 RTT→decide 选次优→真 QUIC 建连→断言新会话 remote_address=次优地址+旧连接关闭无假断线+死地址失败保旧通道降分，全过。不动 ffi/Kotlin/UI/广播包/场景断言；机密扫描 0；未 push。
- T4 完成（2026-09-08，main 直上，单笔）：
  - **ffi 通道数据方案结论**：ChannelTable 本就在 core（`localtrans_core::routing::ChannelTable`，T1 定案），无需挪动，ffi 可达。按本卡红线"ffi 只加只读查询"，落地为**挂载+只读导出**：ffi AppState 增 `channels: Arc<ChannelTable>`（镜像 PC 壳字段），`LocalTransApp` 增 uniffi 只读方法 `channels() -> Vec<ChannelDto>`（字段口径镜像桌面 `list_channels`：fingerprint/addr/via_relay/rtt_ms/est_bps/loss_rate/current/score_ready/probe_disabled/age_secs）。**表当前恒空**：生产者（SessionUp 登记 note_connected + 全量探测 spawn，镜像 PC `probe::on_session_up`）属写路径，红线排除，TODO 注明 M3c 前置（state.rs/lib.rs 注释）——M3c 只补生产者，导出面不再变。Kotlin 绑定已 regen：jniLibs 双 .so 重建（cargo ndk release）+ localtrans_ffi.kt 重生成（167 行纯新增）+ **AppException errorMessage 手工补丁按"重生成后需重做"惯例重打**。构建链坑两枚（均已记录在案并绕过）：① gradle genUniffi 引用的 `target/android/x86_64/*.so` 是陈旧 CARGO_TARGET_DIR 产物，bindgen 必须用 jniLibs 内新 .so（与 2026-08-24 计划卡记录一致）；② `--out-dir` 尾部含 uniffi 会嵌套生成，`cp -r` 误产 uniffi/uniffi/（已删，直接文件级 cp）。
  - **PC 只读命令**：`list_channels`（commands.rs，serde 数组每设备每通道一条；age_secs=单调钟 elapsed 秒）+ generate_handler 注册 + test_api invoke 白名单（ReadOnly）+ dispatch 分支 + 白名单单测扩列 + 契约文档 §5.5 扩列与变更记录。
  - **场景**：`tests/e2e/scenarios/routing-probe.mjs` 条件跳过骨架（任一 PC bridge 未就绪 → outcome=skip exit 0）——握手（含 list_channels 白名单在位自检，旧构建 403=未部署信号）→ 会话在位（无则 connect 注入）→ A/B 双端通道记录断言（恰一条/current=true/rtt_ms 非空/est_bps>0/loss_rate=0/score_ready/健康通道不拉黑/中继关闭时 via_relay=false）→ 第二地址模拟段记 SKIP 注记（白名单无 connect_pinned+辅助 IP 需改测试机 NIC 不可行；多地址评分/退化切换由 core 单测+probe.rs 127.0.0.1 双 listener 环回覆盖）→ 截图证据。接入 run-all 注册表 + docs/e2e-harness.md。**验收实录**：huss_laptop 不可达（本卡既知环境约束）→ 场景跑出 SKIP exit 0、`run-all --only=routing-probe.mjs` PASS；huss_pc 侧本地重建部署（release+test-api+custom-protocol）后实测 `list_channels` 200 `{"data":[]}`（无会话正常态）+ 403 detail.allowed 含 ReadOnly 条目——命令面端到端已验，**通道有数据的双机断言待 laptop 恢复补跑**。
  - **越界修复（relay e2e，缘由必录）**：relay 泳道门禁首跑即红——`relay_pairing_same_flow` 在序列5 永久挂起。二分定位：06b470f（M3a 收官）绿 / 776d2a0（T1+T2 合入）红，**系 T1+T2 探测响应端（serve_probe_streams 独占 ctrl 流之后全部 bi 流）与该测试裸 open_bi/accept_bi 断言抢流**（T1~T3 门禁均未跑 relay 泳道故漏网）。修复=改写该测试序列5 走产品真实路径（`send_rpc` 出站 + pending_rpcs 等待者 + `take_inbound_ctrl_rx` 入站），断言语义不变且更强（真 ctrl 链路往返）；其余 7 个 e2e 测试的裸流断言均在 RelayClient 裸 punch 连接上（无响应端），不受影响。块流为 accept_uni 与 accept_bi 分队列，产品传输面无此竞争。
  - **门禁**：core lib 278 基线不回退（+13 集成=291 全绿）/ shell 81 基线不回退（test-api feature 口径 invoke 白名单 9 测试绿）/ **relay 41 基线恢复全绿**（修复前 27+5 后挂死）/ ui build 绿 / `cargo check --workspace --all-targets` 0 error（顺带补 ffi 测试构造器 channels 字段）。ffi 泳道无门禁：`cargo test -p localtrans-ffi` 73 绿/7 红——对照 pristine main（b3c8f23）同为 73/7，**7 红全部既有**（网络环境依赖类），本卡零新增。
  - 不动：既有 mjs 场景断言/Kotlin 应用代码/广播包/UI 源码/core 生产代码。机密扫描 0；未 push。

## M3b 收官 ✅（2026-09-08）
- T1+T2 b07a22e/776d2a0（通道记录/独立bi流探测/评分迟滞，合回 main core 275）
- T3 b3c8f23（退化切换：RTT 翻倍触发/次优建连/表原子换代/失败降分；"进行中传输原子换流"=引擎 Connection 句柄化 TODO）
- T4 3ff7c68（ffi 只读通道 DTO+regen/list_channels 命令/routing-probe 条件跳过场景；**越界必修**：探测响应端抢 relay 测试裸 bi 流——该测试重写为产品真实路径，relay 门禁 41 恢复绿）
- 收官门禁：core 278、relay 41、shell 81、ui build 全绿
- 双机遗留：routing-probe 双机探测记录断言待 laptop
