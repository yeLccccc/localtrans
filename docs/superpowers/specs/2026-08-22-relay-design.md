# 中继(Relay)功能设计 — v0.3.0

日期:2026-08-22
状态:已与用户逐节确认(架构/数据流/错误处理/测试与切分)

## 1. 目标与需求结论

跨公网(家 ↔ 公司等)互传文件。中继部署在阿里云 Ubuntu 服务器,既是发现服务器也是数据中转服务器。

**需求澄清结论**(全部用户确认):

| 决策点 | 结论 |
|---|---|
| 使用场景 | A:纯自用,预共享密钥(PSK)鉴权,无注册体系 |
| 数据路径 | C:纯中继起步,协议预留直连扩展位(名册带 `relayed_ephemeral` 字段) |
| 局域网 | **硬约束:现有局域网直连路径一行不改**,中继关闭时行为与 v0.2.9 逐字节一致 |
| 离线暂存 | A:不做。纯管道、零落盘、服务器无状态 |
| UDP 封锁 | A:v1 只走 UDP;载体协议留版本号字段,将来可加 TCP 443 兜底 |
| 部署 | A:单个静态二进制 + systemd,目标 Ubuntu;配置 TOML |

## 2. 方案选型

三个候选:

- **A:UDP NAT 式中继(选定)** — 中继模拟虚拟交换机,设备租端口,UDP 包层转发
- B:QUIC-in-QUIC 隧道 — 双重拥塞控制/双重加密,吞吐 8-9 折,否决
- C:应用层中继 — 中继见明文元数据,违背零知识原则;要两套代码路径,违背局域网零改动,否决

A 的三块自定义逻辑(令牌/租约/打洞)均为已解形态,换来:单层加密单层拥塞控制(吞吐最优)、中继纯转发 CPU 极低、QUIC 连接迁移免费获得。A/B 共享 90% 骨架,数据面可局部换 B 不推倒重来。

## 3. 架构总览

```
┌─────────── 家里 PC(A)───────────┐      ┌─────────── 阿里云 Ubuntu ───────────┐
│ [现有局域网端点] ←─完全不动─→ 局域网设备 │      │  localtrans-relay(新crate,单二进制) │
│ [虚拟端点·新] ══密文UDP═══════════┼──────┼─▶ ① 控制面 QUIC:9443                │
│  (quinn跑在自定义socket上)           │      │     认证/名册/打洞通知               │
│                                     │      │ ② 数据面 UDP 租约端口池 9000-9100   │
└─────────────────────────────────────┘      │    (纯转发,不解密)                  │
         ┌──────── 公司电脑(B)────────┐      │                                     │
         │ [虚拟端点·新] ══密文═════════┼──────┘
         └─────────────────────────────┘
```

### 组件清单

| 组件 | 位置 | 职责 |
|---|---|---|
| `localtrans-relay`(bin+lib) | `crates/localtrans-relay` | 服务端:控制面(认证/名册推送)+ 数据面(租约端口 UDP 转发)。lib 暴露 `RelayServer::start()`,集成测试进程内起 |
| `relay_client` 模块 | `crates/localtrans-core/src/relay/` | 客户端:控制连接管理、虚拟端点工厂(实现 quinn `AsyncUdpSocket`,收发包加/剥 18B 令牌头) |
| `relay_proto` 模块 | core 内,两端共用 | 控制消息定义(serde,沿用 `ControlMsg` 风格) |
| 粘合层 | `src-tauri` | 设置项、名册并入设备列表(按指纹合并,远程带标记)、连接路由 |
| UI | `ui/` | 设置页"中继"区块;设备列表远程徽标 |

### 密钥体系

- 控制通道 QUIC:服务端自签证书 + TLS exporter 通道绑定做 PSK 证明(防 MITM 代理)
- PSK 部署时写进服务端 TOML;客户端设置填同一把
- 数据面每包 18B 头:`[版本1B][标志1B][令牌16B]`,注册时签发

## 4. 协议与数据流

### 4.1 控制面消息(relay_proto)

```rust
// 客户端 → 服务器
Register { name, fingerprint, token_req: Option<TokenReq> }
Ping
Punch { target_fp }
Leave

// 服务器 → 客户端
RegisterAck { lease: Option<LeaseInfo> }        // 数据面端口+令牌
Roster { devices: Vec<RemoteDevice>, rev }      // 名册推送(有变即推,全量+rev)
PunchNotif { target_fp, target_lease }
PunchResp { ok, reason }
Error { code, msg }
```

`RemoteDevice { fingerprint, name, lease_addr, relayed_ephemeral }`

### 4.2 上线注册时序

1. 客户端 QUIC TLS 1.3 → 服务器:9443(PSK 通道绑定)
2. Register{fp, name} → 认证通过 → 端口池分配租约 + 签发令牌
3. RegisterAck{lease: 端口+令牌}
4. 客户端 15s Ping 保活;45s 无 Ping 无数据判离线回收
5. Roster 有变即推

### 4.3 数据面转发与打洞(实现精化:每会话端口)

> 计划阶段精化:数据面端口按**会话**分配(Punch 时),而非按设备——单 socket 对称转发,消除 UDP 源地址改写歧义。Register 只发**设备令牌**(数据面身份凭证);PunchResp/PunchNotif 携带**会话地址** `中继:S`。

```
A ──Punch{B}──▶ 中继:从端口池分配会话端口 S
            ◀─ PunchResp{ok, session_addr=中继:S}
中继 ──PunchNotif{from_fp=A, session_addr=中继:S}──▶ B

双方各自向 中继:S 发 KNOCK(中继学到 A、B 的地址)之后:
 ① A 虚拟端点 connect(中继:S)      ← 内层 QUIC 客户端
 ② B 虚拟端点 accept               ← 内层 QUIC 服务端
 ③ 包格式 [18B头{v1,flags,令牌}][载荷];中继在 S 上验令牌 →
    A 的包转给 B 学习地址,B 的包转给 A 学习地址
 ④ 双方看到的对端地址 ≡ 中继:S(单 socket 发出,天然一致)
```

