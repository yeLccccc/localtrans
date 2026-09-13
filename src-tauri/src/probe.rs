//! M3b 通道探测调度与选路接线(壳层编排层)。
//!
//! 职责切分(沿用 M3a T4 定案):core 只出数据结构/探测原语/纯函数
//! (`localtrans_core::routing`);本模块只做四件编排——
//! 1. SessionUp 触发全量探测(登记通道 + 活动传输推迟,FR2 时机纪律);
//! 2. 5min 周期快检循环(掉 50% 升级全量);
//! 3. 退化切换触发检查与编排(FR4,设计 16 §3.3:RTT 连续 3 次复测翻倍
//!    → 次优地址后台建新会话 → 会话表原子换代 → 旧连接静默关闭);
//! 4. connect 命令的目标选择纯函数([`choose_connect_path`],单通道行为不变)。
//!
//! 时机纪律(设计 16 §2.2):有 active/paused 传输 → 轮询等待(5s 步长,
//! 不丢任务);探测不进广播包(红线,本模块不触碰 discovery)。
//!
//! TODO(失速触发,M3c/后续任务):设计 16 §3.3 触发段的另一半——"传输失速
//! 30s"。落点已勘察:sender 侧探针 `run_source_probe`(core transfer/
//! source_probe.rs)每 500ms 发 `SourceSpeed{bps, streams, ..}`(bps==0 且
//! streams>0 持续 60 tick = 失速 30s;bps==0 且 streams==0 是空闲,探针
//! 60s 自杀语义已覆盖,不算失速),壳层事件桥 main.rs `PE::SourceSpeed`
//! 消费处按 job_id→卡片(卡片带 peer 指纹)挂失速计数,满 30s 且该对端
//! 存在更优通道时调用本模块 `failover_plan`+`try_failover`。独立成任务
//! 的原因:侵入传输事件桥(卡片状态机)且需区分 push/pull 两侧信号,
//! 与本任务(通道面)解耦。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use localtrans_core::routing::{self, ChannelRecord};
use tauri::Manager;
use tokio::sync::Mutex;

use crate::AppState;
use crate::transfer_state::TransferCard;

/// 传输活动轮询步长(推迟期间 5s 一查,传输结束即开测)
const DEFER_POLL: Duration = Duration::from_secs(5);

/// 退化切换建连超时(与自动重连同口径:死地址不拖垮切换节奏)
const FAILOVER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 关旧连接前等传输静默的轮询步长
const QUIESCE_POLL: Duration = Duration::from_secs(5);

/// 手动单对端快检超时(M3c T2;快检 64KB + 可能升级的全量 4MB,环回/局域网
/// 秒级完成;死流上限兜底,与自动重连 CONNECT_TIMEOUT 同量级)
const PROBE_NOW_TIMEOUT: Duration = Duration::from_secs(30);

/// 传输表是否有 active/paused 任务(推迟判据;字符串态与 CardState 序列化同源)
async fn transfers_busy(transfers: &Arc<Mutex<std::collections::HashMap<u64, TransferCard>>>) -> bool {
    transfers.lock().await.values().any(|c| c.dto.state == "active" || c.dto.state == "paused")
}

/// SessionUp 入口:登记通道(当前通道=最近一次成功连接地址)并触发全量探测。
/// 经中继判定走 relay 数据面租约地址集(确定性比对,不猜网段);
/// 探测任务后台 spawn,事件泵不阻塞。
pub async fn on_session_up(st: &AppState, fp: [u8; 32], conn: localtrans_core::quinn::Connection) {
    let addr = conn.remote_address();
    let via_relay = match st.relay.lock().await.as_ref() {
        Some(client) => client.is_relay_data_addr(addr).await,
        None => false,
    };
    let table = st.channels.clone();
    let transfers = st.transfers.clone();
    tokio::spawn(async move {
        // 时机纪律:活动/暂停传输期间推迟(轮询等待,不丢任务)
        while transfers_busy(&transfers).await {
            tokio::time::sleep(DEFER_POLL).await;
        }
        // 登记即补测入口(清探测拉黑)+ 记当前通道
        table.note_connected(&fp, addr, via_relay);
        if table.probe_disabled(&fp, &addr) {
            // 理论不可达(register 已清拉黑);防御:本代放弃探测
            return;
        }
        let _ = routing::probe::probe_full(&conn, &table, &fp, &addr).await;
    });
}

