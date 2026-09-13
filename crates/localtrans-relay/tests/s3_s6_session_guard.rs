//! S3 会话成员校验 + S6 端口 TTL/Punch 速率限制——e2e。
//! 非成员 KNOCK/DATA 在会话端口被丢弃;空闲会话端口 2×lease_ttl 回收;
//! 同 fp 的 Punch 超 10 次/分被限。

use localtrans_core::identity::Identity;
use localtrans_core::relay::proto::{
    data_header_encode, decode_relay_msg, encode_relay_msg, FLAG_DATA, FLAG_KNOCK,
};
use localtrans_relay::{DataPlane, RelayConfig, RelayServer};
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::DigitallySignedStruct;
use serial_test::serial;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------- QUIC 测试脚手架(与 s2_register_signature 同模式) ----------

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
impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self, _: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>,
        _: &[u8], _: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message, cert, dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self, message: &[u8], cert: &CertificateDer<'_>, dss: &DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message, cert, dss,
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

async fn connect_and_psk(addr: SocketAddr) -> quinn::Connection {
    let conn = client_ep().connect(addr, "localtrans-relay").unwrap().await.unwrap();
    let (mut tx_proof, _rx) = conn.open_bi().await.unwrap();
    let mut proof = [0u8; 32];
    conn.export_keying_material(&mut proof, b"localtrans-relay-psk", b"dev-psk").unwrap();
    tx_proof.write_all(&proof).await.unwrap();
    tx_proof.finish().unwrap();
    conn
}

async fn recv_push(conn: &quinn::Connection) -> localtrans_core::relay::proto::RelayMsg {
    use localtrans_core::relay::proto::decode_relay_msg;
    let mut rx = conn.accept_uni().await.unwrap();
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await.unwrap();
    decode_relay_msg(&[&len_buf[..], &body[..]].concat()).unwrap()
}

async fn send_msg(conn: &quinn::Connection, msg: &localtrans_core::relay::proto::RelayMsg) {
    use localtrans_core::relay::proto::encode_relay_msg;
    let (mut tx, _rx) = conn.open_bi().await.unwrap();
    tx.write_all(&encode_relay_msg(msg).unwrap()).await.unwrap();
    tx.finish().unwrap();
}

/// 注册设备,返回 (指纹, 数据端口, 数据面令牌, 连接)
async fn register_device(
    addr: SocketAddr,
    name: &str,
) -> ([u8; 32], u16, [u8; 16], quinn::Connection) {
    use localtrans_core::relay::proto::{RelayMsg, LeaseInfo};
    let dir = TempDir::new().unwrap();
    let id = Arc::new(Identity::load_or_create(dir.path()).unwrap());
    // 目录被临时对象释放——把身份留在堆上即可,签名只需要 Arc 里的 id
    let conn = connect_and_psk(addr).await;
    let nonce = match recv_push(&conn).await {
        RelayMsg::ServerNonce { nonce } => nonce,
        other => panic!("期望 ServerNonce, 实得 {:?}", other),
    };
    let fp = id.fingerprint();
    let mut signed = Vec::with_capacity(64);
    signed.extend_from_slice(&fp);
    signed.extend_from_slice(&nonce);
    send_msg(&conn, &RelayMsg::Register {
        name: name.into(),
        fingerprint: fp,
        hidden: false,
        cert_der: id.cert.as_ref().to_vec(),
        nonce_sig: id.sign(&signed),
    }).await;
    match recv_push(&conn).await {
        RelayMsg::RegisterAck { lease: Some(LeaseInfo { data_port, token }), .. } => {
            (fp, data_port, token, conn)
        }
        other => panic!("期望 RegisterAck, 实得 {:?}", other),
    }
}

/// 发送数据面包(18B 头 + 载荷)
async fn send_data_pkt(sock: &tokio::net::UdpSocket, token: &[u8; 16], flag: u8, payload: &[u8], to: SocketAddr) {
    let mut pkt = Vec::new();
    data_header_encode(&mut pkt, token, flag);
    pkt.extend_from_slice(payload);
    sock.send_to(&pkt, to).await.unwrap();
}

