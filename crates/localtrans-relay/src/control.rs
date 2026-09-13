//! 控制面:QUIC 监听 + PSK 通道绑定 + 注册/名册/打洞。
//! 服务端回话统一走 uni 流(客户端 accept_uni 收)。

use crate::config::RelayConfig;
use crate::lease::LeaseTable;
use localtrans_core::relay::proto::{
    decode_relay_msg, encode_relay_msg, RelayMsg,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;
use quinn::{Connection, RecvStream, SendStream};

/// 单个已注册客户端的控制连接上下文
struct CtrlConn {
    conn: Connection,
    conn_id: u64,
    fp: [u8; 32],
    /// uni 流发送端(服务端 → 客户端推送)
    push_tx: Mutex<Option<SendStream>>,
}

pub struct RelayServer {
    pub config: RelayConfig,
    pub leases: Arc<LeaseTable>,
    endpoint: quinn::Endpoint,
    /// fp → 控制连接
    conns: Arc<Mutex<HashMap<[u8; 32], Arc<CtrlConn>>>>,
    /// 认证失败计数(IP → (窗口起点, 次数))
    auth_failures: Arc<Mutex<HashMap<std::net::IpAddr, (std::time::Instant, u32)>>>,
    /// IP 拉黑表(IP → 拉黑截止时刻)
    blocked_ips: Arc<Mutex<HashMap<std::net::IpAddr, std::time::Instant>>>,
    shutdown_flag: Arc<std::sync::atomic::AtomicBool>,
    /// 名册版本号(每次 broadcast 前递增)
    roster_rev: Arc<std::sync::atomic::AtomicU64>,
    /// Punch 滑窗速率限制:fp → 窗口内请求时刻(S6,10 次/分钟)
    punch_rate: Arc<Mutex<HashMap<[u8; 32], Vec<std::time::Instant>>>>,
    /// S7:当前活跃控制连接数(全局上限)
    active_conns: std::sync::atomic::AtomicUsize,
    /// S7:单 IP 活跃连接数(IP → 计数)
    ip_conns: Arc<Mutex<HashMap<std::net::IpAddr, usize>>>,
}

/// S7 连接计数守卫:正常路径在 handle_conn 任务末尾用 .lock().await 精确递减
/// (见 release_conn_slot);Drop 仅作为任务被 abort 时的兜底。
struct ConnGuard {
    server: Arc<RelayServer>,
    ip: std::net::IpAddr,
    /// 正常路径已递减过则置 true,Drop 跳过单 IP 兜底(防双重递减);
    /// 全局 AtomicUsize 的递减幂等性靠 release 侧 set 之前的 fetch_sub 顺序保证。
    released: std::sync::atomic::AtomicBool,
}
impl ConnGuard {
    /// 正常路径释放:任务末尾调用,用 .lock().await 精确递减单 IP 计数
    /// (Drop 里的 try_lock 在锁被短暂时会失败,累积漏减会让该 IP 被永久拒绝)。
    /// 幂等:released 标记保证只生效一次。
    async fn release(&mut self) {
        if self.released.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let mut m = self.server.ip_conns.lock().await;
        match m.entry(self.ip) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                *e.get_mut() -= 1;
                if *e.get() == 0 {
                    e.remove();
                }
            }
            _ => {}
        }
    }
}
impl Drop for ConnGuard {
    fn drop(&mut self) {
        // 原子递减全局计数(无锁,不会失败)
        self.server.active_conns.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        // 兜底递减单 IP 计数:仅当正常路径未执行时(任务被 abort)
        if !self.released.swap(true, std::sync::atomic::Ordering::SeqCst) {
            if let Ok(mut m) = self.server.ip_conns.try_lock() {
                match m.entry(self.ip) {
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        *e.get_mut() -= 1;
                        if *e.get() == 0 {
                            e.remove();
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// S4:单次推送超时——慢读者/死链在 3s 内被检出并断连清理
const PUSH_TIMEOUT: Duration = Duration::from_secs(3);
/// S7:全局并发控制连接上限
const MAX_CONNS: usize = 512;
/// S7:单 IP 并发控制连接上限
const MAX_CONNS_PER_IP: usize = 8;

impl RelayServer {
    /// S4 推送统一入口:快照外的连接推送都走这里。
    /// 超时/失败 → 关闭连接 + 清租约与连接表,防慢读者把服务端拖死。
    async fn push_guarded(&self, fp: &[u8; 32], cc: &CtrlConn, msg: &RelayMsg) {
        let send = cc.push(msg);
        match tokio::time::timeout(PUSH_TIMEOUT, send).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!("推送失败断连 fp={}: {}", hex::encode(&fp[..8]), e);
                self.evict(fp, &cc.conn).await;
            }
            Err(_) => {
                tracing::warn!("推送超时({:?})断连 fp={}", PUSH_TIMEOUT, hex::encode(&fp[..8]));
                self.evict(fp, &cc.conn).await;
            }
        }
    }

    /// 断开慢读者/异常连接并清表
    async fn evict(&self, fp: &[u8; 32], conn: &Connection) {
        conn.close(0u8.into(), b"push timeout");
        self.conns.lock().await.remove(fp);
        self.leases.remove(fp);
    }
}

impl RelayServer {
    /// 绑定控制面端口(数据面 socket 由 Task 4 创建;本任务先占位)
    pub async fn bind(config: RelayConfig) -> Result<Arc<Self>, String> {
        let kp = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let params = rcgen::CertificateParams::new(vec!["localtrans-relay".into()])
            .map_err(|e| e.to_string())?;
        let cert = params.self_signed(&kp).map_err(|e| e.to_string())?;

        let mut roots = rustls::RootCertStore::empty();
        // 服务端不要求客户端证书(PSK 即身份引导,指纹在 Register 里声明,
        // 指纹真实性由后续设备间 mTLS 保证——冒名指纹收不到任何数据)
        let _ = roots;

        let mut server_cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert.der().clone()], rustls_pki_types::PrivateKeyDer::Pkcs8(kp.serialize_der().into()))
            .map_err(|e| e.to_string())?;
        // ALPN 协议名直接赋值
        server_cfg.alpn_protocols = vec![b"localtrans-relay".to_vec()];

        let mut alpn_cfg = quinn::ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(server_cfg).map_err(|e| e.to_string())?,
        ));
        // S7:控制面专用传输配置——不发 keep-alive,靠客户端 15s Ping +
        // 服务端应用层超时(证明 10s / 推送 3s)判定存活并回收半开连接
        alpn_cfg.transport_config(localtrans_core::session::relay_control_transport_config());

        let bind_addr = SocketAddr::new(config.public_ip.is_ipv4().then(|| "0.0.0.0".parse().unwrap()).unwrap_or("::".parse().unwrap()), config.control_port);

        // 自绑 socket 并设 SO_REUSEADDR:服务器重启(systemd Restart)与
        // 测试同端口重绑场景,Windows 的 10048 会持续数秒——REUSEADDR 让
        // TIME_WAIT 端口立即可绑(quinn Endpoint::server 不给设选项的机会,
        // 所以手动走 Endpoint::new)。
        let endpoint = {
            // 注意顺序:socket2 先建 socket → 设 REUSEADDR → 再 bind
            // (std UdpSocket::bind 绑完再设无效——Windows 上必须 bind 前)
            let bind_socket2 = socket2::Socket::new(
                if bind_addr.is_ipv4() { socket2::Domain::IPV4 } else { socket2::Domain::IPV6 },
                socket2::Type::DGRAM,
                Some(socket2::Protocol::UDP),
            ).map_err(|e| e.to_string())?;
            bind_socket2.set_reuse_address(true).map_err(|e| e.to_string())?;
            bind_socket2.bind(&bind_addr.into())
                .map_err(|e| format!("控制面绑定 {bind_addr} 失败: {e}"))?;
            bind_socket2.set_nonblocking(true).map_err(|e| e.to_string())?;
            let std_sock: std::net::UdpSocket = bind_socket2.into();
            quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(alpn_cfg),
                std_sock,
                std::sync::Arc::new(quinn::TokioRuntime),
            ).map_err(|e| e.to_string())?
        };

