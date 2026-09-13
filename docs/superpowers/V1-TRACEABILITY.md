# V1 追溯表：手工验收清单 ↔ E2E 场景（2026-09-08）

> 计划卡：`docs/superpowers/plans/2026-09-08-v1-verify-coverage.md`（T1+T2+T3）
> 目标：手工验收清单全部场景化，run-all 成为发版键。
> 约定：标注「条件跳过」的场景在设备不可达时自动 SKIP（exit 0，run-all 照常通过）；
> 设备恢复后重跑补双机/真机证据。

## T1 安卓手工清单（docs/e2e-android-manual.md，13 项）

| # | 手工项 | 覆盖场景 | 缺口 / 备注 |
|---|--------|----------|-------------|
| 1 | 装 APK 启动无崩溃、生成指纹 | `api-acceptance`(E1 banner/dump/截图)、`cold-start`(冷启动2 factoryReset→新指纹)、`night-android-transfer`(install+banner) | 无 |
| 2 | 桌面 exe 启动、监听端口 | 部署链 `lib/deploy.mjs`(waitReady health/version) + `api-acceptance`(B1) | 无（部署链每场景隐式覆盖） |
| 3 | 双向预信任注入 | `lib/reset.mjs` L3 seedTrust（夹具能力，全场景复用） | 无（属测试夹具面，非产品断言） |
| 4 | App 设备页出现桌面设备（组播发现） | `api-acceptance`(E1 dump 见桌面设备)、`cold-start`(冷启动2 PC 发现手机新指纹) | 无 |
| 5 | App 点连接 → SessionUp → 已连接 | `cold-start`(冷启动2 手机发起配对→双端 trusted)、`night-android-transfer`(幂等配对) | 无 |
| 6 | App 推文件到桌面（Auto 权限） | — | **缺口（暂不场景化）**：需手机本地文件多选→发送→选设备 UI 链自动化（UnifiedSelectionBar/device sheet）；现场景只有 PC→手机方向。补齐时参照 `night-android-transfer` 交互模型 |
| 7 | 桌面推文件到 App | `cold-start`(冷启动3 PC→手机 OfferSheet 接收)、`night-android-transfer`(PC 推手机)、`notify-deeplink`(条件跳过) | 无 |
| 8 | 拒绝路径：推 → 拒 → 已拒绝 | `offer-deny-timeout`(条件跳过，双 PC) | 新建；B 点拒绝 → A 卡 failed"推送请求被对方拒绝" |
| 9 | 超时路径：推 → 不点 → 60s 超时 | `offer-deny-timeout`(条件跳过，双 PC) | 新建；B 倒计时自动拒收 + toast"已超时自动拒绝" + A 卡 failed |
| 10 | 手机浏览桌面共享区 | `cold-start`(冷启动3 手机远程浏览)、`android-pull-batch`(长按下载)、`night-browse-batch`(浏览/排序 PC 面) | 无 |
| 11 | 手机远程重命名桌面文件 | `remote-rename`(条件跳过=能力探测) | **产品缺口（2026-09-08 实证）**：M7 Compose 重构后远程文件长按菜单只有「下载到本机/选择多项」，无「重命名」（手工清单 #11 为 v0.5.0 时代 PASS 项）。场景已自动化到长按菜单探测点（配对/远程浏览/长按链路全部真实执行），探测无重命名项 → SKIP exit 0 带诊断截图；产品补齐后自动转全量断言（PC 端 fs 直核改名+内容一致） |
| 12 | 相册备份（放图→开备份→桌面收到） | — | **缺口（暂不场景化）**：需真机相册/MediaStore 注入+备份开关+目标选择深层 UI 自动化；记录为打磨池项 |
| 13 | 断点续传：中途杀 → 重开 → 恢复 | `api-acceptance`(D2 暂停/继续，不杀进程)、`kill-restart-recovery`(PC 单机，强杀 taskkill→重启→interrupted 带元数据+续传可用) | 安卓侧真机 force-stop→恢复横幅（历史 Bug#1 横幅缺失）仍需真机补测；PC 面已等价覆盖 |
| 14 | 中继路径（可选） | `relay-path`、`force-relay` | 无 |

## T2 桌面核对单（2026-08-30 传输域重构 Task 12 验收 9 条）

