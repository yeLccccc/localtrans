# M3a 执行计划：智能选路·地基（名片/多地址/会话域散项）

> Spec: specs/2026-09-07-m3a-smart-routing-foundation.md
> 顺序：FR1(地址枚举,core 地基) → FR2(relay 公网回报) → FR3+FR4(名片) → FR5(连接记忆制) → FR6(散项) → 场景。
> 纪律：core 改动每步 gate（core 串行为准）+ 契约测试先行；跨端字段走 ffi regen。

## 任务分解

### T1 FR1: 全网卡地址枚举
- [ ] core 新增 `net_addrs.rs`（或 util）：枚举全部非环回 IPv4 地址（接口名标注；过滤虚拟网卡黑名单：vEthernet/WSL/Hyper-V/VMware/Tailscale 等常见前缀+接口类型判断）
- [ ] get_network_status 返回 `local_ips: Vec<{ip, if_name}>`（保留 local_ip 兼容=首选）
- [ ] ffi DTO 镜像 + uniffi regen
- [ ] 单测：过滤规则纯函数直测
- 提交：`feat(core): 全网卡地址枚举(虚拟网卡过滤)`

### T2 FR2: relay 公网出口回报
- [ ] relay proto RegisterAck 增 `observed_addr: Option<String>`（服务端从注册包源地址取）
- [ ] 客户端存入 relay_client 状态并暴露（get_network_status 增 public_exit）
- [ ] relay 门禁（--test-threads=1）+ 客户端单测
- 提交：`feat(relay): 注册应答回报公网出口observed_addr`

### T3 FR3+FR4: 名片体系
- [x] core `business_card.rs`：名片结构（设备名/指纹/地址列表/公网出口?/中继地址?）+ 文本序列化（人可读多行+解析容错）+ 单测 roundtrip
- [x] 生成命令 get_business_card + 解析添加 add_by_card（解析→多地址入 probe_targets→5s 回探 toast 回执——复用手动添加 IP 路径）
- [x] ffi 导出镜像——**定案暂不导出**：Android 手动添加是 IP 录入面，名片属 PC 优先（粘贴文本在手机侧无人走），等 Android 添加面扩充时随 T5-独立做
- [x] 场景：粘名片→发现→配对（`scenarios/card-exchange.mjs`，条件跳过模式参照 connect-memory，已注册 run-all）——广播隔离手段（防火墙挡广播需管理员/端口错开）待双机联调定案，骨架先验名片通路+命令级断言
- 提交：`feat(core): 名片体系(生成/解析/粘贴添加,多地址入重探)`

### T4 FR5: 连接记忆制
- [x] core TrustStore 增 connect_memory（用户主动连过=记；持久化）——落地为独立 `core::connect_memory`（`data/connect_memory.json`，不动信任表格式：信任文件会被 E2E seedTrust 夹具整文件重写，附加字段会被静默丢弃）
- [x] 启动与断线自动重连：退避 2/4/8/16s 封顶 30s，连续 5 败回落停止（带抖动）；配对成功自动连接一次（配对本身运行在已建 QUIC 连接上，complete_pairing 后即 SessionUp，天然达成）
- [x] 单测：退避序列/5 败回落/记忆命中（core 6 例：退避/回落/抖动边界/roundtrip/损坏容忍/格式钉死；壳 1 例 remove_trusted 清记忆）
- [x] 场景 connect-memory.mjs（双 PC：重配对→杀对端→自动重连→清记忆不再连；任一端不可达条件跳过 exit 0，已注册 run-all）——**待 huss_laptop 恢复执行**
- 注：编排落壳层 `src-tauri/src/reconnect.rs`（有 discovery/session 全上下文），core 只出数据结构+持久化+退避纯函数；ffi/Kotlin 不动（Android 重连属后续）
- 提交：`feat(壳): 连接记忆制自动重连(退避2-30s,5败回落)`

### T5 FR6: 会话散项
- [x] TrustBroken：移除信任时向对端发协议消息（ControlMsg 增 TrustBroken；对端收→断开+降级+toast）——**跨端协议变更**：先单独提交协议定义（core 两侧同仓，一次提交）
  - 定案：无载荷纯信号（对齐 Goodbye；收端按 TLS 对端指纹定位会话）；wire 形态 `{"type":"trust_broken"}` 有钉死测试；旧端收到=未知变体→解码失败→干净断连（终态与发送方关连接等价，有模拟测试）
  - 发送：壳层 `remove_trusted_inner` 清记忆后、disconnect 前 `send_ctrl(TrustBroken)`（会话不存在=send_ctrl 报错即跳过；失败仅日志不阻塞本地移除）
  - 接收：core ctrl_loop 增分支——删本端信任条目（定案=双盲对称删除，徽章即时降级待配对；注释在案）+ `SessionEvent::TrustBroken{fingerprint, peer_name}` + break 走 Goodbye 同款收尾（清表+SessionDown）
  - 壳层事件泵：toast「已移除对你的信任，连接已断开」+清连接记忆+设备列表刷新；回路1 自愈与回路3 重连均加"仍受信"守卫（对 TrustBroken 与本地移除双覆盖）；SessionUp 自然解除
  - ffi：最小映射=新 SessionEvent 变体 → `DevicesChanged`（紧随的 SessionDown 承载既有断连展示），不新增 AppEvent 变体、免 uniffi regen；ffi 回路1 同加信任守卫。单测：ctrl_loop 收 TrustBroken 清表+事件序列；协议 roundtrip+wire 形态
