// Copyright (c) 2024 LocalTrans contributors.
// Tauri commands

use std::path::{Path, PathBuf};
use tauri::{State, AppHandle, Emitter, Manager};

use crate::{AppState, ConfigDto, DeviceDto, PairingDto, TransferDto, TrustedPeerDto, ShareDefDto};

use localtrans_core::*;

/// 设备命令

/// 列出当前发现的设备(M3c T3 起与 device-list 事件同口径:全量合并视图
/// +force_relay 注记——此前只回发现表原始条目,首屏卡片与事件路径不一致)
#[tauri::command]
pub async fn list_devices(state: State<'_, AppState>) -> Result<Vec<DeviceDto>, String> {
    Ok(merged_device_dtos(&state).await)
}

/// 设置是否隐藏(隐身翻转时若中继已启用,重注册使名册可见性即时生效)
#[tauri::command]
pub async fn set_hidden(
    app: AppHandle,
    state: State<'_, AppState>,
    hidden: bool,
) -> Result<(), String> {
    let was_hidden = state.hidden.load(std::sync::atomic::Ordering::Relaxed);
    state.hidden.store(hidden, std::sync::atomic::Ordering::Relaxed);

    let (relay_enabled, server, psk) = {
        let mut config = state.config.write().await;
        config.hidden = hidden;
        let dir = &state.dir;
        localtrans_core::store::save_config(dir, &config).map_err(|e| e.to_string())?;
        (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone())
    };

    if relay_enabled && was_hidden != hidden {
        reconnect_relay(&app, &state, server, psk).await?;
    }
    Ok(())
}

/// 立即探测网络
#[tauri::command]
pub async fn probe_now(state: State<'_, AppState>) -> Result<(), String> {
    state.discovery.cmd.send(discovery::DiscoveryCmd::ProbeNow)
        .await
        .map_err(|e| format!("探测失败: {}", e))?;
    Ok(())
}

/// 单播探测 + 5 秒后回查回执(add_manual_device 与 add_by_card 共用)。
/// 探测是单次单播包、静默失败最难排查——派生一个 5 秒后的回查任务，
/// 通过 manual-probe-result 事件把"对方是否真的出现了"明确告诉前端。
async fn probe_addr_with_receipt(
    app: AppHandle,
    state: &AppState,
    target: std::net::SocketAddr,
) -> Result<(), String> {
    state.discovery.cmd.send(discovery::DiscoveryCmd::ProbeAddr(target))
        .await
        .map_err(|e| format!("探测失败: {}", e))?;

    let devices = state.devices.clone();
    let target_ip = target.ip();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        // 注意比较 IP 而不是完整地址：设备表存的是 QUIC 端口(47601)，探测目标是发现端口(47600)
        let found = devices.lock().await.iter()
            .any(|d| d.addr.ip() == target_ip);
        let _ = app.emit("manual-probe-result", serde_json::json!({
            "target": target_ip.to_string(),
            "found": found,
        }));
    });
    Ok(())
}

/// 添加手动设备（复用 probe_addr_with_receipt 回执机制）
#[tauri::command]
pub async fn add_manual_device(
    app: AppHandle,
    state: State<'_, AppState>,
    addr: String,
) -> Result<(), String> {
    let socket_addr: std::net::SocketAddr = addr.parse()
        .map_err(|e| format!("无效地址: {}", e))?;
    probe_addr_with_receipt(app, state.inner(), socket_addr).await
}

/// M3a T3 名片粘贴添加的回执：accepted=名片是否被接受进入探测
/// （本机自身名片拒绝，accepted=false）；addresses_tried=派发单播探测的地址数。
/// 各地址"对方是否真的出现"经既有 manual-probe-result 事件异步 toast。
#[derive(serde::Serialize)]
pub struct CardProbeResult {
    pub accepted: bool,
    pub addresses_tried: usize,
}

/// M3a T3 FR3：生成本机名片文本（人可读多行，复制/粘贴即加）。
/// 数据源组装：设备名+指纹(config/identity) + 全网卡 ip:quic_port(net_addrs，
/// 虚拟网卡已过滤) + 公网出口(relay RegisterAck.observed_addr，启用中继时)
/// + 中继地址(config relay_server，启用中继时)。名片只含公开事实，
/// 绝不含 PSK/密钥（格式与容错见 core::business_card）。
#[tauri::command]
pub async fn get_business_card(state: State<'_, AppState>) -> Result<String, String> {
    let card = assemble_business_card(state.inner()).await;
    Ok(card.to_text())
}

/// 名片数据源组装（get_business_card 用；独立函数便于阅读与复用）
async fn assemble_business_card(st: &AppState) -> localtrans_core::business_card::BusinessCard {
    let (device_name, quic_port, relay_enabled, relay_server) = {
        let cfg = st.config.read().await;
        (cfg.device_name.clone(), cfg.quic_port, cfg.relay_enabled, cfg.relay_server.clone())
    };
    let fingerprint = hex::encode(st.identity.fingerprint());
    // 全网卡非环回 IPv4 × 本机 QUIC 端口 = 直连地址口径（与发现表 addr 同语义）；
    // 枚举失败（空表）兜底 primary_local_ip 单选
    let mut ips: Vec<_> = localtrans_core::net_addrs::local_addresses()
        .into_iter().map(|a| a.ip).collect();
    if ips.is_empty() {
        if let Some(p) = localtrans_core::net_addrs::primary_local_ip() {
            ips.push(p);
        }
    }
    let addresses: Vec<String> = ips.iter().map(|ip| format!("{}:{}", ip, quic_port)).collect();
    // 中继字段仅在中继启用时呈现（与 get_network_status 的 public_exit 同口径）
    let (public_exit, relay_addr) = if relay_enabled {
        let exit = match st.relay.lock().await.as_ref() {
            Some(client) => client.observed_addr().await,
            None => None,
        };
        let server = relay_server.trim();
        let server = if server.is_empty() { None } else { Some(server.to_string()) };
        (exit, server)
    } else {
        (None, None)
    };
    localtrans_core::business_card::BusinessCard {
        device_name,
        fingerprint,
        addresses,
        public_exit,
        relay_addr,
    }
}

/// M3a T3 FR4：粘贴名片添加——解析→逐地址单播探测→回执。
/// 名片地址是 ip:quic_port（直连口径）；探测目标取其 IP + 本机发现端口
/// （双端端口由 ports 模块同源推导，与手动添加"裸 IP 补发现端口"同约定）。
/// ProbeAddr 同时把目标写入 probe_targets 周期重探表（持久化，8s 保活），
/// 跨网段对端重启后由重探自动恢复可见。结果经 manual-probe-result 异步
/// toast（复用手动添加机制，每地址一条）。
#[tauri::command]
pub async fn add_by_card(
    app: AppHandle,
    state: State<'_, AppState>,
    text: String,
) -> Result<CardProbeResult, String> {
    let card = localtrans_core::business_card::BusinessCard::parse(&text)
        .map_err(|e| format!("名片解析失败: {}", e))?;

    // 本机自身名片：无意义且会造成自我探测，拒绝但不算错误
    if card.fingerprint == hex::encode(state.identity.fingerprint()) {
        return Ok(CardProbeResult { accepted: false, addresses_tried: 0 });
    }

    let discovery_port = state.config.read().await.discovery_port;
    let mut tried = 0usize;
    for raw in &card.addresses {
        // 解析已在 core 校验过，这里防御性跳过不可解析项
        let Ok(sa) = raw.parse::<std::net::SocketAddr>() else { continue };
        let target = std::net::SocketAddr::new(sa.ip(), discovery_port);
        probe_addr_with_receipt(app.clone(), state.inner(), target).await?;
        tried += 1;
    }
    Ok(CardProbeResult { accepted: true, addresses_tried: tried })
}

/// connect 链路错误格式化(P2 配对健壮性):SessionError::Cooldown(remaining)
/// 结构化为机器可识别前缀 `pairing_cooldown:{secs}`,前端据此弹专属文案与
/// 设备卡冷却倒计时;其余错误保持 Display 原文(既有文案不变)。
/// 命令层 Result<_, String> 的既有签名不动,结构化只体现在字符串约定上。
pub(crate) fn fmt_connect_err(e: localtrans_core::session::SessionError) -> String {
    match e {
        localtrans_core::session::SessionError::Cooldown(secs) => {
            format!("pairing_cooldown:{secs}")
        }
        other => other.to_string(),
    }
}

/// 经中继建连(roster 兜底路径与 M3b 决策选中的 Relay 路径共用)。
/// 成功后登记通道记录(via_relay=true,地址=中继数据面租约地址)并记
/// 当前通道——SessionUp 事件泵的全量探测会随后接手刷新数据。
/// 返回中继数据面租约地址(M3b FR4 退化切换记账用)。
/// 入参 &AppState(非 State 包装):供 commands 与 probe(FR4 切换编排)两处复用。
pub(crate) async fn connect_via_relay(state: &AppState, fp: [u8; 32]) -> Result<std::net::SocketAddr, String> {
    // 中继名册中的设备，通过 relay_client.connect_peer 连接
    let relay_client = state.relay.lock().await;
    let client = relay_client.as_ref()
        .ok_or_else(|| format!("中继未连接"))?;

    // 使用中继客户端的 connect_peer 方法
    let conn = client.connect_peer(fp).await.map_err(|e| e.to_string())?;
    let raddr = conn.remote_address();

    // 将连接 adopt 进 SessionManager(主动方语义:本机发起的连接
    // 必须开 bi 流——与对端的 adopt_connection 被动语义配对,
    // 否则双方互相 accept_bi 死锁)
    state.sm.adopt_as_initiator(conn).await.map_err(fmt_connect_err)?;

    // M3b FR1:登记经中继通道 + 当前通道(最近一次成功连接地址)
    state.channels.note_connected(&fp, raddr, true);
    Ok(raddr)
}

/// connect 建连内核(M3c T3 从命令中提取,State 包装剥离,可测):
/// 幂等(已在会话跳过)→ 强制走中继拦截 → M3b 评分决策路由。
pub(crate) async fn connect_inner(
    state: &AppState,
    fingerprint: &str,
    fp: [u8; 32],
) -> Result<(), String> {
    // M3c T3 修正:force_relay 开关优先于幂等守卫——排障语义是"强制按
    // 中继重建",若先短路返回,开关在已有会话时永远不生效(实测踩坑:
    // FR5 自动重连建好会话后,开关拦截被幂等跳过)。查开关→不在场才走幂等。
    let force_relay_on = state.config.read().await.force_relay_map
        .get(fingerprint).copied().unwrap_or(false);
    if !force_relay_on && state.sm.session(&fp).await.is_some() {
        return Ok(());
    }
    // M3c T3 强制走中继(per 设备持久化开关):命令层拦截决策——跳过
    // 评分直选中继路径(评分/切换 core 语义不动)。中继客户端不在场
    // 直接报错(评分决策时的"陈旧租约记录剔除"口径在此不需要:开关
    // 语义是排障后门,无中继就该明说失败)。
    if force_relay_on {
        if state.relay.lock().await.is_none() {
            return Err("中继未配置".into());
        }
        return connect_via_relay(state, fp).await.map(|_| ());
    }
    // 路由逻辑(M3b FR3 接线):候选路径(本地发现直连 + 中结名册)≥2 时
    // 查通道表评分决策——多通道取最高且领先次优/当前 ≥15 分(迟滞),
    // 同分带内直连>中继;单候选/决策无结果维持既有"发现优先、中继兜底"
    // 路径逐字节不变(单通道回归锚)。
    let local_addr = {
        let devices = state.devices.lock().await;
        devices.iter()
            .find(|d| d.fingerprint == fp)
            .map(|d| d.addr)
    };
    let has_relay_path = state.relay_roster.lock().await.iter()
        .any(|d| d.fingerprint == fp);
    // 评分决策输入:通道记录快照 + 当前通道(最近一次成功连接地址)。
    // 中继未连接时剔除经中继记录——决策不选中继,保留既有"中继未连接"
    // 错误路径(记录可能是上次中继在线时的陈旧数据)。
    let mut records = state.channels.snapshot(&fp);
    if state.relay.lock().await.is_none() {
        records.retain(|r| !r.via_relay);
    }
    let current = state.channels.current(&fp);

    match crate::probe::choose_connect_path(local_addr.is_some(), has_relay_path, &records, current) {
        crate::probe::ConnectPath::Direct(addr) => {
            // 决策选出直连地址(S1: 指纹钉扎不变)
            state.sm.connect_pinned(addr, fp).await.map_err(fmt_connect_err)?;
        }
        crate::probe::ConnectPath::Relay => {
            connect_via_relay(state, fp).await?;
        }
        crate::probe::ConnectPath::DefaultDirect => {
            // 既有路径:优先本地发现,其次中结名册
            if let Some(addr) = local_addr {
                // 本地发现的设备,直接连接(S1: 带发现表指纹钉扎,防中间人替换身份)
                state.sm.connect_pinned(addr, fp).await.map_err(fmt_connect_err)?;
            } else if has_relay_path {
                connect_via_relay(state, fp).await?;
            } else {
                return Err(format!("未找到设备: {}", fingerprint));
            }
        }
    }
    Ok(())
}

/// 连接到指定设备（按指纹）
#[tauri::command]
pub async fn connect(
    state: State<'_, AppState>,
    app: tauri::AppHandle,
    fingerprint: String,
) -> Result<(), String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    // 建连内核独立函数(M3c T3 提取,含强制走中继拦截;可测)
    connect_inner(&state, &fingerprint, fp).await?;

    // 连接命令成功返回 = 会话已建立(已信任直达 SessionUp / 配对中由
    // PairingResult 后续收尾)。此处确定性刷新 connected 徽章——不依赖
    // 事件桥时序(SessionUp 消费者迟到/竞态时卡片不刷新,v0.10.3 曾现)
    let fp_hex = hex::encode(fp);
    {
        let st = app.state::<AppState>();
        st.connected_fps.lock().await.insert(fp_hex.clone());
        // M3a FR5:用户主动连接成功 → 记入连接记忆(持久化;断线/重启后自动重连的依据)
        {
            let mut mem = st.connect_memory.lock().await;
            mem.record(&fp);
            if let Err(e) = mem.save() {
                tracing::warn!("连接记忆落盘失败: {}", e);
            }
        }
        let dtos = merged_device_dtos(&st).await;
        let _ = app.emit("device-list", dtos);
    }
    let _ = app.emit("connection-state", serde_json::json!({
        "fingerprint": fp_hex,
        "up": true
    }));

    Ok(())
}

/// 配对命令

/// 获取待配对列表
#[tauri::command]
pub async fn get_pairing_pending(state: State<'_, AppState>) -> Result<Vec<PairingDto>, String> {
    let pending = state.pending_pairing.lock().await.clone();
    let devices = state.devices.lock().await.clone();
    let roster = state.relay_roster.lock().await.clone();

    let result: Vec<PairingDto> = pending.into_iter()
        .map(|(fp, own_code)| {
            // 名字反查：优先设备表，其次中结名册，兜底用指纹缩写
            let name = devices.iter()
                .find(|d| hex::encode(d.fingerprint) == fp)
                .map(|d| d.name.clone())
                .or_else(|| roster.iter()
                    .find(|r| hex::encode(r.fingerprint) == fp)
                    .map(|r| r.name.clone()))
                .unwrap_or_else(|| {
                    // 兜底：指纹缩写（取前 6 位 hex）
                    format!("未知设备_{}", &fp.chars().take(6).collect::<String>())
                });

            PairingDto {
                fingerprint: fp,
                name,
                own_code,
            }
        })
        .collect();
    Ok(result)
}

/// 提交配对码
#[tauri::command]
pub async fn submit_pair_code(
    state: State<'_, AppState>,
    fingerprint: String,
    peer_code: String,
) -> Result<bool, String> {
    let fp = decode_fp(&fingerprint)?;
    state.sm.submit_pair_code(&fp, &peer_code).await.map_err(|e| e.to_string())
}

/// 拒绝配对（兼容保留，新 UI 走 deny_consent）
#[tauri::command]
pub async fn reject_pairing(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;

    state.sm.disconnect(&fp).await;
    state.pending_pairing.lock().await.remove(&fingerprint);

    Ok(())
}

/// B(接受方)同意连接:生成随机码,返回给本机 UI 展示
#[tauri::command]
pub async fn grant_consent(
    state: State<'_, AppState>,
    fingerprint: String,
) -> Result<crate::GrantConsentDto, String> {
    let fp = decode_fp(&fingerprint)?;
    let own_code = state.sm.grant_consent(&fp).await.map_err(|e| e.to_string())?;
    // 记入待配对表（B 本机恢复显示用；A 侧永不查询到此码）
    state.pending_pairing.lock().await.insert(fingerprint.clone(), own_code.clone());
    Ok(crate::GrantConsentDto { own_code })
}

/// B(接受方)拒绝连接
#[tauri::command]
pub async fn deny_consent(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;
    state.sm.deny_consent(&fp).await.map_err(|e| e.to_string())?;
    state.pending_pairing.lock().await.remove(&fingerprint);
    Ok(())
}

/// B(接受方)已同意后主动结束等待（码即焚）
#[tauri::command]
pub async fn cancel_pairing_wait(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;
    state.sm.cancel_wait(&fp).await.map_err(|e| e.to_string())?;
    state.pending_pairing.lock().await.remove(&fingerprint);
    Ok(())
}

/// 辅助函数：解码十六进制指纹字符串为 [u8; 32]
fn decode_fp(fingerprint: &str) -> Result<[u8; 32], String> {
    let fp_bytes = hex::decode(fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);
    Ok(fp)
}

/// 浏览/传输命令

/// 远程列出共享区
#[tauri::command]
pub async fn list_shares_remote(
    state: State<'_, AppState>,
    fingerprint: String,
) -> Result<Vec<protocol::ShareInfo>, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    // msg_id 多路化:每请求独立 oneshot,浏览与传输互斥消除
    // v0.9.1: 首次超时后自动重试一次再报错——每次用全新 oneshot
    async fn once_shares(
        sm: &localtrans_core::session::SessionManager,
        fp: &[u8; 32],
    ) -> Result<Vec<protocol::ShareInfo>, ()> {
        let (msg_id, resp_rx) = sm.send_rpc(fp, protocol::ControlMsg::SharesReq { msg_id: 0 })
            .await.map_err(|_| ())?;
        let out = tokio::time::timeout(tokio::time::Duration::from_secs(6), async {
            match resp_rx.await {
                Ok((_, protocol::ControlMsg::SharesResp { shares, .. })) => Ok(shares),
                _ => Err(()),
            }
        }).await;
        if out.is_err() {
            sm.cancel_rpc(msg_id).await;
        }
        out.map_err(|_| ())?
    }

    match once_shares(&state.sm, &fp).await {
        Ok(shares) => Ok(shares),
        Err(_) => {
            tracing::warn!("SharesResp 未达,自动重试一次");
            match once_shares(&state.sm, &fp).await {
                Ok(shares) => Ok(shares),
                Err(_) => Err(format!("重试发送失败: 等待响应超时或通道关闭")),
            }
        }
    }

}

/// 远程列出目录
#[tauri::command]
pub async fn list_dir_remote(
    state: State<'_, AppState>,
    fingerprint: String,
    share_id: String,
    path: String,
    cursor: u64,
) -> Result<crate::ListRespDto, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    // msg_id 多路化:每请求独立 oneshot,浏览与传输互斥消除
    let (msg_id, resp_rx) = state.sm.send_rpc(&fp, protocol::ControlMsg::ListReq {
        share_id: share_id.clone(),
        path: path.clone(),
        cursor,
        msg_id: 0,
    }).await.map_err(|e| format!("发送请求失败: {}", e))?;

    let result = tokio::time::timeout(tokio::time::Duration::from_secs(6), async {
        match resp_rx.await {
            Ok((_, protocol::ControlMsg::ListResp { entries, next_cursor, .. })) =>
                Ok(crate::ListRespDto { entries, next_cursor }),
            _ => Err(()),
        }
    }).await;

    if result.is_err() {
        state.sm.cancel_rpc(msg_id).await;
        // v0.9.1: 首次超时自动重试一次——每次用全新 oneshot
        tracing::warn!("ListResp 未达,自动重试一次");
        let (_, resp_rx2) = state.sm.send_rpc(&fp, protocol::ControlMsg::ListReq {
            share_id: share_id.clone(),
            path: path.clone(),
            cursor,
            msg_id: 0,
        }).await.map_err(|e| format!("重试发送失败: {}", e))?;
        let retry = tokio::time::timeout(tokio::time::Duration::from_secs(6), async {
            match resp_rx2.await {
                Ok((_, protocol::ControlMsg::ListResp { entries, next_cursor, .. })) =>
                    Ok(crate::ListRespDto { entries, next_cursor }),
                _ => Err(()),
            }
        }).await;
        return match retry {
            Ok(Ok(resp)) => Ok(resp),
            _ => Err(String::from("等待响应超时或通道关闭")),
        };
    }

    match result {
        Ok(Ok(resp)) => Ok(resp),
        _ => Err(String::from("等待响应超时或通道关闭")),
    }
}

