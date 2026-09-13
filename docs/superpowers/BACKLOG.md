# LocalTrans 功能盘点与差距清单（BACKLOG）

> 2026-09-07 建立。**规划职能已上移至 `RELEASE.md`（v1.0 发版定义）**——本文退化为
> RELEASE 出口条件①的展开清单（差距明细+spec 计划），不再新增范围。
> 流程约定（§三）继续有效。维护纪律：差距关闭后当日更新对应行。

## 一、现有功能全景（as-built，2026-09-07）

| 域 | 能力 | 验证状态 |
|---|---|---|
| 发现与连接 | UDP 广播发现/probe 对称/在线判定/手动添加(IP)/隐身/改名即时广播/重探持久化 | e2e: api-acceptance B/D1 ✓ |
| 配对与信任 | 同意门+6位码+防暴力/权限三维/移除信任/信任列表 | e2e: 安卓场景全自动配对闭环 ✓（P0-6 修复后任意页面可达） |
| 浏览与下载 | 共享区管理/远程浏览分页/文件夹整棵下载/**多选批次下载→1父卡片**/**本地排序** | e2e: night-browse-batch 真机 PASS ✓ |
| 推送 | 拖放/向导/卡片菜单三途径/Ask+Auto+秒传/断点续传/暂停续取消 | e2e: night-probe 四轮 ✓（含取消后再推即得槽） |
| 传输管理(v0.12) | 单卡片状态机/ID恒定/两级删除+历史/并发闸门+排队位次/父子卡片/双进度/别名/完成toast | e2e: 全量 ✓ |
| 中继 | 注册/名册/打洞/密文转发/控制面重连/对端自愈/失败自动续传 | 单测 ✓，e2E 场景未覆盖 |
| Android | 媒体浏览/推送/拉取(多选聚合+全选+批量下载,2026-09-09 P3 真多选闭环)/远程重命名(2026-09-09 恢复)/配对/中继拉模式/秒传徽标/单进度 | 推送段真机 ✓；批拉真多选 android-pull-batch ✓（P3） |
| 测试基建 | test-api 17端点/双PC部署重置取证/adb通道/panic钩子/4个真机场景 | 13/13 验收 ✓ |

## 二、待补功能（差距 → spec 计划）

按优先级分四批，每批一个 spec 走完整流程：

### A 批：稳定性与测试地基 = RELEASE 里程碑 M1（spec: `2026-09-07-stability-foundation`）
| # | 差距 | 来源 |
|---|---|---|
| A1 | L3 出厂重置（删数据目录+信任库播种）——根治夹具漂移（旧共享路径/身份残留/信任脏数据） | e2e spec M8 遗留 |
| A2 | 手机长时间运行退化（post_handshake 挂起，重启恢复）根因与修复 | 本周实测发现 |
| A3 | 测试端口可配置化 LOCALTRANS_TEST_PORT_BASE（P0-1）——解锁并行泳道+ffi 存量 7-8 失败 | TASKS.md P0-1 |
| A4 | Android 批量拉取 UI 自动化收尾（多选交互模型）+ 夜批场景选择器修正（tapText 模式） | T3 遗留 |

### B 批：Android 传输域对齐 = RELEASE 里程碑 M2（spec: `2026-09-07-android-transfer-parity`，契约先行；并入手机运行时退化根因与批拉UI收尾）
| # | 差距 | 来源 |
|---|---|---|
| B1 | FFI TransferDto 扩字段（queue_pos/batch_id/children/parts_id/health/时间戳） | 8/30 重构计划明示"后续批次" |
| B2 | FFI 卡片状态机（cancelling 仲裁/终态吸收/engine→card 映射） | 同上 |
| B3 | 两级删除+磁盘历史（list_disk_jobs 等 FFI 导出） | 同上 |
| B4 | 并发队列+排队位次（max_active_transfers 配置） | 同上 |
| B5 | 父子卡片 UI（子展开/双进度增强/分区） | 同上 |

### C 批：智能选路 = RELEASE 里程碑 M3（16 篇整篇，拆三个 spec）
| # | 差距 | spec |
|---|---|---|
| C1 | 名片体系+本机全网卡地址枚举+relay 注册回报公网出口（core+relay 地基） | `smart-routing-1-foundation` |
| C2 | 阶梯探测（64KB/512KB/4MB）+RTT+通道记录+评分选路（迟滞≥15）+退化切换 | `smart-routing-2-probe-score` |
| C3 | UI：通道标签+通道面板+强制走中继+名片复制/粘贴添加 | `smart-routing-3-ui` |

### D 批：设备页定案散项与小项（spec: `2026-09-07-devpage-misc`）
| # | 差距 |
|---|---|
| D1 | 连接记忆制自动重连（退避 2/4/8/16s 封顶 30s，5 败回落）+配对成功自动连接 |
| D2 | TrustBroken 协议通知+即时降级+toast |
| D3 | 静默期广播 5s→15s；隐身语义修正（出站探测放行）；卡片独立推送按钮 |
| D4 | 小项：设置页信任列表缓存滞后/手机重复设备条目清理/offer 弹窗 testid |