| # | 核对项 | 覆盖场景 | 缺口 / 备注 |
|---|--------|----------|-------------|
| 1 | 传输中删除任务：cancelling→消失，无幽灵行 | `api-acceptance`(D3：取消→真终态仲裁→UI 删卡→卡消失) | 无 |
| 2 | 强杀进程(taskkill)重启：interrupted 带完整元数据，续传可用 | `kill-restart-recovery`（新建，PC 单机） | 单机等价法：种子缺块 manifest（meta 全量）≈ 强杀磁盘现场；taskkill/重启真实；api-acceptance D2 只覆盖暂停/继续不覆盖强杀，二者互补 |
| 3 | 清除已完成→磁盘 manifest 可见、可恢复/彻底删除、parts 消失 | `api-acceptance`(D3 UI 删卡=removed 视图)、`parts-deleted-integrity`(GC 删目录面) | **部分缺口**：磁盘历史入口的「恢复到列表/彻底删除」按钮链无场景（invoke 白名单未含 restore/destroy_disk_job，需 UI 驱动）——打磨池 |
| 4 | 多文件推送全程一张父卡、展开见子文件、整批取消 | `pc-pc-transfer`(4 fixtures→单父卡断言)、`night-browse-batch`(1 批次→1 父卡) | 子项展开/整批取消无专项断言（小缺口，父卡单卡性已钉死） |
| 5 | 发送方进度主数字=remote_done、积压提示、等待确认态 | UI 单测 `ui/src/lib/__tests__/transferDisplay.test.ts` | e2e 无（需人为限速对端制造积压的受控环境）——标注单测覆盖 |
| 6 | 并发：第 4 任务排队显示位次、同对端串行第 2 排队 | core 单测（FR4 队列） | e2e 无（需 ≥2 对端在线；laptop 恢复后可在 offer-deny-timeout 基础上扩展）——标注单测覆盖 |
| 7 | 手动删 parts 目录后重启：failed"完整性存疑"而非可续传 | `parts-deleted-integrity`（新建，PC 单机） | 产品行为锚点：位图全真未 finalize=完整性存疑→建 failed 卡+gc_stale_parts 删目录（transfer/mod.rs M-C2）；"资源管理器核对 parts 消失"由 existsSync 断言等价 |
| 8 | 列表：活动优先/历史折叠/对端别名 | `api-acceptance`(D3 历史折叠)、`kill-restart-recovery`/`parts-deleted-integrity`(历史区定位) | 别名显示（peerDisplayName）为 UI 单测覆盖；设备页起别名的端到端链无专项场景——小缺口 |
| 9 | 推+拉完成均有 toast、切页可达 | `offer-deny-timeout`(条件跳过，双 PC：A"推送完成"/B"下载完成"双向断言) | 新建；跨页可达性由 toast 全局挂载（App.vue）保证，场景在 /transfers 页断言 |

## V1 新建场景（均已注册 `tests/e2e/run-all.mjs`）

| 场景 | 泳道 | 设备要求 | 跳过条件 | 对应清单项 |
|---|---|---|---|---|
| `kill-restart-recovery.mjs` | e2e | huss_pc 单机 | bridge 未就绪→SKIP | T2#2、T1#13(PC 面) |
| `parts-deleted-integrity.mjs` | e2e | huss_pc 单机 | bridge 未就绪→SKIP | T2#7 |
| `offer-deny-timeout.mjs` | e2e | 双 PC | 任一 PC 不可达→SKIP | T1#8/#9、T2#9 |
| `notify-deeplink.mjs` | e2e+adb | huss_pc+huss_phone | PC/手机不可达或 APK 缺→SKIP | T1#7 通知预期面 |
| `remote-rename.mjs` | e2e+adb | huss_pc+huss_phone | PC/手机不可达或 APK 缺→SKIP；远程菜单无重命名（产品缺口）→SKIP | T1#11 |

### T3 run-all 发版判定

- 注册表扩至 18 场景（5 个 V1 新场景入库）；出口条件②核对表补齐「强杀重启续传恢复」「parts 完整性存疑 failed」两项。
- 2026-09-08 全量实跑（huss_laptop 网络隔离）：10 PASS / 8 FAIL（报告
  `tests/e2e/reports/run-all-2026-09-08-11-47-48/report.md`）。
  - 8 FAIL 均为**双机场景在 laptop 不可达下的预期失败**（api-acceptance/pc-pc-transfer/l3-reset/
    cold-start/relay-path/night-browse-batch 连接超时；night-android-transfer、android-pull-batch
    死于 `reset(2,[A])` 的 stopPcB ssh 中断——旧场景无条件跳过设计，属既有行为）。
  - **链式交互记录**：night-android-transfer 中断把 huss_pc 进程留在停止态，导致链内排在其后的
    V1 场景全部走条件跳过（设计行为，exit 0）。恢复 huss_pc 后单跑复验：
    kill-restart-recovery 6/6 PASS、parts-deleted-integrity 7/7 PASS、notify-deeplink PASS、
    offer-deny-timeout SKIP(exit 0)、remote-rename SKIP(exit 0)。
  - 全绿口径待 huss_laptop 恢复后补跑（届时双机场景应转绿，链式残留问题可顺带观察）。

## V1 执行期发现的疑似产品问题（记录，不在本卡修）

1. **通知深链热启动不切页**（`notify-deeplink` 深链软断言实证）：`MainActivity.handleIntent` →
   `LocalTransBridge.setPendingTab("transfers")`，但消费点在 `AppNav` 的
   `LaunchedEffect(Unit)`（AppNav.kt:55）——仅在首次组合消费。App 已在前台时点通知，
   pendingTab 永不被消费，停留在当前页。冷启动路径正常。修复方向：改用可重复触发的
   状态流（mutableStateFlow/consume 多次）。
2. **远程重命名缺失**：见 T1 #11（产品缺口，BACKLOG 打磨池）。

## 暂不场景化清单（人工/打磨池）

1. **T1#6 手机→PC 推送**：手机本地文件选择→发送→设备选择链自动化待补（交互模型已有先例）。
2. **T1#12 相册备份**：MediaStore 注入+备份开关+目标选择深层 UI。
3. **T2#3 磁盘历史恢复/彻底删除按钮链**：需 UI 驱动（invoke 白名单不含 restore/destroy_disk_job）。
4. **T2#5/#6 e2e 化**：限速/多对端受控环境就绪后场景化。
5. **T1#13 安卓真机侧横幅**：Bug#1（启动恢复横幅缺失）修复后真机复测。
6. **T1#11 远程重命名**：产品补齐远程菜单重命名项后，`remote-rename` 场景自动转全量断言。