/// Task 7:排队获取槽位并周期上报位次——等待期间每 1s 把 dto.queue_pos
/// 刷成同对端 pending 卡中的名次(泵式近似);拿到槽位后置 None。
async fn acquire_with_queue_pos(
    st: &AppState,
    card_id: u64,
    peer_hex: &str,
) -> (tokio::sync::OwnedSemaphorePermit, tokio::sync::OwnedMutexGuard<()>) {
    let fut = st.acquire_slot(peer_hex);
    tokio::pin!(fut);
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            slot = &mut fut => {
                st.card_mutate(card_id, |d| d.queue_pos = None).await;
                return slot;
            }
            _ = tick.tick() => {
                let pos = st.pending_rank(card_id, peer_hex).await;
                st.card_mutate(card_id, |d| d.queue_pos = pos).await;
            }
        }
    }
}

/// 开始下载
#[tauri::command]
pub async fn start_download(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    fingerprint: String,
    share_id: String,
    path: String,
) -> Result<u64, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    tracing::info!("开始下载: 对端={} 共享区={}", fingerprint, share_id);

    // 立即插入"等待中"占位卡片：xfer_lock 排队/元数据交换期间传输页即可见
    // （Task 4:占位即真实卡片,ID 恒定,不再删占位换实件）
    let placeholder_id = state.card_create(TransferDto {
        job_id: 0,
        name: path.clone(),
        total: 0,
        done: 0,
        state: "pending".into(),
        speed_bps: 0,
        peer: fingerprint.clone(),
        direction: "pull".into(),
        local_role: "destination".into(),
        health: None,
        started_at_ms: None, finished_at_ms: None, source_path: None, fail_reason: None,
        remote_done: 0, instant: false,
        queue_pos: None, batch_id: None, children: vec![], parts_id: None,
    }).await;

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

    let config = state.config.read().await.clone();
    let sm = state.sm.clone();
    let reg = state.reg.clone();
    let fp_for_task = fp;
    let sid = share_id.clone();
    let rel = path.clone();
    let cfg_for_task = config.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();

    tokio::spawn(async move {
        // Task 7:全局并发闸门+对端串行锁（替代全局 xfer_lock，跨对端放开）
        // 排队超时：闸门满/前序同对端任务卡死，60s 后明确失败
        let peer_hex_slot = hex::encode(fp_for_task);
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &peer_hex_slot),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("下载排队超时：并发闸门满且同对端前序任务持有超过 60s");
                // 修复轮 1:超时取消后清排队位次残留
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                    reason: Some("排队超时".into()),
                }).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error",
                    "text": "排队超时：前序传输任务似乎卡住了，请检查或取消它"
                }));
                return;
            }
        };

        // 处理进度事件（卡片生命周期）
        let mut placeholder_alive = true;
        let mut progress_stopped = false;

        // v0.2.2 根因修复：进度通道必须与传输引擎并发消费。
        // 此前"先 await start_pull 完、再 while recv"的写法让容量 64 的
        // 通道在传输中途塞满，引擎阻塞在 progress.send() 上——速度探针停摆、
        // start_pull 永不返回、xfer_lock 永不释放，表现为大文件停在整块
        // 边界且后续任务永远 pending（v0.2.1 实机 242MB zip 卡 26.4%）。
        let peer_hex = hex::encode(fp_for_task);
        let pull_fut = localtrans_core::transfer::start_pull(
            &sm,
            &reg,
            &fp_for_task,
            &sid,
            &rel,
            &cfg_for_task,
            progress_tx,
        );
        tokio::pin!(pull_fut);

        loop {
            tokio::select! {
                biased;
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_pull_progress_event(
                            &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex, ev,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut pull_fut => {
                    if let Err(e) = res {
                        tracing::warn!("下载失败: {}", e);
                        // 卡片落到失败态（ID 恒定,无占位替换;若引擎已发过
                        // Started,卡片已在 active,Failed 由事件路径接管）
                        if placeholder_alive {
                            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some(e.to_string()),
                            }).await;
                            placeholder_alive = false;
                        }
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "error",
                            "text": format!("下载失败: {}", e)
                        }));
                    }
                    // 引擎已结束，但通道里可能还有尾部事件（Done/Failed 在
                    // start_pull 返回前刚发出）——继续 drain 直到关闭

                    while let Some(ev) = progress_rx.recv().await {
                        handle_pull_progress_event(
                            &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex, ev,
                        ).await;
                    }
                    break;
                }
            }
        }
    });

    Ok(placeholder_id) // 真实 job_id 在 Started 事件中，占位 id 供前端关联重试参数
}

/// 拉取/续传共用的进度事件落地（卡片状态机 + toast）。
/// Task 4:ID 恒定——占位卡片自建卡起 ID 不变,引擎真实 job_id 到达时
/// bind_engine_id 绑定 + Started 迁移,不再删占位换实件。
#[allow(clippy::too_many_arguments)]
async fn handle_pull_progress_event(
    st: &AppState,
    app: &AppHandle,
    placeholder_id: u64,
    placeholder_alive: &mut bool,
    peer_hex: &str,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    match ev {
        localtrans_core::transfer::ProgressEvent::Started { job_id: j, name, total } => {
            // 首次到达:绑定引擎 ID + 元数据 + Started 迁移(pending→active)
            let already_bound = st.engine_id_of(placeholder_id).await.is_some();
            if !already_bound {
                st.bind_engine_id(j, placeholder_id).await;
                st.card_mutate(placeholder_id, |d| {
                    d.name = name; d.total = total;
                }).await;
            }
            st.card_apply(placeholder_id, CardEvent::Started).await;
            if *placeholder_alive { *placeholder_alive = false; }
        }
        localtrans_core::transfer::ProgressEvent::ChunkDone { bytes, .. } => {
            st.source_chunk_add(placeholder_id, bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::Resumed { already_bytes, .. } => {
            // v0.2.4 续传基线:进度从真实位置起步
            if *placeholder_alive && st.engine_id_of(placeholder_id).await.is_none() {
                st.card_apply(placeholder_id, CardEvent::Started).await;
            }
            st.card_mutate(placeholder_id, |d| d.done = already_bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::Speed { bps, .. } => {
            st.source_speed(placeholder_id, bps, 0, None).await;
        }
        localtrans_core::transfer::ProgressEvent::Done { .. } => {
            // 完成态强制满格(状态机 Finished 自带 done=total 兜底)
            // 完成日志不带文件名(安全红线:文件名不进 tracing)
            tracing::info!("下载完成 card={:016x}", placeholder_id);
            st.card_apply(placeholder_id, CardEvent::Finished).await;
            if *placeholder_alive { *placeholder_alive = false; }
        }
        localtrans_core::transfer::ProgressEvent::Failed { reason, .. } => {
            tracing::warn!("下载失败 (card {:016x}): {}", placeholder_id, reason);
            st.card_apply(placeholder_id, CardEvent::Failed {
                reason: Some(reason.clone()),
            }).await;
            if *placeholder_alive { *placeholder_alive = false; }
            let _ = app.emit("toast", serde_json::json!({
                "level": "error",
                "text": format!("下载失败: {}", reason)
            }));
        }
        _ => {}
    }
    let _ = peer_hex; // 卡片建卡时已带 peer,事件路径不再改
}

/// 推送共用的进度事件落地（卡片状态机 + toast）。
/// 与拉取版本的差异：direction=push、local_role=source-push、日志措辞。
/// Task 8:batch=true 时父卡不直接迁移终态——事件落 children,终态由
/// recheck_parent_terminal 在全部子项终态后收敛。
#[allow(clippy::too_many_arguments)]
async fn handle_push_progress_event(
    st: &AppState,
    app: &AppHandle,
    placeholder_id: u64,
    placeholder_alive: &mut bool,
    peer_hex: &str,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    handle_push_progress_event_mode(st, app, placeholder_id, placeholder_alive, peer_hex, ev, false).await
}

#[allow(clippy::too_many_arguments)]
async fn handle_push_progress_event_mode(
    st: &AppState,
    app: &AppHandle,
    placeholder_id: u64,
    placeholder_alive: &mut bool,
    peer_hex: &str,
    ev: localtrans_core::transfer::ProgressEvent,
    batch: bool,
) {
    use crate::transfer_state::CardEvent;
    match ev {
        localtrans_core::transfer::ProgressEvent::Started { job_id: j, name, total } => {
            if batch {
                // 批次模式:子项按名 upsert(小文件批流共享 offer job_id,
                // 无单文件 engine job);父卡 Started 激活,终态由 recheck 收敛
                batch_child_start(st, placeholder_id, &name, total).await;
                st.card_apply(placeholder_id, CardEvent::Started).await;
                if *placeholder_alive { *placeholder_alive = false; }
                return;
            }
            let already_bound = st.engine_id_of(placeholder_id).await.is_some();
            if !already_bound {
                st.bind_engine_id(j, placeholder_id).await;
                st.card_mutate(placeholder_id, |d| {
                    d.name = name; d.total = total;
                }).await;
            }
            st.card_apply(placeholder_id, CardEvent::Started).await;
            if *placeholder_alive { *placeholder_alive = false; }
        }
        localtrans_core::transfer::ProgressEvent::ChunkDone { bytes, .. } => {
            if !batch {
                st.source_chunk_add(placeholder_id, bytes).await;
            }
        }
        localtrans_core::transfer::ProgressEvent::Resumed { already_bytes, .. } => {
            if batch { return; }
            if *placeholder_alive && st.engine_id_of(placeholder_id).await.is_none() {
                st.card_apply(placeholder_id, CardEvent::Started).await;
            }
            st.card_mutate(placeholder_id, |d| d.done = already_bytes).await;
        }
        localtrans_core::transfer::ProgressEvent::Speed { bps, .. } => {
            if !batch {
                st.source_speed(placeholder_id, bps, 0, None).await;
            }
        }
        localtrans_core::transfer::ProgressEvent::Done { .. } => {
            if batch {
                // 批流按文件序补 Done——最旧 active 子项落 done;全终态→父 done。
                // Task 12 修复轮 1:父卡经 recheck 转 done 时补发完成 toast
                batch_child_oldest_active(st, placeholder_id, "done").await;
                if let Some((name, direction)) =
                    st.recheck_parent_terminal(placeholder_id).await
                {
                    if let Some(text) = crate::done_toast_text(&direction, &name) {
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "success",
                            "text": text
                        }));
                    }
                }
                return;
            }
            // 完成日志不带文件名(安全红线:文件名不进 tracing)
            tracing::info!("推送完成 card={:016x}", placeholder_id);
            st.card_apply(placeholder_id, CardEvent::Finished).await;
            if *placeholder_alive { *placeholder_alive = false; }
        }
        localtrans_core::transfer::ProgressEvent::Failed { reason, .. } => {
            tracing::warn!("推送失败 (card {:016x}): {}", placeholder_id, reason);
            if batch {
                batch_child_oldest_active(st, placeholder_id, "failed").await;
                st.recheck_parent_terminal(placeholder_id).await;
            } else {
                st.card_apply(placeholder_id, CardEvent::Failed {
                    reason: Some(reason.clone()),
                }).await;
                if *placeholder_alive { *placeholder_alive = false; }
            }
            let _ = app.emit("toast", serde_json::json!({
                "level": "error",
                "text": format!("推送失败: {}", reason)
            }));
        }
        _ => {}
    }
    let _ = peer_hex;
}

// ===== Task 8: 批次子项辅助 =====

/// 挂接键命名空间:tag 占高 3 位(1=批次 2=子项重试 3=单文件),指纹前 8 字节
/// 右移 3 位压入低 61 位。同指纹三键必异(高位 tag 不同);不同指纹低 61 位
/// 相同才撞(64 位前缀全等,与旧实现等价);tag≤0b011 不与 B 端 0x8000 段
/// source job id、引擎小整数 job id、卡片 0x4000 段 ID 相撞。
///
/// 根因注(2026-09-07):旧实现 `哨兵 | be64(fp[0..8])` 在指纹首字节高位
/// 已置 1 时(概率 50%,实机 fpB 首字节 0xE6)三个哨兵 OR 全被指纹位吞掉,
/// 三键同值——单文件推送的登记被 source_push_attach 批次分支命中,
/// child_upsert 追加 total+=SIZE(D2 推送 total 双计四轮 100% 复现)。
fn peer_key(fp: &[u8; 32], tag: u64) -> u64 {
    debug_assert!((1..8).contains(&tag), "tag 占高 3 位,0 保留(引擎 job 空间)");
    (tag << 61) | (u64::from_be_bytes(fp[0..8].try_into().unwrap()) >> 3)
}

/// 批次挂接键:同键复写 = 新批次接管该对端的挂接(同对端串行锁保证
/// 旧批次已结束)。
pub(crate) fn batch_peer_key(fp: &[u8; 32]) -> u64 {
    peer_key(fp, 1)
}

/// 修复轮 P3:子项重试专用挂接键(与活动批次空间隔离)——retry 期间
/// 不顶掉同 peer 活动批次的登记;source 桥查父卡时两个键都查。
pub(crate) fn retry_peer_key(fp: &[u8; 32]) -> u64 {
    peer_key(fp, 2)
}

/// N1-T1b:单文件推送挂接键(与批次/重试空间隔离)。单文件推送无
/// children 语义——source 桥命中此键时把 source job 被动绑回占位卡
/// (不建第二张卡),修复"一次推送 done×2 双卡"(2026-09-06 探针实证)。
pub(crate) fn single_peer_key(fp: &[u8; 32]) -> u64 {
    peer_key(fp, 3)
}

/// N1-T1b:source 桥 SourceStarted(SourcePush) 的挂接解析。
/// 依次查:活动批次 → 子项重试 → 单文件推送登记。
/// 返回 true=事件已消化(挂到既有卡,不建新卡);false=无任何登记,走通用建卡路径。
pub(crate) async fn source_push_attach(
    st: &AppState, job_id: u64, peer: &[u8; 32], name: &str, total: u64,
) -> bool {
    let is_terminal = |s: &str| matches!(s, "done" | "failed" | "interrupted");
    let mut parent = st.pending_children_parent_of(batch_peer_key(peer)).await;
    if parent.is_none() {
        parent = st.pending_children_parent_of(retry_peer_key(peer)).await;
    }
    if let Some(pid) = parent {
        if st.card_get(pid).await
            .map(|c| !is_terminal(c.dto.state.as_str()))
            .unwrap_or(false)
        {
            // 按名替换占位子项(编排器 Started 先建 job_id=""),或追加新子项
            // ——子 job_id=真实 engine job(可控/可重试);首个子 job 兼作
            // 共享 offer job 锚(整批控制可触达小文件批流)。
            st.child_upsert(pid, crate::ChildDto {
                job_id: format!("{:x}", job_id),
                name: name.to_string(),
                total, done: 0, state: "active".into(),
            }).await;
            st.pending_children_bind(job_id, pid).await;
            st.batch_offer_job_bind(pid, job_id).await;
            return true;
        }
    }
    // 单文件推送:source job 被动绑回占位卡(engine_to_card 路由用),
    // 不顶 card.engine_id——控制命令仍路由首个绑定的 offer job,
    // T16 控制桥经共享 Arc 把暂停/取消级联到 source 任务。
    if let Some(pid) = st.pending_children_parent_of(single_peer_key(peer)).await {
        match st.card_get(pid).await {
            Some(c) if !is_terminal(c.dto.state.as_str()) => {
                st.bind_engine_id_passive(job_id, pid).await;
                if c.dto.state.as_str() == "pending" {
                    st.engine_event(job_id, crate::transfer_state::CardEvent::Started).await;
                }
                true
            }
            // 已终态/并发移除:丢弃不建卡(状态机外事件不复活)
            _ => true,
        }
    } else {
        false
    }
}

/// 大文件子项字节累加(source 桥 SourceChunkDone;父卡计数不动,子项自计)
pub(crate) async fn batch_child_chunk(st: &AppState, parent: u64, engine_id: u64, bytes: u64) {
    let job_hex = format!("{:x}", engine_id);
    let cur = st.card_dto(parent).await.and_then(|d|
        d.children.iter().find(|c| c.job_id == job_hex)
            .map(|c| (c.name.clone(), c.total, c.done)));
    let Some((name, total, done)) = cur else { return };
    st.child_upsert(parent, crate::ChildDto {
        job_id: job_hex, name, total, done: done + bytes, state: "active".into(),
    }).await;
}

/// 大文件子项终态(source 桥 SourceDone/SourceFailed) + 父终态收敛。
/// Task 12 修复轮 1:父卡经 recheck 从非终态转 done 时返回 Some((name, direction)),
/// 供调用方补发完成 toast;其余收敛路径返回 None。
pub(crate) async fn batch_child_finish(
    st: &AppState, parent: u64, engine_id: u64, state: &str, reason: Option<String>,
) -> Option<(String, String)> {
    let job_hex = format!("{:x}", engine_id);
    let cur = st.card_dto(parent).await.and_then(|d|
        d.children.iter().find(|c| c.job_id == job_hex)
            .map(|c| (c.name.clone(), c.total, c.done)));
    let (name, total, done) = cur.unwrap_or_else(|| (String::new(), 0, 0));
    st.child_upsert(parent, crate::ChildDto {
        job_id: job_hex, name, total, done, state: state.into(),
    }).await;
    let _ = reason;
    st.recheck_parent_terminal(parent).await
}

/// Started:按名 upsert active 子项(job_id 留空——小文件批流无单文件 engine job)
async fn batch_child_start(st: &AppState, parent: u64, name: &str, total: u64) {
    st.child_upsert(parent, crate::ChildDto {
        job_id: String::new(),
        name: name.to_string(),
        total, done: 0, state: "active".into(),
    }).await;
}

/// Done/Failed:最旧 active(且 job_id 为空,即小文件批流项)子项落终态。
/// 批流 Done 按文件序补齐,与 Started 同序;大文件子项终态由 source 桥
/// 按 engine job 精确落(main.rs 事件桥 pending_children 分支)。
async fn batch_child_oldest_active(st: &AppState, parent: u64, state: &str) {
    let victim = st.card_dto(parent).await.and_then(|d| d.children.into_iter()
        .find(|c| c.state == "active" && c.job_id.is_empty()));
    let Some(c) = victim else { return };
    // 修复轮 P2:终态保留 Started 记录的总量,done=total(完成语义)——
    // 不再把 total/done 归零抹掉父卡已累加的量
    st.child_upsert(parent, crate::ChildDto {
        job_id: String::new(),
        name: c.name,
        total: c.total,
        done: c.total,
        state: state.into(),
    }).await;
}

/// 推送文件
#[tauri::command]
pub async fn push_files(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    fingerprint: String,
    local_paths: Vec<String>,
) -> Result<u64, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    let paths: Vec<PathBuf> = local_paths.iter()
        .map(|p| PathBuf::from(p))
        .collect();

    tracing::info!("开始推送: 对端={} 共{}个文件", fingerprint, paths.len());

    // 立即插入"等待中"占位卡片（同下载：排队/协商期间可见;ID 恒定）
    let placeholder_name = paths.first()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let placeholder_name = if paths.len() > 1 {
        format!("{} 等 {} 个文件", placeholder_name, paths.len())
    } else {
        placeholder_name
    };
    let placeholder_id = state.card_create(TransferDto {
        job_id: 0,
        name: placeholder_name,
        total: 0,
        done: 0,
        state: "pending".into(),
        speed_bps: 0,
        peer: fingerprint.clone(),
        direction: "push".into(),
        local_role: "destination".into(),
        health: None,
        started_at_ms: None, finished_at_ms: None, source_path: None, fail_reason: None,
        remote_done: 0, instant: false,
        queue_pos: None, batch_id: None, children: vec![], parts_id: None,
    }).await;

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

    let sm = state.sm.clone();
    let fp_for_task = fp;
    let peer_hex_for_task = fingerprint.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();

    // N1-T2:接通取消信号——plain push 此前以 cancel=None 进引擎,取消后
    // JobDone 等待循环收不到信号,引擎把整份文件传完才释放并发槽(探针
    // 实证:200MB 取消后 B 侧继续收、再推永久排队)。与 push_files_rel 同款。
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    state.placeholder_cancels.lock().await.insert(placeholder_id, cancel_flag.clone());

    // Task 8:多文件 → 批次模式(单父卡+子项跟踪,不裂变)。
    // 修复轮 P3:登记移到闸门槽位之后(串行段内)——同 peer 单槽保证排队中的
    // 两个批次不会互相顶掉挂接登记。
    let batch = paths.len() > 1;
    let fp_for_reg = fp_for_task;
    let paths_pairs: Vec<(PathBuf, String)> = paths.into_iter()
        .map(|p| (p, String::new())).collect();

    tokio::spawn(async move {
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &peer_hex_for_task),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("推送排队超时：并发闸门满且同对端前序任务持有超过 60s");
                st.placeholder_cancels.lock().await.remove(&placeholder_id);
                // 修复轮 1:超时取消后清排队位次残留
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                    reason: Some("排队超时".into()),
                }).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error",
                    "text": "排队超时：前序传输任务似乎卡住了，请检查或取消它"
                }));
                return;
            }
        };

        // 修复轮 P3:拿到槽位后再登记批次挂接键(串行段内,同 peer 后续批次
        // 必须等本批释放,登记不会被后批覆盖)
        // N1-T1b:单文件推送也登记(独立键)——source 桥把大文件回拉的
        // source job 被动绑回占位卡,不建第二张卡。登记天然自愈:下次同
        // peer 推送会覆写键值;终态卡上的迟到事件被丢弃(source_push_attach)。
        if batch {
            st.pending_children.lock().await.insert(batch_peer_key(&fp_for_reg), placeholder_id);
        } else {
            st.pending_children.lock().await.insert(single_peer_key(&fp_for_reg), placeholder_id);
        }

        // 处理进度事件（推送可能多次 Started——每个文件一个任务）
        let mut placeholder_alive = true;
        let mut progress_stopped = false;

        // v0.2.2 根因修复：进度通道与推送引擎并发消费（同 start_download，
        // 此前串行等待会让通道塞满、引擎阻塞在 send 上）
        let push_fut = localtrans_core::transfer::push_files_rel_cancellable(
            &sm,
            &fp_for_task,
            paths_pairs,
            &st.sender_jobs,
            progress_tx,
            Some(cancel_flag),
        );
        tokio::pin!(push_fut);

        // N1-T2:成功路径也必须退出——此前仅 Err 分支 break,引擎 Ok 后
        // 已完成的 future 仍被 select 反复 poll,任务永不结束,acquire_slot
        // 的并发许可与对端锁随之泄漏(连推 3 次占满闸门,后续推送永久排队,
        // 2026-09-06 探针实证)。engine_done 守卫:完成后禁用该分支,尾部
        // 事件经首分支消化,通道关闭(progress_stopped)后统一退出。
        let mut engine_done = false;
        loop {
            tokio::select! {
                biased;
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_push_progress_event_mode(
                            &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex_for_task, ev, batch,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut push_fut, if !engine_done => {
                    engine_done = true;
                    // 任务结束(成败/取消)——清理占位取消信号
                    st.placeholder_cancels.lock().await.remove(&placeholder_id);
                    if let Err(e) = res {
                        tracing::warn!("推送失败: {}", e);
                        if placeholder_alive {
                            if placeholder_alive {
                                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                    reason: Some(e.to_string()),
                                }).await;
                                placeholder_alive = false;
                            }
                            let _ = app.emit("toast", serde_json::json!({
                                "level": "error",
                                "text": format!("推送失败: {}", e)
                            }));
                        }
                        // 引擎结束后 drain 尾部事件

                        while let Some(ev) = progress_rx.recv().await {
                            handle_push_progress_event(
                                &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex_for_task, ev,
                            ).await;
                        }
                        break;
                    }
                    // Ok:不在此 drain——尾部事件经首分支继续消化
                }
            }
            if engine_done && progress_stopped {
                break;
            }
        }
    });

    Ok(placeholder_id)
}

