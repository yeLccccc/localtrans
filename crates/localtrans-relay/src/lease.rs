//! 租约表:指纹 ↔ 数据面端口/令牌,含过期回收与名册快照。
//! 纯 std 同步锁——临界区都是微级操作,不值得上 tokio 锁。

use localtrans_core::relay::proto::{LeaseInfo, RemoteDevice};
use rand::RngCore;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::RwLock;
use std::time::{Duration, Instant};

/// 数据面包载荷上限(含 18B 头总共 1518,防反射放大)
pub const DATA_PACKET_LIMIT: usize = 1500;

pub fn data_packet_ok(payload: &[u8]) -> bool {
    payload.len() <= DATA_PACKET_LIMIT
}

pub struct LeaseEntry {
    pub fingerprint: [u8; 32],
    pub name: String,
    pub port: u16,
    pub token: [u8; 16],
    pub last_active: Instant,
    /// 最近一次 KNOCK 的来源地址(学得的公网出口)
    pub punch_from: Option<SocketAddr>,
    /// 控制连接最近一次 Ping(独立于数据面活跃)
    pub last_ping: Instant,
    /// 隐身注册:名册剔除本设备(连接/打洞能力保留)
    pub hidden: bool,
}

pub struct AllocatedLease {
    pub port: u16,
    pub token: [u8; 16],
}

pub struct LeaseTable {
    /// fp → entry
    pub leases: RwLock<HashMap<[u8; 32], LeaseEntry>>,
    /// 端口池(可分配范围)
    port_range: std::ops::Range<u16>,
    /// 端口 → fp(反查:数据面包到达端口后定位租约)
    pub port_map: RwLock<HashMap<u16, [u8; 32]>>,
    /// 令牌 → fp(反查:验包)
    pub token_map: RwLock<HashMap<[u8; 16], [u8; 32]>>,
    /// 令牌累计签发数(诊断)
    pub tokens_accepted_count: std::sync::atomic::AtomicU64,
    /// 会话端口集合(与设备租约端口共用同一池)
    session_ports: RwLock<std::collections::HashSet<u16>>,
    /// 会话成员登记:port → (请求方 fp, 目标 fp)——数据面准入依据(S3)
    pub session_members: RwLock<HashMap<u16, ([u8; 32], [u8; 32])>>,
    /// 会话端口最近活跃时刻(KNOCK/DATA 刷新;TTL 回收依据,S6)
    session_activity: RwLock<HashMap<u16, Instant>>,
    /// 非成员包拒绝计数(诊断,S3)
    pub nonmember_reject_count: std::sync::atomic::AtomicU64,
}

