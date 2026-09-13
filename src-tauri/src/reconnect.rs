//! M3a FR5 连接记忆制——自动重连编排（壳层）。
//!
//! 数据与退避纯函数在 `localtrans_core::connect_memory`（可测）；
//! 本模块只做编排：启动扫描 + SessionDown 触发，每设备一条独立退避任务
//! （互不阻塞），重连复用 `connect_pinned`（发现表地址，指纹钉扎）。
//!
//! 行为定案（spec FR5 + "软件从不主动打扰"）：
//! - 退避 2/4/8/16s 封顶 30s（±20% 抖动），连续 5 次失败回落停止；
//! - 发现表无地址 = 未发起连接，**不计失败**，等待 discovery 更新再试
//!   （对端整机离线/休眠数小时后上线仍能恢复会话）；
//! - 回落停止后转静默低频监视：设备从发现表消失后重新出现（absent→present
//!   跃迁）即重新武装新一轮周期（等同启动语义）， otherwise 保持停止；
//! - 静默重连：仅"重连成功"与"最终放弃"各一条 toast，过程零打扰；
//! - 防重入：同一设备同时在重连中不重复调度（注册表判重）；已在会话
//!   （手动连上/对端连入）即退出，不与手动 connect 抢（同走 connect_pinned，
//!   重复建连由 core 会话代次 M-B1 幂等覆盖）。
//! - 中继名册独有设备（本地发现不可见）交给回路 1 relay autoheal：本循环
//!   只在等待 discovery 更新，不与之抢路。

use std::time::Duration;

use localtrans_core::connect_memory::{backoff_secs, give_up, jitter_secs, Fingerprint};
use tauri::{Emitter, Manager};

use crate::AppState;

/// 单次连接尝试超时——防死地址（对端掉电/休眠）把退避节奏拖到 QUIC idle 上限
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// 启动扫描：对全部"已记忆 + 仍受信 + 未在会话"的设备逐个调度自动重连。
/// 发现表此刻多半还是空的——循环自带"等待 discovery 更新"分支，无需提前。
pub fn spawn_startup(app: tauri::AppHandle) {
    tokio::spawn(async move {
        let fps: Vec<Fingerprint> = match app.try_state::<AppState>() {
            Some(st) => st.connect_memory.lock().await.remembered(),
            None => return,
        };
        if fps.is_empty() {
            return;
        }
        tracing::info!("连接记忆启动扫描: {} 台已记忆设备", fps.len());
        for fp in fps {
            schedule(&app, fp).await;
        }
    });
}

/// SessionDown 入口：满足条件才调度（记忆 + 信任 + 未在会话 + 防重入）。
/// 移除信任路径已先清记忆再断开（commands::remove_trusted），此处守卫双保险。
pub async fn schedule_on_down(app: &tauri::AppHandle, fp: Fingerprint) {
    schedule(app, fp).await;
}

/// 调度一条每设备退避任务（幂等：注册表已有该设备任务则跳过）。
async fn schedule(app: &tauri::AppHandle, fp: Fingerprint) {
    let fp_hex = hex::encode(fp);
    let Some(st) = app.try_state::<AppState>() else { return };
    if !st.connect_memory.lock().await.contains(&fp) {
        return;
    }
    if !st.trust.lock().await.is_trusted(&fp) {
        return;
    }
    if st.connected_fps.lock().await.contains(&fp_hex) {
        return; // 已在会话（手动连上/对端连入），无需重连
    }
    let mut reg = st.reconnect_tasks.lock().await;
    if reg.contains_key(&fp_hex) {
        return; // 该设备已在重连中，不重复调度
    }
    reg.insert(
        fp_hex.clone(),
        tokio::spawn(reconnect_loop(app.clone(), fp, fp_hex)),
    );
}