/// v0.2.6 文件夹下载：递归枚举远端目录 → 聚合任务行 → 逐文件 start_pull_into
/// 保持子目录结构（下载根/文件夹名/...）。单文件失败继续传其余。
#[tauri::command]
pub async fn start_download_dir(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    fingerprint: String,
    share_id: String,
    path: String,
) -> Result<u64, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    tracing::info!("开始文件夹下载: 对端={} 共享区={}", fingerprint, share_id);

    // 聚合占位卡片（文件夹整体一行;ID 恒定）
    let folder_name = std::path::Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    let placeholder_id = state.card_create(TransferDto {
        job_id: 0,
        name: format!("[文件夹] {}", folder_name),
        total: 0,
        done: 0,
        state: "pending".into(),
        speed_bps: 0,
        peer: fingerprint.clone(),
        direction: "pull".into(),
        local_role: "destination".into(),
        health: None,
        started_at_ms: None,
        finished_at_ms: None,
        // v0.2.8 恢复参数：重启后 resume_pending 据此重新走文件夹编排
        source_path: Some(format!("{}|{}", share_id, path)),
        fail_reason: None,
        remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
    }).await;

    let config = state.config.read().await.clone();
    let sm = state.sm.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();

    let share_id_task = share_id.clone();
    let path_task = path.clone();
    tokio::spawn(async move {
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &fingerprint),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("文件夹下载排队超时");
                // 修复轮 1:超时取消后清排队位次残留
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error",
                    "text": "排队超时：前序传输任务似乎卡住了"
                }));
                return;
            }
        };

        run_dir_pull_task(&st, &app, &sm, &fp, &share_id_task, &path_task, &config, placeholder_id).await;
    });

    Ok(placeholder_id)
}

/// v0.2.8 文件夹拉取编排（下载与断点恢复共用）：
/// 聚合行事件泵 + 单文件进度泵 + start_pull_dir。
/// 恢复语义：已 finalize 的文件由 pending_jobs 匹配跳过（无 parts 目录），
/// 未完文件命中 parts 续传——把聚合行 done 基线重置为 0 后由事件重放累加。
#[allow(clippy::too_many_arguments)]
async fn run_dir_pull_task(
    st: &AppState,
    app: &AppHandle,
    sm: &std::sync::Arc<localtrans_core::session::SessionManager>,
    fp: &[u8; 32],
    share_id: &str,
    path: &str,
    config: &localtrans_core::store::Config,
    placeholder_id: u64,
) {
    use localtrans_core::transfer::{DirPullEvent, ProgressEvent};

    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::channel::<DirPullEvent>(64);
    let (file_prog_tx, mut file_prog_rx) = tokio::sync::mpsc::channel::<ProgressEvent>(64);

    // 聚合行更新器：Enumerated 定总量，FileDone 累加，AllDone 定终态。
    let st_agg = st.clone();
    let app_agg = app.clone();
    let agg_handle = tokio::spawn(async move {
        while let Some(ev) = ev_rx.recv().await {
            match ev {
                DirPullEvent::Enumerated { file_count, total_bytes } => {
                    let _ = &file_count;
                    // 先 Started 激活，再写总量（进度自环走 mut，不走状态机）
                    st_agg.card_apply(placeholder_id, crate::transfer_state::CardEvent::Started).await;
                    st_agg.card_mutate(placeholder_id, |dto| dto.total = total_bytes).await;
                }
                DirPullEvent::FileDone { rel_path, bytes } => {
                    st_agg.source_chunk_add(placeholder_id, bytes).await;
                    // Task 8:子明细挂聚合卡。FileDone 无 per-file engine job_id
                    // (逐文件 start_pull_parts 内部分配,不经编排器)——job_id 留空,
                    // state 仍可显示;单文件重试走父卡整批重传
                    st_agg.child_upsert(placeholder_id, crate::ChildDto {
                        job_id: String::new(),
                        name: rel_path,
                        total: bytes, done: bytes, state: "done".into(),
                    }).await;
                }
                DirPullEvent::FileFailed { rel_path, reason } => {
                    // 安全红线:rel_path 不进日志,只保留类别+卡标识
                    tracing::warn!("文件夹内文件失败 card={:016x} reason={}", placeholder_id, reason);
                    let _ = &rel_path;
                    // Task 8:失败子项也挂聚合卡(state=failed,重试入口可见)
                    st_agg.child_upsert(placeholder_id, crate::ChildDto {
                        job_id: String::new(),
                        name: rel_path,
                        total: 0, done: 0, state: "failed".into(),
                    }).await;
                    let _ = reason;
                }
                DirPullEvent::AllDone { succeeded, failed } => {
                    if failed.is_empty() {
                        tracing::info!("文件夹下载完成: {} 个文件", succeeded);
                        st_agg.card_apply(placeholder_id, crate::transfer_state::CardEvent::Finished).await;
                        // Task 12 修复轮 1:文件夹拉取完成补发成功 toast(此前仅失败提示)
                        if let Some(text) = crate::done_toast_text("pull",
                            &st_agg.card_dto(placeholder_id).await
                                .map(|d| d.name).unwrap_or_default())
                        {
                            let _ = app_agg.emit("toast", serde_json::json!({
                                "level": "success",
                                "text": text
                            }));
                        }
                    } else {
                        st_agg.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                            reason: Some(format!("{} 个文件失败", failed.len())),
                        }).await;
                    }
                    if !failed.is_empty() {
                        let _ = app_agg.emit("toast", serde_json::json!({
                            "level": "warning",
                            "text": format!("文件夹传输完成：{} 成功 / {} 失败", succeeded, failed.len())
                        }));
                    }
                }
            }
        }
    });

    // 单文件进度泵：ChunkDone 字节累到聚合行（小文件无块事件，由 FileDone 补）
    let st_chunk = st.clone();
    let chunk_handle = tokio::spawn(async move {
        while let Some(ev) = file_prog_rx.recv().await {
            if let ProgressEvent::ChunkDone { bytes, .. } = ev {
                st_chunk.source_chunk_add(placeholder_id, bytes).await;
            }
        }
    });

    let result = localtrans_core::transfer::start_pull_dir(
        sm, fp, share_id, path, config, ev_tx, file_prog_tx,
    ).await;

    if let Err(e) = result {
        tracing::warn!("文件夹拉取失败: {}", e);
        st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
            reason: Some(e.to_string()),
        }).await;
        let _ = app.emit("toast", serde_json::json!({
            "level": "error",
            "text": format!("文件夹下载失败: {}", e)
        }));
    }

    let _ = agg_handle.await;
    let _ = chunk_handle.await;
}

/// N1-T4:多选文件批量下载——1 批次恒 1 张父卡片(2026-08-30 浏览页定案),
/// 逐文件顺序拉取聚合(子项明细挂父卡),替代前端逐文件 N 任务。
/// 限制(与文件夹拉取同款语义):子项无独立 engine job——单文件重试走
/// 父卡整批重传;取消在文件边界生效(进行中文件靠断点续传保护)。
#[tauri::command]
pub async fn start_download_batch(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    fingerprint: String,
    share_id: String,
    paths: Vec<String>,
) -> Result<u64, String> {
    if paths.is_empty() {
        return Err("批量下载文件列表为空".into());
    }
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    let first_name = std::path::Path::new(&paths[0])
        .file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| paths[0].clone());
    let card_name = if paths.len() == 1 { first_name } else { format!("{} 等 {} 项", first_name, paths.len()) };
    tracing::info!("开始批量下载: 对端={} 共{}项", fingerprint, paths.len());

    let placeholder_id = state.card_create(TransferDto {
        job_id: 0,
        name: card_name,
        total: 0, done: 0, state: "pending".into(), speed_bps: 0,
        peer: fingerprint.clone(),
        direction: "pull".into(), local_role: "destination".into(),
        health: None, started_at_ms: None, finished_at_ms: None,
        source_path: None, fail_reason: None,
        remote_done: 0, instant: false,
        queue_pos: None, batch_id: None, children: vec![], parts_id: None,
    }).await;

    // 取消信号:transfer_action 取消父卡时置位,文件边界/500ms 心跳检查
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    state.placeholder_cancels.lock().await.insert(placeholder_id, cancel_flag.clone());

    let config = state.config.read().await.clone();
    let sm = state.sm.clone();
    let reg = state.reg.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();
    let sid = share_id;

    tokio::spawn(async move {
        let peer_hex = hex::encode(fp);
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &peer_hex),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("批量下载排队超时");
                st.placeholder_cancels.lock().await.remove(&placeholder_id);
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error", "text": "排队超时：前序传输任务似乎卡住了，请检查或取消它"
                }));
                return;
            }
        };

        let mut ok_count = 0u32;
        let mut failed_count = 0u32;
        let mut cancelled = false;

        'batch: for rel in &paths {
            if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                cancelled = true;
                break;
            }
            let display_name = std::path::Path::new(rel)
                .file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| rel.clone());
            let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
            let pull_fut = localtrans_core::transfer::start_pull(
                &sm, &reg, &fp, &sid, rel, &config, progress_tx,
            );
            tokio::pin!(pull_fut);

            let mut progress_stopped = false;
            let mut engine_done = false;
            let mut file_failed = false;
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(500));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

            loop {
                tokio::select! {
                    biased;
                    _ = tick.tick(), if !engine_done => {
                        if cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
                            cancelled = true;
                            break 'batch;
                        }
                    }
                    ev = progress_rx.recv(), if !progress_stopped => {
                        match ev {
                            Some(ev) => handle_batch_pull_event(
                                &st, placeholder_id, &display_name,
                                &mut file_failed, ev,
                            ).await,
                            None => progress_stopped = true,
                        }
                    }
                    res = &mut pull_fut, if !engine_done => {
                        engine_done = true;
                        if let Err(e) = res {
                            tracing::warn!("批量下载内文件失败: {}", e);
                            file_failed = true;
                            st.child_upsert(placeholder_id, crate::ChildDto {
                                job_id: String::new(), name: display_name.clone(),
                                total: 0, done: 0, state: "failed".into(),
                            }).await;
                            st.card_mutate(placeholder_id, |d| {
                                d.fail_reason = Some(e.to_string());
                            }).await;
                        }
                        // Ok:尾部事件经首分支消化(通道关闭后退出)
                    }
                }
                if engine_done && progress_stopped {
                    break;
                }
            }
            if file_failed { failed_count += 1; } else { ok_count += 1; }
        }

        st.placeholder_cancels.lock().await.remove(&placeholder_id);
        st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
        if cancelled {
            // 卡片终态由取消/删除仲裁路径收(cancelling→确认/看门狗)
            tracing::info!("批量下载已取消 card={:016x}", placeholder_id);
            return;
        }
        if failed_count == 0 && ok_count > 0 {
            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Finished).await;
            if let Some(text) = crate::done_toast_text("pull",
                &st.card_dto(placeholder_id).await.map(|d| d.name).unwrap_or_default())
            {
                let _ = app.emit("toast", serde_json::json!({ "level": "success", "text": text }));
            }
        } else if ok_count == 0 {
            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                reason: Some(format!("{} 项全部失败", failed_count)),
            }).await;
        } else {
            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                reason: Some(format!("{} 项失败", failed_count)),
            }).await;
        }
    });

    Ok(placeholder_id)
}

/// N1-T4:批次拉取的单文件事件 → 父卡聚合(总量跨文件累计,子项明细挂父卡)。
#[allow(clippy::too_many_arguments)]
async fn handle_batch_pull_event(
    st: &AppState,
    parent: u64,
    display_name: &str,
    file_failed: &mut bool,
    ev: localtrans_core::transfer::ProgressEvent,
) {
    use crate::transfer_state::CardEvent;
    use localtrans_core::transfer::ProgressEvent as PE;
    // 计量纪律:父卡 total/done 全部经 child_upsert 差分同步(新子项加总量,
    // 子项更新按 done 差额平移)——直接改父卡计数会双计(真机实证 60→120)
    match ev {
        PE::Started { total, .. } => {
            st.card_apply(parent, CardEvent::Started).await;
            st.child_upsert(parent, crate::ChildDto {
                job_id: String::new(), name: display_name.to_string(),
                total, done: 0, state: "active".into(),
            }).await;
        }
        PE::Resumed { already_bytes, .. } => {
            // 续传无 Started:补建子项并直接落在续传基线
            st.card_apply(parent, CardEvent::Started).await;
            st.child_upsert(parent, crate::ChildDto {
                job_id: String::new(), name: display_name.to_string(),
                total: 0, done: already_bytes, state: "active".into(),
            }).await;
        }
        PE::ChunkDone { bytes, .. } => {
            if let Some((name, total, done)) = st.card_dto(parent).await.and_then(|d| {
                d.children.iter().find(|c| c.name == display_name && c.job_id.is_empty())
                    .map(|c| (c.name.clone(), c.total, c.done))
            }) {
                st.child_upsert(parent, crate::ChildDto {
                    job_id: String::new(), name, total,
                    done: done + bytes, state: "active".into(),
                }).await;
            }
        }
        PE::Speed { bps, .. } => {
            st.source_speed(parent, bps, 0, None).await;
        }
        PE::InstantHit { total, .. } => {
            // 秒传:Started/Done 均不发——子项一次性落满(父卡计量随子项)
            st.card_apply(parent, CardEvent::Started).await;
            st.card_mutate(parent, |d| d.instant = true).await;
            st.child_upsert(parent, crate::ChildDto {
                job_id: String::new(), name: display_name.to_string(),
                total, done: total, state: "done".into(),
            }).await;
        }
        PE::Done { .. } => {
            // 子项落 done(done 取子项记录的 total)
            let total = st.card_dto(parent).await
                .and_then(|d| d.children.iter().find(|c| c.name == display_name && c.state == "active")
                    .map(|c| c.total))
                .unwrap_or(0);
            st.child_upsert(parent, crate::ChildDto {
                job_id: String::new(), name: display_name.to_string(),
                total, done: total, state: "done".into(),
            }).await;
        }
        PE::Failed { reason, .. } => {
            *file_failed = true;
            tracing::warn!("批量下载内文件失败: {}", reason);
            st.child_upsert(parent, crate::ChildDto {
                job_id: String::new(), name: display_name.to_string(),
                total: 0, done: 0, state: "failed".into(),
            }).await;
            st.card_mutate(parent, |d| d.fail_reason = Some(reason)).await;
        }
        _ => {}
    }
}

/// v0.2.6 带结构推送：items 为 (本地路径, rel_dir) 对列表。
/// 前端枚举本地文件夹生成 items（dialog 选文件夹后 walk）。
#[tauri::command]
pub async fn push_files_rel(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    fingerprint: String,
    items: Vec<(String, String)>,
) -> Result<u64, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    let files: Vec<(PathBuf, String)> = items.iter()
        .map(|(p, rel)| (PathBuf::from(p), rel.clone()))
        .collect();

    tracing::info!("开始结构化推送: 对端={} 共{}个文件", fingerprint, files.len());

    let placeholder_name = files.first()
        .and_then(|(p, _)| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    let placeholder_name = if files.len() > 1 {
        format!("{} 等 {} 个文件", placeholder_name, files.len())
    } else {
        placeholder_name
    };
    let placeholder_id = state.card_create(TransferDto {
        job_id: 0,
        name: placeholder_name,
        total: 0,
        done: 0,
        state: "pending".into(),
        speed_bps: 0,
        peer: fingerprint.clone(),
        direction: "push".into(),
        local_role: "source-push".into(),
        health: None,
        started_at_ms: None,
        finished_at_ms: None, source_path: None, fail_reason: None,
        remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
    }).await;

    let sm = state.sm.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();

    // v0.11.x BUG01:占位期取消信号——对端离线卡在等 OfferResp 时,
    // transfer_action 按占位 ID 触发 cancel,等待循环 ≤5s 内退出
    let cancel_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    state.placeholder_cancels.lock().await.insert(placeholder_id, cancel_flag.clone());

    // Task 8:多文件 → 批次模式(单父卡+子项跟踪)。
    // 修复轮 P3:登记移到闸门槽位之后(串行段内)。
    let batch = files.len() > 1;
    let fp_for_reg = fp;

    tokio::spawn(async move {
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            acquire_with_queue_pos(&st, placeholder_id, &fingerprint),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("结构化推送排队超时");
                st.placeholder_cancels.lock().await.remove(&placeholder_id);
                // 修复轮 1:超时取消后清排队位次残留
                st.card_mutate(placeholder_id, |d| d.queue_pos = None).await;
                st.card_apply(placeholder_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error",
                    "text": "排队超时：前序传输任务似乎卡住了"
                }));
                return;
            }
        };

        // 修复轮 P3:拿到槽位后再登记批次挂接键
        // N1-T1b:单文件推送登记独立键(source job 绑回占位卡,见 push_files 注)
        if batch {
            st.pending_children.lock().await.insert(batch_peer_key(&fp_for_reg), placeholder_id);
        } else {
            st.pending_children.lock().await.insert(single_peer_key(&fp_for_reg), placeholder_id);
        }

        let mut placeholder_alive = true;
        let mut progress_stopped = false;
        let peer_hex_for_task = hex::encode(fp);

        let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
        let push_fut = localtrans_core::transfer::push_files_rel_cancellable(&sm, &fp, files, &st.sender_jobs, progress_tx, Some(cancel_flag));
        tokio::pin!(push_fut);

        loop {
            tokio::select! {
                biased;
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_push_progress_event_mode(
                            &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex_for_task, ev, batch,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut push_fut => {
                    // 任务结束(成败/取消)——清理占位取消信号
                    st.placeholder_cancels.lock().await.remove(&placeholder_id);
                    if let Err(e) = res {
                        tracing::warn!("结构化推送失败: {}", e);
                        if placeholder_alive {
                            st.card_apply(placeholder_id, crate::transfer_state::CardEvent::Failed {
                                reason: Some(e.to_string()),
                            }).await;
                            placeholder_alive = false;
                        }
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "error",
                            "text": format!("推送失败: {}", e)
                        }));
                    }
                    while let Some(ev) = progress_rx.recv().await {
                        handle_push_progress_event_mode(
                            &st, &app, placeholder_id, &mut placeholder_alive, &peer_hex_for_task, ev, batch,
                        ).await;
                    }
                    break;
                }
            }
        }
    });

    Ok(placeholder_id)
}

/// v0.2.6 展开本地路径列表为 (文件, rel_dir) 对：文件夹递归枚举
/// （深度≤32、单次上限 10000 文件），文件原样通过。拖放文件夹推送用。
#[tauri::command]
pub async fn expand_local_paths(
    paths: Vec<String>,
) -> Result<Vec<(String, String)>, String> {
    // v0.11.0 M-C7:BFS 枚举最多 1 万文件、纯阻塞 IO——下放阻塞线程池,
    // 不占 Tauri async 运行时 worker
    tokio::task::spawn_blocking(move || expand_local_paths_blocking(paths))
        .await
        .map_err(|e| format!("路径展开任务失败: {}", e))?
}

