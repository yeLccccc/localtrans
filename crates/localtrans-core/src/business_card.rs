//! business_card:名片体系(M3a FR3/FR4)——跨网段与 AP 隔离场景的发现兜底。
//!
//! 名片 = 本机对外可公开的"联系事实"集合:`{设备名, 指纹, 全部直连地址[],
//! 公网出口?, 中继地址?}`。生成与解析都在 core(纯文本双向转换,壳层只负责
//! 组装数据源);文本格式人可读 + 程序可解析,粘贴即加。
//!
//! 文本格式(钉死样例见 `文本格式_钉死` 单测):
//! ```text
//! LocalTrans 名片
//! 名称: huss_pc
//! 指纹: <64 位 hex>
//! 地址: 192.168.1.10:47601
//! 地址: 10.8.0.5:47601
//! 公网: 203.0.113.9:51234
//! 中继: relay.example.com:9443
//! ```
//! 解析**容错**:容忍前后空白/中英文冒号/多余与未知行/缺可选行/CRLF/BOM;
//! 解析**fail-closed**:识别到的标签载荷非法(指纹非 64 位 hex/地址不可解析/
//! 名称含控制符)即整体报错,绝不带病通过。
//!
//! 安全红线:
//! 1. 名片只含公开事实,**绝不含 PSK/私钥/证书**——解析器对未知标签(含
//!    "PSK:"/"私钥:"类敏感行)一律丢弃,roundtrip 天然过滤;
//! 2. 输入总量/字段长度/地址条数封顶,恶意超长粘贴有界拒绝;
//! 3. 本模块只做文本转换,不触碰广播包内容(广播不夹带地址数据)。

use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::SocketAddr;

/// 名片文本输入总量上限(chars)。正常名片 <0.5KB;8K 足够容忍粘贴夹带噪声,
/// 同时封死超大输入的解析开销。
pub const MAX_INPUT_CHARS: usize = 8192;
/// 单地址字段上限(chars)。IPv6 最长表示 45 + "[]:65535" 余量。
const MAX_ADDR_CHARS: usize = 64;
/// 公网出口/中继地址上限(chars)。容忍长域名(host 最长 253)。
const MAX_ENDPOINT_CHARS: usize = 128;
/// 设备名上限(chars)。与常规主机名长度同一量级。
const MAX_NAME_CHARS: usize = 64;
/// 地址条数上限。全网卡枚举个位数;超出即视为恶意/损坏输入。
const MAX_ADDRESSES: usize = 16;

/// 名片数据(生成数据源由壳层组装:device_name=配置;fingerprint=identity;
/// addresses=net_addrs::local_addresses 的 ip:quic_port 列表;public_exit=
/// relay client observed_addr;relay_addr=config relay_server)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BusinessCard {
    /// 设备名(对端发现表显示名)
    pub device_name: String,
    /// 设备指纹,SHA-256 证书指纹的 64 位小写 hex
    pub fingerprint: String,
    /// 直连地址列表 `ip:port`(QUIC 端口口径——发现表/直连用同一端口语义)
    pub addresses: Vec<String>,
    /// 公网出口 `ip:port`(中继 RegisterAck.observed_addr 回报,未启用中继则无)
    pub public_exit: Option<String>,
    /// 中继服务器地址 `host:port`(未启用中继则无)
    pub relay_addr: Option<String>,
}

/// 解析失败原因。Display 文案面向粘贴者,点名问题字段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseError {
    /// 输入为空/剥壳后无任何有效内容
    Empty,
    /// 输入超过 [`MAX_INPUT_CHARS`]
    InputTooLong,
    /// 缺必需字段(名称/指纹/地址)
    MissingField(&'static str),
    /// 设备名非法(空/超长/含控制符)
    BadName,
    /// 指纹不是 64 位 hex
    BadFingerprint,
    /// 地址不可解析(非 ip:port / 端口 0)
    BadAddress(String),
    /// 公网/中继端点形状非法(需 host:port)
    BadEndpoint(&'static str, String),
    /// 地址条数超上限
    TooManyAddresses,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ParseError::Empty => write!(f, "名片内容为空"),
            ParseError::InputTooLong => write!(f, "名片内容超长(上限 {MAX_INPUT_CHARS} 字符)"),
            ParseError::MissingField(k) => write!(f, "名片缺少「{k}」行"),
            ParseError::BadName => write!(f, "设备名非法(空/超长/含控制字符)"),
            ParseError::BadFingerprint => write!(f, "指纹非法(应为 64 位十六进制)"),
            ParseError::BadAddress(a) => write!(f, "地址非法: {a}"),
            ParseError::BadEndpoint(k, v) => write!(f, "{k} 非法(应为 host:port): {v}"),
            ParseError::TooManyAddresses => write!(f, "地址条数超上限(最多 {MAX_ADDRESSES} 条)"),
        }
    }
}

