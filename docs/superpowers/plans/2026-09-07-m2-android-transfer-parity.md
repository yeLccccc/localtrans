# M2 执行计划：Android 传输域对齐

> Spec: specs/2026-09-07-m2-android-transfer-parity.md
> 顺序：FR6a(丢事件,最小修复先解阻塞) → FR1(FFI 契约) → FR2-FR4(状态机/删除/队列) → FR5(Kotlin UI,单进度条 R1) → FR6(退化根因) → FR7(批拉 UI 自动化)。
> 纪律：每个 FR gate+可跑验证；FR1 契约先单独提交；真机验证由 subagent 直接做（adb 可用）。

## 任务分解

### T0 FR6a: Kotlin 事件流丢事件（当前唯一 run-all FAIL）✅ 已完成(2026-09-08)
- [x] 根因确认（实测推翻原假设）：**事件链无丢失**。探针实证 onEvent→EventRouter.route(subs=1)→emit→AppNav collect→offerAsk 置位全程可达，重装后冷启动亦然；截图目检弹窗正常渲染。真根因在 **e2e 断言层**：Compose Button 的 a11y 节点(clickable=true,文本空)与文本子节点(TextView"接收",clickable=false)是两个节点，场景 finder 要求"同节点 clickable&&text 匹配"永假 → 25 轮轮询假阴性"无弹窗"；且 `ch.tap({x,y})` 用错 API（tap 只收选择器，坐标应走 tapXY）。
- [x] 修复：①场景 finder 改 testid 优先(offer-accept-btn)+文本节点回落(tapXY)；②OfferSheet 按钮/弹窗内补 testTag+testTagsAsResourceId（对齐桌面 09193ad 惯例）；③EventRouter.events replay=8 保留（防真正的订阅前窗口，如 MIUI 权限弹窗冷启动）；④EventRouterTest 改 replay 免疫断言。
- [x] 验证：night-android-transfer 修复后连续 2 次全绿(reports/android-xfer-mtrfhd8h、android-xfer-mtrfk5j8)+撤探针重构建后最终确认 1 次全绿(android-xfer-mtrfo8tm)。
- 提交：`fix(安卓): 重装后推送弹窗丢失——根因与修复(e2e断言假阴性,事件链实测无丢失)`

### T1 FR1: FFI DTO 契约扩展（单独提交，契约先行）
- [x] dto.rs TransferDto 扩 queue_pos/batch_id/children/parts_id/started_at_ms/finished_at_ms/source_path（serde default）
- [x] uniffi regen + Kotlin 编译通过
- 提交：`feat(ffi): TransferDto契约扩展(传输域字段,uniffi regen)`

### T2 FR2: FFI 卡片状态机 ✅ 已完成(2026-09-08)
- [x] ffi 侧 transfer_state 等价实现（七态/cancelling 仲裁/终态吸收/engine→card 映射）
- [x] pause/resume/cancel/remove 走状态机；移植 src-tauri 已验证语义
- [x] 单测：迁移表全覆盖
- 提交：`feat(ffi): 卡片状态机(七态/仲裁/终态吸收,移植PC transfer_state语义)`

### T3 FR3: 两级删除+磁盘历史 ✅ 已完成(2026-09-08)
- [x] transfer_remove(level)/list_disk_jobs/restore_disk_job/destroy_disk_job FFI 导出（DiskJobDto 新 Record；destroy 级终态卡改 finalize_destroy 连带 parts 目录）
- [x] 启动 manifest 优先重建（rebuild_cards：多 parts 根扫盘建卡 → transfers.json 索引合并 → gc 收尾；set_inbox_dir 注入后补扫 inbox 根）
- [x] uniffi regen（含 T2 漏掉的 transfer_remove_level 与 docstring 校验和漂移）+ arm64/x86_64 .so 同代重编 + errorMessage 手工补丁重做
- 提交：`feat(ffi): 两级删除收尾+磁盘历史(manifest优先启动重建)`

### T4 FR4: 并发队列 ✅ 已完成(2026-09-08)
- [x] acquire_slot 等价(信号量+对端锁)+queue_pos 上报；SettingsDto 增 max_active_transfers
- 提交：`feat(ffi): 并发队列+排队位次`

