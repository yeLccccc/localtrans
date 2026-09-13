# 03 QUIC 会话与通信协议

> 源码:`crates/localtrans-core/src/session.rs`(3083 行,2022 行后为测试)、`protocol.rs`(651 行)。

## 1. 设计目标

- 所有设备间通信(配对、浏览、传输控制)走**单条 QUIC 连接上的双 streams 控制流**;文件块走独立 uni streams。
- 自签证书 mTLS + **指纹钉扎**:连接前已知对方指纹(发现/名册)则握手期强制比对,防中间人。
- 消息层 JSON(可读、可增量演进),4B 长度前缀分帧;RPC 用 msg_id 多路化,同一连接可并发多个请求。

## 2. TLS 与证书验证

三个自定义 rustls 验证器(session.rs):

| 验证器 | 侧 | 行为 |
|---|---|---|
| `FingerprintVerifier`(L207) | 客户端 | ①自签校验(issuer==subject+自验签) ②有效期窗 ③指纹比对(expected 有值必须相等);TLS1.2 签名一律拒 |
| `AcceptAnySelfSignedClientCert`(L264) | 服务端(普通 listener) | 接受任何自签+时间合法的客户端证书;client_auth 非强制 |
| `PinnedClientCertVerifier`(L325) | 服务端(中继 accept_peer) | 在上述基础上**握手期钉扎客户端指纹**(不符即握手失败);client_auth 强制 |

辅助:证书 Ed25519 公钥提取(`x509-parser` SPKI)、自签校验 `verify_self_signed`、TLS1.3 CertificateVerify 必须 Ed25519。

## 3. TransportConfig(三层,共用 base L392-415)

| 参数 | 值 | 理由 |
|---|---|---|
| keep_alive_interval | 5s | 直连/中继控制面保活 |
| stream_receive_window | 8 MiB | 块 4MiB × 并发流 |
| receive_window | 64 MiB | 连接级(默认窗口回环实测被压 ~200MB/s) |
| mtu_discovery 上界 | 1500 | 曾放开 65527 跨网段黑洞;可靠性优先 |

| 配置函数 | 用途 | 差异 |
|---|---|---|
| `quic_transport_config()` L422 | 局域网直连 | idle 60s |
| `relay_control_transport_config()` L435 | 中继控制面客户端 | keep_alive=None(死连接尽快暴露),存活靠应用层 15s Ping;uni 流并发 1024 |
| `relay_transport_config()` L451 | 中继内层端点 | **idle 30s;MTU 探测关死;initial_mtu=min_mtu=1200**(VPN/跨网 WSAEMSGSIZE 规避) |

## 4. 连接建立

**发起方** `connect_pinned(addr, expected_fp)`(L967)→ `connect_inner`:取/建 endpoint → quinn connect(附自己的证书)→ `post_handshake`:
1. 冷却检查(配对码 3 错 5 分钟冷却,在冷却内 → close + `Cooldown` 错);
2. 已信任 → `establish_control_flow`(open_bi,先发 Hello 再收 Hello)→ SessionUp;
3. 未信任 → 发 `PairingWaitConsent` 事件 → 同样建控制流(配对流程见 01 篇)。

**被动方** `handle_incoming`(L1234):accept → 取对端证书指纹 → 冷却检查 → 10s 超时 accept_bi → 收/回 Hello → 未信任发 `PairingConsentNeeded` + consent 状态机 → `insert_session_and_spawn`。

**会话表**:`HashMap<Fingerprint, Session>`(tokio Mutex);`Session{conn, ctrl_send, pairing, peer_name, consent, generation}`;`generation` 单调递增,**会话删除必须代次匹配**(防新一代连接被旧代清理误删,M-B1)。

## 5. wire 协议(protocol.rs)

### 5.1 控制面帧

```
[4B 大端长度][serde JSON]
```
- `decode_control_body` 限消息 ≤ 4 MiB;长度前缀与实际不一致 → `LengthMismatch` 断连。

