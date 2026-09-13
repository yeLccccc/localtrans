//! M3b 通道质量管理与智能选路(设计 16 篇落地;spec specs/2026-09-07-m3b-smart-routing-probe-score.md)。
//!
//! 职责切分(沿用 M3a T4 定案的"core 只管数据与原语,编排落壳层"):
//! - 本模块:通道记录([`ChannelRecord`]/[`ChannelTable`],内存态不持久化)+ 时机纪律纯函数;
//! - [`probe`]:探测 wire 原语(阶梯带宽 + Ping/Pong RTT,走独立探测 bi 流)+ 每会话响应端;
//! - [`score`]:评分插值表与选路决策纯函数(设计 16 §3.1/§3.2 逐条对齐);
//! - 壳层(src-tauri `probe` 模块):会话建立触发全量、5min 周期快检、活动传输推迟的调度循环。
//!
//! 红线遵守:质量数据不进广播包(广播保持 6 字段);本模块不触碰 discovery。

pub mod probe;
pub mod score;

pub use score::{bw_score, decide, rtt_score, score, stability_score};

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::time::Instant;

pub type Fingerprint = [u8; 32];

/// 周期快检周期(设计 16 §2.2:5 分钟,只测当前通道 64KB 快检)
pub const QUICK_PERIOD_SECS: u64 = 300;

/// 稳定性窗口容量:近 10 次探测丢包率(设计 16 §2.1)
pub const LOSS_WINDOW_CAP: usize = 10;

/// 地址探测连续超时摘除阈值(设计 16 §3.2 规则 4:超时 3 次摘除,下次会话补测)
pub const REMOVE_AFTER_TIMEOUTS: u32 = 3;

/// 退化切换触发阈值(设计 16 §3.3):当前通道 RTT 连续 3 次复测翻倍。
/// 状态在 [`ChannelRecord::rtt_double_streak`],由 [`ChannelTable::record_rtt`]
/// 累计;调度器经 [`ChannelTable::take_rtt_degradation`] 取用(消费性)。
pub const DEGRADE_AFTER_RTT_DOUBLES: u32 = 3;

/// 探测规模:全量(阶梯三级 + RTT)或快检(1 次 Ping + 64KB)。
/// 升级判定([`escalate_full`])只比较快检值与上次全量值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeKind {
    Full,
    Quick,
}

/// 一条通道的探测记录(spec FR1:`{地址, RTT, 估速, 稳定性(近10次丢包), 时间戳}`,
/// 另带选路必需的经中继标记与探测失败状态)。
///
/// 生命周期:内存态,随会话级存在——表不持久化,进程重启即空;地址可能
/// 回来,由下次会话建立时的全量探测补测(设计 16 §3.2 规则 4)。
#[derive(Clone, Debug)]
pub struct ChannelRecord {
    pub addr: SocketAddr,
    /// 经中继通道标记。登记方(壳层 connect/SessionUp)在建连时即知路径
    /// (connect_pinned=直连 / relay connect_peer+conn.remote_address()=中继数据面
    /// 租约地址,用 [`crate::relay::client::RelayClient::is_relay_data_addr`] 判定),
    /// 比按网段猜可靠——中继租约地址是 relay_ip:peer_data_port,天然带中继痕迹。
    pub via_relay: bool,
    /// 最近一次 RTT(Ping×3 中位,毫秒)
    pub rtt_ms: Option<u64>,
    /// 最近一次带宽估算(bps;全量取后两级中位段速率)
    pub est_bps: Option<u64>,
    /// 上次全量带宽基准——快检掉 50% 升级全量的比较锚([`escalate_full`])
    pub last_full_bps: Option<u64>,
    /// 近 `LOSS_WINDOW_CAP` 次探测样本(true=成功)。丢包率 = false 占比,
    /// 空窗按 0%(新通道乐观,评分见 score::stability_score)
    pub loss_window: VecDeque<bool>,
    /// 最近更新时刻(信息性;决策不使用,单测无需注入时钟)
    pub updated_at: Instant,
    /// 连续探测失败次数(成功清零;跨会话累计,达 [`REMOVE_AFTER_TIMEOUTS`] 摘除)
    pub(crate) consecutive_timeouts: u32,
    /// 探测拉黑:第一次探测失败即置位,本记录生命周期内调度器不再探测该地址。
    /// 新会话建立([`ChannelTable::register`],补测语义)时清除,给一次重试机会。
    pub(crate) probe_disabled: bool,
    /// 退化切换触发计数(FR4):连续复测 RTT 翻倍次数,不翻倍即清零;
    /// 达 [`DEGRADE_AFTER_RTT_DOUBLES`] 由 [`ChannelTable::take_rtt_degradation`]
    /// 消费。新会话建立(register)时重置——换代会话 = 劣化判定重新起算。
    pub(crate) rtt_double_streak: u32,
}

