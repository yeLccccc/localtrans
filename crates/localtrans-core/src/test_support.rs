// Test support module (T14: 提升自 session.rs 测试模块)

use crate::identity::{Identity, TrustStore};
use crate::session::{SessionManager, SessionCtx};
use crate::store::Config;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::{mpsc, Mutex};

/// 测试辅助：设置测试上下文（返回 TempDir，调用方须持有到测试结束）
pub fn setup_ctx(name: &str) -> (Arc<SessionManager>, mpsc::Receiver<crate::session::SessionEvent>, SessionCtx, crate::identity::Fingerprint, TempDir) {
    let dir = TempDir::new().unwrap();
    let identity = Arc::new(Identity::load_or_create(dir.path()).unwrap());
    let fp = identity.fingerprint();
    let trust = Arc::new(Mutex::new(TrustStore::load(dir.path())));
    let config = Arc::new(tokio::sync::RwLock::new(Config {
        device_name: name.to_string(),
        download_dir: dir.path().to_path_buf(),
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

    let ctx = SessionCtx { identity, trust, config };
    let (sm, ev_rx) = SessionManager::spawn(ctx.clone());
    (sm, ev_rx, ctx, fp, dir)
}

/// 测试辅助：随机端口启动监听器并返回实际绑定地址（避免并行测试端口冲突）
pub async fn start_listener(sm: &Arc<SessionManager>) -> std::net::SocketAddr {
    sm.start_listener(0).await.unwrap()
}

/// 测试辅助：按 RUST_LOG 初始化 tracing（未设则静默）
pub fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}

/// 测试辅助：设置测试上下文（可自定义门超时秒数，Task 3 T4 用）
pub fn setup_ctx_with_timeout(name: &str, consent_timeout_secs: u64) -> (Arc<SessionManager>, mpsc::Receiver<crate::session::SessionEvent>, SessionCtx, crate::identity::Fingerprint, TempDir) {
    let dir = TempDir::new().unwrap();
    let identity = Arc::new(Identity::load_or_create(dir.path()).unwrap());
    let fp = identity.fingerprint();
    let trust = Arc::new(Mutex::new(TrustStore::load(dir.path())));
    let config = Arc::new(tokio::sync::RwLock::new(Config {
        device_name: name.to_string(),
        download_dir: dir.path().to_path_buf(),
        hidden: false,
        quic_port: 0,
        discovery_port: 0,
        shares: vec![],
        relay_enabled: false,
        relay_server: String::new(),
        relay_psk: String::new(),
        consent_timeout_secs,
        offer_timeout_secs: 60,
        max_active_transfers: 3,
        force_relay_map: std::collections::HashMap::new(),
    }));

    let ctx = SessionCtx { identity, trust, config };
    let (sm, ev_rx) = SessionManager::spawn(ctx.clone());
    (sm, ev_rx, ctx, fp, dir)
}
