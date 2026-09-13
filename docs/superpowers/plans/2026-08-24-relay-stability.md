# 中继稳定性管理实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 中继会话三层稳定性:服务端 NAT 漂移跟踪、客户端 keep-alive 对称 + 15s 判死、壳层自动重连 + 自动续传(一次断点续传)。

**Architecture:** 分层自治(用户裁定方案一):服务端 data_plane.rs 在 DATA 分支重学习漂移地址;core virtual_ep.rs 两端点补齐 TransportConfig(client 侧漏配,server 侧覆写 idle 15s);core 新增 relay/autoheal.rs(退避状态机 + 重连编排 + 自动续传判定)供桌面壳与 FFI 壳共用,壳层只做事件接线与去重守卫。

**Tech Stack:** Rust workspace(localtrans-core / localtrans-relay / localtrans-ffi / src-tauri)、quinn 0.11 QUIC、tokio、Vue3(桌面 UI)、uniFFI(安卓)。

**Spec:** `docs/superpowers/specs/2026-08-24-relay-stability-design.md`(含 d700499 复审修订)

## Global Constraints

- keep-alive 间隔 5s;max_idle_timeout 15_000ms;`max_udp_payload_size(1200)` 留在 EndpointConfig **原位不动**(它是 EndpointConfig 的方法,TransportConfig 没有)。
- 局域网直连端点(session.rs 的 `quic_transport_config` / `bind_endpoint`)**不动**,维持 idle 60s。
- 回路 1 退避:1s → 2s →(翻倍,封顶 30s);连续 3 次失败放弃,等下次 SessionDown/发现再触发。
- 回路 2 自动续传**只重试 1 次**(job_id 进 `auto_retried` 集合,永不二次自动续传)。
- 回路 2 触发规则(spec 复审澄清,取宽):任何 `failed` + parts 残留(pending_jobs 含此 job)+ 对端 SessionUp,即重试 1 次;不区分失败原因。
- 测试命令 per-crate 串行:`cargo test -p <crate> -- --test-threads=1`。
- 提交信息:中文前缀(如 `fix(relay):`/`feat(core):`) + 空行 + `Co-Authored-By: Claude <noreply@anthropic.com>`。
- 安全规约:PSK 不进日志;明文文件名/路径不进 tracing 日志(只打指纹/ID/数量);配对码明文不进日志。
- adapt.rs 本轮不动(B2 增量记 backlog)。
- 测试环境注意:所有 relay e2e 测试必须 `#[serial]`(serial_test),统一 `PORT_OFFSET = 10` + `config.data_port_end = config.data_port_start + 3`。

---

### Task 1: 服务端 NAT 漂移跟踪(data_plane.rs)

**Files:**
- Modify: `crates/localtrans-relay/src/data_plane.rs:164-188`(FLAG_DATA 分支)
- Test: `crates/localtrans-relay/src/data_plane.rs`(tests 模块内新增)

**Interfaces:**
- Consumes: 现有 `LearnedTable = HashMap<u16, HashMap<[u8;32], SocketAddr>>`、`data_header_encode`、`LeaseTable::alloc/alloc_session_port`。
- Produces: 无新公开接口——行为变更(DATA 包源地址漂移时自动重学习),Task 4 的 e2e 依赖此行为。

- [ ] **Step 1: 写失败测试**

在 `data_plane.rs` 的 `mod tests` 末尾(`goodbye_knock_recycles_session_port` 测试之后)添加:

```rust
    /// 测试:NAT 漂移重学习——DATA 包源地址变化时,学习表跟随更新,
    /// 后续对端回包转发到新地址(旧实现转发到死地址,A 新 socket 收不到)
    #[tokio::test]
    #[serial]
    async fn data_relearning_follows_nat_drift() {
        let config = RelayConfig::default_for_test();
        let leases = Arc::new(LeaseTable::new(config.data_port_start..config.data_port_end));
        let dp = DataPlane::spawn(&config, leases.clone()).await.unwrap();

        let la = leases.alloc([1u8; 32], "A".into()).unwrap();
        let lb = leases.alloc([2u8; 32], "B".into()).unwrap();
        let session_port = leases.alloc_session_port().unwrap();
        let session_addr = SocketAddr::new(config.public_ip, session_port);

        // A 的旧地址(sock_a1)与漂移后新地址(sock_a2),B 的 socket
        let sock_a1 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_a2 = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

        // A 自旧地址 KNOCK,B 也 KNOCK(学习表建立)
        let mut knock_a = Vec::new();
        data_header_encode(&mut knock_a, &la.token, FLAG_KNOCK);
        sock_a1.send_to(&knock_a, session_addr).await.unwrap();
        let mut knock_b = Vec::new();
        data_header_encode(&mut knock_b, &lb.token, FLAG_KNOCK);
        sock_b.send_to(&knock_b, session_addr).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // A 自【新地址】发 DATA(模拟 NAT 重绑定)
        let mut data_a = Vec::new();
        data_header_encode(&mut data_a, &la.token, FLAG_DATA);
        data_a.extend_from_slice(b"drift");
        sock_a2.send_to(&data_a, session_addr).await.unwrap();

        // B 应回到 "drift"(转发路径本身不依赖 A 的地址)
        let mut buf = [0u8; 2048];
        let (n, _) = sock_b.recv_from(&mut buf).await.unwrap();
        assert_eq!(&buf[..n], b"drift");

        // B 回 DATA —— 断言转发目的已漂移到 A 的新地址 sock_a2
        // (旧实现转发到 sock_a1,sock_a2 在 300ms 内收不到 → 红)
        let mut data_b = Vec::new();
        data_header_encode(&mut data_b, &lb.token, FLAG_DATA);
        data_b.extend_from_slice(b"back");
        sock_b.send_to(&data_b, session_addr).await.unwrap();

        match tokio::time::timeout(
            std::time::Duration::from_millis(300),
            sock_a2.recv_from(&mut buf),
        ).await {
            Ok(Ok((n, _))) => assert_eq!(&buf[..n], b"back"),
            Ok(Err(e)) => panic!("接收错误: {}", e),
            Err(_) => panic!("NAT 漂移后学习表未更新: 回包仍发往旧地址"),
        }

        dp.shutdown().await;
    }
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-relay --lib data_relearning -- --test-threads=1`
Expected: FAIL,panic "NAT 漂移后学习表未更新: 回包仍发往旧地址"

- [ ] **Step 3: 实现 DATA 分支漂移重学习**

将 `data_plane.rs` FLAG_DATA 分支(164-188 行)整体替换为:

