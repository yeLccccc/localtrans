# Spec M2: Android 传输域对齐（android-transfer-parity）

> 里程碑：M2。节奏位：功能实现第 2 站。**契约先行**：FFI DTO 变更先单独合入，Kotlin 与场景再并行。
> 一句话：手机传输管理与 PC 同构——三端一个心智；**按定案修订 R1 只做一个进度条**。

## 目标
手机端获得 v0.12 传输域同等能力，并关掉运行时退化根因。

## 范围
**做**：FFI DTO 扩字段、卡片状态机、两级删除+磁盘历史、并发队列、Kotlin 传输页改造（父卡/分区/排队/单进度条）、post_handshake 挂起根因、批拉 UI 自动化收尾。
**不做**：智能选路（M3）、PC 端改动（除非契约点）、双进度显示（R1 明令不做）。

## 功能需求
- **FR1 DTO 扩展**（契约）：TransferDto 增 `queue_pos/batch_id/children/parts_id/started_at_ms/finished_at_ms/source_path`（serde default 向后兼容）；uniffi regen。
- **FR2 卡片状态机**：ffi 侧等价 transfer_state.rs（七态+cancelling 仲裁+终态吸收+engine→card 映射+ID 恒定）；pause/resume/cancel/remove 走状态机而非直改字符串。
- **FR3 两级删除+历史**：`transfer_remove(level)`、`list_disk_jobs/restore_disk_job/destroy_disk_job` FFI 导出；启动 manifest 优先重建。
- **FR4 并发队列**：active_gate+对端锁+queue_pos 上报；`max_active_transfers` 进 SettingsDto（1-8）。
- **FR5 Kotlin 传输页**：父子卡片展开/单进度条（R1）/排队位次/活动-历史分区/磁盘历史入口/别名显示。
- **FR6 退化根因**：post_handshake 挂起定位与修复（panic 钩子+点位探针法，重现→根因→回归场景）。
- **FR6a Kotlin 事件流丢事件（2026-09-07 证据链完整）**：重装启动后权限弹窗阶段，AppNav 的 LaunchedEffect 未订阅 EventRouter.events（replay=0 SharedFlow），此间到达的 OfferRequested 被丢弃→推送弹窗永不出现。探针三层定位：core ctrl-fwd ✓ → router ✓ → ffi-ask ✓ → Bridge event ✓ → **UI 无弹窗** ✗。修复方向：EventRouter.events 加 replay + offerAsk 状态提升到单例层（不随 Compose 生命周期）；或 MainActivity 起来后主动向 ffi 拉取 pending offer。
- **FR7 批拉 UI 自动化**：远程文件页多选交互模型摸清+场景断言（多选=1 卡）。

## 验收标准
1. 跨端场景全绿：PC→phone 推送（含暂停/取消）、phone 多选拉取=1 父卡、phone→PC 反向；
2. 手机连续运行 ≥1h 混合操作无退化（场景断言会话可用性）；
3. 传输页三端行为对照清单逐条目检（截图）；
4. FFI host 测试全绿（端口类失败已被 M1-FR2 消化）。

## 依赖与顺序
依赖 M1（场景地基+端口）。FR1 先行合入。

## 风险
- uniffi regen 破坏 Kotlin 编译（校验和变化时流程要熟练）；
- 退化根因若在 core（锁序），修复波及 PC 端需同步回归。
