//! S2 注册占有证明:e2e。
//! 合法签名注册通过;用自己的私钥冒他人指纹注册被拒 + 记认证失败。

use localtrans_core::identity::{fingerprint_of, Identity};
use localtrans_core::relay::proto::{
    decode_relay_msg, encode_relay_msg, LeaseInfo, RelayMsg,
};
use localtrans_relay::{RelayConfig, RelayServer};
use rustls::client::danger::{ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::DigitallySignedStruct;
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ---------- 测试脚手架(与 control.rs 单测同模式) ----------

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

async fn connect_and_psk(addr: std::net::SocketAddr) -> quinn::Connection {
    let conn = client_ep()
        .connect(addr, "localtrans-relay")
        .unwrap()
        .await
        .unwrap();
    // PSK 证明走首条 bi 流
    let (mut tx_proof, _rx) = conn.open_bi().await.unwrap();
    let mut proof = [0u8; 32];
    conn.export_keying_material(&mut proof, b"localtrans-relay-psk", "dev-psk".as_bytes())
        .unwrap();
    tx_proof.write_all(&proof).await.unwrap();
    tx_proof.finish().unwrap();
    conn
}

/// 收服务端 uni 推送(每条消息一条流)
async fn recv_push(conn: &quinn::Connection) -> RelayMsg {
    let mut rx = conn.accept_uni().await.unwrap();
    let mut len_buf = [0u8; 4];
    rx.read_exact(&mut len_buf).await.unwrap();
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    rx.read_exact(&mut body).await.unwrap();
    decode_relay_msg(&[&len_buf[..], &body[..]].concat()).unwrap()
}

async fn send_msg(conn: &quinn::Connection, msg: &RelayMsg) {
    let (mut tx, _rx) = conn.open_bi().await.unwrap();
    tx.write_all(&encode_relay_msg(msg).unwrap()).await.unwrap();
    tx.finish().unwrap();
}

/// 起服务器,返回 (addr, server)
async fn start_server(offset: u16) -> (std::net::SocketAddr, Arc<RelayServer>) {
    let config = RelayConfig::for_test_with_port_offset(offset);
    let server = RelayServer::bind(config.clone()).await.unwrap();
    let addr = format!("127.0.0.1:{}", server.local_control_addr().port())
        .parse()
        .unwrap();
    tokio::spawn(server.clone().run());
    (addr, server)
}

const PORT_OFFSET_S2_OK: u16 = 20;
const PORT_OFFSET_S2_STEAL: u16 = 21;

/// Step 4(先红后绿中"绿"):合法签名注册通过并拿到租约。
#[tokio::test]
#[serial]
async fn s2_valid_signature_register_accepted() {
    let (addr, server) = start_server(PORT_OFFSET_S2_OK).await;

    let dir = TempDir::new().unwrap();
    let id = Arc::new(Identity::load_or_create(dir.path()).unwrap());

    let conn = connect_and_psk(addr).await;
    // 服务端应先推 ServerNonce
    let nonce = match recv_push(&conn).await {
        RelayMsg::ServerNonce { nonce } => nonce,
        other => panic!("期望 ServerNonce, 实得 {:?}", other),
    };

    // 用真实身份签名 fp||nonce 随 Register 发出
    let fp = id.fingerprint();
    let mut signed = Vec::with_capacity(64);
    signed.extend_from_slice(&fp);
    signed.extend_from_slice(&nonce);
    let reg = RelayMsg::Register {
        name: "正规军".into(),
        fingerprint: fp,
        hidden: false,
        cert_der: id.cert.as_ref().to_vec(),
        nonce_sig: id.sign(&signed),
    };
    send_msg(&conn, &reg).await;

    let ack = recv_push(&conn).await;
    match ack {
        RelayMsg::RegisterAck { lease: Some(LeaseInfo { data_port, .. }), .. } => {
            // offset 20 → 数据端口段 9000+20*100=11000 起
            assert!((11000..11100).contains(&data_port));
        }
        other => panic!("合法签名注册应通过, 实得 {:?}", other),
    }
    server.shutdown().await;
}

/// S2 核心:身份 B(自己的私钥)冒 A 的指纹注册 → 被拒 + 记认证失败;
/// 且 A 之后仍能正常注册(连接未被顶替)。
#[tokio::test]
#[serial]
async fn s2_fingerprint_hijack_with_wrong_key_rejected() {
    let (addr, server) = start_server(PORT_OFFSET_S2_STEAL).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let fp_a = id_a.fingerprint();

    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());

    // 抢注者:声明 fp=A,附 A 的证书,但用自己的私钥签名
    let conn_b = connect_and_psk(addr).await;
    let nonce_b = match recv_push(&conn_b).await {
        RelayMsg::ServerNonce { nonce } => nonce,
        other => panic!("期望 ServerNonce, 实得 {:?}", other),
    };
    let mut signed = Vec::with_capacity(64);
    signed.extend_from_slice(&fp_a);
    signed.extend_from_slice(&nonce_b);
    let hijack = RelayMsg::Register {
        name: "抢注者".into(),
        fingerprint: fp_a,
        hidden: false,
        cert_der: id_a.cert.as_ref().to_vec(),
        nonce_sig: id_b.sign(&signed),
    };
    send_msg(&conn_b, &hijack).await;

    // 应收到 Error 而非 RegisterAck
    let resp = recv_push(&conn_b).await;
    assert!(
        matches!(resp, RelayMsg::Error { .. }),
        "抢注注册应被拒并回 Error",
    );

    // 认证失败计数已记(该 IP 至少 1 次);等服务端协程完成 record_auth_failure
    let remote_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let mut count = 0;
    for _ in 0..50 {
        count = server.auth_failure_count(remote_ip).await;
        if count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(count >= 1, "抢注被拒后应记录认证失败, 实际计数 {}", count);

    // A 用真实身份正常注册,不被顶替干扰
    let conn_a = connect_and_psk(addr).await;
    let nonce_a = match recv_push(&conn_a).await {
        RelayMsg::ServerNonce { nonce } => nonce,
        other => panic!("期望 ServerNonce, 实得 {:?}", other),
    };
    let mut signed_a = Vec::with_capacity(64);
    signed_a.extend_from_slice(&fp_a);
    signed_a.extend_from_slice(&nonce_a);
    let legit = RelayMsg::Register {
        name: "真A".into(),
        fingerprint: fp_a,
        hidden: false,
        cert_der: id_a.cert.as_ref().to_vec(),
        nonce_sig: id_a.sign(&signed_a),
    };
    send_msg(&conn_a, &legit).await;
    let ack = recv_push(&conn_a).await;
    assert!(
        matches!(ack, RelayMsg::RegisterAck { lease: Some(_), .. }),
        "真实持有者注册应通过(未被顶替), 实得 {:?}", ack,
    );
    // 名册里只有一份 fp=A 的租约
    assert_eq!(server.leases.lease_count(), 1);

    let _ = fingerprint_of(&id_a.cert); // 保持 import 使用
    server.shutdown().await;
}

/// S5 服务端半(提前):PSK 后发伪造的超长帧头 len=0xFFFFFFFF,
/// 服务端必须拒读并断连,而不是按 len 分配内存(pre-auth 内存放大防护)。
#[tokio::test]
#[serial]
async fn s5_oversized_frame_header_rejected() {
    let (addr, server) = start_server(22).await;

    let conn = connect_and_psk(addr).await;
    // 收掉 ServerNonce,进入等 Register 阶段
    let _ = match recv_push(&conn).await {
        RelayMsg::ServerNonce { nonce } => nonce,
        other => panic!("期望 ServerNonce, 实得 {:?}", other),
    };

    // 发 4B 帧头声明 ~4GB body,不跟任何 body
    let (mut tx, _rx) = conn.open_bi().await.unwrap();
    tx.write_all(&0xFFFFFFFFu32.to_be_bytes()).await.unwrap();
    tx.finish().unwrap();

    // 服务端应拒帧并关连接:closed() 返回而非永久挂起
    let closed = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
    assert!(closed.is_ok(), "超长帧应导致服务端断连, 连接却一直保持");

    // 认证失败也被记录(pre-auth 失败同样计数)
    let remote_ip: std::net::IpAddr = "127.0.0.1".parse().unwrap();
    let mut count = 0;
    for _ in 0..50 {
        count = server.auth_failure_count(remote_ip).await;
        if count >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(count >= 1, "超长帧被拒后应记录认证失败, 实际计数 {}", count);

    server.shutdown().await;
}
