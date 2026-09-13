//! relay 服务端 TOML 配置

use serde::Deserialize;
use std::net::IpAddr;
use std::path::Path;

#[derive(Deserialize, Clone, Debug)]
pub struct RelayConfig {
    /// 控制面 QUIC 监听端口
    pub control_port: u16,
    /// 数据面 UDP 端口池(含两端)
    pub data_port_start: u16,
    pub data_port_end: u16,
    /// 对外公布的中继公网 IP(名册里拼租约地址用)
    pub public_ip: IpAddr,
    /// 预共享密钥(部署时与客户端配置一致)
    pub psk: String,
    /// 租约 TTL(秒)
    #[serde(default = "default_lease_ttl")]
    pub lease_ttl_secs: u64,
    /// 认证失败速率限制:窗口内次数
    #[serde(default = "default_auth_max")]
    pub auth_max_per_min: u32,
    /// 预留:v1 不实现
    #[serde(default)]
    pub max_bps_per_pair: Option<u64>,
}

fn default_lease_ttl() -> u64 { 45 }
fn default_auth_max() -> u32 { 5 }

impl RelayConfig {
    pub fn from_toml_str(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| format!("配置解析失败: {e}"))
    }

    /// P0-3: 严格加载——配置文件缺失/解析失败/psk 过短一律拒绝启动。
    /// (v0.8.1 前缺失时静默回退 dev-psk 并监听 0.0.0.0,公网裸奔)
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|_| format!("配置文件 {} 不存在或不可读, 拒绝启动。必须显式配置 psk。", path.display()))?;
        let cfg = Self::from_toml_str(&content)
            .map_err(|e| format!("配置文件 {} 解析失败 ({}) , 拒绝启动。", path.display(), e))?;
        if cfg.psk.len() < 16 {
            return Err(format!(
                "psk 长度不足 16 字符 (当前 {}) , 拒绝启动。建议 32+ 随机字节 hex。", cfg.psk.len()
            ));
        }
        Ok(cfg)
    }

    /// PSK 摘要(sha256 前 16 hex)——启动日志供运维核对配置是否为预期值,不暴露 PSK
    /// (低危审计修复:8 hex 碰撞空间过小,加长到 16 hex)
    pub fn psk_digest(psk: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(psk.as_bytes());
        hex::encode(&h.finalize()[..8])
    }

    /// 测试/开发用默认配置
    pub fn default_for_test() -> Self {
        Self {
            control_port: 9443,
            data_port_start: 9000,
            data_port_end: 9100,
            public_ip: "127.0.0.1".parse().unwrap(),
            psk: "dev-psk".into(),
            lease_ttl_secs: 45,
            auth_max_per_min: 5,
            max_bps_per_pair: None,
        }
    }

    /// 测试用配置(带端口偏移避免冲突)
    pub fn for_test_with_port_offset(offset: u16) -> Self {
        let mut cfg = Self::default_for_test();
        cfg.control_port = 9443 + offset;
        cfg.data_port_start = 9000 + offset * 100;
        cfg.data_port_end = 9100 + offset * 100;
        cfg
    }
}

#[cfg(test)]
mod strict_load_tests {
    use super::*;

    fn write_cfg(dir: &std::path::Path, content: &str) -> std::path::PathBuf {
        let p = dir.join("relay.toml");
        std::fs::write(&p, content).unwrap();
        p
    }

    #[test]
    fn missing_file_rejected() {
        let err = RelayConfig::load(std::path::Path::new("Z:/不存在/relay.toml")).unwrap_err();
        assert!(err.contains("拒绝启动"), "实际: {}", err);
    }

    #[test]
    fn short_psk_rejected() {
        let tmp = std::env::temp_dir().join(format!("lt-relay-{}-short", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let p = write_cfg(&tmp, "control_port = 9443\ndata_port_start = 9000\ndata_port_end = 9100\npublic_ip = \"127.0.0.1\"\npsk = \"short\"\n");
        let err = RelayConfig::load(&p).unwrap_err();
        assert!(err.contains("16"), "实际: {}", err);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn valid_config_loads() {
        let tmp = std::env::temp_dir().join(format!("lt-relay-{}-ok", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let p = write_cfg(&tmp, "control_port = 9443\ndata_port_start = 9000\ndata_port_end = 9100\npublic_ip = \"127.0.0.1\"\npsk = \"0123456789abcdef0123456789abcdef\"\n");
        let cfg = RelayConfig::load(&p).unwrap();
        assert_eq!(cfg.psk, "0123456789abcdef0123456789abcdef");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn psk_digest_is_16_hex() {
        let d = RelayConfig::psk_digest("whatever-psk-value");
        assert_eq!(d.len(), 16);
        assert!(d.chars().all(|c| c.is_ascii_hexdigit()), "实际: {}", d);
        // 确定性:同 PSK 同摘要
        assert_eq!(d, RelayConfig::psk_digest("whatever-psk-value"));
    }
}