```rust
                FLAG_DATA => {
                    // NAT 漂移检测:DATA 包源地址与学习表不一致 → 更新。
                    // 客户端 NAT 重绑定后发出的第一个数据包即触发重学习,
                    // QUIC 重传兜底丢包间隙(无需客户端重发 KNOCK)。
                    {
                        let learned = self.learned.read().await;
                        let drifted = learned
                            .get(&port)
                            .and_then(|addrs| addrs.get(&src_fp))
                            .is_some_and(|old| *old != from);
                        if drifted {
                            drop(learned);
                            let mut learned = self.learned.write().await;
                            if let Some(addrs) = learned.get_mut(&port) {
                                if let Some(old) = addrs.get(&src_fp).copied() {
                                    addrs.insert(src_fp, from);
                                    tracing::info!(
                                        "NAT 漂移: port={}, fp={}, {} -> {}",
                                        port, hex::encode(src_fp), old, from
                                    );
                                }
                            }
                        }
                    }

                    // 转发给会话另一方(原逻辑不变)
                    let learned = self.learned.read().await;
                    let Some(addrs) = learned.get(&port) else {
                        continue;
                    };

                    // 查对方地址(除 src_fp 外的第一个)
                    let dst_fp = addrs.keys().find(|&&fp| fp != src_fp);
                    let Some(&dst_fp) = dst_fp else {
                        tracing::warn!("数据面: 端口 {} 找不到对方(src_fp={})", port, hex::encode(src_fp));
                        continue; // 对方未 KNOCK,丢弃
                    };

                    let Some(&dst_addr) = addrs.get(&dst_fp) else {
                        tracing::warn!("数据面: 端口 {} 找不到对方地址(dst_fp={})", port, hex::encode(dst_fp));
                        continue;
                    };

                    // 转发包(剥掉 18B 头,载荷直接从本 socket 发出)
                    let payload = &pkt[18..];
                    if let Err(e) = sock.send_to(payload, dst_addr).await {
                        tracing::warn!("转发失败 {}: {}", dst_addr, e);
                    }
                }
```

- [ ] **Step 4: 跑测试确认通过 + 全模块回归**

Run: `cargo test -p localtrans-relay --lib -- --test-threads=1`
Expected: 全部 PASS(含既有 5 个数据面测试)

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-relay/src/data_plane.rs
git commit -m "fix(relay): 数据面 NAT 漂移重学习——DATA 源地址变化时更新学习表

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 2: virtual_ep keep-alive 对称 + 15s 判死

**Files:**
- Modify: `crates/localtrans-core/src/relay/virtual_ep.rs:156-201`(server_endpoint / client_endpoint)
- Test: `crates/localtrans-relay/tests/e2e.rs`(文件末尾追加 2 个测试)

**Interfaces:**
- Consumes: `crate::session::quic_transport_config()`(已存在,session.rs:325,pub)——keep-alive 5s + 流控参数现成;`session::server_config` / `session::client_builder`。
- Produces: 无签名变更。行为契约(供 Task 4 e2e 依赖):中继内层连接 idle 15s 判死;keep-alive 5s 双侧生效。

- [ ] **Step 1: 写失败测试(e2e 追加)**

在 `crates/localtrans-relay/tests/e2e.rs` 文件末尾追加(与 e2e.rs 现有测试的自包含风格一致,不抽公共函数):

```rust
/// 稳定性 T1:keep-alive 互保——应用层静默 20s,连接必须仍活。
/// 20s > idle 15s:若心跳未生效,15s 即判死;心跳在工作则连接永活。
/// (v0.9.1 修复前:client_endpoint 无 TransportConfig,idle 30s 默认,
///  此测试恰好也能过——真正区分度在 T2 的 15s 判死,本测试锁行为防回归)
#[tokio::test]
#[serial]
async fn keepalive_keeps_idle_connection_alive() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into() },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into() },
        id_b.clone(),
    ).await.unwrap();

    // 等名册互见
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // B 侧 accept 后持有连接(存入 oneshot 供测试观察)
    let (b_conn_tx, b_conn_rx) = tokio::sync::oneshot::channel::<quinn::Connection>();
    let client_b_clone = client_b.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { session_addr, .. } = ev {
                if let Ok(conn) = client_b_clone.accept_peer(session_addr).await {
                    let _ = b_conn_tx.send(conn);
                    break;
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("connect_peer 超时").expect("A connect_peer 失败");
    let conn_b = tokio::time::timeout(Duration::from_secs(30), b_conn_rx)
        .await.expect("B accept 超时").expect("B accept 失败");

    // === 应用层静默 20s(不发送任何数据)===
    tokio::time::sleep(Duration::from_secs(20)).await;

    // 断言双向仍活:能开流 + 对端能收流
    let (mut send_a, mut recv_a) = conn_a.open_bi().await.expect("静默 20s 后 A 开流失败——连接被判死");
    send_a.write_all(b"alive").await.unwrap();
    send_a.finish().unwrap();
    let mut buf = [0u8; 8];
    let (mut send_b, mut recv_b) = conn_b.accept_bi().await.expect("B accept_bi 失败");
    recv_b.read_exact(&mut buf).await.unwrap();
    let _ = send_b;
    let _ = recv_a;
    assert_eq!(&buf[..5], b"alive");

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;
}

/// 稳定性 T2:断路 15s 判死——DataPlane 停转(转发停)后,
/// 连接在 30s 内 closed()。修复前 server 侧 idle 60s → 30s 内不 closed → 红。
#[tokio::test]
#[serial]
async fn idle_timeout_kills_dead_path_in_15s() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into() },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into() },
        id_b.clone(),
    ).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (b_conn_tx, _b_conn_rx) = tokio::sync::oneshot::channel::<quinn::Connection>();
    let client_b_clone = client_b.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { session_addr, .. } = ev {
                if let Ok(conn) = client_b_clone.accept_peer(session_addr).await {
                    let _ = b_conn_tx.send(conn);
                    break;
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("connect_peer 超时").expect("A connect_peer 失败");

    // === 断路:停数据面(心跳包发出但不再被转发 → 双向静默)===
    dp.shutdown().await;

    // idle 15s + 余量 → 30s 内必须 closed;旧配置 60s idle 会超时 → 红
    tokio::time::timeout(Duration::from_secs(30), conn_a.closed())
        .await
        .expect("30s 内连接未判死——idle 超时未收紧到 15s");

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
}
```

注意:e2e.rs 头部已 import `quinn`(第 13 行 `tokio::io::{AsyncReadExt, AsyncWriteExt}`;若 `quinn::Connection` 路径报错,在文件头加 `use quinn;`——quinn 是 localtrans-core 的传递依赖,需确认 `localtrans-relay` 的 Cargo.toml `[dev-dependencies]` 有 `quinn`;若无则添加 `quinn = "0.11"`)。

- [ ] **Step 2: 跑测试确认 T2 失败(T1 可能通过,见测试注释)**