        let data_port_start = config.data_port_start;
        let data_port_end = config.data_port_end;

        Ok(Arc::new(Self {
            config,
            leases: Arc::new(LeaseTable::new(data_port_start..data_port_end)),
            endpoint,
            conns: Arc::new(Mutex::new(HashMap::new())),
            auth_failures: Arc::new(Mutex::new(HashMap::new())),
            blocked_ips: Arc::new(Mutex::new(HashMap::new())),
            shutdown_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            roster_rev: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            punch_rate: Arc::new(Mutex::new(HashMap::new())),
            active_conns: std::sync::atomic::AtomicUsize::new(0),
            ip_conns: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    pub fn local_control_addr(&self) -> SocketAddr {
        self.endpoint.local_addr().unwrap()
    }

    pub fn public_ip(&self) -> std::net::IpAddr {
        self.config.public_ip
    }

    pub async fn shutdown(&self) {
        self.shutdown_flag.store(true, std::sync::atomic::Ordering::SeqCst);
        self.endpoint.close(0u8.into(), b"shutdown");
    }

    /// 常驻循环:accept + 回收节拍(5s)+ 速率限制窗口清理
    pub async fn run(self: Arc<Self>) {
        let mut reap_tick = tokio::time::interval(Duration::from_secs(5));
        let mut ratelimit_tick = tokio::time::interval(Duration::from_secs(30));
        loop {
            tokio::select! {
                incoming = self.endpoint.accept() => {
                    let Some(incoming) = incoming else { break };

                    // S7 全局上限:进入即占位(fetch_add 预留),超发在握手前
                    // 就被拦住——并发突发不会越过 512(check-then-add 竞态)
                    let n = self.active_conns.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if n > MAX_CONNS {
                        // 回滚占位并拒(不进入握手)
                        self.active_conns.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                        tracing::warn!("活跃连接达上限 {}, 拒绝新连接", MAX_CONNS);
                        incoming.refuse();
                        continue;
                    }

                    let server = self.clone();
                    tokio::spawn(async move {
                        match incoming.await {
                            Ok(conn) => {
                                let ip = conn.remote_address().ip();
                                // S7 单 IP 上限:握手后计数检查,超限关连接
                                // (全局占位在上面已预留,回滚见下方)
                                let over_limit = {
                                    let mut per_ip = server.ip_conns.lock().await;
                                    let count = per_ip.entry(ip).or_insert(0);
                                    if *count >= MAX_CONNS_PER_IP {
                                        true
                                    } else {
                                        *count += 1;
                                        false
                                    }
                                };
                                if over_limit {
                                    tracing::warn!("IP {} 活跃连接达上限 {}", ip, MAX_CONNS_PER_IP);
                                    // 回滚全局占位再关闭(单 IP 未递增,无需回滚)
                                    server.active_conns.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                    conn.close(0u8.into(), b"too many connections");
                                    return;
                                }
                                let mut guard = ConnGuard {
                                    ip,
                                    server: server.clone(),
                                    released: std::sync::atomic::AtomicBool::new(false),
                                };
                                let result = server.handle_conn(conn).await;
                                if let Err(e) = &result {
                                    tracing::warn!("控制连接处理结束: {}", e);
                                }
                                // Important 1:正常退出走精确释放(.lock().await,
                                // 不会像 Drop 的 try_lock 那样漏减)
                                guard.release().await;
                                drop(guard);
                            }
                            Err(e) => {
                                // 握手失败:回滚全局占位(未到单 IP 计数阶段)
                                server.active_conns.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                                tracing::warn!("握手失败: {}", e);
                            }
                        }
                    });
                }
                _ = reap_tick.tick() => {
                    let expired = self.leases.reap_expired(Duration::from_secs(self.config.lease_ttl_secs));
                    if !expired.is_empty() {
                        tracing::info!("回收 {} 个过期租约", expired.len());
                        self.broadcast_roster().await;
                    }
                }
                _ = ratelimit_tick.tick() => {
                    // 清理已过期的拉黑项与 auth_failures 窗口外条目
                    let now = std::time::Instant::now();
                    let mut blocked = self.blocked_ips.lock().await;
                    blocked.retain(|&ip, expiry| {
                        if now < *expiry {
                            true
                        } else {
                            tracing::debug!("拉黑期过,移除: {}", ip);
                            false
                        }
                    });

                    let mut failures = self.auth_failures.lock().await;
                    failures.retain(|&ip, (window_start, _count)| {
                        if now.duration_since(*window_start).as_secs() > 60 {
                            tracing::debug!("认证失败窗口过期,移除: {}", ip);
                            false
                        } else {
                            true
                        }
                    });

                    // backlog:Punch 速率表中窗口已全过期的 fp 直接清掉
                    {
                        let mut rate = self.punch_rate.lock().await;
                        rate.retain(|_fp, ts| !ts.is_empty());
                        for (_fp, ts) in rate.iter_mut() {
                            ts.retain(|&t| now.duration_since(t).as_secs() <= 60);
                        }
                    }
                }
            }
        }
    }

    /// PSK 校验 + 注册后进入消息循环
    async fn handle_conn(&self, conn: Connection) -> Result<(), String> {
        let remote_ip = conn.remote_address().ip();
        let conn_id = conn.stable_id() as u64;
        let conn_arc = Arc::new(conn.clone());

        // 查拉黑表:已拉黑且未到期则直接拒绝(不读 PSK 证明)
        {
            let blocked = self.blocked_ips.lock().await;
            if let Some(&expiry) = blocked.get(&remote_ip) {
                let now = std::time::Instant::now();
                if now < expiry {
                    tracing::warn!("拉黑IP尝试连接: {} (剩余 {}s)", remote_ip, (expiry - now).as_secs());
                    conn.close(0u8.into(), b"IP blocked");
                    return Err("IP 已被拉黑".into());
                }
            }
        }

        // 首条 bi 流:32B PSK 证明(S7:读证明包 10s 超时,防半开连接占位)
        let (mut tx_proof, mut rx_proof) = tokio::time::timeout(
            Duration::from_secs(10),
            conn.accept_bi(),
        ).await.map_err(|_| "等 PSK 证明流超时".to_string())?.map_err(|e| e.to_string())?;
        let mut proof = [0u8; 32];
        tokio::time::timeout(
            Duration::from_secs(10),
            rx_proof.read_exact(&mut proof),
        ).await.map_err(|_| "读 PSK 证明超时".to_string())?.map_err(|e| e.to_string())?;

        let mut expected = [0u8; 32];
        conn.export_keying_material(&mut expected, b"localtrans-relay-psk", self.config.psk.as_bytes())
            .map_err(|e| format!("export_keying_material error: {:?}", e))?;

        // 低危审计修复:恒时比较,防逐字节时序侧信道泄漏 PSK 证明
        let proof_matches: u8 = proof.iter()
            .zip(expected.iter())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b));
        if proof_matches != 0 {
            self.record_auth_failure(remote_ip).await?;
            // PSK 校验失败:经 uni 流回 Error 后立即关闭连接
            drop(tx_proof);
            let mut uni_err = conn.open_uni().await.map_err(|e| e.to_string())?;
            let err = RelayMsg::Error { code: 1, msg: "PSK 校验失败".into() };
            uni_err.write_all(&encode_relay_msg(&err).unwrap()).await.map_err(|e| e.to_string())?;
            uni_err.finish().map_err(|e| e.to_string())?;
            // 未认证连接不允许继续开 bi 流
            // 关闭连接确保 Error 消息送达后再断开
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
            conn.close(0u8.into(), b"PSK failed");
            return Err("PSK 校验失败".into());
        } else {
            // PSK 成功:丢弃 PSK 证明流的发送端(不再写任何字节到 bi 流)
            drop(tx_proof);
        }

        // 之后客户端每条消息走 bi 流(请求/响应),服务端推送走 uni 流

        // S2 占有证明:PSK 通过后先推一次性 nonce,等 Register 携签名
        let mut nonce = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        {
            let ctrl_tmp = CtrlConn { conn: (*conn_arc).clone(), conn_id, fp: [0u8; 32], push_tx: Mutex::new(None) };
            // S4:nonce 挑战同样走超时推送(慢读者在握手期也应被检出)
            tokio::time::timeout(PUSH_TIMEOUT, ctrl_tmp.push(&RelayMsg::ServerNonce { nonce }))
                .await
                .map_err(|_| "推 ServerNonce 超时".to_string())??;
        }

        // 第一条 bi 流必须是带签名的 Register;验证不过即断连+记认证失败
        let (reg_tx, reg_rx) = tokio::time::timeout(Duration::from_secs(10), conn.accept_bi())
            .await
            .map_err(|_| "等 Register 超时".to_string())?
            .map_err(|e| format!("接受 Register 流失败: {}", e))?;
        if let Err(e) = self.await_signed_register(conn_arc.clone(), conn_id, nonce, reg_rx).await {
            self.record_auth_failure(remote_ip).await?;
            tracing::warn!("Register 占有证明失败({}), 断连 IP: {}", e, remote_ip);
            return Err(e);
        }
        drop(reg_tx);

        loop {
            tokio::select! {
                bi = conn.accept_bi() => {
                    let (tx, rx) = bi.map_err(|e| e.to_string())?;
                    if let Err(e) = self.handle_request(conn_arc.clone(), conn_id, rx, tx).await {
                        tracing::warn!("请求处理失败: {}", e);
                    }
                }
                closed = conn.closed() => {
                    let _ = closed;
                    self.on_conn_closed(conn_id).await;
                    return Ok(());
                }
            }
        }
    }

    /// S2:等带签名的 Register 并做占有证明三连验证。
    /// 任一失败 → 回 Error + 断连(handle_conn 记认证失败)。
    async fn await_signed_register(
        &self,
        conn: Arc<Connection>,
        conn_id: u64,
        nonce: [u8; 32],
        mut rx: RecvStream,
    ) -> Result<(), String> {
        let msg = read_relay_frame(&mut rx).await?;

        let (name, fingerprint, hidden, cert_der, nonce_sig) = match msg {
            RelayMsg::Register { name, fingerprint, hidden, cert_der, nonce_sig } =>
                (name, fingerprint, hidden, cert_der, nonce_sig),
            other => {
                // 先回 Error 让客户端可见,再断连
                self.send_error_then_close(&conn, 2, "首条消息必须是 Register").await;
                return Err(format!("期望 Register, 实得 {:?}", other));
            }
        };

        // 占有证明:sign(fp || nonce)
        let mut signed = Vec::with_capacity(64);
        signed.extend_from_slice(&fingerprint);
        signed.extend_from_slice(&nonce);
        if let Err(e) = localtrans_core::identity::verify_possession(&fingerprint, &cert_der, &signed, &nonce_sig) {
            self.send_error_then_close(&conn, 3, "注册占有证明失败").await;
            return Err(format!("占有证明失败(fp={}): {}", hex::encode(fingerprint), e));
        }

        // 全过才分配租约 + 入表
        let allocated = self.leases.alloc(fingerprint, name.clone(), hidden);
        // M3a FR2:注册连接的源地址即该客户端的公网出口(NAT 外侧 ip:port),
        // 随 RegisterAck 回报——客户端存入本机地址池(名片/选路用)。
        let observed_addr = Some(conn.remote_address().to_string());
        if let Some(a) = allocated {
            let ctrl_conn = Arc::new(CtrlConn {
                conn: (*conn).clone(),
                conn_id,
                fp: fingerprint,
                push_tx: Mutex::new(None),
            });
            self.conns.lock().await.insert(fingerprint, ctrl_conn.clone());

            let lease = localtrans_core::relay::proto::LeaseInfo {
                data_port: a.port,
                token: a.token,
            };
            let ack = RelayMsg::RegisterAck { lease: Some(lease), observed_addr };
            if let Err(e) = tokio::time::timeout(PUSH_TIMEOUT, ctrl_conn.push(&ack)).await {
                tracing::debug!("RegisterAck 推送未完成(fp={}): {:?}", hex::encode(&fingerprint[..8]), e);
            }

            self.broadcast_roster_except(&fingerprint).await;
        } else {
            let ack = RelayMsg::RegisterAck { lease: None, observed_addr };
            let mut tx_uni = conn.open_uni().await.map_err(|e| e.to_string())?;
            let _ = tx_uni.write_all(&encode_relay_msg(&ack).unwrap()).await;
            let _ = tx_uni.finish();
        }

        Ok(())
    }

    /// 经 uni 流回 Error 后关闭连接
    async fn send_error_then_close(&self, conn: &Connection, code: u32, msg: &str) {
        if let Ok(mut uni) = conn.open_uni().await {
            let err = RelayMsg::Error { code, msg: msg.to_string() };
            let _ = uni.write_all(&encode_relay_msg(&err).unwrap()).await;
            let _ = uni.finish();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
        conn.close(0u8.into(), b"register rejected");
    }

    /// 测试辅助:查某 IP 的认证失败计数
    pub async fn auth_failure_count(&self, ip: std::net::IpAddr) -> u32 {
        self.auth_failures.lock().await.get(&ip).map(|(_, c)| *c).unwrap_or(0)
    }

    async fn record_auth_failure(&self, ip: std::net::IpAddr) -> Result<(), String> {
        let mut failures = self.auth_failures.lock().await;
        let entry = failures.entry(ip).or_insert_with(|| (std::time::Instant::now(), 0));
        entry.1 += 1;
        if entry.1 >= self.config.auth_max_per_min {
            // 达到上限:拉黑 10 分钟
            let now = std::time::Instant::now();
            let block_until = now + std::time::Duration::from_secs(600);
            self.blocked_ips.lock().await.insert(ip, block_until);
            tracing::warn!("IP {} 认证失败达到上限,拉黑 10 分钟", ip);
        }
        Ok(())
    }

    /// Punch 滑窗速率检查(S6):60s 窗口内超 10 次则拒。
    /// 通过即记入窗口并返回 true。
    async fn punch_rate_ok(&self, fp: &[u8; 32]) -> bool {
        const WINDOW: std::time::Duration = Duration::from_secs(60);
        const MAX_PER_WINDOW: usize = 10;
        let now = std::time::Instant::now();
        let mut rate = self.punch_rate.lock().await;
        let entry = rate.entry(*fp).or_default();
        entry.retain(|&t| now.duration_since(t) < WINDOW);
        if entry.len() >= MAX_PER_WINDOW {
            return false;
        }
        entry.push(now);
        true
    }

    async fn handle_request(
        &self,
        conn: Arc<Connection>,
        conn_id: u64,
        mut rx: RecvStream,
        mut tx: SendStream,
    ) -> Result<(), String> {
        let msg = read_relay_frame(&mut rx).await?;

        match msg {
            // S2:Register 已前移到 handle_conn 的占有证明阶段,不应再出现
            RelayMsg::Register { .. } => {
                return Err("Register 只允许作为首条消息".into());
            }
            RelayMsg::Ping => {
                // 刷新 heartbeat + 经 uni 流回 Roster(客户端刷新名册机制)
                // S4:锁内快照,锁外 3s 超时推送
                let found = {
                    let conns = self.conns.lock().await;
                    conns.iter()
                        .find(|(_, cc)| cc.conn_id == conn_id)
                        .map(|(fp, cc)| (*fp, Arc::clone(cc)))
                };
                if let Some((fp, cc)) = found {
                    self.leases.heartbeat(&fp);
                    // 递增版本号
                    let rev = self.roster_rev.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // 推送 Roster(排除自己,视角与 broadcast 一致)
                    let roster = RelayMsg::Roster {
                        rev,
                        devices: self.leases.snapshot_roster(&fp, self.config.public_ip),
                    };
                    self.push_guarded(&fp, &cc, &roster).await;
                }
                // 丢弃 bi 流的 tx(不写任何字节)
                drop(tx);
            }
            RelayMsg::Punch { target_fp } => {
                // S4:锁内快照查请求者与目标,推送全在锁外超时执行
                let lookup = {
                    let conns = self.conns.lock().await;
                    let requester_fp = conns.iter().find(|(_, cc)| cc.conn_id == conn_id).map(|(fp, _)| *fp);
                    let target_cc = conns.get(&target_fp).map(|cc| Arc::clone(cc));
                    let requester_cc = requester_fp.and_then(|fp| conns.get(&fp)).map(|cc| Arc::clone(cc));
                    (requester_fp, requester_cc, target_cc)
                };
                let (requester_fp, requester_cc, target_cc) = lookup;
                tracing::debug!("收到 Punch: conn_id={}, requester_fp={:?}, target_fp={}", conn_id, requester_fp.as_ref().map(hex::encode), hex::encode(target_fp));

                if let (Some(requester_fp), Some(requester_cc)) = (requester_fp, requester_cc.as_ref()) {
                    // 低危审计修复:隐身设备不暴露可连接性——名册剔除之外,
                    // Punch 也统一走"不可达"(在线与否的区分本身是信息泄露)
                    let target_hidden = self.leases.is_hidden(&target_fp);
                    let mut target_cc = target_cc;
                    if target_hidden { target_cc = None; }
                    if let Some(target_cc) = target_cc.as_ref() {
                        // S6:Punch 每 fp 滑窗速率限制(10 次/分钟)
                        if !self.punch_rate_ok(&requester_fp).await {
                            tracing::warn!("Punch 超速被限: fp={}", hex::encode(&requester_fp[..4]));
                            let resp = RelayMsg::PunchResp {
                                ok: false,
                                reason: Some("请求过于频繁,请稍后再试".into()),
                                session_addr: None,
                            };
                            self.push_guarded(&requester_fp, requester_cc, &resp).await;
                            drop(tx);
                            return Ok(());
                        }
                        tracing::debug!("Punch: 双方在线, 分配会话端口");
                        // 目标在线:分配会话端口并登记双方成员(S3)
                        if let Some(session_port) = self.leases.alloc_session_port(requester_fp, target_fp) {
                            tracing::debug!("Punch: 会话端口分配成功: {}", session_port);
                            let session_addr = SocketAddr::new(self.config.public_ip, session_port);

                            // 经目标 uni 流推 PunchNotif
                            let notif = RelayMsg::PunchNotif {
                                target_fp: requester_fp,
                                target_lease: session_addr,
                            };
                            self.push_guarded(&target_fp, target_cc, &notif).await;

                            // 经请求者 uni 流回 PunchResp{ok:true, session_addr}
                            let resp = RelayMsg::PunchResp {
                                ok: true,
                                reason: None,
                                session_addr: Some(session_addr.to_string()),
                            };
                            self.push_guarded(&requester_fp, requester_cc, &resp).await;
                            tracing::debug!("Punch: 已发送 PunchResp(ok=true) 到请求者");
                        } else {
                            // 会话端口耗尽
                            tracing::debug!("Punch: 会话端口耗尽");
                            let resp = RelayMsg::PunchResp {
                                ok: false,
                                reason: Some("会话端口耗尽".into()),
                                session_addr: None,
                            };
                            self.push_guarded(&requester_fp, requester_cc, &resp).await;
                        }
                    } else {
                        // 目标不在线(含隐身):统一回"不可达",不区分在线状态
                        tracing::debug!("Punch: 目标不可达");
                        let resp = RelayMsg::PunchResp {
                            ok: false,
                            reason: Some("对方当前不可达".into()),
                            session_addr: None,
                        };
                        self.push_guarded(&requester_fp, requester_cc, &resp).await;
                    }
                } else {
                    tracing::debug!("Punch: 请求者不在 conns 表中");
                }
                // 丢弃 bi 流的 tx(不写任何字节)
                drop(tx);
            }
            RelayMsg::Leave => {
                // remove 租约 + conns + broadcast_roster
                let conns = self.conns.lock().await;
                let fp = conns.iter().find(|(_, cc)| cc.conn_id == conn_id).map(|(fp, _)| *fp);
                if let Some(fp) = fp {
                    drop(conns);
                    self.leases.remove(&fp);
                    self.conns.lock().await.remove(&fp);
                    self.broadcast_roster().await;
                }
                // Leave 不回响应,丢弃 bi 流的 tx
                drop(tx);
            }
            _ => {
                return Err(format!("不支持的消息: {:?}", msg).into());
            }
        }

        Ok(())
    }

    async fn on_conn_closed(&self, conn_id: u64) {
        // 控制连接断开 = 明确离开:立即回收租约并广播名册(对端秒级看到下线)。
        // 网络抖动重连的场景,客户端重连后重新注册,租约即时恢复——
        // 不需要 45s 宽限(宽限只对"数据面还在用令牌"有意义,而令牌随
        // 重新注册换新,旧令牌本就作废)。
        let fp = {
            let mut conns = self.conns.lock().await;
            conns.iter()
                .find(|(_, cc)| cc.conn_id == conn_id)
                .map(|(fp, _)| *fp)
        };
        if let Some(fp) = fp {
            self.conns.lock().await.remove(&fp);
            self.leases.remove(&fp);
            tracing::info!("控制连接断开,回收租约: fp={}", hex::encode(fp));
            self.broadcast_roster().await;
        }
    }

    async fn broadcast_roster(&self) {
        // 递增版本号
        let rev = self.roster_rev.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // S4:锁内只取快照,推送在锁外并发做 3s 超时——慢读者不再阻塞
        // 名册表与整个服务端的事件处理;join_all 并发消除 N×3s 串行最坏
        let snapshot: Vec<([u8; 32], Arc<CtrlConn>)> = {
            let conns = self.conns.lock().await;
            conns.iter().map(|(fp, cc)| (*fp, Arc::clone(cc))).collect()
        };
        futures_util::future::join_all(snapshot.iter().map(|(fp, cc)| async move {
            let roster = RelayMsg::Roster {
                rev,
                devices: self.leases.snapshot_roster(fp, self.config.public_ip),
            };
            self.push_guarded(fp, cc, &roster).await;
        })).await;
    }

    async fn broadcast_roster_except(&self, exclude_fp: &[u8; 32]) {
        // 递增版本号
        let rev = self.roster_rev.fetch_add(1, std::sync::atomic::Ordering::SeqCst);

        // S4:同 broadcast_roster——快照 + 锁外并发超时推送
        let snapshot: Vec<([u8; 32], Arc<CtrlConn>)> = {
            let conns = self.conns.lock().await;
            conns.iter()
                .filter(|(fp, _)| *fp != exclude_fp)
                .map(|(fp, cc)| (*fp, Arc::clone(cc)))
                .collect()
        };
        futures_util::future::join_all(snapshot.iter().map(|(fp, cc)| async move {
            let roster = RelayMsg::Roster {
                rev,
                devices: self.leases.snapshot_roster(fp, self.config.public_ip),
            };
            self.push_guarded(fp, cc, &roster).await;
        })).await;
    }
}

/// 从 bi 流读一帧控制面消息(4B 长度前缀 + JSON body)。
/// 服务端半的帧长上限:与 encode_relay_msg 的 64KB 对称,读出 len 后
/// 先检查再分配,防 pre-auth 内存放大(伪造 len=0xFFFFFFFF 不再触发 ~4GB 分配)。
pub(crate) async fn read_relay_frame(rx: &mut RecvStream) -> Result<RelayMsg, String> {
    const MAX_FRAME: usize = 64 * 1024;
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf).await.map_err(|e| e.to_string())?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(format!("帧超长({} 字节, 上限 {})", len, MAX_FRAME));
    }
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await.map_err(|e| e.to_string())?;
    decode_relay_msg(&[&len_buf[..], &body[..]].concat())
}

