// 下载链路探针（诊断工具）
//
// 用法: cargo run --release -p localtrans-core --example pull_probe -- <对端IP> <身份目录> [下载目录]
//
// 用指定目录里的真实身份（identity.key + trusted_peers.json）连到对端，
// 完整复现 UI 点"下载"后的调用链：
//   connect → SharesReq → ListReq(根目录) → start_pull(第一个文件)
// 逐步打印结果——用于定位"点下载没反应"到底断在哪一层:
//   - connect 失败        → 对端没开 / 证书不被信任
//   - SharesReq 超时      → 控制流/响应路由问题
//   - MetaReq 超时        → 对端权限拒绝或路由器没回 MetaResp
//   - start_pull 报错     → 错误信息直接可见（UI 里只走 toast，容易被忽略）

use std::net::SocketAddr;
use std::sync::Arc;

use localtrans_core::protocol::{ControlMsg, ShareInfo};
use localtrans_core::session::{SessionCtx, SessionManager};
use localtrans_core::share::ShareRegistry;
use localtrans_core::transfer::{self, ProgressEvent};
use tokio::sync::{mpsc, Mutex, RwLock};

fn main() {
    let target_ip = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("用法: pull_probe <对端IP> <身份目录> [下载目录]");
        std::process::exit(2);
    });
    let data_dir = std::env::args().nth(2).unwrap_or_else(|| {
        eprintln!("用法: pull_probe <对端IP> <身份目录> [下载目录]");
        std::process::exit(2);
    });
    let download_dir = std::env::args()
        .nth(3)
        .unwrap_or_else(|| "pull_probe_downloads".to_string());

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio 运行时创建失败");
    rt.block_on(async_main(target_ip, data_dir, download_dir));
}

async fn async_main(target_ip: String, data_dir: String, download_dir: String) {
    // 与应用同构的日志输出，方便对照应用日志
    let log_dir = std::path::Path::new(&data_dir).join("logs");
    let _ = std::fs::create_dir_all(&log_dir);
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(std::io::stderr)
        .init();

    let dir = std::path::PathBuf::from(&data_dir);
    let identity = Arc::new(localtrans_core::identity::Identity::load_or_create(&dir)
        .expect("身份加载失败"));
    println!("[探针] 本机指纹: {}", hex::encode(identity.fingerprint()));

    let trust = Arc::new(Mutex::new(localtrans_core::identity::TrustStore::load(&dir)));
    let mut cfg = localtrans_core::store::load_config(&dir);
    cfg.download_dir = std::path::PathBuf::from(&download_dir);
    let cfg = Arc::new(RwLock::new(cfg));
    println!("[探针] 下载目录: {}", download_dir);

    let ctx = SessionCtx { identity: identity.clone(), trust, config: cfg.clone() };
    let (sm, _events) = SessionManager::spawn(ctx);

    let addr: SocketAddr = format!("{}:{}", target_ip, localtrans_core::ports::quic_port())
        .parse().expect("对端地址无效");
    println!("[探针] 连接 {}", addr);
    let peer = sm.connect(addr).await.expect("连接失败");
    println!("[探针] 已连接，对端指纹: {}", hex::encode(peer));

    // 1. SharesReq（与 list_shares_remote 完全一致的模式）
    let shares = request_shares(&sm, &peer).await.expect("获取共享区失败");
    println!("[探针] 共享区: {:?}", shares.iter().map(|s: &ShareInfo| s.alias.clone()).collect::<Vec<_>>());
    if shares.is_empty() {
        eprintln!("[探针] 对端没有共享区，结束");
        return;
    }
    let share = shares.into_iter().next().unwrap();
    println!("[探针] 使用共享区: id={} alias={}", share.id, share.alias);

    // 2. ListReq 根目录
    let entries = request_list(&sm, &peer, &share.id, "").await.expect("列目录失败");
    println!("[探针] 根目录 {} 项", entries.len());
    let file = match entries.into_iter().find(|e| !e.is_dir) {
        Some(f) => f,
        None => {
            eprintln!("[探针] 根目录没有文件，结束");
            return;
        }
    };
    println!("[探针] 拉取文件: {} ({} 字节)", file.name, file.size);

    // 3. start_pull（与 start_download 命令一致）
    let reg = ShareRegistry::new(vec![]);
    let (tx, mut rx) = mpsc::channel::<ProgressEvent>(64);
    let sm_pull = sm.clone();
    let peer_pull = peer;
    let share_id = share.id.clone();
    let fname = file.name.clone();
    let cfg_pull = cfg.read().await.clone();
    let handle = tokio::spawn(async move {
        transfer::start_pull(&sm_pull, &reg, &peer_pull, &share_id, &fname, &cfg_pull, tx).await
    });

    while let Some(ev) = rx.recv().await {
        match ev {
            ProgressEvent::Started { job_id, name, total } =>
                println!("[进度] 开始 job={} name={} total={}", job_id, name, total),
            ProgressEvent::ChunkDone { job_id, chunk, bytes } =>
                println!("[进度] 块完成 job={} chunk={} bytes={}", job_id, chunk, bytes),
            ProgressEvent::Speed { job_id, bps } =>
                println!("[进度] 速度 job={} {} B/s", job_id, bps),
            ProgressEvent::Done { job_id } =>
                println!("[进度] 完成 job={}", job_id),
            ProgressEvent::Failed { job_id, reason } => {
                println!("[进度] 失败 job={} reason={}", job_id, reason);
                break;
            }
            _ => {}
        }
    }

    match handle.await {
        Ok(Ok(job_id)) => println!("[探针] 拉取成功 job_id={}，下载目录: {}", job_id, download_dir),
        Ok(Err(e)) => eprintln!("[探针] 拉取失败: {:?}", e),
        Err(e) => eprintln!("[探针] 任务 panic: {}", e),
    }
}

async fn request_shares(sm: &SessionManager, peer: &[u8; 32]) -> Option<Vec<ShareInfo>> {
    let (msg_id, resp_rx) = sm.send_rpc(peer, ControlMsg::SharesReq { msg_id: 0 }).await.ok()?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        match resp_rx.await {
            Ok((_, ControlMsg::SharesResp { shares, .. })) => Some(shares),
            _ => None,
        }
    }).await;
    if !matches!(out, Ok(Some(_))) {
        sm.cancel_rpc(msg_id).await;
    }
    out.ok().flatten()
}

async fn request_list(
    sm: &SessionManager,
    peer: &[u8; 32],
    share_id: &str,
    path: &str,
) -> Option<Vec<localtrans_core::protocol::FileEntry>> {
    let (msg_id, resp_rx) = sm.send_rpc(peer, ControlMsg::ListReq {
        share_id: share_id.to_string(),
        path: path.to_string(),
        cursor: 0,
        msg_id: 0,
    }).await.ok()?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        match resp_rx.await {
            Ok((_, ControlMsg::ListResp { entries, .. })) => Some(entries),
            _ => None,
        }
    }).await;
    if !matches!(out, Ok(Some(_))) {
        sm.cancel_rpc(msg_id).await;
    }
    out.ok().flatten()
}
