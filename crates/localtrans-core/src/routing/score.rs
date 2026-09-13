//! M3b FR3 评分与选路(设计 16 §3.1/§3.2 逐条对齐;spec FR3)。
//!
//! ```text
//! score = RTT 分(50%) + 带宽分(35%) + 稳定性分(15%)
//! RTT:  <5ms=100, 50ms=60, 200ms=20, >500ms=0(分段线性插值)
//! 带宽: >100Mbps=100, 10Mbps=50, <1Mbps=0(对数插值)
//! 稳定: 丢包 0%=100, ≥10%=0(线性)
//! ```
//!
//! 选路规则(设计 16 §3.2):
//! 1. 单通道可用 → 直接用(无决策;壳层在候选 ≤1 时不进决策,现状不回归);
//! 2. 多通道:分数最高者,且**领先 ≥15 分才启用**(迟滞,防 49:51 横跳);
//!    迟滞比较对象是**当前通道**(在位者)——spec 验收 3"49:51 不切"即
//!    当前 51 分对 49 分挑战者不换;
//! 3. 同分误差内(≤15 分):直连 > 中继固定优先;
//! 4. 地址探测超时 3 次摘除:摘除在 ChannelTable::reap_timeouts 完成,
//!    本模块只见干净记录(测试见 mod.rs"三次摘除")。

use super::ChannelRecord;
use std::net::SocketAddr;

/// 迟滞阈值/同分判定带宽(分)。领先不足 15 分不切换。
pub const HYSTERESIS: u32 = 15;

/// RTT 分量:分段线性插值(0-100)。
/// 锚点:≤5ms=100;50ms=60;200ms=20;≥500ms=0(设计 16 §3.1 写死)。
pub fn rtt_score(rtt_ms: u64) -> u32 {
    let rtt = rtt_ms as f64;
    let s = if rtt <= 5.0 {
        100.0
    } else if rtt <= 50.0 {
        lerp(100.0, 60.0, 5.0, 50.0, rtt)
    } else if rtt <= 200.0 {
        lerp(60.0, 20.0, 50.0, 200.0, rtt)
    } else if rtt < 500.0 {
        lerp(20.0, 0.0, 200.0, 500.0, rtt)
    } else {
        0.0
    };
    s.round() as u32
}

/// 带宽分量:对数插值(0-100)。
/// 锚点:≤1Mbps=0;10Mbps=50;≥100Mbps=100(设计 16 §3.1 写死)。
/// 对数插值:bps 在 [lo,hi] 内 → 分数 = 分段下限 + 50×log10(bps/lo)/log10(hi/lo)。
pub fn bw_score(bps: u64) -> u32 {
    const MB10: f64 = 10.0 * 1_000_000.0;
    const MB100: f64 = 100.0 * 1_000_000.0;
    let b = bps as f64;
    let s = if b <= 1_000_000.0 {
        0.0
    } else if b < MB10 {
        50.0 * (b.log10() - 6.0) // log10(1Mbps)=6 为零点
    } else if b < MB100 {
        50.0 + 50.0 * (b.log10() - MB10.log10()) / (MB100.log10() - MB10.log10())
    } else {
        100.0
    };
    s.round().clamp(0.0, 100.0) as u32
}

/// 稳定性分量:丢包 0%=100,≥10%=0,线性(设计 16 §3.1)。
/// 入参为丢包率 0.0~1.0;越界钳制。
pub fn stability_score(loss_rate: f64) -> u32 {
    if !loss_rate.is_finite() || loss_rate <= 0.0 {
        return 100;
    }
    if loss_rate >= 0.1 {
        return 0;
    }
    (100.0 * (1.0 - loss_rate * 10.0)).round() as u32
}

/// 综合分:0.5×RTT + 0.35×带宽 + 0.15×稳定,取整。
/// RTT/带宽缺失(未完成全量探测)→ None:不完整数据不参与选路。
pub fn score(rtt_ms: Option<u64>, est_bps: Option<u64>, loss10: f64) -> Option<u32> {
    let (rtt, bps) = (rtt_ms?, est_bps?);
    let r = rtt_score(rtt) as u64;
    let b = bw_score(bps) as u64;
    let s = stability_score(loss10) as u64;
    Some(((50 * r + 35 * b + 15 * s) / 100) as u32)
}