### T5 FR5: Kotlin 传输页改造（R1 单进度条！）
- [x] 父子卡片展开/单进度条/排队位次/活动-历史分区/磁盘历史入口/别名
- [x] 视觉截图目检（三态×两端）
- 提交：`feat(安卓): 传输页v0.12对齐(父卡/分区/排队位次/两级删除/磁盘历史/单进度条R1)`

### T6 FR6: 退化根因（post_handshake 挂起）✅ 已完成(2026-09-08)
- [x] 根因确认（探针法实证推翻原假设）："引擎退化/入站半边死亡"不存在。全链路 T6P 探针（ctrl_loop→router→ask→Kotlin onEvent + ctrl_loop 30s 心跳）实证：复发窗口内 QUIC 连接与会话全部存活、OfferReq 秒达手机、Kotlin 事件正常触发——弹窗实际已渲染，但被系统界面遮挡（复现为 MIUI USB 授权/USB 用途弹窗；生产对应息屏锁屏/系统弹窗），60s 无人应答后手机端 Ask 子任务超时自动拒绝（OfferResp Timeout），PC 端报"对方超时未确认"——与 09-06/07 症状逐字吻合。discovery 照常、手机主动拨号正常等全部旁证均与该机制自洽；"forceStop 后立即恢复"即弹窗界面随重启消失。
- [x] 修复 1（真缺陷，症状3"伪连接"机制）：session.rs ctrl_loop 退出清理——被新一代连接覆盖的僵尸 ctrl_loop 退出时不再误发 SessionDown（改用 quinn stable_id 判定连接归属，M-B1 只护住了表条目、漏了事件；假断线诱发连锁手动重连）。回归测试 zombie_ctrl_loop_exit_does_not_emit_session_down（禁用守卫即红，已验证）。
- [x] 修复 2（可观测性）：engine.rs Ask 子任务超时/拒绝时落 INFO"推送请求未应答,已自动拒绝"——此前完全静默，是本次误判为引擎退化的直接原因。
- [x] 回归：cargo test -p localtrans-core 216/0（含新回归测试）；localtrans-ffi 68 过/11 存量端口类失败（口径不变）；night-android-transfer PASS（android-xfer-mtrrl3f1）；t6-degrade-loop 复现脚本连续 PASS。探针已撤除。
- 排障注意：测试机混用不同版本 adb（LDPlayer 1.0.31 vs SDK platform-tools）会互杀 server 并触发手机 USB 重枚举弹窗，直接遮挡弹窗制造假阳性——调试期严禁混用。
- 提交：`fix(core/ffi): 手机长运行入站退化根因修复(弹窗遮挡+超时自动拒绝,引擎无退化;修僵尸会话假断线)`

### T7 FR7: 批拉 UI 自动化收尾
- [ ] 远程文件页多选交互模型 → 场景断言（多选=1 卡）
- [ ] 接入 run-all；跨端场景补拉取段
- 提交：`feat(测试): 跨端批拉场景`

### 收尾
- [ ] run-all 全量 7/7；门禁三泳道绿；RELEASE/BACKLOG 状态更新 → M3

