use localtrans_core::serde_compat::u64_hex_string;
use localtrans_core::session::next_source_job_id;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Debug)]
struct W {
    #[serde(with = "u64_hex_string")]
    j: u64,
}

#[test]
fn u64_serializes_as_hex_string() {
    let w = W {
        j: 0x1234_5678_9abc_def0,
    };
    let s = serde_json::to_string(&w).unwrap();
    assert!(s.contains("\"j\":\"123456789abcdef0\""), "got: {}", s);
}

#[test]
fn u64_roundtrips_precision_safe() {
    let original = 1u64 << 62; // 4.6e18 — 远超 JS Number 安全整数
    let s = serde_json::to_string(&W { j: original }).unwrap();
    let parsed: W = serde_json::from_str(&s).unwrap();
    assert_eq!(parsed.j, original);
}

#[test]
fn u64_zero_serializes_as_16_zero_chars() {
    let s = serde_json::to_string(&W { j: 0 }).unwrap();
    assert!(s.contains("\"j\":\"0000000000000000\""));
}

#[test]
fn source_job_id_starts_at_high_bit_segment() {
    let id = next_source_job_id();
    assert!(id >= 0x8000_0000_0000_0000, "got: {:x}", id);
}

#[test]
fn source_job_ids_are_unique_across_threads() {
    use std::collections::HashSet;
    use std::sync::{Arc, Mutex};
    use std::thread;

    let seen = Arc::new(Mutex::new(HashSet::new()));
    let mut handles = vec![];
    for _ in 0..8 {
        let seen = seen.clone();
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                let id = next_source_job_id();
                seen.lock().unwrap().insert(id);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(seen.lock().unwrap().len(), 800);
}
