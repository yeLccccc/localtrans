// Task 11: QUIC endpoint 与自签证书锁定握手

use quinn::{Endpoint, ServerConfig, ClientConfig, Connection};
use quinn::crypto::rustls::{QuicServerConfig, QuicClientConfig};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls_pki_types::PrivateKeyDer;
use rustls::ServerConfig as RustlsServerConfig;
use rustls::ClientConfig as RustlsClientConfig;
use rustls::client::danger::{ServerCertVerifier, ServerCertVerified, HandshakeSignatureValid};
use rustls::server::danger::{ClientCertVerifier, ClientCertVerified};
use rustls::DigitallySignedStruct;
use rustls::SignatureScheme;
use rustls::DistinguishedName;
use rustls::Error as RustlsError;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::timeout;
use x509_parser::prelude::FromDer;

use crate::identity::{Identity, Fingerprint, fingerprint_of, TrustStore, TrustedPeer, Perms};
use crate::pairing::{PairingMachine, COOLDOWN_SECS};
use crate::protocol::{ControlMsg, encode_control, decode_control_body};
use crate::store::Config;

/// 会话设置错误
#[derive(thiserror::Error, Debug)]
pub enum SetupError {
    #[error("TLS 配置错误: {0}")]
    Tls(String),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

/// 会话错误
#[derive(thiserror::Error, Debug)]
pub enum SessionError {
    #[error("连接错误: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("连接已关闭: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("对端未提供证书")]
    NoPeerCert,
    #[error("设置错误: {0}")]
    Setup(#[from] SetupError),
    #[error("配对错误: {0}")]
    Pairing(String),
    #[error("连接处于冷却期，剩余 {0} 秒")]
    Cooldown(u64),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
}

impl From<crate::pairing::PairingError> for SessionError {
    fn from(e: crate::pairing::PairingError) -> Self {
        match e {
            crate::pairing::PairingError::Session(se) => se,
        }
    }
}

/// 验证证书时间有效性
fn verify_cert_validity(cert_der: &CertificateDer<'_>, now: UnixTime) -> Result<(), RustlsError> {
    use x509_parser::certificate::X509Certificate;

    // 解析证书
    let (_, parsed) = X509Certificate::from_der(cert_der.as_ref())
        .map_err(|e| RustlsError::General(format!("证书解析失败: {}", e)))?;

    // 获取有效期
    let validity = parsed.validity();
    let not_before = validity.not_before;
    let not_after = validity.not_after;

    // 将 UnixTime 转换为时间戳进行比较
    let now_secs = now.as_secs();
    // ASN1Time 有 timestamp 方法返回 Unix 时间戳
    let nb_secs = not_before.timestamp();
    let na_secs = not_after.timestamp();

    // 检查 not_before <= now <= not_after
    if now_secs < nb_secs as u64 {
        return Err(RustlsError::General("证书尚未生效（now < not_before）".into()));
    }
    if now_secs > na_secs as u64 {
        return Err(RustlsError::General("证书已过期（now > not_after）".into()));
    }

    Ok(())
}

/// 切出 tbsCertificate 的完整 DER(含元素头)。签名覆盖的是 TBS 编码本身,
/// 而 x509-parser 未公开 TBS 原始字节,故手工走一遍 DER 结构。
fn tbs_der(der: &[u8]) -> Result<&[u8], RustlsError> {
    /// 读取一个 DER 元素,返回 (元素头长度, 内容长度);不移动借用
    fn elem(data: &[u8]) -> Option<(usize, usize)> {
        if data.len() < 2 {
            return None;
        }
        let (header_len, content_len) = match data[1] {
            l if l & 0x80 == 0 => (2, l as usize),
            l => {
                let n = (l & 0x7f) as usize;
                if n == 0 || n > 4 || data.len() < 2 + n {
                    return None;
                }
                let mut len = 0usize;
                for b in &data[2..2 + n] {
                    len = (len << 8) | *b as usize;
                }
                (2 + n, len)
            }
        };
        if data.len() < header_len + content_len {
            return None;
        }
        Some((header_len, content_len))
    }

    let err = || RustlsError::General("证书 DER 结构非法".into());
    // Certificate ::= SEQUENCE { tbsCertificate, signatureAlgorithm, signatureValue }
    let (h, c) = elem(der).ok_or_else(err)?;
    if der[0] != 0x30 {
        return Err(err());
    }
    let body = &der[h..h + c];
    let (th, tc) = elem(body).ok_or_else(err)?;
    if body[0] != 0x30 {
        return Err(err());
    }
    Ok(&body[..th + tc])
}

/// 从证书提取 Ed25519 公钥(本项目身份证书固定为 Ed25519)
fn ed25519_public_key(cert_der: &CertificateDer<'_>) -> Result<[u8; 32], RustlsError> {
    use x509_parser::certificate::X509Certificate;

    let (_, parsed) = X509Certificate::from_der(cert_der.as_ref())
        .map_err(|e| RustlsError::General(format!("证书解析失败: {}", e)))?;
    let key = parsed.tbs_certificate.subject_pki.subject_public_key.data;
    if key.len() != 32 {
        return Err(RustlsError::General("仅支持 Ed25519 证书公钥".into()));
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&key);
    Ok(pk)
}

/// 用证书自带公钥验证 msg 上的 Ed25519 签名
fn verify_with_cert_key(
    cert_der: &CertificateDer<'_>,
    msg: &[u8],
    sig: &[u8],
) -> Result<(), RustlsError> {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    let pk = ed25519_public_key(cert_der)?;
    let vk = VerifyingKey::from_bytes(&pk)
        .map_err(|e| RustlsError::General(format!("证书公钥非法: {}", e)))?;
    let sig = Signature::from_slice(sig)
        .map_err(|e| RustlsError::General(format!("签名长度非法: {}", e)))?;
    vk.verify(msg, &sig)
        .map_err(|_| RustlsError::General("证书签名验证失败".into()))
}

/// 验证证书自签:issuer == subject 且证书签名可被其自身公钥验证
fn verify_self_signed(cert_der: &CertificateDer<'_>) -> Result<(), RustlsError> {
    use x509_parser::certificate::X509Certificate;

    let (_, parsed) = X509Certificate::from_der(cert_der.as_ref())
        .map_err(|e| RustlsError::General(format!("证书解析失败: {}", e)))?;

    // C1: 检查 issuer == subject（自签特征）
    if parsed.issuer() != parsed.subject() {
        return Err(RustlsError::General("证书非自签（issuer != subject）".into()));
    }

    // C1: 签名可被证书自身公钥验证（防伪造 issuer==subject 的外来证书）
    let tbs = tbs_der(cert_der.as_ref())?;
    let sig = parsed.signature_value.data;
    verify_with_cert_key(cert_der, tbs, &sig)
}

/// 验证 TLS1.3 CertificateVerify:签名方案必须是 Ed25519,
/// 且签名确实出自对端证书的公钥——否则握手无法证明对端持有私钥。
fn verify_tls13_ed25519(
    cert: &CertificateDer<'_>,
    message: &[u8],
    dss: &DigitallySignedStruct,
) -> Result<HandshakeSignatureValid, RustlsError> {
    if dss.scheme != SignatureScheme::ED25519 {
        return Err(RustlsError::General(format!(
            "不支持的签名方案: {:?}(仅 Ed25519)",
            dss.scheme
        )));
    }
    verify_with_cert_key(cert, message, dss.signature())?;
    Ok(HandshakeSignatureValid::assertion())
}

/// 客户端自定义验证器: 验证自签证书指纹 + 自签一致性 + 时间
#[derive(Debug)]
struct FingerprintVerifier {
    expected: Option<Fingerprint>,
}

impl ServerCertVerifier for FingerprintVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // C1: 检查自签
        verify_self_signed(end_entity)?;

        // C2: 检查时间
        verify_cert_validity(end_entity, _now)?;

        // 检查指纹
        let fp = fingerprint_of(end_entity);
        if let Some(expected) = &self.expected {
            if fp != *expected {
                return Err(RustlsError::General("证书指纹不匹配".into()));
            }
        }

        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General("TLS1.2 不支持".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_ed25519(cert, message, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// C3: 服务端自定义验证器: 接受任何自签客户端证书
#[derive(Debug)]
struct AcceptAnySelfSignedClientCert;

impl ClientCertVerifier for AcceptAnySelfSignedClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        // C1: 检查自签
        verify_self_signed(end_entity)?;

        // C2: 检查时间
        verify_cert_validity(end_entity, _now)?;

        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General("TLS1.2 不支持".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_ed25519(cert, message, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    // I7: 补齐必需方法
    fn client_auth_mandatory(&self) -> bool {
        false
    }
}

/// S1: 服务端钉扎验证器——FingerprintVerifier 的服务端对称版。
/// 自签 + 有效期 + 客户端证书指纹必须等于预期,不符返回 Err 让 TLS
/// 握手本身失败(消除"握手完成后才比对"的验证窗口)。中继 accept_peer
/// 用:from_fp(PunchNotif 宣称的发起方)在 accept 前已知,可直接钉扎。
#[derive(Debug)]
struct PinnedClientCertVerifier {
    expected: Fingerprint,
}

impl ClientCertVerifier for PinnedClientCertVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, RustlsError> {
        // C1: 检查自签
        verify_self_signed(end_entity)?;

        // C2: 检查时间
        verify_cert_validity(end_entity, _now)?;

        // S1: 指纹钉扎(与 FingerprintVerifier 的措辞对齐)
        let fp = fingerprint_of(end_entity);
        if fp != self.expected {
            return Err(RustlsError::General("证书指纹不匹配".into()));
        }

        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        Err(RustlsError::General("TLS1.2 不支持".into()))
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        verify_tls13_ed25519(cert, message, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }

    fn offer_client_auth(&self) -> bool {
        true
    }

    // 钉扎场景客户端证书是强制的——没有证书就无从比对指纹
    fn client_auth_mandatory(&self) -> bool {
        true
    }
}

/// QUIC 传输层公共参数(keep-alive/流控/MTU 发现)。
/// quic_transport_config(局域网 60s)与 relay_transport_config(中继 15s)
/// 共用此构建器,保证除 idle 外参数永不漂移。
fn base_transport_config() -> quinn::TransportConfig {
    let mut tc = quinn::TransportConfig::default();
    tc.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    // 流控窗口：默认 1.25MB/流、15MB/连接，块流单块 4MB、窗口 16 流并发
    // 在途可达 64MB——默认窗口下每批要十几个往返才喂得饱，回环实测被压到
    // ~200MB/s。放宽到单流 8MB/连接 64MB（局域网高带宽低丢包场景）。
    tc.stream_receive_window(
        quinn::VarInt::from_u32(8 * 1024 * 1024),
    );
    tc.receive_window(
        quinn::VarInt::from_u32(64 * 1024 * 1024),
    );
    // RFC 8899 路径 MTU 发现：上界封在 1500（标准以太网）。
    // 曾经放开到 65527 以吃满回环/Jumbo 帧，但实机跨网段（10.50.5.x ↔
    // 10.50.35.x，中间隔路由器）时探测一旦越过路径 MTU，探测包被路由器
    // 静默丢弃，整条连接随之黑洞——表现即传输挂死、无任何错误日志。
    // 回环/同网段实测 1200→1500 已足够（千兆线速 ~113MB/s），可靠性优先。
    tc.mtu_discovery_config(Some({
        let mut m = quinn::MtuDiscoveryConfig::default();
        m.upper_bound(1500);
        m
    }));
    tc
}

/// QUIC 传输层配置：长传输的连接保活与空闲超时加固(局域网直连)。
///
/// quinn 默认 max_idle_timeout=30s 且不发 keep-alive——任何一侧因磁盘慢写/
/// 杀毒扫描等停顿超过 30 秒，即使传输未完成连接也会被判死（回环基准实测
/// 触发过）。这里放宽到 60s 并以 5s 间隔主动保活，保证静默期连接存活。
pub fn quic_transport_config() -> std::sync::Arc<quinn::TransportConfig> {
    let mut tc = base_transport_config();
    tc.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(60))
            .expect("60s 在 IdleTimeout 表示范围内"),
    ));
    std::sync::Arc::new(tc)
}

/// 中继控制面专用传输配置:与 quic_transport_config 同源参数,但
/// 不发 keep-alive(S7)。服务端需要 idle 超时来自然回收半开连接,
/// 客户端不发 keep-alive 反而让死连接尽快被发现;
/// 服务端的存活判定靠客户端 15s Ping 显式心跳 + 应用层超时。
pub fn relay_control_transport_config() -> std::sync::Arc<quinn::TransportConfig> {
    let mut tc = base_transport_config();
    tc.keep_alive_interval(None);
    // 服务端每条推送都是一条 uni 流,放宽客户端的单向流并发上限(默认 100),
    // 避免正常设备在推送风暴下被流预算卡住
    tc.max_concurrent_uni_streams(quinn::VarInt::from_u32(1024));
    tc.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_secs(60))
            .expect("60s 在 IdleTimeout 表示范围内"),
    ));
    std::sync::Arc::new(tc)
}

/// 中继内层端点传输配置:与局域网同源(base_transport_config),
/// 仅 idle 60s → 15s。15s = 3 个心跳周期,躲开单次抖动误杀;
/// 路径死亡的僵尸窗口从 30~60s 缩到 15s。局域网直连端点不受影响。
pub fn relay_transport_config() -> std::sync::Arc<quinn::TransportConfig> {
    let mut tc = base_transport_config();
    tc.max_idle_timeout(Some(
        quinn::IdleTimeout::try_from(std::time::Duration::from_millis(30_000))
            .expect("30s 在 IdleTimeout 表示范围内"),
    ));
    // MTU 探测关死:中继数据面包上限 1500(含 18B 头),quinn 默认探测到
    // 1452+ 会在本机 VPN/隧道接口触发 WSAEMSGSIZE(实测 1389/1404 字节包
    // 持续被拒),连接反复断连重连——会话每分钟级死亡的节拍器。
    // 固定 1200(与 max_udp_payload_size 一致),可靠性优先。
    tc.mtu_discovery_config(None);
    tc.initial_mtu(1200);
    tc.min_mtu(1200);
    std::sync::Arc::new(tc)
}

/// 创建服务器配置
/// 虚拟端点复用:不影响局域网行为
pub fn server_config(id: &Identity) -> Result<ServerConfig, SetupError> {
    let cert = id.cert.clone();
    let key_der = PrivateKeyDer::Pkcs8(id.pkcs8.clone().into());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let rustls_config = RustlsServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| SetupError::Tls(e.to_string()))?
        .with_client_cert_verifier(Arc::new(AcceptAnySelfSignedClientCert))
        .with_single_cert(vec![cert], key_der)
        .map_err(|e| SetupError::Tls(e.to_string()))?;

    let quic_config = QuicServerConfig::try_from(rustls_config)
        .map_err(|e| SetupError::Tls(e.to_string()))?;
    let mut server = ServerConfig::with_crypto(Arc::new(quic_config));
    server.transport_config(quic_transport_config());
    Ok(server)
}

/// S1: 带客户端证书指纹钉扎的服务端配置(中继 accept_peer 用)。
/// 与 server_config 唯一差异:client verifier 换成 PinnedClientCertVerifier,
/// 客户端证书指纹 != expected_fp 时 TLS 握手直接失败。
pub fn server_config_pinned(id: &Identity, expected_fp: Fingerprint) -> Result<ServerConfig, SetupError> {
    let cert = id.cert.clone();
    let key_der = PrivateKeyDer::Pkcs8(id.pkcs8.clone().into());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let rustls_config = RustlsServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| SetupError::Tls(e.to_string()))?
        .with_client_cert_verifier(Arc::new(PinnedClientCertVerifier { expected: expected_fp }))
        .with_single_cert(vec![cert], key_der)
        .map_err(|e| SetupError::Tls(e.to_string()))?;

    let quic_config = QuicServerConfig::try_from(rustls_config)
        .map_err(|e| SetupError::Tls(e.to_string()))?;
    let mut server = ServerConfig::with_crypto(Arc::new(quic_config));
    server.transport_config(quic_transport_config());
    Ok(server)
}

/// 客户端 rustls 构建器公共部分(TLS1.3 + ring + 指纹验证器),
/// client_config 与 connect 共用,避免配置逻辑漂移。
/// 虚拟端点复用:不影响局域网行为
pub fn client_builder(
    expected_peer: Option<Fingerprint>,
) -> Result<
    rustls::ConfigBuilder<RustlsClientConfig, rustls::client::WantsClientCert>,
    SetupError,
> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    Ok(RustlsClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| SetupError::Tls(e.to_string()))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(FingerprintVerifier {
            expected: expected_peer,
        })))
}