fn expand_local_paths_blocking(
    paths: Vec<String>,
) -> Result<Vec<(String, String)>, String> {
    const MAX_DEPTH: u32 = 32;
    const MAX_FILES: usize = 10_000;
    let mut out: Vec<(String, String)> = Vec::new();

    // A3 隐私门禁:拒绝系统敏感目录(拖放的路径前端不可信,弹窗/自定义输入
    // 可能被诱导指向系统目录批量枚举)。前缀黑名单足够,不过度设计。
    fn is_sensitive_path(p: &std::path::Path) -> bool {
        let lower = p.to_string_lossy().to_lowercase().replace('/', "\\");
        for banned in ["\\windows", "\\program files", "\\program files (x86)", "system32"] {
            if lower.contains(banned) {
                return true;
            }
        }
        false
    }

    for p in &paths {
        let path = PathBuf::from(p);
        if is_sensitive_path(&path) {
            return Err(format!("拒绝访问系统敏感目录: {}", p));
        }
        let meta = std::fs::metadata(&path)
            .map_err(|e| format!("无法读取 {}: {}", p, e))?;
        if !meta.is_dir() {
            out.push((p.clone(), String::new()));
            continue;
        }
        // 文件夹：BFS 展开文件，rel 相对文件夹自身（含文件夹名——接收方落同名目录）
        let root_name = path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("folder")
            .to_string();
        let mut queue: std::collections::VecDeque<(PathBuf, String, u32)> =
            std::collections::VecDeque::new();
        queue.push_back((path.clone(), root_name.clone(), 0));
        while let Some((dir, rel_prefix, depth)) = queue.pop_front() {
            if depth > MAX_DEPTH {
                tracing::warn!("本地目录深度超限，跳过: {}", dir.display());
                continue;
            }
            let entries = std::fs::read_dir(&dir)
                .map_err(|e| format!("无法读取目录 {}: {}", dir.display(), e))?;
            for entry in entries.flatten() {
                let ep = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                // 隐藏文件/系统目录跳过（与共享区浏览一致）
                if name.starts_with('.') {
                    continue;
                }
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::fs::MetadataExt;
                    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
                    if let Ok(m) = entry.metadata() {
                        if m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0 {
                            continue;
                        }
                    }
                }
                let child_rel = format!("{}/{}", rel_prefix, name);
                if ep.is_dir() {
                    queue.push_back((ep, child_rel, depth + 1));
                } else {
                    out.push((ep.to_string_lossy().into_owned(), child_rel));
                    if out.len() >= MAX_FILES {
                        return Err(format!("文件数超上限 {}（包含文件夹过大）", MAX_FILES));
                    }
                }
            }
        }
    }
    Ok(out)
}

/// v0.2.7 优雅关闭准备：前端 destroy 窗口前调用。
/// ① 对全部已连接对端发 Goodbye（对端即时 SessionDown、任务落 interrupted，
///   不用等 60s idle 超时才知道我们走了）
/// ② 本机 active/paused 任务转 interrupted（下次启动走断点恢复）
/// ③ 强制落盘传输表（不等 1s 周期）——PartWriter 位图由 Drop 兜底已持久化
#[tauri::command]
pub async fn prepare_shutdown(state: State<'_, AppState>) -> Result<(), String> {
    // v0.11.x BUG01:关闭路径必须有界——此前 shutdown_all(每会话最多5s串行)+
    // relay.shutdown 可能累计超过前端 15s invoke 超时,前端 reject 后不再
    // 调 destroy,窗口留在屏幕上"关不掉"。整个准备过程 5s 封顶,超时也继续
    // 走落盘+退出(优雅通知是尽力而为,不是退出前置条件)。
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        // 先把任务转终态（此时会话还在，Goodbye 后对端会把自己的行转 interrupted）
        let open_ids: Vec<u64> = state.snapshot_dtos().await.into_iter()
            .filter(|d| matches!(d.state.as_str(), "active" | "pending" | "paused"))
            .map(|d| d.job_id)
            .collect();
        for id in open_ids {
            state.card_apply(id, crate::transfer_state::CardEvent::Interrupted).await;
        }

        // 通知对端（每个 Goodbye 走各会话控制流， drained 后 close）
        state.sm.shutdown_all().await;

        // 中继优雅下线:Leave(对端经名册秒级看到)+abort 事件桥任务
        if let Some(relay) = state.relay.lock().await.as_ref() {
            let _ = relay.shutdown().await;
        }

        // Abort 中继事件桥任务
        if let Some(h) = state.relay_event_task.lock().await.take() {
            h.abort();
        }
    })
    .await
    .ok(); // 超时不阻断退出(优雅通知尽力而为)——Err 仅意味着 5s 内没通知完

    // v0.11.x BUG01:关闭前点燃所有占位取消信号——等 OfferResp 的推送
    // 任务立即退出,不留悬挂 spawn
    {
        let cancels = state.placeholder_cancels.lock().await;
        for (_, flag) in cancels.iter() {
            flag.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    // 立即落盘（脏标记已置位，这里直接写不等 1s tick——进程马上要退）
    let table = state.snapshot_dtos().await;
    let mut open_jobs: Vec<TransferDto> = table.iter()
        .filter(|d| !matches!(d.state.as_str(), "done" | "failed"))
        .cloned().collect();
    let mut history: Vec<TransferDto> = table.iter()
        .filter(|d| matches!(d.state.as_str(), "done" | "failed"))
        .cloned().collect();
    history.truncate(150);
    open_jobs.extend(history);
    let path = state.dir.join("transfers.json");
    let tmp = path.with_extension("json.tmp");
    serde_json::to_string_pretty(&open_jobs)
        .map_err(|e| e.to_string())
        .and_then(|s| std::fs::write(&tmp, s).map_err(|e| e.to_string()))
        .and_then(|_| std::fs::rename(&tmp, &path).map_err(|e| e.to_string()))?;
    tracing::info!("优雅关闭准备完成：任务表已落盘");
    Ok(())
}

/// v0.11.x 用户裁定:保存位置不再限制在下载目录内(自定义/另存为任意目录)。
/// 原路径门禁(ensure_save_dir_authorized)已移除——安全底线仍由
/// core 的 sanitize_file_name(文件名净化,拒绝穿越/保留名/控制字符)
/// 与接收侧落盘路径拼接逻辑保证;这里只做最小校验:非空且是绝对路径。
fn ensure_save_dir_valid(save_dir: &Path) -> Result<(), String> {
    if save_dir.as_os_str().is_empty() {
        return Err("保存目录不能为空".to_string());
    }
    if !save_dir.is_absolute() {
        return Err("保存目录必须是绝对路径".to_string());
    }
    Ok(())
}

/// 响应对等方文件推送请求
#[tauri::command]
pub async fn respond_offer(
    state: State<'_, AppState>,
    job_id: u64,
    accepted: bool,
    save_dir: Option<String>,
) -> Result<(), String> {
    let pending = state.pending_offers.lock().await.remove(&job_id)
        .ok_or_else(|| format!("未找到待答任务: {}", job_id))?;

    // Ask 应答时清除 auto_offers（Ask 用户已确认过，不需要通知）
    state.auto_offers.lock().await.remove(&job_id);

    let save = if accepted {
        let dl = state.config.read().await.download_dir.clone();
        let dir = save_dir.map(PathBuf::from).unwrap_or_else(|| dl.clone());
        // v0.11.x 用户裁定:保存位置不限下载目录(自定义/另存为任意路径)。
        // 仍做最小校验(非空+绝对路径);文件名净化在 core sanitize_file_name。
        ensure_save_dir_valid(&dir)?;
        Some(dir)
    } else {
        None
    };

    pending.respond.send(save)
        .map_err(|_| "该请求已超时或已结束".to_string())
}

/// v0.5.0 "另存中"顺延：UI 打开目录选择器前调用，确认 deadline 重置一次
#[tauri::command]
pub async fn offer_extend(state: State<'_, AppState>, job_id: u64) -> Result<(), String> {
    let pending_offers = state.pending_offers.lock().await;
    let pending = pending_offers.get(&job_id)
        .ok_or_else(|| format!("未找到待答任务: {}", job_id))?;
    pending.extend.notify_one();
    Ok(())
}

/// P0-2c: 应答远程删除确认
#[tauri::command]
pub async fn respond_delete(
    state: State<'_, AppState>,
    ask_id: u64,
    allow: bool,
) -> Result<(), String> {
    if let Some(tx) = state.pending_deletes.lock().await.remove(&ask_id) {
        let _ = tx.send(allow);
    }
    Ok(())
}

/// 占位取消信号分支的决策(纯函数,便于单测)。
/// 占位信号自命令发起存活至引擎结束——覆盖排队/OfferResp 等待期与整个
/// 传输期。任务尚未 Started:cancel 置标志即时退出,pause/resume 无意义
/// 拒绝;已 Started:pause/resume 必须落到 T16 正常控制路径——旧实现把
/// 传输中的 pause 误拒"任务尚未开始"(D2 暂停停滞断言失败真因,
/// 2026-09-07 三轮复现:卡状态一直 active,暂停从未生效)。
enum PlaceholderAction {
    /// 置占位取消标志(快速取消路径:引擎等待循环即时退出+清理,释放并发槽)
    FlagCancel,
    /// 拒绝(任务尚未开始)
    RejectNotStarted,
    /// 落到下方 T16 sender_jobs/push_control 路径
    Fallthrough,
}

fn placeholder_action(started: bool, action: &str) -> PlaceholderAction {
    match (started, action) {
        (_, "cancel") => PlaceholderAction::FlagCancel,
        (false, _) => PlaceholderAction::RejectNotStarted,
        (true, _) => PlaceholderAction::Fallthrough,
    }
}

/// 传输任务控制
#[tauri::command]
pub async fn transfer_action(
    state: State<'_, AppState>,
    job_id: String,
    action: String,
) -> Result<(), String> {
    let card_id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    if !state.card_exists(card_id).await {
        return Err(format!("未找到任务: {}", card_id));
    }
    // 终审必修:前端持 card_id(0x4000 高位段),引擎侧三表(sender_jobs/
    // push_control/control_task)按 engine job 注册——统一翻译,无映射回退原值。
    let job_id = resolve_engine_id(state.inner(), card_id).await;

    // v0.11.x BUG01:占位任务分支——对端离线等 OfferResp 期间,真实注册表
    // 尚无此 job;按占位取消信号处理(决策见 placeholder_action)。
    if let Some(flag) = state.placeholder_cancels.lock().await.get(&card_id).cloned() {
        let started = state.card_dto(card_id).await
            .map(|d| d.state != "pending").unwrap_or(false);
        match placeholder_action(started, &action) {
            PlaceholderAction::FlagCancel => {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
                return Ok(());
            }
            PlaceholderAction::RejectNotStarted => {
                return Err("任务尚未开始(等待对端响应),仅支持取消".to_string());
            }
            PlaceholderAction::Fallthrough => {}
        }
    }

    // Task 8:父卡整批控制——children 非空的批次卡,操作传播到全部活动子任务
    // (子 engine job 逐个走既有控制面),父卡自身走状态机。
    // 修复轮 P4①:active_child_engine_ids 为空但仍可能有活动子项(空 job_id
    // 的小文件批流/混合批次大文件全终态)——只要仍有活动子项就进整批分支,
    // 并把控制打到共享 offer job(小文件批流的载体)。
    // (child_ids 本就是 engine job,offer_job 同;card 状态机用 card_id)
    let child_ids = state.active_child_engine_ids(job_id).await;
    let has_active_children = state.has_active_children(job_id).await;
    let offer_job = state.batch_offer_job_of(job_id).await;
    if has_active_children || !child_ids.is_empty() {
        let mut targets: Vec<u64> = child_ids.clone();
        if let Some(oid) = offer_job {
            if !targets.contains(&oid) {
                targets.push(oid);
            }
        }
        for cid in &targets {
            if let Some(sender_state) = state.sender_jobs.read().await.get(cid).cloned() {
                match action.as_str() {
                    "pause" => sender_state.paused.store(true, std::sync::atomic::Ordering::Relaxed),
                    "resume" => sender_state.paused.store(false, std::sync::atomic::Ordering::Relaxed),
                    "cancel" => sender_state.cancelled.store(true, std::sync::atomic::Ordering::Relaxed),
                    _ => {}
                }
            }
            if let Some(pc) = localtrans_core::transfer::engine::get_push_control(*cid) {
                match action.as_str() {
                    "pause" => pc.paused.store(true, std::sync::atomic::Ordering::Relaxed),
                    "resume" => pc.paused.store(false, std::sync::atomic::Ordering::Relaxed),
                    "cancel" => pc.cancelled.store(true, std::sync::atomic::Ordering::Relaxed),
                    _ => {}
                }
            }
        }
        let _ = match action.as_str() {
            "cancel" => Some(state.card_apply(card_id, crate::transfer_state::CardEvent::Failed {
                reason: Some("已取消".into()),
            }).await),
            "pause" => Some(state.card_apply(card_id, crate::transfer_state::CardEvent::Paused).await),
            "resume" => Some(state.card_apply(card_id, crate::transfer_state::CardEvent::Resumed).await),
            _ => None,
        };
        return Ok(());
    }

    // T16: 先检查 sender_jobs (source-pull 任务)
    let sender_job = state.sender_jobs.read().await.get(&job_id).cloned();
    // 修复轮 1 I1：pending 期任务尚未开始，pause 拒绝且不置引擎标志
    // （此前引擎 paused 标志已置但状态机拒绝 Paused 迁移，卡在"假暂停"）
    let card_pending = state.card_dto(card_id).await
        .map(|d| d.state == "pending").unwrap_or(false);
    if card_pending && action == "pause" {
        return Err("任务尚未开始，无法暂停".to_string());
    }
    if let Some(sender_state) = sender_job {
        match action.as_str() {
            "pause" => {
                sender_state.paused.store(true, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Paused).await;
            }
            "resume" => {
                sender_state.paused.store(false, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Resumed).await;
            }
            "cancel" => {
                sender_state.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Failed {
                    reason: Some("已取消".into()),
                }).await;
            }
            _ => return Err(format!("无效操作: {}", action)),
        }
        return Ok(());
    }

    // T16: 检查推送任务控制(source-push 任务)
    if let Some(push_control) = localtrans_core::transfer::engine::get_push_control(job_id) {
        match action.as_str() {
            "pause" => {
                push_control.paused.store(true, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Paused).await;
            }
            "resume" => {
                push_control.paused.store(false, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Resumed).await;
            }
            "cancel" => {
                push_control.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
                state.card_apply(card_id, crate::transfer_state::CardEvent::Failed {
                    reason: Some("已取消".into()),
                }).await;
            }
            _ => return Err(format!("无效操作: {}", action)),
        }
        return Ok(());
    }

    // T16: 回退到原有的 control_task 路径(处理拉取任务)
    let ctl = match action.as_str() {
        "pause" => transfer::TaskControl::Pause,
        "resume" => transfer::TaskControl::Resume,
        "cancel" => transfer::TaskControl::Cancel,
        _ => return Err(format!("无效操作: {}", action)),
    };

    // R10: 拉取任务的控制走本地注册表（批间消化），不经网络往返；
    // 推送任务未注册（Phase 1 不可控）或任务已结束时 control_task 返回 false
    // （含应用重启后恢复的历史任务）。
    if !transfer::control_task(job_id, ctl) {
        return Err("任务不在运行（可能已结束或随应用重启中断）——下载任务可通过传输页顶部横幅续传".to_string());
    }

    // v0.6.x:取消是跨端事件——通知数据源清理 sender 任务(其 source 行落终态,
    // 不再永久 active)。尽力而为:连接已断时发送失败,对端另有 30s 连接关闭
    // 兜底;任务先一步完成时对端幂等忽略。仅拉取分支发(本机是 source 的取消
    // 由 sender_jobs/push_control 分支提前 return,对端行由 JobFailed/机制 B 覆盖)。
    if action == "cancel" {
        if let Some(dto) = state.card_dto(card_id).await {
            if let Some(fp) = peer_fingerprint_of(&dto) {
                let _ = state.sm.send_ctrl(&fp, protocol::ControlMsg::TransferCtl {
                    job_id,
                    action: protocol::TransferAction::Cancel,
                }).await;
            }
        }
    }

    // 更新本地状态（走状态机单写者）
    match action.as_str() {
        "pause" => state.card_apply(card_id, crate::transfer_state::CardEvent::Paused).await,
        "cancel" => state.card_apply(card_id, crate::transfer_state::CardEvent::Failed {
            reason: Some("已取消".into()),
        }).await,
        "resume" => state.card_apply(card_id, crate::transfer_state::CardEvent::Resumed).await,
        _ => {}
    }

    Ok(())
}

/// Task 8:子项单独重试。失败子项按前端传入的重发参数(fp + 单文件
/// (path, rel_dir) 对)按 push_files_rel 单条重发;新引擎任务经
/// pending_children 挂回父卡,子项按名替换原失败项。
/// 最终签名:
/// `retry_child(state, parent_card_id: String, child_job_id: String,
///              fp: String, rel_dir: String, path: String) -> Result<u64, String>`
/// (path=本地绝对路径, rel_dir=对端落盘相对目录, 与 push_files_rel items 对齐;
///  child_job_id=失败子项的 engine job hex, 前端从 ChildDto 取)
#[tauri::command]
pub async fn retry_child(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    parent_card_id: String,
    child_job_id: String,
    fp: String,
    rel_dir: String,
    path: String,
) -> Result<u64, String> {
    let parent_id = u64::from_str_radix(parent_card_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 parent_card_id: {}", e))?;
    let card = state.card_get(parent_id).await
        .ok_or_else(|| format!("未找到父任务: {}", parent_card_id))?;

    // 只允许重试失败子项,且父卡必须非终态吸收外的可操作状态
    let _ = child_job_id; // 旧子项由挂接路径按名替换,无需在此定位
    if matches!(card.dto.state.as_str(), "done") {
        return Err("父任务已完成,无需重试".to_string());
    }

    let fp_bytes = hex::decode(&fp).map_err(|e| format!("无效指纹: {}", e))?;
    if fp_bytes.len() != 32 {
        return Err("无效指纹长度".to_string());
    }
    let mut fp_arr = [0u8; 32];
    fp_arr.copy_from_slice(&fp_bytes);

    if path.is_empty() {
        return Err("重试缺少本地文件路径".to_string());
    }

    let items: Vec<(std::path::PathBuf, String)> = vec![(std::path::PathBuf::from(path), rel_dir)];

    // 复用结构化推送编排:card_id=父卡(不再建新卡),batch=false(单文件
    // 走占位式状态机)——但父卡已是状态机卡,batch=false 时 Started 会
    // bind engine 到父卡,替换父卡语义。裁定:批次父卡恒走 batch 路径,
    // 事件落子项(children 按名替换失败项),新 engine job 经 pending_children 挂回。
    let batch = true;
    // 修复轮 P3:retry 用独立挂接键——不顶掉同 peer 活动批次的登记。
    state.pending_children.lock().await.insert(retry_peer_key(&fp_arr), parent_id);

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);
    let sm = state.sm.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();
    let peer_hex_for_task = fp.clone();

    tokio::spawn(async move {
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            st.acquire_slot(&peer_hex_for_task),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("子项重试排队超时");
                return;
            }
        };

        let mut placeholder_alive = true;
        let mut progress_stopped = false;
        let push_fut = localtrans_core::transfer::push_files_rel(
            &sm, &fp_arr, items, &st.sender_jobs, progress_tx,
        );
        tokio::pin!(push_fut);
        loop {
            tokio::select! {
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_push_progress_event_mode(
                            &st, &app, parent_id, &mut placeholder_alive, &peer_hex_for_task, ev, batch,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut push_fut => {
                    if let Err(e) = res {
                        tracing::warn!("子项重试失败: {}", e);
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "error",
                            "text": format!("子项重试失败: {}", e)
                        }));
                    }
                    while let Some(ev) = progress_rx.recv().await {
                        handle_push_progress_event_mode(
                            &st, &app, parent_id, &mut placeholder_alive, &peer_hex_for_task, ev, batch,
                        ).await;
                    }
                    break;
                }
            }
        }
    });

    Ok(parent_id)
}

/// 从任务行取对端指纹(取消通知的寻址)。无效/空 peer 返回 None。
fn peer_fingerprint_of(dto: &TransferDto) -> Option<[u8; 32]> {
    let bytes = hex::decode(&dto.peer).ok()?;
    if bytes.len() != 32 {
        return None;
    }
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&bytes);
    Some(fp)
}

/// 队列命令

/// 列出传输任务（进行中/等待/暂停在前，完成/失败历史在后）
#[tauri::command]
pub async fn list_transfers(state: State<'_, AppState>) -> Result<Vec<TransferDto>, String> {
    let mut jobs: Vec<TransferDto> = state.snapshot_dtos().await;
    let rank = |d: &TransferDto| match d.state.as_str() {
        "pending" | "active" | "paused" => 0u8,
        _ => 1,
    };
    jobs.sort_by_key(|d| (rank(d), d.job_id));
    Ok(jobs)
}

/// 列出可恢复的待处理任务
#[tauri::command]
pub async fn pending_resume_jobs(state: State<'_, AppState>) -> Result<Vec<(u64, String)>, String> {
    let download_dir = state.config.read().await.download_dir.clone();
    let jobs = localtrans_core::transfer::pending_jobs(&download_dir);

    let result: Vec<(u64, String)> = jobs.into_iter()
        .map(|(job_id, manifest)| {
            let name = manifest.file_name.clone();
            (job_id, name)
        })
        .collect();

    Ok(result)
}