- [x] 静默期广播 5s→15s（无对端活跃时；有活动会话/传输恢复 5s）
  - 落地 `presence_period_for(peers_online: bool)`：表空→14-16s，非空→4-6s（原节奏）；数据源=现有设备表（活动传输必依赖在线对端，不单独判）；切换带 debug 日志；bootstrap（启动/改名 burst、ProbeResp）不受影响
  - 单测：`presence_period_selects_by_peer_state`（双档边界各 50 次采样）
  - e2e 发现等待复查结论：**无需放宽**——各场景发现断言均紧跟进程重启（启动 burst 立即可见），或窗口≥30s 覆盖静默期一个完整周期（15s+2s 抖动）
- [x] 隐身语义修正：隐身只约束被看见（出站 probe 放行）
  - 放行三处出站：ProbeNow 广播探测 / ProbeAddr 单播手动探测 / probe_targets 周期重探（含持久化目标——否则隐身设备重启后跨网段永久失忆）；保持不动：Presence 广播（启动 burst/改名/周期）、被探测不回 ProbeResp、中继 Register.hidden 名册剔除
  - 单测：`hidden_device_probes_outbound_but_stays_hidden_from_probes`（出站放行面 A 见乙 + 入站自隐面 第三方丙不见甲）；端口 1683x 独占（1483x 与 set_name 测试共用，SO_REUSEADDR 并行单播抢占——顺带排掉一颗历史地雷）
- [x] 各自单测+场景断言
  - 场景 `scenarios/trust-broken.mjs`（双 PC，条件跳过模式参照 connect-memory；**待 huss_laptop 恢复执行**）：单边移除→对端 toast+断连+信任对称删除+10s 无自动重连；已注册 run-all；SKIP 路径实测 exit 0
- 提交：三笔——`feat(core): TrustBroken协议通知(移除信任即时断连降级)` / `feat(core): 静默期广播15s(有对端恢复5s)` / `feat(core): 隐身语义修正(出站探测放行,被看见仍受控)`

### 收尾
- [ ] 新场景接入 run-all；gate 三泳道；RELEASE/BACKLOG 状态 → M3b

## 执行记录
（进行中——T1 派发）
- T4 完成（2026-09-08）：core `connect_memory`（记忆表+持久化+backoff/give_up/jitter 纯函数，6 单测）；壳层 `reconnect` 模块（启动扫描+SessionDown 触发，每设备独立退避任务，防重入注册表，发现表无地址不计失败，回落停止后借 absent→present 跃迁重武装，静默重连仅成败各一条 toast）；connect 命令幂等跳过已建会话+成功记记忆；remove_trusted 先清记忆再断会话；E2E 场景条件跳过设计待双机。gate core/shell 全绿 + workspace check 通过。超出原任务卡文件范围的备注：`tests/e2e/lib/reset.mjs` L3 清单纳入 connect_memory.json（出厂重置语义记忆清零，防夹具期自动重连噪声）、`docs/e2e-harness.md` 场景命令一行。
- T5 完成（2026-09-08）：三项散项按上述定案落地（分支 `agent/m3a-t5-session-items`，三笔提交）。gate core 235 绿（含串行 --test-threads=1 口径 54 例）+ shell 71 绿 + workspace/ffi check 绿（ffi 无 regen）。trust-broken.mjs SKIP 路径实测 exit 0。遗留待双机：trust-broken.mjs 与 connect-memory.mjs 真机执行（huss_laptop 恢复后）；ffi/Android 侧收到 TrustBroken 无专用 toast 文案（仅 SessionDown 断连展示，待 Android 事件面扩充）；隐身周期重探放行后，隐身设备若仍在对方 probe_targets 表内会被对方表持续刷新（"出站放行"的预期副作用，非缺陷）。
- T3 完成（2026-09-08）：core `business_card.rs`（名片结构+to_text/parse，解析容错=中英文冒号/空白/CRLF/BOM/噪声行/缺可选行、fail-closed=指纹 64hex/地址 ip:port 端口非 0/名称无控制符/总量 8K 与地址 16 条封顶，敏感行（PSK/私钥类）一律丢弃不复活，17 单测）；名片地址口径=ip:quic_port（与发现表/直连同语义），壳层组装=device_name+identity 指纹+net_addrs 全网卡×config quic_port+relay observed_addr+config relay_server（仅中继启用时）；add_by_card 探测目标取卡片 IP+本机发现端口（ports 同源，与手动添加裸 IP 补发现端口同约定），逐地址复用 add_manual_device 的 5s 回查 manual-probe-result 回执（抽公共 helper probe_addr_with_receipt），本机自身名片 accepted=false 拒绝；test_api 白名单扩列 get_business_card(ReadOnly)/add_by_card(Mutating——写 probe_targets+可触发对端配对弹窗，语义同 connect；任务卡原文"两项 ReadOnly"按分类语义修正为 Mutating，契约文档 §5.5 已同步)。gate core 252 绿 + shell 全绿。card-exchange.mjs 条件跳过设计，SKIP 路径实测 exit 0。超出原任务卡文件范围备注：`docs/e2e-harness.md` 场景命令一行（T4 同款）、`docs/contracts/test-api.md` §5.5+changelog（白名单封闭清单要求同步）。遗留待双机：card-exchange.mjs 真机执行（含广播隔离手段定案，huss_laptop 恢复后）。

## M3a 收官状态（2026-09-08）
- T1 82ee903（全网卡枚举）/ T2 1b329e7（relay 公网回报）/ T3 0ed9e7a（名片体系，add_by_card 按 Mutating 定类复核通过）/ T4 9ac4702（连接记忆制）/ T5 4dd85d0+c44014c+5455d3d（TrustBroken/静默广播/隐身修正，分支合回 main）
- **M3a 代码面 100% 完成**；双机遗留（真机场景补证：card-exchange/trust-broken/connect-memory + T1 运行时面）统一挂"待 huss_laptop 恢复"清单
- M3b（探测选路）可开工：M3a 地基（多地址/公网出口/通道数据源）已齐