/// 创建客户端配置(简报接口:不附带客户端证书的连接场景)
pub fn client_config(expected_peer: Option<Fingerprint>) -> ClientConfig {
    let rustls_config = client_builder(expected_peer)
        .expect("TLS1.3 + ring 构建不会失败")
        .with_no_client_auth();
    let quic_config =
        QuicClientConfig::try_from(rustls_config).expect("QuicClientConfig 转换不会失败");
    let mut client = ClientConfig::new(Arc::new(quic_config));
    client.transport_config(quic_transport_config());
    client
}

/// 绑定 endpoint
pub fn bind_endpoint(port: u16, id: &Identity) -> Result<Endpoint, SetupError> {
    let config = server_config(id)?;
    let addr = SocketAddr::new("0.0.0.0".parse().unwrap(), port);
    Ok(Endpoint::server(config, addr)?)
}

/// 连接到对端并返回连接 + 对端指纹。
/// C3: 携带客户端证书(服务端要求);I4: 指纹从 peer_identity() 证书计算。
pub async fn connect(
    ep: &Endpoint,
    addr: SocketAddr,
    id: &Identity,
    expected: Option<Fingerprint>,
) -> Result<(Connection, Fingerprint), SessionError> {
    let rustls_config = client_builder(expected)?
        .with_client_auth_cert(vec![id.cert.clone()], PrivateKeyDer::Pkcs8(id.pkcs8.clone().into()))
        .map_err(|e| SetupError::Tls(e.to_string()))?;
    let quic_config = QuicClientConfig::try_from(rustls_config)
        .map_err(|e| SetupError::Tls(e.to_string()))?;
    let mut config = ClientConfig::new(Arc::new(quic_config));
    config.transport_config(quic_transport_config());

    let conn = ep.connect_with(config, addr, "localhost")?.await?;

    // quinn rustls 层的 Any 载荷为 Vec<CertificateDer<'static>>
    let peer_certs = conn
        .peer_identity()
        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok());
    let cert = peer_certs
        .as_ref()
        .and_then(|c| c.first())
        .ok_or(SessionError::NoPeerCert)?;

    Ok((conn, fingerprint_of(cert)))
}

// ============ 会话管理器 ============

/// Workspace-level job ID counter (T14: 拉取/推送任务 ID)
static NEXT_JOB_ID: AtomicU64 = AtomicU64::new(1);

/// 获取下一个 job ID (从 1 开始递增)
pub fn next_job_id() -> u64 {
    NEXT_JOB_ID.fetch_add(1, Ordering::SeqCst)
}

/// 分配 sender-side job id(高 bit 段 0x8000_...,与 receiver 错开)。
/// 多线程并发安全;job_id 全局唯一,跨实例也不冲突。
pub fn next_source_job_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0x8000_0000_0000_0000);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// M-B5: RPC 请求关联 ID 计数器——每请求全局唯一,响应按此多路分发
static NEXT_MSG_ID: AtomicU64 = AtomicU64::new(1);

pub fn next_msg_id() -> u64 {
    NEXT_MSG_ID.fetch_add(1, Ordering::Relaxed)
}

/// M-B5: 待完成 RPC 表——msg_id → 等待者 oneshot 发送端。
/// ctrl_loop 收到响应类消息时按 msg_id 投递;找不到等待者则 warn 丢弃
/// (老对端不带 msg_id → id=0 也找不到条目,同路径丢弃)。
type PendingRpcs = Arc<std::sync::Mutex<HashMap<u64, tokio::sync::oneshot::Sender<(Fingerprint, ControlMsg)>>>>;

fn pending_rpcs_new() -> PendingRpcs {
    Arc::new(std::sync::Mutex::new(HashMap::new()))
}

/// 注册一个待响应 RPC:返回 (msg_id, 接收端)
fn register_pending_rpc(map: &PendingRpcs) -> (u64, tokio::sync::oneshot::Receiver<(Fingerprint, ControlMsg)>) {
    let id = next_msg_id();
    let (tx, rx) = tokio::sync::oneshot::channel();
    match map.lock() {
        Ok(mut m) => { m.insert(id, tx); }
        Err(_) => tracing::warn!("pending_rpcs 锁中毒"),
    }
    (id, rx)
}

/// 按消息类型提取 msg_id(ListResp/SharesResp/MetaResp/ShareOpResult 等)
fn resp_msg_id(msg: &ControlMsg) -> u64 {
    match msg {
        ControlMsg::ListResp { msg_id, .. }
        | ControlMsg::SharesResp { msg_id, .. }
        | ControlMsg::MetaResp { msg_id, .. }
        | ControlMsg::ShareOpResult { msg_id, .. } => *msg_id,
        _ => 0,
    }
}

/// 给请求消息写入 msg_id(请求类:MetaReq/ListReq/SharesReq)
fn set_req_msg_id(msg: &mut ControlMsg, id: u64) {
    match msg {
        ControlMsg::MetaReq { msg_id, .. }
        | ControlMsg::ListReq { msg_id, .. }
        | ControlMsg::SharesReq { msg_id }
        | ControlMsg::ShareRename { msg_id, .. }
        | ControlMsg::ShareDelete { msg_id, .. }
        | ControlMsg::ShareMkdir { msg_id, .. } => *msg_id = id,
        _ => {}
    }
}

/// 会话上下文
#[derive(Clone)]
pub struct SessionCtx {
    pub identity: Arc<Identity>,
    pub trust: Arc<Mutex<TrustStore>>,
    pub config: Arc<RwLock<Config>>,
}

/// 会话事件
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// 配对同意请求：被动方收到连接后触发（不含配对码）
    PairingConsentNeeded {
        fingerprint: Fingerprint,
        name: String,
    },
    /// 配对码展示：被动方同意后展示配对码（仅给被动方自己）
    PairingCodeShown {
        fingerprint: Fingerprint,
        own_code: String,
    },
    /// 配对等待同意：主动方等待被动方同意（不含配对码）
    PairingWaitConsent {
        fingerprint: Fingerprint,
        name: String,
    },
    /// 配对码输入：主动方可输入配对码（不含配对码）
    PairingCodeEntry {
        fingerprint: Fingerprint,
        name: String,
    },
    /// 配对结果：双方码匹配成功或失败达到 3 次
    PairingResult {
        fingerprint: Fingerprint,
        ok: bool,
        reason: Option<String>,
    },
    /// 会话建立：配对成功或已信任连接成功
    SessionUp {
        fingerprint: Fingerprint,
        name: String,
        conn: quinn::Connection,
    },
    /// 会话结束：控制流关闭或连接丢失
    SessionDown {
        fingerprint: Fingerprint,
    },
    /// M3a FR6:对端已移除对本机的信任(TrustBroken 协议通知)。
    /// 收到即"即时降级":本端信任条目已由 core 同步删除(双盲对称重配),
    /// 随后走 Goodbye 同款收尾(清会话表 + SessionDown)。壳层据此弹 toast。
    /// peer_name 取会话内缓存的 Hello 名,供提示文案;可能为 None。
    TrustBroken {
        fingerprint: Fingerprint,
        peer_name: Option<String>,
    },
}

/// 配对码判定结果
enum PairOutcome {
    /// 不匹配（未达 3 次）
    Mismatch,
    /// 连续错 3 次
    FailedOut,
    /// 匹配
    Matched,
    /// 会话不在配对中（忽略）
    NotPairing,
}

/// 同意门状态(v0.4.0 配对授权强化)
#[derive(Clone, Debug)]
enum ConsentState {
    /// 被动方:等待本机用户点同意(deadline = 门超时时刻,由 consent_timeout_secs 决定)
    /// pending_code: A 的码先到时的暂存,等 B 同意后判定
    AwaitingConsent { deadline: tokio::time::Instant, pending_code: Option<String> },
    /// 已同意(pairing.machine 持有码哈希)。pending_code = A 的码先到但
    /// B 尚未同意时的暂存——放行时机 = B 点同意那一刻(grant_consent 时已转移)
    Granted { pending_code: Option<String> },
    /// 主动方:同意是对端的事,本方无门
    Initiator,
}

/// 会话状态
struct Session {
    conn: quinn::Connection,
    /// T6-FR6:本条目绑定的 quinn 连接 stable_id——ctrl_loop 退出时区分
    /// "本连接的条目"与"被新一代连接覆盖后的僵尸条目"。代次计数器在
    /// 多连接并发死亡时有删除顺序歧义,stable_id 判定精确无竞态。
    conn_id: usize,
    ctrl_send: mpsc::Sender<ControlMsg>,
    pairing: Option<PairingMachine>,
    peer_name: Option<String>,
    consent: ConsentState,
    /// M-B1: 会话代次——同一指纹被新连接覆盖时递增。
    /// 旧代 ctrl_loop / 门超时任务退出前比对代次,防止误删新一代会话。
    generation: u64,
}

/// 会话管理器
pub struct SessionManager {
    ctx: SessionCtx,
    sessions: Arc<Mutex<HashMap<Fingerprint, Session>>>,
    /// M-B1: 全局代次计数器(单调递增)
    generation_counter: Arc<AtomicU64>,
    event_tx: mpsc::Sender<SessionEvent>,
    /// 冷却期：发起方连接失败 3 次后记录
    cooldown: Arc<Mutex<HashMap<Fingerprint, Instant>>>,
    /// 用于 connect 的 endpoint（不绑定端口）
    endpoint: Arc<Mutex<Option<Endpoint>>>,
    /// 入站控制消息转发（T14: RPC 路由）
    inbound_ctrl: mpsc::Sender<(Fingerprint, ControlMsg)>,
    /// 入站控制消息接收端（可被 take 走一次）
    inbound_ctrl_rx: Arc<Mutex<Option<mpsc::Receiver<(Fingerprint, ControlMsg)>>>>,
    /// 入站主动通知类消息（SharesChanged 等 watchdog 推送）——
    /// 独立于响应通道：不受 xfer_lock 排队影响，常驻消费者随时可读
    inbound_notify: tokio::sync::broadcast::Sender<(Fingerprint, ControlMsg)>,
    /// M-B5: 推送协商类信号(OfferResp/JobDone/JobFailed 等,非请求-响应)
    /// 广播通道——多个等待者各自订阅过滤,不与 msg_id RPC 表混用
    push_signals: tokio::sync::broadcast::Sender<(Fingerprint, ControlMsg)>,
    /// M-B5: 按 msg_id 多路分发的待完成 RPC 表
    pending_rpcs: PendingRpcs,
}

impl SessionManager {
    /// 创建会话管理器
    pub fn spawn(ctx: SessionCtx) -> (Arc<Self>, mpsc::Receiver<SessionEvent>) {
        let (event_tx, event_rx) = mpsc::channel(32);
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let cooldown = Arc::new(Mutex::new(HashMap::new()));
        let (inbound_ctrl_tx, inbound_ctrl_rx) = mpsc::channel(128);
        let (notify_tx, _notify_rx) = tokio::sync::broadcast::channel(64);
        let (signal_tx, _signal_rx) = tokio::sync::broadcast::channel(64);

        let sm = Arc::new(SessionManager {
            ctx: ctx.clone(),
            sessions,
            generation_counter: Arc::new(AtomicU64::new(0)),
            event_tx,
            cooldown,
            endpoint: Arc::new(Mutex::new(None)),
            inbound_ctrl: inbound_ctrl_tx,
            inbound_ctrl_rx: Arc::new(Mutex::new(Some(inbound_ctrl_rx))),
            inbound_notify: notify_tx,
            push_signals: signal_tx,
            pending_rpcs: pending_rpcs_new(),
        });

        (sm, event_rx)
    }

    /// 订阅主动通知类消息（SharesChanged 等）。广播语义：多个订阅者各自
    /// 收到全量消息；无订阅者时消息丢弃（lagged 只会丢旧通知，无碍）
    pub fn subscribe_notify(&self) -> tokio::sync::broadcast::Receiver<(Fingerprint, ControlMsg)> {
        self.inbound_notify.subscribe()
    }

    /// 获取入站控制消息接收端（仅能调用一次，后续调用返回 None）
    /// T14: 用于 RPC 路由器接收控制面消息
    pub async fn take_inbound_ctrl_rx(&self) -> Option<mpsc::Receiver<(Fingerprint, ControlMsg)>> {
        self.inbound_ctrl_rx.lock().await.take()
    }

    /// M-B5: 注册一个待完成 RPC 并发送请求消息。
    /// 返回 (msg_id, oneshot 接收端);调用方等待接收端即得响应。
    /// 并发任意多个 RPC 互不干扰——浏览与传输不再互斥。
    pub async fn send_rpc(
        &self,
        peer: &Fingerprint,
        mut msg: ControlMsg,
    ) -> Result<(u64, tokio::sync::oneshot::Receiver<(Fingerprint, ControlMsg)>), SessionError> {
        let (id, rx) = register_pending_rpc(&self.pending_rpcs);
        set_req_msg_id(&mut msg, id);
        match self.send_ctrl(peer, msg).await {
            Ok(()) => Ok((id, rx)),
            Err(e) => {
                // 发送失败:立即撤销注册,避免表项泄漏
                self.cancel_rpc(id).await;
                Err(e)
            }
        }
    }

    /// M-B5: 订阅推送协商类信号(OfferResp/JobDone/JobFailed)。
    /// 广播语义——多个推送在途时各自订阅、各自过滤,互不干扰
    pub fn subscribe_push_signals(&self) -> tokio::sync::broadcast::Receiver<(Fingerprint, ControlMsg)> {
        self.push_signals.subscribe()
    }

    /// M-B5: 撤销未完成的 RPC 注册（超时/取消路径调用,防止表项泄漏）
    pub async fn cancel_rpc(&self, msg_id: u64) {
        if let Ok(mut m) = self.pending_rpcs.lock() {
            m.remove(&msg_id);
        }
    }