### 5.2 数据面块流头

```
ChunkStreamHeader(12B 小端): job_id u64 ‖ chunk u32
```

### 5.3 ControlMsg 全消息表(`#[serde(tag="type", snake_case)]`)

| 消息 | 字段 | 用途 |
|---|---|---|
| Hello | name, fingerprint | 握手 |
| PairCodeSubmit | code | A 输码 |
| PairResult | ok | 配对判定结果 |
| ConsentGrant / ConsentDeny / ConsentCancel | name/—/— | 同意门 |
| SharesReq → SharesResp | msg_id / shares:[{id,alias}] | 共享区列表 |
| SharesChanged | share_id | 数据方 watchdog 主动推送(无需回复) |
| ListReq → ListResp | share_id, path, cursor / entries, next_cursor | 目录列表(分页) |
| MetaReq → MetaResp | share_id, path / job_id, file_name, total_size, chunk_hashes, file_hash? | 文件清单(块哈希表) |
| FetchReq | job_id, chunk | 拉一块(响应=块流) |
| OfferReq → OfferResp | job_id, files:[{name,size,rel_dir,hash?}] / accepted, save_dir?, reason?, skip_bitmap? | 推送发起/应答 |
| BitmapReq → BitmapResp | job_id / bits | 位图查询 |
| TransferCtl | job_id, action: pause/resume/cancel/throttle{max_streams} | 传输控制 |
| JobDone | job_id, offer_id | 推送模式完成 |
| RecvAck | job_id | 拉取模式接收回执 |
| RecvProgress | job_id, cumulative_bytes | 接收侧逐窗进度 |
| JobFailed | offer_id, reason | 接收失败立即收场 |
| ShareRename / ShareDelete / ShareMkdir → ShareOpResult | share_id, path, new_name / ok, error? | 远程文件操作 |
| Goodbye | — | 优雅关闭 |

**版本兼容纪律(serde_compat 思想)**:新增字段一律 `#[serde(default)]`(+可选 skip_serializing_if),老对端旧 JSON 可解析、新端缺省不发包。每个新字段配套向后兼容测试。`serde_compat.rs` 另提供 `u64_hex_string`(u64↔16位hex),解决 JS 大整数精度(持久化 job_id 用)。

### 5.4 RPC 多路化(M-B5)

- 每请求分配全局 `msg_id`(`next_msg_id` 原子计数);`send_rpc`(L800)注册 `pending_rpcs: HashMap<msg_id, oneshot>` 后发送,带超时,超时 `cancel_rpc` 防泄漏。
- ctrl_loop 收到响应类消息按 `resp_msg_id()` 分发到 oneshot;无主丢弃。
- 推送信号(OfferResp/BitmapResp/JobDone/JobFailed)走 `push_signals` broadcast;SharesChanged 走 `inbound_notify` broadcast。

## 6. ctrl_loop(每会话一个任务,L1423-1802)

```
select {
    ctrl_rx.recv() => send_all(编码外发消息),     // 出站:壳层/引擎 → 对端
    recv.read(16KiB) => inbuf 累积 → take_msg,    // 入站:增量解帧
}
```

**取消安全铁律**:入站读取必须用 `RecvStream::read`(单次完成可安全取消)+ 手动 inbuf 累积;**绝不能在 select! 里 await read_exact** —— 取消时已消费字节随 future 丢弃,控制流永久失步(v0.1.5 实机 bug)。

入站分发:配对消息(按 ConsentState 分派)→ 请求类 `inbound_ctrl.try_send` 给 RPC 路由器 → 响应类按 msg_id 分发 → 推送信号广播 → Goodbye/流结束则 drain 剩余终态消息后 break。

收尾(L1770-1801):代次校验 → remove 会话 → 发 `SessionDown` → failed 时关连接 → A 侧未完成的配对补发 PairResult 失败。

## 7. 会话生命周期 API

