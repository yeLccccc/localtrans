# localTrans 设计文档(as-built 总览)

> 版本基线:v0.11.0(2026-08-27 整理)。本文档系列描述**当前代码实际实现的设计**(as-built),作为后续完善"预期设计"的底稿。
> 三端:PC 应用(Tauri 2 + Vue 3)、Android 应用(Kotlin Compose + uniffi FFI)、中继服务器(localtrans-relay)。三端共享同一个 Rust 核心库 `localtrans-core`。

---

## 1. 产品定位

局域网/跨网 P2P 文件互传工具:

- **无中心服务器即可用**:同一局域网内通过 UDP 广播自动发现设备,QUIC 直连传输。
- **可选自建中继**:跨网段/NAT 场景接入自建 relay 服务器,中继只转发密文(端到端加密),支持设备发现与打洞式会话建立。
- **配对即信任**:6 位配对码完成设备配对,之后按权限(browse/download/push)受控互访。
- **三大能力**:远程文件浏览/下载、文件/文件夹推送(接收方确认或自动接受)、相册备份(Android)。

## 2. 系统架构

```
┌─────────────────────────┐      ┌─────────────────────────┐
│   PC 应用 (Tauri 2)      │      │  Android 应用 (Compose)  │
│  ┌───────────────────┐  │      │  ┌───────────────────┐  │
│  │  Vue 3 前端 (ui/)  │  │      │  │ Kotlin UI 层       │  │
│  └────────┬──────────┘  │      │  └────────┬──────────┘  │
│  invoke/emit (Tauri IPC)│      │  uniffi FFI + callback  │
│  ┌────────▼──────────┐  │      │  ┌────────▼──────────┐  │
│  │ src-tauri 壳       │  │      │  │ localtrans-ffi    │  │
│  └────────┬──────────┘  │      │  └────────┬──────────┘  │
└───────────┼─────────────┘      └───────────┼─────────────┘
            ▼                                ▼
   ┌──────────────────────────────────────────────┐
   │            localtrans-core (共享核心)          │
   │  discovery │ session(QUIC) │ transfer 引擎    │
   │  identity/trust │ share │ relay client       │
   └───────┬──────────────────────────┬───────────┘
           │ UDP 广播 47600            │ QUIC 控制面 9443
           │ QUIC 直连 47601           │ UDP 数据面 9000-9100
           ▼                          ▼
     局域网对端设备              ┌──────────────────┐
                                 │ localtrans-relay  │
                                 │ 控制面 + 数据面    │
                                 └──────────────────┘
```

- **直连路径**:发现(UDP 47600)→ QUIC 连接(47601,mTLS 自签证书 + 指纹钉扎)→ 控制流 JSON 消息 + 块流传输。
- **中继路径**:控制 QUIC 连 relay(9443,PSK + 占有证明注册)→ 名册发现对端 → Punch 分配会话端口 → 双方经 VirtualUdp 把内层 QUIC 包(端到端加密)交 relay 逐包转发(UDP 9000-9100)。
- **中继看不到明文**:内层 QUIC 的 TLS 加密在两端完成,relay 只剥/加 18 字节头转发密文。

## 3. 技术选型总表

| 层 | 技术 | 版本 | 用途 |
|---|---|---|---|
| 异步运行时 | tokio | 1 (full) | 全部核心逻辑(发现层除外) |
| QUIC | quinn | 0.11 | 直连与中继内层传输 |
| TLS | rustls | 0.23 (ring, tls12) | TLS1.3 + 自签证书 mTLS |
| 证书生成 | rcgen | 0.13 (ring) | Ed25519 自签证书 |
| 签名/身份 | ed25519-dalek | 2 | 发现包签名、占有证明 |
| 哈希 | sha2 | — | 指纹(SHA-256)、块/文件哈希 |
| 密钥派生 | hkdf | 0.12 | 会话密钥派生 |
| 常时比较 | subtle | 2.6 | PSK/配对码恒时比较 |
| UDP 选项 | socket2 | 0.5 | SO_REUSEADDR / SO_BROADCAST |
| 序列化 | serde / serde_json | 1 | 全部 wire 格式与持久化 |
| 缓冲池 | crossbeam | 0.8 | SegQueue 缓冲复用 |
| 日志 | tracing + tracing-subscriber/appender | — | 按天滚动日志 |
| PC 壳 | Tauri | 2 | 窗口/IPC/插件(dialog、notification、opener、single-instance) |
| PC 前端 | Vue 3 + Pinia + Vue Router + Vite + Vitest | — | 4 页面 SPA,嵌入 exe |
| Android | Kotlin + Jetpack Compose (BOM 2024.10.01) + Coil | minSdk 26 / target 35 | 独立 Compose UI |
| FFI | uniffi | 0.28 | Rust→Kotlin 绑定 + callback interface |
| 中继配置 | TOML + serde | — | relay.toml |

## 4. 端口与协议总表