- NAT 不再是问题:A、B 都只与中继固定 IP 通信;对称 NAT 兼容
- Punch 的作用 = 让中继分配会话端口 + 通知 B 准备,不承担穿透
- KNOCK 包(标志位):连接前必发——中继据此学习设备地址,不转发
- 令牌仍按设备签发(Register 时),会话上用于区分"这个包来自 A 还是 B"

### 4.4 客户端状态机

```
[Disabled] --开关--> [Connecting → 控制QUIC+PSK] → [Registered]
     ▲                                                    │
     └── 开关关 ←──────────────── 控制连接断 ─→ [Reconnecting]
                            (指数退避 1s/2s/4s/8s→30s 封顶)
```

- 控制连接断:在途传输不断(内层 QUIC 自带 keep-alive/重传),新传输等重连
- 数据面只验令牌不验会话,令牌租约期内有效,租约由数据面活动刷新

## 5. 错误处理与安全

### 5.1 故障矩阵

| # | 故障 | 检测 | 处理 |
|---|---|---|---|
| F1 | 控制连接断 | quinn closed | 状态机重连(指数退避);成功后重新 Register |
| F1' | 重连后名册变化 | Roster rev 比对 | 设备列表合并刷新 |
| F2 | 服务器重启令牌失效 | 数据包无响应 | 内层 QUIC PTO 重传;控制面推 Roster 触发重注册;控制面断走 F1 |
| F3 | 租约到期 | 服务器发 LeaveNotice 或回收 | 客户端立即重注册 |
| F3' | 令牌换新交接窗口 | 新旧并行 | 双令牌宽限期:旧令牌保留 30s 并行有效,QUIC 重传补包 |
| F4 | 客户端掉线 | 服务器 45s 超时 | 回收租约+令牌,推 Roster |
| F4' | 优雅关闭 | prepare_shutdown | 发 Leave + GOODBYE_KNOCK,秒级下线 |
| F5 | 对端不在线 | Roster 无此设备 | UI 提示,不进任务队列 |
| F5' | 对端中途掉线 | 内层 QUIC 断/watchdog | 走现有 interrupted 断点恢复体系 |
| F6 | UDP QoS 丢包 | conn.stats() | QUIC 自适应,无需新逻辑 |
| F7 | PSK 爆破 | 5 次/分钟 | IP 拉黑 10 分钟 |
| F7' | 反射放大 | 包 >1500B | 丢弃+计数 |

### 5.2 安全边界

| 层 | 中继可见 |
|---|---|
| TLS 内层载荷 / 文件名 / 目录结构 | ❌(端到端加密) |
| 设备名 | ⚠️ 明文(可用别名缓解,v1 不做) |
| 指纹 / IP / 时序 / 包大小 / 在线状态 | ✓(by design / 网络中继固有) |

防住:MITM(PSK 通道绑定 + 证书指纹互验)、蹭流量(令牌+重放窗+租约绑定)、重放/放大(滑窗+包长上限+KNOCK 不转发)。服务器被黑最坏情况:只有密文流+设备名+指纹+IP 时序。

### 5.3 运维

- systemd unit `Restart=always`,tracing → journald
- 端口:控制面 9443/udp,数据面 9000-9100/udp(TOML 可改)
- 配置 `/etc/localtrans-relay.toml`:PSK、端口范围、租约 TTL、速率限制参数、`max_bps_per_pair`(预留)
- 内存:每设备 <1KB,转发零分配;无状态,重启即恢复

## 6. 测试策略

| 层 | 内容 |
|---|---|
| relay_proto 单测 | serde roundtrip、版本字段 |
| 服务端单测 | 租约分配/回收/续期、令牌验证、重放窗、速率限制、包长上限 |
| 虚拟端点单测 | 18B 头编解码、非法包丢弃 |
| E2E(核心) | 进程内两设备+一中继,真 UDP 回环:虚拟端点→令牌头→转发→内层 QUIC 握手→传文件→RecvAck |
| 故障注入 | 服务器重启/控制断重连/租约到期/对端掉线→interrupted |
| 回归护栏 | 现有 80+15 全绿,一行不改 |

E2E 关键断言:中继收到的每个字节都带 18B 头且载荷无法解析出明文(转发钩子抓包比对)。

## 7. 实现里程碑

| # | 内容 | 验收 |
|---|---|---|
| M1 | relay_proto 消息定义 | roundtrip 测试绿 |
| M2 | localtrans-relay crate(控制面+数据面+TOML+systemd) | 单测+进程内 E2E |
| M3 | 客户端核心(relay/ 控制状态机+虚拟端点) | E2E:双设备经中继完成 QUIC 握手+文件传输+RecvAck |
| M4 | Tauri 粘合层(设置/名册合并/路由/优雅关闭) | 壳测试 |
| M5 | UI(设置区块/远程徽标/不在线提示) | 手动验证清单 |
| M6 | 故障注入+部署文档+v0.3.0 便携包 | 全绿+dist |

依赖:M1→M2→M3 主链;M4 依赖 M3;M5 依赖 M4;M6 收尾。M3 完成即功能可用。

## 8. v1 明确不做

- TCP 443 兜底(协议留版本位)
- 真正的 P2P 打洞(名册留 `relayed_ephemeral` 字段)
- 离线暂存
- 带宽限速(仅预留字段)
- 多中继/故障转移
- 设备名加密/流量 padding
