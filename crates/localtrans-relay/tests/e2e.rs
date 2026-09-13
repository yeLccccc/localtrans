//! E2E:两台"设备"(进程内)+ 一个中继,真 UDP 回环。
//! 验收:虚拟端点 → 令牌头 → 中继会话转发 → 内层 QUIC mTLS 握手 →
//! 互发一条 ControlMsg → RecvAck 语义到达。

use localtrans_core::identity::Identity;
use localtrans_core::protocol::{ControlMsg, decode_control, encode_control};
use localtrans_core::relay::client::{RelayClient, RelayClientConfig, RelayEvent, RelayClientStatus};
use localtrans_relay::{DataPlane, RelayConfig, RelayServer};
use serial_test::serial;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use quinn;

/// 测试辅助:按 RUST_LOG 初始化 tracing（未设则静默）
fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// 端口偏移 10(避开单测占用的 0-3，且避免 control_port 落入 data_port_range)
/// offset=4 时 control_port=9447 会落在 data_range[9400-9500] 内导致冲突
/// 同时需要足够端口:2 设备租约 + 1 会话端口 = 至少 3 个端口
const PORT_OFFSET: u16 = 10;

#[tokio::test]
#[serial]
async fn two_devices_connect_and_exchange_over_relay() {
    // 初始化日志(方便调试)
    tracing_subscriber::fmt()
        .with_env_filter("localtrans_core=debug,localtrans_relay=debug")
        .try_init()
        .ok();

    eprintln!("[E2E] 起服务端(offset={})", PORT_OFFSET);
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    // 需要端口:2 设备租约 + 1 会话端口 = 至少 3 个端口
    config.data_port_end = config.data_port_start + 3;
    eprintln!("[E2E] 配置: control={}, data_range={}-{}", config.control_port, config.data_port_start, config.data_port_end);
    let server = tokio::time::timeout(
        Duration::from_secs(5),
        RelayServer::bind(config.clone())
    ).await.expect("服务端绑定超时").expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    // 客户端连接地址:服务端绑定 0.0.0.0:PORT,客户端需要连接 127.0.0.1:PORT
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    eprintln!("[E2E] 控制面监听于 {}, 客户端连接于 {}", control_addr, client_connect_addr);

    // 起控制面后台任务
    let server_arc = server.clone();
    tokio::spawn(async move {
        server_arc.run().await;
    });

    // 起数据面
    eprintln!("[E2E] 起数据面");
    let dp = tokio::time::timeout(
        Duration::from_secs(5),
        DataPlane::spawn(&config, server.leases.clone())
    ).await.expect("数据面启动超时").expect("数据面启动失败");
    eprintln!("[E2E] 数据面启动成功");
    let data_addrs = dp.local_addrs();
    eprintln!("[E2E] 数据面监听于 {:?}", data_addrs);

    // 给服务端一点时间完全启动
    tokio::time::sleep(Duration::from_millis(100)).await;

    // === 双设备身份 ===
    eprintln!("[E2E] 创建双设备身份");
    let dir_a = TempDir::new().expect("创建临时目录 A 失败");
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).expect("身份 A 创建失败"));
    let fp_a = id_a.fingerprint();

    let dir_b = TempDir::new().expect("创建临时目录 B 失败");
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).expect("身份 B 创建失败"));
    let fp_b = id_b.fingerprint();

    eprintln!("[E2E] 设备 A 指纹: {}", hex::encode(fp_a));
    eprintln!("[E2E] 设备 B 指纹: {}", hex::encode(fp_b));

    // === 双设备注册 ===
    eprintln!("[E2E] 设备 A 注册中继");
    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备A-实名".into(),
            hidden: false,
        },
        id_a.clone(),
    )
    .await
    .expect("客户端 A 连接失败");

    // M3a FR2:注册成功后客户端应持有服务端回报的公网出口(回环下为 127.0.0.1:客户端源端口)
    let observed_a = client_a.observed_addr().await.expect("注册成功后 observed_addr 应为 Some");
    let observed_a: std::net::SocketAddr = observed_a.parse().expect("observed_addr 应为 ip:port");
    assert!(observed_a.ip().is_loopback(), "回环 E2E 下公网出口应为回环地址, 实得 {}", observed_a);
    assert_ne!(observed_a.port(), 0);
    eprintln!("[E2E] 设备 A 公网出口(observed_addr): {}", observed_a);

    eprintln!("[E2E] 设备 B 注册中继");
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备B-实名".into(),
            hidden: false,
        },
        id_b.clone(),
    )
    .await
    .expect("客户端 B 连接失败");

    // 等名册更新(确保双端都看到了对方)
    eprintln!("[E2E] 等待名册同步");
    let timeout = Duration::from_secs(5);
    let start = tokio::time::Instant::now();

    loop {
        let roster_a = client_a.roster_snapshot().await;
        let has_b = roster_a.iter().any(|d| d.fingerprint == fp_b);
        if has_b {
            eprintln!("[E2E] A 的名册已包含 B");
            break;
        }
        if start.elapsed() > timeout {
            panic!("名册同步超时");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Register 实名断言:名册里对方必须是注册时的 device_name(而非 "LocalTrans")
    let roster_a = client_a.roster_snapshot().await;
    let entry_b = roster_a.iter().find(|d| d.fingerprint == fp_b).expect("名册中应已有 B");
    assert_eq!(entry_b.name, "设备B-实名", "名册应显示 Register 上报的真实设备名");

    // === B 侧先启动等待 PunchIncoming ===
    eprintln!("[E2E] B 侧启动 PunchIncoming 监听");
    let client_b_clone = client_b.clone();
    let b_task = tokio::spawn(async move {
        loop {
            match events_b.recv().await {
                Some(RelayEvent::PunchIncoming { from_fp, session_addr }) => {
                    eprintln!(
                        "[E2E] B 收到 Punch: from={}, session={}",
                        hex::encode(from_fp),
                        session_addr
                    );
                    let conn = client_b_clone
                        .accept_peer(session_addr, from_fp)
                        .await
                        .expect("B accept_peer 失败");
                    eprintln!("[E2E] B 内层连接已建立");
                    break Ok::<_, String>(conn);
                }
                Some(RelayEvent::StatusChanged(s)) => {
                    eprintln!("[E2E] B 状态变更: {:?}", s);
                }
                Some(RelayEvent::RosterUpdated(devices)) => {
                    eprintln!("[E2E] B 名册更新: {} 设备", devices.len());
                }
                None => {
                    return Err("事件通道关闭".into());
                }
                _ => continue,
            }
        }
    });

    // 给 B 的 recv 循环一点时间启动
    tokio::time::sleep(Duration::from_millis(100)).await;

    // === A 侧发起连接 ===
    eprintln!("[E2E] A 发起 connect_peer 到 B");
    eprintln!("[E2E] 目标指纹 B: {}", hex::encode(fp_b));

    // 先等一下确保 B 的事件循环已启动
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(
        Duration::from_secs(30),
        client_a.connect_peer(fp_b),
    )
    .await
    .expect("connect_peer 超时")
    .expect("A connect_peer 失败");
    eprintln!("[E2E] A 内层连接已建立");

    // === 等待 B 侧连接建立 ===
    eprintln!("[E2E] 等待 B 侧连接建立");
    let conn_b = tokio::time::timeout(Duration::from_secs(30), b_task)
        .await
        .expect("B accept_peer 超时") // timeout 返回 Result<Result<Conn, String>, Elapsed>
        .expect("B 任务 panic");      // JoinHandle 返回 Result<Conn, JoinError>
    let conn_b = conn_b.expect("B accept_peer 失败");
    eprintln!("[E2E] 双方内层连接均已建立");

    // === 消息交换验证 ===
    eprintln!("[E2E] A 发送 ControlMsg::SharesReq");
    let (mut send_a, mut recv_a) = conn_a
        .open_bi()
        .await
        .expect("A 打开双向流失败");

    let req_msg = ControlMsg::SharesReq { msg_id: 0 };
    let req_bytes = encode_control(&req_msg);
    send_a
        .write_all(&req_bytes)
        .await
        .expect("A 写入失败");
    send_a.finish().expect("A 完成流失败");
    eprintln!("[E2E] A 已发送 {} 字节", req_bytes.len());

    // B 侧接收
    eprintln!("[E2E] B 接收消息");
    let (mut send_b, mut recv_b) = conn_b
        .accept_bi()
        .await
        .expect("B 接受双向流失败");

    // 读长度前缀
    let mut len_buf = [0u8; 4];
    recv_b.read_exact(&mut len_buf).await.expect("B 读长度失败");
    let msg_len = u32::from_be_bytes(len_buf) as usize;

    // 读消息体
    let mut recv_buf = vec![0u8; msg_len];
    recv_b.read_exact(&mut recv_buf).await.expect("B 读消息体失败");
    eprintln!("[E2E] B 已接收 {} 字节", 4 + msg_len);

    let decoded_req = decode_control(&[&len_buf[..], &recv_buf[..]].concat()).expect("B 解码失败");
    assert_eq!(decoded_req, ControlMsg::SharesReq { msg_id: 0 }, "B 收到的消息不匹配");
    eprintln!("[E2E] 消息匹配: SharesReq");

    // B 侧回复
    eprintln!("[E2E] B 回复 ControlMsg::SharesResp");
    let resp_msg = ControlMsg::SharesResp { shares: vec![], msg_id: 0 };
    let resp_bytes = encode_control(&resp_msg);
    send_b
        .write_all(&resp_bytes)
        .await
        .expect("B 写入失败");
    send_b.finish().expect("B 完成流失败");

    // A 侧接收回复
    eprintln!("[E2E] A 接收回复");
    // 读长度前缀
    let mut resp_len_buf = [0u8; 4];
    recv_a.read_exact(&mut resp_len_buf).await.expect("A 读长度失败");
    let resp_msg_len = u32::from_be_bytes(resp_len_buf) as usize;

    // 读消息体
    let mut resp_buf = vec![0u8; resp_msg_len];
    recv_a.read_exact(&mut resp_buf).await.expect("A 读消息体失败");
    eprintln!("[E2E] A 已接收 {} 字节", 4 + resp_msg_len);

    let decoded_resp = decode_control(&[&resp_len_buf[..], &resp_buf[..]].concat()).expect("A 解码失败");
    assert_eq!(
        decoded_resp,
        ControlMsg::SharesResp { shares: vec![], msg_id: 0 },
        "A 收到的回复不匹配"
    );
    eprintln!("[E2E] 回复匹配: SharesResp");

    eprintln!("[E2E] ========== 测试通过: 中继链路全通 ==========");

    // === 收尾 ===
    eprintln!("[E2E] 清理资源");
    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;

    // 等待清理完成
    tokio::time::sleep(Duration::from_millis(200)).await;

    eprintln!("[E2E] 清理完成");
}

/// 测试 A: 服务器重启恢复（offset 5）
/// 验证客户端在服务器重启后能自动重连并恢复名册
/// NOTE: 在 Windows 上由于 TIME_WAIT 状态，此测试暂时跳过
#[tokio::test]
#[serial]
async fn relay_restart_recovers() {
    // 端口偏移 5（避开主测试的 offset 10）
    const PORT_OFFSET: u16 = 5;

    tracing_subscriber::fmt()
        .with_env_filter("localtrans_core=debug,localtrans_relay=debug")
        .try_init()
        .ok();

    eprintln!("[E2E] 启动服务器 (offset={})", PORT_OFFSET);
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;

    let server = tokio::time::timeout(
        Duration::from_secs(5),
        RelayServer::bind(config.clone())
    ).await.expect("服务端绑定超时").expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();

    // 起控制面后台任务
    let server_arc = server.clone();
    tokio::spawn(async move {
        server_arc.run().await;
    });

    // 起数据面
    eprintln!("[E2E] 启动数据面");
    let dp = tokio::time::timeout(
        Duration::from_secs(5),
        DataPlane::spawn(&config, server.leases.clone())
    ).await.expect("数据面启动超时").expect("数据面启动失败");
    eprintln!("[E2E] 数据面启动成功");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 创建双设备身份
    eprintln!("[E2E] 创建双设备身份");
    let dir_a = TempDir::new().expect("创建临时目录 A 失败");
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).expect("身份 A 创建失败"));
    let fp_a = id_a.fingerprint();

    let dir_b = TempDir::new().expect("创建临时目录 B 失败");
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).expect("身份 B 创建失败"));
    let fp_b = id_b.fingerprint();

    // 注册双客户端
    eprintln!("[E2E] 注册双客户端");
    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备A-实名".into(),
            hidden: false,
        },
        id_a.clone(),
    ).await.expect("客户端 A 连接失败");

    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备B-实名".into(),
            hidden: false,
        },
        id_b.clone(),
    ).await.expect("客户端 B 连接失败");

    // 等待名册同步（确保双端都看到对方）
    eprintln!("[E2E] 等待名册同步");
    let timeout = Duration::from_secs(5);
    let start = tokio::time::Instant::now();

    loop {
        let roster_a = client_a.roster_snapshot().await;
        let has_b = roster_a.iter().any(|d| d.fingerprint == fp_b);
        if has_b {
            eprintln!("[E2E] A 的名册已包含 B");
            break;
        }
        if start.elapsed() > timeout {
            panic!("名册同步超时");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    eprintln!("[E2E] 名册同步完成，暴力关闭服务器");
    // 暴力关闭服务器
    server.shutdown().await;
    dp.shutdown().await;
    eprintln!("[E2E] 服务器已关闭");

    // 等待:①客户端检测断线 ②旧 run 任务退出(accept None → break)释放 Arc/socket
    tokio::time::sleep(Duration::from_secs(6)).await;
    drop(server);

    // 起新服务器（同端口，RelayServer::bind 会生成新证书，但客户端用 PSK 绑定不验证证书）
    eprintln!("[E2E] 启动新服务器 (同端口)");
    let new_server = tokio::time::timeout(
        Duration::from_secs(10),
        RelayServer::bind(config.clone())
    ).await.expect("新服务端绑定超时").expect("新服务端绑定失败");

    // 起新控制面后台任务
    let new_server_arc = new_server.clone();
    tokio::spawn(async move {
        new_server_arc.run().await;
    });

    // 起新数据面
    eprintln!("[E2E] 启动新数据面");
    let new_dp = tokio::time::timeout(
        Duration::from_secs(5),
        DataPlane::spawn(&config, new_server.leases.clone())
    ).await.expect("新数据面启动超时").expect("新数据面启动失败");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 验证客户端自动重连并恢复名册（整个恢复过程 30s 内）
    eprintln!("[E2E] 等待客户端重连并恢复名册");
    let recovery_start = tokio::time::Instant::now();
    let recovery_timeout = Duration::from_secs(30);

    loop {
        // 检查客户端 A 状态
        let status_a = {
            let rx = client_a.subscribe();
            let status = rx.borrow().clone();
            status
        };

        // 检查客户端 A 名册
        let roster_a = client_a.roster_snapshot().await;
        let has_b = roster_a.iter().any(|d| d.fingerprint == fp_b);
        let has_self = roster_a.iter().any(|d| d.fingerprint == fp_a);

        // 恢复条件：状态为 Registered，名册包含 B 但不包含自己（Leave 后自己不在名册）
        if status_a == RelayClientStatus::Registered && has_b && !has_self {
            eprintln!("[E2E] 客户端 A 已恢复：状态={:?}, 名册包含 B", status_a);
            break;
        }

        if recovery_start.elapsed() > recovery_timeout {
            panic!("恢复超时：status={:?}, has_b={}, has_self={}", status_a, has_b, has_self);
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    eprintln!("[E2E] ========== 测试通过：服务器重启恢复成功 ==========");

    // 清理
    client_a.shutdown().await;
    client_b.shutdown().await;
    new_server.shutdown().await;
    new_dp.shutdown().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    eprintln!("[E2E] 清理完成");
}

/// T8: 中继路径下未信任设备同样走同意门+随机码
///
/// 此测试验证:通过中继建立的两个未信任设备,其配对流程与局域网路径完全一致
/// - A 侧发起连接后进入 PairingWaitConsent 状态
/// - B 侧收到 PairingConsentNeeded,同意后生成并显示配对码
/// - A 侧收到 PairingCodeEntry,输入B展示的码
/// - 双方完成配对,TrustStore 互信,SessionUp
#[tokio::test]
#[serial]
async fn relay_pairing_same_flow() {
    use localtrans_core::session::{SessionManager, SessionEvent, SessionCtx};
    use localtrans_core::identity::TrustStore;
    use localtrans_core::store::Config;
    use std::sync::Arc;

    tracing_subscriber::fmt()
        .with_env_filter("localtrans_core=debug,localtrans_relay=debug")
        .try_init()
        .ok();

    eprintln!("[E2E] ========== T8: 中继配对流程测试 ==========");
    eprintln!("[E2E] 起服务端(offset={})", PORT_OFFSET);
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    eprintln!("[E2E] 配置: control={}, data_range={}-{}", config.control_port, config.data_port_start, config.data_port_end);
    let server = tokio::time::timeout(
        Duration::from_secs(5),
        RelayServer::bind(config.clone())
    ).await.expect("服务端绑定超时").expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    eprintln!("[E2E] 控制面监听于 {}, 客户端连接于 {}", control_addr, client_connect_addr);

    // 起控制面后台任务
    let server_arc = server.clone();
    tokio::spawn(async move {
        server_arc.run().await;
    });

    // 起数据面
    eprintln!("[E2E] 起数据面");
    let dp = tokio::time::timeout(
        Duration::from_secs(5),
        DataPlane::spawn(&config, server.leases.clone())
    ).await.expect("数据面启动超时").expect("数据面启动失败");
    eprintln!("[E2E] 数据面启动成功");

    // 给服务端一点时间完全启动
    tokio::time::sleep(Duration::from_millis(100)).await;

    // === 双设备身份(不预置互信) ===
    eprintln!("[E2E] 创建双设备身份(无预互信)");
    let dir_a = TempDir::new().expect("创建临时目录 A 失败");
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).expect("身份 A 创建失败"));
    let fp_a = id_a.fingerprint();

    let dir_b = TempDir::new().expect("创建临时目录 B 失败");
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).expect("身份 B 创建失败"));
    let fp_b = id_b.fingerprint();

    eprintln!("[E2E] 设备 A 指纹: {}", hex::encode(fp_a));
    eprintln!("[E2E] 设备 B 指纹: {}", hex::encode(fp_b));

    // === 创建 SessionManager (保持 SessionCtx 以便后续检查 trust) ===
    let trust_a = Arc::new(tokio::sync::Mutex::new(TrustStore::load(dir_a.path())));
    let config_a = Arc::new(tokio::sync::RwLock::new(Config {
        device_name: "A".to_string(),
        download_dir: dir_a.path().to_path_buf(),
        hidden: false,
        quic_port: 0,
        discovery_port: 0,
        shares: vec![],
        relay_enabled: false,
        relay_server: String::new(),
        relay_psk: String::new(),
        consent_timeout_secs: 60,
        offer_timeout_secs: 60,
        max_active_transfers: 3,
        force_relay_map: std::collections::HashMap::new(),
    }));
    let ctx_a = SessionCtx {
        identity: id_a.clone(),
        trust: trust_a.clone(),
        config: config_a,
    };
    let (sm_a, mut ev_a) = SessionManager::spawn(ctx_a.clone());

    let trust_b = Arc::new(tokio::sync::Mutex::new(TrustStore::load(dir_b.path())));
    let config_b = Arc::new(tokio::sync::RwLock::new(Config {
        device_name: "B".to_string(),
        download_dir: dir_b.path().to_path_buf(),
        hidden: false,
        quic_port: 0,
        discovery_port: 0,
        shares: vec![],
        relay_enabled: false,
        relay_server: String::new(),
        relay_psk: String::new(),
        consent_timeout_secs: 60,
        offer_timeout_secs: 60,
        max_active_transfers: 3,
        force_relay_map: std::collections::HashMap::new(),
    }));
    let ctx_b = SessionCtx {
        identity: id_b.clone(),
        trust: trust_b.clone(),
        config: config_b,
    };
    let (sm_b, mut ev_b) = SessionManager::spawn(ctx_b.clone());

    // === 双设备注册中继 ===
    eprintln!("[E2E] 设备 A 注册中继");
    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备A-实名".into(),
            hidden: false,
        },
        id_a.clone(),
    )
    .await
    .expect("客户端 A 连接失败");

    eprintln!("[E2E] 设备 B 注册中继");
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备B-实名".into(),
            hidden: false,
        },
        id_b.clone(),
    )
    .await
    .expect("客户端 B 连接失败");

    // 等名册更新
    eprintln!("[E2E] 等待名册同步");
    let start = tokio::time::Instant::now();
    loop {
        let roster_a = client_a.roster_snapshot().await;
        let has_b = roster_a.iter().any(|d| d.fingerprint == fp_b);
        if has_b {
            eprintln!("[E2E] A 的名册已包含 B");
            break;
        }
        if start.elapsed() > Duration::from_secs(5) {
            panic!("名册同步超时");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // === B 侧先启动等待 PunchIncoming ===
    eprintln!("[E2E] B 侧启动 PunchIncoming 监听");
    let client_b_clone = client_b.clone();
    let sm_b_clone = sm_b.clone();
    let b_task = tokio::spawn(async move {
        loop {
            match events_b.recv().await {
                Some(RelayEvent::PunchIncoming { from_fp, session_addr }) => {
                    eprintln!(
                        "[E2E] B 收到 Punch: from={}, session={}",
                        hex::encode(from_fp),
                        session_addr
                    );
                    // B 侧接受连接并通过 SessionManager::adopt_connection 进入配对流程
                    let conn = client_b_clone
                        .accept_peer(session_addr, from_fp)
                        .await
                        .expect("B accept_peer 失败");
                    eprintln!("[E2E] B 内层连接已建立,调用 adopt_connection");
                    let result = sm_b_clone.adopt_connection(conn).await;
                    eprintln!("[E2E] B adopt_connection 结果: {:?}", result);
                    break Ok::<_, String>(result);
                }
                Some(RelayEvent::StatusChanged(s)) => {
                    eprintln!("[E2E] B 状态变更: {:?}", s);
                }
                Some(RelayEvent::RosterUpdated(devices)) => {
                    eprintln!("[E2E] B 名册更新: {} 设备", devices.len());
                }
                None => {
                    return Err("事件通道关闭".into());
                }
                _ => continue,
            }
        }
    });

    // 给 B 的 recv 循环一点时间启动
    tokio::time::sleep(Duration::from_millis(100)).await;

    // === 序列1: A 侧发起连接 + adopt_connection ===
    eprintln!("[E2E] ========== 序列1: A connect_peer + adopt_connection ==========");

    // 先等一下确保 B 的事件循环已启动
    tokio::time::sleep(Duration::from_millis(200)).await;

    // A 侧发起连接
    let conn_a = tokio::time::timeout(
        Duration::from_secs(30),
        client_a.connect_peer(fp_b),
    )
    .await
    .expect("connect_peer 超时")
    .expect("A connect_peer 失败");
    eprintln!("[E2E] A 内层连接已建立,调用 adopt_connection");

    // A 侧将连接 adopt 进 SessionManager(主动方语义——A 是发起者)
    let sm_a_clone = sm_a.clone();
    let a_task = tokio::spawn(async move {
        let result = sm_a_clone.adopt_as_initiator(conn_a).await;
        eprintln!("[E2E] A adopt_as_initiator 结果: {:?}", result);
        result
    });

    // === 等待双方 adopt_connection 完成 ===
    eprintln!("[E2E] 等待双方 adopt_connection 完成");
    let a_result = tokio::time::timeout(Duration::from_secs(30), a_task)
        .await
        .expect("A adopt_connection 超时")
        .expect("A 任务 panic");
    let a_result = a_result.expect("A adopt_connection 失败");
    eprintln!("[E2E] A adopt_connection 返回: {:?}", a_result);

    let b_result = tokio::time::timeout(Duration::from_secs(30), b_task)
        .await
        .expect("B adopt_connection 超时")
        .expect("B 任务 panic");
    let b_result = b_result.expect("B adopt_connection 失败");
    eprintln!("[E2E] B adopt_connection 返回: {:?}", b_result);

    // === 断言序列1: A 收 PairingWaitConsent，B 收 PairingConsentNeeded ===
    eprintln!("[E2E] ========== 断言序列1: 门事件 ==========");
    match tokio::time::timeout(Duration::from_secs(5), ev_a.recv())
        .await
        .expect("A 收事件超时")
        .expect("A 事件通道关闭")
    {
        SessionEvent::PairingWaitConsent { fingerprint, .. } => {
            assert_eq!(fingerprint, fp_b, "A 应收到 B 的 WaitConsent");
            eprintln!("[E2E] ✓ A 收到 PairingWaitConsent(fp={})", hex::encode(fingerprint));
        }
        other => panic!("A 应收到 PairingWaitConsent，实际: {:?}", other),
    }

    match tokio::time::timeout(Duration::from_secs(5), ev_b.recv())
        .await
        .expect("B 收事件超时")
        .expect("B 事件通道关闭")
    {
        SessionEvent::PairingConsentNeeded { fingerprint, .. } => {
            assert_eq!(fingerprint, fp_a, "B 应收到 A 的 ConsentNeeded");
            eprintln!("[E2E] ✓ B 收到 PairingConsentNeeded(fp={})", hex::encode(fingerprint));
        }
        other => panic!("B 应收到 PairingConsentNeeded，实际: {:?}", other),
    }

    // === 序列2: B grant_consent → 双方收到码事件 ===
    eprintln!("[E2E] ========== 序列2: B grant_consent 生成码 ==========");
    let code = sm_b.grant_consent(&fp_a).await.expect("B grant_consent 失败");
    eprintln!("[E2E] B 生成的配对码: {}", code);

    // B 收到 PairingCodeShown(含明文码)
    match tokio::time::timeout(Duration::from_secs(5), ev_b.recv())
        .await
        .expect("B 收 CodeShown 超时")
        .expect("B 事件通道关闭")
    {
        SessionEvent::PairingCodeShown { fingerprint, own_code } => {
            assert_eq!(fingerprint, fp_a, "B 的 CodeShown 应针对 A");
            assert_eq!(own_code, code, "CodeShown 应含正确明文码");
            eprintln!("[E2E] ✓ B 收到 PairingCodeShown");
        }
        other => panic!("B 应收到 PairingCodeShown，实际: {:?}", other),
    }

    // A 收到 PairingCodeEntry(可输码状态)
    match tokio::time::timeout(Duration::from_secs(5), ev_a.recv())
        .await
        .expect("A 收 CodeEntry 超时")
        .expect("A 事件通道关闭")
    {
        SessionEvent::PairingCodeEntry { fingerprint, .. } => {
            assert_eq!(fingerprint, fp_b, "A 的 CodeEntry 应针对 B");
            eprintln!("[E2E] ✓ A 收到 PairingCodeEntry(fp={})", hex::encode(fingerprint));
        }
        other => panic!("A 应收到 PairingCodeEntry，实际: {:?}", other),
    }

    // === 序列3: A submit_pair_code → 双方 PairingResult ok + SessionUp ===
    eprintln!("[E2E] ========== 序列3: A submit_pair_code ==========");
    let submit_ok = sm_a.submit_pair_code(&fp_b, &code).await.expect("A submit_pair_code 失败");
    assert!(submit_ok, "A 提交配对码应返回 true");
    eprintln!("[E2E] ✓ A 提交配对码成功");

    // A 收到 PairingResult ok:true 与 SessionUp(到达顺序不限)
    let mut a_got_result = false;
    let mut a_got_up = false;
    let deadline_a = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(a_got_result && a_got_up) {
        let ev = tokio::time::timeout_at(deadline_a, ev_a.recv())
            .await
            .expect("A 收 PairingResult/SessionUp 超时")
            .expect("A 事件通道关闭");
        match ev {
            SessionEvent::PairingResult { fingerprint, ok, reason } => {
                assert_eq!(fingerprint, fp_b, "A 的 PairingResult 应针对 B");
                assert!(ok, "A 的 PairingResult 应成功");
                assert!(reason.is_none(), "A 的 PairingResult 不应有 reason");
                a_got_result = true;
                eprintln!("[E2E] ✓ A 收到 PairingResult ok=true");
            }
            SessionEvent::SessionUp { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_b, "A 的 SessionUp 应针对 B");
                a_got_up = true;
                eprintln!("[E2E] ✓ A 收到 SessionUp(fp={})", hex::encode(fingerprint));
            }
            other => panic!("A 应收到 PairingResult/SessionUp，实际: {:?}", other),
        }
    }

    // B 收到 PairingResult ok:true 与 SessionUp(到达顺序不限)
    let mut b_got_result = false;
    let mut b_got_up = false;
    let deadline_b = tokio::time::Instant::now() + Duration::from_secs(5);
    while !(b_got_result && b_got_up) {
        let ev = tokio::time::timeout_at(deadline_b, ev_b.recv())
            .await
            .expect("B 收 PairingResult/SessionUp 超时")
            .expect("B 事件通道关闭");
        match ev {
            SessionEvent::PairingResult { fingerprint, ok, reason } => {
                assert_eq!(fingerprint, fp_a, "B 的 PairingResult 应针对 A");
                assert!(ok, "B 的 PairingResult 应成功");
                assert!(reason.is_none(), "B 的 PairingResult 不应有 reason");
                b_got_result = true;
                eprintln!("[E2E] ✓ B 收到 PairingResult ok=true");
            }
            SessionEvent::SessionUp { fingerprint, .. } => {
                assert_eq!(fingerprint, fp_a, "B 的 SessionUp 应针对 A");
                b_got_up = true;
                eprintln!("[E2E] ✓ B 收到 SessionUp(fp={})", hex::encode(fingerprint));
            }
            other => panic!("B 应收到 PairingResult/SessionUp，实际: {:?}", other),
        }
    }

    // === 序列4: 双方 TrustStore 互信 ===
    eprintln!("[E2E] ========== 断言序列4: 互信检查 ==========");
    let trust_a = ctx_a.trust.lock().await;
    let trust_b = ctx_b.trust.lock().await;
    assert!(trust_a.is_trusted(&fp_b), "A 应信任 B");
    assert!(trust_b.is_trusted(&fp_a), "B 应信任 A");
    eprintln!("[E2E] ✓ 双方 TrustStore 互信成立");
    drop(trust_a);
    drop(trust_b);

    // === 序列5: 之后一次 SharesReq/SharesResp 经会话 ctrl 流交换成功 ===
    // 【M3b T4 越界修复,缘由】本断言原先用裸 open_bi/accept_bi 交换控制消息;
    // M3b T1+T2 wire 定案(routing/probe.rs)起,会话连接上 ctrl 流之后的
    // bi 流统一归探测响应端 serve_probe_streams 消费——裸 accept_bi 与响应端
    // 抢流,实测 SharesReq 被响应端收走判"首消息非法",本断言 accept_bi 永久
    // 挂起(relay 门禁红;二分定位 06b470f 绿 / 776d2a0 红)。改走产品真实路径
    // send_rpc(出站 ctrl + pending_rpcs 等待者)/入站 ctrl 通道,断言语义不变:
    // 配对完成后会话 ctrl 面可用。
    eprintln!("[E2E] ========== 断言序列5: 控制消息交换 ==========");
    use localtrans_core::protocol::ControlMsg;
    let _ = sm_a.session(&fp_b).await.expect("A 应有 B 的会话");
    let _ = sm_b.session(&fp_a).await.expect("B 应有 A 的会话");
    let mut rx_b = sm_b.take_inbound_ctrl_rx().await.expect("B 入站 ctrl 通道应存在");

    // A 经 send_rpc 发 SharesReq(msg_id 由等待者表分配)
    let (_id, resp_rx) = sm_a
        .send_rpc(&fp_b, ControlMsg::SharesReq { msg_id: 0 })
        .await
        .expect("A 发送 SharesReq 失败");
    eprintln!("[E2E] A 发送 SharesReq");

    // B 经入站 ctrl 通道收 SharesReq
    let (from_a, req) = tokio::time::timeout(Duration::from_secs(5), rx_b.recv())
        .await
        .expect("B 收 SharesReq 超时")
        .expect("B 入站 ctrl 通道关闭");
    assert_eq!(from_a, fp_a, "入站消息应来自 A");
    assert!(
        matches!(req, ControlMsg::SharesReq { .. }),
        "B 应收到 SharesReq,实际 {:?}",
        req
    );
    eprintln!("[E2E] ✓ B 收到 SharesReq");

    // B 原路回复 SharesResp(带回同一 msg_id,A 侧等待者才能命中)
    let resp_msg_id = match req {
        ControlMsg::SharesReq { msg_id } => msg_id,
        _ => unreachable!(),
    };
    sm_b.send_ctrl(&fp_a, ControlMsg::SharesResp { shares: vec![], msg_id: resp_msg_id })
        .await
        .expect("B 回复 SharesResp 失败");
    eprintln!("[E2E] B 回复 SharesResp");

    // A 的等待者收到 SharesResp
    let (_, resp) = tokio::time::timeout(Duration::from_secs(5), resp_rx)
        .await
        .expect("A 收 SharesResp 超时")
        .expect("A RPC 等待者通道关闭");
    assert_eq!(
        resp,
        ControlMsg::SharesResp { shares: vec![], msg_id: resp_msg_id },
        "A 应收到 SharesResp"
    );
    eprintln!("[E2E] ✓ A 收到 SharesResp");

    eprintln!("[E2E] ========== 测试通过: 中继配对流程完整验证 ==========");

    // === 收尾 ===
    eprintln!("[E2E] 清理资源");
    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;

    // 等待清理完成
    tokio::time::sleep(Duration::from_millis(200)).await;

    eprintln!("[E2E] 清理完成");
}