impl CtrlConn {
    async fn push(&self, msg: &RelayMsg) -> Result<(), String> {
        // 每条推送消息独立一条 uni 流(消息=流,天然分帧):
        // 复用单条流连写会让客户端 accept_uni 的下一条消息永远等不到
        // (前一消息的后续字节不在新流上)。
        let mut tx = self.conn.open_uni().await.map_err(|e| e.to_string())?;
        tx.write_all(&encode_relay_msg(msg).unwrap()).await.map_err(|e| e.to_string())?;
        tx.finish().map_err(|e| e.to_string())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use super::*;
    use crate::config::RelayConfig;
    use crate::lease::LeaseTable;
    use localtrans_core::relay::proto::{decode_relay_msg, encode_relay_msg, RelayMsg};
    use quinn::Connection;

    /// 测试用客户端 endpoint(自签证书,不校验服务端)
    fn client_ep() -> quinn::Endpoint {
        let kp = rcgen::KeyPair::generate().unwrap();
        let params = rcgen::CertificateParams::new(vec!["test-client".into()]).unwrap();
        let cert = params.self_signed(&kp).unwrap();

        let mut client_cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"localtrans-relay".to_vec()];
        let quic_cfg = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_cfg).unwrap(),
        ));
        let mut ep = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
        ep.set_default_client_config(quic_cfg);
        ep
    }

    #[derive(Debug)]
    struct NoVerify;
    impl rustls::client::danger::ServerCertVerifier for NoVerify {
        fn verify_server_cert(
            &self,
            _end_entity: &rustls_pki_types::CertificateDer<'_>,
            _intermediates: &[rustls_pki_types::CertificateDer<'_>],
            _server_name: &rustls::pki_types::ServerName<'_>,
            _ocsp: &[u8],
            _now: rustls::pki_types::UnixTime,
        ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        }
        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &rustls_pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls12_signature(
                message, cert, dss, &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }
        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &rustls_pki_types::CertificateDer<'_>,
            dss: &rustls::DigitallySignedStruct,
        ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature(
                message, cert, dss, &rustls::crypto::ring::default_provider().signature_verification_algorithms,
            )
        }
        fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
            vec![
                rustls::SignatureScheme::ED25519,
                rustls::SignatureScheme::RSA_PSS_SHA256,
                rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
                rustls::SignatureScheme::RSA_PKCS1_SHA256,
            ]
        }
    }

    async fn send_msg(conn: &Connection, msg: &RelayMsg) -> Result<(), String> {
        let (mut tx, _rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        tx.write_all(&encode_relay_msg(msg).unwrap()).await.map_err(|e| e.to_string())?;
        tx.finish().map_err(|e| e.to_string())?;
        // 不等 stopped():服务端可能不复位 rx(不发 STOP_SENDING),
        // stopped() 会永久挂起。finish 后直接返回,数据已发出。
        Ok(())
    }

    /// 在指定双向上收一条消息(服务端会经 uni 流回话)
    async fn recv_msg(conn: &Connection) -> Result<RelayMsg, String> {
        // 服务端回话统一走 uni 流(客户端订阅式)
        let mut rx = conn.accept_uni().await.map_err(|e| e.to_string())?;
        super::read_relay_frame(&mut rx).await
    }

    async fn psk_hello(conn: &Connection, psk: &str) -> Result<(), String> {
        let mut proof = [0u8; 32];
        conn.export_keying_material(&mut proof, b"localtrans-relay-psk", psk.as_bytes())
            .map_err(|e| format!("export_keying_material error: {:?}", e))?;
        // PSK 证明走首条 bi 流的前 32 字节(与 Register 合并发送)
        let (mut tx, _rx) = conn.open_bi().await.map_err(|e| e.to_string())?;
        tx.write_all(&proof).await.map_err(|e| e.to_string())?;
        tx.finish().map_err(|e| e.to_string())?;
        // 同 send_msg:不等 stopped()
        Ok(())
    }

    /// 注册辅助(测试):PSK → 收 ServerNonce → 带签名注册
    async fn signed_register(
        conn: &Connection,
        dir: &std::path::Path,
        name: &str,
        hidden: bool,
    ) -> Result<([u8; 32], RelayMsg), String> {
        let id = localtrans_core::identity::Identity::load_or_create(dir).unwrap();
        // 先发 PSK 证明
        psk_hello(conn, "dev-psk").await?;
        // 收 ServerNonce
        let nonce = match recv_msg(conn).await? {
            RelayMsg::ServerNonce { nonce } => nonce,
            other => return Err(format!("期望 ServerNonce, 实得 {:?}", other)),
        };
        let fp = id.fingerprint();
        let mut signed = Vec::with_capacity(64);
        signed.extend_from_slice(&fp);
        signed.extend_from_slice(&nonce);
        send_msg(conn, &RelayMsg::Register {
            name: name.into(),
            fingerprint: fp,
            hidden,
            cert_der: id.cert.as_ref().to_vec(),
            nonce_sig: id.sign(&signed),
        }).await?;
        Ok((fp, recv_msg(conn).await?))
    }

    #[tokio::test]
    #[serial]
    async fn control_registers_and_allocates_lease() {
        let config = RelayConfig::for_test_with_port_offset(0);
        let server = RelayServer::bind(config.clone()).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        let ep = client_ep();
        let client_local = ep.local_addr().unwrap();
        let conn = ep.connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (_, ack) = signed_register(&conn, dir.path(), "家里", false).await.unwrap();
        match ack {
            RelayMsg::RegisterAck { lease: Some(l), observed_addr } => {
                assert!((9000..9100).contains(&l.data_port));
                assert_eq!(l.token.len(), 16);
                // M3a FR2:回报的公网出口 = 注册连接源地址(回环下即客户端 endpoint 地址)
                let observed: SocketAddr = observed_addr
                    .expect("RegisterAck 应携带 observed_addr")
                    .parse()
                    .expect("observed_addr 应为 ip:port");
                assert_eq!(observed, client_local);
            }
            other => panic!("期望 RegisterAck, 实得 {:?}", other),
        }
        server.shutdown().await;
    }

    #[tokio::test]
    #[serial]
    async fn control_rejects_wrong_psk() {
        let config = RelayConfig::for_test_with_port_offset(1);
        let server = RelayServer::bind(config).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        let conn = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        psk_hello(&conn, "WRONG").await.unwrap();

        // PSK 错误后服务端会立即关闭连接,应先收到 Error uni 流
        let resp = recv_msg(&conn).await.unwrap();
        assert!(matches!(resp, RelayMsg::Error { .. }), "应拒错误 PSK, 实得 {:?}", resp);

        // 连接应已关闭,后续操作应失败
        // 验证连接状态:尝试接受新的 uni 流应该失败
        let uni_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            conn.accept_uni()
        ).await;
        assert!(uni_result.is_err() || uni_result.is_ok_and(|r| r.is_err()), "accept_uni 应失败,因为连接已关闭");

        server.shutdown().await;
    }

    #[tokio::test]
    #[serial]
    async fn roster_pushes_on_second_registration() {
        let config = RelayConfig::for_test_with_port_offset(2);
        let server = RelayServer::bind(config).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        // A 注册
        let dir_a = tempfile::tempdir().unwrap();
        let conn_a = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_a, _) = signed_register(&conn_a, dir_a.path(), "A", false).await.unwrap();

        // B 注册 → A 应收到 Roster(含 B)
        let dir_b = tempfile::tempdir().unwrap();
        let conn_b = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_b, _) = signed_register(&conn_b, dir_b.path(), "B", false).await.unwrap();

        let roster = recv_msg(&conn_a).await.unwrap();
        match roster {
            RelayMsg::Roster { devices, .. } => {
                assert!(devices.iter().any(|d| d.fingerprint == fp_b));
            }
            other => panic!("期望 Roster, 实得 {:?}", other),
        }
        server.shutdown().await;
    }

    #[tokio::test]
    #[serial]
    async fn punch_allocates_session_addr_and_notifies_target() {
        let config = RelayConfig::for_test_with_port_offset(3);
        let server = RelayServer::bind(config.clone()).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        // A/B 注册
        let dir_a = tempfile::tempdir().unwrap();
        let conn_a = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_a, _) = signed_register(&conn_a, dir_a.path(), "A", false).await.unwrap();

        let dir_b = tempfile::tempdir().unwrap();
        let conn_b = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_b, _) = signed_register(&conn_b, dir_b.path(), "B", false).await.unwrap();
        let _ = recv_msg(&conn_a).await.unwrap(); // Roster (A 收到 B 的注册推送)

        // A 发 Punch{B}
        send_msg(&conn_a, &RelayMsg::Punch { target_fp: fp_b }).await.unwrap();

        // A 应收到 PunchResp{ok:true, session_addr 有值}
        let resp_a = recv_msg(&conn_a).await.unwrap();
        let session_addr = match resp_a {
            RelayMsg::PunchResp { ok: true, reason: None, session_addr: Some(addr_str) } => {
                let session_addr: SocketAddr = addr_str.parse().unwrap();
                // 端口应在池内
                assert!(config.data_port_start <= session_addr.port() && session_addr.port() < config.data_port_end);
                assert_eq!(session_addr.ip(), config.public_ip);
                session_addr
            }
            other => panic!("期望 PunchResp{{ok:true}}, 实得 {:?}", other),
        };

        // B 应收到 PunchNotif{target_fp: A, target_lease: 同地址}
        let notif_b = recv_msg(&conn_b).await.unwrap();
        match notif_b {
            RelayMsg::PunchNotif { target_fp, target_lease } => {
                assert_eq!(target_fp, fp_a); // A 的指纹
                assert_eq!(target_lease, session_addr); // 地址应相同
            }
            other => panic!("期望 PunchNotif, 实得 {:?}", other),
        }

        server.shutdown().await;
    }

    #[tokio::test]
    #[serial]
    async fn psk_failure_leads_to_block() {
        let config = RelayConfig::for_test_with_port_offset(4);
        let server = RelayServer::bind(config).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        // 同一 IP 连续 5 次用错 PSK
        for i in 0..5 {
            let conn = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
            psk_hello(&conn, "WRONG").await.unwrap();
            // 每次应收到 Error
            let resp = recv_msg(&conn).await.unwrap();
            assert!(matches!(resp, RelayMsg::Error { .. }), "第 {} 次应拒错误 PSK", i + 1);
            // 连接随后被关闭
        }

        // 第 6 次连接应直接被拒绝
        // IP 已被拉黑,服务器在 handle_conn 开头就关闭连接,不会发送 Error 回包
        let conn = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let psk_result = psk_hello(&conn, "WRONG").await;

        // 客户端成功发送 PSK,但服务器会立即关闭连接
        // 尝试接收 Error 应该超时或失败
        let recv_result = tokio::time::timeout(
            tokio::time::Duration::from_millis(100),
            recv_msg(&conn)
        ).await;
        assert!(recv_result.is_err() || recv_result.is_ok_and(|r| r.is_err()), "第 6 次不应收到 Error 回包");

        server.shutdown().await;
    }

    /// S7:连上后一直不发 PSK 证明——服务端应在 ~10s 认证超时断连,
    /// 不给半开连接无限占位的机会。
    /// 慢测试:走真实超时路径,断言 9-15s 断连窗口,约 10s 完成。
    #[tokio::test]
    #[serial]
    async fn silent_client_times_out_before_proof() {
        let config = RelayConfig::for_test_with_port_offset(6);
        let server = RelayServer::bind(config).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        let conn = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();

        let start = std::time::Instant::now();
        // 服务端 10s 读证明超时 → 关闭连接 → conn.closed() 应就绪
        tokio::time::timeout(Duration::from_secs(15), conn.closed())
            .await
            .expect("静默客户端应在认证超时(~10s)内被断连");
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_secs(9) && elapsed <= Duration::from_secs(15),
            "断连时刻应在 10s 附近, 实际 {:?}", elapsed
        );

        server.shutdown().await;
    }

    /// S4:慢读者(注册后完全不收 uni 推送)在服务端持续推送压力下,
    /// 应在 3s 推送超时后被断连清表,且其他设备的正常收发不受影响。
    #[tokio::test]
    #[serial]
    async fn slow_reader_is_cut_and_peers_unaffected() {
        let config = RelayConfig::for_test_with_port_offset(7);
        let server = RelayServer::bind(config.clone()).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        // A 正常注册并持续消费推送
        let dir_a = tempfile::tempdir().unwrap();
        let conn_a = Arc::new(client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap());
        signed_register(&conn_a, dir_a.path(), "A", false).await.unwrap();
        let reader_conn_a = conn_a.clone();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel::<()>();
        let reader = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => break,
                    r = recv_msg(&reader_conn_a) => { if r.is_err() { break; } }
                }
            }
        });


        // B 注册后不再读任何推送(慢读者)
        let dir_b = tempfile::tempdir().unwrap();
        let ep_b = client_ep();
        let conn_b = ep_b.connect(addr, "localtrans-relay").unwrap().await.unwrap();
        signed_register(&conn_b, dir_b.path(), "B", false).await.unwrap();
        // 之后对 B 的所有推送都进不了它的接收窗口

        // 对离线目标并发打 130 发 Punch:每发都触发一次服务端回推,
        // 约 100 流(QUIC 默认 uni 上限)后 open_uni 开始阻塞——
        // 无防护时服务端会永远卡死在这里;有防护时 3s 超时踢掉 B。
        for _ in 0..130 {
            send_msg(&conn_b, &RelayMsg::Punch { target_fp: [0xEE; 32] }).await.unwrap();
        }

        // B 应被踢出连接表(A/C 正常)
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let mut n = usize::MAX;
        while std::time::Instant::now() < deadline {
            let table = server.conns.lock().await;
            n = table.len();
            drop(table);
            if n == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert_eq!(n, 1, "慢读者应被断连清表, 仅剩 A");

        // A 仍能正常收到推送(reader 任务没死):发 Punch 触发一次回推验证
        // (B 已不在表中,Punch 目标不在线也会回 PunchResp)
        let _ = stop_tx.send(());
        reader.await.unwrap();
        send_msg(&conn_a, &RelayMsg::Ping).await.unwrap();

        let resp = tokio::time::timeout(Duration::from_secs(5), recv_msg(&conn_a))
            .await
            .expect("A 应仍能收到服务端推送")
            .unwrap();
        assert!(matches!(resp, RelayMsg::Roster { .. }), "期望 Roster, 实得 {:?}", resp);

        server.shutdown().await;
    }

    #[tokio::test]
    #[serial]
    async fn hidden_client_excluded_from_roster() {
        let config = RelayConfig::for_test_with_port_offset(5);
        let server = RelayServer::bind(config).await.unwrap();
        let port = server.local_control_addr().port();
        let addr = format!("127.0.0.1:{}", port).parse().unwrap();
        tokio::spawn(server.clone().run());

        // A 正常注册
        let dir_a = tempfile::tempdir().unwrap();
        let conn_a = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_a, _) = signed_register(&conn_a, dir_a.path(), "A", false).await.unwrap();

        // B 隐身注册
        let dir_b = tempfile::tempdir().unwrap();
        let conn_b = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
        let (fp_b, _) = signed_register(&conn_b, dir_b.path(), "B", true).await.unwrap();

        // B 注册触发的广播:A 收到 Roster,不含 B(隐身被剔除)
        match recv_msg(&conn_a).await.unwrap() {
            RelayMsg::Roster { devices, .. } => {
                assert!(!devices.iter().any(|d| d.fingerprint == fp_b), "名册不应包含隐身设备");
            }
            other => panic!("期望 Roster, 实得 {:?}", other),
        }

        // B 发 Ping 拉名册:能看到 A(隐身者自己仍可见名册——半隐身语义)
        send_msg(&conn_b, &RelayMsg::Ping).await.unwrap();
        match recv_msg(&conn_b).await.unwrap() {
            RelayMsg::Roster { devices, .. } => {
                assert!(devices.iter().any(|d| d.fingerprint == fp_a), "隐身设备应能收到名册");
            }
            other => panic!("期望 Roster, 实得 {:?}", other),
        }

        server.shutdown().await;
    }
}
