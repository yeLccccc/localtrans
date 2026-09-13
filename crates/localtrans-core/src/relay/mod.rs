pub mod proto;
pub mod virtual_ep;
pub mod client;
pub mod autoheal;

/// 校验中继客户端配置(壳层保存前调用;规则与服务端拒启阈值对齐)
///
/// 返回 Err(中文原因) 时不得落盘、不得发起连接。
pub fn validate_relay_config(enabled: bool, server: &str, psk: &str) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    if server.trim().is_empty() || psk.is_empty() {
        return Err("启用中继需要填写服务器地址和密钥".into());
    }
    if server.parse::<std::net::SocketAddr>().is_err() {
        // 错误细分:双冒号(IPv6 误写)、缺端口是最常见的两种输入错误
        if server.contains("::") {
            return Err(format!(
                "地址格式无效: 疑似多了一个冒号,应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
            ));
        }
        if !server.contains(':') {
            return Err(format!(
                "地址格式无效: 缺少端口,应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
            ));
        }
        return Err(format!(
            "地址格式无效: 应为 IP:端口(如 203.0.113.10:9443),实际「{server}」"
        ));
    }
    if psk.len() < 16 {
        return Err(format!(
            "密钥过短: 至少 16 字符(与服务端要求一致),当前 {} 字符",
            psk.len()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod validate_tests {
    use super::*;

    const OK_ADDR: &str = "203.0.113.10:9443";
    const OK_PSK: &str = "0123456789abcdef";

    #[test]
    fn disabled_passes_even_with_empty_fields() {
        assert!(validate_relay_config(false, "", "").is_ok());
    }

    #[test]
    fn enabled_empty_server_rejected() {
        let e = validate_relay_config(true, "", OK_PSK).unwrap_err();
        assert!(e.contains("服务器地址和密钥"), "实际: {e}");
    }

    #[test]
    fn enabled_empty_psk_rejected() {
        let e = validate_relay_config(true, OK_ADDR, "").unwrap_err();
        assert!(e.contains("服务器地址和密钥"), "实际: {e}");
    }

    #[test]
    fn missing_port_rejected() {
        let e = validate_relay_config(true, "203.0.113.10", OK_PSK).unwrap_err();
        assert!(e.contains("缺少端口"), "实际: {e}");
    }

    #[test]
    fn double_colon_rejected() {
        let e = validate_relay_config(true, "203.0.113.10::9443", OK_PSK).unwrap_err();
        assert!(e.contains("多了一个冒号"), "实际: {e}");
    }

    #[test]
    fn valid_ipv4_passes() {
        assert!(validate_relay_config(true, OK_ADDR, OK_PSK).is_ok());
    }

    #[test]
    fn valid_ipv6_with_port_passes() {
        assert!(validate_relay_config(true, "[::1]:9443", OK_PSK).is_ok());
    }

    #[test]
    fn garbage_addr_rejected() {
        let e = validate_relay_config(true, "not an addr:9443", OK_PSK).unwrap_err();
        assert!(e.contains("地址格式无效"), "实际: {e}");
    }

    #[test]
    fn short_psk_rejected_and_16_passes() {
        let e = validate_relay_config(true, OK_ADDR, "0123456789abcde").unwrap_err();
        assert!(e.contains("密钥过短"), "实际: {e}");
        assert!(validate_relay_config(true, OK_ADDR, "0123456789abcdef").is_ok());
    }
}
