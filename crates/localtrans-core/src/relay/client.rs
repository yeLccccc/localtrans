//! RelayClient:控制连接管理(注册/名册/Punch)+ 会话虚拟端点工厂。

use crate::identity::Identity;
use crate::relay::proto::{decode_relay_msg, encode_relay_msg, RelayMsg};
use quinn::Connection;
use quinn::Endpoint;
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::DigitallySignedStruct;
use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch, Mutex, RwLock};

/// RelayClient 配置
#[derive(Clone)]
pub struct RelayClientConfig {
    pub server_addr: SocketAddr,
    pub psk: String,
    /// Register 上报的显示名(空串时兜底 "LocalTrans")
    pub device_name: String,
    /// 隐身:注册上报,服务端名册剔除本设备(半隐身:可看名册可被 punch,不可被发现)
    pub hidden: bool,
}

/// 客户端状态
#[derive(Clone, Debug, PartialEq)]
pub enum RelayClientStatus {
    Connecting,
    Registered,
    Reconnecting,
}

/// 客户端事件
pub enum RelayEvent {
    RosterUpdated(Vec<crate::relay::proto::RemoteDevice>),
    StatusChanged(RelayClientStatus),
    PunchIncoming { from_fp: [u8; 32], session_addr: SocketAddr },
}

/// RelayClient:中继控制连接 + 虚拟端点工厂
pub struct RelayClient {
    config: RelayClientConfig,
    identity: Arc<Identity>,
    conn: Arc<Mutex<Option<Connection>>>,
    events: mpsc::Sender<RelayEvent>,
    status: watch::Sender<RelayClientStatus>,
    /// 名册快照
    roster: Arc<RwLock<Vec<crate::relay::proto::RemoteDevice>>>,
    /// 会话表:对端fp → 虚拟端点(已 connect 或已 listen)
    sessions: Arc<Mutex<HashMap<[u8; 32], Endpoint>>>,
    /// 本机令牌(数据面身份)
    token: Arc<RwLock<Option<[u8; 16]>>>,
    /// M3a FR2:服务端在 RegisterAck 回报的本机公网出口(注册连接源 ip:port)。
    /// 仅注册成功期间有值;断线清空,重连重注册后刷新(NAT 映射可能已变)。
    observed_addr: Arc<RwLock<Option<String>>>,
    /// M3b T2:中继数据面地址集——本机租约地址(RegisterAck.lease: server_ip:
    /// data_port)+ Punch 往返见过的对端租约地址(PunchResp.session_addr /
    /// PunchNotif.target_lease)。用于判定一条 QUIC 连接的 remote_address 是否
    /// "经中继"(通道记录 via_relay 标记的数据依据;租约地址=relay_ip:数据端口,
    /// 与直连地址天然不同空间,比对是确定性的,不靠网段猜)。
    data_addrs: Arc<std::sync::Mutex<HashSet<SocketAddr>>>,
    shutdown: Arc<AtomicBool>,
    /// Punch 等待表:target_fp → oneshot 发送端
    pending_punches: Arc<Mutex<HashMap<[u8; 32], oneshot::Sender<SocketAddr>>>>,
}

impl RelayClient {
    /// 连接+注册;成功返回客户端实例 + 事件接收端
    pub async fn connect(
        config: RelayClientConfig,
        identity: Arc<Identity>,
    ) -> Result<(Arc<Self>, mpsc::Receiver<RelayEvent>), String> {
        let (events_tx, events_rx) = mpsc::channel(32);
        let (status_tx, _) = watch::channel(RelayClientStatus::Connecting);
        let token = Arc::new(RwLock::new(None));
        let pending_punches = Arc::new(Mutex::new(HashMap::new()));

        let client = Arc::new(RelayClient {
            config: config.clone(),
            identity: identity.clone(),
            conn: Arc::new(Mutex::new(None)),
            events: events_tx,
            status: status_tx,
            roster: Arc::new(RwLock::new(Vec::new())),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            token,
            observed_addr: Arc::new(RwLock::new(None)),
            data_addrs: Arc::new(std::sync::Mutex::new(HashSet::new())),
            shutdown: Arc::new(AtomicBool::new(false)),
            pending_punches,
        });

        // 重连循环
        let mut backoff_secs = 1u64;
        loop {
            match Self::do_connect(&client, &config, &identity).await {
                Ok(conn) => {
                    *client.conn.lock().await = Some(conn.clone());
                    client.status.send_replace(RelayClientStatus::Registered);

                    // 启动后台任务
                    let client_clone = client.clone();
                    tokio::spawn(async move {
                        client_clone.run_background().await;
                    });

                    return Ok((client, events_rx));
                }
                Err(e) => {
                    client.status.send_replace(RelayClientStatus::Reconnecting);
                    tracing::warn!("中继连接失败({}), {}s 后重连", e, backoff_secs);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(30);
                }
            }
        }
    }