/// 每设备退避重连循环。退出路径（注册表自摘）：
/// 已在会话 / 信任被移除 / 记忆被清除 / 应用退出（随进程消亡）。
async fn reconnect_loop(app: tauri::AppHandle, fp: Fingerprint, fp_hex: String) {
    let mut failures: u32 = 0;
    let mut gave_up = false;
    // 上拍设备是否在发现表中：回落停止后借 absent→present 跃迁重新武装
    let mut peer_absent = true;

    loop {
        let Some(st) = app.try_state::<AppState>() else { break };
        // 终止守卫：会话已建立 / 信任或记忆被清除（手动移除信任即停）
        if st.connected_fps.lock().await.contains(&fp_hex) {
            break;
        }
        if !st.trust.lock().await.is_trusted(&fp) {
            break;
        }
        if !st.connect_memory.lock().await.contains(&fp) {
            break;
        }

        // 回落提示（一轮恰一次）：连续 5 败 → 停止退避重试，转静默监视
        if give_up(failures) && !gave_up {
            gave_up = true;
            let name = display_name(&st, &fp_hex).await;
            tracing::info!("自动重连连续 {failures} 次失败，回落停止: {fp_hex}");
            let _ = app.emit_to(
                tauri::EventTarget::Any,
                "toast",
                serde_json::json!({
                    "level": "warning",
                    "text": format!("{} 暂时无法自动连接，已停止重试", name),
                }),
            );
        }

        // 本拍退避等待（抖动 ±20%；回落停止后的监视拍同用封顶节奏）
        let delay = jitter_secs(backoff_secs(failures + 1), rand::random::<f64>());
        drop(st);
        tokio::time::sleep(Duration::from_secs(delay)).await;

        let Some(st) = app.try_state::<AppState>() else { break };
        // 等待期间可能已被手动连上/对端连入
        if st.connected_fps.lock().await.contains(&fp_hex) {
            break;
        }
        // 地址解析只认本地发现表（带签名的广播指纹）；中继路径归回路 1
        let addr = st.devices.lock().await.iter()
            .find(|d| d.fingerprint == fp)
            .map(|d| d.addr);
        let present = addr.is_some();
        let rearm = gave_up && present && peer_absent;
        peer_absent = !present;

        let Some(addr) = addr else {
            // 发现表无地址：等待 discovery 更新再试，不计失败（静默）
            tracing::debug!(target: "reconnect", "发现表暂无 {fp_hex} 地址,等待更新(已败 {failures} 次)");
            continue;
        };
        if gave_up && !rearm {
            // 回落停止中且设备未经历离线→在线跃迁：保持停止，低频监视
            continue;
        }
        if rearm {
            // 设备消失后重新出现 → 新一轮重连周期（等同启动语义），不重复 toast
            tracing::info!("设备重新出现在发现表，恢复自动重连: {fp_hex}");
            gave_up = false;
            failures = 0;
        }

        // 发起一次连接（connect_pinned 与手动 connect 同路，重复建连由
        // core 会话代次幂等覆盖）；单次超时防死地址拖垮节奏
        drop(st);
        let attempt = tokio::time::timeout(CONNECT_TIMEOUT, async {
            let Some(st) = app.try_state::<AppState>() else {
                return Err("应用退出".to_string());
            };
            st.sm.connect_pinned(addr, fp).await.map_err(|e| e.to_string())
        }).await;
        match attempt {
            Ok(Ok(_)) => {
                // 成功：SessionUp 事件驱动徽章刷新；仅此一条成功 toast
                let name = match app.try_state::<AppState>() {
                    Some(st) => display_name(&st, &fp_hex).await,
                    None => peer_hex_chars(&fp_hex),
                };
                tracing::info!("自动重连成功: {fp_hex}");
                let _ = app.emit_to(
                    tauri::EventTarget::Any,
                    "toast",
                    serde_json::json!({
                        "level": "info",
                        "text": format!("已自动重连: {}", name),
                    }),
                );
                break;
            }
            Ok(Err(e)) => {
                failures += 1;
                tracing::debug!(target: "reconnect", "自动重连失败({failures}): {e}");
            }
            Err(_) => {
                failures += 1;
                tracing::debug!(target: "reconnect", "自动重连超时({failures})");
            }
        }
    }

    // 注册表自摘（schedule 的防重入以此为准）
    if let Some(st) = app.try_state::<AppState>() {
        st.reconnect_tasks.lock().await.remove(&fp_hex);
    }
}

/// 展示名：信任别名 > 信任名 > 发现表名 > 指纹缩写（toast 文案用）。
async fn display_name(st: &tauri::State<'_, AppState>, fp_hex: &str) -> String {
    let (alias_name, dev_name) = {
        let trust = st.trust.lock().await;
        let t = trust.all_peers().into_iter()
            .find(|p| hex::encode(p.fingerprint) == fp_hex)
            .map(|p| (p.alias, p.name));
        let dev = st.devices.lock().await.iter()
            .find(|d| hex::encode(d.fingerprint) == fp_hex)
            .map(|d| d.name.clone());
        (t, dev)
    };
    match alias_name {
        Some((alias, _name)) if !alias.is_empty() => alias,
        Some((_, name)) => name,
        None => dev_name.unwrap_or_else(|| peer_hex_chars(fp_hex)),
    }
}

/// 指纹 hex 截断显示（与 main.rs 同款口径）
fn peer_hex_chars(hex: &str) -> String {
    hex.get(0..8).unwrap_or(hex).to_string()
}