/// 测试 B: 客户端 Leave 通知对端（offset 6）
/// 验证客户端 shutdown() 后对端在 5s 内收到 RosterUpdated 且设备列表消失
/// NOTE: 此测试验证 Leave 机制存在，但实际名册更新可能存在时序问题
#[tokio::test]
#[serial]
async fn client_leave_notifies_peer() {
    // 端口偏移 6（避开其他测试）
    const PORT_OFFSET: u16 = 6;

    tracing_subscriber::fmt()
        .with_env_filter("localtrans_core=debug,localtrans_relay=debug")
        .try_init()
        .ok();

    eprintln!("[E2E] 启动服务器 (offset={})", PORT_OFFSET);
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;

    let server = tokio::time::timeout(
        Duration::from_secs(5),
        RelayServer::bind(config.clone())
    ).await.expect("服务端绑定超时").expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();

    // 起控制面后台任务
    let server_arc = server.clone();
    tokio::spawn(async move {
        server_arc.run().await;
    });

    // 起数据面
    eprintln!("[E2E] 启动数据面");
    let dp = tokio::time::timeout(
        Duration::from_secs(5),
        DataPlane::spawn(&config, server.leases.clone())
    ).await.expect("数据面启动超时").expect("数据面启动失败");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // 创建双设备身份
    eprintln!("[E2E] 创建双设备身份");
    let dir_a = TempDir::new().expect("创建临时目录 A 失败");
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).expect("身份 A 创建失败"));
    let fp_a = id_a.fingerprint();

    let dir_b = TempDir::new().expect("创建临时目录 B 失败");
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).expect("身份 B 创建失败"));
    let fp_b = id_b.fingerprint();

    // 注册双客户端
    eprintln!("[E2E] 注册双客户端");
    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备A-实名".into(),
            hidden: false,
        },
        id_a.clone(),
    ).await.expect("客户端 A 连接失败");

    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig {
            server_addr: client_connect_addr,
            psk: "dev-psk".into(),
            device_name: "设备B-实名".into(),
            hidden: false,
        },
        id_b.clone(),
    ).await.expect("客户端 B 连接失败");

    // 等待名册同步（确保 B 看到 A）
    eprintln!("[E2E] 等待名册同步");
    let timeout = Duration::from_secs(5);
    let start = tokio::time::Instant::now();

    loop {
        let roster_b = client_b.roster_snapshot().await;
        let has_a = roster_b.iter().any(|d| d.fingerprint == fp_a);
        if has_a {
            eprintln!("[E2E] B 的名册已包含 A");
            break;
        }
        if start.elapsed() > timeout {
            panic!("名册同步超时");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    eprintln!("[E2E] 名册同步完成，A 发起 Leave（shutdown）");

    // A 发起 shutdown（会发送 Leave）
    client_a.shutdown().await;
    eprintln!("[E2E] A 已发送 Leave");

    // 等待服务器处理 Leave 并广播名册更新（增加等待时间）
    tokio::time::sleep(Duration::from_secs(2)).await;

    // 验证 B 在 5s 内收到 RosterUpdated 且 A 的指纹消失
    eprintln!("[E2E] 等待 B 收到 RosterUpdated 且 A 消失");
    let notify_start = tokio::time::Instant::now();
    let notify_timeout = Duration::from_secs(5);

    loop {
        // 先检查 roster_snapshot（兜底，更可靠）
        let roster_b = client_b.roster_snapshot().await;
        let has_a = roster_b.iter().any(|d| d.fingerprint == fp_a);
        eprintln!("[E2E] B 的名册快照: {} 设备, 包含 A: {}", roster_b.len(), has_a);

        // 检查 B 是否还连接
        let status_rx = client_b.subscribe();
        let status_b = status_rx.borrow().clone();
        eprintln!("[E2E] B 的状态: {:?}", status_b);
        drop(status_rx);

        if !has_a {
            eprintln!("[E2E] 通过 roster_snapshot 确认 A 已消失");
            break;
        }

        // 同时检查事件队列（事件驱动，更快）
        match tokio::time::timeout(Duration::from_millis(100), events_b.recv()).await {
            Ok(Some(RelayEvent::RosterUpdated(devices))) => {
                eprintln!("[E2E] B 收到 RosterUpdated: {} 设备", devices.len());
                for d in &devices {
                    eprintln!("[E2E]   - 设备: {} ({})", hex::encode(d.fingerprint), d.name);
                }
                let has_a = devices.iter().any(|d| d.fingerprint == fp_a);
                if !has_a {
                    eprintln!("[E2E] A 已从 B 的名册消失");
                    break;
                }
            }
            Ok(Some(RelayEvent::StatusChanged(s))) => {
                eprintln!("[E2E] B 状态变更: {:?}", s);
            }
            Ok(Some(e)) => {
                eprintln!("[E2E] B 收到其他事件: {:?}", std::mem::discriminant(&e));
            }
            Ok(None) => {
                panic!("事件通道关闭");
            }
            Err(_) => {
                // recv 超时，继续循环
            }
        }

        // 超时检查
        if notify_start.elapsed() > notify_timeout {
            let final_roster = client_b.roster_snapshot().await;
            eprintln!("[E2E] 最终名册状态:");
            for d in &final_roster {
                eprintln!("[E2E]   - 设备: {} ({})", hex::encode(d.fingerprint), d.name);
            }
            panic!("Leave 通知超时：A 仍在 B 的名册中");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    eprintln!("[E2E] ========== 测试通过：Leave 通知成功 ==========");

    // 清理
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;

    tokio::time::sleep(Duration::from_millis(200)).await;
    eprintln!("[E2E] 清理完成");
}

/// 推送确认超时原因端到端测试（Task 6）
/// 验证:通过 Ask 档推送,对方超时未确认,发送方收到 EngineError::OfferTimeout(而非泛化 error)
#[tokio::test]
#[serial]
async fn push_ask_timeout_reason_reaches_sender() {
    use localtrans_core::identity::PushPolicy;
    use localtrans_core::transfer::{push_files, spawn_rpc_router, EngineError, ProgressEvent};
    use localtrans_core::share::ShareRegistry;
    use localtrans_core::transfer::sender_state::new_sender_job_map;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{timeout, Duration};

    localtrans_core::test_support::init_tracing();

    eprintln!("[E2E] ========== 推送确认超时测试 ==========");

    // 复用 test_support 脚手架(与 core engine.rs 测试同模式)
    let (sm_a, _ev_a, ctx_a, fp_a, _dir_a) = localtrans_core::test_support::setup_ctx("甲");
    let (sm_b, _ev_b, ctx_b, fp_b, _dir_b) = localtrans_core::test_support::setup_ctx("乙");

    eprintln!("[E2E] 设备 A 指纹: {}", hex::encode(fp_a));
    eprintln!("[E2E] 设备 B 指纹: {}", hex::encode(fp_b));

    // 差异 1:乙 Ask 档
    {
        use localtrans_core::identity::{TrustedPeer, Perms};
        let mut trust = ctx_b.trust.lock().await;
        trust.upsert(TrustedPeer {
            fingerprint: fp_a,
            name: "甲".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Ask },
        });
        trust.save().unwrap();
    }

    // 差异 2:1s 确认超时
    ctx_b.config.write().await.offer_timeout_secs = 1;

    // 甲侧信任乙(Auto)
    {
        use localtrans_core::identity::{TrustedPeer, Perms};
        let mut trust = ctx_a.trust.lock().await;
        trust.upsert(TrustedPeer {
            fingerprint: fp_b,
            name: "乙".to_string(),
            alias: String::new(),
            paired_at: 1000,
            perms: Perms { browse: true, download: true, push: PushPolicy::Auto },
        });
        trust.save().unwrap();
    }

    // 创建测试文件
    let src = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(src.path(), b"ask-timeout").unwrap();
    eprintln!("[E2E] 创建测试文件: {:?}", src.path());

    // B 侧启动监听
    let b_addr = localtrans_core::test_support::start_listener(&sm_b).await;
    eprintln!("[E2E] B 监听于: {}", b_addr);

    // B 侧启动 RPC 路由(ask 通道开但不消费——永不响应)
    let ctrl_rx_b = sm_b.take_inbound_ctrl_rx().await.unwrap();
    let (ask_tx_b, _ask_rx_b) = mpsc::channel::<localtrans_core::transfer::OfferAsk>(8);
    spawn_rpc_router(
        sm_b.clone(),
        ctx_b.clone(),
        Arc::new(ShareRegistry::new(vec![])),
        ctrl_rx_b,
        ask_tx_b,
        mpsc::channel(8).0, // 删除确认占位(e2e 不涉及删除)
        new_sender_job_map(),
        None,
    );

    // A 侧启动 RPC 路由
    let ctrl_rx_a = sm_a.take_inbound_ctrl_rx().await.unwrap();
    let (ask_tx_a, _ask_rx_a) = mpsc::channel::<localtrans_core::transfer::OfferAsk>(8);
    spawn_rpc_router(
        sm_a.clone(),
        ctx_a.clone(),
        Arc::new(ShareRegistry::new(vec![])),
        ctrl_rx_a,
        ask_tx_a,
        mpsc::channel(8).0, // 删除确认占位(e2e 不涉及删除)
        new_sender_job_map(),
        None,
    );

    // A 连接 B
    timeout(Duration::from_secs(5), sm_a.connect(b_addr))
        .await
        .unwrap()
        .unwrap();
    eprintln!("[E2E] A 连接 B 成功");

    // 核心断言:OfferTimeout(而非 OfferRejected/Timeout 泛化)
    let (progress_tx, _progress_rx) = mpsc::channel::<ProgressEvent>(8);
    let res = timeout(
        Duration::from_secs(15),
        push_files(&sm_a, &fp_b, vec![src.path().to_path_buf()], &localtrans_core::transfer::sender_state::new_sender_job_map(), progress_tx),
    )
    .await
    .unwrap();

    eprintln!("[E2E] 推送结果: {:?}", res);
    assert!(
        matches!(res, Err(EngineError::OfferTimeout)),
        "实际: {:?}",
        res.err()
    );

    eprintln!("[E2E] ========== 测试通过: OfferTimeout 正确传播 ==========");
}

/// 稳定性 T1:keep-alive 互保——应用层静默 20s,连接必须仍活。
/// 20s > idle 15s:若心跳未生效,15s 即判死;心跳在工作则连接永活。
/// (v0.9.1 修复前:client_endpoint 无 TransportConfig,idle 30s 默认,
///  此测试恰好也能过——真正区分度在 T2 的 15s 判死,本测试锁行为防回归)
#[tokio::test]
#[serial]
async fn keepalive_keeps_idle_connection_alive() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into(), hidden: false },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into(), hidden: false },
        id_b.clone(),
    ).await.unwrap();

    // 等名册互见
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // B 侧 accept 后持有连接(存入 oneshot 供测试观察)
    let (b_conn_tx, b_conn_rx) = tokio::sync::oneshot::channel::<quinn::Connection>();
    let client_b_clone = client_b.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { from_fp, session_addr } = ev {
                if let Ok(conn) = client_b_clone.accept_peer(session_addr, from_fp).await {
                    let _ = b_conn_tx.send(conn);
                    break;
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("connect_peer 超时").expect("A connect_peer 失败");
    let conn_b = tokio::time::timeout(Duration::from_secs(30), b_conn_rx)
        .await.expect("B accept 超时").expect("B accept 失败");

    // === 应用层静默 20s(不发送任何数据)===
    tokio::time::sleep(Duration::from_secs(20)).await;

    // 断言双向仍活:能开流 + 对端能收流
    let (mut send_a, mut recv_a) = conn_a.open_bi().await.expect("静默 20s 后 A 开流失败——连接被判死");
    send_a.write_all(b"alive").await.unwrap();
    send_a.finish().unwrap();
    let mut buf = [0u8; 5];
    let (mut send_b, mut recv_b) = conn_b.accept_bi().await.expect("B accept_bi 失败");
    recv_b.read_exact(&mut buf).await.unwrap();
    let _ = send_b;
    let _ = recv_a;
    assert_eq!(&buf[..5], b"alive");

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;
}

/// 稳定性 T2:断路 15s 判死——DataPlane 停转(转发停)后,
/// 连接在 30s 内 closed()。修复前 server 侧 idle 60s → 30s 内不 closed → 红。
#[tokio::test]
#[serial]
async fn idle_timeout_kills_dead_path_in_15s() {
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    config.data_port_end = config.data_port_start + 3;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into(), hidden: false },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into(), hidden: false },
        id_b.clone(),
    ).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (b_conn_tx, _b_conn_rx) = tokio::sync::oneshot::channel::<quinn::Connection>();
    let client_b_clone = client_b.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { from_fp, session_addr } = ev {
                if let Ok(conn) = client_b_clone.accept_peer(session_addr, from_fp).await {
                    let _ = b_conn_tx.send(conn);
                    break;
                }
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("connect_peer 超时").expect("A connect_peer 失败");

    // === 断路:停数据面(心跳包发出但不再被转发 → 双向静默)===
    dp.shutdown().await;

    // idle 15s + 余量 → 30s 内必须 closed;旧配置 60s idle 会超时 → 红
    tokio::time::timeout(Duration::from_secs(30), conn_a.closed())
        .await
        .expect("30s 内连接未判死——idle 超时未收紧到 15s");

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
}

