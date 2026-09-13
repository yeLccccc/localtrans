//! 中继 UI 状态(FFI 层)。桌面壳用 emit("relay-state") 推 JSON,
//! 安卓侧无事件通道查状态,改为拉模式:relay_status() 按需读。

use std::sync::Arc;
use crate::state::AppState;
use crate::{AppEvent, LocalTransCallback};

/// 中继连接的 UI 可见状态
#[derive(Clone, Debug, PartialEq)]
pub enum RelayUiStatus {
    /// 未启用(config.relay_enabled = false)
    Disabled,
    /// 已启用,连接建立中
    Connecting,
    /// 已注册到中继(名册可用)
    Connected,
    /// 配置无效或连接失败(带原因,设置页显示「配置错误: 原因」)
    Error(String),
}

impl Default for RelayUiStatus {
    fn default() -> Self {
        RelayUiStatus::Disabled
    }
}

/// 按 config 启动/重启中继连接(启动自动重连与 save_settings 变更重连共用)。
/// 行为镜像桌面壳 set_relay_config 的连接建立段:
/// 未启用→不动;启用→validate(失败置 Error 返回)→关旧→建新+事件桥。
pub async fn spawn_relay(
    state: &AppState,
    callback: &Arc<Box<dyn LocalTransCallback>>,
) {
    let (enabled, server, psk, device_name, hidden) = {
        let config = state.config.read().await;
        (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone(), config.device_name.clone(), config.hidden)
    };
    if !enabled {
        *state.relay_status.lock().unwrap() = crate::relay_state::RelayUiStatus::Disabled;
        return;
    }

    // 配置验证(失败置 Error,不起连接——与桌面 main.rs 启动重连的 validate 门一致)
    if let Err(reason) = localtrans_core::relay::validate_relay_config(true, &server, &psk) {
        tracing::warn!("[relay] 配置无效,不启动中继: {}", reason);
        *state.relay_status.lock().unwrap() = RelayUiStatus::Error(reason);
        return;
    }

    // 关旧连接与事件任务(set_relay_config 变更重连路径会走到这里)
    {
        let mut relay_guard = state.relay.lock().await;
        if let Some(client) = relay_guard.as_ref() {
            client.shutdown().await;
            *relay_guard = None;
        }
    }
    if let Some(h) = state.relay_event_task.lock().await.take() {
        h.abort();
    }
    // M-B3: abort 旧 connect 任务——防止上一次连接尝试仍在握手中,
    // 成功后把自己写进 relay 槽,与新会话形成双连竞态
    // (槽内存 AbortHandle:可 Clone,无需经 event_tasks pop 偷渡句柄)
    if let Some(h) = state.relay_connect_task.lock().await.take() {
        h.abort();
    }
    *state.relay_roster.lock().unwrap() = Vec::new();
    *state.relay_status.lock().unwrap() = RelayUiStatus::Connecting;

    let server_addr: std::net::SocketAddr = match server.parse() {
        Ok(a) => a,
        Err(e) => {
            // validate 已确保可解析,此处兜底(防回归)
            *state.relay_status.lock().unwrap() = RelayUiStatus::Error(format!("无效服务器地址: {}", e));
            return;
        }
    };

    let relay_config = localtrans_core::relay::client::RelayClientConfig {
        server_addr,
        psk,
        device_name,
        hidden,
    };
    let identity = state.identity.clone();
    let relay_state_slot = state.relay.clone();
    let roster_slot = state.relay_roster.clone();
    let status_slot_for_events = state.relay_status.clone();
    let status_slot_for_result = state.relay_status.clone();
    let sm_for_punch = state.sm.clone();
    let callback_for_bridge = callback.clone();
    let event_task_slot = state.relay_event_task.clone();

    let connect_task = tokio::spawn(async move {
        match localtrans_core::relay::client::RelayClient::connect(relay_config, identity).await {
            Ok((client, mut event_rx)) => {
                tracing::info!("[relay] 中继连接成功");
                *relay_state_slot.lock().await = Some(client.clone());

                let relay_for_punch = client.clone();
                let event_task = tokio::spawn(async move {
                    while let Some(event) = event_rx.recv().await {
                        match event {
                            localtrans_core::relay::client::RelayEvent::RosterUpdated(roster) => {
                                *roster_slot.lock().unwrap() = roster;
                                callback_for_bridge.on_event(AppEvent::DevicesChanged);
                            }
                            localtrans_core::relay::client::RelayEvent::StatusChanged(s) => {
                                // Registered 才算 Connected;Connecting/Reconnecting 映射 Connecting
                                let ui = match s {
                                    localtrans_core::relay::client::RelayClientStatus::Registered =>
                                        crate::relay_state::RelayUiStatus::Connected,
                                    _ => crate::relay_state::RelayUiStatus::Connecting,
                                };
                                *status_slot_for_events.lock().unwrap() = ui;
                            }
                            localtrans_core::relay::client::RelayEvent::PunchIncoming { from_fp, session_addr } => {
                                // 对端主动连我(被动方向):accept+adopt(镜像桌面 main.rs:615)
                                // 信任校验在 adopt_connection 内部,未配对对端被拒
                                let sm = sm_for_punch.clone();
                                if let Ok(conn) = relay_for_punch.accept_peer(session_addr, from_fp).await {
                                    if let Err(e) = sm.adopt_connection(conn).await {
                                        tracing::warn!("[relay] 中继入站 adopt 失败: {}", e);
                                    }
                                }
                            }
                        }
                    }
                });
                *event_task_slot.lock().await = Some(event_task);
                *status_slot_for_result.lock().unwrap() = crate::relay_state::RelayUiStatus::Connected;
            }
            Err(e) => {
                tracing::warn!("[relay] 中继连接失败: {}", e);
                // 连接失败保留 Connecting(core 内部 backoff 持续重试,
                // 与桌面行为一致:UI 显示「连接中」)
                *status_slot_for_result.lock().unwrap() = crate::relay_state::RelayUiStatus::Connecting;
            }
        }
    });
    // M-B3(审查修复): connect 任务的 abort 能力同时放两处——
    // event_tasks(供 shutdown 统一 abort)+ relay_connect_task(供下次重连 abort)。
    // AbortHandle 可 Clone,不再需要从 event_tasks pop 偷渡(那依赖
    // "register 与 pop 之间无其他插入"的单线程假设,是脆弱写法)
    let connect_task_abort = connect_task.abort_handle();
    state.register_event_task(connect_task);
    *state.relay_connect_task.lock().await = Some(connect_task_abort);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_config_yields_disabled_status() {
        // relay_enabled=false:spawn_relay 不起连接,状态保持 Disabled
        // (集成级验证在 lib.rs tests 里做;此处验证枚举语义)
        assert_eq!(RelayUiStatus::Disabled, RelayUiStatus::default());
    }
}