Run: `cargo test -p localtrans-relay --test e2e idle_timeout_kills -- --test-threads=1`
Expected: FAIL——"30s 内连接未判死"(server 侧 idle 仍 60s)

Run: `cargo test -p localtrans-relay --test e2e keepalive_keeps -- --test-threads=1`
Expected: PASS(client 侧 idle 30s > 20s 静默,恰好存活——本测试是行为锁)

- [ ] **Step 3: 实现两端点 TransportConfig 对称**

`virtual_ep.rs` 新增模块级函数(放在 `TokioPoller` 定义之前):

```rust
/// 中继内层端点传输配置:复用 session::quic_transport_config 的
/// keep-alive(5s)+ 流控参数,仅覆写 idle 超时 60s → 15s。
/// 15s = 3 个心跳周期,躲开单次抖动误杀;路径死亡(断路/对端消失)
/// 的僵尸窗口从 30~60s 缩到 15s。局域网直连端点不受影响(仍 60s)。
fn relay_transport_config() -> std::sync::Arc<quinn::TransportConfig> {
    let mut tc = (*crate::session::quic_transport_config()).clone();
    tc.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_millis(15_000))
            .expect("15s 在 IdleTimeout 表示范围内"),
    ));
    std::sync::Arc::new(tc)
}
```

`server_endpoint`(156-172 行)在 `let mut server = ServerConfig::with_crypto(...)` 之后(即现有 `server_endpoint` 调 `session::server_config` 的路径——注意 `session::server_config` 内部已 `transport_config(quic_transport_config())`,这里覆写):

```rust
pub async fn server_endpoint(vudp: Arc<VirtualUdp>, id: &crate::identity::Identity) -> Result<Endpoint, String> {
    let server_cfg = crate::session::server_config(id)
        .map_err(|e| format!("server_config: {}", e))?;
    // 覆写 idle 15s(见 relay_transport_config 注释)
    let mut server_cfg = server_cfg;
    server_cfg.transport_config(relay_transport_config());
    let mut ep_cfg = quinn::EndpointConfig::default();
    // MTU 1200 保持 EndpointConfig 原位(它是 EndpointConfig 的方法)
    ep_cfg.max_udp_payload_size(1200).map_err(|e| format!("max_udp_payload_size: {}", e))?;
    let mut ep = Endpoint::new_with_abstract_socket(
        ep_cfg,
        Some(server_cfg),
        vudp,
        Arc::new(quinn::TokioRuntime),
    ).map_err(|e| e.to_string())?;
    Ok(ep)
}
```

注意:`session::server_config` 返回 `quinn::ServerConfig`,其 `transport_config` 是 `&mut self` 方法,可直接覆写。若 `server_config` 返回类型带 `Arc<QuicServerConfig>` 无法覆写,则改法:复制 `server_config` 的构建逻辑到本地并挂 `relay_transport_config()`——以编译为准,语义就是"server 侧也用 15s"。

`client_endpoint`(180-201 行)在 `let mut client_cfg = quinn::ClientConfig::new(Arc::new(quic_cfg));` 之后加一行:

```rust
    client_cfg.transport_config(relay_transport_config());
```

(原 `ep.set_default_client_config(client_cfg)` 保持不变。)

- [ ] **Step 4: 跑两个测试确认通过**

Run: `cargo test -p localtrans-relay --test e2e keepalive_keeps -- --test-threads=1 && cargo test -p localtrans-relay --test e2e idle_timeout_kills -- --test-threads=1`
Expected: 两个 PASS(T2 现在在 15~30s 窗口内 closed)

- [ ] **Step 5: core 单元回归(确认虚拟端点既有 4 测试不受影响)+ relay 全量**

Run: `cargo test -p localtrans-core --lib virtual_udp -- --test-threads=1`
Run: `cargo test -p localtrans-relay -- --test-threads=1`
Expected: 全 PASS

- [ ] **Step 6: Commit**

```bash
git add crates/localtrans-core/src/relay/virtual_ep.rs crates/localtrans-relay/tests/e2e.rs
git commit -m "fix(core): 中继内层端点 TransportConfig 对称——client 补挂 + 双侧 idle 收紧 15s

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 3: core autoheal 纯逻辑(退避状态机 + 自动续传判定)

**Files:**
- Create: `crates/localtrans-core/src/relay/autoheal.rs`
- Modify: `crates/localtrans-core/src/relay/mod.rs`(加 `pub mod autoheal;`)
- Test: `crates/localtrans-core/src/relay/autoheal.rs`(内嵌 tests 模块)

**Interfaces:**
- Consumes: 无(纯逻辑)。
- Produces(Task 4/5/6 依赖,签名精确):
  - `pub struct BackoffState { pub fails: u32 }`,`pub fn new() -> Self`、`pub fn record_failure(&mut self) -> Option<u64>`、`pub fn reset(&mut self)`
  - `pub fn auto_resumable_jobs(transfers: Vec<(u64, String, String)>, peer_hex: &str, pending_job_ids: &[u64], already_retried: &std::collections::HashSet<u64>) -> Vec<u64>`

- [ ] **Step 1: 写失败测试**

创建 `crates/localtrans-core/src/relay/autoheal.rs`:

```rust
//! 中继会话自愈:退避状态机 + 自动续传判定(纯逻辑,双壳共用)。

/// 回路 1 退避状态机:尝试失败后取下次延迟;连续 3 次失败放弃。
/// 延迟序列 1s → 2s →(翻倍,封顶 30s);MAX_FAILS=3 时第 3 次失败返回 None。
pub struct BackoffState {
    pub fails: u32,
}

const MAX_FAILS: u32 = 3;
const MAX_DELAY_SECS: u64 = 30;

// (实现见 Step 3)

/// 回路 2 判定:传输表里 failed 且对端匹配且 parts 残留且未自动重试过的任务。
/// spec 复审裁定:取宽规则——不区分失败原因,任何 failed 满足其余条件即重试 1 次。
pub fn auto_resumable_jobs(
    transfers: Vec<(u64, String, String)>,
    peer_hex: &str,
    pending_job_ids: &[u64],
    already_retried: &std::collections::HashSet<u64>,
) -> Vec<u64> {
    // (实现见 Step 3)
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_sequence_and_giveup() {
        let mut b = BackoffState::new();
        assert_eq!(b.fails, 0);
        assert_eq!(b.record_failure(), Some(1)); // 第 1 败 → 1s 后重试
        assert_eq!(b.record_failure(), Some(2)); // 第 2 败 → 2s 后重试
        assert_eq!(b.record_failure(), None);    // 第 3 败 → 放弃
    }

    #[test]
    fn backoff_reset() {
        let mut b = BackoffState::new();
        b.record_failure();
        b.record_failure();
        b.reset();
        assert_eq!(b.fails, 0);
        assert_eq!(b.record_failure(), Some(1)); // 重置后从头计
    }

    #[test]
    fn auto_resumable_filters() {
        let retried = [9u64].into_iter().collect::<std::collections::HashSet<u64>>();
        let transfers = vec![
            (1u64, "failed".into(), "aa".into()),   // 命中
            (2u64, "done".into(), "aa".into()),     // 非 failed
            (3u64, "failed".into(), "bb".into()),   // 对端不匹配
            (4u64, "failed".into(), "aa".into()),   // 无 parts 残留
            (9u64, "failed".into(), "aa".into()),   // 已自动重试过
        ];
        let pending = [1u64, 2, 3, 9];
        let jobs = auto_resumable_jobs(transfers, "aa", &pending, &retried);
        assert_eq!(jobs, vec![1]);
    }
}
```

同时 `mod.rs` 加 `pub mod autoheal;`(在 `pub mod client;` 之后)。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-core --lib autoheal -- --test-threads=1`
Expected: 编译失败或断言失败(`record_failure` 未实现/返回错误值)