/// 5min 周期快检循环(设计 16 §2.2:只测当前通道 64KB 快检;
/// 快检值比上次全量掉 50% → 升级全量)。进程级常驻,随 setup 启动。
/// 同循环承担退化切换触发检查(FR4):RTT 连续 3 次复测翻倍 → 切换。
pub fn spawn_scheduler(app: tauri::AppHandle) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(routing::QUICK_PERIOD_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            let Some(st) = app.try_state::<AppState>() else { break };
            for fp in st.channels.all_fps() {
                // 只探有活会话的设备(探测骑既有会话;离线设备等下次 SessionUp 补测)
                let Some(conn) = st.sm.session(&fp).await else { continue };
                let Some(addr) = st.channels.current(&fp) else { continue };

                // ===== FR4 退化切换触发检查(设计 16 §3.3) =====
                // RTT 连续 3 次复测翻倍 → 尝试切换。放在探测推迟循环**之前**:
                // 切换动作是后台建新会话+换表,不占探测流量、不碰传输流,
                // 传输进行中同样允许(失速场景恰是传输中,等探测窗口就反了)。
                // 触发判定本身连续累计在 ChannelTable(record_rtt 翻倍连击),
                // 此处只做消费性取用。
                if st.channels.take_rtt_degradation(&fp, &addr) {
                    // M3c T3:强制走中继的设备跳过退化切换——开关语义是"钉在
                    // 中继",评分切换(哪怕切向更优直连)会悄悄撤销用户选择;
                    // 与 connect 命令层拦截同口径,core 评分/切换语义不动。
                    let force_relay = st.config.read().await.force_relay_map
                        .get(&hex::encode(fp)).copied().unwrap_or(false);
                    if force_relay {
                        tracing::info!("通道劣化但该设备已强制走中继,跳过退化切换({addr})");
                        continue;
                    }
                    let relay_up = st.relay.lock().await.is_some();
                    let records = st.channels.snapshot(&fp);
                    match failover_plan(&records, addr, relay_up) {
                        Some(plan) => {
                            tracing::info!(
                                "通道连续翻倍劣化({addr}),执行退化切换 → {}(经中继={})",
                                plan.addr, plan.via_relay
                            );
                            // 失败已在 try_failover 内降分+保持旧通道;触发已消费,
                            // 需重新累计 3 次翻倍才会再试(自带 ≥15min 退避)
                            try_failover(&st, fp, plan).await;
                        }
                        None => {
                            tracing::info!("通道连续翻倍劣化({addr})但无更优通道,保持现状");
                        }
                    }
                    continue; // 本轮到此为止:成功则下轮探新通道;失败已降分
                }

                if st.channels.probe_disabled(&fp, &addr) {
                    continue; // 第一次探测失败即拉黑:本记录生命周期内不再探测
                }
                while transfers_busy(&st.transfers).await {
                    tokio::time::sleep(DEFER_POLL).await; // 轮询等待,不丢本轮
                }
                let last_full = st
                    .channels
                    .snapshot(&fp)
                    .into_iter()
                    .find(|r| r.addr == addr)
                    .and_then(|r| r.last_full_bps);
                match routing::probe::probe_quick(&conn, &st.channels, &fp, &addr).await {
                    Some(quick) if routing::escalate_full(last_full, quick) => {
                        tracing::info!("快检掉 50%({last_full:?} → {quick}bps),升级全量复测");
                        let _ = routing::probe::probe_full(&conn, &st.channels, &fp, &addr).await;
                    }
                    _ => {}
                }
            }
        }
    });
}