    /// 执行实际连接:QUIC 连接 + PSK 证明 + Register
    async fn do_connect(
        client: &RelayClient,
        config: &RelayClientConfig,
        identity: &Arc<Identity>,
    ) -> Result<Connection, String> {
        // 1. 建客户端 QUIC endpoint(自签+NoVerify 服务端证书)
        let ep = Self::client_endpoint()?;
        let conn = ep
            .connect(config.server_addr, "localtrans-relay")
            .map_err(|e| format!("连接失败: {}", e))?
            .await
            .map_err(|e| format!("握手失败: {}", e))?;

        // 2. 首条 bi 流发 32B PSK exporter 证明
        let (mut tx_proof, _rx_proof) = conn
            .open_bi()
            .await
            .map_err(|e| format!("打开 PSK 流失败: {}", e))?;
        let mut proof = [0u8; 32];
        conn.export_keying_material(&mut proof, b"localtrans-relay-psk", config.psk.as_bytes())
            .map_err(|e| format!("export_keying_material: {:?}", e))?;
        tx_proof
            .write_all(&proof)
            .await
            .map_err(|e| format!("写 PSK 证明失败: {}", e))?;
        tx_proof.finish().map_err(|e| format!("完成 PSK 流失败: {}", e))?;

        // 3. 先收服务端 uni 推的 ServerNonce(S2 占有证明挑战)
        let nonce = tokio::time::timeout(Duration::from_secs(10), async {
            let mut uni_rx = conn
                .accept_uni()
                .await
                .map_err(|e| format!("接收 uni 流失败: {}", e))?;
            read_uni_msg(&mut uni_rx).await
        })
        .await
        .map_err(|_| "等 ServerNonce 超时".to_string())??;
        let nonce = match nonce {
            RelayMsg::ServerNonce { nonce } => nonce,
            other => return Err(format!("期望 ServerNonce, 实得 {:?}", other)),
        };

        // 4. 发 Register{name, fingerprint, cert_der, sign(fp||nonce)}——名字来自配置(真实设备名),
        //    此前硬编码 "LocalTrans" 导致名册里所有设备同名
        let fp = identity.fingerprint();
        let mut signed = Vec::with_capacity(64);
        signed.extend_from_slice(&fp);
        signed.extend_from_slice(&nonce);
        let (mut tx, _rx) = conn
            .open_bi()
            .await
            .map_err(|e| format!("打开 Register 流失败: {}", e))?;
        let reg = RelayMsg::Register {
            name: register_display_name(&config.device_name),
            fingerprint: fp,
            hidden: config.hidden,
            cert_der: identity.cert.as_ref().to_vec(),
            nonce_sig: identity.sign(&signed),
        };
        let msg_bytes = encode_relay_msg(&reg).map_err(|e| format!("编码 Register: {}", e))?;
        tx.write_all(&msg_bytes)
            .await
            .map_err(|e| format!("写 Register 失败: {}", e))?;
        tx.finish().map_err(|e| format!("完成 Register 流失败: {}", e))?;

        // 5. 等 uni 流的 RegisterAck{lease:Some} → 记 token(+ 公网出口 observed_addr)
        let mut uni_rx = conn
            .accept_uni()
            .await
            .map_err(|e| format!("接收 uni 流失败: {}", e))?;
        let ack = read_uni_msg(&mut uni_rx).await.map_err(|e| format!("解码 RegisterAck: {}", e))?;

        match ack {
            RelayMsg::RegisterAck { lease: Some(lease), observed_addr } => {
                *client.token.write().await = Some(lease.token);
                // M3b T2:记本机租约数据面地址(server_ip:lease.data_port),供
                // is_relay_data_addr 判定(远端内层连接 remote_address 形态相同)
                {
                    let mut da = client.data_addrs.lock().expect("data_addrs 锁中毒");
                    da.insert(SocketAddr::new(config.server_addr.ip(), lease.data_port));
                }
                // M3a FR2:旧服务端不带字段 → None(保持"未知"而非沿用旧值)
                if let Some(a) = observed_addr.as_deref() {
                    tracing::info!("中继回报本机公网出口: {}", a);
                }
                *client.observed_addr.write().await = observed_addr;
                Ok(conn)
            }
            RelayMsg::RegisterAck { lease: None, .. } => Err("端口耗尽".into()),
            other => Err(format!("期望 RegisterAck, 实得 {:?}", other)),
        }
    }

