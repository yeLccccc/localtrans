# P3 执行计划：打磨·交互流程梳理（打磨池清零）

> Spec: specs/2026-09-07-p3-polish-flows.md（收集器型：以打磨池实际内容为准）

## 打磨池当前清单（逐项消化）

### T1 通知深链热启动不切页
- [x] pendingTab 仅在 AppNav LaunchedEffect(Unit) 首组合消费——前台点通知无效（V1 发现，AppNav.kt:55）
- [x] 修：pendingTab 改 MutableStateFlow，AppNav 持续 collect（onNewIntent 重发即触发；冷启动首 collect 消费初值）
- [x] 真机验证：notify-deeplink PASS + 专项热启动检查（设备页停留→am start 等价通知 PendingIntent→落传输页，截图 v1-devices-before/v2-transfers-after，reports/t1-warmstart-mtswme97）
### T2 远程重命名 Compose 移植
- [x] 核对：M7 后远程长按菜单只剩「下载到本机/选择多项」，Rename/Delete/Mkdir 均不在远程 UI；VM renameEntry REMOTE 分支+FFI shareOp(Rename)+协议 ShareRename 全通——纯 UI 缺口
- [x] 修：FileEntryMenuSheet 远程分支恢复「重命名」一项；Delete/Mkdir 按 PC 砍掉定案不恢复
- [x] 真机验证：remote-rename.mjs PASS×2（长按→重命名→对话框追加→PC 落盘改名+内容一致；含最终 APK 回归）
### T3 手机远程多选交互
- [x] 核对：多选骨架已存在（长按「选择多项」/勾选高亮/pull_files 聚合单卡），缺「全选」与可见的批量下载按钮——真机实根因：LazyColumn fillMaxSize 把 UnifiedSelectionBar 挤出屏外，多选栏从未现形
- [x] 修：底栏远程态改「下载(N)」主按钮+「全选」（只选文件）；远程/本机全部两处 LazyColumn weight(1f) 还底栏可见性；远程选中字节合计真实展示
- [x] android-pull-batch.mjs 升级真多选断言（专属子目录播种=全选恰 3 文件；长按→选择多项→全选→下载(3)）——PASS：PC 3 卡 done+字节一致、手机落盘×3、传输页 1 张聚合卡「3 files」done（截图目检 v-multiselect-3/v-phone-pull-batch）
### T4 信任列表缓存滞后
- [x] 修：settingsStore.refreshTrusted()（仅回读信任列表）；App 全局订阅 pairing-result 成功即回读；removeTrusted 本地摘除后回读复同步
- [x] vitest 验证：新增 settings.test.ts 3 用例（事件刷新/移除复同步/失败保列表），全套 205 绿+build 绿
### T5 手机重复设备条目清理
- [x] 核对：PC 设备页同广播名新旧指纹两条=信任表兜底离线卡（旧指纹）+发现层在线卡（新指纹）；发现层缓存 15s 过期，24h 判据落在信任条目 paired_at（unix 秒）
- [x] 修（core device_merge 数据面）：offline 且陈旧（last_seen/paired_at 超 24h）且同广播名另有伴生条目的旧条目从合并视图隐藏；信任表数据不动；无伴生的正常离线常驻卡不隐藏；5 个新单测（归档/缓冲期/无伴生/发现层陈旧/重新上线）
- [x] 验证：core 287 绿（含新 5 用例）+shell 83 绿+ffi check 过；test-api snapshot 同步签名
### T6 事件流 CoroutineExceptionHandler
- [x] 修：LocalTransBridge.applicationScope 加 SupervisorJob+CoroutineExceptionHandler（Log.e 到 LT::kotlin，对齐 LT::kotlin::hooks 命名族；单点失败不连坐 app.start()/事件路由）
- [x] 验证：gradle assembleDebug+testDebugUnitTest 全绿

## 收尾
- [x] 打磨池清零核验（BACKLOG §四：T1-T6 全闭环；「4Hz 进度台阶感」移交 P4；「配对失败路径收集器」随 P2/P3 关门收口）
- [x] gate：core 287 / shell 83 / ui vitest 205+build / android gradle 全绿（relay 泳道无改动未跑；run-all 需 laptop 当前不可达，三场景已单独真机跑绿）

## 执行记录

| 项 | 提交 | 验证 |
|---|---|---|
| T1 | 3cc2e4a | notify-deeplink PASS + 热启动专项（截图） |
| T6 | e684429 | gradle 全绿 |
| T2 | 73e142b + 8fe6161(场景) | remote-rename PASS×2 |
| T3 | 229c2ab + 3549dc7(布局) + 8fe6161(场景) | android-pull-batch PASS（真多选，截图目检） |
| T4 | 29e9b4a | vitest 205+build 绿 |
| T5 | ce90ee6 | core 287+shell 83 绿 |

（2026-09-09 P3 关门：打磨池清零，六项六提交+两项验证期补丁；真机证据 reports/{notify-deeplink-mtswgzi7,t1-warmstart-mtswme97,remote-rename-mtswqzea,remote-rename-mtsxuep5,android-pull-batch-mtsxr4ap}）