/// 选路决策(spec FR3;多通道取最高且领先次优/当前 ≥15)。
///
/// - 候选 = 评分齐备(rtt+bps)的记录;无候选 → None(调用方维持现状);
/// - `current = None`(无在位通道)→ 返回最高分者(同分带内直连优先);
/// - `current` 有值:
///   - 不在候选内(未测/被摘)→ 换到最高分者;
///   - 就是首选 → None(保持,不折腾);
///   - 首选领先当前 ≥ [`HYSTERESIS`] → 换;否则 None(迟滞防横跳)。
/// - "首选"在最高分 ±15 分同分带内做直连>中继裁决(同带取分最高的
///   直连,无直连取最高分者;同分再按地址稳定排序)。
pub fn decide(records: &[ChannelRecord], current: Option<SocketAddr>) -> Option<SocketAddr> {
    let mut scored: Vec<(u32, &ChannelRecord)> = records
        .iter()
        .filter_map(|r| score(r.rtt_ms, r.est_bps, r.loss_rate()).map(|s| (s, r)))
        .collect();
    if scored.is_empty() {
        return None;
    }
    // 分数降序;同分直连优先;再按地址保证全序稳定
    scored.sort_by(|(sa, a), (sb, b)| {
        sb.cmp(sa)
            .then_with(|| a.via_relay.cmp(&b.via_relay)) // false(直连)在前
            .then_with(|| a.addr.cmp(&b.addr))
    });
    let best_score = scored[0].0;
    // 同分带内直连固定优先:带内已有直连则在直连中取最优
    let preferred = scored
        .iter()
        .filter(|(s, r)| best_score.saturating_sub(*s) <= HYSTERESIS && !r.via_relay)
        .max_by_key(|(s, r)| (s, std::cmp::Reverse(r.addr)))
        .map(|(s, r)| (*s, *r))
        .unwrap_or(scored[0]);

    let Some(cur) = current else {
        return Some(preferred.1.addr);
    };
    match scored.iter().find(|(_, r)| r.addr == cur) {
        None => Some(preferred.1.addr), // 当前通道无评分数据 → 换
        Some((cur_score, _)) => {
            if preferred.1.addr == cur {
                None // 已是最优,保持
            } else if preferred.0.saturating_sub(*cur_score) >= HYSTERESIS {
                Some(preferred.1.addr)
            } else {
                None // 领先不足 15 分:迟滞不切
            }
        }
    }
}