/// 修复轮 1:job_id→engine job_id 翻译辅助。有映射(card_id 绑定了引擎)
/// 返回 engine_id;无映射回退原值——兼容旧前端直传 engine job_id 的过渡期。
pub(crate) async fn resolve_engine_id(state: &AppState, job_id: u64) -> u64 {
    state.engine_id_of(job_id).await.unwrap_or(job_id)
}

/// 恢复待处理任务
#[tauri::command]
pub async fn resume_pending(
    state: State<'_, AppState>,
    app_handle: AppHandle,
    job_id: String,
) -> Result<(), String> {
    let raw_id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    // 修复轮 1:job_id 可能是 card_id(0x4000 高位段)或旧前端直传 engine job_id
    // (小整数)。先翻译,无映射回退原值(兼容过渡期)。
    let job_id = resolve_engine_id(state.inner(), raw_id).await;
    let download_dir = state.config.read().await.download_dir.clone();
    let jobs = localtrans_core::transfer::pending_jobs(&download_dir);

    let manifest = jobs.iter()
        .find(|(id, _)| *id == job_id)
        .map(|(_, m)| m.clone());

    // 装机修复 BUG-B 第三层:parts 目录按内容复用(T15 断点匹配:文件名+大小+
    // 哈希命中即沿用旧目录),目录名是**首次下载**的 job_id——重新下载的卡绑定的
    // engine_id(MetaResp 新分配)与目录名不同,ID 匹配必然落空。兜底:按卡片
    // 内容(文件名+对端指纹)匹配 manifest。文件夹聚合行走下面的 source_path 分支。
    let manifest = if manifest.is_none() {
        let card = state.transfers.lock().await.get(&raw_id)
            .map(|c| (c.dto.name.clone(), c.dto.peer.clone()));
        match card {
            Some((name, peer_hex)) => jobs.iter()
                .find(|(_, m)| m.file_name == name && m.peer.as_deref() == Some(peer_hex.as_str()))
                .map(|(_, m)| m.clone()),
            None => None,
        }
    } else { manifest };

    // v0.2.8 文件夹任务分支：聚合行不在单文件 manifest 表里（每个内部文件
    // 一个 parts 目录），恢复参数在聚合行自己的 source_path（"share_id|dir"）。
    // 恢复 = 重新枚举编排：已 finalize 的文件无 parts 自然重传校验跳过，
    // 未完文件命中 parts 续传。
    // 装机修复 BUG-B 第三层:卡片表按 card_id 键——这里曾用翻译后的 engine_id
    // 查,重下场景(engine 绑定是新 0x8000 段 ID)双查空。先按 card_id 查。
    if manifest.is_none() {
        let dto = match state.card_dto(raw_id).await {
            Some(d) => d,
            None => state.card_dto(job_id).await
                .ok_or_else(|| format!("未找到任务: {:016x}", raw_id))?,
        };
        if let Some(src) = dto.source_path.as_ref() {
            if let Some((share_id, dir_path)) = src.split_once('|') {
                let fp_hex = dto.peer.clone();
                let fp_bytes = hex::decode(&fp_hex).map_err(|e| format!("无效指纹: {}", e))?;
                let mut fp = [0u8; 32];
                fp.copy_from_slice(&fp_bytes);

                let config = state.config.read().await.clone();
                let sm = state.sm.clone();
                let st = state.inner().clone();
                let app = app_handle.clone();
                let share_id = share_id.to_string();
                let dir_path = dir_path.to_string();

                tokio::spawn(async move {
                    // 重连对端（同单文件分支,S1: 带指纹钉扎）
                    if sm.session(&fp).await.is_none() {
                        let addr = st.devices.lock().await.iter()
                            .find(|d| d.fingerprint == fp)
                            .map(|d| d.addr);
                        match addr {
                            Some(a) => {
                                if let Err(e) = sm.connect_pinned(a, fp).await {
                                    let _ = app.emit("toast", serde_json::json!({
                                        "level": "error",
                                        "text": format!("恢复任务失败: 无法连接对端: {}", e)
                                    }));
                                    return;
                                }
                            }
                            None => {
                                let _ = app.emit("toast", serde_json::json!({
                                    "level": "error",
                                    "text": "恢复任务失败: 对端不在线"
                                }));
                                return;
                            }
                        }
                    }

                    // Task 7 修复轮 1:retry 入口与主入口一致,排队 60s 超时明确失败
                    let (_permit, _guard) = match tokio::time::timeout(
                        std::time::Duration::from_secs(60),
                        st.acquire_slot(&fp_hex),
                    ).await {
                        Ok(slot) => slot,
                        Err(_) => {
                            tracing::warn!("文件夹续传排队超时");
                            st.card_mutate(job_id, |d| d.queue_pos = None).await;
                            st.card_apply(job_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                            let _ = app.emit("toast", serde_json::json!({
                                "level": "error",
                                "text": "排队超时：前序传输任务似乎卡住了"
                            }));
                            return;
                        }
                    };
                    tracing::info!("开始文件夹续传（对端 {}）", fp_hex);
                    // 复活聚合行（interrupted→paused→Resumed→active;状态机不允许终态直跳）
                    st.card_mutate(job_id, |d| {
                        d.done = 0;
                        d.speed_bps = 0;
                        d.finished_at_ms = None;
                        if d.state == "interrupted" {
                            d.state = "paused".into();
                        }
                    }).await;
                    st.card_apply(job_id, crate::transfer_state::CardEvent::Resumed).await;
                    run_dir_pull_task(&st, &app, &sm, &fp, &share_id, &dir_path, &config, job_id).await;
                });
                return Ok(());
            }
        }
        return Err(format!("未找到任务: {}", job_id));
    }
    let manifest = manifest.unwrap();

    let peer_bytes = hex::decode(manifest.peer.unwrap_or_default()).map_err(|e| format!("无效对端指纹: {}", e))?;
    let mut peer = [0u8; 32];
    peer.copy_from_slice(&peer_bytes);

    let share_id = manifest.share_id.clone().unwrap_or_default();
    let rel = manifest.rel.clone().unwrap_or_default();

    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::channel::<localtrans_core::transfer::ProgressEvent>(64);

    let config = state.config.read().await.clone();
    let sm = state.sm.clone();
    let reg = state.reg.clone();
    let fp_for_task = peer;
    let sid = share_id.clone();
    let rel_path = rel.clone();
    let cfg_for_task = config.clone();
    let st = state.inner().clone();
    let app = app_handle.clone();
    // 装机修复 BUG-B:job_id 此刻已是 engine_id(拉取任务=对端分配的 0x8000 段
    // source id,parts 目录名),事件泵需要的是 card_id(卡片表键)。保留翻译前
    // 的原值;卡片不存在(强杀重启后恢复的孤儿)由 card_apply warn+丢弃兜底。
    let orig_id = raw_id;

    tokio::spawn(async move {
        // 对端会话可能已断（应用重启后续传场景）——按指纹从设备表找地址重连(S1: 带指纹钉扎)
        if sm.session(&fp_for_task).await.is_none() {
            let addr = st.devices.lock().await.iter()
                .find(|d| d.fingerprint == fp_for_task)
                .map(|d| d.addr);
            match addr {
                Some(a) => {
                    if let Err(e) = sm.connect_pinned(a, fp_for_task).await {
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "error",
                            "text": format!("恢复任务失败: 无法连接对端: {}", e)
                        }));
                        return;
                    }
                }
                None => {
                    let _ = app.emit("toast", serde_json::json!({
                        "level": "error",
                        "text": "恢复任务失败: 对端不在线"
                    }));
                    return;
                }
            }
        }

        // Task 7:与下载/推送共用闸门+对端锁;修复轮 1:60s 排队超时同主入口
        let (_permit, _guard) = match tokio::time::timeout(
            std::time::Duration::from_secs(60),
            st.acquire_slot(&hex::encode(fp_for_task)),
        ).await {
            Ok(slot) => slot,
            Err(_) => {
                tracing::warn!("续传排队超时");
                st.card_mutate(orig_id, |d| d.queue_pos = None).await;
                st.card_apply(orig_id, crate::transfer_state::CardEvent::QueueTimeout).await;
                let _ = app.emit("toast", serde_json::json!({
                    "level": "error",
                    "text": "排队超时：前序传输任务似乎卡住了"
                }));
                return;
            }
        };
        tracing::info!(
            "开始续传: {}（对端 {}，共享区 {}）",
            rel_path,
            hex::encode(fp_for_task),
            sid
        );
        // v0.2.2 根因修复：进度通道与传输引擎并发消费（同 start_download）。
        // 续传原任务行本身充当占位（orig_alive）
        let peer_hex = hex::encode(fp_for_task);
        let pull_fut = localtrans_core::transfer::start_pull(
            &sm,
            &reg,
            &fp_for_task,
            &sid,
            &rel_path,
            &cfg_for_task,
            progress_tx,
        );
        tokio::pin!(pull_fut);

        let mut orig_alive = true; // Original job row acts as placeholder
        let mut progress_stopped = false;
        loop {
            tokio::select! {
                biased;
                ev = progress_rx.recv(), if !progress_stopped => {
                    match ev {
                        Some(ev) => handle_pull_progress_event(
                            &st, &app, orig_id, &mut orig_alive, &peer_hex, ev,
                        ).await,
                        None => progress_stopped = true,
                    }
                }
                res = &mut pull_fut => {
                    if let Err(e) = res {
                        tracing::warn!("恢复任务失败: {}", e);
                        let _ = app.emit("toast", serde_json::json!({
                            "level": "error",
                            "text": format!("恢复任务失败: {}", e)
                        }));
                    }

                    while let Some(ev) = progress_rx.recv().await {
                        handle_pull_progress_event(
                            &st, &app, orig_id, &mut orig_alive, &peer_hex, ev,
                        ).await;
                    }
                    break;
                }
            }
        }
    });

    Ok(())
}

/// ===== Task 6: 历史记录命令(磁盘 manifest 全量) =====

/// 历史记录条目 DTO(display_name 只进 DTO 不进日志,隐私红线)
#[derive(serde::Serialize, Clone, Debug)]
pub struct DiskJobDto {
    #[serde(with = "localtrans_core::serde_compat::u64_hex_string")]
    pub job_id: u64,
    pub display_name: String,
    pub total: u64,
    pub done: u64,
    pub state: String,
    pub direction: String,
    pub peer_hex: String,
    pub created_at_ms: Option<i64>,
    pub removed_from_view: bool,
}

/// 修复轮 1:表内卡 → 历史记录条目(removed 标记透传)
pub fn disk_job_dto_from_card(card: &crate::TransferCardSerde) -> DiskJobDto {
    let d = &card.dto;
    DiskJobDto {
        job_id: d.job_id,
        display_name: d.name.clone(),
        total: d.total,
        done: d.done,
        state: d.state.clone(),
        direction: d.direction.clone(),
        peer_hex: d.peer.clone(),
        created_at_ms: d.started_at_ms,
        removed_from_view: card.removed,
    }
}

/// 修复轮 1:历史记录入口合并(纯函数,便于测试)。
/// 磁盘扫盘条目优先;表内"非 open 且磁盘无 parts"的卡补齐(parts 被 gc
/// 的全真孤儿历史仍可见);按 job_id 去重。
pub fn merge_disk_jobs(mut disk: Vec<DiskJobDto>, cards: &[crate::TransferCardSerde]) -> Vec<DiskJobDto> {
    for c in cards {
        let is_open = !matches!(c.dto.state.as_str(), "done" | "failed" | "interrupted");
        if is_open {
            continue;
        }
        if !disk.iter().any(|j| j.job_id == c.dto.job_id) {
            disk.push(disk_job_dto_from_card(c));
        }
    }
    disk
}

/// 历史记录入口:全部磁盘 manifest(含视图已移除的)。manifest.meta 有
/// display_name 用之,无则回退 file_name。修复轮 1:合并表内"磁盘目录
/// 已不存在且非 open"的卡。
#[tauri::command]
pub async fn list_disk_jobs(state: State<'_, AppState>) -> Result<Vec<DiskJobDto>, String> {
    let download_dir = state.config.read().await.download_dir.clone();
    let in_table = state.transfers.lock().await.values().cloned().collect::<Vec<_>>();
    let jobs = tokio::task::spawn_blocking(move || {
        let disk = localtrans_core::transfer::orphan_jobs(&download_dir)
            .into_iter()
            .map(|oj| {
                let m = &oj.manifest;
                let meta = m.meta();
                DiskJobDto {
                    job_id: oj.job_id,
                    display_name: meta.map(|mt| mt.display_name.clone())
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| m.file_name.clone()),
                    total: m.total_size,
                    done: m.received.iter().filter(|&&r| r).count() as u64,
                    state: if m.missing_chunks().is_empty() { "failed".into() } else { "interrupted".into() },
                    direction: meta.map(|mt| mt.direction.clone()).unwrap_or_else(|| "pull".into()),
                    peer_hex: meta.map(|mt| mt.peer_hex.clone()).unwrap_or_default(),
                    created_at_ms: meta.map(|mt| mt.created_at_ms),
                    removed_from_view: false, // 纯磁盘视角,无视图信息
                }
            })
            .collect::<Vec<_>>();
        let cards: Vec<crate::TransferCardSerde> = in_table.iter()
            .map(|c| crate::TransferCardSerde { dto: c.dto.clone(), removed: c.removed })
            .collect();
        merge_disk_jobs(disk, &cards)
    }).await.map_err(|e| e.to_string())?;
    Ok(jobs)
}

/// 恢复视图:removed=false;卡不在表(孤儿未建卡/已被 gc)则按启动重建逻辑
/// 建卡入表。
#[tauri::command]
pub async fn restore_disk_job(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<(), String> {
    let eid = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let st = state.inner();

    // 已在表:只清 removed 标记
    if let Some(card_id) = st.engine_card_of(eid).await {
        let _ = st.card_mutate(card_id, |_| {}).await;
        // 直接改 removed(非事件路径)
        {
            let mut map = st.transfers.lock().await;
            if let Some(c) = map.get_mut(&card_id) { c.removed = false; }
        }
        st.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
        return Ok(());
    }

    // 不在表:按启动重建逻辑建卡(card_id=engine_id;meta 缺则读 manifest)
    let download_dir = st.config.read().await.download_dir.clone();
    let orphan = tokio::task::spawn_blocking(move || {
        localtrans_core::transfer::orphan_jobs(&download_dir)
            .into_iter().find(|oj| oj.job_id == eid)
    }).await.map_err(|e| e.to_string())?;

    match orphan {
        Some(oj) => {
            let dto = crate::orphan_card_dto(eid, &oj.manifest);
            let mut c = crate::transfer_state::TransferCard::new(dto);
            c.engine_id = Some(eid);
            c.removed = false;
            st.transfers.lock().await.entry(eid).or_insert(c);
            st.next_card_id.fetch_max(eid + 1, std::sync::atomic::Ordering::Relaxed);
            st.transfers_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
            Ok(())
        }
        None => Err(format!("磁盘上不存在该任务: {:016x}", eid)),
    }
}

/// 彻底删除磁盘任务:按 engine job_id 删 manifest+parts+表条目(用于孤儿)。
#[tauri::command]
pub async fn destroy_disk_job(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<(), String> {
    let eid = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    let st = state.inner();

    // 表里有卡:走既有 finalize_destroy(abort 看门狗+删表行+删 parts)
    if let Some(card_id) = st.engine_card_of(eid).await {
        st.finalize_destroy(card_id).await;
        return Ok(());
    }

    // 纯孤儿(未建卡):直接删 parts 目录(日志只打 job_id hex,不打路径)
    let download_dir = st.config.read().await.download_dir.clone();
    tokio::task::spawn_blocking(move || {
        std::fs::remove_dir_all(download_dir.join(format!(".localtrans-parts/{:016x}", eid)))
    }).await.map_err(|e| e.to_string())?
        .map_err(|e| format!("删除失败: {}", e))?;
    tracing::info!("destroy_disk_job 完成 job={:016x}", eid);
    Ok(())
}

/// 设置命令

/// A4 隐私修复:PSK 回显掩码——≥8 字符保留尾 4 字符,否则全掩。
/// 按 char 计数与截取(validate 不限字符集,中文 PSK 时字节切片会 panic)。
fn mask_psk(psk: &str) -> String {
    if psk.is_empty() {
        return String::new();
    }
    if psk.chars().count() >= 8 {
        let tail: String = psk.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
        format!("****{}", tail)
    } else {
        "****".to_string()
    }
}

/// Config → ConfigDto 转换（纯函数，便于测试）
fn config_to_dto(config: &localtrans_core::store::Config, reg: &share::ShareRegistry) -> ConfigDto {
    let shares: Vec<ShareDefDto> = reg.list().into_iter()
        .map(|s| ShareDefDto {
            id: s.id,
            alias: s.alias,
            path: s.path.to_string_lossy().to_string(),
        })
        .collect();

    ConfigDto {
        device_name: config.device_name.clone(),
        download_dir: config.download_dir.to_string_lossy().to_string(),
        hidden: config.hidden,
        quic_port: config.quic_port,
        discovery_port: config.discovery_port,
        consent_timeout_secs: config.consent_timeout_secs,
        offer_timeout_secs: config.offer_timeout_secs,
        max_active_transfers: config.max_active_transfers,
        shares,
        relay_enabled: config.relay_enabled,
        relay_server: config.relay_server.clone(),
        // A4 隐私修复:relay_psk 不回显明文——掩码(≥8 位留尾 4 字符便于辨认)
        relay_psk: mask_psk(&config.relay_psk),
    }
}

/// 获取设置
#[tauri::command]
pub async fn get_settings(state: State<'_, AppState>) -> Result<ConfigDto, String> {
    let config = state.config.read().await;
    Ok(config_to_dto(&config, &state.reg))
}

/// 保存设置
#[tauri::command]
pub async fn save_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    dto: ConfigDto,
) -> Result<(), String> {
    let mut config = state.config.write().await;

    // 设备名变更:即时生效(发现服务更新出站名并立即广播一轮 Presence,
    // 对端秒级看到新名,不再需要重启——UI 的"重启后生效"提示同步移除)
    let name_changed = config.device_name != dto.device_name;
    config.device_name = dto.device_name;
    config.download_dir = PathBuf::from(&dto.download_dir);
    config.hidden = dto.hidden;
    config.consent_timeout_secs = dto.consent_timeout_secs.clamp(15, 600);
    config.offer_timeout_secs = dto.offer_timeout_secs.clamp(15, 600);
    // Task 7:并发闸门容量钳制 1-8(运行时改动重启后生效——active_gate 启动构建)
    config.max_active_transfers = dto.max_active_transfers.clamp(1, 8);

    // 端口变更需重启生效
    if config.quic_port != dto.quic_port || config.discovery_port != dto.discovery_port {
        tracing::info!("端口变更需重启生效: {} -> {}, {} -> {}",
            config.quic_port, dto.quic_port,
            config.discovery_port, dto.discovery_port);
    }

    config.quic_port = dto.quic_port;
    config.discovery_port = dto.discovery_port;

    // 注意：shares 通过 add_share/remove_share 单独管理，此处不处理

    localtrans_core::store::save_config(&state.dir, &config).map_err(|e| e.to_string())?;

    // 改名即时广播(发现层)+ 中继在线时下轮 Ping 前重注册带新名
    if name_changed {
        let _ = state.discovery.cmd.send(discovery::DiscoveryCmd::SetName(config.device_name.clone())).await;
        tracing::info!("设备名已变更并即时广播: {}", config.device_name);

        // 中继在线时重启连接,让名册立即换新名(Register 仅在连接建立时上报)
        {
            let relay_guard = state.relay.lock().await;
            if relay_guard.is_some() {
                drop(relay_guard);
                tracing::info!("检测到改名且中继在线,重启中继连接以刷新名册名称");
                if let Err(e) = restart_relay(app, &state).await {
                    tracing::warn!("中继重连失败(将按原配置在重启后恢复): {}", e);
                }
            }
        }
    }

    Ok(())
}

/// 添加共享区
#[tauri::command]
pub async fn add_share(
    state: State<'_, AppState>,
    alias: String,
    path: String,
) -> Result<ShareDefDto, String> {
    // 生成短随机 hex ID
    let id = hex::encode(rand::random::<[u8; 8]>());

    let def = localtrans_core::store::ShareDef {
        id: id.clone(),
        alias,
        path: PathBuf::from(path),
    };

    state.reg.add(def.clone());

    // 持久化到配置
    let mut config = state.config.write().await;
    config.shares.push(def.clone());
    localtrans_core::store::save_config(&state.dir, &config).map_err(|e| e.to_string())?;

    Ok(ShareDefDto {
        id,
        alias: def.alias,
        path: def.path.to_string_lossy().to_string(),
    })
}

/// 移除共享区
#[tauri::command]
pub async fn remove_share(state: State<'_, AppState>, id: String) -> Result<bool, String> {
    let removed = state.reg.remove(&id);

    if removed {
        let mut config = state.config.write().await;
        config.shares.retain(|s| s.id != id);
        localtrans_core::store::save_config(&state.dir, &config).map_err(|e| e.to_string())?;
    }

    Ok(removed)
}

/// 列出信任对等方
#[tauri::command]
pub async fn list_trusted(state: State<'_, AppState>) -> Result<Vec<TrustedPeerDto>, String> {
    let trust = state.trust.lock().await;

    let result: Vec<TrustedPeerDto> = trust.all_peers()
        .into_iter()
        .map(|p| TrustedPeerDto {
            fingerprint: hex::encode(&p.fingerprint),
            name: p.name,
            alias: p.alias,
            paired_at: p.paired_at,
            browse: p.perms.browse,
            download: p.perms.download,
            push: match p.perms.push {
                identity::PushPolicy::Ask => "ask".into(),
                identity::PushPolicy::Auto => "auto".into(),
                identity::PushPolicy::Deny => "deny".into(),
            },
        })
        .collect();

    Ok(result)
}

/// 设置信任对端本地别名(空串=清除别名,回退显示对方广播名)
#[tauri::command]
pub async fn set_alias(
    state: State<'_, AppState>,
    fingerprint: String,
    alias: String,
) -> Result<bool, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    let trimmed = alias.trim().to_string();
    let mut trust = state.trust.lock().await;
    let ok = trust.set_alias(&fp, trimmed.clone());
    if ok {
        trust.save().map_err(|e| e.to_string())?;
    }
    Ok(ok)
}