    /// 启动监听器（绑定端口并接受连接），返回实际绑定地址（port=0 时由系统分配）
    pub async fn start_listener(&self, port: u16) -> Result<SocketAddr, SetupError> {
        let ep = bind_endpoint(port, &self.ctx.identity)?;
        let mut local_addr = ep.local_addr()?;
        // 绑定在未指定地址（0.0.0.0）时不可用于拨出，回环测试以 127.0.0.1 呈现
        if local_addr.ip().is_unspecified() {
            local_addr.set_ip(match local_addr.ip() {
                std::net::IpAddr::V4(_) => "127.0.0.1".parse().unwrap(),
                std::net::IpAddr::V6(_) => "::1".parse().unwrap(),
            });
        }
        tracing::info!("监听器绑定到端口: {}", local_addr.port());
        *self.endpoint.lock().await = Some(ep.clone());

        let sm = self.clone();
        tokio::spawn(async move {
            tracing::info!("开始接受连接循环");
            loop {
                match ep.accept().await {
                    Some(incoming) => {
                        tracing::info!("收到传入连接");
                        let sm_clone = sm.clone();
                        tokio::spawn(async move {
                            match incoming.await {
                                Ok(conn) => {
                                    tracing::info!("传入连接已建立");
                                    if let Err(e) = sm_clone.handle_incoming(conn).await {
                                        tracing::error!("处理传入连接失败: {}", e);
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!("接受连接失败: {}", e);
                                }
                            }
                        });
                    }
                    None => {
                        tracing::warn!("Endpoint 关闭，停止接受连接");
                        break;
                    }
                }
            }
        });

        Ok(local_addr)
    }

    /// 同意门超时秒数:读取配置,缺省 60,钳制到 [1, 600]
    ///
    /// 分层说明:
    /// - Core 层(此函数):下限放宽到 1s 供测试使用,上限 600s(10 分钟)
    /// - 用户面(Tauri 壳层 save_settings):钳制到 [15, 600] 保证可用性
    /// - 这样既满足测试快速验证(1s 门超时),又保证生产环境不会出现过短的超时
    async fn consent_timeout(&self) -> Duration {
        let secs = self.ctx.config.read().await.consent_timeout_secs;
        Duration::from_secs(secs.clamp(1, 600))
    }

    /// v0.5.0 推送确认超时（core 钳制 1-600；发送方兜底 = 此值 + 30s）
    pub async fn offer_timeout_secs(&self) -> u64 {
        self.ctx.config.read().await.offer_timeout_secs.clamp(1, 600)
    }

    /// 握手后的通用处理流程（冷却检查 → 信任检查 → SAS 派生 → 控制流建立）
    /// connect() 和 adopt_connection() 都走此路径，确保局域网路径行为逐字节不变
    async fn post_handshake(
        &self,
        conn: quinn::Connection,
        peer_fp: Fingerprint,
        addr: Option<std::net::SocketAddr>,
    ) -> Result<Fingerprint, SessionError> {
        tracing::info!("握手后处理开始，对端指纹: {}", hex::encode(peer_fp));

        // 冷却期检查：3 次配对码错误的指纹在冷却时间内直接拒绝
        {
            let mut cooldown = self.cooldown.lock().await;
            // 顺手清理已过期项,防表无限增长(低危审计修复)
            cooldown.retain(|_, e| *e > Instant::now());
            if let Some(&expires) = cooldown.get(&peer_fp) {
                let now = Instant::now();
                if now < expires {
                    let remaining = (expires - now).as_secs();
                    drop(cooldown);
                    conn.close(0u8.into(), b"cooldown");
                    return Err(SessionError::Cooldown(remaining));
                }
            }
        }

        // 检查是否已信任
        let is_trusted = {
            let trust = self.ctx.trust.lock().await;
            trust.is_trusted(&peer_fp)
        };

        let own_code = String::new(); // Task 3: 同意门下码在 grant_consent 生成

        if is_trusted {
            // 已信任：发起控制流并直接报告 SessionUp
            let peer_name = self.establish_control_flow(conn.clone(), peer_fp, own_code.clone(), true).await?;

            let _ = self.event_tx.send(SessionEvent::SessionUp {
                fingerprint: peer_fp,
                name: peer_name,
                conn,
            }).await;

            Ok(peer_fp)
        } else {
            // 未信任：发起配对流程，等待 UI 双向输码
            // addr 用于 PairingWaitConsent 事件（局域网场景有地址，中继场景可能为 None）
            let addr_for_event = addr.unwrap_or_else(|| {
                // 中继场景无真实地址，用零地址占位
                std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED), 0)
            });
            let _ = self.event_tx.send(SessionEvent::PairingWaitConsent {
                fingerprint: peer_fp,
                name: addr_for_event.to_string(),
            }).await;

            self.establish_control_flow(conn, peer_fp, own_code, false).await?;

            Ok(peer_fp)
        }
    }

    /// 主动连接到对端。
    /// **仅测试/无指纹场景使用**——生产路径(壳层连接设备)必须走
    /// [`SessionManager::connect_pinned`] 带预期指纹,否则不设防中间人
    /// 替换身份(S1 审计高危)。
    pub async fn connect(&self, addr: std::net::SocketAddr) -> Result<Fingerprint, SessionError> {
        self.connect_inner(addr, None).await
    }

    /// S1: 主动连接到对端,带预期指纹钉扎(TLS 握手期验证,不符即失败)。
    /// expected_fp 来源:发现表(带签名的广播指纹)或中结名册指纹。
    /// 生产入口(壳层 connect 命令 / FFI connect_device)应全部走此方法。
    pub async fn connect_pinned(
        &self,
        addr: std::net::SocketAddr,
        expected_fp: Fingerprint,
    ) -> Result<Fingerprint, SessionError> {
        self.connect_inner(addr, Some(expected_fp)).await
    }

    /// connect/connect_pinned 共用内核:expected None 时无指纹校验。
    async fn connect_inner(
        &self,
        addr: std::net::SocketAddr,
        expected: Option<Fingerprint>,
    ) -> Result<Fingerprint, SessionError> {
        tracing::info!("尝试连接到: {}", addr);

        // 获取或创建 endpoint
        let ep = {
            let ep_guard = self.endpoint.lock().await;
            if let Some(ref ep) = *ep_guard {
                ep.clone()
            } else {
                drop(ep_guard);
                let new_ep = bind_endpoint(0, &self.ctx.identity)?;
                tracing::info!("创建新的 endpoint 用于连接");
                let mut endpoint_lock = self.endpoint.lock().await;
                *endpoint_lock = Some(new_ep.clone());
                new_ep
            }
        };

        // 建立连接（mTLS 握手完成后才知道对端指纹）
        tracing::info!("开始建立连接");
        let (conn, peer_fp) = connect(&ep, addr, &self.ctx.identity, expected).await?;
        tracing::info!("连接已建立，对端指纹: {}", hex::encode(peer_fp));

        // 握手后处理（复用同一套逻辑，确保局域网路径零行为变化）
        self.post_handshake(conn, peer_fp, Some(addr)).await
    }

    /// 接受已建立的连接（从中继 punch 或其他外部来源）。
    /// 语义与 handle_incoming 一致——被 adopt 的一方是**被动方**:
    /// 等对端开 bi 流、被动交换 Hello(对端在中继另一端 connect_peer,
    /// 它才是发起方)。若误用主动方语义(establish_control_flow),
    /// 双方都 open_bi 等 Hello 会死锁。
    pub async fn adopt_connection(&self, conn: quinn::Connection) -> Result<Fingerprint, SessionError> {
        self.handle_incoming(conn).await
    }

    /// 发起方接管已建立的连接(中继远程路径用):
    /// connect_peer 握手完成后调用——本机是发起方,走 post_handshake
    /// (主动开 bi 流+Hello+PairingWaitConsent)。对端在另一侧必须
    /// 走 adopt_connection(被动)。v0.4.0 前壳层对两侧都用 adopt,
    /// 双方互相 accept_bi 死锁 10s 超时——中继远程配对从未真正走通,
    /// 同意门 T8 E2E 首次暴露。
    pub async fn adopt_as_initiator(&self, conn: quinn::Connection) -> Result<Fingerprint, SessionError> {
        // 取对端指纹(mTLS 双向证书,client_endpoint 已附带自签证书)
        let peer_fp = {
            let peer_certs = conn.peer_identity()
                .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok());
            let cert = peer_certs
                .as_ref()
                .and_then(|c| c.first())
                .ok_or(SessionError::NoPeerCert)?;
            fingerprint_of(cert)
        };
        // 中继场景无真实对端地址(session_addr 是中继端口)
        self.post_handshake(conn, peer_fp, None).await
    }

    /// 提交配对码（用户输入对端屏幕展示的码）。
    /// A 侧(Initiator):透传给 B 判定,返回 true 表示"已提交"(挂起语义)。
    /// B 侧(接受方):不应调用(无输码入口),返回 Err。
    pub async fn submit_pair_code(
        &self,
        fingerprint: &Fingerprint,
        peer_code: &str,
    ) -> Result<bool, SessionError> {
        let (ctrl_send, consent) = {
            let sessions = self.sessions.lock().await;
            let session = sessions.get(fingerprint)
                .ok_or_else(|| SessionError::Pairing("会话不存在".into()))?;
            (session.ctrl_send.clone(), session.consent.clone())
        };

        match consent {
            ConsentState::Initiator => {
                // A 侧:直接透传,返回 true(挂起语义)
                // 低危审计修复:5s 超时兜底,防对端半死时 send 无限挂起
                tokio::time::timeout(Duration::from_secs(5),
                    ctrl_send.send(ControlMsg::PairCodeSubmit { code: peer_code.to_string() })
                ).await
                    .map_err(|_| SessionError::Pairing("控制流发送超时".into()))?
                    .map_err(|_| SessionError::Pairing("控制流已关闭".into()))?;
                Ok(true)
            }
            _ => {
                // B 侧(Granted/AwaitingConsent):不应调用
                Err(SessionError::Pairing("本机为接受方,无输码入口".into()))
            }
        }
    }

    /// 获取会话连接
    pub async fn session(&self, fingerprint: &Fingerprint) -> Option<Connection> {
        self.sessions.lock().await.get(fingerprint).map(|s| s.conn.clone())
    }

    /// 断开会话
    pub async fn disconnect(&self, fingerprint: &Fingerprint) {
        let session = self.sessions.lock().await.remove(fingerprint);
        if let Some(session) = session {
            // 尽力发送 Goodbye 后关闭连接(5s 超时兜底,低危审计修复)
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                session.ctrl_send.send(ControlMsg::Goodbye),
            ).await;
            session.conn.close(0u8.into(), b"Goodbye");
        }
    }

    /// v0.2.7 优雅关闭：对全部已建立会话发 Goodbye 并断连。
    /// 对端收到 Goodbye 会立即 SessionDown（而不是等 60s idle 超时），
    /// 其传输任务落 interrupted、UI 即时反馈——"关闭程序要通知对方"。
    /// 传输中的位图持久化由 PartWriter::drop 兜底，不在这里等。
    pub async fn shutdown_all(&self) {
        let drained: Vec<Session> = {
            let mut sessions = self.sessions.lock().await;
            sessions.drain().map(|(_, s)| s).collect()
        };
        let n = drained.len();
        for session in drained {
            // 尽力发送 Goodbye(5s 超时兜底,低危审计修复——关闭路径不等待半死对端)
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                session.ctrl_send.send(ControlMsg::Goodbye),
            ).await;
            session.conn.close(0u8.into(), b"Goodbye");
        }
        if n > 0 {
            tracing::info!("优雅关闭：已向 {} 个对端发送 Goodbye", n);
        }
    }

    /// 向指定会话发送控制消息（T14: RPC 路由器响应）
    pub async fn send_ctrl(&self, fingerprint: &Fingerprint, msg: ControlMsg) -> Result<(), SessionError> {
        let ctrl_send = {
            let sessions = self.sessions.lock().await;
            sessions.get(fingerprint).map(|s| s.ctrl_send.clone())
        };

        if let Some(ctrl_send) = ctrl_send {
            // 低危审计修复:5s 超时兜底,防对端半死时 send 无限挂起
            tokio::time::timeout(Duration::from_secs(5), ctrl_send.send(msg)).await
                .map_err(|_| SessionError::Connection(quinn::ConnectionError::TimedOut))?
                .map_err(|_| SessionError::Connection(quinn::ConnectionError::Reset))?;
            Ok(())
        } else {
            Err(SessionError::Connection(quinn::ConnectionError::Reset))
        }
    }

    /// B(接受方)点同意:随机生成配对码、存哈希、通知 A 可输码。
    /// 返回明文码(仅本机 UI 展示,不得发往对端)。
    pub async fn grant_consent(&self, fingerprint: &Fingerprint) -> Result<String, SessionError> {
        let (code, ctrl_send, conn) = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(fingerprint)
                .ok_or_else(|| SessionError::Pairing("会话不存在".into()))?;
            // 门状态检查:仅 AwaitingConsent 可同意(幂等:Granted 再点返回旧码?
            // 不——码已生成无法取回明文。二次调用直接报错)
            let pending_code = match &session.consent {
                ConsentState::AwaitingConsent { pending_code, .. } => pending_code.clone(),
                _ => return Err(SessionError::Pairing("会话不处于待同意状态".into())),
            };
            let code = crate::pairing::generate_pair_code();
            let code_hash = crate::pairing::hash_code(&code);
            session.pairing = Some(PairingMachine::new(code_hash));
            session.consent = ConsentState::Granted { pending_code };
            (code, session.ctrl_send.clone(), session.conn.clone())
        };

        // 发送 ConsentGrant 到 A 的 ctrl_loop(通过本地 ctrl_loop 转发到网络)
        let peer_name = self.ctx.config.read().await.device_name.clone();
        tracing::info!("grant_consent: 发送 ConsentGrant 到 {}, name: {}", hex::encode(fingerprint), peer_name);
        // 5s 超时兜底,防对端半死时挂起(低危审计修复)
        tokio::time::timeout(Duration::from_secs(5),
            ctrl_send.send(ControlMsg::ConsentGrant { name: peer_name })
        ).await
            .map_err(|_| SessionError::Pairing("控制流发送超时".into()))?
            .map_err(|_| SessionError::Pairing("控制流已关闭".into()))?;

        // 先发 PairingCodeShown 事件给本机 B (UI 展示码),避免暂存码立即完成配对时 UI 先收成功再收亮码
        let _ = self.event_tx.send(SessionEvent::PairingCodeShown {
            fingerprint: *fingerprint,
            own_code: code.clone(),
        }).await;

        // 同意瞬间检查暂存码(A 的码可能先到)
        // try_release_pending 会在 sessions.lock 内部检查并消费 pending_code
        let fp = *fingerprint;
        self.try_release_pending(&fp, &conn).await;

        Ok(code)
    }

    /// B(接受方)拒绝同意:断连并通知双方
    pub async fn deny_consent(&self, fingerprint: &Fingerprint) -> Result<(), SessionError> {
        let ctrl_send = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(fingerprint)
                .ok_or_else(|| SessionError::Pairing("会话不存在".into()))?;
            match session.consent {
                ConsentState::AwaitingConsent { .. } => {}
                _ => return Err(SessionError::Pairing("会话不处于待同意状态".into())),
            }
            session.ctrl_send.clone()
        };

        // 发送 ConsentDeny(让 A 的 ctrl_loop 处理并断连;5s 超时兜底,低危审计修复)
        tokio::time::timeout(Duration::from_secs(5),
            ctrl_send.send(ControlMsg::ConsentDeny)
        ).await
            .map_err(|_| SessionError::Pairing("控制流发送超时".into()))?
            .map_err(|_| SessionError::Pairing("控制流已关闭".into()))?;

        // 发 PairingResult 给本机 B (UI 收尾) - 注意这个会在 ctrl_loop 断开后发送
        let _ = self.event_tx.send(SessionEvent::PairingResult {
            fingerprint: *fingerprint,
            ok: false,
            reason: Some("已拒绝对方连接".into()),
        }).await;

        Ok(())
    }

    /// B(接受方)取消等待(已同意但未完成):断连并通知双方
    pub async fn cancel_wait(&self, fingerprint: &Fingerprint) -> Result<(), SessionError> {
        let ctrl_send = {
            let mut sessions = self.sessions.lock().await;
            let session = sessions.get_mut(fingerprint)
                .ok_or_else(|| SessionError::Pairing("会话不存在".into()))?;
            match session.consent {
                ConsentState::Granted { .. } => {}
                _ => return Err(SessionError::Pairing("会话不处于已同意状态".into())),
            }
            session.ctrl_send.clone()
        };

        // 发送 ConsentCancel(让 A 的 ctrl_loop 处理并断连;5s 超时兜底,低危审计修复)
        tokio::time::timeout(Duration::from_secs(5),
            ctrl_send.send(ControlMsg::ConsentCancel)
        ).await
            .map_err(|_| SessionError::Pairing("控制流发送超时".into()))?
            .map_err(|_| SessionError::Pairing("控制流已关闭".into()))?;

        // 发 PairingResult 给本机 B (UI 收尾)
        let _ = self.event_tx.send(SessionEvent::PairingResult {
            fingerprint: *fingerprint,
            ok: false,
            reason: Some("已结束等待".into()),
        }).await;

        Ok(())
    }

    /// 接受方：等待发起方的控制流并交换 Hello
    async fn handle_incoming(&self, conn: Connection) -> Result<Fingerprint, SessionError> {
        // 获取对端指纹（mTLS 双向证书）
        let peer_fp = {
            let peer_certs = conn.peer_identity()
                .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok());
            let cert = peer_certs
                .as_ref()
                .and_then(|c| c.first())
                .ok_or(SessionError::NoPeerCert)?;
            fingerprint_of(cert)
        };

        let _addr = conn.remote_address();

        // B 侧冷却期检查:对端在 3 次配对码错误后的冷却时间内,直接拒绝连接
        {
            let mut cooldown = self.cooldown.lock().await;
            // 顺手清理已过期项,防表无限增长(低危审计修复)
            cooldown.retain(|_, e| *e > Instant::now());
            if let Some(&expires) = cooldown.get(&peer_fp) {
                let now = Instant::now();
                if now < expires {
                    let remaining = (expires - now).as_secs();
                    drop(cooldown);
                    conn.close(0u8.into(), b"cooldown");
                    return Err(SessionError::Cooldown(remaining));
                }
            }
        }

        let is_trusted = {
            let trust = self.ctx.trust.lock().await;
            trust.is_trusted(&peer_fp)
        };

        let local_fp = self.ctx.identity.fingerprint();
        let own_code = String::new(); // Task 3: 同意门下码在 grant_consent 生成

        // 等待发起方打开双向流
        let (mut send, mut recv) = match timeout(Duration::from_secs(10), conn.accept_bi()).await {
            Ok(r) => r?,
            Err(_) => return Err(SessionError::Connection(quinn::ConnectionError::TimedOut)),
        };

        // 交换 Hello
        let peer_name = match recv_msg(&mut recv).await? {
            ControlMsg::Hello { name, .. } => name,
            _ => return Err(SessionError::Pairing("控制流首消息应为 Hello".into())),
        };
        let hello = ControlMsg::Hello {
            name: self.ctx.config.read().await.device_name.clone(),
            fingerprint: hex::encode(local_fp),
        };
        send_all(&mut send, &hello).await?;

        // Hello 交换后，未信任时发送同意门事件
        if !is_trusted {
            let _ = self.event_tx.send(SessionEvent::PairingConsentNeeded {
                fingerprint: peer_fp,
                name: peer_name.clone(),
            }).await;
        }

        let consent = if is_trusted {
            ConsentState::Initiator
        } else {
            ConsentState::AwaitingConsent {
                deadline: tokio::time::Instant::now() + self.consent_timeout().await,
                pending_code: None,
            }
        };

        self.insert_session_and_spawn(conn.clone(), peer_fp, own_code, is_trusted, peer_name.clone(), send, recv, consent).await;

        if is_trusted {
            let _ = self.event_tx.send(SessionEvent::SessionUp {
                fingerprint: peer_fp,
                name: peer_name,
                conn,
            }).await;
        }

        Ok(peer_fp)
    }

    /// 发起方：打开控制流并交换 Hello，插入会话并启动控制循环
    async fn establish_control_flow(
        &self,
        conn: Connection,
        peer_fp: Fingerprint,
        own_code: String,
        is_trusted: bool,
    ) -> Result<String, SessionError> {
        // 只有发起方打开双向流
        let (mut send, mut recv) = conn.open_bi().await?;

        let local_fp = self.ctx.identity.fingerprint();
        let hello = ControlMsg::Hello {
            name: self.ctx.config.read().await.device_name.clone(),
            fingerprint: hex::encode(local_fp),
        };
        send_all(&mut send, &hello).await?;

        let peer_name = match recv_msg(&mut recv).await? {
            ControlMsg::Hello { name, .. } => name,
            _ => return Err(SessionError::Pairing("控制流首消息应为 Hello".into())),
        };

        let consent = if is_trusted {
            ConsentState::Initiator
        } else {
            ConsentState::Initiator
        };

        self.insert_session_and_spawn(conn, peer_fp, own_code, is_trusted, peer_name.clone(), send, recv, consent).await;

        Ok(peer_name)
    }

    /// 插入会话并启动控制循环（发起方/接受方共用）
    async fn insert_session_and_spawn(
        &self,
        conn: Connection,
        peer_fp: Fingerprint,
        _own_code: String, // Task 3: 同意门下码在 grant_consent 生成,此处不再使用
        is_trusted: bool,
        peer_name: String,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
        consent: ConsentState,
    ) {
        let generation = self.generation_counter.fetch_add(1, Ordering::SeqCst);
        let (ctrl_tx, ctrl_rx) = mpsc::channel(32);
        self.sessions.lock().await.insert(peer_fp, Session {
            conn: conn.clone(),
            conn_id: conn.stable_id(),
            ctrl_send: ctrl_tx,
            pairing: None, // Task 3: 受信路径无 pairing,未信任路径 B 在 grant_consent 时创建
            peer_name: Some(peer_name),
            consent: consent.clone(),
            generation,
        });

        // 门超时驱动(实现注意 7):独立 spawn 任务
        if let ConsentState::AwaitingConsent { deadline, .. } = consent {
            let sm = self.clone();
            tokio::spawn(async move {
                tokio::time::sleep_until(deadline).await;
                // M-B1(审查修复): 校验代次+取 conn+删条目在同一锁临界区内原子完成。
                // 此前"校验通过 → drop 锁 → sm.disconnect 重加锁无条件删"的间隙里,
                // 新一代连接插入会被 disconnect 误杀并收到错误的 PairingResult。
                // 现不走 sm.disconnect(它按 fp 无条件删表),而是锁内取 conn、
                // 条件删表,锁外只关连接+发事件——关连接会令该代 ctrl_loop 退出,
                // 其收尾代次校验发现条目已删,自然跳过二次 remove。
                let conn_to_close = {
                    let mut sessions = sm.sessions.lock().await;
                    match sessions.get_mut(&peer_fp) {
                        Some(session) if session.generation == generation
                            && matches!(session.consent, ConsentState::AwaitingConsent { .. }) => {
                            // 还没同意且仍是本代:超时断连(不记冷却)
                            let conn = session.conn.clone();
                            sessions.remove(&peer_fp);
                            conn
                        }
                        // 代次不符(已被新连接覆盖)或已同意/已完成:不动作
                        _ => return,
                    }
                };
                conn_to_close.close(0u8.into(), b"consent timeout");
                let _ = sm.event_tx.send(SessionEvent::PairingResult {
                    fingerprint: peer_fp,
                    ok: false,
                    reason: Some("同意超时".into()),
                }).await;
            });
        }

        // M3b FR2:探测响应端——每会话一条 accept_bi 循环,只处理探测流。
        // 探测消息(Ping/Pong/ProbeReq/ProbeResp)只走独立探测 bi 流,绝不进
        // ctrl 流:老版本对端无此消费者,探测方超时即判不支持,老端连接无损
        // (wire 兼容定案与权衡见 routing::probe 模块注释)。
        let conn_for_probe = conn.clone();
        let sm = self.clone();
        tokio::spawn(async move {
            sm.ctrl_loop(conn, peer_fp, send, recv, ctrl_rx).await;
        });
        tokio::spawn(crate::routing::probe::serve_probe_streams(conn_for_probe));
    }

    /// 每会话控制循环：外发消息经 mpsc 转发到控制流，入站消息按类型分发。
    /// 配对完成判定在两端独立进行，任一端 failed_out 即断连并记冷却。
    ///
    /// 取消安全：入站读取必须用 RecvStream::read（单次完成、可安全取消）+
    /// 累积缓冲增量解帧。绝不能在 select! 里 await recv_msg——其内部 read_exact
    /// 被取消时已消费的字节会随 future 一起丢弃，控制流从此永久失步
    /// （v0.1.5 实机现象：浏览正常，下载静默超时）。
    async fn ctrl_loop(
        self,
        conn: Connection,
        peer_fp: Fingerprint,
        mut send: quinn::SendStream,
        mut recv: quinn::RecvStream,
        mut ctrl_rx: mpsc::Receiver<ControlMsg>,
    ) {
        let mut failed = false;
        let mut terminal_result_sent = false;
        let mut inbuf: Vec<u8> = Vec::with_capacity(4096);
        let mut chunk = [0u8; 16 * 1024];
        'outer: loop {
            tokio::select! {
                // 外发：submit_pair_code / disconnect 等经 mpsc 投递
                msg = ctrl_rx.recv() => {
                    match msg {
                        Some(msg) => {
                            if send_all(&mut send, &msg).await.is_err() {
                                break 'outer;
                            }
                        }
                        None => { /* 发送端已全部释放，继续等待对端消息 */ }
                    }
                }
                // 入站数据：read 单次完成、可安全取消；消息边界由 inbuf 维护
                result = recv.read(&mut chunk) => {
                    match result {
                        Ok(Some(n)) => {
                            inbuf.extend_from_slice(&chunk[..n]);
                            while let Some(msg) = match take_msg(&mut inbuf) {
                                Ok(opt) => opt,
                                Err(_) => {
                                    tracing::warn!("控制流消息解码失败，断开会话");
                                    break 'outer;
                                }
                            } {
                                match msg {
                                    ControlMsg::Hello { .. } => {
                                        tracing::debug!("收到重复 Hello");
                                    }
                                    ControlMsg::PairCodeSubmit { code } => {
                                        // B 侧收到 A 提交的码,按 ConsentState 分派
                                        tracing::info!("收到 PairCodeSubmit {}", hex::encode(peer_fp));
                                        let outcome = {
                                            let mut sessions = self.sessions.lock().await;
                                            match sessions.get_mut(&peer_fp) {
                                                Some(session) => {
                                                    tracing::info!("会话状态: consent={:?}", session.consent);
                                                    match &mut session.consent {
                                                        ConsentState::Initiator => {
                                                            // A 侧不应收到 PairCodeSubmit(除非回环),忽略
                                                            tracing::debug!("Initiator 收到 PairCodeSubmit,忽略");
                                                            PairOutcome::NotPairing
                                                        }
                                                        ConsentState::AwaitingConsent { pending_code, .. } => {
                                                            // A 的码先到:暂存,不发任何回包
                                                            *pending_code = Some(code.clone());
                                                            tracing::debug!("AwaitingConsent 时收到码,暂存");
                                                            PairOutcome::NotPairing
                                                        }
                                                        ConsentState::Granted { .. } => {
                                                            // B 已同意,正常判定码
                                                            // B 已同意,正常判定码
                                                            tracing::info!("Granted 状态,判定码");
                                                            session.pairing.as_mut().map(|pm| {
                                                                if pm.submit_remote(&code) {
                                                                    tracing::info!("码匹配");
                                                                    PairOutcome::Matched
                                                                } else if pm.failed_out() {
                                                                    tracing::info!("码错误达到 3 次");
                                                                    PairOutcome::FailedOut
                                                                } else {
                                                                    tracing::info!("码不匹配");
                                                                    PairOutcome::Mismatch
                                                                }
                                                            }).unwrap_or(PairOutcome::NotPairing)
                                                        }
                                                    }
                                                }
                                                None => {
                                                    tracing::warn!("会话不存在");
                                                    PairOutcome::NotPairing
                                                }
                                            }
                                        };
                                        match outcome {
                                            PairOutcome::Matched => {
                                                terminal_result_sent = true;
                                                // 先回包再本地收尾:A 侧(输码方)依赖 PairResult 解除输码等待;
                                                // complete_pairing(写信任+SessionUp)耗时或连接竞争不应推迟回包
                                                let _ = send_all(&mut send, &ControlMsg::PairResult { ok: true }).await;
                                                if self.complete_pairing(&peer_fp, &conn).await {
                                                    let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                        fingerprint: peer_fp,
                                                        ok: true,
                                                        reason: None,
                                                    }).await;
                                                }
                                            }
                                            PairOutcome::FailedOut => {
                                                failed = true;
                                                terminal_result_sent = true;
                                                self.record_cooldown(peer_fp).await;
                                                let _ = send_all(&mut send, &ControlMsg::PairResult { ok: false }).await;
                                                let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                    fingerprint: peer_fp,
                                                    ok: false,
                                                    reason: Some("配对码错误超过 3 次".to_string()),
                                                }).await;
                                                break 'outer;
                                            }
                                            PairOutcome::Mismatch => {
                                                let _ = send_all(&mut send, &ControlMsg::PairResult { ok: false }).await;
                                            }
                                            PairOutcome::NotPairing => {
                                                tracing::debug!("非配对会话收到 PairCodeSubmit，忽略");
                                            }
                                        }
                                    }
                                    ControlMsg::PairResult { ok } => {
                                        if ok {
                                            // 对端确认码匹配：本方配对完成。仅首次
                                            // 完成时发事件（本方可能在 PairCodeSubmit
                                            // 路径已经完成过，见上）。
                                            terminal_result_sent = true;
                                            if self.complete_pairing(&peer_fp, &conn).await {
                                                let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                    fingerprint: peer_fp,
                                                    ok: true,
                                                    reason: None,
                                                }).await;
                                            }
                                        } else {
                                            // P2 打磨:wire PairResult{ok:false} 只可能出自 B 的
                                            // 错码判定路径(Mismatch/FailedOut——真拒绝走 ConsentDeny,
                                            // 另有"对方拒绝连接"文案)。旧文案"对端拒绝"令发起方 UI
                                            // 把可重输的错码误判为终态拒绝,重输流程断裂。
                                            terminal_result_sent = true;
                                            let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                fingerprint: peer_fp,
                                                ok: false,
                                                reason: Some("配对码不匹配".to_string()),
                                            }).await;
                                        }
                                    }
                                    ControlMsg::ListReq { .. } |
                                    ControlMsg::SharesReq { .. } |
                                    ControlMsg::MetaReq { .. } |
                                    ControlMsg::FetchReq { .. } |
                                    ControlMsg::OfferReq { .. } |
                                    ControlMsg::BitmapReq { .. } |
                                    ControlMsg::TransferCtl { .. } |
                                    ControlMsg::RecvProgress { .. } |
                                    ControlMsg::RecvAck { .. } => {
                                        // T14: 转发到入站控制通道供 RPC 路由器处理。
                                        // RecvAck 归此类：它的消费者是发送方路由器
                                        // （据此发 SourceDone/清理任务），而非传输等待者
                                        if let Err(_) = self.inbound_ctrl.try_send((peer_fp, msg.clone())) {
                                            tracing::warn!("入站控制通道已满，丢弃消息: {:?}", msg);
                                        }
                                    }
                                    ControlMsg::SharesResp { .. } |
                                    ControlMsg::ListResp { .. } |
                                    ControlMsg::MetaResp { .. } |
                                    ControlMsg::ShareOpResult { .. } => {
                                        // M-B5: 按请求关联 ID 多路分发到等待中的 RPC 调用方。
                                        // 找不到等待者(老对端无 msg_id / 已超时撤销)则丢弃
                                        let id = resp_msg_id(&msg);
                                        let waiter = self.pending_rpcs.lock().ok().and_then(|mut m| m.remove(&id));
                                        match waiter {
                                            Some(tx) => {
                                                if tx.send((peer_fp, msg.clone())).is_err() {
                                                    tracing::debug!("RPC 等待者已放弃(msg_id={}),响应丢弃", id);
                                                }
                                            }
                                            None => {
                                                tracing::warn!("收到无主响应(msg_id={}),丢弃: {:?}", id, msg);
                                            }
                                        }
                                    }
                                    ControlMsg::OfferResp { .. } |
                                    ControlMsg::BitmapResp { .. } |
                                    ControlMsg::JobDone { .. } |
                                    ControlMsg::JobFailed { .. } => {
                                        // M-B5: 推送协商类信号走广播通道——多个推送在途时
                                        // 各等待者订阅过滤,不再独占一次性响应通道
                                        let _ = self.push_signals.send((peer_fp, msg.clone()));
                                    }
                                    ControlMsg::SharesChanged { .. } => {
                                        // watchdog 推送：走独立广播通道，常驻消费者
                                        // （Tauri 壳事件泵）随时可读，不受传输排队影响
                                        let _ = self.inbound_notify.send((peer_fp, msg.clone()));
                                    }
                                    ControlMsg::ConsentGrant { name } => {
                                        // A 侧收到 B 的同意:发 PairingCodeEntry 事件
                                        tracing::info!("收到 ConsentGrant from {}, name: {}", hex::encode(peer_fp), name);
                                        let send_result = self.event_tx.send(SessionEvent::PairingCodeEntry {
                                            fingerprint: peer_fp,
                                            name,
                                        }).await;
                                        tracing::info!("发送 PairingCodeEntry 事件结果: {:?}", send_result);
                                    }
                                    ControlMsg::ConsentDeny => {
                                        // A 侧收到 B 的拒绝:发 PairingResult 并断连
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方拒绝连接".into()),
                                        }).await;
                                        break 'outer;
                                    }
                                    ControlMsg::ConsentCancel => {
                                        // A 侧收到 B 取消等待:发 PairingResult 并断连
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方已结束等待".into()),
                                        }).await;
                                        break 'outer;
                                    }
                                    // v0.6.0 远程文件操作消息路由(安卓 T2)
                                    ControlMsg::ShareRename { .. } | ControlMsg::ShareDelete { .. } | ControlMsg::ShareMkdir { .. } => {
                                        // 请求类消息转发到 RPC 路由器
                                        if let Err(_) = self.inbound_ctrl.try_send((peer_fp, msg.clone())) {
                                            tracing::warn!("入站控制通道已满,丢弃消息: {:?}", msg);
                                        }
                                    }
                                    ControlMsg::ShareOpResult { .. } => {
                                        // M-B5: ShareOpResult 已在上面按 msg_id 分发(此分支不可达,防御保留)
                                    }
                                    ControlMsg::Goodbye => {
                                        tracing::info!("对端发送 Goodbye");
                                        break 'outer;
                                    }
                                    // M3b:探测类消息只应出现在独立探测 bi 流上(routing::probe)。
                                    // ctrl 流收到(第三方实现误投/异常)一律忽略——不断连不回包:
                                    // 新变体若按未知变体断连会伤及混版本组网(老 ctrl_loop 遇
                                    // 未知变体断连是他们的事,本端能容忍就多一分互通)。
                                    ControlMsg::Ping { .. }
                                    | ControlMsg::Pong { .. }
                                    | ControlMsg::ProbeReq { .. }
                                    | ControlMsg::ProbeResp { .. } => {
                                        tracing::debug!("ctrl 流收到探测类消息(应走探测流),忽略");
                                    }
                                    ControlMsg::TrustBroken => {
                                        // M3a FR6:对端移除了对本机的信任。定案语义=双盲对称重配:
                                        // 本端同步删除信任条目(徽章即时降级为待配对),再发
                                        // TrustBroken 事件供壳层 toast;随后 break 走 Goodbye
                                        // 同款收尾(清会话表 + SessionDown),重配对由用户发起。
                                        tracing::info!("对端发送 TrustBroken:已移除对本机的信任,本端同步降级并断开");
                                        {
                                            let mut trust = self.ctx.trust.lock().await;
                                            if trust.remove(&peer_fp) {
                                                if let Err(e) = trust.save() {
                                                    tracing::warn!("TrustBroken 后信任表落盘失败: {}", e);
                                                }
                                            }
                                        }
                                        let peer_name = self.sessions.lock().await
                                            .get(&peer_fp).and_then(|s| s.peer_name.clone());
                                        let _ = self.event_tx.send(SessionEvent::TrustBroken {
                                            fingerprint: peer_fp,
                                            peer_name,
                                        }).await;
                                        break 'outer;
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            tracing::debug!("控制流读取结束,处理剩余缓冲消息");
                            // 处理缓冲区中剩余的消息,避免遗漏
                            'drain: loop {
                                let msg = match take_msg(&mut inbuf) {
                                    Ok(Some(msg)) => msg,
                                    Ok(None) => break 'drain,
                                    Err(_) => {
                                        tracing::warn!("剩余缓冲消息解码失败");
                                        break;
                                    }
                                };
                                match msg {
                                    ControlMsg::PairResult { ok } => {
                                        if ok {
                                            terminal_result_sent = true;
                                            if self.complete_pairing(&peer_fp, &conn).await {
                                                let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                    fingerprint: peer_fp,
                                                    ok: true,
                                                    reason: None,
                                                }).await;
                                            }
                                        } else {
                                            terminal_result_sent = true;
                                            // 同上:PairResult{ok:false}=错码判定,非对端拒绝(P2 打磨)
                                            let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                fingerprint: peer_fp,
                                                ok: false,
                                                reason: Some("配对码不匹配".to_string()),
                                            }).await;
                                        }
                                    }
                                    ControlMsg::ConsentDeny => {
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方拒绝连接".into()),
                                        }).await;
                                    }
                                    ControlMsg::ConsentCancel => {
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方已结束等待".into()),
                                        }).await;
                                    }
                                    _ => {}
                                }
                            }
                            break 'outer;
                        }
                        Err(_) => {
                            tracing::debug!("控制流读取错误,处理剩余缓冲消息");
                            // 处理缓冲区中剩余的消息,避免遗漏
                            'drain_err: loop {
                                let msg = match take_msg(&mut inbuf) {
                                    Ok(Some(msg)) => msg,
                                    Ok(None) => break 'drain_err,
                                    Err(_) => {
                                        tracing::warn!("剩余缓冲消息解码失败");
                                        break;
                                    }
                                };
                                match msg {
                                    ControlMsg::PairResult { ok } => {
                                        if ok {
                                            terminal_result_sent = true;
                                            if self.complete_pairing(&peer_fp, &conn).await {
                                                let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                    fingerprint: peer_fp,
                                                    ok: true,
                                                    reason: None,
                                                }).await;
                                            }
                                        } else {
                                            terminal_result_sent = true;
                                            // 同上:PairResult{ok:false}=错码判定,非对端拒绝(P2 打磨)
                                            let _ = self.event_tx.send(SessionEvent::PairingResult {
                                                fingerprint: peer_fp,
                                                ok: false,
                                                reason: Some("配对码不匹配".to_string()),
                                            }).await;
                                        }
                                    }
                                    ControlMsg::ConsentDeny => {
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方拒绝连接".into()),
                                        }).await;
                                    }
                                    ControlMsg::ConsentCancel => {
                                        terminal_result_sent = true;
                                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                                            fingerprint: peer_fp,
                                            ok: false,
                                            reason: Some("对方已结束等待".into()),
                                        }).await;
                                    }
                                    _ => {}
                                }
                            }
                            break 'outer;
                        }
                    }
                }
            }
        }

        // 会话结束：清理并通知
        let (consent, is_trusted_before_cleanup) = {
            let trust = self.ctx.trust.lock().await;
            let sessions = self.sessions.lock().await;
            sessions.get(&peer_fp).map(|s| {
                (s.consent.clone(), trust.is_trusted(&peer_fp))
            }).unwrap_or((ConsentState::Initiator, false))
        };
        // M-B1 + T6-FR6: 连接归属校验(以 quinn stable_id 判定,精确无并发歧义)
        // ——仅当表内条目仍是本连接时才删除;已被新一代连接覆盖或被
        // disconnect 显式移除(表内无条目)时跳过 remove
        let my_conn_id = conn.stable_id();
        let mut sessions = self.sessions.lock().await;
        let superseded = sessions.get(&peer_fp)
            .map(|s| s.conn_id != my_conn_id)
            .unwrap_or(false);
        if superseded {
            tracing::debug!("ctrl_loop 退出:会话已被新一代连接覆盖,跳过 remove");
        } else {
            sessions.remove(&peer_fp);
        }
        drop(sessions);
        // T6-FR6(伪连接根因修复):被新一代覆盖的僵尸 ctrl_loop 退出必须静默。
        // 同一指纹重连后,旧连接的 ctrl_loop 仍存活(QUIC 双向保活,旧连接
        // 不会自行死亡);其随旧连接死亡退出时,表内已是新一代会话——若仍发
        // SessionDown,壳层 connected_fps/UI 会在会话实际健康时显示"已断开"
        // (2026-09-08 真机取证:重连 churn 下僵尸连接死亡触发假断线,诱发
        // "伪连接"误报与连锁手动重连)。disconnect 路径依赖本函数在
        // "表内已无条目"时通知 UI,该语义保留(此处仅屏蔽"表内是新条目")。
        if superseded {
            if failed {
                conn.close(0u8.into(), b"Pairing failed");
            }
            return;
        }
        let _ = self.event_tx.send(SessionEvent::SessionDown { fingerprint: peer_fp }).await;
        if failed {
            conn.close(0u8.into(), b"Pairing failed");
        }

        // A 侧断连感知(实现注意 8):若本方 consent==Initiator 且配对未完成
        // (即尚未信任对方) → 发 PairingResult{ok:false, "连接已断开"}
        // 但若已在 ctrl_loop 内发送过终止性结果(Deny/Cancel/FailedOut/Matched),则不再发送
        if matches!(consent, ConsentState::Initiator) && !is_trusted_before_cleanup && !terminal_result_sent {
            let _ = self.event_tx.send(SessionEvent::PairingResult {
                fingerprint: peer_fp,
                ok: false,
                reason: Some("连接已断开".into()),
            }).await;
        }
    }

    /// 尝试释放暂存的码(B 同意时调用)
    async fn try_release_pending(&self, fp: &Fingerprint, conn: &Connection) {
        let pending_code = {
            let mut sessions = self.sessions.lock().await;
            match sessions.get_mut(fp) {
                Some(session) => {
                    if let ConsentState::Granted { ref mut pending_code } = session.consent {
                        pending_code.take()
                    } else {
                        return;
                    }
                }
                None => return,
            }
        };

        if let Some(code) = pending_code {
            let outcome = self.judge_code(fp, &code).await;
            match outcome {
                PairOutcome::Matched => {
                    if self.complete_pairing(fp, conn).await {
                        let _ = self.event_tx.send(SessionEvent::PairingResult {
                            fingerprint: *fp,
                            ok: true,
                            reason: None,
                        }).await;
                    }
                }
                PairOutcome::FailedOut => {
                    self.record_cooldown(*fp).await;
                    let _ = self.event_tx.send(SessionEvent::PairingResult {
                        fingerprint: *fp,
                        ok: false,
                        reason: Some("配对码错误超过 3 次".to_string()),
                    }).await;
                    self.disconnect(fp).await;
                }
                PairOutcome::Mismatch => {
                    let _ = self.event_tx.send(SessionEvent::PairingResult {
                        fingerprint: *fp,
                        ok: false,
                        reason: Some("配对码不匹配".to_string()),
                    }).await;
                }
                PairOutcome::NotPairing => {}
            }
        }
    }

    /// 判定码(提取得私有判定逻辑)
    async fn judge_code(&self, fp: &Fingerprint, code: &str) -> PairOutcome {
        let mut sessions = self.sessions.lock().await;
        match sessions.get_mut(fp).and_then(|s| s.pairing.as_mut()) {
            Some(pm) => {
                if pm.submit_remote(code) {
                    PairOutcome::Matched
                } else if pm.failed_out() {
                    PairOutcome::FailedOut
                } else {
                    PairOutcome::Mismatch
                }
            }
            None => PairOutcome::NotPairing,
        }
    }

    /// 配对成功：清空配对状态、写入互信并发 SessionUp（幂等，仅首次生效）。
    /// 返回是否为首次完成——调用方据此决定是否发 PairingResult 事件，
    /// 避免双侧同时输码的竞态下重复弹出。
    async fn complete_pairing(&self, peer_fp: &Fingerprint, conn: &Connection) -> bool {
        let Some(peer_name) = self.complete_pairing_core(peer_fp).await else {
            return false;
        };

        let _ = self.event_tx.send(SessionEvent::SessionUp {
            fingerprint: *peer_fp,
            name: peer_name,
            conn: conn.clone(),
        }).await;
        true
    }

    /// complete_pairing 内核:清配对态+写信任,返回对端名(已信任时 None,
    /// 即"非首次完成")。BUG03 修复:码验证为真 = 配对成功的最高事实,
    /// 会话条目即使被并发清掉(连接抖动/新一代覆盖,中继 60s 会话死亡
    /// 场景实测发生)信任也必须写入——否则对端写信任、本端没写,形成
    /// 单向信任:对方能推我、我推对方被拒"未配对"。peer_name 拿不到时
    /// 用指纹短 hex 兜底(显示名可后补,信任不能缺席)。
    async fn complete_pairing_core(&self, peer_fp: &Fingerprint) -> Option<String> {
        // 已信任:非首次完成(幂等)
        if self.ctx.trust.lock().await.is_trusted(peer_fp) {
            return None;
        }

        let peer_name = {
            let mut sessions = self.sessions.lock().await;
            match sessions.get_mut(peer_fp) {
                Some(session) => {
                    // 接受方 B: 有 pairing machine, 完成时清空
                    // 发起方 A: 无 pairing machine, 通过 is_trusted 检查幂等
                    session.pairing.take();
                    session.peer_name.clone().unwrap_or_default()
                }
                None => {
                    tracing::warn!(
                        "配对完成时会话条目已丢失,兜底写信任: peer={}",
                        hex::encode(&peer_fp[..8])
                    );
                    format!("device-{}", hex::encode(&peer_fp[..4]))
                }
            }
        };

        let paired_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        {
            let mut trust = self.ctx.trust.lock().await;
            trust.upsert(TrustedPeer {
                fingerprint: *peer_fp,
                name: peer_name.clone(),
                alias: String::new(),
                paired_at,
                perms: Perms::default(),
            });
            if let Err(e) = trust.save() {
                tracing::warn!("信任列表保存失败: {}", e);
            }
        }
        Some(peer_name)
    }

    /// 测试专用:直接调 complete_pairing 内核(生产路径经 ctrl_loop 的
    /// PairResult{ok:true}/Matched 分支,带真实 conn 发 SessionUp;
    /// 测试只验证信任写入,无需连接)
    #[cfg(test)]
    pub async fn complete_pairing_for_test(&self, peer_fp: &Fingerprint) -> bool {
        self.complete_pairing_core(peer_fp).await.is_some()
    }

    /// 记录指纹冷却期（3 次配对码失败后 COOLDOWN_SECS 内拒绝连接）
    async fn record_cooldown(&self, peer_fp: Fingerprint) {        let mut cooldown = self.cooldown.lock().await;
        // 顺手清理已过期项,防表无限增长(低危审计修复)
        cooldown.retain(|_, e| *e > Instant::now());
        cooldown.insert(
            peer_fp,
            Instant::now() + Duration::from_secs(COOLDOWN_SECS),
        );
    }
}