impl std::error::Error for ParseError {}

impl BusinessCard {
    /// 序列化为人可读多行文本(规范形态:英文冒号 + 单空格;解析端兼容
    /// 中文冒号/任意空白)。行序固定,地址按表内顺序逐行输出。
    pub fn to_text(&self) -> String {
        let mut s = String::from("LocalTrans 名片\n");
        s.push_str(&format!("名称: {}\n", self.device_name));
        s.push_str(&format!("指纹: {}\n", self.fingerprint));
        for a in &self.addresses {
            s.push_str(&format!("地址: {a}\n"));
        }
        if let Some(e) = &self.public_exit {
            s.push_str(&format!("公网: {e}\n"));
        }
        if let Some(r) = &self.relay_addr {
            s.push_str(&format!("中继: {r}\n"));
        }
        // 末行不带换行,便于整段复制进聊天工具不带回车尾巴
        s.trim_end().to_string()
    }

    /// 从粘贴文本解析名片。容错与红线见模块头注释。
    pub fn parse(text: &str) -> Result<BusinessCard, ParseError> {
        // 去 BOM(剪贴板常见)后总量封顶——有界解析是恶意输入防线的第一道
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        if text.chars().count() > MAX_INPUT_CHARS {
            return Err(ParseError::InputTooLong);
        }

        let mut name: Option<String> = None;
        let mut fingerprint: Option<String> = None;
        let mut addresses: Vec<String> = Vec::new();
        let mut public_exit: Option<String> = None;
        let mut relay_addr: Option<String> = None;

        for line in text.lines() {
            let line = line.trim(); // 容忍行首尾空白与 \r(CRLF)
            if line.is_empty() {
                continue;
            }
            // 找首个冒号(中/英)切 标签/载荷;无冒号的行(如表头"LocalTrans 名片"
            // 或粘贴夹带的噪声行)一律忽略——多余行容错
            let Some((label, value)) = split_label(line) else {
                continue;
            };
            match label {
                "名称" | "设备名" => name = Some(value.to_string()), // 后行覆盖前行(最后 wins)
                "指纹" => fingerprint = Some(value.to_lowercase()),  // hex 大小写归一
                "地址" => {
                    if addresses.len() >= MAX_ADDRESSES {
                        return Err(ParseError::TooManyAddresses);
                    }
                    validate_address(value)?;
                    if !addresses.contains(&value.to_string()) {
                        addresses.push(value.to_string()); // 去重保序
                    }
                }
                "公网" => {
                    validate_endpoint("公网出口", value)?;
                    public_exit = Some(value.to_string());
                }
                "中继" => {
                    validate_endpoint("中继地址", value)?;
                    relay_addr = Some(value.to_string());
                }
                // 未知标签(含 PSK/私钥等敏感行)一律丢弃——名片不携带 secrets,
                // 也不因粘贴文本里混入无关行而失败
                _ => {}
            }
        }

        if text.trim().is_empty() {
            return Err(ParseError::Empty);
        }
        let device_name = match name.as_deref() {
            Some(n) if !n.is_empty() => validate_name(n)?,
            _ => return Err(ParseError::MissingField("名称")),
        };
        let fingerprint = match fingerprint {
            Some(fp) => validate_fingerprint(&fp)?,
            None => return Err(ParseError::MissingField("指纹")),
        };
        if addresses.is_empty() {
            return Err(ParseError::MissingField("地址"));
        }
        Ok(BusinessCard { device_name, fingerprint, addresses, public_exit, relay_addr })
    }
}

/// 切分行标签:首个中/英冒号前为标签( trim),后为载荷( trim)。
fn split_label(line: &str) -> Option<(&str, &str)> {
    let idx = line.find([':', '：'])?;
    let sep = line[idx..].chars().next()?;
    let label = line[..idx].trim();
    let value = line[idx + sep.len_utf8()..].trim();
    Some((label, value))
}

/// 直连地址校验:必须可解析为 ip:port 且端口非 0。
fn validate_address(raw: &str) -> Result<(), ParseError> {
    let ok = raw.chars().count() <= MAX_ADDR_CHARS
        && raw.parse::<SocketAddr>().map(|sa| sa.port() != 0).unwrap_or(false);
    if ok {
        Ok(())
    } else {
        Err(ParseError::BadAddress(raw.to_string()))
    }
}