impl ChannelRecord {
    pub fn new(addr: SocketAddr, via_relay: bool) -> Self {
        ChannelRecord {
            addr,
            via_relay,
            rtt_ms: None,
            est_bps: None,
            last_full_bps: None,
            loss_window: VecDeque::new(),
            updated_at: Instant::now(),
            consecutive_timeouts: 0,
            probe_disabled: false,
            rtt_double_streak: 0,
        }
    }

    /// 近 10 次探测丢包率(0.0~1.0;空窗=0)
    pub fn loss_rate(&self) -> f64 {
        loss_rate(&self.loss_window)
    }

    /// 评分是否可用(RTT+带宽齐备才能算分,才参与选路)
    pub fn score_ready(&self) -> bool {
        self.rtt_ms.is_some() && self.est_bps.is_some()
    }
}

/// 丢包率(0.0~1.0;空窗=0)。独立纯函数供直测。
pub fn loss_rate(window: &VecDeque<bool>) -> f64 {
    if window.is_empty() {
        return 0.0;
    }
    let fails = window.iter().filter(|&&ok| !ok).count();
    fails as f64 / window.len() as f64
}

/// 时机纪律:有活动/暂停中的传输 → 推迟探测(轮询等待,不丢任务;
/// 推迟后是否重试由调度循环决定,本函数只做瞬时判定)。
pub fn should_defer(has_active_or_paused_transfers: bool) -> bool {
    has_active_or_paused_transfers
}

/// 快检值比上次全量掉 50% → 升级全量复测(设计 16 §2.2)。
/// 整数运算:`2×quick <= full` 判升级——恰掉 50% 即升级,49% 以上不动。
/// 无基准(首次快检)不升级。
pub fn escalate_full(last_full_bps: Option<u64>, quick_bps: u64) -> bool {
    match last_full_bps {
        None => false,
        Some(full) => quick_bps.saturating_mul(2) <= full,
    }
}

/// 三个采样值的中位数(RTT Ping×3 取中位)
pub fn median3(vals: [u64; 3]) -> u64 {
    let mut v = vals;
    v.sort_unstable();
    v[1]
}

/// 一次复测是否构成"RTT 翻倍"(设计 16 §3.3 触发判定的单步,纯函数):
/// - 基线 >0:亚毫秒归零的基线(回环 0ms→0ms)无法谈翻倍,不计入;
/// - 当前 ≥ 2×基线(saturating 乘法,溢出安全;"恰翻倍"计入)。
pub fn rtt_doubled(prev: u64, cur: u64) -> bool {
    prev > 0 && cur >= prev.saturating_mul(2)
}

/// 通道记录表:每设备(指纹)一份通道列表 + "当前通道"(最近一次成功连接地址)。
/// 全内存态不持久化(spec FR1)。std Mutex:临界区均为纯内存操作,不跨 await。
///
/// 挂载位置:壳层 AppState(与 connect 决策同处,读写都最顺);
/// core 内探测原语按参数收表,不反向依赖壳层。
#[derive(Default)]
pub struct ChannelTable {
    inner: std::sync::Mutex<TableInner>,
}

#[derive(Default)]
struct TableInner {
    channels: HashMap<Fingerprint, Vec<ChannelRecord>>,
    current: HashMap<Fingerprint, SocketAddr>,
}