- [ ] **Step 3: 实现两个纯函数**

替换 autoheal.rs 中两处 `// (实现见 Step 3)`:

```rust
impl BackoffState {
    pub fn new() -> Self {
        Self { fails: 0 }
    }

    /// 记录一次失败,返回下次重试延迟(秒)。None = 连续 3 次失败,放弃。
    pub fn record_failure(&mut self) -> Option<u64> {
        self.fails += 1;
        if self.fails >= MAX_FAILS {
            None
        } else {
            Some((1u64 << (self.fails - 1)).min(MAX_DELAY_SECS))
        }
    }

    pub fn reset(&mut self) {
        self.fails = 0;
    }
}

impl Default for BackoffState {
    fn default() -> Self {
        Self::new()
    }
}
```

```rust
pub fn auto_resumable_jobs(
    transfers: Vec<(u64, String, String)>,
    peer_hex: &str,
    pending_job_ids: &[u64],
    already_retried: &std::collections::HashSet<u64>,
) -> Vec<u64> {
    transfers
        .into_iter()
        .filter(|(id, state, peer)| {
            state == "failed"
                && peer == peer_hex
                && pending_job_ids.contains(id)
                && !already_retried.contains(id)
        })
        .map(|(id, _, _)| id)
        .collect()
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p localtrans-core --lib autoheal -- --test-threads=1`
Expected: 3 个测试 PASS

- [ ] **Step 5: Commit**

```bash
git add crates/localtrans-core/src/relay/autoheal.rs crates/localtrans-core/src/relay/mod.rs
git commit -m "feat(core): 自愈纯逻辑——退避状态机(3败停)与自动续传判定

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 4: autoheal 编排 + 断路自愈 E2E

**Files:**
- Modify: `crates/localtrans-core/src/relay/autoheal.rs`(追加编排函数)
- Test: `crates/localtrans-relay/tests/e2e.rs`(追加 1 个 e2e)

**Interfaces:**
- Consumes: Task 3 `BackoffState`;`RelayClient::connect_peer(&self, target_fp: [u8;32]) -> Result<quinn::Connection, String>`(client.rs:341);`SessionManager::adopt_as_initiator(&self, conn: quinn::Connection) -> Result<Fingerprint, SessionError>`(session.rs:789)。
- Produces(Task 5/6 依赖,签名精确):
  - `pub async fn reconnect_peer_conn(client: &Arc<relay::client::RelayClient>, target_fp: [u8; 32]) -> Option<quinn::Connection>`
  - `pub async fn auto_reconnect(client: &Arc<relay::client::RelayClient>, sm: &Arc<session::SessionManager>, target_fp: [u8; 32]) -> bool`

- [ ] **Step 1: 写失败 E2E**

在 `crates/localtrans-relay/tests/e2e.rs` 末尾追加:

```rust
/// 稳定性 T3(一测覆盖三模块):断路 → 15s 判死 → 数据面重启 →
/// autoheal 重连 → 新连接可交换消息。
/// 覆盖:Task 2 的 15s 判死 + Task 4 的重连编排 + Task 1 的重学习
/// (重连后新 KNOCK 地址学习)。
#[tokio::test]
#[serial]
async fn autoheal_after_data_plane_outage() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into() },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into() },
        id_b.clone(),
    ).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // B 侧 accept 循环(断路重连后 PunchIncoming 会再来,循环不退出)
    let client_b_clone = client_b.clone();
    let b_conns = Arc::new(tokio::sync::Mutex::new(Vec::<quinn::Connection>::new()));
    let b_conns_clone = b_conns.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { session_addr, .. } = ev {
                if let Ok(conn) = client_b_clone.accept_peer(session_addr).await {
                    b_conns_clone.lock().await.push(conn);
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("首次 connect_peer 超时").expect("首次 connect_peer 失败");
    // 等 B 侧也拿到连接
    tokio::time::sleep(Duration::from_millis(300)).await;

    // === 断路:停数据面 → 15s 判死 ===
    dp.shutdown().await;
    tokio::time::timeout(Duration::from_secs(30), conn_a.closed())
        .await.expect("30s 内连接未判死");

    // === 数据面重启(旧 socket 随 Arc drop 关闭,端口重绑)===
    drop(dp);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let dp2 = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面重启失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    // === 自愈:A 侧重连(B 侧 accept 循环自动接住)===
    let conn_a2 = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Some(conn) = localtrans_core::relay::autoheal::reconnect_peer_conn(&client_a, fp_b).await {
                return conn;
            }
            // reconnect_peer_conn 内部 3 败即弃;外层兜底再等数据面稳定
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }).await.expect("自愈重连超时");

    // === 新连接交换消息(证明全链路恢复)===
    let (mut send_a2, mut recv_a2) = conn_a2.open_bi().await.expect("新连接开流失败");
    send_a2.write_all(b"revived").await.unwrap();
    send_a2.finish().unwrap();
    // B 侧最新连接应收到
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut conns = b_conns.lock().await;
            if let Some(conn) = conns.last() {
                if let Ok((mut send_b, mut recv_b)) = conn.accept_bi().await {
                    let mut buf = [0u8; 8];
                    recv_b.read_exact(&mut buf).await.map_err(|e| format!("{}", e))?;
                    let _ = send_b;
                    return Ok::<_, String>(buf.to_vec());
                }
            }
            drop(conns);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("B 侧收消息超时").expect("B 侧读取失败");
    assert_eq!(&got[..7], b"revived");
    let _ = recv_a2;

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp2.shutdown().await;
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p localtrans-relay --test e2e autoheal_after -- --test-threads=1`
Expected: 编译失败——`reconnect_peer_conn` 不存在

- [ ] **Step 3: 实现编排函数**

在 `autoheal.rs` 头部补 import 并追加(注意 relay 模块引 session 模块,同 crate 无循环):

```rust
use std::sync::Arc;