impl Clone for SessionManager {
    fn clone(&self) -> Self {
        SessionManager {
            ctx: self.ctx.clone(),
            sessions: self.sessions.clone(),
            generation_counter: self.generation_counter.clone(),
            event_tx: self.event_tx.clone(),
            cooldown: self.cooldown.clone(),
            endpoint: self.endpoint.clone(),
            inbound_ctrl: self.inbound_ctrl.clone(),
            inbound_ctrl_rx: self.inbound_ctrl_rx.clone(),
            inbound_notify: self.inbound_notify.clone(),
            push_signals: self.push_signals.clone(),
            pending_rpcs: self.pending_rpcs.clone(),
        }
    }
}

/// 发送控制消息（带长度前缀）
async fn send_all(stream: &mut (impl AsyncWrite + Unpin), msg: &ControlMsg) -> Result<(), SessionError> {
    let data = encode_control(msg);
    stream.write_all(&data).await?;
    stream.flush().await?;
    Ok(())
}

/// 接收控制消息（4 字节 u32BE 长度前缀 + JSON 消息体）
/// 从累积缓冲解出一条完整控制消息（配合 ctrl_loop 的取消安全读取）。
/// Ok(None) = 数据不足一条，继续等；Err = 长度前缀或消息体非法，调用方应断开会话。
fn take_msg(inbuf: &mut Vec<u8>) -> Result<Option<ControlMsg>, SessionError> {
    if inbuf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([inbuf[0], inbuf[1], inbuf[2], inbuf[3]]) as usize;
    if len == 0 || len > 4 * 1024 * 1024 {
        return Err(SessionError::Connection(quinn::ConnectionError::Reset));
    }
    if inbuf.len() < 4 + len {
        return Ok(None);
    }
    let body = inbuf[4..4 + len].to_vec();
    inbuf.drain(..4 + len);
    decode_control_body(&body)
        .map(Some)
        .map_err(|_| SessionError::Connection(quinn::ConnectionError::Reset))
}