const PORT_OFFSET: u16 = 30;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// 组装:S3/S6 公共环境。返回 (server, dp, 会话地址)
/// 连接保活:所有连接装箱进返回值由调用方持有。
async fn setup_with_session(
    config: RelayConfig,
) -> (
    Arc<RelayServer>,
    Arc<DataPlane>,
    ([u8; 32], [u8; 16]),
    ([u8; 32], [u8; 16]),
    SocketAddr,
    Vec<quinn::Connection>, // 保活连接
) {
    let server = RelayServer::bind(config.clone()).await.unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_control_addr().port()).parse().unwrap();
    tokio::spawn(server.clone().run());
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (fpa, _, tok_a, conn_a) = register_device(addr, "A").await;
    let (fpb, _, tok_b, conn_b) = register_device(addr, "B").await;

    // A → Punch{B}
    send_msg(&conn_a, &localtrans_core::relay::proto::RelayMsg::Punch { target_fp: fpb }).await;
    // A 吐掉积压推送直到拿到 PunchResp
    let mut session_addr: Option<SocketAddr> = None;
    for _ in 0..10 {
        match recv_push(&conn_a).await {
            localtrans_core::relay::proto::RelayMsg::PunchResp { ok, session_addr: sa, .. } => {
                assert!(ok, "Punch 应成功");
                session_addr = Some(sa.unwrap().parse().unwrap());
                break;
            }
            _ => continue,
        }
    }
    let session_addr = session_addr.expect("未收到 PunchResp");
    (server, dp, (fpa, tok_a), (fpb, tok_b), session_addr, vec![conn_a, conn_b])
}

/// S3:非成员 fp 的 KNOCK 进不了学习表,DATA 不被转发;成员转发不受干扰。
#[tokio::test]
#[serial]
async fn s3_non_member_knock_data_dropped() {
    init_tracing();
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 4;
    let (server, dp, (_, tok_a), (_, tok_b), session_addr, _keep) = setup_with_session(config).await;

    let sock_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let sock_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // 成员双方 KNOCK 学习(轮询等待学习生效)
    send_data_pkt(&sock_a, &tok_a, FLAG_KNOCK, b"", session_addr).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    send_data_pkt(&sock_b, &tok_b, FLAG_KNOCK, b"", session_addr).await;
    {
        let dl = tokio::time::Instant::now() + Duration::from_secs(3);
        loop {
            if dp.learned_len(session_addr.port()).await == 2 { break; }
            assert!(tokio::time::Instant::now() < dl, "成员 KNOCK 学习超时");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // 第三方注册并冒充加入同一会话端口。
    // 注意:必须持有 C 的连接直到断言结束——连接一旦 drop,服务端会因连接关闭
    // 回收 C 的租约,后续 KNOCK 的令牌就变成未知令牌被静默丢弃(而非走
    // 非成员拒绝计数),与本断言的语义产生竞态(曾表现为偶发失败)。
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_control_addr().port()).parse().unwrap();
    let (_fpc, _, tok_c, conn_c) = register_device(addr, "C Intruder").await;
    let sock_c = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send_data_pkt(&sock_c, &tok_c, FLAG_KNOCK, b"", session_addr).await;
    // 轮询等拒绝计数出现(C 的 KNOCK 已被处理)
    {
        let dl = tokio::time::Instant::now() + Duration::from_secs(3);
        while dp.nonmember_rejected() < 1 {
            assert!(tokio::time::Instant::now() < dl, "非成员 KNOCK 未被拒");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // 断言1:学习表仍只有 2 个条目(C 没进去)
    assert_eq!(dp.learned_len(session_addr.port()).await, 2, "非成员 KNOCK 不该进学习表");

    // 断言2:C 的 DATA 不被转发给任何人;A↔B 正常互转不受干扰
    send_data_pkt(&sock_c, &tok_c, FLAG_DATA, b"injected", session_addr).await;

    let mut data_a = Vec::new();
    data_header_encode(&mut data_a, &tok_a, FLAG_DATA);
    data_a.extend_from_slice(b"genuine");
    sock_a.send_to(&data_a, session_addr).await.unwrap();

    // B 先收到的不可能是 "injected",而是(或不收后超时的)"genuine"
    let mut buf = [0u8; 2048];
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        assert!(tokio::time::Instant::now() < deadline, "A→B 的正常转发丢失");
        let n = tokio::time::timeout_at(deadline, sock_b.recv(&mut buf)).await.unwrap().unwrap();
        assert_ne!(&buf[..n], b"injected", "注入包不该到达 B");
        if &buf[..n] == b"genuine" { break; }
    }

    // C 自己也收不到任何转发(中继不会把会话内容发给它)
    match tokio::time::timeout(Duration::from_millis(200), sock_c.recv(&mut buf)).await {
        Err(_) => {}
        Ok(Ok(n)) => panic!("非成员 C 不该收到转发, 实得 {}B", n),
        Ok(Err(e)) => panic!("C 接收错误: {}", e),
    }

    // 计数器已累加(KNOCK + DATA 至少各一次)
    assert!(dp.nonmember_rejected() >= 2, "非成员拒绝计数应 >= 2, 实得 {}", dp.nonmember_rejected());

    drop(conn_c); // 断言完毕才允许 C 的租约被回收
    server.shutdown().await;
    dp.shutdown().await;
}

/// S6a:空闲会话端口在 last_activity 超 2×lease_ttl 后被 reap_expired 连带回收。
#[tokio::test]
#[serial]
async fn s6_idle_session_port_reclaimed_after_ttl() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET + 1);
    config.data_port_end = config.data_port_start + 4;
    config.lease_ttl_secs = 1; // 2×ttl = 2s,测试可承受

    let (server, dp, _, _, session_addr, _keep) = setup_with_session(config).await;
    let port = session_addr.port();
    assert!(server.leases.is_session_port(port));

    // 空闲超过 2×ttl(2s)——期间无任何 KNOCK/DATA 刷新会话端口活跃时刻
    tokio::time::sleep(Duration::from_millis(2300)).await;

    // ttl 已调小到 1s,设备租约同样会过期(测试环境无客户端心跳);
    // 关键断言是会话端口必须被连带回收
    let _gone = server.leases.reap_expired(Duration::from_secs(1));
    assert!(!server.leases.is_session_port(port), "空闲会话端口应在 2×ttl 后被回收");

    server.shutdown().await;
    dp.shutdown().await;
}