impl LeaseTable {
    pub fn new(port_range: std::ops::Range<u16>) -> Self {
        LeaseTable {
            leases: RwLock::new(HashMap::new()),
            port_range,
            port_map: RwLock::new(HashMap::new()),
            token_map: RwLock::new(HashMap::new()),
            tokens_accepted_count: std::sync::atomic::AtomicU64::new(0),
            session_ports: RwLock::new(std::collections::HashSet::new()),
            session_members: RwLock::new(HashMap::new()),
            session_activity: RwLock::new(HashMap::new()),
            nonmember_reject_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// 分配租约;端口耗尽返回 None
    pub fn alloc(&self, fp: [u8; 32], name: String, hidden: bool) -> Option<AllocatedLease> {
        let mut leases = self.leases.write().unwrap();
        let mut port_map = self.port_map.write().unwrap();
        let mut token_map = self.token_map.write().unwrap();

        // 已有租约:换新令牌重签(旧令牌交接待回收——控制面负责宽限)
        if let Some(existing) = leases.get_mut(&fp) {
            token_map.remove(&existing.token);
            let mut token = [0u8; 16];
            rand::thread_rng().fill_bytes(&mut token);
            existing.token = token;
            existing.name = name;
            existing.hidden = hidden;
            existing.last_active = Instant::now();
            existing.last_ping = Instant::now();
            token_map.insert(token, fp);
            return Some(AllocatedLease { port: existing.port, token });
        }

        // 找空闲端口
        let used: std::collections::HashSet<u16> = port_map.keys().copied().collect();
        let port = self.port_range.clone().find(|p| !used.contains(p))?;

        let mut token = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut token);
        token_map.insert(token, fp);
        port_map.insert(port, fp);
        leases.insert(fp, LeaseEntry {
            fingerprint: fp,
            name,
            port,
            token,
            last_active: Instant::now(),
            punch_from: None,
            last_ping: Instant::now(),
            hidden,
        });
        Some(AllocatedLease { port, token })
    }

    pub fn get_by_port(&self, port: u16) -> Option<[u8; 32]> {
        self.port_map.read().unwrap().get(&port).copied()
    }

    /// 验令牌 → 指纹;命中即刷新数据面活跃时间
    pub fn get_by_token(&self, token: &[u8; 16]) -> Option<[u8; 32]> {
        let fp = { *self.token_map.read().unwrap().get(token)? };
        if let Some(e) = self.leases.write().unwrap().get_mut(&fp) {
            e.last_active = Instant::now();
        }
        self.tokens_accepted_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(fp)
    }

    pub fn remove(&self, fp: &[u8; 32]) -> bool {
        let mut leases = self.leases.write().unwrap();
        if let Some(e) = leases.remove(fp) {
            drop(leases);
            self.port_map.write().unwrap().remove(&e.port);
            self.token_map.write().unwrap().remove(&e.token);
            true
        } else {
            false
        }
    }

    /// 记录 KNOCK 来源地址
    pub fn punch_seen(&self, fp: &[u8; 32], from: SocketAddr) -> bool {
        match self.leases.write().unwrap().get_mut(fp) {
            Some(e) => { e.punch_from = Some(from); true }
            None => false,
        }
    }

    /// 控制面 Ping:刷新 last_ping(独立于数据面活跃)
    pub fn heartbeat(&self, fp: &[u8; 32]) {
        if let Some(e) = self.leases.write().unwrap().get_mut(fp) {
            e.last_ping = Instant::now();
        }
    }

    /// 回收 ttl 内无数据面活动且无 Ping 的租约,返回被回收指纹(供名册推送)
    pub fn reap_expired(&self, ttl: Duration) -> Vec<[u8; 32]> {
        let now = Instant::now();
        let expired: Vec<[u8; 32]> = {
            let leases = self.leases.read().unwrap();
            leases.iter()
                .filter(|(_, e)| now.duration_since(e.last_active) > ttl && now.duration_since(e.last_ping) > ttl)
                .map(|(fp, _)| *fp)
                .collect()
        };
        for fp in &expired { self.remove(fp); }
        // S6:空闲会话端口连带回收(不依赖 GOODBYE——断电/断网也能收回端口)
        {
            let mut idle_ports: Vec<u16> = Vec::new();
            let activity = self.session_activity.read().unwrap();
            for (&port, &last) in activity.iter() {
                if now.duration_since(last) > ttl * 2 {
                    idle_ports.push(port);
                }
            }
            drop(activity);
            for port in idle_ports {
                tracing::info!("会话端口 {} 空闲超 2×ttl,回收", port);
                self.remove_session_port(port);
            }
        }
        expired
    }

    /// 名册快照(排除自己;地址用对外公网 IP + 租约端口)
    pub fn snapshot_roster(&self, exclude_fp: &[u8; 32], public_ip: IpAddr) -> Vec<RemoteDevice> {
        let leases = self.leases.read().unwrap();
        leases.iter()
            .filter(|(fp, e)| *fp != exclude_fp && !e.hidden)
            .map(|(fp, e)| RemoteDevice {
                fingerprint: *fp,
                name: e.name.clone(),
                lease_addr: SocketAddr::new(public_ip, e.port),
                relayed_ephemeral: false,
            })
            .collect()
    }

    pub fn lease_count(&self) -> usize {
        self.leases.read().unwrap().len()
    }

    /// 低危审计修复:查询指纹是否处于隐身注册(Punch 统一"不可达"判定用)
    pub fn is_hidden(&self, fp: &[u8; 32]) -> bool {
        self.leases.read().unwrap().get(fp)
            .map(|e| e.hidden)
            .unwrap_or(false)
    }

    /// 为 RegisterAck 构造 LeaseInfo
    pub fn lease_info(&self, fp: &[u8; 32]) -> Option<LeaseInfo> {
        self.leases.read().unwrap().get(fp).map(|e| LeaseInfo {
            data_port: e.port,
            token: e.token,
        })
    }

    /// 从端口池分配会话端口并登记双方成员(与设备租约端口不冲突)
    pub fn alloc_session_port(&self, fp_a: [u8; 32], fp_b: [u8; 32]) -> Option<u16> {
        let mut session_ports = self.session_ports.write().unwrap();
        let port_map = self.port_map.read().unwrap();
        let used: std::collections::HashSet<u16> = port_map.keys().copied().collect();
        let session_used: std::collections::HashSet<u16> = session_ports.iter().copied().collect();

        let port = self.port_range.clone()
            .find(|p| !used.contains(p) && !session_used.contains(p))?;

        session_ports.insert(port);
        self.session_members.write().unwrap().insert(port, (fp_a, fp_b));
        self.session_activity.write().unwrap().insert(port, Instant::now());
        Some(port)
    }

    /// 回收会话端口(连带成员登记与活跃时刻)
    pub fn remove_session_port(&self, port: u16) {
        self.session_ports.write().unwrap().remove(&port);
        self.session_members.write().unwrap().remove(&port);
        self.session_activity.write().unwrap().remove(&port);
    }

    /// 查询某端口的会话成员
    pub fn session_members_of(&self, port: u16) -> Option<([u8; 32], [u8; 32])> {
        self.session_members.read().unwrap().get(&port).copied()
    }

    /// 刷新会话端口活跃时刻(KNOCK/DATA 到达即调用,S6)
    pub fn touch_session(&self, port: u16) {
        if let Some(a) = self.session_activity.write().unwrap().get_mut(&port) {
            *a = Instant::now();
        }
    }

    /// 会话成员准入校验(S3):src_fp 是该端口成员之一才放行
    pub fn session_member_ok(&self, port: u16, src_fp: &[u8; 32]) -> bool {
        match self.session_member_ok_inner(port, src_fp) {
            true => true,
            false => {
                self.nonmember_reject_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                false
            }
        }
    }

    fn session_member_ok_inner(&self, port: u16, src_fp: &[u8; 32]) -> bool {
        match self.session_members_of(port) {
            Some((a, b)) => a == *src_fp || b == *src_fp,
            None => false,
        }
    }

    /// 查询某端口是否为会话端口(测试用)
    pub fn is_session_port(&self, port: u16) -> bool {
        self.session_ports.read().unwrap().contains(&port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::SocketAddr;

    fn peer_addr(port: u16) -> SocketAddr {
        format!("127.0.0.1:{}", port).parse().unwrap()
    }

    #[test]
    fn alloc_assigns_unique_ports_and_tokens() {
        let t = LeaseTable::new(9000..9010);
        let l1 = t.alloc([1u8; 32], "A".into(), false).unwrap();
        let l2 = t.alloc([2u8; 32], "B".into(), false).unwrap();
        assert_ne!(l1.port, l2.port);
        assert_ne!(l1.token, l2.token);
        assert!((9000..9010).contains(&l1.port));
        assert!((9000..9010).contains(&l2.port));
    }

    #[test]
    fn alloc_exhausts_returns_none() {
        let t = LeaseTable::new(9000..9002); // 只有 2 个端口
        assert!(t.alloc([1u8; 32], "A".into(), false).is_some());
        assert!(t.alloc([2u8; 32], "B".into(), false).is_some());
        assert!(t.alloc([3u8; 32], "C".into(), false).is_none());
    }

    #[test]
    fn token_lookup_roundtrip() {
        let t = LeaseTable::new(9000..9010);
        let l = t.alloc([1u8; 32], "A".into(), false).unwrap();
        assert_eq!(t.get_by_token(&l.token).unwrap(), [1u8; 32]);
        assert!(t.get_by_token(&[0u8; 16]).is_none());
    }

    #[test]
    fn remove_frees_port_for_reuse() {
        let t = LeaseTable::new(9000..9001); // 1 个端口
        let l = t.alloc([1u8; 32], "A".into(), false).unwrap();
        assert!(t.remove(&[1u8; 32]));
        let l2 = t.alloc([2u8; 32], "B".into(), false).unwrap();
        assert_eq!(l.port, l2.port); // 端口复用
        assert_ne!(l.token, l2.token); // 令牌必须换新
    }

    #[test]
    fn heartbeat_and_reap() {
        let t = LeaseTable::new(9000..9010);
        let l = t.alloc([1u8; 32], "A".into(), false).unwrap();
        t.heartbeat(&[1u8; 32]);
        // 未过期的租约不被回收
        let gone = t.reap_expired(std::time::Duration::from_secs(45));
        assert!(gone.is_empty());
        // 伪造 60s 前的活跃时间和 ping → 被回收
        {
            let mut leases = t.leases.write().unwrap();
            let entry = leases.get_mut(&[1u8; 32]).unwrap();
            entry.last_active = std::time::Instant::now() - std::time::Duration::from_secs(60);
            entry.last_ping = std::time::Instant::now() - std::time::Duration::from_secs(60);
        }
        let gone = t.reap_expired(std::time::Duration::from_secs(45));
        assert_eq!(gone, vec![[1u8; 32]]);
        assert!(t.get_by_token(&l.token).is_none());
    }

    #[test]
    fn roster_snapshot_excludes_self() {
        let t = LeaseTable::new(9000..9010);
        t.alloc([1u8; 32], "A".into(), false).unwrap();
        t.alloc([2u8; 32], "B".into(), false).unwrap();
        let roster = t.snapshot_roster(&[2u8; 32], "5.6.7.8".parse().unwrap());
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].fingerprint, [1u8; 32]);
        assert_eq!(roster[0].lease_addr.port(), 9000);
        assert!(roster[0].lease_addr.ip().to_string().starts_with("5.6.7.8"));
    }

    #[test]
    fn punch_seen_records_addr() {
        let t = LeaseTable::new(9000..9010);
        t.alloc([1u8; 32], "A".into(), false).unwrap();
        assert!(t.punch_seen(&[1u8; 32], peer_addr(5000)));
        {
            let leases = t.leases.read().unwrap();
            assert_eq!(leases.get(&[1u8; 32]).unwrap().punch_from, Some(peer_addr(5000)));
        }
    }

    #[test]
    fn data_packet_size_limit_enforced() {
        let small: Vec<u8> = (0..1500).map(|_| 0).collect();
        let large: Vec<u8> = (0..1501).map(|_| 0).collect();
        assert!(data_packet_ok(&small));
        assert!(!data_packet_ok(&large));
    }

    #[test]
    fn snapshot_roster_excludes_hidden() {
        let table = LeaseTable::new(19000..19100);
        table.alloc([1u8; 32], "可见".into(), false).unwrap();
        table.alloc([2u8; 32], "隐身".into(), true).unwrap();
        let roster = table.snapshot_roster(&[9u8; 32], "1.2.3.4".parse().unwrap());
        assert!(roster.iter().any(|d| d.fingerprint == [1u8; 32]), "可见设备应在名册");
        assert!(!roster.iter().any(|d| d.fingerprint == [2u8; 32]), "隐身设备不应在名册");
    }
}