/// 手动单对端快检(M3c T2 通道面板「重新探测」入口):复用调度器的
/// 快检逻辑——当前通道 64KB 快检,快检值比上次全量掉 50% → 升级全量。
/// 与 spawn_scheduler 的差异:只探指定对端、同步等待结果(前端按钮
/// 转"探测中…"),并用超时兜底(死流不拖住 UI 命令)。
pub async fn probe_peer_now(st: &AppState, fp: routing::Fingerprint) -> Result<(), String> {
    // 只探有活会话的设备(探测骑既有会话;离线设备等 SessionUp 补测)
    let Some(conn) = st.sm.session(&fp).await else {
        return Err("设备未连接,无法探测".into());
    };
    let Some(addr) = st.channels.current(&fp) else {
        return Err("无通道记录,无法探测".into());
    };
    if st.channels.probe_disabled(&fp, &addr) {
        return Err("该通道探测已被拉黑(此前探测失败),等待下次连接恢复".into());
    }
    let last_full = st
        .channels
        .snapshot(&fp)
        .into_iter()
        .find(|r| r.addr == addr)
        .and_then(|r| r.last_full_bps);
    let probe = async {
        match routing::probe::probe_quick(&conn, &st.channels, &fp, &addr).await {
            Some(quick) if routing::escalate_full(last_full, quick) => {
                tracing::info!("手动快检掉 50%({last_full:?} → {quick}bps),升级全量复测");
                let _ = routing::probe::probe_full(&conn, &st.channels, &fp, &addr).await;
            }
            _ => {}
        }
    };
    tokio::time::timeout(PROBE_NOW_TIMEOUT, probe)
        .await
        .map_err(|_| format!("探测超时({:?})", PROBE_NOW_TIMEOUT))?;
    Ok(())
}

/// connect 目标选择结果。`DefaultDirect` = 维持既有"本地发现优先、
/// 中结名册兜底"路径(单候选/无决策时的行为,与历史版本逐字节一致)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectPath {
    DefaultDirect,
    /// 决策选出某直连地址(可与发现表地址不同:多网卡/名片多地址)
    Direct(SocketAddr),
    /// 决策选中经中继通道(按中结名册路径 connect_peer)
    Relay,
}

/// connect 地址选择纯函数(FR3 接线;可单测):
/// - 候选路径 ≤1(单通道)→ 默认路径,不做决策——**现状不回归锚**;
/// - 多候选时查通道表评分决策:决策无结果(None=无评分/迟滞保持/已是
///   最优)→ 默认路径(connect 只发生在未连接时,"保持"退化为直连优先,
///   避免无谓的中继建连);
/// - 决策命中:按该记录的 via_relay 标记映射到直连地址或中继路径。
pub fn choose_connect_path(
    has_discovery_addr: bool,
    has_relay_path: bool,
    records: &[ChannelRecord],
    current: Option<SocketAddr>,
) -> ConnectPath {
    let candidates = has_discovery_addr as usize + has_relay_path as usize;
    if candidates <= 1 {
        return ConnectPath::DefaultDirect;
    }
    match routing::decide(records, current) {
        None => ConnectPath::DefaultDirect,
        Some(addr) => match records.iter().find(|r| r.addr == addr) {
            Some(r) if r.via_relay => ConnectPath::Relay,
            _ => ConnectPath::Direct(addr),
        },
    }
}

// ================= FR4 退化切换(设计 16 §3.3) =================

/// 退化切换计划:触发通过后的切换目标。
/// `addr` = 目标通道地址(直连地址,或经中继记录登记的中继租约地址——
/// 后者仅作降分记账,实际建连走中结名册,新租约地址以建连结果为准)。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailoverPlan {
    pub addr: SocketAddr,
    pub via_relay: bool,
}

/// 退化切换目标选择(纯函数,可单测)。复用 `decide()`——迟滞(≥15 分)、
/// 同分带内直连>中继语义与 connect 决策同源,天然挡住"劣化但次优只是
/// 略好"的无效切换:
/// - 中继客户端不在场时剔除经中继记录(与 connect 命令同口径:陈旧租约
///   记录不参与决策,保留"中继未连接"失败路径);
/// - decide 返回 None(无候选/已在最优/迟滞保持)→ 不切,保持旧通道。
pub fn failover_plan(
    records: &[ChannelRecord],
    current: SocketAddr,
    relay_up: bool,
) -> Option<FailoverPlan> {
    let mut candidates = records.to_vec();
    if !relay_up {
        candidates.retain(|r| !r.via_relay);
    }
    routing::decide(&candidates, Some(current)).map(|addr| {
        let via_relay = candidates
            .iter()
            .find(|r| r.addr == addr)
            .map(|r| r.via_relay)
            .unwrap_or(false);
        FailoverPlan { addr, via_relay }
    })
}