/// 端点形状校验(公网出口/中继地址):`host:port`,host 容忍域名与 [IPv6],
/// 端口必须为数字。不落 IP 级强校验——中继地址常为域名,字段属展示/兜底用途。
fn validate_endpoint(kind: &'static str, raw: &str) -> Result<(), ParseError> {
    let bad = || ParseError::BadEndpoint(kind, raw.to_string());
    if raw.chars().count() > MAX_ENDPOINT_CHARS {
        return Err(bad());
    }
    if raw.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad());
    }
    let Some((host, port)) = raw.rsplit_once(':') else {
        return Err(bad());
    };
    if host.is_empty() || port.is_empty() || port.parse::<u16>().is_err() {
        return Err(bad());
    }
    Ok(())
}

/// 指纹校验:恰好 64 位 hex(parse 前已 lowercase)。
fn validate_fingerprint(raw: &str) -> Result<String, ParseError> {
    let is_hex = raw.len() == 64
        && raw.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if is_hex {
        Ok(raw.to_string())
    } else {
        Err(ParseError::BadFingerprint)
    }
}

/// 设备名校验:非空、长度受限、不含控制符(换行已被行切分天然排除)。
fn validate_name(raw: &str) -> Result<String, ParseError> {
    let ok = raw.chars().count() <= MAX_NAME_CHARS && !raw.chars().any(char::is_control);
    if ok {
        Ok(raw.to_string())
    } else {
        Err(ParseError::BadName)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP_A: &str = "3f2a4b5c6d7e8f90112233445566778899aabbccddeeff001122334455667788";

    fn card_a() -> BusinessCard {
        BusinessCard {
            device_name: "huss_pc".into(),
            fingerprint: FP_A.into(),
            addresses: vec!["192.168.1.10:47601".into(), "10.8.0.5:47601".into()],
            public_exit: Some("203.0.113.9:51234".into()),
            relay_addr: Some("relay.example.com:9443".into()),
        }
    }

    #[test]
    fn 文本格式_钉死() {
        // 格式契约:任何改动都必须有意识地同步解析端与文档
        assert_eq!(
            card_a().to_text(),
            "LocalTrans 名片\n\
             名称: huss_pc\n\
             指纹: 3f2a4b5c6d7e8f90112233445566778899aabbccddeeff001122334455667788\n\
             地址: 192.168.1.10:47601\n\
             地址: 10.8.0.5:47601\n\
             公网: 203.0.113.9:51234\n\
             中继: relay.example.com:9443"
        );
        // 最小名片:可选行(公网/中继)缺省不输出
        let min = BusinessCard {
            public_exit: None,
            relay_addr: None,
            ..card_a()
        };
        let t = min.to_text();
        assert!(!t.contains("公网") && !t.contains("中继"));
        assert_eq!(t.lines().count(), 5);
    }

    #[test]
    fn roundtrip_全字段() {
        let c = card_a();
        assert_eq!(BusinessCard::parse(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn roundtrip_最小卡片() {
        let c = BusinessCard { public_exit: None, relay_addr: None, ..card_a() };
        assert_eq!(BusinessCard::parse(&c.to_text()).unwrap(), c);
    }

    #[test]
    fn 容错_中英文冒号空白CRLF与噪声行() {
        let noisy = format!(
            "\u{feff}  \r\n somebody pasted above \r\nLocalTrans 名片\r\n\r\n \
             名称： huss_pc  \r\n \
             指纹:  {FP_A}\r\n \
             —— 分割线 ——\r\n \
             地址:192.168.1.10:47601\r\n \
             地址：  [::1]:47601  \r\n \
             备注line without any meaning\r\n \
             公网 : 203.0.113.9:51234 \r\n \
             中继：relay.example.com:9443\r\n \r\n   "
        );
        let c = BusinessCard::parse(&noisy).unwrap();
        assert_eq!(c.device_name, "huss_pc");
        assert_eq!(c.fingerprint, FP_A);
        assert_eq!(c.addresses, vec!["192.168.1.10:47601", "[::1]:47601"]);
        assert_eq!(c.public_exit.as_deref(), Some("203.0.113.9:51234"));
        assert_eq!(c.relay_addr.as_deref(), Some("relay.example.com:9443"));
    }

    #[test]
    fn 容错_无表头与重复行_最后wins地址去重() {
        let t = format!(
            "名称: 甲机器\n名称: 乙机器\n指纹: {FP_A}\n地址: 10.0.0.2:47601\n地址: 10.0.0.2:47601"
        );
        let c = BusinessCard::parse(&t).unwrap();
        assert_eq!(c.device_name, "乙机器");
        assert_eq!(c.addresses, vec!["10.0.0.2:47601"], "重复地址去重保序");
    }

    #[test]
    fn 容错_别名标签_设备名同名称() {
        let t = format!("设备名: huss_pc\n指纹: {FP_A}\n地址: 10.0.0.2:47601");
        assert_eq!(BusinessCard::parse(&t).unwrap().device_name, "huss_pc");
    }

    #[test]
    fn 解析安全_空输入() {
        assert_eq!(BusinessCard::parse(""), Err(ParseError::Empty));
        assert_eq!(BusinessCard::parse("   \r\n\t "), Err(ParseError::Empty));
        // 全是噪声行 → 无必需字段
        assert_eq!(BusinessCard::parse("hello\nworld"), Err(ParseError::MissingField("名称")));
    }

    #[test]
    fn 解析安全_超长输入整体拒绝() {
        let big = "x".repeat(MAX_INPUT_CHARS + 1);
        assert_eq!(BusinessCard::parse(&big), Err(ParseError::InputTooLong));
        // 恰好在上限内的纯噪声 → 走缺字段而非超长(有界但不误报)
        let ok_len = "x".repeat(MAX_INPUT_CHARS);
        assert_eq!(BusinessCard::parse(&ok_len), Err(ParseError::MissingField("名称")));
    }

    #[test]
    fn 解析安全_超长行_识别标签报错未知标签忽略() {
        // 识别标签带超长载荷 → 地址校验拒绝(有界报错)
        let junk_addr = format!("地址: {}", "9".repeat(600));
        let t = format!("名称: a\n指纹: {FP_A}\n{junk_addr}");
        assert!(matches!(BusinessCard::parse(&t), Err(ParseError::BadAddress(_))));
        // 未知标签超长行 → 忽略(不因噪声崩),随后按缺字段报错
        let noise = format!("psk: {}\n", "s".repeat(2000));
        let t2 = format!("{noise}名称: a\n指纹: {FP_A}\n地址: 10.0.0.2:47601");
        assert!(BusinessCard::parse(&t2).is_ok());
    }

    #[test]
    fn 解析安全_缺必需字段逐个点名() {
        let base = |name: &str, fp: &str, addr: &str| format!("{name}{fp}{addr}");
        let fp_line = format!("指纹: {FP_A}\n");
        assert_eq!(
            BusinessCard::parse(&base("", &fp_line, "地址: 10.0.0.2:47601\n")),
            Err(ParseError::MissingField("名称"))
        );
        assert_eq!(
            BusinessCard::parse(&base("名称: a\n", "", "地址: 10.0.0.2:47601\n")),
            Err(ParseError::MissingField("指纹"))
        );
        assert_eq!(
            BusinessCard::parse(&base("名称: a\n", &fp_line, "")),
            Err(ParseError::MissingField("地址"))
        );
        // 值为空白等同缺行
        assert_eq!(
            BusinessCard::parse(&base("名称:   \n", &fp_line, "地址: 10.0.0.2:47601\n")),
            Err(ParseError::MissingField("名称"))
        );
    }

    #[test]
    fn 解析安全_指纹非法() {
        let addr = "地址: 10.0.0.2:47601\n";
        for bad in [
            "0123".to_string(),                        // 过短
            "a".repeat(63),                            // 63 位
            "a".repeat(65),                            // 65 位
            format!("{}g", "a".repeat(63)),            // 非 hex 字符
            "中".repeat(64),                           // 多字节充数
            String::new(),                             // 空值
        ] {
            let t = format!("名称: a\n指纹: {bad}\n{addr}");
            assert_eq!(BusinessCard::parse(&t), Err(ParseError::BadFingerprint), "bad={bad:?}");
        }
        // 大写 hex 合法且归一为小写
        let upper = FP_A.to_uppercase();
        let t = format!("名称: a\n指纹: {upper}\n{addr}");
        assert_eq!(BusinessCard::parse(&t).unwrap().fingerprint, FP_A);
    }

    #[test]
    fn 解析安全_地址非法() {
        let head = format!("名称: a\n指纹: {FP_A}\n");
        for bad in [
            "not-an-address",
            "1.2.3.4",           // 缺端口
            "1.2.3.4:0",         // 端口 0
            "999.1.1.1:47601",   // 非法 IP
            "1.2.3.4:99999",     // 端口越界
            ":47601",            // 缺 host
            "1.2.3.4:",          // 缺端口数字
        ] {
            let t = format!("{head}地址: {bad}");
            assert!(
                matches!(BusinessCard::parse(&t), Err(ParseError::BadAddress(_))),
                "bad={bad:?}"
            );
        }
        // 多地址中一条非法 → 整体拒绝(不带病通过)
        let t = format!("{head}地址: 10.0.0.2:47601\n地址: 垃圾\n");
        assert!(matches!(BusinessCard::parse(&t), Err(ParseError::BadAddress(_))));
    }

    #[test]
    fn 解析安全_地址超量() {
        let head = format!("名称: a\n指纹: {FP_A}\n");
        let mut t = head;
        for i in 1..=MAX_ADDRESSES {
            t.push_str(&format!("地址: 10.0.0.{i}:47601\n"));
        }
        assert!(BusinessCard::parse(&t).is_ok(), "恰好上限合法");
        t.push_str("地址: 10.9.9.9:47601\n");
        assert_eq!(BusinessCard::parse(&t), Err(ParseError::TooManyAddresses));
    }

    #[test]
    fn 解析安全_名称非法() {
        let tail = format!("指纹: {FP_A}\n地址: 10.0.0.2:47601");
        let ctrl = format!("名称: a\u{0}b\n{tail}");
        assert_eq!(BusinessCard::parse(&ctrl), Err(ParseError::BadName));
        let long = format!("名称: {}\n{tail}", "名".repeat(MAX_NAME_CHARS + 1));
        assert_eq!(BusinessCard::parse(&long), Err(ParseError::BadName));
        // 恰好上限合法;名称含冒号取首冒号后全值
        let edge = format!("名称: {}\n{tail}", "名".repeat(MAX_NAME_CHARS));
        assert_eq!(BusinessCard::parse(&edge).unwrap().device_name.chars().count(), MAX_NAME_CHARS);
        let colon = format!("名称: 服务器: 二号\n{tail}");
        assert_eq!(BusinessCard::parse(&colon).unwrap().device_name, "服务器: 二号");
    }

    #[test]
    fn 解析安全_端点字段非法() {
        let head = format!("名称: a\n指纹: {FP_A}\n地址: 10.0.0.2:47601\n");
        for (line, kind) in [
            ("公网: no-port-here", "公网出口"),
            ("公网: 1.2.3.4:", "公网出口"),
            ("公网: 1.2.3.4:99999", "公网出口"),
            ("中继: host not allowed:9443", "中继地址"),
            ("中继: host:port:9443:bad", "中继地址"),
        ] {
            let t = format!("{head}{line}");
            assert_eq!(
                BusinessCard::parse(&t),
                Err(ParseError::BadEndpoint(kind, line.split_once(':').unwrap().1.trim().to_string())),
                "line={line:?}"
            );
        }
        // 域名中继合法
        let t = format!("{head}中继: relay.example.com:9443");
        assert!(BusinessCard::parse(&t).is_ok());
    }

    #[test]
    fn 解析安全_敏感行被丢弃且roundtrip不复活() {
        // 粘贴文本被恶意/意外夹带 PSK/私钥行 → 解析丢弃,再序列化不复现
        let hostile = format!(
            "LocalTrans 名片\n名称: a\n指纹: {FP_A}\n地址: 10.0.0.2:47601\n\
             PSK: top-secret-psk-value\n私钥: -----BEGIN PRIVATE KEY-----\nrelay_psk: hunter2\n"
        );
        let c = BusinessCard::parse(&hostile).expect("敏感行为未知标签,名片本体应正常解析");
        let out = c.to_text();
        assert!(!out.contains("top-secret"), "输出泄漏 PSK: {out}");
        assert!(!out.contains("PRIVATE KEY"), "输出泄漏私钥: {out}");
        assert!(!out.contains("hunter2"), "输出泄漏 relay_psk: {out}");
        // roundtrip 后同样干净
        assert_eq!(BusinessCard::parse(&out).unwrap(), c);
        assert!(!serde_json::to_string(&c).unwrap().contains("top-secret"));
    }

    #[test]
    fn 错误文案_可读且点名字段() {
        assert_eq!(ParseError::MissingField("指纹").to_string(), "名片缺少「指纹」行");
        assert!(ParseError::BadAddress("1.2.3.4".into()).to_string().contains("1.2.3.4"));
        assert!(ParseError::BadFingerprint.to_string().contains("64"));
    }
}
