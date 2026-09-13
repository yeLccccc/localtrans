//! 设备列表合并(本地发现 + 中结名册 + 信任表)——桌面壳与 FFI 壳共用。
//!
//! 规则:
//! 1. 本地发现的设备优先(via_relay=false,地址用发现层地址)
//! 2. 仅中继名册中的设备标记 via_relay=true,地址用 lease_addr
//! 3. 按指纹去重,本地优先
//! 4. 信任表条目兜底常驻:已配对设备即使不在发现/名册也显示 offline 卡片
//!    (配对成功即常驻,对端离线/隐身/跨网时设备页不消失)
//! 5. 显示名优先级:本地别名(alias) > 对端广播名 > 信任表配对名
//! 6. 输出排序全序:connected desc → online desc → name asc → fingerprint asc
//!    (指纹 tiebreaker 保证集合不变时相对顺序稳定,卡片不跳动)
//! 7. P3-T5 同广播名重复旧条目归档:对端换身份(重置/重装)后,旧指纹的
//!    offline 卡与新指纹条目同名并存——offline 且陈旧(发现层 last_seen 超
//!    24h / 信任兜底条目 paired_at 超 24h)的旧条目从合并视图隐藏。
//!    仅视图层过滤,信任表数据不动;同广播名无伴生条目的正常离线常驻卡
//!    不受影响,旧对端重新上线即恢复显示。