/// 回路 1 核心:connect_peer 带退避重试,成功返回新连接,3 败返回 None。
/// e2e 与壳层(auto_reconnect)共用。
pub async fn reconnect_peer_conn(
    client: &Arc<crate::relay::client::RelayClient>,
    target_fp: [u8; 32],
) -> Option<quinn::Connection> {
    let mut backoff = BackoffState::new();
    loop {
        match client.connect_peer(target_fp).await {
            Ok(conn) => {
                tracing::info!("中继重连成功: fp={}", hex::encode(target_fp));
                return Some(conn);
            }
            Err(e) => tracing::debug!("中继重连失败(第 {} 次): {}", backoff.fails + 1, e),
        }
        match backoff.record_failure() {
            Some(secs) => tokio::time::sleep(std::time::Duration::from_secs(secs)).await,
            None => {
                tracing::info!("中继重连放弃(连续 3 次失败): fp={}", hex::encode(target_fp));
                return None;
            }
        }
    }
}

/// 回路 1 壳层入口:重连 + adopt 为发起方。true = 会话已恢复。
/// adopt 失败直接 false(会话层异常,重连不解决;等下次 SessionDown 再触发)。
pub async fn auto_reconnect(
    client: &Arc<crate::relay::client::RelayClient>,
    sm: &Arc<crate::session::SessionManager>,
    target_fp: [u8; 32],
) -> bool {
    let Some(conn) = reconnect_peer_conn(client, target_fp).await else {
        return false;
    };
    match sm.adopt_as_initiator(conn).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("中继自愈 adopt 失败: {}", e);
            false
        }
    }
}
```

- [ ] **Step 4: 跑 E2E 确认通过**

Run: `cargo test -p localtrans-relay --test e2e autoheal_after -- --test-threads=1`
Expected: PASS(总时长约 40-60s:15s 判死 + 重连)

- [ ] **Step 5: relay 全量回归**

Run: `cargo test -p localtrans-relay -- --test-threads=1`
Expected: 全 PASS

- [ ] **Step 6: Commit**

```bash
git add crates/localtrans-core/src/relay/autoheal.rs crates/localtrans-relay/tests/e2e.rs
git commit -m "feat(core): 自愈编排 reconnect_peer_conn/auto_reconnect + 断路自愈 E2E

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 5: 桌面壳接线(回路 1 + 回路 2 + UI)

**Files:**
- Modify: `src-tauri/src/main.rs`(AppState 字段 + SessionDown/SessionUp 分支)
- Modify: `ui/src/api.ts`(onPeerReconnecting)
- Modify: `ui/src/stores/devices.ts`(reconnecting 集合)
- Modify: `ui/src/components/DeviceCard.vue`(重连中徽章)

**Interfaces:**
- Consumes: Task 4 `localtrans_core::relay::autoheal::auto_reconnect`、Task 3 `auto_resumable_jobs`;`localtrans_core::transfer::pending_jobs(&Path) -> Vec<(u64, Manifest)>`(transfer/mod.rs:23);`crate::commands::resume_pending(State<'_, AppState>, AppHandle, String) -> Result<(), String>`(commands.rs:1431,pub)。
- Produces: 新 Tauri 事件 `peer-reconnecting`(payload `{fingerprint: String, active: bool}`),UI 消费。

- [ ] **Step 1: AppState 加两个字段**

`main.rs` AppState 结构体(74 行 `}` 前,`relay_event_task` 字段之后)加:

```rust
    /// 回路 1 去重:正在自愈的指纹(SessionDown 触发时 insert,结束移除)
    healing_fps: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 回路 2 一次性守卫:已自动续传过的 job_id(永不二次自动续传)
    auto_retried: Arc<Mutex<std::collections::HashSet<u64>>>,
```

构造处(约 561 行 `relay: Arc::new(Mutex::new(None))` 附近)加:

```rust
                    healing_fps: Arc::new(Mutex::new(std::collections::HashSet::new())),
                    auto_retried: Arc::new(Mutex::new(std::collections::HashSet::new())),
```

(注意 main.rs 的 Mutex 是 `tokio::sync::Mutex`——与 relay/relay_roster 字段的 `lock().await` 用法一致。)

- [ ] **Step 2: SessionDown 分支接回路 1**

`main.rs:822` SessionDown 分支,在现有 `emit("connection-state", ...)` 之后追加:

```rust
                                // 回路 1:中继会话自愈(本地发现无此设备 && 名册有)
                                {
                                    let st = app_handle_clone.try_state::<AppState>().unwrap();
                                    let is_local = st.devices.lock().await.iter()
                                        .any(|d| d.fingerprint == fingerprint);
                                    let in_roster = st.relay_roster.lock().await.iter()
                                        .any(|d| d.fingerprint == fingerprint);
                                    if !is_local && in_roster {
                                        // 去重:已在自愈中则跳过(insert 返回 false 表示已存在)
                                        if st.healing_fps.lock().await.insert(fp_hex.clone()) {
                                            let relay = st.relay.lock().await.clone();
                                            let sm = st.sm.clone();
                                            let healing = st.healing_fps.clone();
                                            let fp_hex_ui = fp_hex.clone();
                                            let app_ui = app_handle_clone.clone();
                                            tokio::spawn(async move {
                                                let _ = app_ui.emit("peer-reconnecting", serde_json::json!({
                                                    "fingerprint": fp_hex_ui, "active": true
                                                }));
                                                let ok = match relay {
                                                    Some(client) => localtrans_core::relay::autoheal::auto_reconnect(&client, &sm, fingerprint).await,
                                                    None => false,
                                                };
                                                if !ok {
                                                    let _ = app_ui.emit("peer-reconnecting", serde_json::json!({
                                                        "fingerprint": fp_hex_ui, "active": false
                                                    }));
                                                }
                                                // 成功时 SessionUp 事件驱动 UI 清态;此处统一清守卫
                                                healing.lock().await.remove(&fp_hex_ui);
                                            });
                                        }
                                    }
                                }
```

注意:闭包变量 `fingerprint` 是 `[u8;32]`(SessionDown 事件字段),需 `Copy`(确认 `[u8;32]` 是 Copy,可直接 move 进 spawn)。

- [ ] **Step 3: SessionUp 分支接回路 2**

`main.rs:803` SessionUp 分支,在现有 `emit("connection-state", ...)` 之后追加:

