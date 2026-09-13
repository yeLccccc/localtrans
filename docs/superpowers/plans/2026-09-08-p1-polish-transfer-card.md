# P1 执行计划：打磨·传输卡视觉与速度平滑

> Spec: specs/2026-09-07-p1-polish-transfer-card.md（承载用户定案修订 R1/R2）
> R1=传输卡只保留一个进度条；R2=网速显示平滑（瞬时报大→EMA+滑动窗口）。

## 任务分解

### T1 R1 核对: PC 传输卡单进度条
- [x] 现状核对：PC TransferItem.vue 在 v0.12 已实现什么形态（M3c 后 Android 已按 R1 落地；PC 端检查 progressPair/senderDisplayState 的**渲染面**——确认没有第二条进度条/积压角标残留，只有"等待对方确认"文案态）
  - **核对结论（2026-09-08）**：渲染面本就只有一条进度条（无第二 bar）；但存在三处 R1 残留——①`网络积压`角标（backlog>20% 渲染，违反"只显文案态"）②`已发 X` 辅助小字（与单进度=对端确认语义冲突）③缺"等待对方确认"文案态（Android M2 已有 `transfer-awaiting-confirm`）。数据面 progressPair/senderDisplayState 保留不动。
- [x] 若有双条/角标残留 → 移除渲染（数据面保留）；若无 → 记录核对结论（已撤角标+已发小字，新增 `transfer-awaiting-confirm` 文案态：source-push + active/paused + 非 normal 态，对齐 Android `isAwaitingConfirmText`）
- [x] vitest/截图（TransferItem 29/29 绿：单 bar 断言×1、角标/已发撤除断言×3、文案态断言×4；截图见 speed-curve 报告）
### T2 R2 速度平滑
- [x] 数据源：速度来自 Rust 4Hz 泵（speed_bps 逐事件）。平滑在**前端显示层**做（不动 Rust 发送频率）：
- [x] transfers store 或 TransferItem 计算层加 EMA（窗口 ≥2s）+ 最小刷新间隔 500ms + 块边界尖峰抑制（计入窗口不当帧）
  - 落地：`ui/src/lib/speedSmooth.ts`（EMA τ=2s + 1s 滑窗 + 尖峰按窗均速封顶 + 500ms 显示节流 + 归零豁免）；store 每任务一 smoother，speed_bps 进表前过平滑（终态清零+清 smoother）；ETA 在组件层随平滑速度。Android 同构 `SpeedSmoother.kt` 接入 TransfersViewModel（SpeedEstimator 保留为 raw=0 回退）。
  - 实测修正（两轮）：①ema==0（挂起归零后/首样）时尖峰抑制同样生效——Rust 泵首发窗口 dt 极小会出现 131MB/s 级瞬跳，原"ema>0 才抑制"会直爬满；②输入基准改 3s 滑窗 done 差分（进度导数，跨块周期）——逐拍 raw 在尾段停滞时数倍虚高（run-all 实测 27.2 vs 中位 7.5）、慢速块节奏下又低报（0.6 vs 真实 2.3）；3s 环差分后显示与真实速率逐点贴合。
- [x] ETA 用平滑后速度
- [x] 单测：EMA 计算/尖峰抑制/窗口边界（TS 17/17：EMA 序列/收敛/恢复跟随/尖峰/零基线首发/归零/节流/formatSpeed；Kotlin 12/12 同口径）
### T3 验收
- [x] **速度曲线场景**：500MB 推送全程采样显示速度序列，断言无 >3× 中位数的单点尖刺（写进场景可自动判定——speed-curve.mjs 或扩展 api-acceptance）
  - 落地 `tests/e2e/scenarios/speed-curve.mjs`（对端自适应 laptop→phone，双端不可达条件跳过 exit 0；已注册 run-all；采样 DOM 显示速度 200ms/拍+Rust raw 对照；断言尖刺/爬升/归零三段）
- [x] 双端截图目检（PC+Android 单进度条一致）
- [x] 全场景回归绿（vitest 191/191 + gate ui + gate shell 82/82 + Kotlin 5 套件 55/55 + speed-curve + night-android-transfer + run-all 关键场景）
### 收尾
- [x] 门禁；RELEASE P1 状态；打磨池 R1/R2 关闭

## 执行记录
（2026-09-08 完成：T1/T2/T3 全落地。平滑参数：输入基准=3s 滑窗 done 差分(进度导数,raw 仅首拍回退)+EMA τ=2s+1s 滑窗尖峰按窗均速封顶(3×判定,ema==0 同样生效)+500ms 显示节流+零速满 τ 归零，双端同构。速度曲线最终验收（500MB PC→手机，链式 run-all 内 6/6 PASS）：显示中位 2.0MB/s、最大 3.4（raw 泵峰值 130.7，≤3×中位 ✓）、爬升可见（首 2s 0.1→2.0）✓、显示与真实速率逐 20s 检查点贴合（2.4/2.8、2.0/1.7、2.4/2.4…）✓、终态归零 ✓。）

## P1 关门 ✅（2026-09-08）
- 9a42abe R1 核对落地（PC 渲染面本就单条，撤积压角标+已发小字残留，补等待确认文案态对齐 Android）
- dfacafa R2 速度平滑（**输入基准=3s 滑窗 done 差分**——两轮 E2E 实测逼出：逐拍 raw 尾段虚高 3.6×、慢速低报 4×；EMA τ=2s+3×尖峰封顶+500ms 节流；ETA 随动；SpeedSmoother.kt 双端同构）
- 验收：speed-curve 场景 6/6（raw 泵峰值 130.7 削平至 3.4，≤3×中位✓；爬升可见；检查点贴合；归零✓）；vitest 196、shell 82/82、Kotlin 56/56、run-all 关键场景 5/5
- 双机遗留：night-android/notify-deeplink 待 laptop
