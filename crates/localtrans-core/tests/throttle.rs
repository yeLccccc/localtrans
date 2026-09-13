use localtrans_core::protocol::{ControlMsg, TransferAction};

#[test]
fn throttle_action_serializes_with_max_streams() {
    let msg = ControlMsg::TransferCtl {
        job_id: 1,
        action: TransferAction::Throttle { max_streams: 2 },
    };
    let s = serde_json::to_string(&msg).unwrap();
    // Per controller ruling A: assert actual shape ("throttle" and "max_streams" substrings)
    // NOT "Throttle" because rename_all = "snake_case" affects mixed enum variants
    assert!(s.contains("\"throttle\""), "got: {}", s);
    assert!(s.contains("\"max_streams\":2"), "got: {}", s);

    // Also do a full round-trip test
    let round_trip = serde_json::from_str::<ControlMsg>(&s).unwrap();
    assert_eq!(msg, round_trip, "round-trip should preserve equality");
}