```rust
                                // 回路 2:对端恢复在线 → 自动续传 failed 且有 parts 的任务(每任务仅 1 次)
                                {
                                    let st = app_handle_clone.try_state::<AppState>().unwrap();
                                    let st_for_jobs = st.inner().clone();
                                    let app2 = app_handle_clone.clone();
                                    let fp_hex2 = fp_hex.clone();
                                    tokio::spawn(async move {
                                        let download_dir = st_for_jobs.config.read().await.download_dir.clone();
                                        let pending: Vec<u64> = localtrans_core::transfer::pending_jobs(&download_dir)
                                            .into_iter().map(|(id, _)| id).collect();
                                        let candidates = {
                                            let transfers = st_for_jobs.transfers.lock().await;
                                            let retried = st_for_jobs.auto_retried.lock().await;
                                            localtrans_core::relay::autoheal::auto_resumable_jobs(
                                                transfers.iter().map(|(id, d)| (*id, d.state.clone(), d.peer.clone())).collect(),
                                                &fp_hex2, &pending, &retried,
                                            )
                                        };
                                        for job_id in candidates {
                                            st_for_jobs.auto_retried.lock().await.insert(job_id);
                                            let _ = app2.emit("toast", serde_json::json!({
                                                "level": "info",
                                                "text": "连接恢复,已自动续传"
                                            }));
                                            let state_for_resume = app2.state::<AppState>();
                                            let _ = crate::commands::resume_pending(
                                                state_for_resume, app2.clone(), format!("{:x}", job_id),
                                            ).await;
                                        }
                                    });
                                }
```

**实施者注意**:`st.inner()` 是否存在以 main.rs 实际代码为准——若 AppState 无 `inner()`(那是 commands.rs 里 State 的用法),则直接用 `st` 的字段(config/transfers/auto_retried 都是 `AppState` 上的 `Arc`,clone 出来即可):

```rust
                                    let config_arc = st.config.clone();
                                    let transfers_arc = st.transfers.clone();
                                    let retried_arc = st.auto_retried.clone();
```

以编译通过为准,语义不变:算出 candidates → 标记 auto_retried → toast → 调 `resume_pending`。`resume_pending` 需要 `State<'_, AppState>`,从 `app2.state::<AppState>()` 获取(tauri::Manager trait,main.rs 已有 use)。

- [ ] **Step 4: UI——api.ts 加事件**

`ui/src/api.ts` 的 `onConnectionState`(429 行)之后加:

```ts
/**
 * 中继会话自愈状态(重连中/放弃)
 */
export function onPeerReconnecting(callback: (s: { fingerprint: string; active: boolean }) => void) {
  return onEvent('peer-reconnecting', callback)
}
```

- [ ] **Step 5: UI——devices store 维护 reconnecting 集合**

`ui/src/stores/devices.ts`:import 行(第 9 行)改为:

```ts
import { api, onDeviceList, onConnectionState, onPeerReconnecting } from '../api'
```

状态定义区(`selected_fp` 附近)加:

```ts
  /** 中继自愈中的指纹(设备卡片显示"重连中…") */
  const reconnecting = ref<Set<string>>(new Set())
```

`setupEventListeners`(149-162 行)里,`onConnectionState` 回调改为并在其后追加:

```ts
    onConnectionState(({ fingerprint, up }) => {
      devices.value = devices.value.map((d) =>
        d.fingerprint === fingerprint ? { ...d, connected: up } : d
      )
      // 会话恢复 → 清"重连中"态(SessionUp 自然到达)
      if (up) {
        const s = new Set(reconnecting.value)
        s.delete(fingerprint)
        reconnecting.value = s
      }
    })

    // 中继自愈状态(后端放弃时 active:false;成功由 connection-state up 清)
    onPeerReconnecting(({ fingerprint, active }) => {
      const s = new Set(reconnecting.value)
      if (active) s.add(fingerprint)
      else s.delete(fingerprint)
      reconnecting.value = s
    })
```

return 对象(`selected_fp,` 之后)加 `reconnecting,`。

- [ ] **Step 6: UI——DeviceCard 重连中徽章**

`ui/src/components/DeviceCard.vue`:

script 区(`const isConnecting = ref(false)` 附近)加:

```ts
const devicesStore = useDevicesStore()

// 中继自愈中(卡片状态徽章显示"重连中…")
const isReconnecting = computed(() => devicesStore.reconnecting.has(props.device.fingerprint))
```

(确认 `useDevicesStore` 的 import 与 `computed` 已在既有 import 中;若 devicesStore 未引入则补 `import { useDevicesStore } from '../stores/devices'`。)

template 的 `device-status` 区(19-25 行)改为:

```html
      <div class="device-status">
        <div v-if="isReconnecting" class="status-badge reconnecting">重连中…</div>
        <div v-else-if="device.connected" class="status-badge connected">已连接</div>
        <div v-else-if="device.online" class="status-badge online">在线</div>
        <div v-else class="status-badge offline">离线</div>
        <div v-if="isTrusted" class="paired-badge">已配对</div>
        <div v-else class="paired-badge unpaired">待配对</div>
      </div>
```

style 区(找现有 `.status-badge` 样式块,同级追加;颜色对齐项目 design token 的 warning 色,若无对应 token 用以下兜底):

```css
.status-badge.reconnecting {
  background: rgba(240, 154, 10, 0.15);
  color: #f09a0a;
}
```

- [ ] **Step 7: 编译 + 回归**

Run: `cargo test -p localtrans-core --lib -- --test-threads=1`
Run: `cargo check -p localtrans-app 2>/dev/null || cargo check --workspace`
(以仓库实际 package 名为准;src-tauri 的包名看 `src-tauri/Cargo.toml` 的 `[package] name`。)
Run: 前端构建 `cd ui && npm run build`(scripts 名以 ui/package.json 为准)
Expected: Rust 全 PASS + cargo check 无错 + 前端构建成功

- [ ] **Step 8: Commit**

```bash
git add src-tauri/src/main.rs ui/src/api.ts ui/src/stores/devices.ts ui/src/components/DeviceCard.vue
git commit -m "feat(desktop): 壳层自愈接线——SessionDown 自动重连 + SessionUp 自动续传 + 重连中徽章

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 6: FFI 壳接线(安卓,镜像桌面)

**Files:**
- Modify: `crates/localtrans-ffi/src/state.rs`(AppState 加 auto_retried)
- Modify: `crates/localtrans-ffi/src/lib.rs`(retry 核心抽函数 + SessionDown/SessionUp 分支接线)

**Interfaces:**
- Consumes: Task 4 `auto_reconnect`、Task 3 `auto_resumable_jobs`;`App::retry_transfer(&self, job_id: u64) -> u64`(lib.rs:1408,改造为薄包装)。
- Produces: `pub(crate) fn spawn_retry_transfer(state: &Arc<state::AppState>, callback: &Arc<Box<dyn LocalTransCallback>>, runtime: &Arc<tokio::runtime::Runtime>, job_id: u64)`(lib.rs 内部)。
- Android/Kotlin 侧**零改动**(SessionUp/Down 已驱动 DevicesViewModel 刷新;自动续传经现有 TransferUpdated 事件自然上屏;不新增 AppEvent → 不需要重建 uniFFI 绑定)。

- [ ] **Step 1: state.rs 加字段**

`crates/localtrans-ffi/src/state.rs` AppState(`relay_event_task` 字段之后)加:

```rust
    /// 回路 1 去重:正在自愈的指纹
    pub healing_fps: Arc<Mutex<std::collections::HashSet<String>>>,
    /// 回路 2 一次性守卫:已自动续传过的 job_id
    pub auto_retried: Arc<Mutex<std::collections::HashSet<u64>>>,