/// 稳定性 T3(一测覆盖三模块):对端连接死亡 → 15s 判死 → autoheal 重连 →
/// 新连接可交换消息。
/// 覆盖:Task 2 的 15s 判死 + Task 4 的重连编排 + Task 1 的重学习
/// (重连后新 KNOCK 地址学习)。
///
/// 设计说明:不做"中继整体重启"——Windows 上旧 server/数据面的 socket
/// 释放时序不可控(SO_REUSEADDR 双绑后入包随机落入已停实例的空状态,
/// 实测连续踩坑),而自愈的对象是**会话**(spec 回路 1 的触发即对端
/// SessionDown),对端连接死亡场景更贴近生产真实故障:手机切网/NAT 重绑定。
/// 中继全程存活,KNOCK 重学习由 Task 1 的漂移跟踪保障。
#[tokio::test]
#[serial]
async fn autoheal_after_data_plane_outage() {
    init_tracing();
    let mut config = RelayConfig::for_test_with_port_offset(PORT_OFFSET);
    // 端口池要 6 个:2 设备租约 + 断路判死的旧会话端口(GOODBYE 发不出,
    // 不可回收,只能靠池容量兜底)+ 自愈重连的新会话端口×2 + 冗余 1
    config.data_port_end = config.data_port_start + 6;
    let server = RelayServer::bind(config.clone()).await.expect("服务端绑定失败");
    let control_addr = server.local_control_addr();
    let client_connect_addr = format!("127.0.0.1:{}", control_addr.port()).parse().unwrap();
    let server_arc = server.clone();
    tokio::spawn(async move { server_arc.run().await });
    let dp = DataPlane::spawn(&config, server.leases.clone()).await.expect("数据面启动失败");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let dir_a = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let dir_b = TempDir::new().unwrap();
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let fp_b = id_b.fingerprint();

    let (client_a, _events_a) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "A".into(), hidden: false },
        id_a.clone(),
    ).await.unwrap();
    let (client_b, mut events_b) = RelayClient::connect(
        RelayClientConfig { server_addr: client_connect_addr, psk: "dev-psk".into(), device_name: "B".into(), hidden: false },
        id_b.clone(),
    ).await.unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if client_a.roster_snapshot().await.iter().any(|d| d.fingerprint == fp_b) { break; }
        assert!(tokio::time::Instant::now() < deadline, "名册同步超时");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // B 侧 accept 循环(accept_peer spawn 并行:串行会阻塞事件泵,
    // 自愈场景的二次 PunchNotif 会卡在上一轮 accept 超时后面错过 A 的握手窗口)
    let client_b_clone = client_b.clone();
    let b_conns = Arc::new(tokio::sync::Mutex::new(Vec::<quinn::Connection>::new()));
    let b_conns_clone = b_conns.clone();
    tokio::spawn(async move {
        while let Some(ev) = events_b.recv().await {
            if let RelayEvent::PunchIncoming { from_fp, session_addr } = ev {
                let cb = client_b_clone.clone();
                let conns = b_conns_clone.clone();
                tokio::spawn(async move {
                    if let Ok(conn) = cb.accept_peer(session_addr, from_fp).await {
                        conns.lock().await.push(conn);
                    }
                });
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;

    let conn_a = tokio::time::timeout(Duration::from_secs(30), client_a.connect_peer(fp_b))
        .await.expect("首次 connect_peer 超时").expect("首次 connect_peer 失败");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // === 故障注入:对端连接死亡(drop B 持有的全部内层连接,
    // 模拟手机切网/NAT 重绑定——QUIC 连接对象销毁,路径即断)===
    b_conns.lock().await.clear();
    tokio::time::timeout(Duration::from_secs(30), conn_a.closed())
        .await.expect("30s 内连接未判死");

    // === 自愈:A 侧重连(B 侧 accept 循环自动接住)===
    let conn_a2 = tokio::time::timeout(Duration::from_secs(45), async {
        loop {
            if let Some(conn) = localtrans_core::relay::autoheal::reconnect_peer_conn(&client_a, fp_b).await {
                return conn;
            }
            // reconnect_peer_conn 内部 3 败即弃;外层兜底再等对端稳定
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }).await.expect("自愈重连超时");

    // === 新连接交换消息(证明全链路恢复 + KNOCK 重学习生效)===
    let (mut send_a2, mut recv_a2) = conn_a2.open_bi().await.expect("新连接开流失败");
    send_a2.write_all(b"revived").await.unwrap();
    send_a2.finish().unwrap();
    let got = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let mut conns = b_conns.lock().await;
            if let Some(conn) = conns.last() {
                if let Ok((mut send_b, mut recv_b)) = conn.accept_bi().await {
                    let mut buf = [0u8; 7];   // "revived" 恰 7B——多读会 FinishedEarly
                    recv_b.read_exact(&mut buf).await.map_err(|e| format!("{}", e))?;
                    let _ = send_b;
                    return Ok::<_, String>(buf.to_vec());
                }
            }
            drop(conns);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }).await.expect("B 侧收消息超时").expect("B 侧读取失败");
    assert_eq!(&got[..7], b"revived");
    let _ = recv_a2;

    client_a.shutdown().await;
    client_b.shutdown().await;
    server.shutdown().await;
    dp.shutdown().await;
}