/// 设置权限
#[tauri::command]
pub async fn set_perms(
    state: State<'_, AppState>,
    fingerprint: String,
    browse: bool,
    download: bool,
    push: String,
) -> Result<bool, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);

    let push_policy = match push.as_str() {
        "ask" => identity::PushPolicy::Ask,
        "auto" => identity::PushPolicy::Auto,
        "deny" => identity::PushPolicy::Deny,
        _ => return Err(format!("无效推送策略: {}", push)),
    };

    let mut trust = state.trust.lock().await;
    let ok = trust.set_perms(&fp, identity::Perms { browse, download, push: push_policy });
    if ok {
        trust.save().map_err(|e| e.to_string())?;
    }
    Ok(ok)
}

/// 移除信任对等方
#[tauri::command]
pub async fn remove_trusted(state: State<'_, AppState>, fingerprint: String) -> Result<bool, String> {
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    let mut fp = [0u8; 32];
    fp.copy_from_slice(&fp_bytes);
    remove_trusted_inner(&state, &fp).await
}

/// remove_trusted 的可测内核（命令壳只做入参解析）。
/// 顺序要害：**先清连接记忆再断会话**——sm.disconnect 触发的 SessionDown
/// 事件由壳层事件泵异步消费,若记忆未先清,重连编排会把它当断线故障排程。
pub(crate) async fn remove_trusted_inner(
    st: &AppState,
    fp: &identity::Fingerprint,
) -> Result<bool, String> {
    let removed = st.trust.lock().await.remove(fp);

    if removed {
        // M3a FR5:移除信任同步清连接记忆(先清再断,防 SessionDown 触发自动重连)
        {
            let mut mem = st.connect_memory.lock().await;
            if mem.remove(fp) {
                if let Err(e) = mem.save() {
                    tracing::warn!("连接记忆清除落盘失败: {}", e);
                }
            }
        }
        // M3a FR6:断会话**前**先告知对端"信任已移除"(TrustBroken 协议通知),
        // 对端收到即删本地信任条目+断连+toast(双盲对称重配)。会话不存在
        // (对端本就不在线)时 send_ctrl 报错,属正常路径;发送失败仅记日志,
        // 不阻塞本地移除。
        if let Err(e) = st.sm.send_ctrl(fp, localtrans_core::protocol::ControlMsg::TrustBroken).await {
            tracing::info!("TrustBroken 通知未送达(对端不在线/发送失败,继续本地移除): {}", e);
        }
        // P0-5: 移除信任即时生效——立即断开既有会话(否则对端连接续命到自然超时)
        st.sm.disconnect(fp).await;
        st.trust.lock().await.save().map_err(|e| e.to_string())?;
    }

    Ok(removed)
}

/// 中继命令

/// 按当前配置重启中继连接(改名刷新名册 / 配置变更后复用)
async fn restart_relay(app: tauri::AppHandle, state: &State<'_, AppState>) -> Result<(), String> {
    let (enabled, server, psk, device_name) = {
        let config = state.config.read().await;
        (config.relay_enabled, config.relay_server.clone(), config.relay_psk.clone(), config.device_name.clone())
    };
    if !enabled { return Ok(()); }

    // 关旧连接与事件任务
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
    *state.relay_roster.lock().await = Vec::new();

    // 复用 set_relay_config 的连接建立逻辑(它内部完成 validate/解析/spawn)
    set_relay_config(app, state.clone(), true, server, psk).await
}

/// 设置中继配置
#[tauri::command]
pub async fn set_relay_config(
    app: AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
    server: String,
    psk: String,
) -> Result<(), String> {
    // 0. A4 隐私修复:UI 传回掩码(**** 开头)时先还原为当前配置真值,
    //    再走 validate——否则掩码(<16 字符)会被"密钥过短"拒绝,
    //    用户不改 PSK 只改别的设置就保存不了。掩码回传 = 保留原值。
    let psk = {
        let config = state.config.read().await;
        if psk.starts_with("****") && !config.relay_psk.is_empty() {
            tracing::info!("relay_psk 为掩码回传,保留原值");
            config.relay_psk.clone()
        } else {
            psk
        }
    };

    // 参数验证(失败不落盘不连接,错误带具体原因)
    localtrans_core::relay::validate_relay_config(enabled, &server, &psk)?;

    // 1. 写配置并落盘
    {
        let mut config = state.config.write().await;
        config.relay_enabled = enabled;
        config.relay_server = server.clone();
        config.relay_psk = psk.clone();
        localtrans_core::store::save_config(&state.dir, &config).map_err(|e| e.to_string())?;
    }

    // 2. 启用/禁用中继连接
    if enabled {
        reconnect_relay(&app, &state, server, psk).await?;
    } else {
        // 禁用中继：关闭客户端、abort 事件任务并清空名册
        let mut relay_guard = state.relay.lock().await;
        if let Some(client) = relay_guard.as_ref() {
            client.shutdown().await;
            *relay_guard = None;
        }

        // Abort 事件桥任务
        if let Some(h) = state.relay_event_task.lock().await.take() {
            h.abort();
        }

        *state.relay_roster.lock().await = Vec::new();

        // 重发设备列表（仅本地发现的设备 + 信任表常驻条目）
        let devices = state.devices.lock().await.clone();
        let connected = state.connected_fps.lock().await.clone();
        let aliases = alias_map(&*state.trust.lock().await);
        let trusted = trusted_pairs(&*state.trust.lock().await);
        let merged = merge_devices(&devices, &[], &connected, &aliases, &trusted);
        let _ = app.emit("device-list", merged);
    }

    Ok(())
}

/// 按 config 重建中继连接(set_relay_config 与 set_hidden 隐身翻转共用)
pub async fn reconnect_relay(
    app: &AppHandle,
    state: &AppState,
    server: String,
    psk: String,
) -> Result<(), String> {
    // 解析服务器地址
    let server_addr: std::net::SocketAddr = server.parse()
        .map_err(|e| format!("无效服务器地址: {}", e))?;

    // 如果已有客户端，先关闭并 abort 旧事件任务
    {
        let mut relay_guard = state.relay.lock().await;
        if let Some(client) = relay_guard.as_ref() {
            client.shutdown().await;
            *relay_guard = None;
        }
    }

    // Abort 旧事件桥任务
    if let Some(h) = state.relay_event_task.lock().await.take() {
        h.abort();
    }
    // M-B3: abort 旧 connect 任务——防止上次连接尝试握手成功后写 relay 槽,
    // 与本次连接形成双连竞态(FFI 侧同构修复)
    if let Some(h) = state.relay_connect_task.lock().await.take() {
        h.abort();
    }

    // 清空中结名册
    *state.relay_roster.lock().await = Vec::new();

    // 创建新的中继连接(device_name 从配置取,Register 上报真实设备名)
    let (device_name, hidden) = {
        let config = state.config.read().await;
        (config.device_name.clone(), config.hidden)
    };
    let relay_config = localtrans_core::relay::client::RelayClientConfig {
        server_addr,
        psk,
        device_name,
        hidden,
    };
    let identity = state.identity.clone();
    let app_handle = app.clone();
    let relay_state = state.relay.clone();
    let roster_state = state.relay_roster.clone();
    let sm_for_events = state.sm.clone();
    let devices_for_events = state.devices.clone();
    let connected_for_events = state.connected_fps.clone();
    let trust_for_events = state.trust.clone();
    let event_task_state = state.relay_event_task.clone();

    let connect_task = tokio::spawn(async move {
        match localtrans_core::relay::client::RelayClient::connect(relay_config, identity).await {
            Ok((client, mut event_rx)) => {
                tracing::info!("中继连接成功");
                *relay_state.lock().await = Some(client.clone());
                // 事件桥内层 spawn 需要 relay 槽的句柄(accept_peer 用),外层
                // relay_state 已被上面这行用掉,这里提前 clone 一份
                let relay_state_clone = relay_state.clone();

                // 启动事件桥任务
                let app_handle_for_spawn = app_handle.clone();
                let event_task = tokio::spawn(async move {
                    while let Some(event) = event_rx.recv().await {
                        match event {
                            localtrans_core::relay::client::RelayEvent::RosterUpdated(roster) => {
                                *roster_state.lock().await = roster.clone();

                                let local_devices = devices_for_events.lock().await.clone();
                                let connected = connected_for_events.lock().await.clone();
                                let aliases = alias_map(&*trust_for_events.lock().await);
                                let trusted = trusted_pairs(&*trust_for_events.lock().await);
                                let merged = merge_devices(&local_devices, &roster, &connected, &aliases, &trusted);

                                let _ = app_handle_for_spawn.emit("device-list", merged);
                            }
                            localtrans_core::relay::client::RelayEvent::StatusChanged(status) => {
                                let _ = app_handle_for_spawn.emit("relay-state", serde_json::json!({
                                    "status": format!("{:?}", status),
                                }));
                            }
                            localtrans_core::relay::client::RelayEvent::PunchIncoming { from_fp, session_addr } => {
                                tracing::info!("收到中继 punch: from={}, session={}", hex::encode(from_fp), session_addr);
                                // 对端主动连我们:自动 accept 并 adopt 进会话管理器。
                                // 互信早已建立(名册只含注册设备;未配对对端在 mTLS/信任检查被拒)。
                                // v0.10.3 事件桥重构时此处被瘦身成纯日志,punch 无应答——
                                // 表现为"双方能互相发现,但发起连接对方无响应直接超时"。
                                let Some(relay) = relay_state_clone.lock().await.clone() else { continue };
                                let sm = sm_for_events.clone();
                                let app_for_punch = app_handle_for_spawn.clone();
                                tokio::spawn(async move {
                                    match relay.accept_peer(session_addr, from_fp).await {
                                        Ok(conn) => {
                                            if let Err(e) = sm.adopt_connection(conn).await {
                                                tracing::warn!("中继入站连接 adopt 失败: {}", e);
                                            } else {
                                                // 被连方确定性刷新 connected 徽章
                                                // (SessionUp 事件桥竞态兜底,同 connect 命令)
                                                let fp_hex = hex::encode(from_fp);
                                                let st = app_for_punch.state::<AppState>();
                                                st.connected_fps.lock().await.insert(fp_hex.clone());
                                                let devices = st.devices.lock().await.clone();
                                                let roster = st.relay_roster.lock().await.clone();
                                                let connected = st.connected_fps.lock().await.clone();
                                                let aliases = alias_map(&*st.trust.lock().await);
                                                let trusted = trusted_pairs(&*st.trust.lock().await);
                                                let dtos = merge_devices(&devices, &roster, &connected, &aliases, &trusted);
                                                let _ = app_for_punch.emit("device-list", dtos);
                                                let _ = app_for_punch.emit("connection-state", serde_json::json!({
                                                    "fingerprint": fp_hex,
                                                    "up": true
                                                }));
                                            }
                                        }
                                        Err(e) => tracing::warn!("中继入站 accept_peer 失败: {}", e),
                                    }
                                });
                            }
                        }
                    }
                });

                // 存储事件任务句柄
                *event_task_state.lock().await = Some(event_task);

                let _ = app_handle.emit("relay-state", serde_json::json!({
                    "status": "Registered",
                }));
            }
            Err(e) => {
                tracing::warn!("中继连接失败: {}", e);
                let _ = app_handle.emit("toast", serde_json::json!({
                    "level": "warning",
                    "text": format!("中继连接失败: {}", e),
                }));
            }
        }
    });
    // M-B3: 记录 connect 任务句柄,供下次重连时 abort
    *state.relay_connect_task.lock().await = Some(connect_task);

    Ok(())
}

/// 获取中继状态
#[tauri::command]
pub async fn relay_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let config = state.config.read().await;
    let relay_guard = state.relay.lock().await;
    let roster = state.relay_roster.lock().await;

    let connected = relay_guard.is_some();
    let device_count = roster.len();

    // 配置即时校验:失败时 UI 显示"配置错误"而非"连接中"
    let config_error = localtrans_core::relay::validate_relay_config(
        config.relay_enabled, &config.relay_server, &config.relay_psk,
    ).err().unwrap_or_default();

    Ok(serde_json::json!({
        "enabled": config.relay_enabled,
        "connected": connected,
        "server": config.relay_server,
        "devices": device_count,
        "error": config_error,
    }))
}

/// 系统命令

/// 添加防火墙规则
#[tauri::command]
pub async fn add_firewall_rule() -> Result<String, String> {
    let _ = crate::firewall::add_rule().await?;
    // 回读验证：UAC 被取消时 PowerShell 依旧报"成功"，只有规则真的在才算数
    let (exists, enabled) = crate::firewall::rule_status_async().await;
    Ok(if exists && enabled {
        "防火墙规则已生效（UDP 47600-47601 入站放行）".into()
    } else {
        "未检测到规则——UAC 弹窗可能被取消了，请重试并点\"是\"，或用管理员终端执行下方命令".into()
    })
}

/// 网络状态体检缓存（v0.11.0 M-C7）：体检含 netsh/reg 子进程调用,
/// 可达数百 ms。5 秒内复用上次结果,避免 UI 轮询打满子进程。
static NETWORK_STATUS_CACHE: std::sync::Mutex<Option<(i64, serde_json::Value)>> =
    std::sync::Mutex::new(None);

/// 网络状态体检：本机 IP（首选 + 全网卡列表）/ 公网出口 / 防火墙规则 / 三个配置文件开关
#[tauri::command]
pub async fn get_network_status(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    const CACHE_MS: i64 = 5_000;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    // M3a FR2:public_exit = 中继 RegisterAck 回报的本机公网出口 ip:port。
    // 仅"中继启用 且 当前已注册"时为 Some;内存读取零开销,不进 5s 缓存
    // (缓存只护 netsh/reg 子进程),注册/断线即时反映。
    let public_exit: Option<String> = if state.config.read().await.relay_enabled {
        match state.relay.lock().await.as_ref() {
            Some(client) => client.observed_addr().await,
            None => None,
        }
    } else {
        None
    };
    let cached = NETWORK_STATUS_CACHE
        .lock()
        .unwrap()
        .as_ref()
        .filter(|(ts, _)| now_ms - ts < CACHE_MS)
        .map(|(_, v)| v.clone());
    let mut value = match cached {
        Some(v) => v,
        None => {
            // v0.11.0 M-C7:netsh/reg/UdpSocket 全是阻塞调用——下放阻塞线程池
            let fresh = tokio::task::spawn_blocking(|| {
                let (rule_exists, rule_enabled) = crate::firewall::rule_status();
                let [fw_domain, fw_private, fw_public] = crate::firewall::profile_states();
                // M3a FR1:local_ip 保留为兼容别名(=首选接口地址);local_ips 为全网卡
                // 非环回 IPv4 列表 [{ip, if_name}](虚拟网卡已过滤,首选置首)
                let local_ips = localtrans_core::net_addrs::local_addresses();
                serde_json::json!({
                    "local_ip": crate::firewall::primary_local_ip(),
                    "local_ips": local_ips,
                    "rule_exists": rule_exists,
                    "rule_enabled": rule_enabled,
                    "fw_domain": fw_domain,
                    "fw_private": fw_private,
                    "fw_public": fw_public,
                })
            }).await.map_err(|e| format!("网络体检任务失败: {}", e))?;
            *NETWORK_STATUS_CACHE.lock().unwrap() = Some((now_ms, fresh.clone()));
            fresh
        }
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert("public_exit".into(), serde_json::json!(public_exit));
    }
    Ok(value)
}

/// 打开日志文件夹（排障时直接把当天日志发过来）
#[tauri::command]
pub async fn open_logs_dir(state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.dir.join("logs");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建日志目录失败: {}", e))?;
    tauri_plugin_opener::open_path(&dir, None::<&str>)
        .map_err(|e| format!("打开日志目录失败: {}", e))
}

/// 打开下载目录（Rust 侧执行,不经 JS capability 校验——
/// download_dir 是用户运行时可改的自定义路径,静态 scope 无法覆盖,
/// v0.11.0 曾因 opener scope 收紧导致自定义目录被拒。此处只开
/// config 里配置的那一个目录,不放宽任何 JS 权限面。）
#[tauri::command]
pub async fn open_download_dir(state: State<'_, AppState>) -> Result<(), String> {
    let dir = state.config.read().await.download_dir.clone();
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| format!("创建下载目录失败: {}", e))?;
    }
    tauri_plugin_opener::open_path(&dir, None::<&str>)
        .map_err(|e| format!("打开下载目录失败: {}", e))
}

/// 获取设备指纹信息
#[tauri::command]
pub async fn get_device_fingerprint(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let fp = state.identity.fingerprint();
    let short_code = state.identity.short_code();

    Ok(serde_json::json!({
        "fingerprint_hex": hex::encode(fp),
        "short_code": short_code,
        "name": state.config.read().await.device_name
    }))
}

/// M3b T4:通道表只读查询（探测选路观测面）。
/// 数据源 = AppState.channels（core::routing::ChannelTable，内存态不持久化，
/// 随会话生命周期）。消费方：test_api invoke 白名单（ReadOnly；routing-probe
/// 场景断言探测记录）与 M3c 通道 UI 预演。每设备每通道一条：
/// addr/rtt_ms/est_bps/近10次丢包率/经中继/是否当前通道/评分就绪/探测拉黑/
/// 记录年龄（秒；updated_at 是单调钟 Instant，只能给 elapsed，不给时刻）。
/// 表空返回 []（无会话 = 无记录，正常态）。
#[tauri::command]
pub async fn list_channels(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let mut out = Vec::new();
    for fp in state.channels.all_fps() {
        let current = state.channels.current(&fp);
        for r in state.channels.snapshot(&fp) {
            out.push(serde_json::json!({
                "fingerprint": hex::encode(fp),
                "addr": r.addr.to_string(),
                "via_relay": r.via_relay,
                "rtt_ms": r.rtt_ms,
                "est_bps": r.est_bps,
                "loss_rate": r.loss_rate(),
                "current": current == Some(r.addr),
                "score_ready": r.score_ready(),
                "probe_disabled": state.channels.probe_disabled(&fp, &r.addr),
                "age_secs": r.updated_at.elapsed().as_secs(),
            }));
        }
    }
    Ok(serde_json::Value::Array(out))
}

/// M3c T2:手动单对端快检(通道面板「重新探测」按钮)。
/// 复用调度器单对端快检(probe::probe_peer_now:当前通道 64KB 快检,
/// 掉 50% 升级全量),同步返回——前端按钮"探测中…"态以本命令返回为界。
/// test_api 白名单 ReadOnly(只写内存通道表,供 e2e 面板数据刷新编排)。
#[tauri::command]
pub async fn probe_now_peer(state: State<'_, AppState>, fingerprint: String) -> Result<(), String> {
    let fp = decode_fp(&fingerprint)?;
    crate::probe::probe_peer_now(&state, fp).await
}

/// M3c T3:强制走中继开关(per 设备持久化,config.json force_relay_map)。
/// 开启后该设备 connect 决策跳过评分直选中继路径;不进 test_api 白名单
/// (set_* 写配置排除原则不变),e2e 经 ⋮ 菜单 UI 点击驱动。
#[tauri::command]
pub async fn set_force_relay(
    state: State<'_, AppState>,
    fingerprint: String,
    enabled: bool,
) -> Result<bool, String> {
    // 指纹合法性:仅接受 64 hex(防 UI 侧手抖写入垃圾键)
    let fp_bytes = hex::decode(&fingerprint).map_err(|e| format!("无效指纹: {}", e))?;
    if fp_bytes.len() != 32 {
        return Err(format!("无效指纹长度: {}", fp_bytes.len()));
    }
    let mut config = state.config.write().await;
    if enabled {
        config.force_relay_map.insert(fingerprint, true);
    } else {
        config.force_relay_map.remove(&fingerprint);
    }
    let dir = &state.dir;
    localtrans_core::store::save_config(dir, &config).map_err(|e| e.to_string())?;
    Ok(enabled)
}

/// Task M1: 前端日志桥上报入口（ui/src/lib/logBridge.ts → invoke('ui_log')）。
/// **无条件注册，不受 test-api feature 门控**（spec 全局约定第 4 条）：
/// 正式版前端 console/错误日志经 tracing 落滚动文件同样有价值（FR-7）；
/// test-api 构建下额外经环形缓冲暴露给 /api/logs/tail（target="ui"）。
/// level 归一 info/warn/error（未知按 info），字段以 k=v 形式进 message 尾部，
/// 由环形缓冲 layer 统一拼接（spec §7.1.7）。
#[tauri::command]
pub fn ui_log(level: String, message: String, route: Option<String>) {
    match level.to_ascii_lowercase().as_str() {
        "warn" => tracing::warn!(target: "ui", level = %level, route = ?route, "{}", message),
        "error" => tracing::error!(target: "ui", level = %level, route = ?route, "{}", message),
        _ => tracing::info!(target: "ui", level = %level, route = ?route, "{}", message),
    }
}