/// 退化切换编排(设计 16 §3.3 动作段):
/// 1. 后台在次优地址建新 QUIC 会话(直连 connect_pinned / 中继
///    connect_peer,限时 [`FAILOVER_CONNECT_TIMEOUT`]);
/// 2. **就绪即原子换代**:connect 成功返回时两端会话表已由 core M-B1
///    语义替换(insert_session_and_spawn 以同指纹覆盖旧条目,generation+1
///    ——复用既有"新一代连接覆盖旧代"路径,不另造切换原语);
/// 3. 换当前通道指针(新发起的传输/连接决策立即用新通道;SessionUp 事件
///    泵随后幂等重登+全量探测,刷新新通道数据);
/// 4. 等传输静默后关旧连接——正在进行的传输尽量不动(引擎在 start_pull
///    等入口持有 `sm.session()` 的 Connection 克隆,提前关=直接杀流);
///    被新一代覆盖的旧代 ctrl_loop 双端**静默退出**(stable_id 归属校验,
///    无 SessionDown 假断线,不触发重连链)。
///
/// 已知边界(诚实交付,M3c/后续任务):**进行中传输的"原子换流"需要引擎层
/// 支持**——接收侧逐块 FetchReq 走 `sm.send_ctrl`(会话表),对端开块流也查
/// 它的会话表;表换代后,持旧 Connection 克隆的接收循环将等不到对端在新
/// 会话上开的块流(块边界失速→看门狗→任务失败,断点续传可恢复)。把
/// Connection 抽象为可替换句柄超出本任务工作量;切换的完整收益当前由
/// "新发起的传输走上新通道"承载。
///
/// 安全边界:新会话建不上(失败/超时)→ 保持旧通道,目标记录降分
/// (失败样本入稳定窗+超时计数,累计 3 次由 reap_timeouts 摘除,
/// 下次会话补测恢复)。返回 true=已切换。
pub async fn try_failover(st: &AppState, fp: routing::Fingerprint, plan: FailoverPlan) -> bool {
    let Some(old_conn) = st.sm.session(&fp).await else {
        tracing::debug!("退化切换放弃:无在位会话");
        return false;
    };

    let fp_hex = hex::encode(fp);
    let attempt = tokio::time::timeout(FAILOVER_CONNECT_TIMEOUT, async {
        if plan.via_relay {
            // connect_via_relay 内部已 note_connected(新租约地址,via_relay)
            crate::commands::connect_via_relay(st, fp).await
        } else {
            st.sm
                .connect_pinned(plan.addr, fp)
                .await
                .map(|_| plan.addr)
                .map_err(|e| e.to_string())
        }
    })
    .await;

    let new_addr = match attempt {
        Ok(Ok(addr)) => addr,
        Ok(Err(e)) => {
            tracing::warn!("退化切换失败({fp_hex} → {plan:?}): {e},保持旧通道,目标记录降分");
            demote_channel(&st.channels, &fp, &plan.addr);
            return false;
        }
        Err(_) => {
            tracing::warn!("退化切换超时({FAILOVER_CONNECT_TIMEOUT:?},{fp_hex} → {plan:?}),保持旧通道,目标记录降分");
            demote_channel(&st.channels, &fp, &plan.addr);
            return false;
        }
    };

    // 换当前通道指针(直连路径在此确定;中继路径 connect_via_relay 已按
    // 新租约登记,此处幂等重登一次,别依赖事件泵时序)
    st.channels.note_connected(&fp, new_addr, plan.via_relay);
    tracing::info!("退化切换完成: {fp_hex} 当前通道 {new_addr}(旧连接待传输静默后关闭)");

    // 关旧连接:等全局传输静默(任意 active/paused 都算——保守口径,
    // 避免关连接杀掉任何在途流;空闲时立即关)
    let transfers = st.transfers.clone();
    tokio::spawn(async move {
        while transfers_busy(&transfers).await {
            tokio::time::sleep(QUIESCE_POLL).await;
        }
        old_conn.close(0u8.into(), b"failover");
        tracing::info!("退化切换:旧通道已关闭({fp_hex})");
    });
    true
}