impl ChannelTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一条通道(不存在则建)。语义:
    /// - 新会话建立的补测入口:清除探测拉黑(给一次重试机会),
    ///   历史数据与超时计数保留(跨代累计才够 3 次摘除);
    /// - via_relay 通道每设备同时只认一条:中继租约地址随对端重注册而变,
    ///   登记新的经中继通道时丢弃该设备其余经中继旧记录(旧租约已死)。
    /// 返回 true 表示新登记(此前无此地址记录)。
    pub fn register(&self, fp: &Fingerprint, addr: SocketAddr, via_relay: bool) -> bool {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if via_relay {
            t.channels.entry(*fp).or_default().retain(|r| !(r.via_relay && r.addr != addr));
        }
        let list = t.channels.entry(*fp).or_default();
        match list.iter_mut().find(|r| r.addr == addr) {
            Some(r) => {
                r.probe_disabled = false;
                // 新会话代 = 劣化判定重新起算(与清拉黑对称:register 是补测入口,
                // 新连接上的复测数据才是本代现状)
                r.rtt_double_streak = 0;
                r.updated_at = Instant::now();
                false
            }
            None => {
                list.push(ChannelRecord::new(addr, via_relay));
                true
            }
        }
    }

    /// 登记并标记"当前通道"(最近一次成功连接地址;connect 成功/SessionUp 调用)
    pub fn note_connected(&self, fp: &Fingerprint, addr: SocketAddr, via_relay: bool) {
        self.register(fp, addr, via_relay);
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        t.current.insert(*fp, addr);
    }

    /// 当前通道(最近一次成功连接的地址)
    pub fn current(&self, fp: &Fingerprint) -> Option<SocketAddr> {
        self.inner
            .lock()
            .expect("ChannelTable 锁中毒")
            .current
            .get(fp)
            .copied()
    }

    /// 某设备的通道记录快照(clone;调用方据此做评分/决策)
    pub fn snapshot(&self, fp: &Fingerprint) -> Vec<ChannelRecord> {
        self.inner
            .lock()
            .expect("ChannelTable 锁中毒")
            .channels
            .get(fp)
            .cloned()
            .unwrap_or_default()
    }

    /// 全部已登记指纹(调度器遍历用)
    pub fn all_fps(&self) -> Vec<Fingerprint> {
        self.inner
            .lock()
            .expect("ChannelTable 锁中毒")
            .channels
            .keys()
            .copied()
            .collect()
    }

    /// 某通道是否被探测拉黑(调度器跳过判据)
    pub fn probe_disabled(&self, fp: &Fingerprint, addr: &SocketAddr) -> bool {
        self.inner
            .lock()
            .expect("ChannelTable 锁中毒")
            .channels
            .get(fp)
            .and_then(|l| l.iter().find(|r| &r.addr == addr))
            .map(|r| r.probe_disabled)
            .unwrap_or(false)
    }

    /// 记录一次 RTT 测量。同时累计退化触发计数(FR4):本次相对上次翻倍则
    /// 连击 +1,否则清零——"连续 3 次复测翻倍"的"连续"语义。
    pub fn record_rtt(&self, fp: &Fingerprint, addr: &SocketAddr, rtt_ms: u64) {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if let Some(r) = find_rec(&mut t, fp, addr) {
            r.rtt_double_streak = if r.rtt_ms.map_or(false, |prev| rtt_doubled(prev, rtt_ms)) {
                r.rtt_double_streak.saturating_add(1)
            } else {
                0
            };
            r.rtt_ms = Some(rtt_ms);
            r.updated_at = Instant::now();
        }
    }

    /// 取用退化触发(FR4;设计 16 §3.3):该通道 RTT 连续
    /// [`DEGRADE_AFTER_RTT_DOUBLES`] 次复测翻倍。**取用即消费**——返回 true
    /// 时连击清零,无论随后的切换成败都需重新累计才会再次触发(切换失败
    /// 另有记录降分+摘除退避,触发器不重复放大)。
    pub fn take_rtt_degradation(&self, fp: &Fingerprint, addr: &SocketAddr) -> bool {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if let Some(r) = find_rec(&mut t, fp, addr) {
            if r.rtt_double_streak >= DEGRADE_AFTER_RTT_DOUBLES {
                r.rtt_double_streak = 0;
                return true;
            }
        }
        false
    }

    /// 记录一次带宽测量。全量结果同时刷新升级判定基准 last_full_bps;
    /// 快检结果只刷新 est_bps(基准保留,供 [`escalate_full`] 比较)。
    pub fn record_bandwidth(&self, fp: &Fingerprint, addr: &SocketAddr, kind: ProbeKind, est_bps: u64) {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if let Some(r) = find_rec(&mut t, fp, addr) {
            r.est_bps = Some(est_bps);
            if kind == ProbeKind::Full {
                r.last_full_bps = Some(est_bps);
            }
            r.updated_at = Instant::now();
        }
    }

    /// 记录一次探测样本(一次探测运行 = 一个稳定性样本)。
    /// 成功:入窗、清零超时计数;失败:入窗、计数 +1、第一次失败即拉黑探测,
    /// 累计达 [`REMOVE_AFTER_TIMEOUTS`] 次将整条记录摘除(下次会话补测)。
    ///
    /// 拉黑与摘除并存的语义(wire 权衡定案,详见 probe 模块注释):
    /// 探测失败对老版本对端是"无响应"(探测流无消费者),重试只是反复白耗
    /// 流量与超时等待,故本代内一次失败即停;摘除阈值 3 次靠"每次新会话
    /// 补测一次"跨代累计到达——地址真死了会被摘出决策集,地址回来会在
    /// 补测成功后自然恢复数据。
    pub fn record_probe_sample(&self, fp: &Fingerprint, addr: &SocketAddr, ok: bool) {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if let Some(r) = find_rec(&mut t, fp, addr) {
            push_loss_sample(&mut r.loss_window, ok);
            r.updated_at = Instant::now();
            if ok {
                r.consecutive_timeouts = 0;
            } else {
                r.consecutive_timeouts += 1;
                r.probe_disabled = true;
            }
        }
    }

    /// 执行摘除判定:连续超时达阈值的通道记录移除(设计 16 §3.2 规则 4)。
    /// 独立方法便于单测"摘除计数";record_probe_sample 内部不自动摘,
    /// 由探测编排(壳层/环回测试)在失败路径后调用。
    pub fn reap_timeouts(&self, fp: &Fingerprint) {
        let mut t = self.inner.lock().expect("ChannelTable 锁中毒");
        if let Some(list) = t.channels.get_mut(fp) {
            list.retain(|r| r.consecutive_timeouts < REMOVE_AFTER_TIMEOUTS);
        }
    }
}

