# 中继稳定性管理设计(relay stability)

日期:2026-08-24
状态:已与用户逐节确认
复审修订(2026-08-24):逐条对照代码复核,修正模块 2 事实前提
(端点已有半套 keep-alive,实际是配置不对称)、修正不可编译的代码块、
澄清判死测试语义与回路 2 触发规则。方向与数字(5s/15s)不变。

## 背景与问题

v0.9.0/0.9.1 实测(跨公网,中继出口 5Mbps)暴露的稳定性短板:

1. **NAT 地址漂移不跟踪**:中继数据面只在 KNOCK 时学习客户端地址,NAT
   重绑定(手机切网/运营商超时重分配)后转发全部发往失效地址——传输
   突然 0 字节、设备"隐身"。
2. **会话死亡感知慢且配置不对称**(复审修正):虚拟端点两侧行为不一——
   `server_endpoint` 经 `session::server_config` 已继承
   `quic_transport_config()`(keep-alive 5s + idle 60s);`client_endpoint`
   自建 ClientConfig 漏挂 TransportConfig,落在 quinn 默认(无 keep-alive,
   idle 30s)。initiator 侧存活全靠对端 PING 顺带保命(碰巧,非设计)。
   路径死亡后僵尸窗口 30~60s,期间所有操作超时(浏览页进不去的诱因之一)。
3. **断线后无自愈**:SessionDown 后设备离线、传输落 failed,一切靠用户
   手动重连/手动续传。

前置修复(已上线,commit 37800ef):初始窗口 16→4、单块 60s 硬超时改
idle watchdog(90s 零进展判死)、控制面 MTU 1200、浏览超时重试。

## 用户裁定的约束

- **恢复体验 = A2**:传输中断后短暂卡住,自动重新打通并从断点续传,
  用户零操作。
- **带宽自适应 = B2**:纯客户端探测,零协议改动。现有 500ms 速率/丢包
  探测 + 窗口爬坡(4→32)保留,不在服务端做配额分发。
- **作用范围 = R2**:所有中继会话(含浏览/配对)都保活,不只传输会话。
- 架构 = **方案一(分层自治)**:服务端/端点配置/壳层各改一小块,
  互相独立可回滚;否决集中式 RelaySessionManager(与现有
  SessionManager 职责重叠,改动量 3-4 倍)。

## 设计

### 模块 1:服务端 NAT 漂移跟踪(data_plane.rs)

DATA 分支在查对方地址前,比对收包源地址 `from` 与学习表已记录地址:

```
收 DATA → 验令牌 → 查学习表
  → 若 learned[port][src_fp] != from: 更新 + tracing::info!("NAT 漂移: ...")
  → 查对方 → 转发(路径不变)
```

- 不改 KNOCK 语义(仍显式注册)。
- 不做服务端主动探测(判死归客户端心跳)。
- 客户端发出的第一个数据包即触发重学习,QUIC 重传兜底丢包间隙。

测试:单测模拟 KNOCK 自 A1 → DATA 自 A2 → 断言转发目的随新地址更新。

### 模块 2:客户端 keep-alive 快速判死(virtual_ep.rs)

复审修正:keep-alive 不缺,缺的是对称性——`server_endpoint` 经
`session::server_config` 已继承 `quic_transport_config()`(keep-alive 5s
+ idle 60s),`client_endpoint` 自建 ClientConfig 漏挂 TransportConfig,
落在 quinn 默认(无 keep-alive、idle 30s)。改动:**不新造配置函数**,
两端复用 `session::quic_transport_config()` 现成参数,只把 idle 超时
从 60s 覆写为 15s:

```rust
// virtual_ep.rs 两端点;EndpointConfig 的 max_udp_payload_size(1200)
// 保持原位不动(它是 EndpointConfig 的方法,TransportConfig 没有)
let tc = crate::session::quic_transport_config();  // keep-alive 5s + 流控现成
let mut tc2 = (*tc).clone();                       // Arc → 可变副本
tc2.max_idle_timeout(Some(
    quinn::IdleTimeout::try_from(Duration::from_millis(15_000)).unwrap(),
));
// client_endpoint: client.transport_config(Arc::new(tc2));
// server_endpoint: server.transport_config(Arc::new(tc2));(覆写继承值)
```

