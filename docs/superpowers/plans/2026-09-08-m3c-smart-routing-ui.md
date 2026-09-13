# M3c 执行计划：智能选路·呈现与设备页收尾

> Spec: specs/2026-09-07-m3c-smart-routing-ui.md
> 数据源已就绪（M3b：ffi channels()/PC list_channels/probe 调度器）。M3b-T4 已确认 ffi 通道表**生产者缺失**（SessionUp 登记+全量探测是 PC 壳写路径，ffi 红线未做）——T0 先补 ffi 生产者，否则 Android UI 永远空表。

## 任务分解

### T0: ffi 通道表生产者（前置修复）
- [ ] ffi 侧镜像 PC probe.rs 的 SessionUp 登记+全量探测+调度循环（ffi/src/lib.rs start 内 spawn；复用 core routing 原语）
- [ ] 验证：装机后 channels() 非空（配对直连 PC 后 dump 断言）
- 提交：`feat(ffi): 通道表生产者(SessionUp登记+探测调度)`

### T1: 通道标签（设备卡）
- [ ] PC DeviceCard.vue 地址行改「直连 · 2ms」/「经中继 · 120ms」/「未知」；数据源 list_channels（PC）/channels()（安卓）
- [ ] 安卓 DeviceScreen 设备卡同构
- [ ] 三态截图目检
- 提交：`feat(ui): 通道标签(直连/经中继/未知,双端)`

### T2: 通道面板
- [ ] PC：点击标签弹面板（每地址一行：addr/RTT/估速/✓当前/最近探测时间+重测按钮=手动触发快检）
- [ ] 安卓同构（BottomSheet 惯例）
- [ ] 数据一致性断言（e2e 读通道表对照面板渲染）
- 提交：`feat(ui): 通道面板(双端)`

### T3: 强制走中继
- [ ] 卡片 ⋮ 菜单开关（持久化 per 设备）；开启时 connect 决策跳过评分直选中继通道
- [ ] 卡上角标可见；e2e：开→会话 viaRelay 断言→关→恢复
- 提交：`feat(壳): 强制走中继开关(排障后门)`

### T4: 设备页收尾
- [ ] 卡片独立「推送」按钮（直开向导步 2——机制已通只差入口，DeviceCard 操作区）
- [ ] 本机 IP 徽章升级名片一键复制（M3a get_business_card 接入 UI）
- [ ] 手动添加弹窗支持粘名片全文（add_by_card 接入——解析失败提示格式错误）
- [ ] 截图目检+e2e
- 提交：`feat(ui): 设备页收尾(推送按钮/名片复制/粘贴添加)`

### 收尾
- [ ] run-all 全量（含条件跳过场景标注）；gate 三泳道；RELEASE/BACKLOG → M3 关门 → V1/V2

## 执行记录
- T0+T1 完成：40d6497（ffi 通道生产者）+ 71e9e0f（通道标签双端，真机"直连·20ms"像素目检）
- T2+T3 完成：20f344b（通道面板 ChannelPanel+强制走中继开关 force_relay_map+viaRelay e2e force-relay.mjs 条件跳过；Kotlin 测试时序修——init loadDevices 需 emitEvent+advance 装载）
- T4 待派；双机遗留挂起（laptop 不可达）

## M3c 关门 ✅（2026-09-08）
- T0+T1 40d6497/71e9e0f（ffi 通道生产者+通道标签双端，真机"直连·20ms"目检）
- T2+T3 20f344b+e8c351f（通道面板+强制走中继开关，viaRelay e2e 条件跳过就绪）
- T4 31087d8（推送按钮/名片复制/粘贴添加，166 vitest 全过）
- 双机遗留：通道面板三态截图（laptop 恢复后 PC 端复拍）、force-relay/routing-probe 真机跑