```

构造函数同步初始化(`Arc::new(Mutex::new(std::collections::HashSet::new()))`;注意此处 Mutex 是 std 的 `sync::Mutex`,与该文件既有 `connected_fps` 用法一致)。

- [ ] **Step 2: retry_transfer 核心抽为自由函数**

lib.rs:将 `retry_transfer`(1408-1523 行)中"从 `let inbox = ...` 开始到方法尾"的 **spawn 部分**抽为:

```rust
/// 续传核心(抽自 retry_transfer,供命令入口与自动续传共用)。
/// 行为与原 retry_transfer 完全一致:占位行 → xfer_lock(60s)→ start_pull_into → 进度泵。
pub(crate) fn spawn_retry_transfer(
    state: &std::sync::Arc<crate::state::AppState>,
    callback: &std::sync::Arc<Box<dyn LocalTransCallback>>,
    job_id: u64,
) {
    // === 原 retry_transfer 1408-1523 行的函数体原样搬入 ===
    // 差异仅三处:
    // 1. self.state.lock() → state 直接用(参数已是 Arc<AppState>)
    // 2. self.callback.clone() → callback.clone()
    // 3. self.runtime.spawn / block_on → 用 tokio::runtime::Handle::current()
    //    (调用方在 runtime 上下文内;若从同步入口调,App::retry_transfer 负责 block_on)
}
```

**实施者注意(此步骤必须完整执行,不是占位)**:打开 lib.rs 1408-1523 行,把函数体逐行搬入 `spawn_retry_transfer`,按上面三条差异做机械替换。搬完后 `App::retry_transfer` 改为:

```rust
    /// Retry a failed/interrupted transfer(薄包装:找 job + 抽核心)
    pub fn retry_transfer(&self, job_id: u64) -> u64 {
        let state_guard = self.state.lock().unwrap();
        match &*state_guard {
            Some(state) => {
                // 未找到任务返回 0(原逻辑),否则调核心
                let jobs = localtrans_core::transfer::pending_jobs(&state.dir.clone());
                if jobs.into_iter().find(|(id, _)| *id == job_id).is_none() {
                    return 0;
                }
                let runtime = self.runtime.clone();
                let _ = runtime.spawn(async move {
                    // spawn_retry_transfer 需要 runtime 句柄内的同步调用;
                    // 其内部若含 block_on(原 1450 行 config 读取),改为
                    // async 化:let config = state.config.read().await.clone();
                    spawn_retry_transfer_inner(state_for_task, callback_for_task, job_id).await;
                });
                job_id
            }
            None => 0,
        }
    }
```

**简化裁定**:原函数体里 1450 行的 `self.runtime.block_on(async { state.config.read().await.clone() })` 在抽函数后处于 async 上下文,改为 `.await`;整个核心改为 `pub(crate) async fn spawn_retry_transfer(state, callback, job_id)`(async 化,内部 spawn 的恢复任务保持 `tokio::spawn`)。两个调用点:
- `App::retry_transfer`(同步入口)最终形态(上面 Step 2 代码块中 `state_for_task/callback_for_task` 未定义的笔误,以本形态为准):

```rust
    /// Retry a failed/interrupted transfer(薄包装:找 job + 抽核心)
    pub fn retry_transfer(&self, job_id: u64) -> u64 {
        let state_guard = self.state.lock().unwrap();
        let Some(state) = state_guard.as_ref() else { return 0 };
        let jobs = localtrans_core::transfer::pending_jobs(&state.dir.clone());
        if jobs.into_iter().find(|(id, _)| *id == job_id).is_none() {
            return 0;
        }
        let state_arc = state.clone();       // state 是 &Arc<AppState>,clone 得新 Arc
        let callback = self.callback.clone();
        self.runtime.spawn(async move {
            spawn_retry_transfer(&state_arc, &callback, job_id).await;
        });
        job_id
    }
```

- 事件泵 SessionUp 分支(async 上下文):直接 `tokio::spawn(async move { spawn_retry_transfer(...).await });`

测试 `retry_transfer` 相关既有单测(lib.rs tests 模块)必须保持通过——它们走 `App::retry_transfer` 公开入口,行为不变(返回值语义:未找到 0,找到返回 job_id)。

- [ ] **Step 3: SessionDown 分支接回路 1**

lib.rs 475-481 行 SessionDown 分支,`callback_for_bridge.on_event(AppEvent::SessionDown {...})` 之后追加:

```rust
                        // 回路 1:中继会话自愈(镜像桌面壳 main.rs)
                        {
                            let st = state_for_bridge.clone();
                            let rt = runtime_for_autoheal.clone(); // 见下方闭包捕获说明
                            let fp_bytes = fingerprint; // [u8;32], Copy
                            let fp_hex_auto = fp_hex.clone();
                            tokio::spawn(async move {
                                let is_local = st.devices.lock().unwrap()
                                    .iter().any(|d| d.fingerprint == fp_bytes);
                                let in_roster = st.relay_roster.lock().unwrap()
                                    .iter().any(|d| d.fingerprint == fp_bytes);
                                if is_local || !in_roster { return; }
                                if !st.healing_fps.lock().unwrap().insert(fp_hex_auto.clone()) { return; }
                                let client = st.relay.lock().await.clone();
                                let Some(client) = client else {
                                    st.healing_fps.lock().unwrap().remove(&fp_hex_auto);
                                    return;
                                };
                                let sm = st.sm.clone();
                                let ok = localtrans_core::relay::autoheal::auto_reconnect(&client, &sm, fp_bytes).await;
                                if !ok {
                                    tracing::info!("[autoheal] 放弃: fp={}", fp_hex_auto);
                                }
                                st.healing_fps.lock().unwrap().remove(&fp_hex_auto);
                                let _ = rt; // runtime 句柄备用(若 spawn_retry 需要同步入口;当前不需要)
                            });
                        }
