//! localtrans-relay:部署在公网服务器的中继(发现 + UDP 转发)。
//! lib 形态便于集成测试进程内起;bin 见 src/main.rs(Task 5)。

pub mod config;
pub mod lease;
pub mod control;
pub mod data_plane;

pub use config::RelayConfig;
pub use lease::LeaseTable;
pub use control::RelayServer;
pub use data_plane::DataPlane;
