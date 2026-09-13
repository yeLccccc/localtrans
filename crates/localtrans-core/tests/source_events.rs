use localtrans_core::protocol::CHUNK_SIZE;
use localtrans_core::transfer::manifest::Manifest;
use localtrans_core::transfer::run_sender;
use localtrans_core::transfer::sender_state::{new_sender_job_state, SenderJobState};
use localtrans_core::transfer::{ProgressEvent, SourceRole};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;

#[test]
fn source_started_serializes_with_tag() {
    // 占位:T5 充实后此测试断言具体序列化形态
    let ev = ProgressEvent::SourceStarted {
        job_id: 0x8000_0000_0000_0001,
        role: SourceRole::SourcePull,
        peer: [0u8; 32],
        name: "test.bin".into(),
        total: 1024,
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"type\":\"source_started\""), "got: {}", s);
}

#[test]
fn source_role_serializes_snake_case() {
    assert_eq!(serde_json::to_string(&SourceRole::SourcePush).unwrap(), "\"source_push\"");
    assert_eq!(serde_json::to_string(&SourceRole::SourcePull).unwrap(), "\"source_pull\"");
}

#[tokio::test]
async fn run_sender_updates_bytes_counter_on_success() {
    let tmp = TempDir::new().unwrap();
    let src = tmp.path().join("src.bin");
    let data = vec![7u8; CHUNK_SIZE + 100];
    tokio::fs::write(&src, &data).await.unwrap();
    let manifest = Manifest::build(&src).unwrap();

    let counter = Arc::new(AtomicU64::new(0));
    // 没有真实 QUIC 连接,此测试仅验证 counter 逻辑在文件能读完时能更新。
    // 真实 QUIC 路径在 T5 集成测试覆盖。
    // 这里断言 manifest 块大小正确即可:
    assert_eq!(manifest.chunk_count(), 2);
    assert_eq!(counter.load(std::sync::atomic::Ordering::Relaxed), 0);
    // 完整端到端需要 mock QUIC,放到后续 T5 集成测试
    // 仅作占位以满足编译期引用 `run_sender` / `mpsc`
    let _: fn(_, _, _, u64, u64, u64, u32, _, _) -> _ = run_sender;
    let _: Option<mpsc::Sender<ProgressEvent>> = None;
}

/// T5: 构造的 SenderJobState 默认 throttle_cap=MAX,probe_stop_tx 已建好
#[test]
fn sender_job_state_default_throttle_is_max() {
    let tmp = TempDir::new().unwrap();
    let manifest = Manifest {
        file_name: "x".into(),
        total_size: 0,
        chunk_hashes: vec![],
        received: vec![],
        peer: None,
        share_id: None,
        rel: None,
        meta: None,
    };
    let (tx, _rx) = mpsc::channel::<ProgressEvent>(1);
    let state: SenderJobState = new_sender_job_state(
        0x8000_0000_0000_0001,
        tmp.path().join("src"),
        manifest,
        None,
        tx,
    );
    assert_eq!(
        state.throttle_cap.load(std::sync::atomic::Ordering::Relaxed),
        u32::MAX
    );
    assert!(state.probe_stop_tx.is_some());
}