    /// 后台任务:15s Ping + uni 推送分发;断线后自动重连(指数退避)。
    /// 重连成功重走完整注册流(新令牌),名册由服务器推送重建。
    async fn run_background(self: Arc<Self>) {
        let mut ping_interval = tokio::time::interval(Duration::from_secs(15));
        loop {
            tokio::select! {
                _ = ping_interval.tick() => {
                    // 发 Ping
                    let conn = { self.conn.lock().await.clone() };
                    if let Some(conn) = conn {
                        if let Err(e) = self.send_msg(&conn, &RelayMsg::Ping).await {
                            tracing::warn!("Ping 失败: {}", e);
                            self.on_disconnect().await;
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // uni 流推送分发
                uni_result = self.recv_uni() => {
                    match uni_result {
                        Ok(msg) => {
                            if let Err(e) = self.handle_push(msg).await {
                                tracing::warn!("处理推送失败: {}", e);
                            }
                        }
                        Err(_) => {
                            self.on_disconnect().await;
                            break;
                        }
                    }
                }
            }
        }

        // 走到这里说明断线(Ping 失败/连接关闭/无连接):重连循环
        self.on_disconnect().await;
        let mut backoff_secs = 1u64;
        loop {
            if self.shutdown.load(Ordering::SeqCst) {
                return;
            }
            match Self::do_connect(&self, &self.config, &self.identity).await {
                Ok(conn) => {
                    *self.conn.lock().await = Some(conn.clone());
                    self.status.send_replace(RelayClientStatus::Registered);
                    let _ = self.events.send(RelayEvent::StatusChanged(RelayClientStatus::Registered)).await;
                    tracing::info!("中继重连成功");
                    break; // 回到外层主循环(Ping/分发)
                }
                Err(e) => {
                    tracing::warn!("中继重连失败({}), {}s 后重试", e, backoff_secs);
                    tokio::time::sleep(Duration::from_secs(backoff_secs)).await;
                    backoff_secs = (backoff_secs * 2).min(30);
                }
            }
        }
        // 重连后重置 Ping 节拍
        ping_interval = tokio::time::interval(Duration::from_secs(15));
        ping_interval.tick().await; // 跳过立即 tick
    }

    /// 处理 uni 推送
    async fn handle_push(&self, msg: RelayMsg) -> Result<(), String> {
        match msg {
            RelayMsg::Roster { devices, .. } => {
                *self.roster.write().await = devices.clone();
                let _ = self
                    .events
                    .send(RelayEvent::RosterUpdated(devices))
                    .await;
            }
            RelayMsg::PunchNotif {
                target_fp,
                target_lease,
            } => {
                // M3b T2:对端租约地址入数据面地址集(via_relay 判定依据)
                if let Ok(mut da) = self.data_addrs.lock() {
                    da.insert(target_lease);
                }
                let _ = self
                    .events
                    .send(RelayEvent::PunchIncoming {
                        from_fp: target_fp,
                        session_addr: target_lease,
                    })
                    .await;
            }
            RelayMsg::PunchResp {
                ok,
                session_addr,
                ..
            } => {
                tracing::debug!("收到 PunchResp: ok={}, session_addr={:?}", ok, session_addr);
                if ok {
                    if let Some(addr_str) = session_addr {
                        if let Ok(addr) = addr_str.parse::<SocketAddr>() {
                            // M3b T2:对端租约地址入数据面地址集(via_relay 判定依据)
                            if let Ok(mut da) = self.data_addrs.lock() {
                                da.insert(addr);
                            }
                            // resolve punch 等待
                            // 这里的 key 应该是上次 punch 的目标,但我们不知道是哪个
                            // 简化:遍历查找第一个等待者(实际应用应该用 target_fp 索引)
                            let pending = self.pending_punches.lock().await.drain().next();
                            if let Some((_, tx)) = pending {
                                tracing::debug!("PunchResp 发送等待者, 地址={}", addr);
                                let _ = tx.send(addr);
                            } else {
                                tracing::warn!("PunchResp 到达但无等待者");
                            }
                        }
                    }
                } else {
                    tracing::warn!("Punch 失败");
                }
            }
            RelayMsg::Error { code, msg } => {
                tracing::error!("服务器错误: code={}, msg={}", code, msg);
            }
            _ => {}
        }
        Ok(())
    }

    /// 断线处理
    async fn on_disconnect(&self) {
        self.status.send_replace(RelayClientStatus::Reconnecting);
        *self.conn.lock().await = None;
        // 公网出口随注册失效(重注册后由新 RegisterAck 刷新)
        *self.observed_addr.write().await = None;
        // 清理 punch 等待
        self.pending_punches.lock().await.clear();
    }

    /// 订阅状态
    pub fn subscribe(&self) -> watch::Receiver<RelayClientStatus> {
        self.status.subscribe()
    }

    /// 获取名册快照
    pub async fn roster_snapshot(&self) -> Vec<crate::relay::proto::RemoteDevice> {
        self.roster.read().await.clone()
    }

    /// M3a FR2:服务端观察到的本机公网出口 `ip:port`(RegisterAck.observed_addr)。
    /// 仅当前处于已注册状态且服务端支持该字段时为 Some;断线/旧服务端为 None。
    pub async fn observed_addr(&self) -> Option<String> {
        self.observed_addr.read().await.clone()
    }

    /// M3b T2:判定地址是否为本中继的数据面租约地址(本机租约或 Punch 往返
    /// 见过的对端租约)。仅在中继已连接时有意义——断线后一律 false(此时
    /// 不会有活跃的中继通道)。通道记录 via_relay 标记的数据依据,比网段
    /// 推断可靠:租约地址 = relay_ip:数据端口,与直连地址天然不同空间。
    pub async fn is_relay_data_addr(&self, addr: SocketAddr) -> bool {
        if self.conn.lock().await.is_none() {
            return false;
        }
        match self.data_addrs.lock() {
            Ok(da) => da.contains(&addr),
            Err(_) => false,
        }
    }

    /// Punch 对方:发送 Punch{target_fp} → 等 PunchResp{session_addr}
    pub async fn punch(&self, target_fp: [u8; 32]) -> Result<SocketAddr, String> {
        let conn = {
            let c = self.conn.lock().await;
            c.clone().ok_or("未连接")?
        };

        // 创建 oneshot 等待
        let (tx, rx) = oneshot::channel();
        self.pending_punches.lock().await.insert(target_fp, tx);

        // 发 Punch
        self.send_msg(&conn, &RelayMsg::Punch { target_fp })
            .await?;

        // 等响应
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .map_err(|_| "Punch 超时".to_string())?
            .map_err(|_| "Punch 等待被取消".to_string())
    }

    /// 建立到对端的内层 QUIC 连接(发起方)
    pub async fn connect_peer(
        &self,
        target_fp: [u8; 32],
    ) -> Result<Connection, String> {
        tracing::debug!("[connect_peer] 开始, target_fp={}", hex::encode(target_fp));
        // punch 拿 session_addr
        let session_addr = self.punch(target_fp).await?;
        tracing::debug!("[connect_peer] punch 完成, session_addr={}", session_addr);

        // 获取 token
        let token = {
            let t = self.token.read().await;
            t.ok_or("未注册")?
        };

        // VirtualUdp::bind(先建 socket)
        tracing::debug!("[connect_peer] 创建 VirtualUdp");
        let vudp = super::virtual_ep::VirtualUdp::bind(token, session_addr)
            .await
            .map_err(|e| format!("VirtualUdp::bind: {}", e))?;
        tracing::debug!("[connect_peer] VirtualUdp 创建成功");

        // KNOCK 必须从 VirtualUdp 自己的 socket 发(中继学习本端真实地址)
        tracing::debug!("[connect_peer] 发送 KNOCK");
        vudp.send_knock().await.map_err(|e| format!("KNOCK: {}", e))?;
        tracing::debug!("[connect_peer] KNOCK 发送完成");

        // 客户端端点（带预期指纹钉扎）
        tracing::debug!("[connect_peer] 创建客户端端点，预期指纹={}", hex::encode(target_fp));
        let ep = super::virtual_ep::client_endpoint(vudp, &self.identity, Some(target_fp)).await?;
        tracing::debug!("[connect_peer] 客户端端点创建成功");

        // 内层 QUIC 连接(目标必须是 session_addr)
        tracing::debug!("[connect_peer] 开始 QUIC 握手到 {}", session_addr);
        let conn = ep
            .connect(session_addr, "localhost")
            .map_err(|e| format!("内层连接失败: {}", e))?
            .await
            .map_err(|e| format!("内层握手失败: {}", e))?;
        tracing::debug!("[connect_peer] QUIC 握手完成");

        Ok(conn)
    }

    /// 响应方:接受对端连接(PunchNotif 到达后调用)
    pub async fn accept_peer(&self, session_addr: SocketAddr, from_fp: [u8; 32]) -> Result<Connection, String> {
        tracing::debug!("[accept_peer] 开始, session_addr={}, from_fp={}", session_addr, hex::encode(from_fp));
        // 获取 token
        let token = {
            let t = self.token.read().await;
            t.ok_or("未注册")?
        };

        // VirtualUdp::bind(先建 socket)
        tracing::debug!("[accept_peer] 创建 VirtualUdp");
        let vudp = super::virtual_ep::VirtualUdp::bind(token, session_addr)
            .await
            .map_err(|e| format!("VirtualUdp::bind: {}", e))?;
        tracing::debug!("[accept_peer] VirtualUdp 创建成功");

        // KNOCK 必须从 VirtualUdp 自己的 socket 发(中继学习本端真实地址)
        tracing::debug!("[accept_peer] 发送 KNOCK");
        vudp.send_knock().await.map_err(|e| format!("KNOCK: {}", e))?;
        tracing::debug!("[accept_peer] KNOCK 发送完成");

        // 服务端端点(S1: 带客户端证书钉扎——from_fp 在 accept 前已知,
        // 指纹不符的握手在 TLS 层直接失败,不再有"握手后才比对"的窗口)
        tracing::debug!("[accept_peer] 创建钉扎服务端端点, 预期指纹={}", hex::encode(from_fp));
        let ep = super::virtual_ep::server_endpoint_pinned(vudp, &self.identity, from_fp).await?;
        tracing::debug!("[accept_peer] 服务端端点创建成功");

        // accept()
        tracing::debug!("[accept_peer] 等待 incoming 连接");
        let incoming = tokio::time::timeout(
            Duration::from_secs(10),
            ep.accept(),
        )
        .await
        .map_err(|_| "accept 超时".to_string())?
        .ok_or("endpoint 已关闭".to_string())?;

        tracing::debug!("[accept_peer] incoming 连接到达,等待握手完成");
        let conn = incoming.await.map_err(|e| format!("接受连接失败: {}", e))?;
        tracing::debug!("[accept_peer] 连接建立成功");

        // S1 防御性断言:钉扎验证器已在握手期拒绝不符指纹,这里到不了
        // 不符的连接。保留 debug 断言便于日志侧确认钉扎生效。
        use crate::identity::fingerprint_of;
        let peer_certs = conn.peer_identity()
            .and_then(|any| any.downcast::<Vec<rustls::pki_types::CertificateDer<'static>>>().ok());
        if let Some(cert) = peer_certs.as_ref().and_then(|c| c.first()) {
            let actual_fp = fingerprint_of(cert);
            debug_assert_eq!(actual_fp, from_fp, "钉扎验证器已放行,指纹必须一致");
            tracing::debug!("[accept_peer] 指纹钉扎确认: {}", hex::encode(actual_fp));
        }

        Ok(conn)
    }

    /// 发 KNOCK 包
    async fn send_knock(&self, session_addr: SocketAddr, token: [u8; 16]) -> Result<(), String> {
        use crate::relay::proto::{data_header_encode, FLAG_KNOCK};

        let bind_addr: SocketAddr = if session_addr.is_ipv4() {
            "0.0.0.0:0".parse().unwrap()
        } else {
            "[::]:0".parse().unwrap()
        };
        let sock = tokio::net::UdpSocket::bind(bind_addr)
            .await
            .map_err(|e| format!("绑定 KNOCK socket 失败: {}", e))?;

        let mut pkt = Vec::new();
        data_header_encode(&mut pkt, &token, FLAG_KNOCK);
        sock.send_to(&pkt, session_addr)
            .await
            .map_err(|e| format!("发送 KNOCK 失败: {}", e))?;

        Ok(())
    }

    /// 发消息(经 bi 流)
    async fn send_msg(&self, conn: &Connection, msg: &RelayMsg) -> Result<(), String> {
        let (mut tx, _rx) = conn
            .open_bi()
            .await
            .map_err(|e| format!("打开流失败: {}", e))?;
        let bytes = encode_relay_msg(msg).map_err(|e| format!("编码失败: {}", e))?;
        tx.write_all(&bytes)
            .await
            .map_err(|e| format!("写失败: {}", e))?;
        tx.finish().map_err(|e| format!("完成流失败: {}", e))?;
        Ok(())
    }

    /// 接收 uni 流
    async fn recv_uni(&self) -> Result<RelayMsg, String> {
        let conn = {
            let c = self.conn.lock().await;
            c.clone().ok_or("未连接")?
        };

        let mut uni_rx = conn
            .accept_uni()
            .await
            .map_err(|e| format!("accept_uni 失败: {}", e))?;
        let mut len_buf = [0u8; 4];
        uni_rx
            .read_exact(&mut len_buf)
            .await
            .map_err(|e| format!("读长度失败: {}", e))?;
        let len = u32::from_be_bytes(len_buf) as usize;
        // S5 客户端帧上限:与服务端 read_relay_frame(64KB)对称,超限断连
        const MAX_FRAME: usize = 64 * 1024;
        if len > MAX_FRAME {
            return Err(format!("帧超长({} 字节, 上限 {})", len, MAX_FRAME));
        }
        let mut body = vec![0u8; len];
        uni_rx
            .read_exact(&mut body)
            .await
            .map_err(|e| format!("读消息体失败: {}", e))?;
        decode_relay_msg(&[&len_buf[..], &body[..]].concat())
            .map_err(|e| format!("解码失败: {}", e))
    }

    /// 关闭
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);
        // 发 Leave(尽力)
        if let Some(conn) = self.conn.lock().await.as_ref() {
            let _ = self.send_msg(conn, &RelayMsg::Leave).await;
        }
        // 给服务器处理 Leave 的小窗口再关连接(服务器 select! 里
        // accept_bi 与 conn.closed() 竞争——立即 close 可能令 Leave
        // 的流未被 accept 就走了 closed 分支;不过 on_conn_closed
        // 也会回收租约,这里只是双保险)
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        if let Some(conn) = self.conn.lock().await.as_ref() {
            conn.close(0u8.into(), b"shutdown");
        }
    }

    /// 创建客户端 endpoint(NoVerify 服务端证书)
    fn client_endpoint() -> Result<Endpoint, String> {
        let kp = rcgen::KeyPair::generate().map_err(|e| e.to_string())?;
        let params = rcgen::CertificateParams::new(vec!["relay-client".into()])
            .map_err(|e| e.to_string())?;
        let _cert = params.self_signed(&kp).map_err(|e| e.to_string())?;

        let mut client_cfg = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify))
            .with_no_client_auth();
        client_cfg.alpn_protocols = vec![b"localtrans-relay".to_vec()];
        let mut quic_cfg = quinn::ClientConfig::new(Arc::new(
            quinn::crypto::rustls::QuicClientConfig::try_from(client_cfg).map_err(|e| e.to_string())?,
        ));
        // v0.9.1: 控制面同样限 1200——此前只有数据面(virtual_ep)限了,
        // 控制面按 quinn 默认 1472 发包,跨网路径(WSL Hyper-V 过滤器/PPPoE)
        // 上持续 EMSGSIZE(code 10040,len 1389/1404),控制连接反复断连重连
        // (名册反复重推=设备列表"闪现/隐身"的诱因之一)。实测日志证实。
        // v0.11.x 补丁:max_udp_payload_size 只限接收侧,发送侧 MTU 由
        // transport_config 的探测决定——quinn 0.11 默认探测会涨回 1452+,
        // 仍撞 WSAEMSGSIZE(2026-08-27 实测日志 len 1389/1404 持续复发)。
        // 显式关探测+固定 1200(与 relay_transport_config 同策略)。
        let mut tc = quinn::TransportConfig::default();
        tc.mtu_discovery_config(None);
        tc.initial_mtu(1200);
        tc.min_mtu(1200);
        tc.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
        quic_cfg.transport_config(Arc::new(tc));
        let mut ep_cfg = quinn::EndpointConfig::default();
        ep_cfg.max_udp_payload_size(1200).map_err(|e| format!("max_udp_payload_size: {}", e))?;
        let socket = std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
        let mut ep = quinn::Endpoint::new(
            ep_cfg,
            None,
            socket,
            std::sync::Arc::new(quinn::TokioRuntime),
        ).map_err(|e| e.to_string())?;
        ep.set_default_client_config(quic_cfg);
        Ok(ep)
    }
}