/// 切换失败的记录降分:记一次探测失败样本(稳定窗丢包+超时计数+本代
/// 探测拉黑),累计 3 次(连续三次切换失败)由 reap_timeouts 摘除——
/// 摘除后 decide 无此候选,不再反复撞死地址;地址恢复由下次会话补测。
fn demote_channel(channels: &routing::ChannelTable, fp: &routing::Fingerprint, addr: &SocketAddr) {
    channels.record_probe_sample(fp, addr, false);
    channels.reap_timeouts(fp);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::net::{IpAddr, Ipv4Addr};

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), port)
    }

    fn rec(port: u16, via_relay: bool, rtt: u64, bps: u64) -> ChannelRecord {
        let mut r = ChannelRecord::new(addr(port), via_relay);
        r.rtt_ms = Some(rtt);
        r.est_bps = Some(bps);
        r.loss_window = VecDeque::new();
        r
    }

    #[test]
    fn 单候选路径_不做决策_维持默认() {
        // 只有直连 / 只有中继 / 都没有:一律默认(既有行为逐字节不变)
        assert_eq!(choose_connect_path(true, false, &[], None), ConnectPath::DefaultDirect);
        assert_eq!(choose_connect_path(false, true, &[], None), ConnectPath::DefaultDirect);
        assert_eq!(choose_connect_path(false, false, &[], None), ConnectPath::DefaultDirect);
        // 有评分记录但候选只有一个路径面,同样不决策
        let records = [rec(1, false, 2, 100_000_000)];
        assert_eq!(
            choose_connect_path(true, false, &records, None),
            ConnectPath::DefaultDirect,
            "单通道行为不变锚"
        );
    }

    #[test]
    fn 双候选_无评分_维持默认直连() {
        assert_eq!(choose_connect_path(true, true, &[], None), ConnectPath::DefaultDirect);
    }

    #[test]
    fn 双候选_决策命中直连地址() {
        // 直连 90 分,中继 100 分但直连在同分带内且直连优先 → 仍直连
        let records = [
            rec(1, true, 2, 100_000_000),   // 中继 100
            rec(2, false, 28, 100_000_000), // 直连 90
        ];
        // 决策(带内直连优先)= addr(2)
        assert_eq!(
            choose_connect_path(true, true, &records, None),
            ConnectPath::Direct(addr(2)),
            "同分带内直连优先映射到 Direct 路径"
        );
        // 中继大幅领先(带外):直连 55 vs 中继 100 → Relay
        let records = [
            rec(1, true, 2, 100_000_000),   // 中继 100
            rec(2, false, 28, 1_000_000),   // 直连 55
        ];
        assert_eq!(choose_connect_path(true, true, &records, None), ConnectPath::Relay);
    }

    #[test]
    fn 双候选_迟滞保持_退化默认路径() {
        // 在位=直连(最近一次连接),中继领先 10 分(<15)→ decide None → 默认
        let records = [
            rec(1, true, 2, 100_000_000),   // 中继 100
            rec(2, false, 28, 100_000_000), // 直连 90(在位)
        ];
        assert_eq!(
            choose_connect_path(true, true, &records, Some(addr(2))),
            ConnectPath::DefaultDirect,
            "迟滞保持期不切中继(连接动作退化为既有直连优先)"
        );
    }

    // ===== FR4 退化切换:计划选择(纯函数) =====

    /// 劣化在位记录(400ms/2Mbps ≈ 23 分)
    fn degraded(port: u16, via_relay: bool) -> ChannelRecord {
        rec(port, via_relay, 400, 2_000_000)
    }

    #[test]
    fn 切换计划_当前劣化_次优健康_选次优() {
        let records = [
            degraded(1, false),                  // 23 分(在位)
            rec(2, false, 2, 100_000_000),       // 100 分(次优)
        ];
        assert_eq!(
            failover_plan(&records, addr(1), false),
            Some(FailoverPlan { addr: addr(2), via_relay: false }),
            "劣化当前+健康次优 → 切向次优直连"
        );
    }

    #[test]
    fn 切换计划_迟滞不足_不切() {
        // 在位 90 分,挑战 80 分:领先 10 < 15,decide 迟滞挡住(与 connect 决策同源)
        let records = [
            rec(1, false, 28, 100_000_000), // 90(在位)
            rec(2, false, 28, 28_000_000),  // 80
        ];
        assert_eq!(failover_plan(&records, addr(1), false), None, "迟滞不足不切");
    }

    #[test]
    fn 切换计划_中继不在场_剔除经中继记录() {
        let records = [
            degraded(1, false),            // 直连 18 分(在位)
            rec(2, true, 2, 100_000_000),  // 中继 100 分
        ];
        assert_eq!(
            failover_plan(&records, addr(1), false),
            None,
            "中继未连接:陈旧租约记录剔除,无候选可切"
        );
        assert_eq!(
            failover_plan(&records, addr(1), true),
            Some(FailoverPlan { addr: addr(2), via_relay: true }),
            "中继在场:中继大幅领先可切"
        );
    }

    #[test]
    fn 切换计划_当前已是最优_不切() {
        let records = [
            rec(1, false, 2, 100_000_000), // 100(在位)
            rec(2, false, 28, 28_000_000), // 80
        ];
        assert_eq!(failover_plan(&records, addr(1), true), None);
    }

    // ===== FR4 退化切换:环回集成(127.0.0.1 双 listener 模拟双地址) =====

    /// 甲乙互信(两处环回测试共用)
    async fn pair_trust(
        st_trust: &std::sync::Arc<tokio::sync::Mutex<localtrans_core::TrustStore>>,
        ctx_b: &localtrans_core::session::SessionCtx,
        fp_a: [u8; 32],
        fp_b: [u8; 32],
    ) {
        use localtrans_core::{Perms, TrustedPeer};
        let mut ta = st_trust.lock().await;
        ta.upsert(TrustedPeer {
            fingerprint: fp_b,
            name: "乙".into(),
            alias: String::new(),
            paired_at: 1,
            perms: Perms::default(),
        });
        let mut tb = ctx_b.trust.lock().await;
        tb.upsert(TrustedPeer {
            fingerprint: fp_a,
            name: "甲".into(),
            alias: String::new(),
            paired_at: 1,
            perms: Perms::default(),
        });
    }

    #[tokio::test]
    async fn 环回_退化切换_新会话落于次优地址_旧连接静默关闭() {
        use localtrans_core::routing::probe::ping_once;
        use localtrans_core::session::bind_endpoint;

        localtrans_core::test_support::init_tracing();
        // 甲 = 真实 AppState(sm/channels/transfers 全真);乙 = core 会话管理器
        let st = crate::test_support::test_app_state().await;
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = localtrans_core::test_support::setup_ctx("乙");
        let fp_a = st.identity.fingerprint();
        pair_trust(&st.trust, &ctx_b, fp_a, fp_b).await;

        // 乙双地址:地址1 托管 listener;地址2 裸端点 + adopt 循环
        let addr1 = localtrans_core::test_support::start_listener(&sm_b).await;
        let ep2 = bind_endpoint(0, &ctx_b.identity).unwrap();
        let mut addr2 = ep2.local_addr().unwrap();
        addr2.set_ip(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)));
        assert_ne!(addr1, addr2, "双地址=本机两个端口");
        let sm_b2 = sm_b.clone();
        tokio::spawn(async move {
            while let Some(incoming) = ep2.accept().await {
                if let Ok(conn) = incoming.await {
                    let _ = sm_b2.adopt_connection(conn).await;
                }
            }
        });

        // 现状通道:甲经地址1 与乙建会话
        let got = tokio::time::timeout(Duration::from_secs(5), st.sm.connect_pinned(addr1, fp_b))
            .await
            .expect("连接超时")
            .expect("连接失败");
        assert_eq!(got, fp_b);

        // 通道表:在位=addr1(手写劣化 RTT/带宽),次优=addr2(健康数据)
        st.channels.note_connected(&fp_b, addr1, false);
        st.channels.register(&fp_b, addr2, false);
        st.channels.record_rtt(&fp_b, &addr1, 400);
        st.channels.record_bandwidth(&fp_b, &addr1, routing::ProbeKind::Full, 2_000_000);
        st.channels.record_rtt(&fp_b, &addr2, 2);
        st.channels.record_bandwidth(&fp_b, &addr2, routing::ProbeKind::Full, 100_000_000);

        // 次优选择(任务卡验证点:手写坏 RTT → 决策选次优)
        let plan = failover_plan(&st.channels.snapshot(&fp_b), addr1, false)
            .expect("劣化在位+健康次优,应有切换计划");
        assert_eq!(plan, FailoverPlan { addr: addr2, via_relay: false });

        // 切换编排:新会话建立 → 会话表原子换代 → 指针切换 → (传输表空)立即关旧
        assert!(try_failover(&st, fp_b, plan).await, "切换应成功");

        // 断言:会话表已在次优地址换代;当前通道指针已换
        let conn = st.sm.session(&fp_b).await.expect("切换后应有会话");
        assert_eq!(conn.remote_address(), addr2, "新会话必须落在次优地址");
        assert_eq!(st.channels.current(&fp_b), Some(addr2), "当前通道指针已切换");

        // 旧连接关闭(传输表空,后台任务立即关)不得误杀新会话/不得触发假断线:
        // 等旧代退出后会话仍在且仍在次优地址;探测 Ping 在新代上端到端可用
        tokio::time::sleep(Duration::from_millis(800)).await;
        let conn2 = st.sm.session(&fp_b).await.expect("旧连接关闭不得误杀新会话");
        assert_eq!(conn2.remote_address(), addr2);
        let rtt = tokio::time::timeout(Duration::from_secs(5), ping_once(&conn2))
            .await
            .expect("Ping 超时")
            .expect("新会话应可承载探测流(响应端在新代在位)");
        assert!(rtt < 1000, "环回 RTT 应远小于 1s,实际 {rtt}ms");

        // 对端侧:表内是新一代且存活(旧代 ctrl_loop 静默退出,未误删未误报)
        let peer_conn = sm_b.session(&fp_a).await.expect("对端会话应存活");
        let peer_rtt = tokio::time::timeout(Duration::from_secs(5), ping_once(&peer_conn))
            .await
            .expect("对端 Ping 超时")
            .expect("对端新会话探测流应可用");
        assert!(peer_rtt < 1000);

        sm_b.shutdown_all().await;
    }

    #[tokio::test]
    async fn 退化切换_目标建不上_保持旧通道并降分() {
        use localtrans_core::routing::probe::ping_once;

        localtrans_core::test_support::init_tracing();
        let st = crate::test_support::test_app_state().await;
        let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = localtrans_core::test_support::setup_ctx("乙");
        let fp_a = st.identity.fingerprint();
        pair_trust(&st.trust, &ctx_b, fp_a, fp_b).await;

        let addr1 = localtrans_core::test_support::start_listener(&sm_b).await;
        tokio::time::timeout(Duration::from_secs(5), st.sm.connect_pinned(addr1, fp_b))
            .await
            .expect("连接超时")
            .expect("连接失败");
        st.channels.note_connected(&fp_b, addr1, false);

        // 目标=死地址(环回拒绝端口):登记记录以便降分可落表
        let dead = std::net::SocketAddr::new(
            std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)),
            1,
        );
        st.channels.register(&fp_b, dead, false);

        let ok = try_failover(&st, fp_b, FailoverPlan { addr: dead, via_relay: false }).await;
        assert!(!ok, "建连失败必须报切换失败");

        // 安全边界:旧通道保持——会话仍在 addr1 且探测流可用(旧连接未被动过)
        let conn = st.sm.session(&fp_b).await.expect("失败必须保持旧会话");
        assert_eq!(conn.remote_address(), addr1, "会话不得被切换动作破坏");
        assert_eq!(st.channels.current(&fp_b), Some(addr1), "当前通道指针不动");
        let rtt = tokio::time::timeout(Duration::from_secs(5), ping_once(&conn))
            .await
            .expect("Ping 超时")
            .expect("旧通道应存活");
        assert!(rtt < 1000);

        // 降分:目标记录拉黑探测+稳定窗有失败样本(3 次失败将摘除,阻断反复撞死地址)
        assert!(st.channels.probe_disabled(&fp_b, &dead), "失败记录应拉黑探测");
        let rec_dead = st
            .channels
            .snapshot(&fp_b)
            .into_iter()
            .find(|r| r.addr == dead)
            .expect("目标记录应存在");
        assert!(rec_dead.loss_rate() > 0.0, "失败样本应入稳定窗");

        sm_b.shutdown_all().await;
    }
}
