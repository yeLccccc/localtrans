//! net_addrs:本机全网卡地址枚举(M3a FR1)。
//!
//! 智能选路的前提是"知道本机有哪些路":`local_addresses()` 枚举全部
//! 非环回 IPv4 地址并标注接口名,替代 UDP-connect 单选(`primary_local_ip`,
//! 保留为兼容别名/首选项)。多网卡环境(Hyper-V/WSL/VMware/VPN)的枚举噪声
//! 走接口名黑名单过滤——规则抽成纯函数(`is_virtual_iface` /
//! `filter_candidates`)直测,枚举本身只做系统调用薄封装。
//!
//! 红线:本模块只提供本机视角的地址事实,不参与广播包内容(广播不夹带质量数据)。

use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

/// 本机一条可用地址:IPv4 + 所属接口名(Windows 为友好名,如 "以太网"/"WLAN";
/// Linux/Android 为设备名,如 "eth0"/"wlan0")。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalAddr {
    pub ip: Ipv4Addr,
    pub if_name: String,
}

/// 虚拟网卡关键词(大小写不敏感,子串匹配)。命中即视为虚拟/隧道接口剔除。
/// 面向 Windows 友好名:`vEthernet (WSL (Hyper-V firewall))`、`VMware Network
/// Adapter VMnet8`、`VirtualBox Host-Only Network`、`Npcap Loopback Adapter`、
/// `Tailscale`、`Clash`、`short-tun` 等。
const VIRTUAL_IF_KEYWORDS: &[&str] = &[
    "vethernet",
    "wsl",
    "hyper-v",
    "vmware",
    "virtualbox",
    "vbox",
    "tailscale",
    "loopback",
    "clash",
    "short-tun",
    "zerotier",
    "wireguard",
    "docker",
    "openvpn",
];

/// 虚拟网卡前缀(大小写不敏感,前缀匹配)。面向 Linux/Android 短设备名:
/// `veth*`/`virbr0`/`vmnet8`/`vboxnet0`/`docker0`/`br-xxxx`/`tun0`/`tap0`/
/// `utun*`/`wg0`/`zt*`(ZeroTier)。用前缀而非子串,避免误杀 `wlan0` 这类真网卡。
const VIRTUAL_IF_PREFIXES: &[&str] = &[
    "veth", "virbr", "vmnet", "vboxnet", "docker", "br-", "tun", "tap", "utun", "wg", "zt",
];

/// 接口名是否属于虚拟/隧道网卡(纯函数,黑名单规则唯一出口)。
pub fn is_virtual_iface(if_name: &str) -> bool {
    let lower = if_name.trim().to_lowercase();
    if lower.is_empty() {
        return false;
    }
    VIRTUAL_IF_KEYWORDS.iter().any(|kw| lower.contains(kw))
        || VIRTUAL_IF_PREFIXES.iter().any(|p| lower.starts_with(p))
}

/// 地址本身是否可作为局域网候选:剔除环回/未指定/链路本地(169.254/16)/
/// 多播/受限广播。私网与公网单播一律保留(公网单播对直连同样有意义)。
pub fn is_usable_ipv4(ip: &Ipv4Addr) -> bool {
    !(ip.is_loopback()
        || ip.is_unspecified()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast())
}

/// 对原始 (接口名, 地址) 候选做过滤 + 去重(纯函数):
/// 1. 仅 IPv4;2. 地址可用(`is_usable_ipv4`);3. 接口非虚拟(`is_virtual_iface`);
/// 4. 同一 IP 只保留首次出现(同一网卡多别名/系统重复上报)。
/// 保持输入顺序,排序由调用方决定。
pub fn filter_candidates<I, S>(candidates: I) -> Vec<LocalAddr>
where
    I: IntoIterator<Item = (S, IpAddr)>,
    S: Into<String>,
{
    let mut out: Vec<LocalAddr> = Vec::new();
    for (name, ip) in candidates {
        let IpAddr::V4(v4) = ip else { continue };
        if !is_usable_ipv4(&v4) {
            continue;
        }
        let if_name: String = name.into();
        if is_virtual_iface(&if_name) {
            continue;
        }
        if out.iter().any(|a| a.ip == v4) {
            continue;
        }
        out.push(LocalAddr { ip: v4, if_name });
    }
    out
}

