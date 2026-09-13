// Step 1: 写失败测试
use ed25519_dalek::{SigningKey, VerifyingKey, Signer};
use serde::{Serialize, Deserialize};
use thiserror::Error;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::net::SocketAddr;
use std::time::Instant;
use std::collections::HashMap;
use tokio::sync::{mpsc, watch};
use rand::rngs::OsRng;

/// Packet kind for discovery messages
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PacketKind {
    Presence,
    Probe,
    ProbeResp,
}

/// 发现包时间戳容忍窗口（毫秒）。
/// 防重放由 nonce 去重承担（ReplayGuard 保留时长与此窗口对齐），
/// 时间戳只负责挡住远期旧包。±10 分钟可容纳没开 NTP 同步、时钟慢漂移
/// 的机器——真机排障实测过两台机器日期差 2 天导致互相丢包、设备互相看不见。
pub const CLOCK_SKEW_TOLERANCE_MS: u64 = 10 * 60 * 1000;

/// Discovery packet structure
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryPacket {
    pub v: u8,
    pub kind: PacketKind,
    pub name: String,
    pub fingerprint: [u8; 32],
    #[serde(rename = "pubkey")]
    pub pubkey: [u8; 32], // Ed25519 public key (verifying key) - per R1 ruling
    pub quic_port: u16,
    pub ts_ms: u64,
    pub nonce: [u8; 12],
}

/// Discovery error types
#[derive(Error, Debug)]
pub enum DiscoveryError {
    #[error("签名验证失败")]
    BadSignature,
    #[error("时间戳过期或来自未来")]
    Stale,
    #[error("检测到重放攻击")]
    Replay,
    #[error("数据格式错误")]
    Malformed,
    #[error("nonce 表容量已满")]
    Overflow,
}

