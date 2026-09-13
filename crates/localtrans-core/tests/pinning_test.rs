// Fingerprint pinning tests for S1 audit fix
// Tests that expected fingerprint validation is enforced on all three connection paths:
// 1. Relay inner handshake, client side (connect_peer → client_builder(Some(fp)))
// 2. Relay inner handshake, server side (accept_peer → PinnedClientCertVerifier)
// 3. LAN direct connection (SessionManager::connect_pinned)

use localtrans_core::identity::Identity;
use localtrans_core::test_support::init_tracing;
use std::sync::Arc;
use tempfile::TempDir;

/// Client-side rejection: session::connect with wrong expected fingerprint fails.
/// This is the verification path used by virtual_ep::client_endpoint (relay
/// connect_peer) and SessionManager::connect_pinned (LAN).
///
/// Scenario: A connects to M's endpoint while expecting B's fingerprint.
#[tokio::test]
async fn relay_inner_handshake_rejects_wrong_fingerprint() {
    init_tracing();

    // Create three identities: A (client), B (expected target), M (mitm)
    let dir_a = TempDir::new().unwrap();
    let dir_b = TempDir::new().unwrap();
    let dir_m = TempDir::new().unwrap();

    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let id_b = Arc::new(Identity::load_or_create(dir_b.path()).unwrap());
    let id_m = Arc::new(Identity::load_or_create(dir_m.path()).unwrap());

    let fp_b = id_b.fingerprint();
    let _fp_m = id_m.fingerprint();

    // This minimal test verifies that session::connect with wrong expected fingerprint fails.
    // The full relay topology (connect_peer via VirtualUdp) requires a running relay server
    // and punch coordination. For v0.11.0, we test the core verification logic first.

    // Test that session::connect with wrong expected fingerprint fails
    use localtrans_core::session;
    let ep_a = session::bind_endpoint(0, &id_a).unwrap();
    let ep_m = session::bind_endpoint(0, &id_m).unwrap();

    let m_port = ep_m.local_addr().unwrap().port();
    let m_addr = std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), m_port);

    // Spawn server task with M's identity (pretending to be B)
    tokio::spawn(async move {
        if let Some(incoming) = ep_m.accept().await {
            let _ = incoming.await;
        }
    });

    // Try to connect to M's endpoint while expecting B's fingerprint
    let result = session::connect(&ep_a, m_addr, &id_a, Some(fp_b)).await;

    // Should fail with fingerprint mismatch error
    assert!(result.is_err(), "connect with wrong expected fingerprint should fail");

    let err = result.unwrap_err();
    let err_msg = err.to_string();
    assert!(
        err_msg.contains("指纹") || err_msg.to_lowercase().contains("fingerprint"),
        "Error should mention fingerprint mismatch: {}",
        err_msg
    );
}

/// Server-side rejection: server endpoint built with server_config_pinned (the
/// config accept_peer now uses) rejects a client whose certificate fingerprint
/// does not match the pinned expectation — the TLS handshake itself fails.
///
/// Scenario: B's endpoint is pinned to expect A's fingerprint, but M connects.
#[tokio::test]
async fn pinned_server_endpoint_rejects_wrong_client_fingerprint() {
    init_tracing();

    use localtrans_core::session;
    use std::time::Duration;

    let dir_a = TempDir::new().unwrap();
    let dir_m = TempDir::new().unwrap();
    let id_a = Arc::new(Identity::load_or_create(dir_a.path()).unwrap());
    let id_m = Arc::new(Identity::load_or_create(dir_m.path()).unwrap());

    let fp_a = id_a.fingerprint();

    // Server endpoint pinned to A's fingerprint (as accept_peer does with PunchNotif.from_fp).
    // quinn requires accept().await to drive the TLS handshake, so spawn an accept loop
    // that records whether each incoming completed the handshake.
    let server_cfg = session::server_config_pinned(&id_a, fp_a).unwrap();
    let ep_b = quinn::Endpoint::server(server_cfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let b_port = ep_b.local_addr().unwrap().port();
    let b_addr = std::net::SocketAddr::new("127.0.0.1".parse().unwrap(), b_port);

    let (result_tx, mut result_rx) = tokio::sync::mpsc::unbounded_channel::<bool>();
    tokio::spawn(async move {
        while let Some(incoming) = ep_b.accept().await {
            let tx = result_tx.clone();
            tokio::spawn(async move {
                // true = handshake completed, false = rejected during handshake
                let _ = tx.send(incoming.await.is_ok());
            });
        }
    });

    // M connects with its own identity — fingerprint does not match the pin.
    // Drive both sides concurrently; the server-side accept is the real verdict.
    let m_connect = tokio::spawn({
        let id_m = id_m.clone();
        async move {
            let ep_m = session::bind_endpoint(0, &id_m).unwrap();
            let _ = session::connect(&ep_m, b_addr, &id_m, None).await;
        }
    });

    let server_verdict = tokio::time::timeout(Duration::from_secs(15), result_rx.recv()).await
        .expect("server should deliver a verdict")
        .expect("accept loop alive");
    assert!(
        !server_verdict,
        "pinned server endpoint must reject M's handshake (fingerprint mismatch)"
    );
    let _ = m_connect.await;

    // Control: same pinned endpoint accepts the genuinely expected client (A)
    let a_connect = tokio::spawn({
        let id_a = id_a.clone();
        async move {
            let ep_a = session::bind_endpoint(0, &id_a).unwrap();
            session::connect(&ep_a, b_addr, &id_a, None).await.is_ok()
        }
    });
    let (server_verdict, client_ok) = tokio::time::timeout(Duration::from_secs(15), async {
        let v = result_rx.recv().await.expect("accept loop alive");
        let c = a_connect.await.unwrap();
        (v, c)
    }).await.expect("control case should complete in time");

    assert!(server_verdict, "pinned endpoint must accept the expected client (server side)");
    assert!(client_ok, "pinned endpoint must accept the expected client (client side)");
}

/// LAN production path: SessionManager::connect_pinned with a wrong expected
/// fingerprint returns Err mentioning the mismatch.
#[tokio::test]
async fn connect_pinned_with_wrong_fingerprint_returns_err() {
    init_tracing();

    use localtrans_core::test_support::{setup_ctx, start_listener};

    let (sm_a, _ev_a, _ctx_a, _fp_a, _dir_a) = setup_ctx("甲");
    let (sm_b, _ev_b, _ctx_b, _fp_b, _dir_b) = setup_ctx("乙");

    let b_addr = start_listener(&sm_b).await;

    // Wrong expected fingerprint: connect_pinned must fail at the TLS handshake
    let wrong_fp = [9u8; 32];
    let result = sm_a.connect_pinned(b_addr, wrong_fp).await;
    assert!(result.is_err(), "connect_pinned with wrong fingerprint must fail");

    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("指纹") || err_msg.to_lowercase().contains("fingerprint"),
        "Error should mention fingerprint mismatch: {}",
        err_msg
    );
}