| 方法 | 行为 |
|---|---|
| `session(fp)` | 取连接(浏览/推送前查询) |
| `send_ctrl(msg)` | 克隆 ctrl_send 后 5s 超时发送 |
| `adopt_connection / adopt_as_initiator` | 中继自愈:外部建好的连接注入会话表(见 07 篇) |
| `disconnect(fp)` | 尽力发 Goodbye(5s 超时)→ close |
| `shutdown_all()` | 全会话 drain+Goodbye——对端立即 SessionDown,传输落 interrupted,位图由 PartWriter::drop 兜底持久化 |

## 8. 事件(SessionEvent,mpsc 32)

| 事件 | 时机 |
|---|---|
| PairingConsentNeeded | 被连接且未信任 |
| PairingCodeShown | B 同意生成码 |
| PairingWaitConsent | A 连上未信任对端 |
| PairingCodeEntry | A 收 ConsentGrant |
| PairingResult | 配对终态(ok/reason) |
| SessionUp | 会话建立/配对完成 |
| SessionDown | 会话终止(唯一出口:ctrl_loop 收尾) |

心跳:QUIC 层 keep-alive 5s + idle timeout 60s(直连);中继控制面另加应用层 15s Ping(见 07 篇)。

## 9. 关键设计取舍记录

- **JSON 而非 bincode**:可读性/增量演进优先;4MiB 上限兜底内存;块数据不进 JSON(独立 uni streams + 12B 头)。
- **响应也走 uni/broadcast 而非固定 bi 流回写**:推送信号天然一对多订阅。
- **msg_id 多路化**解决"同连接并发 RPC 响应通道被占用"(响应通道曾被单请求独占,浏览与传输并发即冲突)。
- TLS1.2 一律拒:只信 TLS1.3 + Ed25519。

## 10. 连接状态机与自动重连(2026-08-30 定案,未实现)

来源:specs/2026-08-30-device-identity-network-design.md §7/§8。三处现状缺口:配对成功不自动建连(用户须再点"浏览")、断开后无会话级自动重连(只有中继链路自愈)、移除信任对对端静默(TrustBroken 见 01 篇 §3a.2)。

### 10.1 状态机(显式全图)

```
不可见(未配对,探不到即消失)
  ↓ 发现
在线·待配对 ──配对成功──→ 在线·已配对(静默,无会话)
                            ↓ 配对成功自动连接(§10.2)/用户点连接/浏览/推送/拖放
                          已连接
                            ↓ 网络断
                          重连中 ──成功──→ 已连接
                            └5 败──→ 在线·已配对(回落,可再手动点)
离线常驻卡:已配对+离线(信任表兜底,灰色)
```

**关键区分**:「在线」永远只是"广播可见",不代表已建会话。增补边:在线·已配对 ──收到 TrustBroken──→ 在线·待配对(即时,toast,见 01 篇)。

### 10.2 自动重连(连接记忆制)

- **触发条件**:本软件运行期内用户主动连接过(点过连接/浏览/推送/拖放,**含配对成功后的自动连接**)。软件只守护"用户建立的连接",从不主动打扰——不是一进软件发现对方就连接。
- 未连接过的已配对设备:永远停在"在线·已配对",不自动建连。
- 节奏:指数退避 2s/4s/8s/16s,30s 封顶,每级最多 1 次,连续 5 败回落"在线·已配对"停止。
- 用户再点一次连接 = 重新开始一轮(记忆保留)。
- 重启软件 = 新会话,记忆清零,全部回静默。
- 与中继自愈的关系:会话级重连优先,失败轮到链路自愈(07 篇);用户视角合一(都显示"重连中")。

### 10.3 显示稳定

- 下线判定迟滞:15s + 连续探测无响应才变离线(防单包丢失闪"离线")。
- 排序:connected desc → online desc → name asc → fingerprint asc(指纹 tiebreaker,集合不变时卡片不跳动)。
- 刷新按钮已删(02 篇 §5):全通道自动化后无存在必要。