/// Replay attack prevention guard
pub struct ReplayGuard {
    nonces: std::collections::HashMap<[u8; 12], u64>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        ReplayGuard {
            nonces: std::collections::HashMap::with_capacity(4096),
        }
    }

    /// Check if nonce is valid (not seen before within time window)
    /// Returns Ok if nonce is fresh, Err if replay detected
    pub fn check_nonce(&mut self, nonce: &[u8; 12], ts_ms: u64, now_ms: u64) -> Result<(), DiscoveryError> {
        // 清除窗口外的旧 nonce——保留时长必须 ≥ 时间戳容忍窗口，
        // 否则窗口内重放 nonce 已被清除的旧包会被放行
        let cutoff = now_ms.saturating_sub(CLOCK_SKEW_TOLERANCE_MS);
        self.nonces.retain(|_, last_ts| *last_ts > cutoff);

        // Check if nonce was already seen(先于容量检查:重复 nonce 不增表长,
        // 洪泛期重放检测仍准确返回 Replay 而非 Overflow)
        if let Some(&existing_ts) = self.nonces.get(nonce) {
            if existing_ts == ts_ms {
                return Err(DiscoveryError::Replay);
            }
        }

        // 容量兜底(I1):攻击者用有效签名包(发现广播里就有公钥)在单个
        // 时钟窗口内灌满 4096 条新鲜 nonce 后,retain 全为 no-op,表若
        // 继续插入会无界膨胀。此时必须**拒绝新条目**——洪泛者拿到
        // 丢弃,内存回到有界;已记录的窗口内 nonce 不被洗掉,重放仍拒。
        // 合法对端 nonce 不重复,自己不会被拒。
        if self.nonces.len() >= 4096 {
            self.nonces.retain(|_, last_ts| *last_ts > cutoff);
            if self.nonces.len() >= 4096 {
                return Err(DiscoveryError::Overflow);
            }
        }

        // Record this nonce
        self.nonces.insert(*nonce, ts_ms);
        Ok(())
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode discovery packet to JSON and append Ed25519 signature
pub fn encode_and_sign(pkt: &DiscoveryPacket, key: &SigningKey) -> Vec<u8> {
    let json_bytes = serde_json::to_vec(pkt).expect("序列化失败");
    let signature = key.sign(&json_bytes);
    let mut result = Vec::with_capacity(json_bytes.len() + 64);
    result.extend_from_slice(&json_bytes);
    result.extend_from_slice(&signature.to_bytes());
    result
}

/// Verify signature and parse discovery packet
pub fn verify_and_parse(
    bytes: &[u8],
    now_ms: u64,
    replay_guard: &mut ReplayGuard,
) -> Result<DiscoveryPacket, DiscoveryError> {
    // Check minimum length (at least 64 bytes signature + minimal JSON)
    if bytes.len() < 64 {
        return Err(DiscoveryError::Malformed);
    }

    // Split into JSON and signature
    let (json_bytes, sig_bytes) = bytes.split_at(bytes.len() - 64);

    // Parse JSON to get packet (includes pubkey field per R1)
    let pkt: DiscoveryPacket = serde_json::from_slice(json_bytes)
        .map_err(|_| DiscoveryError::Malformed)?;

    // S8 验签必须先行:失败早退、不消耗 ReplayGuard 槽位。
    // 若先记录 nonce 后验签,攻击者可伪造签名抢注他人 nonce(DoS),
    // 或借坏包洪泛洗掉去重状态。内容真实性未确认前不得改写任何状态。
    let verifying_key = VerifyingKey::from_bytes(&pkt.pubkey)
        .map_err(|_| DiscoveryError::BadSignature)?;

    use ed25519_dalek::Verifier;
    let signature = ed25519_dalek::Signature::try_from(sig_bytes)
        .map_err(|_| DiscoveryError::BadSignature)?;

    verifying_key
        .verify(json_bytes, &signature)
        .map_err(|_| DiscoveryError::BadSignature)?;

    // 时间戳须落在 ±CLOCK_SKEW_TOLERANCE_MS 窗口内
    let now_diff = if now_ms > pkt.ts_ms {
        now_ms - pkt.ts_ms
    } else {
        pkt.ts_ms - now_ms
    };
    if now_diff > CLOCK_SKEW_TOLERANCE_MS {
        tracing::warn!(
            "时钟窗口外拒绝: 本机 now={} 包 ts={} 偏差 {:+} 秒——两台机器系统时间差超过 10 分钟会互相丢弃发现包，请校准系统时间",
            now_ms, pkt.ts_ms, (pkt.ts_ms as i64 - now_ms as i64) as f64 / 1000.0
        );
        return Err(DiscoveryError::Stale);
    }

    // Check nonce for replay attack
    replay_guard.check_nonce(&pkt.nonce, pkt.ts_ms, now_ms)?;

    Ok(pkt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::OsRng;

    #[test]
    fn sign_verify_roundtrip() {
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let pkt = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: "甲".into(),
            fingerprint: [2; 32],
            pubkey: pubkey_bytes,
            quic_port: crate::ports::quic_port(),
            ts_ms: 1_000_000,
            nonce: [9; 12],
        };

        let mut guard = ReplayGuard::new();
        let encoded = encode_and_sign(&pkt, &key);
        let parsed = verify_and_parse(&encoded, 1_000_000, &mut guard).unwrap();
        assert_eq!(parsed.name, "甲");
        assert_eq!(parsed.kind, PacketKind::Presence);
        assert_eq!(parsed.quic_port, crate::ports::quic_port());
    }

    #[test]
    fn tampered_packet_rejected() {
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let pkt = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: "乙".into(),
            fingerprint: [3; 32],
            pubkey: pubkey_bytes,
            quic_port: 48001,
            ts_ms: 2_000_000,
            nonce: [7; 12],
        };

        let mut guard = ReplayGuard::new();
        let mut raw = encode_and_sign(&pkt, &key);

        // Tamper with a byte in the middle of the JSON part
        let i = raw.len() / 2;
        raw[i] ^= 1;

        let result = verify_and_parse(&raw, 2_000_000, &mut guard);
        // Should fail with either BadSignature or Malformed depending on what got corrupted
        assert!(result.is_err());
        match result {
            Err(DiscoveryError::BadSignature) => {},
            Err(DiscoveryError::Malformed) => {},
            other => panic!("Expected BadSignature or Malformed, got {:?}", other),
        }
    }

    #[test]
    fn stale_and_replay_rejected() {
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let pkt = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Probe,
            name: "丙".into(),
            fingerprint: [4; 32],
            pubkey: pubkey_bytes,
            quic_port: 49001,
            ts_ms: 3_000_000,
            nonce: [5; 12],
        };

        let mut guard = ReplayGuard::new();
        let encoded = encode_and_sign(&pkt, &key);

        // Test stale timestamp (窗口 + 1 毫秒之前)
        let result = verify_and_parse(&encoded, 3_000_000 + CLOCK_SKEW_TOLERANCE_MS + 1, &mut guard);
        assert!(matches!(result, Err(DiscoveryError::Stale)));

        // Test future timestamp (窗口 + 1 毫秒之后)
        let result = verify_and_parse(&encoded, 3_000_000 - CLOCK_SKEW_TOLERANCE_MS - 1, &mut guard);
        assert!(matches!(result, Err(DiscoveryError::Stale)));

        // Test replay attack - first submission should succeed
        let now = 3_000_000;
        verify_and_parse(&encoded, now, &mut guard).unwrap();

        // Second submission with same nonce should fail
        let result = verify_and_parse(&encoded, now, &mut guard);
        assert!(matches!(result, Err(DiscoveryError::Replay)));
    }

    #[test]
    fn unsigned_or_short_rejected() {
        let mut guard = ReplayGuard::new();
        let now = 4_000_000;

        // Empty packet
        assert!(matches!(
            verify_and_parse(b"", now, &mut guard),
            Err(DiscoveryError::Malformed)
        ));

        // Too short (less than 64 bytes)
        assert!(matches!(
            verify_and_parse(b"{}", now, &mut guard),
            Err(DiscoveryError::Malformed)
        ));

        // Invalid JSON
        let invalid = vec![b'x'; 100];
        assert!(matches!(
            verify_and_parse(&invalid, now, &mut guard),
            Err(DiscoveryError::Malformed)
        ));
    }

    #[test]
    fn different_nonce_allowed() {
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let mut guard = ReplayGuard::new();
        let now = 5_000_000;

        // First packet with nonce [1; 12]
        let pkt1 = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: "丁".into(),
            fingerprint: [6; 32],
            pubkey: pubkey_bytes,
            quic_port: 50001,
            ts_ms: now,
            nonce: [1; 12],
        };
        let enc1 = encode_and_sign(&pkt1, &key);
        verify_and_parse(&enc1, now, &mut guard).unwrap();

        // Second packet with different nonce [2; 12] should succeed
        let pkt2 = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: "丁".into(),
            fingerprint: [6; 32],
            pubkey: pubkey_bytes,
            quic_port: 50001,
            ts_ms: now,
            nonce: [2; 12],
        };
        let enc2 = encode_and_sign(&pkt2, &key);
        verify_and_parse(&enc2, now, &mut guard).unwrap();
    }

    #[test]
    fn timestamp_boundary_conditions() {
        let mut csprng = OsRng;
        let key = SigningKey::generate(&mut csprng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let pkt = DiscoveryPacket {
            v: 1,
            kind: PacketKind::ProbeResp,
            name: "戊".into(),
            fingerprint: [7; 32],
            pubkey: pubkey_bytes,
            quic_port: 51001,
            ts_ms: 6_000_000,
            nonce: [8; 12],
        };

        let mut guard = ReplayGuard::new();

        // Exactly 30 seconds in the past should succeed
        let encoded = encode_and_sign(&pkt, &key);
        verify_and_parse(&encoded, 5_970_000, &mut guard).unwrap();

        // Exactly 30 seconds in the future should succeed
        let mut guard2 = ReplayGuard::new();
        verify_and_parse(&encoded, 6_030_000, &mut guard2).unwrap();

        // 恰好在 ±10 分钟边界上仍应通过
        let mut guard3 = ReplayGuard::new();
        verify_and_parse(&encoded, 6_000_000 - CLOCK_SKEW_TOLERANCE_MS, &mut guard3).unwrap();
        let mut guard4 = ReplayGuard::new();
        verify_and_parse(&encoded, 6_000_000 + CLOCK_SKEW_TOLERANCE_MS, &mut guard4).unwrap();

        // 超出边界 1 毫秒应拒绝（Stale）
        let mut guard5 = ReplayGuard::new();
        assert!(matches!(
            verify_and_parse(&encoded, 6_000_000 - CLOCK_SKEW_TOLERANCE_MS - 1, &mut guard5),
            Err(DiscoveryError::Stale)
        ));
    }

    #[test]
    fn replay_guard_eviction_and_capacity() {
        let mut guard = ReplayGuard::new();

        // Fill the guard with nonces
        let mut now = 1_000_000u64;
        for i in 0..5000 {
            let nonce = [
                (i >> 24) as u8,
                (i >> 16) as u8,
                (i >> 8) as u8,
                i as u8,
                0, 0, 0, 0, 0, 0, 0, 0
            ];
            let _ = guard.check_nonce(&nonce, now + i as u64, now);
        }

        // 容量满后新条目被拒(Overflow),表有界不膨胀
        let test_nonce = [99u8; 12];
        assert!(matches!(
            guard.check_nonce(&test_nonce, now + 1, now),
            Err(DiscoveryError::Overflow)
        ));

        // 时间推进越过窗口:旧条目过期淘汰后,新 nonce 恢复可入
        now += CLOCK_SKEW_TOLERANCE_MS + 1;
        guard.check_nonce(&test_nonce, now, now).unwrap();
    }

    /// S8 审计(I1):窗口内新鲜 nonce 灌满容量后,后续 retain 全为
    /// no-op,表若继续插入会无界膨胀(数百 MB)。容量满语义必须是
    /// **拒绝新条目**(Overflow)——洪泛者拿到丢弃,内存有界;
    /// 已记录的窗口内 nonce 不受影响,重放仍被拒。合法对端 nonce
    /// 不重复,自己不会被拒。
    #[test]
    fn capacity_overflow_rejects_new_entries_and_keeps_window() {
        let mut guard = ReplayGuard::new();
        let now = 10_000_000u64;

        // 灌入容量上限条窗口内的新鲜记录(占满 4096)
        for i in 0..4096u32 {
            let nonce = [(i >> 8) as u8, i as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
            guard.check_nonce(&nonce, now - 1000, now).unwrap();
        }

        // 第 4097 条:retain 后仍满 → 拒绝插入而非膨胀
        assert!(
            matches!(guard.check_nonce(&[0xEE; 12], now, now), Err(DiscoveryError::Overflow)),
            "容量满时应返回 Overflow 而非继续插入"
        );

        // 已记录的窗口内 nonce 仍在表里,重放被拒(未被洗掉)
        let first = [0u8, 0u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(
            matches!(guard.check_nonce(&first, now - 1000, now), Err(DiscoveryError::Replay)),
            "容量满拒绝新条目后,窗口内已见 nonce 的重放应仍被拒"
        );

        // 时间推进越过窗口:旧条目过期淘汰后,新 nonce 恢复可入
        let later = now + CLOCK_SKEW_TOLERANCE_MS + 1;
        guard.check_nonce(&[0xDD; 12], later, later).unwrap();
    }

    /// S8 审计:验签必须先于 nonce 消耗——无效签名的包不得占用
    /// ReplayGuard 槽位,否则攻击者可抢注他人的 nonce 制造 DoS。
    #[test]
    fn invalid_signature_does_not_consume_nonce_slot() {
        let key = SigningKey::generate(&mut OsRng);
        let pubkey_bytes = key.verifying_key().to_bytes();

        let pkt = DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: "己".into(),
            fingerprint: [8; 32],
            pubkey: pubkey_bytes,
            quic_port: 52001,
            ts_ms: 20_000_000,
            nonce: [0xAB; 12],
        };
        let now = 20_000_000;

        // 篡改签名段最后一个字节 → 验签必败,但 JSON(nonce)未变
        let mut bad = encode_and_sign(&pkt, &key);
        let last = bad.len() - 1;
        bad[last] ^= 1;

        let mut guard = ReplayGuard::new();
        assert!(matches!(
            verify_and_parse(&bad, now, &mut guard),
            Err(DiscoveryError::BadSignature)
        ));

        // 合法重签同一包(同 nonce):若坏包消耗了槽位这里会误判 Replay
        let good = encode_and_sign(&pkt, &key);
        verify_and_parse(&good, now, &mut guard)
            .expect("验签失败的包不应消耗 nonce 槽位");
    }

    #[test]
    fn own_nonce_registration_and_window() {
        let mut own = std::collections::VecDeque::new();
        let n1 = next_own_nonce(&mut own);
        let n2 = next_own_nonce(&mut own);
        assert_ne!(n1, n2);
        // 登记过的 nonce 能被识别为本机所发（自己的广播回环）
        assert!(own.iter().any(|n| *n == n1));
        // 未登记的 nonce 视为外来——身份冲突告警的判定依据
        let foreign = [0xAB; 12];
        assert!(!own.iter().any(|n| *n == foreign));
        // 窗口滑动:超过 128 条后最旧的被淘汰
        for _ in 0..200 {
            next_own_nonce(&mut own);
        }
        assert_eq!(own.len(), 128);
        assert!(!own.iter().any(|n| *n == n1), "最旧的 nonce 应已被淘汰");
    }
}

// ==================== SERVICE LAYER ====================

/// Discovery service configuration
#[derive(Clone)]
pub struct DiscoveryConfig {
    pub bind_port: u16,
    pub target: SocketAddr,
    pub hidden: Arc<AtomicBool>,
    pub name: String,
    pub quic_port: u16,
    pub offline_secs: u64,
    /// 设备真实指纹(SHA-256(证书 DER),由 identity 层提供)。
    /// 发现层不推导只透传——它是对外身份,配对/信任判定依赖此值。
    pub fingerprint: [u8; 32],
    /// 数据目录(身份/信任同级的 data/)。设置后重探目标持久化到
    /// probe_targets.json——跨网段对端靠单播重探保活，受限广播到不了对方，
    /// 重启后若不恢复目标表，两边会互相“失忆”（真机踩过：双双重启后互不可见）
    pub data_dir: Option<std::path::PathBuf>,
}

impl Default for DiscoveryConfig {
    fn default() -> Self {
        // T1:默认端口统一走 ports 模块——绑定/广播目标/QUIC 端口必须
        // 同源,否则设 LOCALTRANS_TEST_PORT_BASE 后三者错位、互相发现不了
        let discovery = crate::ports::discovery_port();
        DiscoveryConfig {
            bind_port: discovery,
            target: SocketAddr::from(([255, 255, 255, 255], discovery)),
            hidden: Arc::new(AtomicBool::new(false)),
            name: "unknown".to_string(),
            quic_port: crate::ports::quic_port(),
            offline_secs: 15,
            data_dir: None,
            fingerprint: [0u8; 32],
        }
    }
}

/// Information about a discovered device
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub fingerprint: [u8; 32],
    pub addr: SocketAddr,
    pub last_seen: Instant,
}