### E 批：测试场景库补全（并入 A 批 spec 或独立小 spec）
| # | 差距 |
|---|---|
| E1 | M9：13 项安卓手工清单+桌面手工清单 → 场景库；run-all 入口 |
| E2 | 中继场景（防火墙强制走 relay 路径）|
| E3 | 零信任冷启动覆盖常态化（每场景含新用户路径——P0-6 教训） |

## 三、执行流程约定（superpowers）

1. **每批一个 spec** → `docs/superpowers/specs/<日期>-<名>.md`（目标/边界/验收标准/风险）；
2. **writing-plan** → `docs/superpowers/plans/<同名>.md`（任务分解+TDD 步骤+门禁+e2e 验证点）；
3. **subagent 执行**：单泳道任务派 general-purpose agent 按计划执行；主会话只做评审与集成；
4. **e2e 确认**：改动涉及传输/配对/浏览域必须跑对应真机场景+截图目检；
5. 门禁：改哪条泳道跑哪条 gate；core 并行 flaky 时以 `--test-threads=1` 为准（存量，P0-1 根治）。

## 四、打磨池（M1-M3 期间随做随记，P 阶段成 spec 消化，不中途插队）

- [已修 2026-09-08 P1] 大文件双进度条 → 只期望一个（定案修订 R1）——PC 撤角标/已发小字,双端单进度+"等待对方确认"文案态
- [已修 2026-09-08 P1] 网速一卡一卡瞬时报大 → 平滑（R2）——双端 EMA τ=2s+滑窗尖峰封顶+500ms 显示节流,speed-curve 场景验收
- [已修 92cdeae] 跨端弹窗丢失——根因是 e2e 断言假阴性(Compose 按钮双节点)+OfferSheet testid 缺失;事件链实测无丢失(replay=8 保留)
- [2026-09-07 M1收官] laptop schtasks 电池条件静默不跑(relay/PB 任务踩 M6 同款坑)——已修入 relay.mjs(Set-ScheduledTask)
- [2026-09-07 M1收官] laptop 截图 500(schtasks 会话显示器枚举空)——场景已 soft 降级,环境问题
- [已收口 2026-09-09 P3] 配对失败路径/流程不合理项（P2/P3 收集器）——P2 配对健壮性关门（61f8040），P3 为末站收集器，打磨池清零
- [已知→P4] 4Hz 进度台阶感（性能域，移交 2026-09-07-p4-polish-performance）
- [新 2026-09-09 RC] 中继服务器地址不支持域名——validate_relay_config 只接受 SocketAddr 字面量（IP:端口），relay.example.com:9443 保存被拒；公网中继部署实测发现（relay-public 场景首跑 FAIL 定位）。临时路径：填静态公网 IP（阿里云 IP 固定不受影响）。修法：validate 放宽 host:port + 连接期 tokio lookup_host 解析（core relay/client 单点，三壳共用），随 v0.13.x
- [已修 2026-09-09 P3] 信任列表缓存滞后——settingsStore.refreshTrusted()+pairing-result 事件驱动回读（29e9b4a，vitest 3 用例）
- [已修 2026-09-09 P3] 手机重复设备条目——device_merge 视图归档同名陈旧离线旧条目（ce90ee6，5 单测）；顺带真机实证多选底栏被 LazyColumn 挤出屏外一并修（3549dc7）

## 五、spec 总账（至 v1.0 共 11 个，已全部建档 docs/superpowers/specs/）

| # | spec 文件 | 站 |
|---|---|---|
| 1 | 2026-09-07-m1-stability-foundation | M1 |
| 2 | 2026-09-07-m2-android-transfer-parity | M2 |
| 3 | 2026-09-07-m3a-smart-routing-foundation | M3 |
| 4 | 2026-09-07-m3b-smart-routing-probe-score | M3 |
| 5 | 2026-09-07-m3c-smart-routing-ui | M3 |
| 6 | 2026-09-07-v1-verify-coverage | V |
| 7 | 2026-09-07-v2-verify-cross | V |
| 8 | 2026-09-07-p1-polish-transfer-card | P（承载修订R1/R2） |
| 9 | 2026-09-07-p2-polish-pairing | P |
| 10 | 2026-09-07-p3-polish-flows | P |
| 11 | 2026-09-07-p4-polish-performance | P |

流水线：逐 spec writing-plan → subagent 执行 → 自动化验收 → 微调下一 spec → 循环至 RC。

## 六、当前批次指针

- **进行中**：P4 性能打磨（待派）；V1 已关门；V2 待 laptop；M3 已关门；P1/P2/P3 已关门（P3 打磨池清零）
- **顺序**：M1→M2→M3→V1/V2→P1→P2→P3→P4→RC
- 变更记录：2026-09-07 建档；2026-09-07 增打磨池/定案修订R1R2/spec总账（用户节奏对齐）；2026-09-09 P3 关门——打磨池清零（T1-T6 全闭环，4Hz 台阶感移交 P4）