```

**闭包捕获说明**:事件泵闭包(458 行区域)已捕获 `state_for_bridge` 与 `callback_for_bridge`;若 `runtime` 未捕获,在闭包外(506 行 `spawn_relay` 调用之前的闭包构造处)加 `let runtime_for_autoheal = self.runtime.clone();` 并列入 move 捕获。以实际闭包结构为准,目标是异步分支里能 `tokio::spawn`。

- [ ] **Step 4: SessionUp 分支接回路 2**

lib.rs 467-474 行 SessionUp 分支,`on_event(AppEvent::SessionUp {...})` 之后追加:

```rust
                        // 回路 2:对端恢复在线 → 自动续传(镜像桌面壳;只 1 次)
                        {
                            let st = state_for_bridge.clone();
                            let cb = callback_for_bridge.clone();
                            let fp_hex_auto = fp_hex.clone();
                            tokio::spawn(async move {
                                let dir = st.dir.clone();
                                let pending: Vec<u64> = localtrans_core::transfer::pending_jobs(&dir)
                                    .into_iter().map(|(id, _)| id).collect();
                                let candidates = {
                                    let transfers = st.transfers.lock().await;
                                    let retried = st.auto_retried.lock().unwrap();
                                    localtrans_core::relay::autoheal::auto_resumable_jobs(
                                        transfers.iter().map(|(id, d)| (*id, d.state.clone(), d.peer.clone())).collect(),
                                        &fp_hex_auto, &pending, &retried,
                                    )
                                };
                                for job_id in candidates {
                                    st.auto_retried.lock().unwrap().insert(job_id);
                                    cb.on_event(AppEvent::Hello { message: "连接恢复,已自动续传".into() });
                                    spawn_retry_transfer(&st, &cb, job_id).await;
                                }
                            });
                        }
```

**注意**:toast 事件复用 `AppEvent::Hello`(message 字段)是**临时方案审定**——若 AppEvent 已有更合适的通知变体(浏览 lib.rs AppEvent 枚举 46-66 行后自行判断),用之;**不要新增 AppEvent 变体**(新增会触发 Kotlin 绑定重建,本轮明确零 Kotlin 改动)。若 Hello 语义不妥,可直接不打 toast(自动续传的可见性由 TransferUpdated 进度事件天然承载——任务从 failed 变 pending/active 即用户可见)。

- [ ] **Step 5: 编译 + FFI 回归**

Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Run: `cargo check -p localtrans-core`
Expected: 全 PASS(含 retry_transfer 既有单测)

- [ ] **Step 6: Commit**

```bash
git add crates/localtrans-ffi/src/state.rs crates/localtrans-ffi/src/lib.rs
git commit -m "feat(ffi): 安卓壳自愈接线——SessionDown 自动重连 + SessionUp 自动续传(零 Kotlin 改动)

Co-Authored-By: Claude <noreply@anthropic.com>"
```

---

### Task 7: 全量回归 + CHANGELOG + 版本收尾

**Files:**
- Modify: `CHANGELOG.md`
- Modify: 版本号三处(`src-tauri/Cargo.toml`、`src-tauri/tauri.conf.json`、`Cargo.toml` workspace package 或各 crate——先 grep 定位)

**Interfaces:**
- Consumes: Task 1-6 全部产出。
- Produces: v0.9.2 收尾(版本号按 grep 结果统一递增;若发现当前版本与预期不符,以现有最高位为准 +0.0.1)。

- [ ] **Step 1: 全量回归**

Run: `cargo test -p localtrans-core -- --test-threads=1`
Run: `cargo test -p localtrans-relay -- --test-threads=1`
Run: `cargo test -p localtrans-ffi -- --test-threads=1`
Run: `cargo test --manifest-path src-tauri/Cargo.toml -- --test-threads=1`
Expected: 全 PASS(e2e 套件含 3 个新测试,总时长约多 2 分钟)

- [ ] **Step 2: 定位并递增版本号**

Run: `grep -rn '0\.9\.1\|0\.9\.0\|version' src-tauri/Cargo.toml src-tauri/tauri.conf.json Cargo.toml ui/package.json 2>/dev/null | grep -i version`
按结果把所有桌面端版本声明统一递增到下一补丁位(预期 0.9.2;若 src-tauri/Cargo.toml 仍是 0.8.3 而其余是 0.9.x,统一到 0.9.2)。

- [ ] **Step 3: CHANGELOG 追加**

`CHANGELOG.md` 顶部追加(格式对齐既有条目):

```markdown
## [0.9.2] - 2026-08-24

### 修复
- 中继数据面 NAT 漂移跟踪:DATA 包源地址变化自动重学习,手机切网/运营商重分配后传输不再突然 0 字节
- 中继内层端点 keep-alive 对称:client 侧补挂 TransportConfig,双侧 idle 收紧 15s,死连接 15s 判死(原 30~60s)

### 新增
- 壳层自动重连:中继会话断开自动重 punch(退避 1s/2s,3 败停),设备卡片显示"重连中…"
- 壳层自动续传:对端恢复在线后自动续传 failed 且有断点的任务(每任务仅 1 次)
```

- [ ] **Step 4: Commit**

```bash
git add CHANGELOG.md src-tauri/Cargo.toml src-tauri/tauri.conf.json Cargo.toml ui/package.json
git commit -m "chore(release): v0.9.2 收尾——CHANGELOG + 版本号

Co-Authored-By: Claude <noreply@anthropic.com>"
```

(打包/发 Release 不在本计划内——由用户手打 dist;dist zip 规约:不含 data/ 目录。)

---

## 执行说明(Subagent-Driven)

- 按任务序派发(Task 1 → 7),每任务 implementer + reviewer 两阶段。
- Task 2/4 的 e2e 测试耗时 20-60s/个,reviewer 无需重跑(实施者报告携测试输出);reviewer 重点核代码。
- Task 5/6 涉及既有大文件(main.rs / lib.rs),实施者必须先读目标分支上下文再改,禁止盲写行号。
- 中继服务器部署提醒:Task 1 改的是 relay 服务端——用户阿里云需重新编译部署后才对现网生效(桌面/安卓客户端先发不冲突,协议无变更)。

## 与 spec 测试策略的对照(覆盖声明)

spec 测试表 4 行的落点:
- relay NAT 漂移重学习(单测)→ Task 1 ✅
- virtual_ep 15s 判死 / 心跳互保(集成)→ Task 2 两个 e2e ✅
- 自动重连函数状态机(单测)→ Task 3(纯逻辑)+ Task 4(reconnect_peer_conn 走 e2e)✅
- E2E 杀中继→自愈→断点续传 → **部分覆盖**:Task 4 的 e2e 验证"断路→判死→重连→消息恢复";**完整断点续传**(传输中杀中继→续传后文件完整)未纳入本轮——需要共享区/引擎层夹具,改动量大。裁定:Task 4 e2e + 既有 parts 续传链路测试(桌面壳 resume 路径)组合已覆盖风险面;完整传输级 E2E 记 backlog,装机实测时人工验证(用户重测 37800ef 时一并跑)。