/// Commands to control the discovery service
#[derive(Debug)]
pub enum DiscoveryCmd {
    ProbeNow,
    ProbeAddr(SocketAddr),
    /// 改设备名并立即生效:更新出站包的名字快照 + 连发一轮 Presence,
    /// 对端无需等本机重启即可看到新名(发现包每次都带最新 name)
    SetName(String),
}

/// Handle to control and receive updates from the discovery service
pub struct DiscoveryHandle {
    pub devices: watch::Receiver<Vec<DeviceInfo>>,
    pub cmd: mpsc::Sender<DiscoveryCmd>,
    pub shutdown: mpsc::Sender<()>,
    stop: Arc<AtomicBool>,
    join_handle: Option<std::thread::JoinHandle<()>>,
}

impl Drop for DiscoveryHandle {
    fn drop(&mut self) {
        // 句柄销毁即停止服务线程并回收:防止线程泄漏占用端口
        self.stop.store(true, Ordering::Relaxed);
        if let Some(j) = self.join_handle.take() {
            let _ = j.join();
        }
    }
}

/// Spawn the discovery service.
///
/// 实现为专用 OS 线程 + 阻塞 std UDP 套接字,而非 tokio 任务:
/// 本机 Windows 环境下 tokio UDP 存在缺陷——套接字发送过包后,
/// recv 完成一次并再次挂起会使整个运行时冻结(所有任务与定时器
/// 停摆,含 multi_thread flavor,最小 6 行可复现)。发现服务是
/// 低速率控制面,改走阻塞线程彻底规避;对外句柄 API 不变。
pub fn spawn(cfg: DiscoveryConfig, key: Arc<SigningKey>) -> std::io::Result<DiscoveryHandle> {
    let (cmd_tx, cmd_rx) = mpsc::channel(8);
    let (devices_tx, devices_rx) = watch::channel(Vec::new());
    let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

    // std::net::UdpSocket 在 Windows 上没有 set_reuse_address,经 socket2
    // 在 bind 前设置 SO_REUSEADDR + SO_BROADCAST(简报第 14 行要求)。
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    sock.set_reuse_address(true)?;
    sock.set_broadcast(true)?;
    let bind_addr: std::net::SocketAddr = format!("0.0.0.0:{}", cfg.bind_port).parse().unwrap();
    sock.bind(&bind_addr.into())?;
    let socket: std::net::UdpSocket = sock.into();

    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = stop.clone();

    let join_handle = std::thread::Builder::new()
        .name("localtrans-discovery".to_string())
        .spawn(move || {
            discovery_loop(socket, cfg, key, cmd_rx, devices_tx, shutdown_rx, stop_flag);
        })?;

    Ok(DiscoveryHandle {
        devices: devices_rx,
        cmd: cmd_tx,
        shutdown: shutdown_tx,
        stop,
        join_handle: Some(join_handle),
    })
}

