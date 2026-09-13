//! M3c T0 通道表生产者(FFI 壳编排层;等价移植桌面壳 src-tauri/src/probe.rs)。
//!
//! 职责切分沿用 M3a T4 定案:core 只出数据结构/探测原语/纯函数
//! (`localtrans_core::routing`);本模块只做两件编排——
//! 1. SessionUp 触发全量探测(登记通道 + 活动传输推迟,FR2 时机纪律);
//! 2. 5min 周期快检循环(掉 50% 升级全量)。
//!
//! **退化切换不移植**(M3c T0 定案):RTT 翻倍→try_failover 的退化触发
//! 仅桌面壳承担——Android 以单通道场景为主(Wi-Fi/蜂窝各一条,无多地址
//! 候选面),退化切换的价值在"多地址里挑次优",单通道下无可切目标;
//! 快检循环仍持续刷新 RTT/稳定性数据,呈现与评分不受影响。
//!
//! 时机纪律(设计 16 §2.2):有 active/paused 传输 → 轮询等待(5s 步长,
//! 不丢任务);探测不进广播包(红线,本模块不触碰 discovery)。

use std::sync::Arc;
use std::time::Duration;

use localtrans_core::routing;
use tokio::sync::Mutex;

use crate::state::AppState;

/// 传输活动轮询步长(推迟期间 5s 一查,传输结束即开测)
const DEFER_POLL: Duration = Duration::from_secs(5);

/// 手动单对端快检超时(M3c T2 通道面板「重新探测」;快检 64KB + 可能升级的
/// 全量 4MB,局域网秒级;死流上限兜底,与 connect_device 15s 同量级取宽)
const PROBE_NOW_TIMEOUT: Duration = Duration::from_secs(20);

/// 传输表是否有 active/paused 任务(推迟判据;字符串态与 CardState 序列化同源)
async fn transfers_busy(
    transfers: &Arc<Mutex<std::collections::HashMap<u64, crate::transfer_state::TransferCard>>>,
) -> bool {
    transfers
        .lock()
        .await
        .values()
        .any(|c| c.dto.state == "active" || c.dto.state == "paused")
}

/// SessionUp 入口(镜像桌面壳 probe::on_session_up):登记通道(当前通道=
/// 最近一次成功连接地址)并触发全量探测。经中继判定走 relay 数据面租约
/// 地址集(确定性比对,不猜网段);探测任务后台 spawn,事件泵不阻塞。
pub async fn on_session_up(
    st: &AppState,
    fp: routing::Fingerprint,
    conn: localtrans_core::quinn::Connection,
) {
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

/// 5min 周期快检循环(镜像桌面壳 probe::spawn_scheduler;设计 16 §2.2:
/// 只测当前通道 64KB 快检,快检值比上次全量掉 50% → 升级全量)。
/// 句柄由调用方(register_event_task)持有,shutdown 时随事件任务一并 abort。
/// 不承担退化切换触发检查——见模块注释"退化切换不移植"。
pub fn spawn_scheduler(st: Arc<AppState>) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(routing::QUICK_PERIOD_SECS));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticker.tick().await;
            for fp in st.channels.all_fps() {
                // 只探有活会话的设备(探测骑既有会话;离线设备等下次 SessionUp 补测)
                let Some(conn) = st.sm.session(&fp).await else { continue };
                let Some(addr) = st.channels.current(&fp) else { continue };

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
    })
}

/// 手动单对端快检(M3c T2 通道面板「重新探测」入口,镜像桌面壳
/// probe::probe_peer_now):只探指定对端的当前通道 64KB 快检,掉 50%
/// 升级全量。同步等待结果(超时 [`PROBE_NOW_TIMEOUT`] 兜底),UI 按钮
/// "探测中…"态以本函数返回为界。
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
