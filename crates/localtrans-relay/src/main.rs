//! relay 服务端入口:读配置 → 起控制面+数据面 → 常驻。

use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info".into()),
        )
        .init();

    let config_path = std::env::args().nth(1)
        .unwrap_or_else(|| "/etc/localtrans-relay.toml".into());
    let config = localtrans_relay::RelayConfig::load(std::path::Path::new(&config_path))?;

    tracing::info!(
        "中继启动: 控制面 :{} (udp), 数据面 {}-{} (udp), 公网IP {}",
        config.control_port, config.data_port_start, config.data_port_end, config.public_ip
    );
    tracing::info!("psk 摘要: {} (sha256 前 16 位, 供核对)", localtrans_relay::RelayConfig::psk_digest(&config.psk));

    let server = localtrans_relay::RelayServer::bind(config.clone()).await?;
    let data_plane = localtrans_relay::DataPlane::spawn(&config, server.leases.clone()).await?;

    let server_clone = server.clone();
    tokio::spawn(async move { server_clone.run().await });
    // 数据面循环已在 spawn 内启动;主任务挂起等待信号
    tokio::signal::ctrl_c().await.map_err(|e| e.to_string())?;
    tracing::info!("收到退出信号");
    server.shutdown().await;
    data_plane.shutdown().await;
    Ok(())
}