| 用途 | 协议 | 端口(默认) | 说明 |
|---|---|---|---|
| 设备发现 | 裸 UDP 广播/单播 | 47600 | JSON+Ed25519 签名, presence/probe/probe_resp |
| 数据面 QUIC(直连) | QUIC/TLS1.3 | 47601 | mTLS 自签证书, 指纹钉扎 |
| 中继控制面 | QUIC, ALPN `localtrans-relay` | 9443(可配) | PSK + 注册签名, bi 流请求 / uni 流推送 |
| 中继数据面 | 裸 UDP | 9000–9100(池) | 18B 头 + 密文载荷, KNOCK/DATA/GOODBYE |
| 防火墙规则(Windows) | UDP 入站 | 47600-47601 | netsh 添加, UAC 提权 |

## 5. 核心流程一页图

```
首次运行:  生成 identity.key + cert.der → 指纹=SHA256(cert) → 默认 config.json 落盘(exe旁 data/)
发现:      每 5s±1s 广播 presence;收到 probe 回 probe_resp;三方包都入设备表;15s 未见即离线
配对:      A 连 B(QUIC)→ B 弹同意门(60s)→ B 同意即生成 6 位码只在 B 屏显示
           → A 人工输码 → B 常时比对 → 双方各自写信任表(perms 默认 browse=1,download=1,push=ask)
           → 3 次错码 → 5 分钟冷却
浏览/下载: SharesReq → ListReq(分页500) → MetaReq(块清单) → FetchReq 逐块拉(滑动窗口 4-32 流自适应)
推送:      OfferReq(≤1MiB 文件带整文件hash) → B 按 push 策略 ask/auto/deny → OfferResp(+秒传位图)
           → 小文件单流批量发;大文件由 B 反向 MetaReq/FetchReq 驱动 → RecvAck → JobDone
断点续传:  接收侧 .localtrans-parts/{job}/part.bin + manifest.json 位图;重开按位图续拉
秒传:      下载根 .localtrans-inbox-index.json(整文件 SHA-256→路径);命中则硬链接/复制零传输
中继:      注册(PSK→nonce→签名) → 名册 → Punch(限速10/60s) → 会话端口 + token → 双方 KNOCK 学习 → DATA 转发
           租约 TTL 45s(数据面/控制面双活跃保活);断线 autoheal 1s→30s 退避重连,3 败放弃
```

## 6. 文档导读

| 文档 | 内容 |
|---|---|
| [01-身份信任与配对](01-身份信任与配对.md) | 密钥/证书/指纹、信任表、配对码全流程、防暴力 |
| [02-设备发现](02-设备发现.md) | UDP 广播协议、重放防护、在线判定、probe_targets、隐身 |
| [03-QUIC会话与通信协议](03-QUIC会话与通信协议.md) | TLS 验证器、transport 配置、全部控制消息、RPC 多路化、会话生命周期 |
| [04-文件传输引擎](04-文件传输引擎.md) | 推/拉全流程、分块、断点、秒传、流控自适应、净化、文件夹 |
| [05-共享目录与远程文件操作](05-共享目录与远程文件操作.md) | ShareRegistry、路径防御、watchdog、远程改名/删除/建目录 |
| [06-中继服务器](06-中继服务器.md) | 控制面/数据面协议、租约、防滥用、配置 |
| [07-中继客户端与自动愈合](07-中继客户端与自动愈合.md) | VirtualUdp 虚拟端点、punch、重连退避、自动续传 |
| [08-PC应用-Tauri壳](08-PC应用-Tauri壳.md) | 启动流程、全部 Tauri 命令、事件桥、防火墙、状态管理 |
| [09-PC应用-Vue前端](09-PC应用-Vue前端.md) | 4 页面、组件、stores、API 层、事件、样式体系 |
| [10-Android应用](10-Android应用.md) | uniffi FFI 全函数、Kotlin 结构、权限、与 PC 差异 |
| [11-配置与数据持久化](11-配置与数据持久化.md) | Config 字段、data/ 目录全部文件、原子写、损坏恢复 |
| [12-安全设计](12-安全设计.md) | 全链路安全机制汇总(认证/防滥用/隐私/fail-closed) |
| [13-线程与任务模型](13-线程与任务模型.md) | 全部线程/tokio task/锁清单与并发纪律 |
| [14-功能清单总表](14-功能清单总表.md) | **按端枚举全部功能点矩阵**(做预期设计的对照底稿) |
| [15-PC应用功能大树](15-PC应用功能大树.md) | 行为视角功能树 + 分歧点索引(D1-D8,含定案标记) |
| [16-通道质量管理与智能选路](16-通道质量管理与智能选路.md) | 两通道模型/阶梯探测/评分选路/退化切换(**2026-08-30 定案,目标设计**) |

## 7. 源码地图

```
crates/
  localtrans-core/           共享核心
    src/ store.rs identity.rs pairing.rs protocol.rs discovery.rs
         session.rs share.rs share_watch.rs device_merge.rs serde_compat.rs
         transfer/{engine,manifest,dedup,adapt,sender_state,source_probe}.rs
         relay/{client,proto,virtual_ep,autoheal}.rs
  localtrans-relay/          中继服务器(config/control/data_plane/lease)
  localtrans-ffi/            Android 绑定(lib/state/dto/relay_state)
src-tauri/                   PC 壳(main/commands/events/firewall)
ui/                          PC 前端(Vue 3)
android/                     Android 工程(Kotlin Compose)
```