#[derive(Debug)]
struct NoVerify;

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
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

/// Register 显示名:空名兜底旧默认值
fn register_display_name(name: &str) -> String {
    if name.trim().is_empty() { "LocalTrans".into() } else { name.trim().to_string() }
}

/// 读一条 uni 流上的 RelayMsg(4B 长度前缀 + JSON)
async fn read_uni_msg(rx: &mut quinn::RecvStream) -> Result<RelayMsg, String> {
    // S5 帧上限:与 encode_relay_msg 的 64KB 对称。伪造/异常的超长 len
    // 在分配前直接拒绝(报错即触发上层断连重连),不做 ~4GB 内存放大。
    const MAX_FRAME: usize = 64 * 1024;
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf)
        .await
        .map_err(|e| format!("读长度失败: {}", e))?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_FRAME {
        return Err(format!("帧超长({} 字节, 上限 {})", len, MAX_FRAME));
    }
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body)
        .await
        .map_err(|e| format!("读消息体失败: {}", e))?;
    decode_relay_msg(&[&len_buf[..], &body[..]].concat())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_and_status_constructible() {
        let config = RelayClientConfig {
            server_addr: "127.0.0.1:8080".parse().unwrap(),
            psk: "test-psk".into(),
            device_name: "我的电脑".into(),
            hidden: false,
        };
        assert_eq!(config.server_addr.port(), 8080);
        assert_eq!(config.device_name, "我的电脑");

        let s1 = RelayClientStatus::Connecting;
        let s2 = RelayClientStatus::Connecting;
        assert_eq!(s1, s2);

        let s3 = RelayClientStatus::Registered;
        assert_ne!(s1, s3);
    }

    /// Register 名兜底:空 device_name 时上报 "LocalTrans"(兼容旧调用方)
    #[test]
    fn register_name_fallback_for_empty() {
        let name = register_display_name("");
        assert_eq!(name, "LocalTrans");
        assert_eq!(register_display_name("实名"), "实名");
    }
}
