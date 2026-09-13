//! 默认端口集中管理(T1:测试端口可配置化)。
//!
//! 发现端口/QUIC 端口的"默认值"只允许从本模块取——禁止在生产或测试
//! 代码里重新手写 47600/47601,否则设了 LOCALTRANS_TEST_PORT_BASE 后
//! 各消费点会错位(绑定端口与广播目标不一致 = 互相发现不了)。
//!
//! 语义:设 `LOCALTRANS_TEST_PORT_BASE=<base>` 时,发现端口=<base>,
//! QUIC=<base+1>;不设则与历史默认 47600/47601 逐字节一致。
//! 本变量仅供测试/并行泳道隔离用,生产环境不设、行为不变。

/// 历史默认发现端口(env 未设时的唯一真值)
pub const DEFAULT_DISCOVERY_PORT: u16 = 47600;
/// 环境变量名:测试端口基址(发现端口直接取该值,QUIC 端口 = 基址+1)
pub const PORT_BASE_ENV: &str = "LOCALTRANS_TEST_PORT_BASE";

/// 纯函数:由 env 原始值推导 (发现端口, QUIC 端口)。
///
/// - `None`(未设置)→ (47600, 47601);
/// - `Some(s)` 解析为 u16 且 QUIC 端口不溢出 → (base, base+1);
/// - 非法/越界(非数字、65535 等)→ 回退默认,不 panic(测试进程
///   可能残留脏值,回退比崩溃更适合测试基础设施)。
pub fn resolve(raw: Option<&str>) -> (u16, u16) {
    const DEFAULT: (u16, u16) = (DEFAULT_DISCOVERY_PORT, DEFAULT_DISCOVERY_PORT + 1);
    match raw {
        None => DEFAULT,
        Some(s) => match s.trim().parse::<u16>() {
            Ok(base) if base < u16::MAX => (base, base + 1),
            _ => DEFAULT,
        },
    }
}

/// 当前进程生效的发现端口。每次直读 env、不缓存:进程内 env 变更
/// 即时生效,且避免 OnceLock 全局缓存把"设 env"变成不可测的时序问题。
pub fn discovery_port() -> u16 {
    resolve(std::env::var(PORT_BASE_ENV).ok().as_deref()).0
}

/// 当前进程生效的 QUIC 端口(=发现端口+1)
pub fn quic_port() -> u16 {
    resolve(std::env::var(PORT_BASE_ENV).ok().as_deref()).1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 未设env保持历史默认() {
        assert_eq!(resolve(None), (47600, 47601));
    }

    #[test]
    fn 设env后端口为base与base加1() {
        assert_eq!(resolve(Some("50100")), (50100, 50101));
        assert_eq!(resolve(Some("1024")), (1024, 1025));
        // 首尾合法值
        assert_eq!(resolve(Some("0")), (0, 1));
        assert_eq!(resolve(Some("65534")), (65534, 65535));
    }

    #[test]
    fn 非法或越界base回退默认() {
        assert_eq!(resolve(Some("junk")), (47600, 47601));
        assert_eq!(resolve(Some("")), (47600, 47601));
        assert_eq!(resolve(Some("  ")), (47600, 47601));
        assert_eq!(resolve(Some("-1")), (47600, 47601));
        // 65535 会使 QUIC=65536 溢出 → 整体回退
        assert_eq!(resolve(Some("65535")), (47600, 47601));
        assert_eq!(resolve(Some("70000")), (47600, 47601));
    }

    #[test]
    fn 包装函数与进程env推导一致() {
        // 不改进程 env(并行测试安全):包装函数必须等于"直读当前 env 的 resolve"
        let raw = std::env::var(PORT_BASE_ENV).ok();
        assert_eq!((discovery_port(), quic_port()), resolve(raw.as_deref()));
    }

    #[test]
    fn 默认常量与resolve默认一致() {
        // DEFAULT_DISCOVERY_PORT 是对外常量,必须与 resolve(None) 钉死在同一真值
        assert_eq!(resolve(None).0, DEFAULT_DISCOVERY_PORT);
    }
}