use std::collections::{HashMap, HashSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// P3-T5:重复旧条目归档阈值——offline 且陈旧超 24h 才隐藏(留足重装/重置
/// 期间的缓冲,避免刚换身份就把旧卡藏掉导致配对状态一时对不上号)
const STALE_DUPLICATE_SECS: u64 = 24 * 60 * 60;

/// 合并后的设备视图(壳层 DTO 由此转换)
#[derive(Clone, Debug, PartialEq)]
pub struct MergedDevice {
    pub fingerprint: String,
    pub name: String,
    pub addr: String,
    pub online: bool,
    pub connected: bool,
    pub via_relay: bool,
}

pub fn merge_devices(
    local: &[crate::discovery::DeviceInfo],
    roster: &[crate::relay::proto::RemoteDevice],
    connected: &HashSet<String>,
    aliases: &HashMap<String, String>,
    trusted: &[(String, String, u64)],
) -> Vec<MergedDevice> {
    let mut result: HashMap<String, MergedDevice> = HashMap::new();

    for d in local {
        let fp_hex = hex::encode(d.fingerprint);
        let display_name = aliases.get(&fp_hex)
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_else(|| d.name.clone());
        result.insert(fp_hex.clone(), MergedDevice {
            fingerprint: fp_hex,
            name: display_name,
            addr: d.addr.to_string(),
            online: d.last_seen.elapsed().as_secs() < 30,
            connected: connected.contains(&hex::encode(d.fingerprint)),
            via_relay: false,
        });
    }

    for r in roster {
        let fp_hex = hex::encode(r.fingerprint);
        if result.contains_key(&fp_hex) {
            continue; // 本地优先
        }
        let display_name = aliases.get(&fp_hex)
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_else(|| r.name.clone());
        result.insert(fp_hex.clone(), MergedDevice {
            fingerprint: fp_hex.clone(),
            name: display_name,
            addr: r.lease_addr.to_string(),
            online: true,
            connected: connected.contains(&fp_hex),
            via_relay: true,
        });
    }

    // 信任表兜底:已配对但既不在发现表也不在名册(对端离线/隐身/跨网)——
    // 生成 offline 常驻卡片。不覆盖已存在的条目(发现/名册信息更丰富)。
    for (fp_hex, paired_name, _paired_at) in trusted {
        if result.contains_key(fp_hex) {
            continue;
        }
        let display_name = aliases.get(fp_hex)
            .filter(|a| !a.is_empty())
            .cloned()
            .unwrap_or_else(|| paired_name.clone());
        result.insert(fp_hex.clone(), MergedDevice {
            fingerprint: fp_hex.clone(),
            name: display_name,
            addr: String::new(),
            online: false,
            connected: connected.contains(fp_hex),
            via_relay: false,
        });
    }

    // P3-T5:同广播名重复旧条目归档(视图层)。offline 且陈旧的条目,若同
    // 广播名另有伴生条目(换身份后的新指纹),从合并视图隐藏。
    let now_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let local_last_seen: HashMap<String, Instant> = local
        .iter()
        .map(|d| (hex::encode(d.fingerprint), d.last_seen))
        .collect();
    let paired_at_by_fp: HashMap<String, u64> = trusted
        .iter()
        .map(|(fp, _, pa)| (fp.clone(), *pa))
        .collect();

    let stale_fps: Vec<String> = result
        .iter()
        .filter(|(fp, d)| {
            if d.online {
                return false; // 在线条目永不归档
            }
            // 同广播名必须存在另一条目——否则是正常的配对常驻离线卡(保留)
            if !result
                .values()
                .any(|o| o.fingerprint != d.fingerprint && o.name == d.name)
            {
                return false;
            }
            if let Some(ls) = local_last_seen.get(fp.as_str()) {
                ls.elapsed().as_secs() >= STALE_DUPLICATE_SECS
            } else if let Some(pa) = paired_at_by_fp.get(fp.as_str()) {
                *pa > 0 && now_unix.saturating_sub(*pa) >= STALE_DUPLICATE_SECS
            } else {
                false
            }
        })
        .map(|(fp, _)| fp.clone())
        .collect();
    for fp in stale_fps {
        result.remove(&fp);
    }

    let mut list: Vec<MergedDevice> = result.into_values().collect();
    list.sort_by(|a, b| {
        b.connected.cmp(&a.connected)
            .then(b.online.cmp(&a.online))
            .then(a.name.cmp(&b.name))
            .then(a.fingerprint.cmp(&b.fingerprint))
    });
    list
}

/// 从信任列表提取 指纹→别名 映射(空别名不进表)
pub fn alias_map(
    trust: &crate::identity::TrustStore,
) -> HashMap<String, String> {
    trust.all_peers().into_iter()
        .filter(|p| !p.alias.is_empty())
        .map(|p| (hex::encode(p.fingerprint), p.alias))
        .collect()
}

/// 从信任列表提取 指纹→(配对名, 配对时间) 列表(merge_devices 第 4 数据源,
/// 已配对设备常驻设备页——离线/隐身/跨网时兜底显示;paired_at 供 P3-T5
/// 同名重复旧条目归档判定)
pub fn trusted_pairs(
    trust: &crate::identity::TrustStore,
) -> Vec<(String, String, u64)> {
    trust.all_peers().into_iter()
        .map(|p| (hex::encode(p.fingerprint), p.name, p.paired_at))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::DeviceInfo;
    use crate::relay::proto::RemoteDevice;
    use std::time::Instant;

    fn dev(fp_byte: u8, name: &str) -> DeviceInfo {
        dev_seen(fp_byte, name, Instant::now())
    }

    /// 指定 last_seen 的发现条目(P3-T5 陈旧归档测试用)
    fn dev_seen(fp_byte: u8, name: &str, last_seen: Instant) -> DeviceInfo {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        DeviceInfo {
            fingerprint: fp,
            name: name.into(),
            addr: format!("192.168.1.{}:{}", fp_byte, crate::ports::quic_port()).parse().unwrap(),
            last_seen,
        }
    }

    /// 当前 unix 秒(P3-T5 paired_at 构造用)
    fn now_unix() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn rem(fp_byte: u8, name: &str) -> RemoteDevice {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        RemoteDevice {
            fingerprint: fp,
            name: name.into(),
            lease_addr: format!("10.0.0.{}:{}", fp_byte, crate::ports::quic_port()).parse().unwrap(),
            relayed_ephemeral: false,
        }
    }

    fn fp_hex(fp_byte: u8) -> String {
        let mut fp = [0u8; 32];
        fp[0] = fp_byte;
        hex::encode(fp)
    }

    #[test]
    fn dedupes_by_fingerprint_local_wins() {
        let local = vec![dev(1, "Device A")];
        let roster = vec![rem(1, "Device A (Relay)"), rem(2, "Device B")];
        let merged = merge_devices(&local, &roster, &HashSet::new(), &HashMap::new(), &[]);
        assert_eq!(merged.len(), 2);
        let a = merged.iter().find(|d| d.fingerprint == fp_hex(1)).unwrap();
        assert_eq!(a.name, "Device A");
        assert!(!a.via_relay);
        let b = merged.iter().find(|d| d.fingerprint == fp_hex(2)).unwrap();
        assert!(b.via_relay);
        assert!(b.online);
    }

    #[test]
    fn alias_overrides_broadcast_name() {
        let mut aliases = HashMap::new();
        aliases.insert(fp_hex(1), "我起的名".to_string());
        aliases.insert(fp_hex(2), String::new());
        let merged = merge_devices(&[dev(1, "广播名")], &[rem(2, "Relay B")], &HashSet::new(), &aliases, &[]);
        assert_eq!(merged.iter().find(|d| d.fingerprint == fp_hex(1)).unwrap().name, "我起的名");
        assert_eq!(merged.iter().find(|d| d.fingerprint == fp_hex(2)).unwrap().name, "Relay B");
    }

    #[test]
    fn sort_connected_first_then_online_then_name() {
        // 全离线、不同名:按名字升序
        let mut c_offline = HashSet::new();
        c_offline.insert(fp_hex(9));
        let merged = merge_devices(
            &[dev(9, "张三"), dev(3, "李四"), dev(5, "王五")],
            &[],
            &c_offline,
            &HashMap::new(),
            &[],
        );
        let names: Vec<&str> = merged.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, vec!["张三", "李四", "王五"], "离线按名字升序");

        // connected 的排最前(即使名字靠后)
        let mut c2 = HashSet::new();
        c2.insert(fp_hex(5));
        let merged = merge_devices(
            &[dev(9, "张三"), dev(3, "李四"), dev(5, "王五")],
            &[],
            &c2,
            &HashMap::new(),
            &[],
        );
        assert_eq!(merged[0].name, "王五", "connected 排最前");
    }

    #[test]
    fn same_name_devices_sorted_by_fingerprint_stably() {
        // 两台同名设备:指纹 tiebreaker 保证顺序确定
        let merged = merge_devices(
            &[dev(7, "同名的设备"), dev(2, "同名的设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[],
        );
        assert_eq!(merged[0].fingerprint, fp_hex(2));
        assert_eq!(merged[1].fingerprint, fp_hex(7));
        // 重复调用结果一致(无 HashMap 随机序)
        let again = merge_devices(
            &[dev(7, "同名的设备"), dev(2, "同名的设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[],
        );
        assert_eq!(merged, again);
    }

    #[test]
    fn trusted_peer_resident_when_off_discovery_and_roster() {
        // 已配对设备不在发现表也不在名册(对端离线/隐身/跨网)——
        // 设备页仍显示 offline 卡片(配对即常驻)
        let merged = merge_devices(
            &[dev(1, "在线设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(2), "配对过的设备".to_string(), now_unix())],
        );
        assert_eq!(merged.len(), 2, "信任表条目兜底显示");
        let t = merged.iter().find(|d| d.fingerprint == fp_hex(2)).unwrap();
        assert_eq!(t.name, "配对过的设备");
        assert!(!t.online, "离线");
        assert!(!t.connected);
        assert!(!t.via_relay);
        assert_eq!(t.addr, "", "无地址信息");
    }

    #[test]
    fn trusted_peer_does_not_override_discovery() {
        // 信任表条目与发现表重叠时不覆盖——发现层信息(地址/在线判定)更丰富
        let merged = merge_devices(
            &[dev(1, "发现层名字")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(1), "信任表名字".to_string(), now_unix())],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].name, "发现层名字");
        assert!(merged[0].online);
    }

    #[test]
    fn trusted_peer_alias_wins_over_paired_name() {
        // 别名优先级最高(信任表兜底条目也遵守)
        let mut aliases = HashMap::new();
        aliases.insert(fp_hex(3), "我起的名".to_string());
        let merged = merge_devices(
            &[],
            &[],
            &HashSet::new(),
            &aliases,
            &[(fp_hex(3), "配对名".to_string(), now_unix())],
        );
        assert_eq!(merged[0].name, "我起的名");
    }

    // ===== P3-T5 同广播名重复旧条目归档 =====

    #[test]
    fn stale_same_name_trusted_duplicate_hidden() {
        // 对端换身份:新指纹在发现表在线,旧指纹只剩信任表兜底卡(离线,
        // 配对已超 24h)→ 旧条目从合并视图隐藏(信任表数据本身不动)
        let merged = merge_devices(
            &[dev(1, "我的手机")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(2), "我的手机".to_string(), now_unix() - 25 * 3600)],
        );
        assert_eq!(merged.len(), 1, "同名陈旧旧条目应隐藏");
        assert_eq!(merged[0].fingerprint, fp_hex(1));
        assert!(merged[0].online);
    }

    #[test]
    fn fresh_trusted_duplicate_kept() {
        // 同广播名但配对未超 24h(刚换身份的缓冲期)→ 两条都显示
        let merged = merge_devices(
            &[dev(1, "我的手机")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(2), "我的手机".to_string(), now_unix() - 3600)],
        );
        assert_eq!(merged.len(), 2, "24h 内的旧条目保留显示");
    }

    #[test]
    fn stale_trusted_without_same_name_kept() {
        // 配对很久但同广播名无伴生条目(正常离线常驻卡)→ 不归档
        let merged = merge_devices(
            &[dev(1, "另一台设备")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(2), "很久没上线".to_string(), now_unix() - 30 * 24 * 3600)],
        );
        assert_eq!(merged.len(), 2, "无同名伴生条目的离线常驻卡不隐藏");
    }

    #[test]
    fn stale_same_name_discovery_duplicate_hidden() {
        // 发现层同名双条目:旧指纹 last_seen 超 24h(offline)、新指纹在线
        // (发现层 15s 过期前理论上不会出现,防御性覆盖快照合并场景)
        // Windows 的 Instant 自开机起算:开机不足 25h 时无法回退 25h(下溢 panic),
        // 此时无法构造"陈旧 Instant"前提,跳过本用例
        let Some(stale_seen) = Instant::now().checked_sub(std::time::Duration::from_secs(25 * 3600)) else {
            return;
        };
        let merged = merge_devices(
            &[dev_seen(2, "我的手机", stale_seen), dev(1, "我的手机")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[],
        );
        assert_eq!(merged.len(), 1, "同名陈旧发现条目应隐藏");
        assert_eq!(merged[0].fingerprint, fp_hex(1));
    }

    #[test]
    fn stale_duplicate_online_again_shows() {
        // 旧对端重新上线(进入发现表,online=true)→ 不再归档,恢复显示
        let merged = merge_devices(
            &[dev(2, "我的手机")],
            &[],
            &HashSet::new(),
            &HashMap::new(),
            &[(fp_hex(2), "我的手机".to_string(), now_unix() - 25 * 3600)],
        );
        assert_eq!(merged.len(), 1, "发现表条目覆盖(本地优先),旧信任条目并入同指纹");
        assert!(merged[0].online, "重新上线后在线显示");
    }
}