/// 首选本机 IPv4(兼容别名):UDP connect 公网地址只做本地选路不发包,
/// 取系统默认路由所在网卡的地址。无默认路由/离线时 None。
pub fn primary_local_ip() -> Option<Ipv4Addr> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect(SocketAddr::from(([8, 8, 8, 8], 9))).ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if is_usable_ipv4(&v4) => Some(v4),
        _ => None,
    }
}

/// 枚举本机全部可用 IPv4 地址(带接口名),已过滤虚拟网卡与不可用地址。
/// 排序:首选地址(`primary_local_ip`)置首,其余按接口名、IP 稳定排序——
/// 调用方取 `[0]` 即得与旧单选口径一致的地址。系统调用失败返回空表
/// (调用方兜底 `primary_local_ip`)。
pub fn local_addresses() -> Vec<LocalAddr> {
    let raw = match if_addrs::get_if_addrs() {
        Ok(ifs) => ifs,
        Err(e) => {
            tracing::warn!("枚举网卡地址失败: {}", e);
            return Vec::new();
        }
    };
    let mut addrs = filter_candidates(raw.into_iter().map(|i| (i.name.clone(), i.ip())));
    sort_preferred_first(&mut addrs, primary_local_ip());
    addrs
}

/// 首选地址置首,其余按 (接口名, IP) 稳定排序(纯函数,供单测)。
pub fn sort_preferred_first(addrs: &mut [LocalAddr], preferred: Option<Ipv4Addr>) {
    addrs.sort_by(|a, b| {
        let pa = Some(a.ip) == preferred;
        let pb = Some(b.ip) == preferred;
        pb.cmp(&pa)
            .then_with(|| a.if_name.cmp(&b.if_name))
            .then_with(|| a.ip.cmp(&b.ip))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }

    #[test]
    fn 虚拟网卡黑名单_windows友好名() {
        // 剔除
        for name in [
            "vEthernet (WSL (Hyper-V firewall))",
            "vEthernet (Default Switch)",
            "vEthernet (nat)",
            "VMware Network Adapter VMnet8",
            "VMware Network Adapter VMnet1",
            "VirtualBox Host-Only Network",
            "Tailscale",
            "Npcap Loopback Adapter",
            "Clash",
            "short-tun",
            "Hyper-V Virtual Ethernet Adapter",
            "ZeroTier One [abcdef]",
            "WireGuard Tunnel",
            "OpenVPN TAP-Windows6",
        ] {
            assert!(is_virtual_iface(name), "应剔除: {name}");
        }
        // 保留(含中文名/带空格与星号的系统默认名)
        for name in ["以太网", "以太网 2", "WLAN", "Wi-Fi", "本地连接* 1", "Ethernet", "无线网络连接"] {
            assert!(!is_virtual_iface(name), "应保留: {name}");
        }
    }

    #[test]
    fn 虚拟网卡黑名单_linux_android短名() {
        for name in [
            "veth1a2b3c", "virbr0", "vmnet8", "vboxnet0", "docker0", "br-9f8e7d6c", "tun0",
            "tap0", "utun3", "wg0", "zt7nnig26d", "tailscale0",
        ] {
            assert!(is_virtual_iface(name), "应剔除: {name}");
        }
        // 前缀匹配不误杀真网卡:wlan0 不含 "wsl"/"wg" 前缀,eth0/rmnet 保留
        for name in ["eth0", "eth1", "wlan0", "wlan1", "enp3s0", "wlp2s0", "rmnet_data0", "ap0", "p2p0"] {
            assert!(!is_virtual_iface(name), "应保留: {name}");
        }
    }

    #[test]
    fn 黑名单_大小写不敏感与空名() {
        assert!(is_virtual_iface("VETHERNET (X)"));
        assert!(is_virtual_iface("TAILSCALE"));
        assert!(!is_virtual_iface(""));
        assert!(!is_virtual_iface("   "));
    }

    #[test]
    fn 地址可用性_剔除环回链路本地等() {
        assert!(!is_usable_ipv4(&"127.0.0.1".parse().unwrap()));
        assert!(!is_usable_ipv4(&"0.0.0.0".parse().unwrap()));
        assert!(!is_usable_ipv4(&"169.254.10.20".parse().unwrap()));
        assert!(!is_usable_ipv4(&"224.0.0.1".parse().unwrap()));
        assert!(!is_usable_ipv4(&"255.255.255.255".parse().unwrap()));
        assert!(is_usable_ipv4(&"192.168.1.10".parse().unwrap()));
        assert!(is_usable_ipv4(&"10.0.0.5".parse().unwrap()));
        assert!(is_usable_ipv4(&"172.16.3.4".parse().unwrap()));
        assert!(is_usable_ipv4(&"203.0.113.9".parse().unwrap()), "公网单播保留");
    }

    #[test]
    fn 过滤候选_综合规则与去重() {
        let cands = vec![
            ("以太网", v4("192.168.1.10")),
            ("vEthernet (WSL)", v4("172.28.0.1")),          // 虚拟 → 剔
            ("WLAN", v4("192.168.1.11")),
            ("Loopback Pseudo-Interface 1", v4("127.0.0.1")), // 环回 → 剔(名/地址双重)
            ("以太网", IpAddr::V6("fe80::1".parse().unwrap())), // IPv6 → 剔
            ("本地连接* 1", v4("169.254.3.3")),               // 链路本地 → 剔
            ("以太网 别名", v4("192.168.1.10")),               // 重复 IP → 剔
            ("VMware Network Adapter VMnet8", v4("192.168.56.1")),
        ];
        let got = filter_candidates(cands);
        assert_eq!(
            got,
            vec![
                LocalAddr { ip: "192.168.1.10".parse().unwrap(), if_name: "以太网".into() },
                LocalAddr { ip: "192.168.1.11".parse().unwrap(), if_name: "WLAN".into() },
            ]
        );
    }

    #[test]
    fn 排序_首选置首其余按接口名() {
        let mut addrs = vec![
            LocalAddr { ip: "10.0.0.2".parse().unwrap(), if_name: "WLAN".into() },
            LocalAddr { ip: "192.168.1.10".parse().unwrap(), if_name: "以太网".into() },
            LocalAddr { ip: "10.0.0.1".parse().unwrap(), if_name: "WLAN".into() },
        ];
        sort_preferred_first(&mut addrs, Some("192.168.1.10".parse().unwrap()));
        assert_eq!(addrs[0].if_name, "以太网");
        assert_eq!(addrs[1].ip, "10.0.0.1".parse::<Ipv4Addr>().unwrap());
        assert_eq!(addrs[2].ip, "10.0.0.2".parse::<Ipv4Addr>().unwrap());

        // 首选不在表内 → 纯按接口名/IP 排序,不 panic
        sort_preferred_first(&mut addrs, Some("1.2.3.4".parse().unwrap()));
        assert_eq!(addrs[0].if_name, "WLAN");
        sort_preferred_first(&mut addrs, None);
        assert_eq!(addrs[0].if_name, "WLAN");
    }

    #[test]
    fn 枚举_不panic且全部通过过滤规则() {
        // 真机枚举结果因环境而异,只断言不变量:无环回/无虚拟名/无重复
        let addrs = local_addresses();
        let mut seen = std::collections::HashSet::new();
        for a in &addrs {
            assert!(is_usable_ipv4(&a.ip), "{:?}", a);
            assert!(!is_virtual_iface(&a.if_name), "{:?}", a);
            assert!(seen.insert(a.ip), "重复 IP: {:?}", a);
        }
        // 首选地址若存在且在表内,必在首位
        if let Some(p) = primary_local_ip() {
            if addrs.iter().any(|a| a.ip == p) {
                assert_eq!(addrs[0].ip, p);
            }
        }
    }

    #[test]
    fn local_addr_json形态_ip为字串() {
        let a = LocalAddr { ip: "192.168.1.10".parse().unwrap(), if_name: "以太网".into() };
        let v = serde_json::to_value(&a).unwrap();
        assert_eq!(v, serde_json::json!({ "ip": "192.168.1.10", "if_name": "以太网" }));
        let back: LocalAddr = serde_json::from_value(v).unwrap();
        assert_eq!(back, a);
    }
}