- 5s < 运营商 UDP NAT 映射最短寿命(~30s),映射不会过期。
- 15s = 3 个心跳周期,躲开单次抖动误杀。僵尸窗口 30~60s 缩至 15s。
- 配在两个虚拟端点 = R2 全会话保活(浏览/配对/传输共用),调用点零改动。
- 局域网直连端点(session.rs)不动——无 NAT 问题,维持 60s。

测试(集成):判死语义按**断路**验证,不按应用层静默——keep-alive
开着,应用静默连接必须仍活(心跳互保);只有路径死才 15s 判死。两用例:
① 活连接互心跳,应用静默 20s,断言仍存活(证明心跳在工作);
② 停对端收包路径(断路),~15-20s 内断言 closed()。

### 模块 3:壳层自动重连与自动续传(main.rs / commands.rs + FFI)

**回路 1 会话自愈**:事件桥收到 SessionDown 且该指纹来自中继名册
(本地发现列表无此设备)→ 自动 relay_client.connect_peer +
adopt_as_initiator → SessionUp 事件流自然恢复设备在线。
退避 1s/2s/4s,上限 30s 周期;连续 3 次失败停止(对端真下线),
等下次发现再触发。

**回路 2 传输自愈**:传输任务 failed 且有 parts 残留
(可续传)且对端经回路 1 恢复在线(SessionUp)→ 自动重新发起拉取,
走已有 parts 断点续传。**只重试 1 次**,防对端文件已删/权限变化的
无限循环。进度从断点值继续。
(复审澄清:壳层 fail reason 粒度不区分"连接死亡"与其他失败——
触发规则取宽:**任何 failed + parts 残留 + 对端 SessionUp** 即重试
1 次,有界可接受;非连接类失败重试 1 次同样会快速落回 failed,无
无限循环风险。)

**FFI 共用**:两回路逻辑抽成 core 公共函数(relay::auto_reconnect),
桌面壳(main.rs)与安卓壳(localtrans-ffi/lib.rs)共用,不复制。

**UI**:设备卡片"重连中…"(复用 Connecting 样式);自动续传 toast
"连接中断,已自动续传"。无新页面。

测试:E2E——传输中杀中继服务器进程 → 15-20s 判死 → 自动重 punch
(中继重启后)→ 断点续传完成。一测覆盖三模块。

### B2 增量(非本轮重点,记录在案)

现有 AdaptiveStreams 已按 500ms 探测调窗。增量项(后续 backlog):
首块完成前禁止窗口爬升(防冷启动过冲);丢包率 >5% 降窗已有。
本轮不动 adapt.rs。

## 错误处理

- 服务端漂移更新失败(学习表无此端口):维持现状 continue 丢弃。
- 回路 1 punch 失败:退避重试,3 败停止,不弹错误框(静默,设备保持
  离线显示)。
- 回路 2 续传失败:任务落 failed(现有路径),toast 告知,不再重试。

## 测试策略

| 层 | 测试 | 类型 |
|---|---|---|
| relay | NAT 漂移重学习 | 单测(伪造地址变化包) |
| core virtual_ep | 15s 判死 / 心跳互保 | 集成(静默计时) |
| core | 自动重连函数状态机(退避/3败停) | 单测 |
| E2E | 杀中继→自愈→断点续传 | 集成(relay E2E 套件扩展) |

## 不做的事(YAGNI)

- 服务端带宽配额分发(B1,用户否决)
- 集中式 RelaySessionManager(方案二,否决)
- 主动带宽测量(B3,否决)
- 连接迁移(A1 无感切换,否决——quinn 迁移工程量大边缘多)
- adapt.rs 冷启动限爬(B2 增量,记 backlog)
- 服务端指标面板(#5 观测,未纳入本轮)