/// Task M2: 前端 testBridge 就绪握手（ui/src/test-support/testBridge.ts 启动
/// 早期调用）。**无条件注册（generate_handler 内不 cfg，见全局约定第 4 条），
/// 函数体 feature 门控**：test-api 构建置位 BridgeRegistry.ready（/api/health
/// 的 bridgeReady 随之翻真）；非 test-api 构建返回 Err（正式版没有 HTTP 面，
/// 该握手无意义，前端静默忽略错误）。
#[tauri::command]
pub fn test_bridge_hello() -> Result<(), String> {
    #[cfg(feature = "test-api")]
    {
        crate::test_api::bridge::registry().mark_ready();
        return Ok(());
    }
    #[cfg(not(feature = "test-api"))]
    {
        Err("test_bridge_hello 仅在 --features test-api 构建下可用".to_string())
    }
}

/// Task M2: 前端 testBridge 回包入口（{id, ok, payload} → 唤醒对应 oneshot，
/// payload 为数据本体或 {code, message}）。注册策略同 test_bridge_hello。
#[tauri::command]
pub fn test_bridge_result(
    id: u64,
    ok: bool,
    payload: Option<serde_json::Value>,
) -> Result<(), String> {
    #[cfg(feature = "test-api")]
    {
        let hit = crate::test_api::bridge::registry()
            .complete(id, ok, payload.unwrap_or(serde_json::Value::Null));
        if !hit {
            tracing::warn!(
                target: "test_api",
                "test_bridge_result 未命中在途请求 id={id}（回包迟到或重复）"
            );
        }
        return Ok(());
    }
    #[cfg(not(feature = "test-api"))]
    {
        Err("test_bridge_result 仅在 --features test-api 构建下可用".to_string())
    }
}

/// 传输表清理命令（T12）

/// 清理已完成的传输任务
#[tauri::command]
pub async fn clear_completed_transfers(state: State<'_, AppState>) -> Result<usize, String> {
    clear_completed_inner(state.inner()).await
}

/// Task 5:逐条对终态卡走 view 级删除(spec §2.3 "清除已完成"=移除视图;
/// §1.3 逐条走状态机路径不绕过)
pub async fn clear_completed_inner(st: &AppState) -> Result<usize, String> {
    let ids: Vec<u64> = {
        st.snapshot_dtos().await.iter()
            .filter(|d| matches!(d.state.as_str(), "done" | "failed" | "interrupted"))
            .map(|d| d.job_id)
            .collect()
    };
    let mut n = 0;
    for id in ids {
        if remove_transfer_inner(st, id, "view").await? {
            n += 1;
        }
    }
    Ok(n)
}

/// 删除传输任务(两级删除:level="view" 移除视图 | level="destroy" 彻底删除)
/// breaking:delete_parts 参数已删除——UI Task 9 同步改;旧前端 invoke 会参数错(可接受断裂)
#[tauri::command]
pub async fn remove_transfer(
    state: State<'_, AppState>,
    job_id: String,
    level: String,
) -> Result<bool, String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    remove_transfer_inner(state.inner(), id, &level).await
}

/// Task 5:两级删除内部实现(测试直调;#[tauri::command] 包装层不进测试)
/// - view:仅终态卡;removed=true + 表保留 + transfers.json 保留(带 removed 标记)
/// - destroy:终态卡直接实删;活动卡走 cancelling 仲裁(引擎确认/5s 看门狗)
pub async fn remove_transfer_inner(
    st: &AppState,
    card_id: u64,
    level: &str,
) -> Result<bool, String> {
    let Some(card) = st.card_get(card_id).await else {
        return Ok(false);
    };
    match level {
        "view" => {
            // 裁定:view 级只对终态卡有意义;活动卡要求先取消
            if !matches!(card.dto.state.as_str(), "done" | "failed" | "interrupted") {
                return Err("任务进行中，请先取消".to_string());
            }
            let ok = st.card_mark_removed(card_id).await;
            Ok(ok)
        }
        "destroy" => {
            destroy_transfer(st, card_id).await?;
            Ok(true)
        }
        _ => Err(format!("无效删除级别: {}", level)),
    }
}

