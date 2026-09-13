pub mod store;
pub mod ports;
pub mod identity;
pub mod protocol;
pub mod discovery;
pub mod share;
pub mod share_watch;
pub mod transfer;
pub mod session;
pub mod relay;
pub mod pairing;
pub mod serde_compat;
pub mod device_merge;
pub mod net_addrs;
pub mod connect_memory;
pub mod business_card;
pub mod routing;

// 壳层探测编排需要引用 quinn::Connection 类型(on_session_up 签名),
// 经此再导出,避免壳层重复声明同一版本的 quinn 依赖(Cargo.lock 单一来源)
pub use quinn;

// Re-exports for discoverable public API
pub use identity::{TrustStore, Perms, PushPolicy, TrustedPeer};

pub mod test_support;

#[cfg(test)]
mod tests {
    #[test]
    fn smoke() { assert_eq!(2 + 2, 4); }
}