fn discovery_loop(
    socket: std::net::UdpSocket,
    mut cfg: DiscoveryConfig,
    key: Arc<SigningKey>,
    mut cmd_rx: mpsc::Receiver<DiscoveryCmd>,
    devices_tx: watch::Sender<Vec<DeviceInfo>>,
    mut shutdown_rx: mpsc::Receiver<()>,
    stop: Arc<AtomicBool>,
) {
    use std::time::Duration;

    const STARTUP_PRESENCE_COUNT: u8 = 3;
    /// v0.2.9：100ms→250ms。读超时只决定"空闲时循环唤醒频率"，
    /// 250ms 对命令响应/收包延迟无感（人眼级），唤醒次数 10/s→4/s
    const READ_TIMEOUT: Duration = Duration::from_millis(250);
    /// 周期重探间隔——必须明显小于 offline_secs(15s)，
    /// 否则跨网段设备会在两次重探之间过期掉线
    const PROBE_RETRY_PERIOD: Duration = Duration::from_secs(8);
    const TARGET_CHECK_INTERVAL: Duration = Duration::from_secs(1);
    /// 重探目标上限，防止长期运行无限累积
    const MAX_PROBE_TARGETS: usize = 8;

    let pubkey_bytes = key.verifying_key().to_bytes();
    let fingerprint = cfg.fingerprint;

    let mut replay_guard = ReplayGuard::new();
    let mut devices: HashMap<[u8; 32], DeviceInfo> = HashMap::new();
    let mut buf = [0u8; 4096];

    // 本机发出过的 nonce(滑动窗口)。收到"本机指纹"的包时用它区分:
    // nonce 在列 → 自己的广播回环，正常;不在列 → 另一台设备复制了
    // 本机的 data/ 身份文件在跑——两头会互相把对方当"自己"过滤掉，
    // 表现为设备列表永远为空。真机踩过:整目录拷贝把 data/ 一起带走了
    let mut own_nonces: std::collections::VecDeque<[u8; 12]> = std::collections::VecDeque::new();

    let _ = socket.set_read_timeout(Some(READ_TIMEOUT));

    // 启动即连发 Presence,让对端立刻看到自己
    tracing::info!(
        "发现服务运行: 端口={} 广播目标={} QUIC端口={} 隐身={}",
        cfg.bind_port, cfg.target, cfg.quic_port, cfg.hidden.load(Ordering::Relaxed)
    );
    if !cfg.hidden.load(Ordering::Relaxed) {
        for _ in 0..STARTUP_PRESENCE_COUNT {
            let pkt = presence_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
            if let Err(e) = socket.send_to(&pkt, cfg.target) {
                tracing::warn!("Presence 发送失败 -> {}: {}", cfg.target, e);
            }
        }
    }
    let mut next_presence = Instant::now() + presence_period_for(false);
    // M3a FR6:静默期状态跟踪——仅用于周期切换时打一条 debug 日志
    let mut last_presence_silent: Option<bool> = None;

    // 周期重探目标：手动添加的地址 + 自动学到的设备地址。
    // 跨网段时对端的周期广播到不了本机，若只靠广播刷新，
    // 已发现设备会在 offline_secs(默认15s) 后从列表过期消失——
    // 真机表现为"刚看到对方，半分钟后又不见了"。
    // 值为该目标下一次到期重探的时刻。
    let mut probe_targets: HashMap<SocketAddr, Instant> = HashMap::new();
    let mut next_target_check = Instant::now();
    // v0.2.9：过期清除独立节拍（与 target_check 分开，避免共用条件时隔轮跳过）
    let mut next_expire_check = Instant::now();
    // 重探目标失联计数:连续 PROBE_FAIL_EVICTION 次探测无回应即移除。
    // 防陈旧地址永久残留(模拟器换了 IP/软件已卸载,目标还在表里反复空探)
    const PROBE_FAIL_EVICTION: u32 = 10; // 10 次 × 8s ≈ 80s 无回应即淘汰
    let mut probe_fail_counts: HashMap<SocketAddr, u32> = HashMap::new();

    // 跨网段对端靠单播重探保活；重启后内存表清空 = 互相“失忆”。
    // 从 data/probe_targets.json 恢复上次的探测目标，启动即探
    let persist_dir = cfg.data_dir.clone();
    if let Some(dir) = persist_dir.as_ref() {
        for addr in load_probe_targets(dir) {
            probe_targets.insert(addr, Instant::now());
        }
        if !probe_targets.is_empty() {
            tracing::info!("从 data/ 恢复 {} 个重探目标", probe_targets.len());
        }
    }

    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }

        // 非阻塞排空命令
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                DiscoveryCmd::ProbeNow => {
                    // M3a FR6 隐身语义修正:隐身只约束"被看见"(Presence 广播/ProbeResp 自隐),
                    // 出站探测放行——用户主动找人是显式行为,不应被隐身禁用
                    let pkt = probe_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
                    tracing::info!("发出广播探测 -> {}", cfg.target);
                    if let Err(e) = socket.send_to(&pkt, cfg.target) {
                        tracing::warn!("广播探测发送失败 -> {}: {}", cfg.target, e);
                    }
                }
                DiscoveryCmd::ProbeAddr(addr) => {
                    // M3a FR6 隐身语义修正:手动单播探测同上,隐身时放行
                    let pkt = probe_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
                    tracing::info!("发出单播探测 -> {}", addr);
                    if let Err(e) = socket.send_to(&pkt, addr) {
                        tracing::warn!("单播探测发送失败 -> {}: {}", addr, e);
                    }
                    // 记入周期重探：对方还没打开软件、或中途重启，
                    // 都由重探自动补上，不再需要人工反复手动添加
                    probe_targets.insert(addr, Instant::now() + PROBE_RETRY_PERIOD);
                    if let Some(dir) = persist_dir.as_ref() {
                        save_probe_targets(dir, &probe_targets);
                    }
                }
                DiscoveryCmd::SetName(name) => {
                    if name.trim().is_empty() {
                        // 空名拒绝——出站包带空名会让对端列表出现无名设备
                        tracing::warn!("SetName 拒绝空设备名");
                        continue;
                    }
                    tracing::info!("设备名变更即时生效: {:?} -> {:?}", cfg.name, name);
                    cfg.name = name;
                    // 连发 3 个 Presence 让对端立刻刷新(与启动 burst 同策略),
                    // 对端收到即覆盖设备表里的旧名
                    if !cfg.hidden.load(Ordering::Relaxed) {
                        for _ in 0..STARTUP_PRESENCE_COUNT {
                            let pkt = presence_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
                            if let Err(e) = socket.send_to(&pkt, cfg.target) {
                                tracing::warn!("改名 Presence 发送失败 -> {}: {}", cfg.target, e);
                            }
                        }
                    }
                    // 单播通知已知对端与重探目标——跨网段设备收不到广播,
                    // 但它们都在重探表里,定向补一发
                    for addr in probe_targets.keys() {
                        let pkt = presence_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
                        if let Err(e) = socket.send_to(&pkt, addr) {
                            tracing::warn!("改名单播 Presence 失败 -> {}: {}", addr, e);
                        }
                    }
                }
            }
        }

        if let Ok(()) = shutdown_rx.try_recv() {
            break;
        }

        // 100ms 读超时保持循环响应
        match socket.recv_from(&mut buf) {
            Ok((len, src_addr)) => {
                let now = now_ms();
                match verify_and_parse(&buf[..len], now, &mut replay_guard) {
                    Ok(pkt) => {
                        // 不把自己加入设备列表
                        if pkt.fingerprint != fingerprint {
                            // 三种包都携带完整身份信息——Probe 也入表，发现因此对称：
                            // 任何一方发一次探测，双方列表都会出现对方。
                            // （此前只有 ProbeResp 入表，被探测方要等自己也发一次
                            //   探测才能看到对方，跨网段场景极易表现为"单边可见"）
                            tracing::info!(
                                "发现设备: {} @ {} (来源={} 指纹={:02x}{:02x}{:02x}{:02x})",
                                pkt.name,
                                SocketAddr::new(src_addr.ip(), pkt.quic_port),
                                src_addr,
                                pkt.fingerprint[0], pkt.fingerprint[1], pkt.fingerprint[2], pkt.fingerprint[3]
                            );
                            devices.insert(
                                pkt.fingerprint,
                                DeviceInfo {
                                    name: pkt.name.clone(),
                                    fingerprint: pkt.fingerprint,
                                    // 连接地址必须用包内声明的 QUIC 端口——
                                    // src_addr 的端口是发现端口(47600)，QUIC 监听在
                                    // 另一个端口(47601)，误用会导致连接永远超时
                                    addr: SocketAddr::new(src_addr.ip(), pkt.quic_port),
                                    last_seen: Instant::now(),
                                },
                            );
                            publish_devices(&devices, &devices_tx);
                            // 对端有回应:清失联计数
                            probe_fail_counts.remove(&src_addr);
                            // 对端地址记入周期重探列表（已存在则保持原节奏）——
                            // 跨网段设备靠它刷新在线状态，而非到不了的广播。
                            // 新地址入表时持久化，重启后自动恢复
                            if !probe_targets.contains_key(&src_addr) {
                                probe_targets.insert(src_addr, Instant::now() + PROBE_RETRY_PERIOD);
                                if let Some(dir) = persist_dir.as_ref() {
                                    save_probe_targets(dir, &probe_targets);
                                }
                            }
                        } else if !own_nonces.iter().any(|n| *n == pkt.nonce) {
                            // 带本机指纹、却不是本机发出的包（自己的广播回环 nonce
                            // 一定在 own_nonces 里）——只能是另一台设备复制了本机的
                            // data/ 身份文件。双方会互相把对方当"自己"静默过滤，
                            // 设备列表永远为空，必须在日志里喊出来
                            tracing::warn!(
                                "身份冲突: 来自 {} 的包携带本机指纹但非本机所发——data/ 身份目录疑似被复制到多台设备，请在其中一台删除 data/ 后重启",
                                src_addr
                            );
                        }
                        if pkt.kind == PacketKind::Probe && !cfg.hidden.load(Ordering::Relaxed) {
                            // 收到探测:非隐身则单播回应
                            tracing::info!("收到探测来自 {}，回应 ProbeResp", src_addr);
                            let resp = DiscoveryPacket {
                                v: 1,
                                kind: PacketKind::ProbeResp,
                                name: cfg.name.clone(),
                                fingerprint,
                                pubkey: pubkey_bytes,
                                quic_port: cfg.quic_port,
                                ts_ms: now,
                                nonce: next_own_nonce(&mut own_nonces),
                            };
                            if let Err(e) = socket.send_to(&encode_and_sign(&resp, &key), src_addr) {
                                tracing::warn!("ProbeResp 发送失败 -> {}: {}", src_addr, e);
                            }
                        }
                    }
                    Err(e) => {
                        // 非法包丢弃，但记录原因——排障时"包到了但被拒"和"包没到"天差地别。
                        // Overflow 降为 debug:洪泛场景下每包一条 warn 会把日志一起淹掉
                        if matches!(e, DiscoveryError::Overflow) {
                            tracing::debug!("nonce 表已满,丢弃来自 {} 的发现包({} 字节)", src_addr, len);
                        } else {
                            tracing::warn!("丢弃来自 {} 的发现包({} 字节): {:?}", src_addr, len, e);
                        }
                    }
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => {
                // 其他错误:短暂退避,避免热循环
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        // 周期 Presence,隐身时跳过。节奏自适应(M3a FR6 静默期):
        // 设备表非空(有在线对端,活动传输必然依赖在线对端)→ 5s;
        // 表空(无任何在线对端)→ 15s,降空播能耗与信道占用。
        if !cfg.hidden.load(Ordering::Relaxed) && Instant::now() >= next_presence {
            let pkt = presence_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
            tracing::debug!("周期 Presence -> {}", cfg.target);
            if let Err(e) = socket.send_to(&pkt, cfg.target) {
                tracing::warn!("周期 Presence 发送失败 -> {}: {}", cfg.target, e);
            }
            let silent = devices.is_empty();
            if last_presence_silent != Some(silent) {
                tracing::debug!(
                    "Presence 周期切换: {}(设备表 {} 项)",
                    if silent { "5s -> 15s 进入静默期" } else { "15s -> 5s 恢复活跃" },
                    devices.len()
                );
                last_presence_silent = Some(silent);
            }
            next_presence = Instant::now() + presence_period_for(!silent);
        }

        // 周期重探到期目标——手动添加的地址与已发现的跨网段设备。
        // 发送节奏由每个目标的到期时刻控制，这里每秒检查一次
        if Instant::now() >= next_target_check {
            next_target_check = Instant::now() + TARGET_CHECK_INTERVAL;
            // 超出上限时丢弃任意目标，防止长期运行无限累积
            let mut evicted = false;
            while probe_targets.len() > MAX_PROBE_TARGETS {
                let victim = probe_targets.keys().next().copied();
                match victim {
                    Some(addr) => {
                        probe_targets.remove(&addr);
                        evicted = true;
                    }
                    None => break,
                }
            }
            if evicted {
                if let Some(dir) = persist_dir.as_ref() {
                    save_probe_targets(dir, &probe_targets);
                }
            }
            for (addr, next_due) in probe_targets.iter_mut() {
                if Instant::now() >= *next_due {
                    // M3a FR6 隐身语义修正:周期重探同为出站探测,隐身时放行——
                    // 否则隐身设备重启后对跨网段已配对对端永久失忆(探测不到=连不上)
                    let pkt = probe_packet(&cfg, &key, fingerprint, pubkey_bytes, next_own_nonce(&mut own_nonces));
                    tracing::debug!("周期重探 -> {}", addr);
                    if let Err(e) = socket.send_to(&pkt, addr) {
                        tracing::warn!("周期重探发送失败 -> {}: {}", addr, e);
                    }
                    *next_due = Instant::now() + PROBE_RETRY_PERIOD;
                    // 失联计数:上次探测周期内没收到该地址的任何包则 +1。
                    // (设备表里有该地址的活跃设备说明对方其实在线,不计)
                    let peer_alive = devices.values().any(|d| d.addr.ip() == addr.ip());
                    if !peer_alive {
                        let fails = probe_fail_counts.entry(*addr).or_insert(0);
                        *fails += 1;
                    }
                }
            }
            // 连续失联达到阈值:淘汰目标并落盘(收包路径会清零计数,
            // 对方一旦回应立即恢复)
            let mut evicted_stale = false;
            probe_fail_counts.retain(|addr, fails| {
                if *fails >= PROBE_FAIL_EVICTION {
                    if probe_targets.remove(addr).is_some() {
                        tracing::info!("重探目标连续 {} 次无回应,移除: {}", fails, addr);
                        evicted_stale = true;
                    }
                    return false; // 计数一并清除
                }
                true
            });
            if evicted_stale {
                if let Some(dir) = persist_dir.as_ref() {
                    save_probe_targets(dir, &probe_targets);
                }
            }
        }

        // 过期设备清除——v0.2.9 挪进独立 1s 节拍（此前每 250ms 循环都
        // retain 一遍纯属浪费；offline_secs 是 15s 级粒度，1s 检查绰绰有余）
        if Instant::now() >= next_expire_check {
            next_expire_check = Instant::now() + TARGET_CHECK_INTERVAL;
            let before = devices.len();
            let timeout = Duration::from_secs(cfg.offline_secs);
            let now = Instant::now();
            devices.retain(|_, device| now.duration_since(device.last_seen) < timeout);
            if devices.len() != before {
                publish_devices(&devices, &devices_tx);
            }
        }
    }
}

fn presence_packet(
    cfg: &DiscoveryConfig,
    key: &SigningKey,
    fingerprint: [u8; 32],
    pubkey_bytes: [u8; 32],
    nonce: [u8; 12],
) -> Vec<u8> {
    encode_and_sign(
        &DiscoveryPacket {
            v: 1,
            kind: PacketKind::Presence,
            name: cfg.name.clone(),
            fingerprint,
            pubkey: pubkey_bytes,
            quic_port: cfg.quic_port,
            ts_ms: now_ms(),
            nonce,
        },
        key,
    )
}

fn probe_packet(
    cfg: &DiscoveryConfig,
    key: &SigningKey,
    fingerprint: [u8; 32],
    pubkey_bytes: [u8; 32],
    nonce: [u8; 12],
) -> Vec<u8> {
    encode_and_sign(
        &DiscoveryPacket {
            v: 1,
            kind: PacketKind::Probe,
            name: cfg.name.clone(),
            fingerprint,
            pubkey: pubkey_bytes,
            quic_port: cfg.quic_port,
            ts_ms: now_ms(),
            nonce,
        },
        key,
    )
}

/// 生成并登记一个本机 nonce——所有出站包必须经由此函数取 nonce，
/// 才能在收到"本机指纹"的包时区分自己的广播回环与身份被复制的对端
fn next_own_nonce(own: &mut std::collections::VecDeque<[u8; 12]>) -> [u8; 12] {
    let n = random_nonce();
    own.push_back(n);
    if own.len() > 128 {
        own.pop_front();
    }
    n
}

/// 重探目标持久化文件路径（跟随 data/ 目录）
fn probe_targets_path(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join("probe_targets.json")
}

/// 保存重探目标集合（集合变化时调用，量小直接整写）
fn save_probe_targets(dir: &std::path::Path, targets: &HashMap<SocketAddr, Instant>) {
    let list: Vec<String> = targets.keys().map(|a| a.to_string()).collect();
    let path = probe_targets_path(dir);
    match serde_json::to_string_pretty(&list) {
        Ok(text) => {
            if let Err(e) = std::fs::write(&path, text) {
                tracing::warn!("重探目标保存失败 {}: {}", path.display(), e);
            }
        }
        Err(e) => tracing::warn!("重探目标序列化失败: {}", e),
    }
}

/// 读取上次保存的重探目标（文件缺失/损坏按空集处理，不阻断启动）
fn load_probe_targets(dir: &std::path::Path) -> Vec<SocketAddr> {
    let Ok(text) = std::fs::read_to_string(probe_targets_path(dir)) else {
        return Vec::new();
    };
    match serde_json::from_str(&text) {
        Ok(list) => list,
        Err(e) => {
            tracing::warn!("重探目标文件损坏，忽略: {}", e);
            Vec::new()
        }
    }
}

/// Presence 周期(M3a FR6 静默期自适应):有在线对端 → 5s±1s(原节奏,贴近);
/// 无任何在线对端(设备表空)→ 15s±1s 静默期,降空播能耗与信道占用。
/// 判定数据源=现有设备表(在线对端必在表内;活动传输依赖在线对端,无需单独判)。
/// Bootstrap 不受影响:启动/改名 burst 与 ProbeResp 照常,对端上线即被立刻看见。
fn presence_period_for(peers_online: bool) -> std::time::Duration {
    use rand::Rng;
    use std::time::Duration;
    let base_ms: u64 = if peers_online { 4000 } else { 14000 };
    Duration::from_millis(base_ms + OsRng.gen_range(0..2000))
}

fn publish_devices(devices: &HashMap<[u8; 32], DeviceInfo>, devices_tx: &watch::Sender<Vec<DeviceInfo>>) {
    let mut device_list: Vec<DeviceInfo> = devices.values().cloned().collect();
    device_list.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = devices_tx.send(device_list);
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn random_nonce() -> [u8; 12] {
    let mut nonce = [0u8; 12];
    let mut rng = OsRng;
    use rand::RngCore;
    rng.fill_bytes(&mut nonce);
    nonce
}

#[cfg(test)]
mod service_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::time::Duration;

    /// 测试用指纹:SHA-256(公钥)。生产语义是 SHA-256(证书 DER,见
    /// DiscoveryConfig::fingerprint 注释),但发现层把指纹当 opaque
    /// 标识透传,测试只需唯一且稳定,故以公钥哈希代替,不依赖 identity 层。
    fn test_fp(key: &SigningKey) -> [u8; 32] {
        let mut fp = [0u8; 32];
        fp.copy_from_slice(&Sha256::digest(key.verifying_key().to_bytes()));
        fp
    }

    #[tokio::test]
    async fn two_services_discover_each_other() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        let a = spawn(DiscoveryConfig {
            bind_port: 14760,
            target: "127.0.0.1:14761".parse().unwrap(),
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24761,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14761,
            target: "127.0.0.1:14760".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 24762,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        // 等待两端启动 burst 完成(服务线程独立于本运行时运行)
        tokio::time::sleep(Duration::from_millis(100)).await;

        // 主动触发一次探测,不依赖 burst 时序
        let _ = a.cmd.send(DiscoveryCmd::ProbeNow).await;

        // 轮询等待 B 发现 A(异步 sleep 让出;服务在线程中,不受影响)
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(5) {
            let list = b.devices.borrow().clone();
            if let Some(d) = list.iter().find(|d| d.name == "甲") {
                // 回归:连接地址必须是 A 声明的 QUIC 端口(24761)，
                // 而不是发现包源端口(14760)——误用源端口曾导致真机连接必超时
                assert_eq!(
                    d.addr,
                    SocketAddr::new(std::net::IpAddr::from([127, 0, 0, 1]), 24761),
                    "设备表中的连接地址应为 QUIC 端口"
                );
                return; // Success!
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // If we get here, timeout occurred
        panic!("Timeout: Service B did not discover Service A within 5 seconds");
    }

    #[tokio::test]
    async fn hidden_device_not_listed_but_can_see() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        // Device B is hidden
        let hidden = Arc::new(AtomicBool::new(true));

        let a = spawn(DiscoveryConfig {
            bind_port: 14770,
            target: "127.0.0.1:14771".parse().unwrap(),
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24771,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14771,
            target: "127.0.0.1:14770".parse().unwrap(),
            hidden: hidden.clone(),
            name: "乙".into(),
            quic_port: 24772,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        // A probes B
        a.cmd.send(DiscoveryCmd::ProbeNow).await.unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        // A should NOT see B (B is hidden and doesn't respond)
        let a_list = a.devices.borrow().clone();
        assert!(!a_list.iter().any(|d| d.name == "乙"), "甲 不应看到隐藏的乙");

        // B should still see A (hidden devices listen but don't announce)
        let b_list = b.devices.borrow().clone();
        assert!(b_list.iter().any(|d| d.name == "甲"), "乙(隐藏) 应看到甲");

        // Properly shutdown both services and wait for completion
        let _ = tokio::join!(a.shutdown.send(()), b.shutdown.send(()));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn stale_entries_expire() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        // Device B will be stopped quickly
        let a = spawn(DiscoveryConfig {
            bind_port: 14780,
            target: "127.0.0.1:14781".parse().unwrap(),
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24781,
            offline_secs: 1, // Short timeout for testing
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14781,
            target: "127.0.0.1:14780".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 24782,
            offline_secs: 1,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        // A probes B and discovers it
        a.cmd.send(DiscoveryCmd::ProbeNow).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let a_list = a.devices.borrow().clone();
        assert!(a_list.iter().any(|d| d.name == "乙"), "甲 应看到乙");

        // Shutdown B
        b.shutdown.send(()).await.unwrap();

        // Wait for B to expire (1 second offline_secs + small margin)
        tokio::time::sleep(Duration::from_secs(2)).await;

        // A should no longer see B
        let a_list = a.devices.borrow().clone();
        assert!(!a_list.iter().any(|d| d.name == "乙"), "甲 不应看到过期的乙");

        // Properly shutdown service and wait for completion
        let _ = a.shutdown.send(());
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn probe_addr_sends_unicast_probe() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        let a = spawn(DiscoveryConfig {
            bind_port: 14800,
            target: "127.0.0.1:14801".parse().unwrap(), // This won't be used for ProbeAddr
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24801,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14801,
            target: "127.0.0.1:14800".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 24802,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        // A sends unicast probe directly to B's address
        a.cmd.send(DiscoveryCmd::ProbeAddr("127.0.0.1:14801".parse().unwrap())).await.unwrap();

        tokio::time::sleep(Duration::from_secs(2)).await;

        // Check if Service B sees Service A (via Presence from startup burst or ProbeResp)
        let list = b.devices.borrow().clone();
        assert!(list.iter().any(|d| d.name == "甲"), "乙 应看到甲");

        // Properly shutdown both services and wait for completion
        let _ = tokio::join!(a.shutdown.send(()), b.shutdown.send(()));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    #[tokio::test]
    async fn single_probe_lists_both_sides() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        // 双方广播都指向无人端口——隔离出"单次单播探测"的纯粹效果
        let a = spawn(DiscoveryConfig {
            bind_port: 14810,
            target: "127.0.0.1:14899".parse().unwrap(),
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24811,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14811,
            target: "127.0.0.1:14899".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 24812,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;

        // 唯一一次跨服务交互：A 单播探测 B
        a.cmd.send(DiscoveryCmd::ProbeAddr("127.0.0.1:14811".parse().unwrap())).await.unwrap();

        // 双方列表都应出现对方——此前只有探测方(A)能看到被探测方(B)，
        // 被探测方要等自己也发探测才可见（跨网段真机的"单边可见"根源）
        let start = Instant::now();
        loop {
            let a_sees_b = a.devices.borrow().iter().any(|d| d.name == "乙");
            let b_sees_a = b.devices.borrow().iter().any(|d| d.name == "甲");
            if a_sees_b && b_sees_a {
                break;
            }
            if start.elapsed() > Duration::from_secs(5) {
                panic!("单次探测后应双边可见: 甲看到乙={a_sees_b} 乙看到甲={b_sees_a}");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let _ = tokio::join!(a.shutdown.send(()), b.shutdown.send(()));
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    /// 回归（v0.1.6 实机）：跨网段对端靠单播重探保活，受限广播到不了对方。
    /// 双方重启后内存目标表清空 = 互相"失忆"。目标必须持久化到 data/ 并在
    /// 启动时恢复——本测试验证 手动添加→落盘→重启→自动探测 全链路。
    #[tokio::test]
    async fn probe_targets_persist_across_restart() {
        let dir = tempfile::tempdir().unwrap();
        let key = Arc::new(SigningKey::generate(&mut OsRng));

        // 假对端：任意端口收探测
        let peer_sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let peer_addr = peer_sock.local_addr().unwrap();
        peer_sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();

        // 第一次运行：手动添加对端地址 → 应写入 probe_targets.json
        let a = spawn(DiscoveryConfig {
            bind_port: 14820,
            target: "127.0.0.1:14899".parse().unwrap(), // 死地址，探测会失败但目标仍记录
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24820,
            offline_secs: 15,
            data_dir: Some(dir.path().to_path_buf()),
            fingerprint: test_fp(&key),
        }, key.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;

        a.cmd.send(DiscoveryCmd::ProbeAddr(peer_addr)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        let _ = a.shutdown.send(());
        tokio::time::sleep(Duration::from_millis(200)).await;

        let saved = std::fs::read_to_string(dir.path().join("probe_targets.json")).unwrap();
        assert!(saved.contains(&peer_addr.to_string()), "手动添加的目标应落盘: {}", saved);

        // 第二次运行（"重启"）：不手动添加，启动即应向保存的地址发探测
        let b = spawn(DiscoveryConfig {
            bind_port: 14820,
            target: "127.0.0.1:14899".parse().unwrap(),
            hidden: Default::default(),
            name: "甲".into(),
            quic_port: 24820,
            offline_secs: 15,
            data_dir: Some(dir.path().to_path_buf()),
            fingerprint: test_fp(&key),
        }, key).unwrap();

        let mut buf = [0u8; 1024];
        let start = Instant::now();
        let mut got_probe = false;
        while start.elapsed() < Duration::from_secs(5) {
            if let Ok((len, from)) = peer_sock.recv_from(&mut buf) {
                assert_eq!(from.port(), 14820, "探测应来自发现服务端口");
                let mut guard = ReplayGuard::new();
                if verify_and_parse(&buf[..len], now_ms(), &mut guard).is_ok() {
                    got_probe = true;
                    break;
                }
            }
        }
        let _ = b.shutdown.send(());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(got_probe, "重启后应自动向持久化目标发探测");
    }

    /// SetName 即时生效:A 改名后,B 无需等 A 重启就应该看到新名字。
    /// 改名触发连发 Presence(burst),B 收到后设备表覆盖旧名。
    #[tokio::test]
    async fn set_name_takes_effect_immediately() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        let a = spawn(DiscoveryConfig {
            bind_port: 14830,
            target: "127.0.0.1:14831".parse().unwrap(),
            hidden: Default::default(),
            name: "旧名字".into(),
            quic_port: 24831,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        let b = spawn(DiscoveryConfig {
            bind_port: 14831,
            target: "127.0.0.1:14830".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 24832,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();

        // 先确认 B 看到 A 的旧名字(轮询等待——固定 sleep 在高负载下不够,
        // 首个 presence 到达时间随调度波动,实测 env 偏移下 200ms 偶发不足)
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(10) {
            if b.devices.borrow().iter().any(|d| d.name == "旧名字") { break; }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            b.devices.borrow().iter().any(|d| d.name == "旧名字"),
            "前置条件:B 应先看到 A 的旧名字"
        );

        // A 改名 → B 应在秒级看到新名字(不等 A 重启)
        a.cmd.send(DiscoveryCmd::SetName("新名字".into())).await.unwrap();

        let start = Instant::now();
        loop {
            if b.devices.borrow().iter().any(|d| d.name == "新名字") { break; }
            if start.elapsed() > Duration::from_secs(5) {
                panic!("SetName 后 5s 内 B 仍未看到新名字");
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        // 新名字出现的同时,旧名字应消失(同一指纹覆盖)
        assert!(
            !b.devices.borrow().iter().any(|d| d.name == "旧名字"),
            "改名后旧名字应被同指纹覆盖,不得出现新旧两个条目"
        );

        let _ = tokio::join!(a.shutdown.send(()), b.shutdown.send(()));
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    /// SetName 空名拒绝:出站包不得带空名(否则对端列表出现无名设备)
    #[tokio::test]
    async fn set_name_rejects_empty() {
        let key = Arc::new(SigningKey::generate(&mut OsRng));
        let a = spawn(DiscoveryConfig {
            bind_port: 14840,
            target: "127.0.0.1:14899".parse().unwrap(),
            hidden: Default::default(),
            name: "原名".into(),
            quic_port: 24841,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&key),
        }, key.clone()).unwrap();

        tokio::time::sleep(Duration::from_millis(100)).await;
        // 空名/纯空白名都不应改名成功——服务内部保留原名
        // (无外部可观测通道直接读内部名,以"不 panic + 后续 SetName 正常"
        //  的行为验证;这里至少验证命令通道不挂)
        let _ = a.cmd.send(DiscoveryCmd::SetName("   ".into())).await;
        a.cmd.send(DiscoveryCmd::SetName("改名OK".into())).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let _ = a.shutdown.send(());
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    /// M3a FR6 静默期:周期选择纯函数——有在线对端 4-6s(原节奏),设备表空 14-16s。
    #[test]
    fn presence_period_selects_by_peer_state() {
        for _ in 0..50 {
            let active = presence_period_for(true);
            assert!(
                active >= Duration::from_millis(4000) && active <= Duration::from_millis(6000),
                "活跃期(有在线对端)应为 4-6s,实得 {:?}",
                active
            );
            let silent = presence_period_for(false);
            assert!(
                silent >= Duration::from_millis(14000) && silent <= Duration::from_millis(16000),
                "静默期(设备表空)应为 14-16s,实得 {:?}",
                silent
            );
        }
    }

    /// M3a FR6 隐身语义修正:隐身只约束"被看见"(不广播 Presence、被探测不回应
    /// ProbeResp),出站探测放行——用户主动找人是显式行为,不应被隐身禁用。
    #[tokio::test]
    async fn hidden_device_probes_outbound_but_stays_hidden_from_probes() {
        let (k1, k2) = (
            Arc::new(SigningKey::generate(&mut OsRng)),
            Arc::new(SigningKey::generate(&mut OsRng))
        );

        // B(正常)先启动:启动 burst 在 A 起来之前完成,不干扰断言。
        // 端口 1683x 独占(set_name 等测试用 1483x;SO_REUSEADDR 下并行同端口
        // 单播会被抢占,历史教训——勿复用他测端口)
        let b = spawn(DiscoveryConfig {
            bind_port: 16831,
            target: "127.0.0.1:16830".parse().unwrap(),
            hidden: Default::default(),
            name: "乙".into(),
            quic_port: 26832,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k2),
        }, k2.clone()).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;

        // A(隐身)后启动:不发 burst、不周期广播、被探测不回应——出站探测放行
        let a = spawn(DiscoveryConfig {
            bind_port: 16830,
            target: "127.0.0.1:16831".parse().unwrap(),
            hidden: Arc::new(AtomicBool::new(true)),
            name: "甲".into(),
            quic_port: 26831,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k1),
        }, k1.clone()).unwrap();

        // 出站放行面:A 主动广播探测 → B 回 ProbeResp → A 应能看到乙。
        // 若出站仍被隐身门控拦下:A 无探测可发 → 设备表保持空(红)。
        // 轮询等待(全量并行时线程调度抖动大,固定 sleep 会误报);期间补发探测,
        // 兼顾极端负载下的丢包。
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut a_saw_b = false;
        let mut a_list;
        loop {
            a_list = a.devices.borrow().clone();
            if a_list.iter().any(|d| d.name == "乙") {
                a_saw_b = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            let _ = a.cmd.send(DiscoveryCmd::ProbeNow).await;
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        assert!(
            a_saw_b,
            "隐身设备的出站探测应放行,A 应经 ProbeResp 看到乙(实得 {:?})",
            a_list
        );

        // 入站自隐面(不变):丙(正常,未听过甲)探测隐身 A → A 不回 ProbeResp
        // → 丙看不到甲。用未听过甲的第三方隔离验证 ProbeResp 抑制不受出站
        // 放行影响(B 表里有甲属"出站放行"的预期结果,不作本断言依据)。
        let k3 = Arc::new(SigningKey::generate(&mut OsRng));
        let c = spawn(DiscoveryConfig {
            bind_port: 16832,
            target: "127.0.0.1:16830".parse().unwrap(),
            hidden: Default::default(),
            name: "丙".into(),
            quic_port: 26833,
            offline_secs: 15,
            data_dir: None,
            fingerprint: test_fp(&k3),
        }, k3.clone()).unwrap();
        c.cmd.send(DiscoveryCmd::ProbeNow).await.unwrap();
        // 负向断言:固定观察窗内( ProbeResp 若有早该到)丙表恒无甲
        tokio::time::sleep(Duration::from_millis(1000)).await;
        let c_list = c.devices.borrow().clone();
        assert!(
            c_list.iter().all(|d| d.name != "甲"),
            "隐身设备被探测必须不回 ProbeResp,丙不应看到甲(实得 {:?})",
            c_list
        );

        let _ = tokio::join!(a.shutdown.send(()), b.shutdown.send(()), c.shutdown.send(()));
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}