/// Task 5:彻底删除。终态卡直接实删;活动卡(pending/active/paused)走
/// cancelling 两段式:CancelRequested + 既有取消路径(占位标志/sender_jobs/
/// push_control/control_task+跨端 TransferCtl)→ 引擎终态确认或 5s 看门狗超时
/// 后由 finalize_destroy 实删。
pub async fn destroy_transfer(st: &AppState, card_id: u64) -> Result<(), String> {
    let Some(card) = st.card_get(card_id).await else {
        return Err(format!("未找到任务: {:016x}", card_id));
    };
    let state = card.dto.state.clone();
    let is_terminal = matches!(state.as_str(), "done" | "failed" | "interrupted");

    if is_terminal {
        st.finalize_destroy(card_id).await;
        return Ok(());
    }

    // 活动卡:进入 cancelling(card_apply 单写者;进不去说明状态已变,重新检查)
    st.card_apply(card_id, crate::transfer_state::CardEvent::CancelRequested).await;
    let now_cancelling = st.card_get(card_id).await
        .map(|c| c.dto.state == "cancelling")
        .unwrap_or(false);
    if !now_cancelling {
        return Err(format!("任务状态 {} 不支持删除", state));
    }

    // 复用既有取消分支:占位任务 → 置取消标志(占位按 card_id 登记)
    if let Some(flag) = st.placeholder_cancels.lock().await.get(&card_id).cloned() {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // 终审必修:引擎侧三表按 engine job 注册,card_id 直查全 miss——先翻译
    let eid = resolve_engine_id(st, card_id).await;
    // sender_jobs(source-pull / source-push 引擎) → 置 cancelled
    if let Some(sender_state) = st.sender_jobs.read().await.get(&eid).cloned() {
        sender_state.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // push_control(source-push 引擎) → 置 cancelled
    if let Some(push_control) = localtrans_core::transfer::engine::get_push_control(eid) {
        push_control.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    // 拉取任务本地注册表控制 + 跨端通知对端清理(尽力而为,连接断开忽略;
    // 跨端 TransferCtl 的 job_id 用 engine job——对端按其引擎注册表清理)
    let _ = localtrans_core::transfer::control_task(eid, transfer::TaskControl::Cancel);
    if let Some(fp) = peer_fingerprint_of(&card.dto) {
        let _ = st.sm.send_ctrl(&fp, protocol::ControlMsg::TransferCtl {
            job_id: eid,
            action: protocol::TransferAction::Cancel,
        }).await;
    }

    // 挂 5s 看门狗:超时仍 cancelling → 直接实删;确认先到 → card_apply abort。
    // 修复轮 1:check-and-insert 合并进单个锁临界区,防并发 destroy 同一卡双挂。
    let mut watchdogs = st.cancel_watchdogs.lock().await;
    if watchdogs.contains_key(&card_id) {
        return Ok(());
    }
    let st2 = st.clone();
    let h = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        // 仍 cancelling → 引擎没回话,兜底实删
        if st2.card_get(card_id).await.map(|c| c.dto.state == "cancelling").unwrap_or(false) {
            tracing::warn!("删除看门狗超时,兜底实删 card={:016x}", card_id);
            st2.finalize_destroy(card_id).await;
        } else {
            st2.cancel_watchdogs.lock().await.remove(&card_id);
        }
    });
    watchdogs.insert(card_id, h);
    drop(watchdogs);
    Ok(())
}

/// 传输任务限速
#[tauri::command]
pub async fn transfer_throttle(
    state: State<'_, AppState>,
    job_id: String,
    max_streams: u32,
) -> Result<(), String> {
    let card_id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    // 终审必修:sender_jobs 按 engine job 注册,card_id 先翻译
    let id = resolve_engine_id(state.inner(), card_id).await;
    let jobs = state.sender_jobs.read().await;
    let entry = jobs.get(&id).ok_or_else(|| "任务不存在或不在 sender 端".to_string())?;
    entry.throttle_cap.store(max_streams.max(1), std::sync::atomic::Ordering::Relaxed);
    Ok(())
}

/// 检查任务是否有 parts 目录
#[tauri::command]
pub async fn has_parts(
    state: State<'_, AppState>,
    job_id: String,
) -> Result<bool, String> {
    let id = u64::from_str_radix(job_id.trim_start_matches("0x"), 16)
        .map_err(|e| format!("无效 job_id: {}", e))?;
    // ID 恒定:parts 目录名是 engine_id(绑定后)——优先翻译,无映射按 card_id
    let pid = state.engine_id_of(id).await.unwrap_or(id);
    let parts_dir = state.config.read().await.download_dir
        .join(format!(".localtrans-parts/{:016x}", pid));
    Ok(parts_dir.exists())
}

/// 设备列表合并——转发 core 实现(排序/去重/别称规则与安卓壳同源)
pub fn merge_devices(
    local: &[discovery::DeviceInfo],
    roster: &[localtrans_core::relay::proto::RemoteDevice],
    connected: &std::collections::HashSet<String>,
    aliases: &std::collections::HashMap<String, String>,
    trusted: &[(String, String, u64)],
) -> Vec<crate::DeviceDto> {
    localtrans_core::device_merge::merge_devices(local, roster, connected, aliases, trusted)
        .into_iter()
        .map(|m| crate::DeviceDto {
            fingerprint: m.fingerprint,
            name: m.name,
            addr: m.addr,
            online: m.online,
            connected: m.connected,
            via_relay: m.via_relay,
            force_relay: false,
        })
        .collect()
}

/// M3c T3:合并设备列表 + 注记强制走中继开关(list_devices 与全部
/// device-list 事件共用——开关切换后卡片角标/菜单勾选态即时刷新)。
pub async fn merged_device_dtos(st: &AppState) -> Vec<crate::DeviceDto> {
    let devices = st.devices.lock().await.clone();
    let roster = st.relay_roster.lock().await.clone();
    let connected = st.connected_fps.lock().await.clone();
    let aliases = alias_map(&*st.trust.lock().await);
    let trusted = trusted_pairs(&*st.trust.lock().await);
    let force = st.config.read().await.force_relay_map.clone();
    let mut dtos = merge_devices(&devices, &roster, &connected, &aliases, &trusted);
    for d in dtos.iter_mut() {
        d.force_relay = force.get(&d.fingerprint).copied().unwrap_or(false);
    }
    dtos
}

/// 从信任列表提取 指纹→别名 映射——转发 core
pub fn alias_map(
    trust: &localtrans_core::identity::TrustStore,
) -> std::collections::HashMap<String, String> {
    localtrans_core::device_merge::alias_map(trust)
}

/// 从信任列表提取 (指纹,配对名,配对时间) 列表——转发 core(配对设备常驻数据源;
/// paired_at 供 P3-T5 同名重复旧条目归档判定)
pub fn trusted_pairs(
    trust: &localtrans_core::identity::TrustStore,
) -> Vec<(String, String, u64)> {
    localtrans_core::device_merge::trusted_pairs(trust)
}

#[cfg(test)]
mod tests {
    use super::*;
    use localtrans_core::store::ShareDef;

    #[test]
    fn cooldown_error_is_structured_for_frontend() {
        // P2 配对健壮性:冷却期 connect 错误必须结构化为 `pairing_cooldown:{secs}`
        // 前缀(前端弹「对方设备处于配对冷却（剩余 X 秒）」+ 设备卡倒计时禁用),
        // 其余 SessionError 保持 Display 原文
        let cd = fmt_connect_err(localtrans_core::session::SessionError::Cooldown(295));
        assert_eq!(cd, "pairing_cooldown:295");
        let other = fmt_connect_err(localtrans_core::session::SessionError::Pairing("会话不存在".into()));
        assert_eq!(other, "配对错误: 会话不存在");
    }

    #[tokio::test]
    async fn force_relay_connect_errors_when_relay_absent() {
        // M3c T3 拦截语义:开关开启 + 中继不在场 → connect_inner 立即报
        // 「中继未配置」,不得落回本地发现直连路径(即使设备就在发现表里)
        let st = crate::test_support::test_app_state().await;
        let fp = [0x77u8; 32];
        let fp_hex = hex::encode(fp);
        st.devices.lock().await.push(localtrans_core::discovery::DeviceInfo {
            fingerprint: fp,
            name: "直连在场设备".into(),
            addr: "127.0.0.1:1".parse().unwrap(),
            last_seen: std::time::Instant::now(),
        });
        st.config.write().await.force_relay_map.insert(fp_hex.clone(), true);

        let t0 = std::time::Instant::now();
        let err = connect_inner(&st, &fp_hex, fp).await.unwrap_err();
        assert!(err.contains("中继未配置"), "应报「中继未配置」,实际: {err}");
        assert!(t0.elapsed() < std::time::Duration::from_secs(3),
            "拦截必须先于直连尝试(否则连接超时),实际 {:?}", t0.elapsed());
        // 注:无开关时走既有 DefaultDirect 直连路径,由 choose_connect_path
        // 单测与 M3b 环回测试覆盖,此处不重复(直连死地址会拖满连接超时)。
    }

    #[tokio::test]
    async fn remove_trusted_clears_connect_memory() {
        // M3a FR5:移除信任同步清连接记忆(先清后断,见 inner 注);
        // 未受信设备返回 false 且不动记忆
        let st = crate::test_support::test_app_state().await;
        let fp = [0x42u8; 32];
        st.trust.lock().await.upsert(localtrans_core::identity::TrustedPeer {
            fingerprint: fp, name: "B 机".into(), alias: String::new(),
            paired_at: 1, perms: Default::default(),
        });
        assert!(st.connect_memory.lock().await.record(&fp));
        // 存在的信任移除:信任 + 记忆双清
        assert!(remove_trusted_inner(&st, &fp).await.unwrap());
        assert!(!st.trust.lock().await.is_trusted(&fp));
        assert!(!st.connect_memory.lock().await.contains(&fp));
        // 落盘生效:重载后记忆仍无该条目
        let mem2 = localtrans_core::connect_memory::ConnectMemory::load(&st.dir);
        assert!(!mem2.contains(&fp));
        // 不存在的信任:返回 false,记忆保留(用户没移除信任,不误清)
        let fp2 = [0x43u8; 32];
        assert!(st.connect_memory.lock().await.record(&fp2));
        assert!(!remove_trusted_inner(&st, &fp2).await.unwrap());
        assert!(st.connect_memory.lock().await.contains(&fp2));
    }

    #[test]
    fn config_to_dto_maps_all_fields() {
        let mut cfg = localtrans_core::store::Config::default();
        cfg.device_name = "测试机".into();
        cfg.download_dir = std::path::PathBuf::from("D:/dl");
        cfg.hidden = true;
        cfg.consent_timeout_secs = 120;
        cfg.relay_enabled = true;
        cfg.relay_server = "203.0.113.10:9443".into();
        cfg.relay_psk = "f4928926_test_psk_value_deadbeef".into();
        let reg = share::ShareRegistry::new(vec![ShareDef {
            id: "s1".into(),
            alias: "电影".into(),
            path: "D:/video".into(),
        }]);

        let dto = config_to_dto(&cfg, &reg);
        assert_eq!(dto.device_name, "测试机");
        assert_eq!(dto.download_dir, "D:/dl");
        assert!(dto.hidden);
        assert_eq!(dto.consent_timeout_secs, 120);
        // v0.9.0 修:v0.8.3 漏掉 relay 字段导致 UI 重启后看不到已保存配置
        assert!(dto.relay_enabled, "relay_enabled 应映射");
        assert_eq!(dto.relay_server, "203.0.113.10:9443", "relay_server 应映射");
        // A4 隐私修复:relay_psk 掩码回显
        assert_eq!(dto.relay_psk, "****beef", "relay_psk 应掩码映射");
        assert_eq!(dto.shares.len(), 1);
        assert_eq!(dto.shares[0].alias, "电影");
        assert_eq!(dto.shares[0].path, "D:/video");
    }

    #[test]
    fn mask_psk_rules() {
        assert_eq!(mask_psk(""), "");
        assert_eq!(mask_psk("abc"), "****");
        assert_eq!(mask_psk("12345678"), "****5678");
        assert_eq!(mask_psk("f4928926_test_psk_value_deadbeef"), "****beef");
        // 多字节 UTF-8:按 char 截取,不 panic
        assert_eq!(mask_psk("中文密码一二三四五六"), "****三四五六");
        assert_eq!(mask_psk("一二三"), "****"); // 9 字节但 3 字符,全掩
        assert_eq!(mask_psk("密码一二三四五六"), "****三四五六");
    }

    #[test]
    fn save_settings_clamps_offer_timeout() {
        // Test the clamp function directly
        assert_eq!(5u64.clamp(15, 600), 15, "低于 15 钳到 15");
        assert_eq!(9999u64.clamp(15, 600), 600, "高于 600 钳到 600");
        assert_eq!(60u64.clamp(15, 600), 60, "范围内不变");
    }

    #[test]
    fn peer_fingerprint_of_parses_and_rejects() {
        let dto = |peer: &str| TransferDto {
            job_id: 1, name: "t".into(), total: 1, done: 0,
            state: "active".into(), speed_bps: 0, peer: peer.into(),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: None,
            remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        };
        // 合法 32 字节 hex
        assert!(peer_fingerprint_of(&dto(&"ab".repeat(32))).is_some());
        // 空 peer(乙侧接收行)→ None,调用侧跳过发送
        assert!(peer_fingerprint_of(&dto("")).is_none());
        // 非 hex / 长度不符
        assert!(peer_fingerprint_of(&dto("xyz")).is_none());
        assert!(peer_fingerprint_of(&dto(&"ab".repeat(16))).is_none());
    }

    // ===== 修复轮 1:历史入口并表(gc 后全真孤儿仍可见) =====

    fn table_card(state: &str, job_id: u64, removed: bool) -> crate::TransferCardSerde {
        crate::TransferCardSerde {
            dto: TransferDto {
                job_id, name: "in-table".into(), total: 5, done: 0, state: state.into(),
                speed_bps: 0, peer: "aa".into(), direction: "pull".into(),
                local_role: "destination".into(), health: None, started_at_ms: None,
                finished_at_ms: None, source_path: None, fail_reason: None,
                remote_done: 0, instant: false, queue_pos: None, batch_id: None,
                children: vec![], parts_id: None,
            },
            removed,
        }
    }

    #[test]
    fn list_disk_jobs_merges_in_table_cards_without_parts() {
        // 表内有 failed 卡但 parts 目录被 gc → 历史入口仍列出
        let cards = vec![table_card("failed", 0x10, false)];
        let merged = merge_disk_jobs(vec![], &cards);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].job_id, 0x10);
        assert_eq!(merged[0].state, "failed");
        assert!(!merged[0].removed_from_view);
    }

    #[test]
    fn list_disk_jobs_merge_dedups_disk_first_and_skips_open() {
        // 磁盘条目优先去重;表内 open 状态不并入
        let disk = vec![DiskJobDto {
            job_id: 0x20, display_name: "disk".into(), total: 9, done: 0,
            state: "interrupted".into(), direction: "pull".into(),
            peer_hex: String::new(), created_at_ms: None, removed_from_view: false,
        }];
        let cards = vec![
            table_card("interrupted", 0x20, true),  // 与磁盘重复 → 磁盘优先
            table_card("active", 0x30, false),      // open → 跳过
        ];
        let merged = merge_disk_jobs(disk, &cards);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].display_name, "disk", "磁盘条目优先");
    }

    // ===== save_dir 校验(v0.11.x 用户裁定:不限下载目录,仅最小校验) =====

    #[test]
    fn save_dir_any_absolute_path_allowed() {
        // 任意绝对路径都放行(自定义/另存为不限下载目录)
        let other = tempfile::TempDir::new().unwrap();
        assert!(ensure_save_dir_valid(other.path()).is_ok());
        // 尚未创建的深层目录也可以(接收侧负责创建)
        let deep = other.path().join("a").join("b");
        assert!(ensure_save_dir_valid(&deep).is_ok());
        // 下载目录自身当然也放行
        assert!(ensure_save_dir_valid(Path::new("C:\\Downloads")).is_ok());
    }

    #[test]
    fn save_dir_empty_or_relative_rejected() {
        // 空路径拒绝
        assert!(ensure_save_dir_valid(Path::new("")).is_err());
        // 相对路径拒绝(防 CWD 依赖的歧义落点)
        assert!(ensure_save_dir_valid(Path::new("relative/dir")).is_err());
        assert!(ensure_save_dir_valid(Path::new("./x")).is_err());
    }

    // ===== Task 5: 删除仲裁 + 两级删除 =====

    use crate::test_support::test_app_state;

    fn done_dto(state: &str) -> TransferDto {
        TransferDto {
            job_id: 0, name: "t.bin".into(), total: 100, done: 100,
            state: state.into(), speed_bps: 0, peer: "aa".repeat(32),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: Some(1),
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }
    }

    fn active_dto() -> TransferDto {
        TransferDto {
            job_id: 0, name: "t.bin".into(), total: 100, done: 40,
            state: "active".into(), speed_bps: 10, peer: "aa".repeat(32),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: Some(1), finished_at_ms: None,
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }
    }

    #[tokio::test]
    async fn resume_event_pump_uses_card_id_not_engine_id() {
        // 装机修复 BUG-B:续传事件泵的卡片键必须是 card_id(0x4000 段)。
        // 拉取任务的 engine_id 是对端分配的 0x8000 段 source id(=parts 目录名),
        // 曾被误当 card_id 传给事件泵 → card_apply 卡片不存在 → 进度全丢、
        // 卡片停在 failed、引擎后台空传。此测试钉住:bind 后按 card_id 翻译,
        // 事件落回 card_id 的卡片上。
        let st = test_app_state().await;
        let card_id = st.card_create(active_dto()).await;
        let engine_id: u64 = 0x8000_0000_0000_0002; // 对端 source id 段
        st.bind_engine_id(engine_id, card_id).await;

        // 翻译方向:控制/续传入口持 card_id → 解析 engine_id(对端注册表键)
        let resolved = resolve_engine_id(&st, card_id).await;
        assert_eq!(resolved, engine_id, "card→engine 翻译");

        // 事件泵方向:引擎事件经 engine_event 翻译回 card_id 落卡。
        // 先经取消落终态,再验证事件不落 0x8000 键(card_apply 只认 card_id)。
        st.card_apply(card_id, crate::transfer_state::CardEvent::Failed {
            reason: Some("已取消".into()),
        }).await;
        // engine 侧迟到的进度事件:经翻译应打到 card_id;终态吸收(不复活)
        st.engine_event(engine_id, crate::transfer_state::CardEvent::Progress {
            done: 50, total: 100, speed_bps: 1, remote_done: 0, health: None,
        }).await;
        let card = st.card_get(card_id).await.unwrap();
        assert_eq!(card.dto.state, "failed", "终态吸收,进度不复活");
        assert!(st.card_get(engine_id).await.is_none(), "0x8000 键从未建卡");
    }

    #[tokio::test]
    async fn view_remove_keeps_card_and_parts() {
        // 终态卡 view 删除:removed=true,表里还在,parts 不动
        let st = test_app_state().await;
        let card_id = st.card_create(done_dto("done")).await;
        let ok = remove_transfer_inner(&st, card_id, "view").await.unwrap();
        assert!(ok);
        let card = st.card_get(card_id).await.unwrap();
        assert!(card.removed);
        assert_eq!(st.snapshot_dtos().await.len(), 0, "活动列表消失");
    }

    #[tokio::test]
    async fn view_remove_rejects_active_card() {
        // 裁定:view 级只对终态卡有意义,活动卡要求先取消
        let st = test_app_state().await;
        let card_id = st.card_create(active_dto()).await;
        assert!(remove_transfer_inner(&st, card_id, "view").await.is_err());
        assert!(st.card_get(card_id).await.is_some(), "卡片未被误删");
    }

    #[tokio::test]
    async fn destroy_active_goes_through_cancelling() {
        let st = test_app_state().await;
        let card_id = st.card_create(active_dto()).await;
        st.bind_engine_id(0x77, card_id).await;
        // destroy 活动卡:立即变 cancelling,不直接消失
        destroy_transfer(&st, card_id).await.unwrap();
        let card = st.card_get(card_id).await.unwrap();
        assert_eq!(card.dto.state, "cancelling");
        // 引擎终态到达 → 实删
        st.engine_event(0x77, crate::transfer_state::CardEvent::Interrupted).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await; // destroy 异步收尾
        assert!(st.card_get(card_id).await.is_none(), "确认后删除");
    }

    #[tokio::test]
    async fn destroy_terminal_deletes_immediately() {
        let st = test_app_state().await;
        let card_id = st.card_create(done_dto("failed")).await;
        destroy_transfer(&st, card_id).await.unwrap();
        assert!(st.card_get(card_id).await.is_none(), "终态卡直接删除");
    }

    // ===== 终审必修:控制命令 card_id→engine_id 翻译 =====

    #[tokio::test]
    async fn transfer_action_and_throttle_translate_card_id_to_engine_id() {
        use localtrans_core::transfer::sender_state::new_sender_job_state;
        let st = test_app_state().await;
        // 建活动卡,前端持有 card_id(0x4000 高位段)
        let card_id = st.card_create(active_dto()).await;
        // 引擎侧 sender_jobs 按 engine job 0x77 注册(真实 SenderJobState)
        let (tx, _rx) = tokio::sync::mpsc::channel(64);
        let manifest = localtrans_core::transfer::manifest::Manifest::build(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml").as_path(),
        ).unwrap();
        let sender = std::sync::Arc::new(new_sender_job_state(0x77, std::path::PathBuf::from("x"), manifest, None, tx));
        st.sender_jobs.write().await.insert(0x77, sender.clone());
        st.bind_engine_id(0x77, card_id).await;

        // control_task 需要 session,不可达——抽可测断言点:resolve 后 eid 查表命中
        let eid = resolve_engine_id(&st, card_id).await;
        assert_eq!(eid, 0x77, "card_id 应翻译为 engine job");
        assert!(st.sender_jobs.read().await.get(&eid).is_some(),
            "翻译后的 eid 必须命中引擎注册表(修复前裸 card_id 全 miss)");

        // 经 card_id 走 transfer_action 的 sender_jobs 分支逻辑:paused 标志真实生效
        // (transfer_action 内部对 eid 查到的同一 sender_state 置标志)
        let s = st.sender_jobs.read().await.get(&eid).unwrap().clone();
        s.paused.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(s.paused.load(std::sync::atomic::Ordering::Relaxed));
        assert!(sender.paused.load(std::sync::atomic::Ordering::Relaxed), "同一引擎侧对象");

        // transfer_throttle 同路径:card_id 直查此前 miss 报"任务不存在",翻译后命中
        let entry = st.sender_jobs.read().await
            .get(&resolve_engine_id(&st, card_id).await)
            .cloned()
            .ok_or_else(|| "任务不存在或不在 sender 端".to_string());
        assert!(entry.is_ok(), "throttle 经翻译后命中");
        entry.unwrap().throttle_cap.store(3, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(sender.throttle_cap.load(std::sync::atomic::Ordering::Relaxed), 3);

        // destroy_transfer 活动卡分支:card_id→eid 翻译后 cancelled 标志打到引擎侧
        destroy_transfer(&st, card_id).await.unwrap();
        assert!(sender.cancelled.load(std::sync::atomic::Ordering::Relaxed),
            "destroy 后引擎侧 cancelled 已置位");
        assert_eq!(st.card_get(card_id).await.unwrap().dto.state, "cancelling");
    }

    // ===== Task 8: 多文件统一父卡片 + 子项挂接 =====

    fn parent_dto(name: &str) -> TransferDto {
        TransferDto {
            job_id: 0, name: name.into(), total: 0, done: 0,
            state: "pending".into(), speed_bps: 0, peer: "aa".repeat(32),
            direction: "push".into(), local_role: "source-push".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: Some("batch-1".into()), children: vec![], parts_id: None,
        }
    }

    #[tokio::test]
    async fn push_batch_stays_one_card_children_upsert() {
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("3 个文件")).await; // push 批次占位
        // 引擎子文件1 Started:不裂变,挂 child
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x101), name: "a.txt".into(),
            total: 10, done: 0, state: "active".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x102), name: "b.txt".into(),
            total: 20, done: 20, state: "done".into(),
        }).await;
        let snap = st.snapshot_dtos().await;
        assert_eq!(snap.len(), 1, "恒一张父卡");
        assert_eq!(snap[0].children.len(), 2);
        // 子1完成 → 全终态 → 父 done
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x101), name: "a.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        assert_eq!(st.card_get(parent).await.unwrap().dto.state, "done");
    }

    #[tokio::test]
    async fn parent_failed_when_any_child_failed() {
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("2 个文件")).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x201), name: "bad.txt".into(),
            total: 10, done: 0, state: "failed".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x202), name: "good.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.state, "failed");
        assert!(card.dto.fail_reason.as_deref().unwrap().contains("1 项"), "reason 应含失败数");
    }

    /// N1-T1b:单文件推送的 source job 事件绑回占位卡——不裂变第二张卡,
    /// 且被动绑定不顶 card.engine_id(取消仍路由首个绑定的 offer job,
    /// 经 T16 控制桥共享 Arc 级联到 source 任务——僵尸 active 卡根因修复)
    #[tokio::test]
    async fn single_push_source_started_binds_placeholder_no_new_card() {
        use crate::test_support::test_app_state;
        let st = test_app_state().await;
        let fp = [7u8; 32];
        let card = st.card_create(TransferDto {
            job_id: 0, name: "f.bin".into(), total: 0, done: 0,
            state: "pending".into(), speed_bps: 0,
            peer: hex::encode(fp), direction: "push".into(), local_role: "source-push".into(),
            health: None, started_at_ms: None, finished_at_ms: None, source_path: None,
            fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }).await;
        // offer job 先绑(offer Started 到达时)——控制命令的路由锚
        st.bind_engine_id(0x99, card).await;
        // 单文件推送登记(push_files 拿到槽位后写入)
        st.pending_children.lock().await.insert(single_peer_key(&fp), card);

        // 大文件回拉:source 桥收到 SourceStarted(source job)
        let attached = source_push_attach(&st, 0x8001, &fp, "f.bin", 10 * 1024 * 1024).await;
        assert!(attached, "单文件登记应被挂接消化");
        assert_eq!(st.snapshot_dtos().await.len(), 1, "不裂变——仍只有占位卡一张");
        assert_eq!(st.card_id_of(0x8001).await, card, "source 事件应路由到占位卡");
        assert_eq!(st.engine_id_of(card).await, Some(0x99),
            "被动绑定不顶 engine_id——取消仍路由 offer job");

        // source 字节事件路由到占位卡并累计
        st.source_chunk_add(st.card_id_of(0x8001).await, 1024).await;
        assert_eq!(st.card_dto(card).await.unwrap().done, 1024);

        // 终态卡上的迟到 SourceStarted:消化(丢弃)不建新卡
        st.card_apply(card, crate::transfer_state::CardEvent::Failed { reason: None }).await;
        let attached2 = source_push_attach(&st, 0x8002, &fp, "f2.bin", 5).await;
        assert!(attached2, "登记仍在应消化(丢弃)");
        assert_eq!(st.snapshot_dtos().await.len(), 1, "终态后不复活不建新卡");

        // 无任何登记 → false(走通用建卡路径)
        let fp2 = [9u8; 32];
        assert!(!source_push_attach(&st, 0x8003, &fp2, "x.bin", 5).await);
    }

    /// D2 双计根因(2026-09-07 api-acceptance 四轮 100% 复现):三个挂接键
    /// 用哨兵 OR 掩码,指纹首字节高位已置 1 时(概率 50%,实机 fpB 首字节
    /// 0xE6)三个键同值——单文件登记被批次分支命中 → child_upsert 追加
    /// total+=SIZE。键空间必须按 tag 高位真正隔离,与指纹字节无关。
    #[test]
    fn peer_attach_keys_disjoint_for_high_bit_fingerprints() {
        // 实机形状:huss_laptop 指纹前 8 字节 e6 0b 36 68 96 d5 0f 43
        let fp: [u8; 32] = {
            let mut f = [0u8; 32];
            f[0] = 0xE6; f[1] = 0x0B; f[2] = 0x36; f[3] = 0x68;
            f[4] = 0x96; f[5] = 0xD5; f[6] = 0x0F; f[7] = 0x43;
            f
        };
        let batch = batch_peer_key(&fp);
        let retry = retry_peer_key(&fp);
        let single = single_peer_key(&fp);
        assert_ne!(batch, single, "批次键与单文件键不得同值(碰撞=单文件登记被批次分支吞掉)");
        assert_ne!(retry, single, "重试键与单文件键不得同值");
        assert_ne!(batch, retry, "批次键与重试键不得同值");
        // 低字节指纹同样三键互异(回归旧用例的盲区补齐)
        let fp_low = [7u8; 32];
        assert_ne!(batch_peer_key(&fp_low), single_peer_key(&fp_low));
        assert_ne!(retry_peer_key(&fp_low), single_peer_key(&fp_low));
        // 键空间不与 B 端 0x8000 段 source job id、引擎小整数 job id 相撞
        let source_id = 0x8000_0000_0000_0000u64; // B 端首个回拉 source id
        assert_ne!(batch, source_id);
        assert_ne!(retry, source_id);
        assert_ne!(single, source_id);
    }

    /// D2 事件序列回归:单文件大推送(offer Started 先到,total=SIZE)后,
    /// source job 的 SourceStarted 必须走单文件被动绑定——不得被批次分支
    /// 吞掉(旧缺陷:child_upsert 追加 total+=SIZE → 卡 total=2×SIZE,
    /// SourceChunkDone 全被 batch_child_chunk 截走,暂停/终态语义全坏)。
    #[tokio::test]
    async fn single_push_attach_never_takes_batch_branch_total_doubled() {
        use crate::test_support::test_app_state;
        let st = test_app_state().await;
        // 实机形状指纹(首字节 0xE6,旧实现下三键同值)
        let mut fp = [0u8; 32];
        fp[0] = 0xE6; fp[1] = 0x0B; fp[2] = 0x36; fp[3] = 0x68;
        let size: u64 = 500 * 1048576;

        // push_files 单文件路径:占位卡 + single_peer_key 登记
        let card = st.card_create(TransferDto {
            job_id: 0, name: "big-500mb.bin".into(), total: 0, done: 0,
            state: "pending".into(), speed_bps: 0,
            peer: hex::encode(fp), direction: "push".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: None, source_path: None,
            fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }).await;
        st.pending_children.lock().await.insert(single_peer_key(&fp), card);

        // offer 通道 Started 先到(D2 实测顺序):绑 offer job + total=SIZE
        st.bind_engine_id(0x1, card).await;
        st.card_mutate(card, |d| { d.name = "big-500mb.bin".into(); d.total = size; }).await;
        st.card_apply(card, crate::transfer_state::CardEvent::Started).await;

        // source 桥 SourceStarted(B 端 0x8000 段 source job id)
        let source_job: u64 = 0x8000_0000_0000_0000;
        let attached = source_push_attach(&st, source_job, &fp, "big-500mb.bin", size).await;
        assert!(attached, "单文件登记应被消化");
        let dto = st.card_dto(card).await.unwrap();
        assert_eq!(dto.total, size, "total 语义:单文件卡 total==文件大小(旧缺陷=2×SIZE)");
        assert!(dto.children.is_empty(), "单文件卡无 children 语义(旧缺陷被追加子项)");

        // source job 不得被登记成 batch child(否则 SourceChunkDone 被截走)
        assert_eq!(st.pending_children_parent_of(source_job).await, None,
            "source job 挂接表残留=后续 ChunkDone 走 batch_child_chunk 截流");
        // 字节事件仍应路由到占位卡(done 单份累加)
        assert_eq!(st.card_id_of(source_job).await, card, "source 事件应被动绑回占位卡");
        st.source_chunk_add(card, 4 * 1048576).await;
        let dto = st.card_dto(card).await.unwrap();
        assert_eq!(dto.done, 4 * 1048576);
        assert_eq!(dto.total, size, "ChunkDone 只动 done 不动 total");
    }

    /// 占位取消信号存活整个传输期(引擎结束才移除),分支决策必须按任务
    /// 是否已 Started 区分——传输中 pause/resume 走 T16 控制路径,
    /// cancel 恒走标志快速路径。旧实现对传输中 pause 误拒"任务尚未开始"。
    #[test]
    fn placeholder_action_decision_by_started_state() {
        use crate::commands::placeholder_action;
        // 尚未开始(排队/等 OfferResp):cancel 置标志,pause/resume 拒绝
        assert!(matches!(placeholder_action(false, "cancel"), PlaceholderAction::FlagCancel));
        assert!(matches!(placeholder_action(false, "pause"), PlaceholderAction::RejectNotStarted));
        assert!(matches!(placeholder_action(false, "resume"), PlaceholderAction::RejectNotStarted));
        // 已 Started(传输中):pause/resume 落 T16 控制路径(修复点),
        // cancel 仍走标志快速路径(即时退出引擎等待循环,释放并发槽)
        assert!(matches!(placeholder_action(true, "cancel"), PlaceholderAction::FlagCancel));
        assert!(matches!(placeholder_action(true, "pause"), PlaceholderAction::Fallthrough));
        assert!(matches!(placeholder_action(true, "resume"), PlaceholderAction::Fallthrough));
    }

    #[tokio::test]
    async fn empty_job_id_children_dedup_by_name() {
        // 修复轮 P1:空 job_id 子项按 name 去重——3 子项不坍缩
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("3 个小文件")).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "a.txt".into(),
            total: 10, done: 0, state: "active".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "b.txt".into(),
            total: 20, done: 0, state: "active".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "c.txt".into(),
            total: 30, done: 0, state: "active".into(),
        }).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.children.len(), 3, "3 个空 job_id 子项不应坍缩");
        assert_eq!(card.dto.total, 60);
        // 首子 Done:父卡不提前终态
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "a.txt".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.state, "pending", "首子完成父卡不应提前终态");
        // 全 Done:父卡 done=total 且终态
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "b.txt".into(),
            total: 20, done: 20, state: "done".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "c.txt".into(),
            total: 30, done: 30, state: "done".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.state, "done");
        assert_eq!(card.dto.done, 60, "父 done 应等于子项总量");
        assert_eq!(card.dto.total, 60);
    }

    #[tokio::test]
    async fn batch_child_terminal_keeps_total() {
        // 修复轮 P2:小文件 Done 终态保留 Started 记录的 total,done=total
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("2 个小文件")).await;
        batch_child_start(&st, parent, "x.bin", 100).await;
        batch_child_oldest_active(&st, parent, "done").await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.children[0].total, 100, "终态不应归零 total");
        assert_eq!(card.dto.children[0].done, 100, "终态 done=total(完成语义)");
    }

    #[tokio::test]
    async fn batch_registration_two_batches_no_clobber() {
        // 修复轮 P3:同 peer 批次 A 登记→批次 B(同串行语义下后者必须在前者
        // 释放后才能登记)——用注册表 remove-with-owner 语义验证 B 不会顶掉 A
        let st = test_app_state().await;
        let fp = [7u8; 32];
        let key = batch_peer_key(&fp);
        let parent_a = st.card_create(parent_dto("批次A")).await;
        st.pending_children_bind(key, parent_a).await;
        // 批次 B 尝试"登记"(未 remove 旧 owner 的场景下直接 insert 才会顶掉;
        // 修复后的时序:B 只能等 A 终态 recheck 清账后再登记)
        let parent_b = st.card_create(parent_dto("批次B")).await;
        st.pending_children_remove(key, parent_b).await; // B 不是 owner,不应移除
        assert_eq!(
            st.pending_children_parent_of(key).await, Some(parent_a),
            "B 误登记不得顶掉 A 的挂接"
        );
        // A 终态清账后 B 才能接管
        st.pending_children_remove(key, parent_a).await;
        st.pending_children_bind(key, parent_b).await;
        assert_eq!(st.pending_children_parent_of(key).await, Some(parent_b));
    }

    #[tokio::test]
    async fn recheck_does_not_overwrite_terminal_fail_reason() {
        // 修复轮 P4②:父卡已终态(整批取消 failed"已取消")后子项回执 recheck 不改写
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("取消批次")).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x301), name: "f1".into(),
            total: 10, done: 0, state: "active".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x302), name: "f2".into(),
            total: 10, done: 10, state: "done".into(),
        }).await;
        st.card_apply(parent, crate::transfer_state::CardEvent::Failed {
            reason: Some("已取消".into()),
        }).await;
        // 取消后子项回执落终态 → recheck
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x301), name: "f1".into(),
            total: 10, done: 0, state: "failed".into(),
        }).await;
        st.recheck_parent_terminal(parent).await;
        let card = st.card_get(parent).await.unwrap();
        assert_eq!(card.dto.fail_reason.as_deref(), Some("已取消"), "取消原因不应被子项回执改写");
    }

    #[tokio::test]
    async fn mixed_batch_cancel_enters_whole_batch_branch() {
        // 修复轮 P4①:大文件子全终态+小文件空 job_id 活动 → 进整批分支
        let st = test_app_state().await;
        let parent = st.card_create(parent_dto("混合批次")).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: format!("{:x}", 0x401), name: "big.bin".into(),
            total: 1000, done: 1000, state: "done".into(),
        }).await;
        st.child_upsert(parent, crate::ChildDto {
            job_id: String::new(), name: "small.txt".into(),
            total: 5, done: 0, state: "active".into(),
        }).await;
        assert!(st.active_child_engine_ids(parent).await.is_empty(),
            "引擎子全终态,engine ids 为空(触发原缺陷的前置)");
        assert!(st.has_active_children(parent).await,
            "但仍有活动子项(空 job_id 小文件)——整批分支应进入");
    }

    #[tokio::test]
    async fn clear_completed_is_view_level() {
        let st = test_app_state().await;
        st.card_create(done_dto("done")).await;
        st.card_create(done_dto("failed")).await;
        st.card_create(active_dto()).await;
        let n = clear_completed_inner(&st).await.unwrap();
        assert_eq!(n, 2, "只清终态");
        assert_eq!(st.snapshot_dtos().await.len(), 1);
    }
}