fn find_rec<'a>(
    t: &'a mut TableInner,
    fp: &Fingerprint,
    addr: &SocketAddr,
) -> Option<&'a mut ChannelRecord> {
    t.channels
        .get_mut(fp)
        .and_then(|l| l.iter_mut().find(|r| &r.addr == addr))
}

fn push_loss_sample(window: &mut VecDeque<bool>, ok: bool) {
    if window.len() >= LOSS_WINDOW_CAP {
        window.pop_front();
    }
    window.push_back(ok);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn fp(b: u8) -> Fingerprint {
        [b; 32]
    }

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), port)
    }

    fn rec_with_loss(losses: usize, total: usize) -> ChannelRecord {
        let mut r = ChannelRecord::new(addr(1), false);
        for i in 0..total {
            push_loss_sample(&mut r.loss_window, i >= losses);
        }
        r
    }

    #[test]
    fn 丢包率_空窗为0_容量封顶10() {
        assert_eq!(loss_rate(&VecDeque::new()), 0.0);
        let mut w = VecDeque::new();
        for i in 0..(LOSS_WINDOW_CAP * 2) {
            push_loss_sample(&mut w, i % 3 != 0); // ok = 非 3 的倍数
        }
        assert_eq!(w.len(), LOSS_WINDOW_CAP, "窗口容量封顶 10");
        // 后 10 个样本:i=10..20,失败位 i%3==0 → 12,15,18 共 3 个
        assert!((loss_rate(&w) - 0.3).abs() < 1e-9);
        assert!((rec_with_loss(2, 10).loss_rate() - 0.2).abs() < 1e-9);
    }

    #[test]
    fn 时机纪律_活动传输推迟_快检掉半升级() {
        assert!(should_defer(true), "有 active/paused 传输必须推迟");
        assert!(!should_defer(false));

        // 掉 50% 边界:恰为半速即升级(整数口径 2×quick <= full)
        assert!(!escalate_full(Some(100), 51), "只掉 49% 不升级");
        assert!(escalate_full(Some(100), 50), "恰掉 50% 升级");
        assert!(escalate_full(Some(100), 10), "掉 90% 升级");
        assert!(!escalate_full(None, 1), "无全量基准不升级");
        assert!(!escalate_full(Some(10), 20), "快检高于基准不升级");
        // 溢出防御:quick 极大时 saturating 乘法不 panic(半速语义自然退化)
        assert!(escalate_full(Some(u64::MAX), u64::MAX / 2));
    }

    #[test]
    fn 中位数与rtt样例() {
        assert_eq!(median3([30, 10, 20]), 20);
        assert_eq!(median3([7, 7, 7]), 7);
        assert_eq!(median3([100, 1, 5]), 5);
        assert_eq!(median3([9, 9, 1]), 9);
    }

    #[test]
    fn 记录更新_rtt_带宽_全量基准分离() {
        let table = ChannelTable::new();
        let a = addr(47001);
        assert!(table.register(&fp(1), a, false), "首次登记返回 true");
        assert!(!table.register(&fp(1), a, false), "重复登记 false");

        table.record_rtt(&fp(1), &a, 12);
        table.record_bandwidth(&fp(1), &a, ProbeKind::Full, 90_000_000);
        let snap = table.snapshot(&fp(1));
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].rtt_ms, Some(12));
        assert_eq!(snap[0].est_bps, Some(90_000_000));
        assert_eq!(snap[0].last_full_bps, Some(90_000_000));

        // 快检更新 est 不动全量基准
        table.record_bandwidth(&fp(1), &a, ProbeKind::Quick, 40_000_000);
        let snap = table.snapshot(&fp(1));
        assert_eq!(snap[0].est_bps, Some(40_000_000));
        assert_eq!(snap[0].last_full_bps, Some(90_000_000));
        assert!(escalate_full(snap[0].last_full_bps, 40_000_000), "40<45 掉超半应升级");
        assert!(snap[0].score_ready());

        // 未知地址/未知设备的写入是 no-op,不 panic 不建条目
        table.record_rtt(&fp(1), &addr(9), 5);
        table.record_rtt(&fp(2), &a, 5);
        assert_eq!(table.snapshot(&fp(1)).len(), 1);
        assert!(table.snapshot(&fp(2)).is_empty());
    }

    #[test]
    fn 当前通道_最近一次成功连接() {
        let table = ChannelTable::new();
        assert_eq!(table.current(&fp(3)), None);
        table.note_connected(&fp(3), addr(100), false);
        table.note_connected(&fp(3), addr(200), true);
        assert_eq!(table.current(&fp(3)), Some(addr(200)), "覆盖为最近一次");
        assert_eq!(table.snapshot(&fp(3)).len(), 2);
        assert_eq!(table.all_fps(), vec![fp(3)]);
    }

    #[test]
    fn 探测失败拉黑_第一次失败即停_新会话补测清除_三次摘除() {
        let table = ChannelTable::new();
        let a = addr(5001);
        table.register(&fp(4), a, false);

        // 第一次失败:拉黑 + 计数 1(未达摘除)
        table.record_probe_sample(&fp(4), &a, false);
        assert!(table.probe_disabled(&fp(4), &a), "第一次探测失败即拉黑");
        assert_eq!(table.snapshot(&fp(4)).len(), 1, "计数 1 不摘除");

        // 本代内调度器看到 disabled 不再探测——表不再变化(语义锚)
        // 新会话建立 register(补测):清拉黑,计数保留
        table.register(&fp(4), a, false);
        assert!(!table.probe_disabled(&fp(4), &a), "补测入口清拉黑");

        // 补测再失败:计数 2,仍不摘
        table.record_probe_sample(&fp(4), &a, false);
        assert_eq!(table.snapshot(&fp(4)).len(), 1, "计数 2 仍在表");

        // 成功清零
        table.register(&fp(4), a, false);
        table.record_probe_sample(&fp(4), &a, true);
        table.reap_timeouts(&fp(4));
        assert_eq!(table.snapshot(&fp(4)).len(), 1);

        // 连续 3 次失败(跨三个会话代)→ 摘除
        for _ in 0..3 {
            table.register(&fp(4), a, false); // 每代补测一次
            table.record_probe_sample(&fp(4), &a, false);
        }
        table.reap_timeouts(&fp(4));
        assert!(table.snapshot(&fp(4)).is_empty(), "累计 3 次超时摘除");
        // 摘除后 register 重新建档(地址可能回来)
        assert!(table.register(&fp(4), a, false));
    }

    #[test]
    fn 经中继通道_一设备只认一条() {
        let table = ChannelTable::new();
        let r1 = addr(9001);
        let r2 = addr(9002);
        table.register(&fp(5), r1, true);
        table.note_connected(&fp(5), addr(7000), false); // 直连共存
        table.register(&fp(5), r2, true);
        let snap = table.snapshot(&fp(5));
        let relays: Vec<_> = snap.iter().filter(|r| r.via_relay).map(|r| r.addr).collect();
        assert_eq!(relays, vec![r2], "新中继租约登记时旧租约记录让位");
        assert_eq!(snap.len(), 2, "直连记录不受影响");
    }

    // ===== FR4 退化切换触发:RTT 连续 3 次复测翻倍 =====

    #[test]
    fn rtt翻倍单步_边界钉死() {
        assert!(rtt_doubled(10, 20), "恰翻倍计入");
        assert!(rtt_doubled(10, 21), "超过翻倍计入");
        assert!(!rtt_doubled(10, 19), "不足翻倍不计入");
        assert!(!rtt_doubled(1, 1));
        assert!(!rtt_doubled(0, 100), "亚毫秒归零基线无法谈翻倍");
        assert!(!rtt_doubled(0, 0), "回环 0ms→0ms 不算连续翻倍");
        assert!(rtt_doubled(u64::MAX, u64::MAX), "saturating 乘法不 panic");
        assert!(!rtt_doubled(u64::MAX, 0));
    }

    #[test]
    fn rtt连续翻倍_3次触发_中断清零_取用即消费() {
        let table = ChannelTable::new();
        let a = addr(6001);
        table.register(&fp(9), a, false);

        table.record_rtt(&fp(9), &a, 10); // 基线(无前值,连击 0)
        assert!(!table.take_rtt_degradation(&fp(9), &a));
        table.record_rtt(&fp(9), &a, 20); // 1
        table.record_rtt(&fp(9), &a, 19); // 不翻倍 → 中断清零
        assert!(!table.take_rtt_degradation(&fp(9), &a), "中断后必须重新累计");

        table.record_rtt(&fp(9), &a, 38); // 1
        table.record_rtt(&fp(9), &a, 76); // 2
        assert!(!table.take_rtt_degradation(&fp(9), &a), "2 次不够");
        table.record_rtt(&fp(9), &a, 152); // 3 → 触发
        assert!(table.take_rtt_degradation(&fp(9), &a), "连续 3 次翻倍触发");
        assert!(!table.take_rtt_degradation(&fp(9), &a), "取用即消费,不重复触发");

        // 未知地址/设备:一律 false,不 panic
        assert!(!table.take_rtt_degradation(&fp(9), &addr(6003)));
        assert!(!table.take_rtt_degradation(&fp(11), &a));
    }

    #[test]
    fn 新会话register_重置翻倍连击() {
        let table = ChannelTable::new();
        let a = addr(6002);
        table.register(&fp(10), a, false);
        table.record_rtt(&fp(10), &a, 10);
        table.record_rtt(&fp(10), &a, 20);
        table.record_rtt(&fp(10), &a, 40); // 连击 2

        table.register(&fp(10), a, false); // 新会话建立(补测入口)
        table.record_rtt(&fp(10), &a, 80); // 重置后连击 1
        assert!(
            !table.take_rtt_degradation(&fp(10), &a),
            "register 重置后需在本代重新累计 3 次"
        );
    }
}