async fn recv_msg(stream: &mut (impl AsyncReadExt + Unpin)) -> Result<ControlMsg, SessionError> {
    // 读取长度前缀
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf).await?;

    let len = u32::from_be_bytes(len_buf) as usize;
    if len == 0 || len > 4 * 1024 * 1024 {
        return Err(SessionError::Connection(quinn::ConnectionError::Reset));
    }

    // 读取消息体（前缀已剥离，用 body 解码）
    let mut msg_buf = vec![0u8; len];
    stream.read_exact(&mut msg_buf).await?;

    decode_control_body(&msg_buf)
        .map_err(|_| SessionError::Connection(quinn::ConnectionError::Reset))
}

// ============ 测试 ============

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{setup_ctx, start_listener, init_tracing};
    use tempfile::tempdir;
    use std::time::{Duration, SystemTime};
    use tokio::time::{timeout, timeout_at};

    /// BUG03 回归:配对码判定成功(B 判 Matched 回 PairResult{ok:true})时,
    /// 若本机会话条目已被并发清掉(连接抖动/新一代覆盖),complete_pairing
    /// 仍必须写入信任——否则对端写信任、本端没写,形成单向信任:
    /// 对端能推我,我推对端被拒"未配对"。
    #[tokio::test]
    async fn pairing_writes_trust_even_if_session_entry_replaced() {
        init_tracing();
        let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预置:乙信任甲(乙侧配对已完成的姿态);甲不信任乙
        {
            let mut trust_b = ctx_b.trust.lock().await;
            trust_b.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms::default(),
            });
        }

        // 建立真实会话(甲主动连乙监听器),甲侧拿到会话条目与 conn
        let b_addr = start_listener(&sm_b).await;
        let peer_fp = timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await
            .expect("连接应在超时前完成").expect("连接应成功");
        assert_eq!(peer_fp, fp_b);

        // 模拟连接抖动:把甲侧会话条目清掉(complete_pairing 的
        // sessions.get(peer_fp) 命中 None 分支——现实里由 60s 会话死亡/
        // 新一代覆盖触发)
        sm_a.sessions.lock().await.remove(&fp_b);

        // 甲收到乙的 PairResult{ok:true}(码已在乙侧验证通过):
        // 直接调 complete_pairing——码验证为真,信任必须写入
        let conn = sm_a.session(&fp_b).await;
        // 会话条目已清,conn 拿不到:用 dummy 连接不可行——
        // 但真实路径里 ctrl_loop 持有 conn,不受表清除影响。
        // 这里直接验证:complete_pairing 在无表条目时不再静默 false,
        // 而是兜底写信任。为可测,用 pub 接口 submit 侧验证:
        drop(conn);
        let first = sm_a.complete_pairing_for_test(&fp_b).await;
        assert!(first, "码验证成功后即使会话表条目丢失,配对完成(信任写入)必须生效");

        // 信任表必须已有乙
        let trusted = ctx_a.trust.lock().await.is_trusted(&fp_b);
        assert!(trusted, "甲的信任表必须写入乙(双向信任闭合)");
    }

    /// T6-FR6 回归:僵尸 ctrl_loop 退出不得误发 SessionDown("伪连接"根因)。
    /// 甲对乙连续三次建连(代次 0/1/2),乙侧留下 2 个僵尸 ctrl_loop;随后
    /// 只关 gen0 连接(建连时留存的句柄)——乙的 gen0 僵尸退出,表内条目
    /// 属于 gen2(不同 stable_id),必须静默:乙侧不得收到任何 SessionDown,
    /// 且 gen2 会话仍健康。修复前此处会收到 1 条假 SessionDown,UI 在
    /// 会话实际存活时显示"已断开"(伪连接)。
    #[tokio::test]
    async fn zombie_ctrl_loop_exit_does_not_emit_session_down() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预置互信
        {
            let mut trust_a = _ctx_a.trust.lock().await;
            trust_a.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms::default(),
            });
            trust_a.save().unwrap();
            let mut trust_b = _ctx_b.trust.lock().await;
            trust_b.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
                paired_at: 1000,
                perms: Perms::default(),
            });
            trust_b.save().unwrap();
        }

        let b_addr = start_listener(&sm_b).await;

        // gen0:留存连接句柄(此后表内条目会被 gen1/gen2 覆盖,句柄仍可关连接)
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("gen0 连接超时").expect("gen0 连接失败");
        let conn0 = sm_a.session(&fp_b).await.expect("gen0 会话应存在");
        // gen1/gen2:重连 churn,gen0 在双端沦为僵尸 ctrl_loop
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("gen1 连接超时").expect("gen1 连接失败");
        timeout(Duration::from_secs(5), sm_a.connect(b_addr))
            .await.expect("gen2 连接超时").expect("gen2 连接失败");

        // 双端各应收到 3 条 SessionUp(每次建连一条);排空
        for _ in 0..3 {
            match timeout(Duration::from_secs(3), ev_b.recv()).await {
                Ok(Some(SessionEvent::SessionUp { .. })) => {}
                other => panic!("乙侧事件应为 SessionUp,实际 {:?}", other),
            }
        }
        for _ in 0..3 {
            match timeout(Duration::from_secs(3), ev_a.recv()).await {
                Ok(Some(SessionEvent::SessionUp { .. })) => {}
                other => panic!("甲侧事件异常: {:?}", other),
            }
        }

        // 只关 gen0 连接 → 双端 gen0 僵尸 ctrl_loop 退出。
        // 修复点:僵尸退出时表内条目(gen2)不属于它,必须不发 SessionDown。
        conn0.close(0u8.into(), b"t6-kill-gen0");

        // 2s 内双端都不得收到任何事件(尤其 SessionDown)
        match timeout(Duration::from_secs(2), ev_b.recv()).await {
            Err(_) => {} // 超时=无事件=正确
            other => panic!("乙侧不应收到事件(僵尸退出须静默),实际 {:?}", other),
        }
        match timeout(Duration::from_secs(2), ev_a.recv()).await {
            Err(_) => {}
            other => panic!("甲侧不应收到事件(僵尸退出须静默),实际 {:?}", other),
        }

        // gen2 会话必须仍然健康:双端表内都还有对端条目
        assert!(sm_a.session(&fp_b).await.is_some(), "甲侧 gen2 会话应存活");
        assert!(sm_b.session(&fp_a).await.is_some(), "乙侧 gen2 会话应存活");
    }

    /// Step 1 测试 2: 预置互信后，重连应该静默成功
    #[tokio::test]
    async fn trusted_reconnect_is_silent() {        init_tracing();
        // 预置互信
        let (sm_a, mut ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 手动添加到信任列表
        {
            let mut trust_a = ctx_a.trust.lock().await;
            trust_a.upsert(TrustedPeer {
                fingerprint: fp_b,
                name: "乙".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms::default(),
            });
            trust_a.save().unwrap();
        }

        {
            let mut trust_b = ctx_b.trust.lock().await;
            trust_b.upsert(TrustedPeer {
                fingerprint: fp_a,
                name: "甲".to_string(),
                alias: String::new(),
            paired_at: 1000,
                perms: Perms::default(),
            });
            trust_b.save().unwrap();
        }

        let b_addr = start_listener(&sm_b).await;

        // connect 应该直接返回成功，不应该有 PairingRequested
        let result = timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await;
        assert!(result.is_ok(), "连接应在超时前完成");

        let peer_fp = result.unwrap().unwrap();
        assert_eq!(peer_fp, fp_b);

        // 应该直接收到 SessionUp
        match timeout(Duration::from_secs(1), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::SessionUp { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b);
            }
            _other => {
                panic!("已信任设备应直接收到 SessionUp");
            }
        }
    }

    /// 测试回环连接能正确交换指纹（C3: 两侧都必须拿到对方指纹）
    #[tokio::test]
    async fn loopback_connect_yields_peer_fingerprints() {
        let a = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
        let a_fp = a.fingerprint();
        let b = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
        let b_fp = b.fingerprint();
        let ep_a = bind_endpoint(0, &a).unwrap();
        let ep_b = bind_endpoint(0, &b).unwrap();

        let b_port = ep_b.local_addr().unwrap().port();
        let b_addr = SocketAddr::new("127.0.0.1".parse().unwrap(), b_port);

        // 服务端任务（也需要获取客户端指纹）
        let server_handle = tokio::spawn({
            let ep_b = ep_b.clone();
            async move {
                if let Some(incoming) = ep_b.accept().await {
                    let conn = incoming.await.unwrap();
                    // C3: 服务端也能获取客户端指纹
                    let peer_certs = conn.peer_identity()
                        .and_then(|any| any.downcast::<Vec<CertificateDer<'static>>>().ok());
                    assert!(peer_certs.is_some(), "服务端应能获取客户端证书");
                    let cert = peer_certs.as_ref().unwrap().first().unwrap();
                    let client_fp = fingerprint_of(cert);
                    assert_eq!(client_fp, a_fp, "服务端获取的客户端指纹应匹配");
                }
                b_fp
            }
        });

        let (conn, peer_fp) = connect(&ep_a, b_addr, &a, None).await.unwrap();
        assert_eq!(peer_fp, b_fp);
        let _ = conn;

        server_handle.await.unwrap();
    }

    /// 测试错误的期望指纹会导致握手失败
    #[tokio::test]
    async fn wrong_expected_fingerprint_fails_handshake() {
        let a = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
        let b = Identity::load_or_create(&tempdir().unwrap().path()).unwrap();
        let ep_a = bind_endpoint(0, &a).unwrap();
        let ep_b = bind_endpoint(0, &b).unwrap();

        let b_port = ep_b.local_addr().unwrap().port();
        let b_addr = SocketAddr::new("127.0.0.1".parse().unwrap(), b_port);

        tokio::spawn({
            let ep_b = ep_b.clone();
            async move {
                if let Some(incoming) = ep_b.accept().await {
                    let _ = incoming.await;
                }
            }
        });

        let wrong_fp = [9u8; 32];
        let result = connect(&ep_a, b_addr, &a, Some(wrong_fp)).await;
        assert!(result.is_err());
    }

    /// C1 行为测试: 验证自签证书通过检查
    #[test]
    fn self_signed_certificate_accepted() {
        let dir = tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        let cert_der = id.cert.clone();

        // 自签证书应通过 verify_self_signed
        assert!(verify_self_signed(&cert_der).is_ok(), "自签证书应通过验证");
    }

    /// C1 行为测试: CA 签发证书应被拒绝（非自签）
    #[test]
    fn ca_signed_certificate_rejected() {
        use rcgen::{BasicConstraints, CertificateParams, KeyPair};

        // 生成 CA 密钥对和证书
        let ca_key_pair = KeyPair::generate().unwrap();
        let mut ca_params = CertificateParams::new(vec!["Test CA".to_string()]).unwrap();
        ca_params.is_ca = rcgen::IsCa::Ca(BasicConstraints::Constrained(0));
        let ca_cert = ca_params.self_signed(&ca_key_pair).unwrap();

        // 生成客户端密钥对
        let client_key_pair = KeyPair::generate().unwrap();
        let mut client_params = CertificateParams::new(vec!["Test Client".to_string()]).unwrap();
        client_params.serial_number = Some(vec![2].into());

        // 用 CA 签发客户端证书
        let client_cert = client_params.signed_by(&client_key_pair, &ca_cert, &ca_key_pair).unwrap();

        let client_cert_der = CertificateDer::from(client_cert.der().to_vec());

        // 验证器应拒绝 CA 签发的证书（issuer != subject）
        let result = verify_self_signed(&client_cert_der);
        assert!(result.is_err(), "CA 签发的证书应被拒绝（非自签）");
    }

    /// C2 时间有效性测试: 证书应在有效期内
    #[test]
    fn certificate_time_validity_checked() {
        let dir = tempdir().unwrap();
        let id = Identity::load_or_create(dir.path()).unwrap();
        let cert_der = id.cert.clone();

        // 使用当前时间验证
        let now = UnixTime::since_unix_epoch(Duration::from_secs(
            SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs()
        ));

        let result = verify_cert_validity(&cert_der, now);
        assert!(result.is_ok(), "当前时间应在证书有效期内");
    }

    /// 回归：take_msg 增量分帧——半条消息不误读、跨读取拼装、非法前缀报错
    #[test]
    fn take_msg_frames_incrementally() {
        let mut empty: Vec<u8> = vec![];
        assert!(take_msg(&mut empty).unwrap().is_none(), "空缓冲应返回 None");

        let msg = ControlMsg::SharesReq { msg_id: 0 };
        let wire = crate::protocol::encode_control(&msg);

        // 只有 4 字节前缀
        let body_len = wire.len() - 4;
        let mut buf = wire[..4].to_vec();
        assert!(take_msg(&mut buf).unwrap().is_none(), "缺消息体应返回 None");

        // 前缀 + 半个消息体
        let mut buf = wire[..4 + body_len / 2].to_vec();
        assert!(take_msg(&mut buf).unwrap().is_none(), "半个消息体应返回 None");

        // 完整一条 + 下一条的前缀（粘包）
        let mut buf = wire.clone();
        buf.extend_from_slice(&wire[..3]);
        let got = take_msg(&mut buf).unwrap().expect("完整消息应解出");
        assert!(matches!(got, ControlMsg::SharesReq { .. }), "解出的应是原消息");
        assert_eq!(buf.len(), 3, "应残留下一帧的前 3 字节");
        let _ = take_msg(&mut buf); // 数据不足，None
        assert_eq!(buf.len(), 3, "不解帧时缓冲不应被消费");

        // 非法长度前缀
        let mut bad = 0xFFFF_FFFFu32.to_be_bytes().to_vec();
        bad.extend_from_slice(&[0u8; 8]);
        assert!(take_msg(&mut bad).is_err(), "超长前缀应报错");
    }

    /// 回归（v0.1.5 实机）：控制流在双向并发收发下不得失步。
    /// 曾因 select! 里 recv_msg(read_exact) 被取消丢字节，浏览正常但下载静默超时。
    /// 配对后双方在同一条控制流上交替并发收发数百条消息，最后全部送达。
    #[tokio::test]
    async fn ctrl_loop_survives_bidirectional_burst() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        // A 收到 PairingWaitConsent，B 收到 PairingConsentNeeded
        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        // B 同意并拿到码
        let code = sm_b.grant_consent(&fp_a).await.unwrap();
        assert_eq!(code.len(), 6);

        // B 收到 PairingCodeShown
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { .. } => {}
            other => panic!("B 应收到 CodeShown,实际: {:?}", other),
        }

        // A 收到 PairingCodeEntry
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 输码完成配对
        assert!(sm_a.submit_pair_code(&fp_b, &code).await.unwrap());

        // 双方 SessionUp + PairingResult ok:true(到达顺序不限,平台调度差异)
        for (ev, peer_fp) in [(&mut ev_a, fp_b), (&mut ev_b, ctx_a.identity.fingerprint())] {
            let mut got_up = false;
            let mut got_result = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while !(got_up && got_result) {
                match timeout_at(deadline, ev.recv()).await.unwrap().unwrap() {
                    SessionEvent::SessionUp { fingerprint, .. } => {
                        assert_eq!(fingerprint, peer_fp);
                        got_up = true;
                    }
                    SessionEvent::PairingResult { fingerprint, ok, .. } => {
                        assert_eq!(fingerprint, peer_fp);
                        assert!(ok);
                        got_result = true;
                    }
                    other => panic!("应收到 SessionUp/PairingResult,实际: {:?}", other),
                }
            }
        }

        // 等待连接完成
        let _ = connect_handle.await.unwrap().unwrap();

        // 排空事件通道:配对阶段的事件在 Linux 调度下可能尚未被上面
        // 的顺序无关循环消费完(通道容量 32,残留会滞留),先清空再进爆发段
        while timeout(Duration::from_millis(50), ev_a.recv()).await.is_ok() {}
        while timeout(Duration::from_millis(50), ev_b.recv()).await.is_ok() {}

        // 双方各自取走入站通道（模拟 RPC 路由器/传输等待者）
        let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.expect("A 入站控制通道");
        let mut ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站控制通道");

        // 双向并发爆发：B 向 A 发请求，同时 A 向 B 发响应（同一条流上收发交错）。
        // 读者必须并发消费（生产环境 RPC 路由器即如此）——否则会触发
        // 入站通道"满则丢弃"的背压设计，与本测试要验证的分帧正确性无关。
        //
        // 就绪屏障(Linux 稳定性):reader 消费首条(探针)后经 oneshot 发回执,
        // 主流程等两个回执都到手才放行爆发段——spawn 不等于已在 recv,
        // 无屏障时 sender 可在 reader 起跑前灌满 128 容量通道,超出部分
        // 被 try_send 丢弃,帧序断言因缺帧而错位(WSL 复现:期望 128 实收 192)。
        const N: u64 = 300;
        let (probe_ack_a_tx, mut probe_ack_a_rx) = tokio::sync::oneshot::channel::<()>();
        let (probe_ack_b_tx, mut probe_ack_b_rx) = tokio::sync::oneshot::channel::<()>();
        sm_b.send_ctrl(&fp_a, ControlMsg::ListReq { share_id: "probe".into(), path: String::new(), cursor: 0, msg_id: 0 }).await.unwrap();
        sm_a.send_ctrl(&fp_b, ControlMsg::BitmapReq { job_id: 0 }).await.unwrap();

        let reader_a = tokio::spawn(async move {
            let mut got = 0u64;       // 已消费条数
            let mut next_expected = 0u64; // 下一条应收到的 cursor(允许跳号=通道丢弃,不允许错位)
            let mut rx = ctrl_rx_a;
            let mut ack = Some(probe_ack_a_tx);
            // 2s 静默期 = 爆发结束(尾部帧可能整批被丢,游标到不了 N,
            // 不能用固定总时限——用"收不到新消息"判定流结束)
            while let Some((from, msg)) = timeout(Duration::from_secs(2), rx.recv()).await.unwrap_or(None) {
                assert_eq!(from, fp_b);
                match msg {
                    ControlMsg::ListReq { cursor, .. } => {
                        assert!(cursor >= next_expected, "请求帧序错位: 期望>= {},实际 {}", next_expected, cursor);
                        next_expected = cursor + 1;
                        got += 1;
                    }
                    _ => panic!("A 只应收到 ListReq"),
                }
                if let Some(tx) = ack.take() { let _ = tx.send(()); }
                if next_expected >= N { break; }
            }
            (got, next_expected, rx)
        });
        let reader_b = tokio::spawn(async move {
            let mut got = 0u64;
            let mut next_expected = 0u64;
            let mut ack = Some(probe_ack_b_tx);
            while let Some((from, msg)) = timeout(Duration::from_secs(2), ctrl_rx_b.recv()).await.unwrap_or(None) {
                assert_eq!(from, fp_a);
                match msg {
                    ControlMsg::BitmapReq { job_id } => {
                        assert!(job_id >= next_expected, "帧序错位: 期望>= {},实际 {}", next_expected, job_id);
                        next_expected = job_id + 1;
                        got += 1;
                    }
                    _ => panic!("B 只应收到 BitmapReq"),
                }
                if let Some(tx) = ack.take() { let _ = tx.send(()); }
                if next_expected >= N { break; }
            }
            (got, next_expected)
        });

        // 屏障:两个 reader 都消费了探针,才进入爆发段
        let _ = timeout(Duration::from_secs(5), &mut probe_ack_a_rx).await.expect("A reader 就绪超时");
        let _ = timeout(Duration::from_secs(5), &mut probe_ack_b_rx).await.expect("B reader 就绪超时");

        let sender_b = {
            let sm_b = sm_b.clone();
            let fp = fp_a;
            tokio::spawn(async move {
                for i in 1..N {
                    sm_b.send_ctrl(&fp, ControlMsg::ListReq {
                        share_id: format!("s{}", i),
                        path: format!("p{}", i),
                        cursor: i,
                        msg_id: 0,
                    }).await.unwrap();
                }
            })
        };
        let sender_a = {
            let sm_a = sm_a.clone();
            let fp = fp_b;
            tokio::spawn(async move {
                for i in 1..N {
                    sm_a.send_ctrl(&fp, ControlMsg::BitmapReq { job_id: i }).await.unwrap();
                }
            })
        };
        sender_b.await.unwrap();
        sender_a.await.unwrap();

        // 分帧正确性判定(v0.4.0 修订):本测试守卫的是"收到的消息帧序
        // 不错位、无误读"(v0.1.5 失步回归)。入站通道是容量 128 的
        // "满则丢弃"背压设计(生产语义,传输层常驻消费者),消费速率
        // 赶不上生产速率时允许丢弃,且尾部帧可能整批被丢(WSL 单核
        // 复现:sender 100ms 内灌完 300 条,reader 晚起只消费前 128,
        // 之后的帧在通道满时全丢弃,尾部游标永远到不了 N)。
        // 因此断言:①收到的全部按序、零错位(读者内断言,失步即炸)
        // ②两侧各收到足量消息(至少通道容量 128 条——证明爆发确实
        // 打满了通道且分帧没乱) ③金丝雀:爆发后通道仍活着。
        // 不断言 300 条全到——送达率是消费调度的属性,不是分帧属性。
        let (got_a, _seen_a, mut ctrl_rx_a) = reader_a.await.unwrap();
        assert!(got_a >= 128, "A 收到的请求数应至少填满通道容量: {}", got_a);
        let (got_b, _seen_b) = reader_b.await.unwrap();
        assert!(got_b >= 128, "B 收到的响应数应至少填满通道容量: {}", got_b);
        eprintln!("爆发段实际送达: A {}/{} , B {}/{}", got_a, N, got_b, N);

        // 金丝雀：爆发之后再发一条，通道仍活着（B→A 经会话管理器回环）
        sm_b.send_ctrl(&fp_a, ControlMsg::SharesReq { msg_id: 0 }).await.unwrap();
        match timeout(Duration::from_secs(5), ctrl_rx_a.recv()).await.unwrap().unwrap() {
            (_, ControlMsg::SharesReq { .. }) => {}
            _ => panic!("B→A 的 SharesReq 应送达"),
        }
    }

    /// adopt_connection 测试：进程内两 endpoint，一端 connect 一端 adopt，
    /// 验证 SessionUp 事件正常触发
    #[tokio::test]
    async fn adopt_connection_establishes_session() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
        let b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
        let a_fp = a.fingerprint();
        let b_fp = b.fingerprint();

        // 会话管理器 A（被动方，使用 adopt）
        let trust_a = Arc::new(Mutex::new(TrustStore::load(dir_a.path())));
        // 预互信:绕过配对 UI 流程(establish_control_flow 未信任分支会
        // 等待输码永不返回——本测试只验证 adopt 的会话建立路径)
        {
            let mut t = trust_a.lock().await;
            t.upsert(TrustedPeer {
                fingerprint: b_fp,
                name: "B".into(),
                alias: String::new(),
            paired_at: 0,
                perms: Perms::default(),
            });
        }
        let config_a = Arc::new(RwLock::new(Config::default()));
        let (sm_a, mut ev_a) = SessionManager::spawn(SessionCtx {
            identity: a.clone(),
            trust: trust_a.clone(),
            config: config_a.clone(),
        });
        let ep_a = bind_endpoint(0, &a).unwrap();
        let a_port = ep_a.local_addr().unwrap().port();
        let a_addr = SocketAddr::new("127.0.0.1".parse().unwrap(), a_port);

        // 会话管理器 B（主动方，使用 connect）
        let trust_b = Arc::new(Mutex::new(TrustStore::load(dir_b.path())));
        {
            let mut t = trust_b.lock().await;
            t.upsert(TrustedPeer {
                fingerprint: a_fp,
                name: "A".into(),
                alias: String::new(),
            paired_at: 0,
                perms: Perms::default(),
            });
        }
        let config_b = Arc::new(RwLock::new(Config::default()));
        let (sm_b, _ev_b) = SessionManager::spawn(SessionCtx {
            identity: b.clone(),
            trust: trust_b.clone(),
            config: config_b.clone(),
        });

        // 监听端接受连接
        let conn_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move {
                if let Some(incoming) = ep_a.accept().await {
                    let conn = incoming.await.unwrap();
                    // 使用 adopt_connection 接受连接
                    let result = sm_a.adopt_connection(conn).await;
                    (sm_a, result)
                } else {
                    panic!("未收到连接");
                }
            }
        });

        // 主动方连接
        let connect_result = sm_b.connect(a_addr).await;
        assert!(connect_result.is_ok(), "connect 应成功: {:?}", connect_result);

        // 等待 adopt_connection 完成
        let (sm_a_back, adopt_result) = conn_handle.await.unwrap();
        assert!(adopt_result.is_ok(), "adopt_connection 应成功: {:?}", adopt_result);
        assert_eq!(adopt_result.unwrap(), b_fp, "adopt 应返回正确的对端指纹");

        // 验证 SessionUp 事件触发
        let session_up_event = timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap();
        match session_up_event {
            SessionEvent::SessionUp { fingerprint, name: _, conn: _ } => {
                assert_eq!(fingerprint, b_fp, "SessionUp 指纹应匹配");
            }
            other => panic!("期望 SessionUp，实际收到: {:?}", other),
        }

        // 清理
        sm_a.shutdown_all().await;
        sm_b.shutdown_all().await;
    }

    /// M-B1 竞态回归:同一指纹先后两次 adopt(模拟对端重连),
    /// 新代会话覆盖旧代后,旧 ctrl_loop 退出不得误删新代会话。
    /// 通过 ctrl_loop 收尾路径(断开旧连接触发 remove)后,
    /// 断言 sm.session(fp) 仍返回新一代连接。
    #[tokio::test]
    async fn stale_ctrl_loop_exit_keeps_newer_generation_session() {
        let dir_a = tempdir().unwrap();
        let dir_b = tempdir().unwrap();
        let a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
        let b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
        let a_fp = a.fingerprint();
        let b_fp = b.fingerprint();

        // A 为被 adopt 方,预互信 B(走受信路径)
        let trust_a = Arc::new(Mutex::new(TrustStore::load(dir_a.path())));
        {
            let mut t = trust_a.lock().await;
            t.upsert(TrustedPeer {
                fingerprint: b_fp,
                name: "B".into(),
                alias: String::new(),
                paired_at: 0,
                perms: Perms::default(),
            });
        }
        let (sm_a, _ev_a) = SessionManager::spawn(SessionCtx {
            identity: a.clone(),
            trust: trust_a.clone(),
            config: Arc::new(RwLock::new(Config::default())),
        });
        let ep_a = bind_endpoint(0, &a).unwrap();
        let a_port = ep_a.local_addr().unwrap().port();
        let a_addr = SocketAddr::from(([127, 0, 0, 1], a_port));

        // B 侧同样预互信 A(让它的 ctrl_loop 走受信路径建立会话)
        let trust_b = Arc::new(Mutex::new(TrustStore::load(dir_b.path())));
        {
            let mut t = trust_b.lock().await;
            t.upsert(TrustedPeer {
                fingerprint: a_fp,
                name: "A".into(),
                alias: String::new(),
                paired_at: 0,
                perms: Perms::default(),
            });
        }
        let (sm_b, _ev_b) = SessionManager::spawn(SessionCtx {
            identity: b.clone(),
            trust: trust_b.clone(),
            config: Arc::new(RwLock::new(Config::default())),
        });

        // 第一次连接:adopt → 会话建立(generation g0)
        // 注意:accept 任务必须先于 connect spawn,否则握手无人应答而超时
        let adopt_task = |ep: quinn::Endpoint, sm: Arc<SessionManager>| {
            tokio::spawn(async move {
                if let Some(incoming) = ep.accept().await {
                    sm.adopt_connection(incoming.await.unwrap()).await.is_ok()
                } else { false }
            })
        };
        let first_accept = adopt_task(ep_a.clone(), sm_a.clone());
        sm_b.connect(a_addr).await.expect("第一次 connect 应成功");
        assert!(first_accept.await.unwrap(), "第一次 adopt 应成功");
        // 此时 A 表内有 B 的会话(gen0)。B 再发起第二次连接——
        // adopt 覆盖插入(gen1),同时旧连接关闭令 gen0 的 ctrl_loop 退出。
        let new_conn_injected = adopt_task(ep_a, sm_a.clone());
        sm_b.connect(a_addr).await.expect("第二次 connect 应成功");
        assert!(new_conn_injected.await.unwrap(), "第二次 adopt 应成功");

        // 新一代已覆盖。给旧 ctrl_loop 一点时间走到收尾 remove:
        // 旧行为在收尾无条件 remove(fp),会把新代也删掉。
        tokio::time::sleep(Duration::from_millis(300)).await;

        // 核心断言:B 的会话仍在表里(sm.session 返回 Some)
        let sess = timeout(Duration::from_secs(2), sm_a.session(&b_fp)).await.unwrap();
        assert!(sess.is_some(), "旧代 ctrl_loop 退出后新代会话不应被删除");
    }

    /// M3a FR6:对端发 TrustBroken → 本端清信任条目(双盲对称降级)+清会话表,
    /// 事件序列 TrustBroken → SessionDown(对齐 Goodbye 收尾;toast 先于断连展示)。
    #[tokio::test]
    async fn trust_broken_degrades_local_state_and_notifies() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // 预互信(双端,连接走受信快速路径,无配对噪声)
        for (trust, peer_fp, peer_name) in [
            (&ctx_a.trust, fp_b, "乙"),
            (&ctx_b.trust, fp_a, "甲"),
        ] {
            trust.lock().await.upsert(TrustedPeer {
                fingerprint: peer_fp,
                name: peer_name.to_string(),
                alias: String::new(),
                paired_at: 0,
                perms: Perms::default(),
            });
        }

        let b_addr = start_listener(&sm_b).await;
        timeout(Duration::from_secs(5), sm_a.connect(b_addr)).await
            .expect("连接应在超时前完成").expect("连接应成功");

        // 双端 SessionUp 就绪(甲侧消耗,顺带验证 Hello 名缓存)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::SessionUp { fingerprint, name, .. } => {
                assert_eq!(fingerprint, fp_b);
                assert_eq!(name, "乙");
            }
            other => panic!("甲应收到 SessionUp,实际: {:?}", other),
        }
        let _ = timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap();

        // 乙发 TrustBroken(生产路径:壳层 remove_trusted 在断会话前 send_ctrl;
        // 此处直接驱动协议面,单侧即可验证接收方行为)
        sm_b.send_ctrl(&fp_a, ControlMsg::TrustBroken).await
            .expect("TrustBroken 应送达(会话存在)");

        // 事件 1:TrustBroken(带 Hello 名供 toast)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::TrustBroken { fingerprint, peer_name } => {
                assert_eq!(fingerprint, fp_b);
                assert_eq!(peer_name.as_deref(), Some("乙"));
            }
            other => panic!("甲应收到 TrustBroken,实际: {:?}", other),
        }
        // 事件 2:SessionDown(Goodbye 同款收尾,顺序紧随其后)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::SessionDown { fingerprint } => assert_eq!(fingerprint, fp_b),
            other => panic!("甲应紧随收到 SessionDown,实际: {:?}", other),
        }

        // 本端状态降级:信任条目已删(对称)+会话表已清
        assert!(!ctx_a.trust.lock().await.is_trusted(&fp_b), "收到 TrustBroken 后必须删除对端信任条目(双盲对称重配)");
        assert!(sm_a.session(&fp_b).await.is_none(), "收到 TrustBroken 后会话表必须清空");
    }

    /// 事件定义冒烟:未信任连接双向事件序列的前两步(不含码)
    #[tokio::test]
    async fn untrusted_connect_emits_consent_events() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b);
            }
            other => panic!("A 应收到 PairingWaitConsent,实际: {:?}", other),
        }

        let fp_a_got = match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { fingerprint, .. } => fingerprint,
            other => panic!("B 应收到 PairingConsentNeeded,实际: {:?}", other),
        };
        assert_eq!(fp_a_got, _fp_a);

        // 事件枚举变体不携带码——由类型定义保证,此处再无事件可收即通过。
        // (连接后续停在等同意,这里不断言更多)
        connect_handle.abort();
    }

    // ============ Task 3: B 侧门与码测试 ============

    /// T1: 同意在先、输码在后 → 双方互信
    #[tokio::test]
    async fn grant_then_code_completes() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = ctx_a.identity.fingerprint();

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        // B 弹门
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { .. } => {}
            other => panic!("B 应收到 ConsentNeeded,实际: {:?}", other),
        }
        // A 等待
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("A 应收到 WaitConsent,实际: {:?}", other),
        }

        // B 同意 → 拿到码
        let code = sm_b.grant_consent(&fp_a).await.unwrap();
        assert_eq!(code.len(), 6);

        // B 收到 PairingCodeShown(UI 展示码)
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { .. } => {}
            other => panic!("B 应收到 CodeShown,实际: {:?}", other),
        }

        // A 被通知可输码
        tracing::info!("等待 A 收到 PairingCodeEntry 事件...");
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }
        tracing::info!("A 收到 PairingCodeEntry 事件");

        // A 输码
        let submit_result = sm_a.submit_pair_code(&fp_b, &code).await;
        tracing::info!("A 输码结果: {:?}", submit_result);
        assert!(submit_result.unwrap(),
            "正确码应返回 true");

        // 双方 SessionUp
        for (ev, peer_fp) in [(&mut ev_a, fp_b), (&mut ev_b, ctx_a.identity.fingerprint())] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::SessionUp { fingerprint, .. } => assert_eq!(fingerprint, peer_fp),
                other => panic!("应收到 SessionUp,实际: {:?}", other),
            }
        }
        // 双方 PairingResult ok:true
        for (ev, peer_fp) in [(&mut ev_a, fp_b), (&mut ev_b, ctx_a.identity.fingerprint())] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingResult { fingerprint, ok, .. } => {
                    assert_eq!(fingerprint, peer_fp);
                    assert!(ok);
                }
                other => panic!("应收到 PairingResult,实际: {:?}", other),
            }
        }

        assert!(ctx_a.trust.lock().await.is_trusted(&fp_b));
        assert!(ctx_b.trust.lock().await.is_trusted(&fp_a));
        connect_handle.await.unwrap().unwrap();
    }

    /// T2: 码先到、同意后到 → 挂起放行
    #[tokio::test]
    async fn code_arrives_before_consent_suspends_and_releases() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        // 双方门/等待事件
        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        // B 同意,拿到码
        let code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 故意"先知道码"(模拟肩窥/时序:B 已亮码)再输——正常顺序。
        // 真正的乱序场景:B 未同意时 A 就输码。构造:B 同意前 A 提交。
        // 重新来一遍连接:
        connect_handle.await.unwrap().unwrap();
        sm_a.disconnect(&fp_b).await;
        sm_b.disconnect(&fp_a).await;

        // 排空调道中的 SessionDown 事件
        while timeout(Duration::from_millis(100), ev_a.recv()).await.is_ok() {}
        while timeout(Duration::from_millis(100), ev_b.recv()).await.is_ok() {}

        // 第二轮:B 不先同意
        let b_addr2 = start_listener(&sm_b).await;
        let connect_handle2 = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr2).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("第二轮应收到门事件,实际: {:?}", other),
            }
        }

        // 等待 connect 完成(控制流建立后即返回)
        let _ = connect_handle2.await.unwrap().unwrap();

        // A 先输一个"将来才有效"的码——此刻 B 未同意,码任意。
        // submit 返回什么取决于挂起语义:本地无法判定(B 的码 B 还没生成),
        // A 侧只透传,返回 true 表示"已提交"
        assert!(sm_a.submit_pair_code(&fp_b, "123456").await.unwrap(),
            "A 提交应成功(挂起语义)");

        // 短暂等待确保 PairCodeSubmit 已到达 B
        tokio::time::sleep(Duration::from_millis(300)).await;

        // B 现在同意——生成的码恰好是 A 输的那个?不可能(随机)。
        // 挂起放行的正确语义:B 同意瞬间验暂存码,错则计数(不匹配几乎必然)。
        // 所以本测试验证:挂起期间无 PairingResult(ok:false)泛洪、B 同意后
        // 系统状态一致。真正的"先码后同意成功"需要 A 在 B 同意并亮码后
        // 输对——那与 T1 相同。此测试固化:乱序不崩溃、不误完成。
        let code2 = sm_b.grant_consent(&fp_a).await.unwrap();
        assert_eq!(code2.len(), 6);

        // B 同意后,A 收到 CodeEntry;A 的旧提交已按暂存码验错(计数+1),
        // 但不应断连(未满 3 次)
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("第二轮 A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 用正确码完成
        assert!(sm_a.submit_pair_code(&fp_b, &code2).await.unwrap());

        // B 先收到暂存码的不匹配结果(如果有)
        let mut got_ok = false;
        loop {
            match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingResult { ok, .. } => {
                    if ok {
                        got_ok = true;
                        break;
                    }
                    // ok:false 继续等下一个事件(可能是正确的码的结果)
                }
                SessionEvent::SessionUp { .. } => {
                    // SessionUp 表示配对成功,可以退出
                    got_ok = true;
                    break;
                }
                other => {
                    // 其他事件继续等
                    tracing::warn!("B 收到非预期事件: {:?}", other);
                }
            }
        }
        assert!(got_ok, "B 应收到配对成功结果");
    }

    /// T3: B 拒绝 → A 断连、无互信
    #[tokio::test]
    async fn deny_consent_disconnects_initiator() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = ctx_a.identity.fingerprint();

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        sm_b.deny_consent(&fp_a).await.unwrap();

        // A 收到拒绝结果
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("对方拒绝连接"));
            }
            other => panic!("A 应收到拒绝结果,实际: {:?}", other),
        }

        // connect 已经成功返回(在控制流建立时),但连接随后关闭
        let _ = connect_handle.await.unwrap();

        // 无互信
        assert!(!ctx_a.trust.lock().await.is_trusted(&fp_b));
    }

    /// T4: 门超时断连、不记冷却
    #[tokio::test]
    async fn consent_timeout_disconnects() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, fp_a, _dir_a) = setup_ctx("甲");

        let (sm_b_t, mut ev_b_t, _ctx_b_t, fp_b_t, _dir_b_t) =
            crate::test_support::setup_ctx_with_timeout("乙", 1);

        let b_addr = start_listener(&sm_b_t).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("A 应收到 WaitConsent,实际: {:?}", other),
        }

        match timeout(Duration::from_secs(5), ev_b_t.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { .. } => {}
            other => panic!("B 应收到 ConsentNeeded,实际: {:?}", other),
        }

        // 不点同意,等 2s(门 1s 到期)
        tokio::time::sleep(Duration::from_secs(2)).await;

        // B 侧:应收到 PairingResult{ok:false, "同意超时"}
        match timeout(Duration::from_secs(5), ev_b_t.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { fingerprint, ok, reason } => {
                assert_eq!(fingerprint, fp_a);
                assert!(!ok, "B 应收到配对失败结果");
                assert_eq!(reason.as_deref(), Some("同意超时"));
            }
            other => panic!("B 应收到超时结果,实际: {:?}", other),
        }

        // connect 已经成功返回,等待确保完成
        let _ = connect_handle.await.unwrap();

        // A 侧:应收到 PairingResult{ok:false, "连接已断开"} 与 SessionDown
        let mut got_pairing_result = false;
        let mut got_session_down = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);

        while deadline.elapsed().as_secs() < 5 && (got_pairing_result == false || got_session_down == false) {
            match timeout(Duration::from_millis(500), ev_a.recv()).await {
                Ok(Some(SessionEvent::PairingResult { fingerprint, ok, reason })) => {
                    assert_eq!(fingerprint, fp_b_t);
                    assert!(!ok);
                    assert_eq!(reason.as_deref(), Some("连接已断开"));
                    got_pairing_result = true;
                }
                Ok(Some(SessionEvent::SessionDown { fingerprint })) => {
                    assert_eq!(fingerprint, fp_b_t);
                    got_session_down = true;
                }
                Ok(Some(other)) => {
                    panic!("A 收到非预期事件: {:?}", other);
                }
                Ok(None) | Err(_) => break,
            }
        }

        assert!(got_pairing_result, "A 应收到 PairingResult(连接已断开)");
        assert!(got_session_down, "A 应收到 SessionDown");
    }

    /// T5: 挂起期间暂存码验错同样计数, 3 次断连+冷却
    #[tokio::test]
    async fn wrong_code_counted_even_when_suspended() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        // B 同意(生成随机码,A 不知道)
        let _code = sm_b.grant_consent(&fp_a).await.unwrap();
        // B 收到 PairingCodeShown
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { .. } => {}
            other => panic!("B 应收到 CodeShown,实际: {:?}", other),
        }
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 连错 3 次(避开真实码:真实码随机,000000 命中概率 1e-6;
        // 为确定性,连输 000000/111111/222222 三个不同错码)
        for wrong in ["000000", "111111", "222222"] {
            // A 侧 submit_pair_code 返回 true(挂起语义),实际结果由 PairingResult 事件返回
            let r = sm_a.submit_pair_code(&fp_b, wrong).await.unwrap();
            assert!(r, "A 侧 submit_pair_code 应返回 true(挂起语义)");
            // 第 3 次错码时,连接可能在 PairingResult 之前关闭,需要灵活处理
            loop {
                match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
                    SessionEvent::PairingResult { ok, .. } => {
                        assert!(!ok);
                        break;
                    }
                    SessionEvent::SessionDown { .. } => {
                        // 连接关闭,可能没有收到 PairingResult(第3次错码的竞态)
                        // 这种情况下,通过 SessionDown 即可判断配对失败
                        if wrong == "222222" {
                            // 第 3 次:接受 SessionDown 作为失败信号
                            break;
                        } else {
                            panic!("前 2 次错码不应先收到 SessionDown");
                        }
                    }
                    other => panic!("应收到 PairingResult,实际: {:?}", other),
                }
            }
        }

        // connect 已经成功返回了(在控制流建立时),这里等待确保完成
        let _ = connect_handle.await.unwrap();

        // 冷却:B 侧已记录 A 的冷却,但 A 侧无法直接验证
        // 在实际使用中,B 会拒绝 A 的后续连接请求
        // 这里仅验证 B 侧确实记录了 cooldown(通过 session 不可用或后续连接被拒)
        // 由于 A 侧 SessionManager 无法直接检查 B 的 cooldown,我们通过
        // 检查 A 尝试再次连接是否被 B 拒绝来间接验证
        let b_addr2 = start_listener(&sm_b).await;
        // A 再次连接:B 会检查自己的 cooldown 并拒绝
        let r2 = timeout(Duration::from_secs(5), sm_a.connect(b_addr2)).await;
        // 连接可能超时(B 拒绝)或返回错误
        assert!(r2.is_err() || r2.unwrap().is_err(), "冷却期内重连应被 B 拒绝");
    }

    /// T9: B 主动结束等待 → 断连+码即焚;A 收"对方已结束等待"
    #[tokio::test]
    async fn cancel_wait_burns_code_and_disconnects() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, _fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        let _code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // B 结束等待
        sm_b.cancel_wait(&fp_a).await.unwrap();

        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("对方已结束等待"));
            }
            other => panic!("A 应收到结束等待结果,实际: {:?}", other),
        }

        // connect 已经成功返回(控制流建立后即返回),不再检查失败
    }

    /// P2 错码文案锚:发起方 A 错码 1 次收到的事件必须是「配对码不匹配」,
    /// 不得再是旧文案「对端拒绝」——真拒绝走 ConsentDeny(「对方拒绝连接」),
    /// 旧文案令 UI 把可重输的错码误判为终态拒绝,重输流程断裂(P2 T1 表 #3)。
    #[tokio::test]
    async fn wrong_code_result_says_mismatch_not_denied() {
        init_tracing();
        let (sm_a, mut ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, _ctx_b, fp_b, _dir_b) = setup_ctx("乙");
        let fp_a = _fp_a;

        let b_addr = start_listener(&sm_b).await;
        let connect_handle = tokio::spawn({
            let sm_a = sm_a.clone();
            async move { sm_a.connect(b_addr).await }
        });

        for ev in [&mut ev_a, &mut ev_b] {
            match timeout(Duration::from_secs(5), ev.recv()).await.unwrap().unwrap() {
                SessionEvent::PairingWaitConsent { .. }
                | SessionEvent::PairingConsentNeeded { .. } => {}
                other => panic!("应收到门事件,实际: {:?}", other),
            }
        }

        let _code = sm_b.grant_consent(&fp_a).await.unwrap();
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { .. } => {}
            other => panic!("B 应收到 CodeShown,实际: {:?}", other),
        }
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("A 应收到 CodeEntry,实际: {:?}", other),
        }

        // A 错码一次(避开随机真码)
        assert!(sm_a.submit_pair_code(&fp_b, "000000").await.unwrap()
            || _code == "000000");
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingResult { ok, reason, .. } => {
                assert!(!ok);
                assert_eq!(reason.as_deref(), Some("配对码不匹配"),
                    "错码(未满 3 次)必须报「配对码不匹配」让 UI 回输码态,不得报「对端拒绝」");
            }
            other => panic!("A 应收到错码结果,实际: {:?}", other),
        }
        let _ = connect_handle.await.unwrap();
    }

    /// P2 双盲并发 #1(顺序互连,确定性):A 连 B 完成后 B 立即连 A——
    /// 会话表按 fp 单条目、后插者覆盖,最终双方表内存活的必须是同一条
    /// 连接(后到的 C2),且在 C2 上完成一次配对即双端互信。
    /// 锚定"一边胜出"语义:双端各恰好一次 ok:true,无重复完成、无死锁。
    #[tokio::test]
    async fn mutual_connect_sequential_converges_single_session() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        // C1:甲连乙
        let b_addr = start_listener(&sm_b).await;
        let h1 = tokio::spawn({ let sm = sm_a.clone(); async move { sm.connect(b_addr).await } });
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("A(C1) 应收到 WaitConsent,实际: {:?}", other),
        }
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { .. } => {}
            other => panic!("B(C1) 应收到 ConsentNeeded,实际: {:?}", other),
        }
        h1.await.unwrap().unwrap();

        // C2:乙连甲(紧随其后,双方表内 C1 均被 C2 覆盖,C1 沦为僵尸)
        let a_addr = start_listener(&sm_a).await;
        let h2 = tokio::spawn({ let sm = sm_b.clone(); async move { sm.connect(a_addr).await } });
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingWaitConsent { .. } => {}
            other => panic!("B(C2) 应收到 WaitConsent,实际: {:?}", other),
        }
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingConsentNeeded { .. } => {}
            other => panic!("A(C2) 应收到 ConsentNeeded,实际: {:?}", other),
        }
        h2.await.unwrap().unwrap();

        // 双端表内各恰一条会话,且都是 C2 世代:乙表=C2 发起方(Initiator),
        // 甲表=C2 接受方(AwaitingConsent)——甲在 C2 上同意,一发一收收敛
        let code = sm_a.grant_consent(&fp_b).await
            .expect("甲(接受方,存活条目 C2)应能同意——双盲后插者覆盖语义");
        match timeout(Duration::from_secs(5), ev_a.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeShown { .. } => {}
            other => panic!("A 应收到 CodeShown,实际: {:?}", other),
        }
        match timeout(Duration::from_secs(5), ev_b.recv()).await.unwrap().unwrap() {
            SessionEvent::PairingCodeEntry { .. } => {}
            other => panic!("B 应收到 CodeEntry,实际: {:?}", other),
        }
        assert!(sm_b.submit_pair_code(&fp_a, &code).await.unwrap());

        // 双端各恰好一次 PairingResult ok:true + 互信写入
        for (ev, _peer) in [(&mut ev_a, fp_b), (&mut ev_b, fp_a)] {
            let mut ok_count = 0;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while ok_count == 0 {
                match timeout_at(deadline, ev.recv()).await.unwrap().unwrap() {
                    SessionEvent::PairingResult { ok: true, .. } => ok_count += 1,
                    SessionEvent::PairingResult { ok: false, reason, .. } => {
                        panic!("顺序互连不应失败,实际 ok:false reason={reason:?}");
                    }
                    SessionEvent::SessionUp { .. } => {}
                    other => panic!("非预期事件: {:?}", other),
                }
            }
            // 收尾窗口内不得出现第二次 ok:true(重复完成)
            match timeout(Duration::from_millis(300), ev.recv()).await {
                Ok(Some(SessionEvent::PairingResult { ok: true, .. })) => {
                    panic!("配对完成事件应恰好一次,出现重复 ok:true");
                }
                _ => {}
            }
        }
        assert!(ctx_a.trust.lock().await.is_trusted(&fp_b), "甲应写入乙信任");
        assert!(ctx_b.trust.lock().await.is_trusted(&fp_a), "乙应写入甲信任");
    }

    /// P2 双盲并发 #2(真并发):A、B 同时互连。现有语义(后插者覆盖)下
    /// 交错有三种落点:好交错(存活条目同属一条连接)一轮收敛;坏交错
    /// (双方表内都剩自己主动连接,双方 grant 报「会话不处于待同意状态」)
    /// 或交叉双待同意(grant 都成但判定请求被僵尸忽略)——本测试锚定:
    /// 任一落点下,**至多补一次单向重连即收敛**,永不死锁、不 panic、
    /// 双端各恰好一次 ok:true(重复完成/双 ok 也视为违例)。
    #[tokio::test]
    async fn simultaneous_connect_bounded_recovery() {
        init_tracing();
        let (sm_a, mut ev_a, ctx_a, fp_a, _dir_a) = setup_ctx("甲");
        let (sm_b, mut ev_b, ctx_b, fp_b, _dir_b) = setup_ctx("乙");

        let a_addr = start_listener(&sm_a).await;
        let b_addr = start_listener(&sm_b).await;

        // 双盲:同时发起
        let h_a = tokio::spawn({ let sm = sm_a.clone(); async move { sm.connect(b_addr).await } });
        let h_b = tokio::spawn({ let sm = sm_b.clone(); async move { sm.connect(a_addr).await } });
        let _ = timeout(Duration::from_secs(5), h_a).await.unwrap().unwrap();
        let _ = timeout(Duration::from_secs(5), h_b).await.unwrap().unwrap();
        // 让插入竞态落定
        tokio::time::sleep(Duration::from_millis(500)).await;

        /// 尝试在 acceptor(接受方)上 grant 并由 initiator 输码完成配对;
        /// Err=该侧表内条目不处于待同意态(无法在此连接上配对)。
        async fn try_pair_on(
            acceptor: &Arc<SessionManager>,
            acceptor_fp: &crate::identity::Fingerprint,
            initiator: &Arc<SessionManager>,
            initiator_fp: &crate::identity::Fingerprint,
        ) -> Result<(), ()> {
            let code = acceptor.grant_consent(acceptor_fp).await.map_err(|_| ())?;
            initiator.submit_pair_code(initiator_fp, &code).await.map_err(|_| ())?;
            Ok(())
        }

        // 第一轮:任一侧可 grant 则完成;双侧都 grant 失败(坏交错)则记为需补连
        let round1 = match tokio::time::timeout(
            Duration::from_secs(5),
            try_pair_on(&sm_a, &fp_a, &sm_b, &fp_b),
        ).await {
            Ok(Ok(())) => true,
            _ => match tokio::time::timeout(
                Duration::from_secs(5),
                try_pair_on(&sm_b, &fp_b, &sm_a, &fp_a),
            ).await {
                Ok(Ok(())) => true,
                _ => false,
            },
        };

        // 交叉双待同意落点:两处 grant 都成功,但判定请求被对端僵尸忽略,
        // 完成事件不来——等待窗超时同样走"补一次单向重连"恢复路径
        if round1 {
            let got = tokio::time::timeout(Duration::from_secs(3), async {
                loop {
                    match ev_a.recv().await {
                        Some(SessionEvent::PairingResult { ok: true, .. }) => break,
                        Some(_) => {}
                        None => return,
                    }
                }
            }).await;
            if got.is_err() {
                // 第一轮判定未收敛(交叉落点),走补连恢复
                let addr = start_listener(&sm_b).await;
                let _ = timeout(Duration::from_secs(5), sm_a.connect(addr)).await;
                let code = sm_b.grant_consent(&fp_a).await.expect("补连后乙表应为新连接的待同意态");
                sm_a.submit_pair_code(&fp_b, &code).await.unwrap();
            }
        } else {
            // 坏交错:补一次单向重连(甲连乙),乙表被新连接覆盖为待同意态
            let addr = start_listener(&sm_b).await;
            let _ = timeout(Duration::from_secs(5), sm_a.connect(addr)).await;
            let code = sm_b.grant_consent(&fp_a).await.expect("补连后乙表应为新连接的待同意态");
            sm_a.submit_pair_code(&fp_b, &code).await.unwrap();
        }

        // 终局断言:5s 内双端各恰好一次 ok:true + 互信(有界恢复,不死锁)
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        for (ev, peer, ctx, name) in [
            (&mut ev_a, fp_b, &ctx_a, "甲"),
            (&mut ev_b, fp_a, &ctx_b, "乙"),
        ] {
            let mut ok_count = 0u32;
            while ok_count == 0 {
                match timeout_at(deadline, ev.recv()).await {
                    Err(_) => panic!("{name} 5s 内未收到配对完成(死锁?)"),
                    Ok(None) => panic!("{name} 事件通道异常关闭"),
                    Ok(Some(SessionEvent::PairingResult { ok: true, .. })) => ok_count += 1,
                    Ok(Some(SessionEvent::PairingResult { ok: false, reason, .. })) => {
                        panic!("{name} 收到失败终态: {reason:?}")
                    }
                    Ok(Some(_)) => {}
                }
            }
            match timeout(Duration::from_millis(300), ev.recv()).await {
                Ok(Some(SessionEvent::PairingResult { ok: true, .. })) => {
                    panic!("{name} 配对完成事件应恰好一次")
                }
                _ => {}
            }
            assert!(ctx.trust.lock().await.is_trusted(&peer), "{name} 应写入对端信任");
        }
    }
}