/// [lo_s, hi_s] 分数区间上 [lo_v, hi_v] 值域的线性插值
fn lerp(s_hi: f64, s_lo: f64, v_lo: f64, v_hi: f64, v: f64) -> f64 {
    s_hi + (s_lo - s_hi) * (v - v_lo) / (v_hi - v_lo)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routing::ChannelRecord;
    use std::collections::VecDeque;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port)
    }

    /// 构造带评分数据的记录(rtt/bps 直填,loss 为失败样本个数/窗口总数)
    fn rec(port: u16, via_relay: bool, rtt: u64, bps: u64, loss_fails: usize) -> ChannelRecord {
        let mut r = ChannelRecord::new(addr(port), via_relay);
        r.rtt_ms = Some(rtt);
        r.est_bps = Some(bps);
        // loss 样本全部失败排前面:率 = loss_fails/LOSS_WINDOW_CAP
        let total = crate::routing::LOSS_WINDOW_CAP;
        for i in 0..total {
            r.loss_window.push_back(i >= loss_fails);
        }
        r
    }

    /// 常用分值锚(手工按插值表核算,防止表漂移悄悄改迟滞语义):
    /// (2ms,100Mbps,0) = (5000+3500+1500)/100 = 100
    /// (28ms,100Mbps,0) = (4000+3500+1500)/100 = 90
    /// (5ms,100Mbps,0) = 100;(5ms,100Mbps,1/10 丢) = (5000+3500+0)/100 = 85
    /// (28ms,31.622777Mbps,0) = (4000+2625+1500)/100 = 81
    /// (28ms,28Mbps,0) = (4000+2520+1500)/100 = 80
    /// (5ms,1Mbps,0) = (5000+0+1500)/100 = 65
    /// (2ms,1Mbps,0) = 65;(28ms,1Mbps,0) = (4000+0+1500)/100 = 55

    #[test]
    fn 分值锚_手工核算钉死() {
        assert_eq!(score(Some(2), Some(100_000_000), 0.0), Some(100));
        assert_eq!(score(Some(28), Some(100_000_000), 0.0), Some(90));
        assert_eq!(score(Some(5), Some(100_000_000), 0.0), Some(100));
        assert_eq!(score(Some(5), Some(100_000_000), 0.1), Some(85));
        assert_eq!(score(Some(28), Some(31_622_777), 0.0), Some(81));
        assert_eq!(score(Some(28), Some(28_000_000), 0.0), Some(80));
        assert_eq!(score(Some(5), Some(1_000_000), 0.0), Some(65));
        assert_eq!(score(Some(2), Some(1_000_000), 0.0), Some(65));
        assert_eq!(score(Some(28), Some(1_000_000), 0.0), Some(55));
    }

    // ===== RTT 插值表逐点(spec §3.1/设计 16 §3.1 写死) =====

    #[test]
    fn rtt插值表_锚点与段内线性() {
        assert_eq!(rtt_score(0), 100);
        assert_eq!(rtt_score(5), 100, "<5ms=100");
        assert_eq!(rtt_score(50), 60, "50ms=60");
        assert_eq!(rtt_score(200), 20, "200ms=20");
        assert_eq!(rtt_score(500), 0, "500ms 恰 0");
        assert_eq!(rtt_score(501), 0, ">500ms=0");
        assert_eq!(rtt_score(u64::MAX), 0);
        // 段内线性:5~50 区间 28ms → 100-40×23/45 = 79.6 → 80
        assert_eq!(rtt_score(28), 80);
        // 50~200 区间:125ms → 40
        assert_eq!(rtt_score(125), 40);
        // 200~500 区间:350ms → 10
        assert_eq!(rtt_score(350), 10);
    }

    // ===== 带宽对数插值逐点 =====

    #[test]
    fn 带宽插值表_锚点与对数中点() {
        assert_eq!(bw_score(0), 0);
        assert_eq!(bw_score(1_000_000), 0, "<1Mbps=0");
        assert_eq!(bw_score(10_000_000), 50, "10Mbps=50");
        assert_eq!(bw_score(100_000_000), 100, "100Mbps=100");
        assert_eq!(bw_score(200_000_000), 100, ">100Mbps 封顶");
        // 对数中点:2Mbps → 50×log10(2)≈15.05 → 15
        assert_eq!(bw_score(2_000_000), 15);
        // 10~100Mbps 对数中点 ≈ 31.62Mbps → 75
        assert_eq!(bw_score(31_622_777), 75);
    }

    // ===== 稳定性边界 =====

    #[test]
    fn 稳定性插值_0到10百分() {
        assert_eq!(stability_score(0.0), 100);
        assert_eq!(stability_score(0.01), 90);
        assert_eq!(stability_score(0.05), 50, "中点线性");
        assert_eq!(stability_score(0.099), 1);
        assert_eq!(stability_score(0.1), 0, "≥10%=0");
        assert_eq!(stability_score(1.0), 0);
        assert_eq!(stability_score(-0.5), 100, "越界钳下限");
        assert_eq!(stability_score(f64::NAN), 100, "非法输入按无损");
    }

    // ===== 综合分权重与缺项 =====

    #[test]
    fn 综合分_权重加权与缺项() {
        // 全满分 = 100;全零分 = 0
        assert_eq!(score(Some(1), Some(100_000_000), 0.0), Some(100));
        assert_eq!(score(Some(600), Some(500_000), 0.2), Some(0));
        // 50×80 + 35×50 + 15×100 = 7250 → 72
        assert_eq!(score(Some(28), Some(10_000_000), 0.0), Some(72));
        // RTT 或带宽缺一(未完成全量探测)→ 不参与选路
        assert_eq!(score(None, Some(1), 0.0), None);
        assert_eq!(score(Some(1), None, 0.0), None);
    }

    // ===== 迟滞(spec 验收 3:领先次优/当前 ≥15 才切) =====

    #[test]
    fn 迟滞_分差1_49比51不切() {
        // 在位 81 分(addr1),挑战 80 分(addr2):领先 1 分,不切
        let records = [
            rec(1, false, 28, 31_622_777, 0), // 81
            rec(2, false, 28, 28_000_000, 0), // 80
        ];
        assert_eq!(decide(&records, Some(addr(1))), None, "49:51 型横跳必须被迟滞挡住");
        // 反向在位(80 在位,81 挑战):领先 1 分同样不切
        assert_eq!(decide(&records, Some(addr(2))), None);
    }

    #[test]
    fn 迟滞_领先恰15切_领先10不切() {
        // 领先 20(100 vs 80)→ 切
        let records = [
            rec(1, false, 28, 28_000_000, 0), // 80 在位
            rec(2, false, 2, 100_000_000, 0), // 100 挑战
        ];
        assert_eq!(decide(&records, Some(addr(1))), Some(addr(2)), "51:36 型:大幅领先必切");

        // 恰 15 分领先(100 vs 85,挑战者在位)→ 切(阈值含 15)
        let records15 = [
            rec(1, false, 5, 100_000_000, 1), // 85 在位(10% 丢包)
            rec(2, false, 2, 100_000_000, 0), // 100 挑战
        ];
        assert_eq!(decide(&records15, Some(addr(1))), Some(addr(2)), "恰领先 15 分:切");

        // 领先 10 分(100 vs 90)→ 不切
        let records10 = [
            rec(1, false, 28, 100_000_000, 0), // 90 在位
            rec(2, false, 2, 100_000_000, 0),  // 100 挑战
        ];
        assert_eq!(decide(&records10, Some(addr(1))), None, "领先 10 < 15:不切");
        // 在位即最优 → 保持
        assert_eq!(decide(&records15, Some(addr(2))), None);
    }

    // ===== 同分带内直连 > 中继(设计 16 §3.2 规则 3) =====

    #[test]
    fn 同分带内_直连固定优先中继() {
        // 同分(100:100)→ 直连
        let records = [
            rec(1, true, 2, 100_000_000, 0), // 中继 100
            rec(2, false, 5, 100_000_000, 0), // 直连 100
        ];
        assert_eq!(decide(&records, None), Some(addr(2)), "同分直连优先");
        // 直连落后但在同分带内(100 vs 90,差 10 ≤ 15)→ 仍直连
        let records = [
            rec(1, true, 2, 100_000_000, 0),  // 中继 100
            rec(2, false, 28, 100_000_000, 0), // 直连 90
        ];
        assert_eq!(decide(&records, None), Some(addr(2)), "带内直连固定优先");
        // 带外(直连 65 vs 中继 100,差 35 > 15)→ 分高者胜
        let records = [
            rec(1, true, 2, 100_000_000, 0), // 中继 100
            rec(2, false, 2, 1_000_000, 0),  // 直连 65
        ];
        assert_eq!(decide(&records, None), Some(addr(1)), "带外不硬保直连");
    }

    // ===== 决策周边 =====

    #[test]
    fn 无记录或无完整评分_维持现状() {
        assert_eq!(decide(&[], None), None);
        assert_eq!(decide(&[], Some(addr(1))), None);
        // 只有未评分记录(探测失败/未全量)
        let unmeasured = [ChannelRecord::new(addr(1), false)];
        assert_eq!(decide(&unmeasured, Some(addr(1))), None);
        assert_eq!(decide(&unmeasured, None), None);
    }

    #[test]
    fn 当前通道不在候选_换到最优() {
        let mut stale = ChannelRecord::new(addr(1), false);
        stale.loss_window = VecDeque::from([false, false]); // 失败拉黑,无评分
        let good = rec(2, false, 2, 100_000_000, 0);
        let records = [stale, good];
        assert_eq!(decide(&records, Some(addr(1))), Some(addr(2)));
        assert_eq!(decide(&records, None), Some(addr(2)));
    }

    #[test]
    fn 单通道_有分即用_已用则保持() {
        let only = [rec(7, false, 10, 50_000_000, 0)];
        assert_eq!(decide(&only, None), Some(addr(7)), "无在位通道:单通道直接用");
        assert_eq!(decide(&only, Some(addr(7))), None, "已在用:保持(单通道现状不回归锚)");
    }
}