## 执行记录
- 2026-09-08 T6 完成：见 T6 小节。核心结论:引擎无退化(假设 A/B/C/D 全部排除),症状=弹窗被系统界面遮挡+60s 自动拒绝的观测假象;附带修复僵尸会话假断线(SessionDown 误发)并补自动拒绝日志。下一站 T7(FR7 批拉 UI 自动化收尾)。
- 2026-09-08 T5 完成：Kotlin 传输页对齐 PC v0.12(参照 TransferItem.vue/Transfers.vue/transferDisplay.ts)。新增 TransferDisplay.kt 纯函数层(progressPair/senderDisplayState/queueText/splitActiveHistory/peerDisplayName/childStateText,与桌面 transferDisplay.ts 同口径,13 单测);TransferUi 消费 T1 契约 8 字段(queuePos/children/batchId/partsId/startedAtMs/finishedAtMs 等,uniffi ULong→Long 收敛);TransfersRepo+FFI 导出 transferRemoveLevel/listDiskJobs/restoreDiskJob/destroyDiskJob。R1 落地:单进度条(发送方主进度=remoteDone 无镜像回退 done),满格未确认/积压>20% 只显"等待对方确认"文案,不渲染第二条进度条/角标(旧"对方已收"删除)。交互:活动-历史分区(历史默认折叠,标头计数),父卡子项展开(▸/▾+子行名/大小/状态徽章/细进度条),排队位次文案,终态卡删除两级确认(移除视图=可找回/彻底删除=不可恢复),活动卡取消=取消+destroy(cancelling 仲裁),清空记录逐卡 view 级(可在磁盘历史找回),磁盘历史入口恒可达(列表空也显示;恢复/彻底删除+确认),removed 事件删行+removingJobIds 守卫防删除中事件插回。testTag 同表铺设:transfers-history-fold-btn/transfers-disk-history-btn/transfer-expand-btn/transfer-child-{id}/transfer-pause/resume/cancel/retry/remove-btn/transfer-remove-view/destroy-btn/transfer-queue-text/transfer-awaiting-confirm/disk-job-{id}(-restore/-destroy)-btn;transfers-debug-seed-btn(演示卡注入,debug 构建专属,BuildConfig.DEBUG 门控——FFI 现阶段 children 恒空/积压态依赖时序,六状态目检与 T7 场景断言用)。真机踩坑三个修复:①LazyColumn key 禁 ULong(Bundle 不支持,2^63 占位 id 崩溃→disk 行字符串 key);②Dialog 独立窗口需单独 semantics{testTagsAsResourceId}(OfferSheet 同款);③view 级删除后列表空,磁盘历史入口需恒在。验证:gradle assembleDebug+testDebugUnitTest 102 测 0 败(1 存量 @Ignore);真机六状态截图目检(tests/e2e/reports/t5-visual/:活动区/子展开/历史折叠/历史展开/磁盘历史/删除弹窗/恢复回流)+dump 断言排队位次"排队中 · 第 2 位"/等待对方确认/子项 testid 全中;night-android-transfer 真实推送回归 PASS(android-xfer-mtrmkcd9)。遗留:FFI 不产 children(文件夹推送 UI 已铺待 FFI 填充,当前仅演示卡可见)、接收侧任务不落 FFI 表(进程死后事件卡消失,T3 已知特性)、DeviceDto 无 alias 字段(别名显示降级为广播名>指纹缩写,接口已按 alias>name>fp 预留)。下一站 T6(FR6 退化根因)。
- 2026-09-08 T4 完成：ffi 并发队列对齐 PC Task 7。AppState 删全局 xfer_lock，增 active_gate(permits=max_active_transfers,启动按 config 构建 clamp 1-8;信号量容量不可变,运行中改设置不热更新重启生效)+peer_locks(peer_hex→锁懒创建)+acquire_slot(先全局 permit 后对端锁,同对端串行跨对端放开)+pending_rank(泵式位次近似;ffi 差异:占位 ID 递减故创建序=ID 降序,PC 递增升序)。四个传输入口全量迁移(push_files/push_files_rel/pull_files/backup_push)+spawn_retry_transfer:60s 排队超时→清 queue_pos 残留→QueueTimeout 落 failed(对齐 PC 修复轮 1);卡片路径走 acquire_with_queue_pos(1s tick 刷 dto.queue_pos,拿到槽位/超时后清 None,仅改状态不发事件,UI 展示归 T5);backup_push 无占位卡不做位次。SettingsDto 增 max_active_transfers(settings() 透出/save_settings clamp 1-8 持久化;core store.rs 字段与 serde default 已有直接复用),Kotlin 侧 3 构造点+2 测试构造点补参(uniffi 0.28 无默认参数),SettingsViewModel 无表单项故加载暂存保存透传(同备份字段惯例)。uniffi regen(踩 target/android 滞留旧 so 已知坑——改从 target/x86_64-linux-android/release 取新 so 生成)+errorMessage 手工补丁重做+双 ABI jniLibs .so 同代重编。单测 5 新增:并发上限出队/同对端串行/跨对端并行/位次创建序/queue_pos 置位与清零。验证:cargo test 68 过/11 存量端口类失败(与 T3 基线 63/11 口径一致,新增 5 测全绿);cargo check ✅;gradle assembleDebug+testDebugUnitTest 81 测 0 败(1 存量 @Ignore)。下一站 T5(FR5 Kotlin 传输页)。
- 2026-09-08 T3 完成：ffi 补齐 list_disk_jobs/restore_disk_job/destroy_disk_job 导出（DiskJobDto 对齐 PC 字段，job_id 直传 u64）+ manifest 优先启动重建 rebuild_cards（移植 PC main.rs：孤儿建卡 缺块→interrupted/全真→failed"完整性存疑"，meta.display_name 优先，卡键=engine_id；索引合并 manifest 侧为准、索引补 name/removed/时间戳/fail_reason；gc 在扫盘后）。ffi 特有差异：① parts 根多处（inbox_dir=推送接收落点、config.download_dir=拉取落点、dir=兼容）→ AppState::parts_roots 三根去重全遍历，PC 只有 download_dir；② inbox 根启动时未知 → set_inbox_dir 注入时 merge_orphans_from_root 只补缺不覆盖在表卡 + gc 该根；③ destroy 级终态卡改 finalize_destroy 连带删 parts，活动卡 cancelling 仲裁路径仍 finalize_remove 保留 parts（续传数据交磁盘历史入口决定）；④ restore/destroy 补发 TransferUpdated/TransferDone(removed) 事件（Kotlin 事件驱动，PC 前端按需拉取无此事件）；⑤ TransferRecord 索引加 started_at_ms/finished_at_ms/parts_id/source_path 四个 serde default 字段（旧文件可读）。uniffi regen 顺带补上 T2 漏 regen 的 transfer_remove_level 与 docstring 校验和漂移。验证：cargo test 63 过/11 存量端口类失败（与基线 55/11 口径一致，新增 8 测全绿）；gradle assembleDebug+testDebugUnitTest 81 测 0 败（1 存量 @Ignore）。下一站 T4(FR4 并发队列)。
- 2026-09-08 T0 完成：原"事件流丢事件"假设被三层探针+截图证伪（链路 onEvent→route(subs=1)→collect→offerAsk 全通，重装后亦然）；真根因=e2e 断言层（Compose Button a11y 节点与文本子节点分离致 finder 永假 + ch.tap 坐标对象误用）。修复与验证见 T0 小节。下一站 T1(FR1 FFI 契约)。
- 2026-09-07 T1 完成：dto.rs TransferDto 扩 8 字段(queue_pos/batch_id/children/parts_id/started_at_ms/finished_at_ms/source_path/health)+新增 ChildDto/HealthDto(Default 兼容)；lib.rs 15 处构造点 `..Default::default()` 补齐，事件/表逻辑零改动(children/queue_pos 恒空/None，T2-T4 接入)；uniffi regen 后重做 AppException errorMessage 手工补丁；TransfersViewModelTest 7 处 Kotlin 构造点补 8 参(uniffi 0.28 不生成默认参数)。验证：cargo build ✅；cargo test 29过/11存量失败(与基线口径一致) ✅；cargo ndk arm64 jniLibs .so 重编 ✅；gradle assembleDebug+testDebugUnitTest 80/80 ✅。下一站 T2(FR2 卡片状态机)。

## M2 关门 ✅（2026-09-08）
- T0 92cdeae（弹窗丢失=e2e断言假阴性+testid）/ T1 c7b469d（DTO契约）/ T2 6e719c7（状态机）/ T3 4130d94（磁盘历史）/ T4 53f1efc（并发队列）/ T5 dbfe98b（Kotlin 传输页含 R1 单进度条）/ T6 de96d3d（"退化"根因=系统弹窗遮挡+Ask 超时静默；修僵尸假 SessionDown）/ T7 0b20f29+90f856b（批拉场景两连绿）
- **收官 run-all：8/8 PASS exit=0**（reports/run-all-m2final.log）——三端一致达成
- 遗留移交：①EventRouter CoroutineExceptionHandler 缺失（打磨池）；②OfferSheet peer_name 显示指纹（T5 观感）；③BrowseScreen 多选交互缺失（产品迭代，打磨池）；④手机推送落盘私有目录（公共 Download/LocalTrans 别名仅部分场景）