/// S6b:同 fp 连发 Punch 超 10 次/分钟,第 11 次起被限(ok:false)。
#[tokio::test]
#[serial]
async fn s6_punch_rate_limited_per_fp() {
    let config = RelayConfig::for_test_with_port_offset(PORT_OFFSET + 2);
    let server = RelayServer::bind(config.clone()).await.unwrap();
    let addr: SocketAddr = format!("127.0.0.1:{}", server.local_control_addr().port()).parse().unwrap();
    tokio::spawn(server.clone().run());

    let (_fpa, _, _, conn_a) = register_device(addr, "RL-A").await;
    let (fpb, _, _, conn_b) = register_device(addr, "RL-B").await;
    let _ = recv_push(&conn_a).await; // 吞掉 Roster 推送(若有)

    // 前 10 次 ok:true
    for i in 0..10 {
        send_msg(&conn_a, &localtrans_core::relay::proto::RelayMsg::Punch { target_fp: fpb }).await;
        let mut ok_resp = false;
        for _ in 0..10 {
            match recv_push(&conn_a).await {
                localtrans_core::relay::proto::RelayMsg::PunchResp { ok, .. } => {
                    if i < 10 { assert!(ok, "第 {} 次 Punch 应放行", i + 1); }
                    ok_resp = true;
                    break;
                }
                _ => continue,
            }
        }
        assert!(ok_resp, "第 {} 次未收到 PunchResp", i + 1);
    }

    // 第 11 次:应被限流 → ok:false
    send_msg(&conn_a, &localtrans_core::relay::proto::RelayMsg::Punch { target_fp: fpb }).await;
    let mut limited = false;
    for _ in 0..10 {
        match recv_push(&conn_a).await {
            localtrans_core::relay::proto::RelayMsg::PunchResp { ok, reason, .. } => {
                assert!(!ok, "第 11 次 Punch 应被限流");
                assert!(reason.is_some(), "限流响应应带 reason");
                limited = true;
                break;
            }
            _ => continue,
        }
    }
    assert!(limited, "限流响应未送达");
    let _ = conn_b; // B 保持在线身份即可
    server.shutdown().await;
}
