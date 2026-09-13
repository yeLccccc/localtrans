//! relay_proto:中继控制面消息 + 数据面 18B 头。客户端/服务端共用。

use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// 数据面包头长度:[版本1B][标志1B][令牌16B]
pub const DATA_HEADER_LEN: usize = 18;
/// 当前载体协议版本(TCP 兜底等将来扩展靠它)
pub const DATA_VERSION: u8 = 0x01;
/// KNOCK 标志:打洞包,中继不转发,只学得发送方公网地址
pub const FLAG_KNOCK: u8 = 0x01;
/// GOODBYE_KNOCK 标志:优雅关闭,中继收到即回收租约
pub const FLAG_GOODBYE_KNOCK: u8 = 0x02;
/// 普通数据包(需转发)
pub const FLAG_DATA: u8 = 0x00;

/// 数据面租约(端口+令牌)
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct LeaseInfo {
    pub data_port: u16,
    pub token: [u8; 16],
}

/// 名册里的远程设备
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct RemoteDevice {
    pub fingerprint: [u8; 32],
    pub name: String,
    /// 对方租约地址(中继IP:端口)——虚拟端点 connect 的目标
    pub lease_addr: SocketAddr,
    /// 预留:直连扩展位(v1 恒 false)
    #[serde(default)]
    pub relayed_ephemeral: bool,
}

/// 控制面消息(两端共用,风格对齐 protocol::ControlMsg)
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RelayMsg {
    // 客户端 → 服务器
    Register {
        name: String,
        fingerprint: [u8; 32],
        #[serde(default)]
        hidden: bool,
        /// S2 占有证明:客户端叶子证书 DER(声明指纹 = sha256(cert_der))
        /// 硬切换:字段必填,缺字段直接解码失败,不留"可不带"的口子
        cert_der: Vec<u8>,
        /// 对 ServerNonce 的 ed25519 签名 sign(fp || nonce)
        #[serde(with = "nonce_sig_serde")]
        nonce_sig: [u8; 64],
    },
    Ping,
    Punch { target_fp: [u8; 32] },
    Leave,
    // 服务器 → 客户端
    /// TLS 握手完成后、等 Register 前服务端先推的一次性 nonce(S2 占有证明)
    ServerNonce { nonce: [u8; 32] },
    /// 注册应答。M3a FR2:`observed_addr` 为服务端观察到的本客户端公网出口
    /// (注册连接的源 `ip:port`),客户端存入本机地址池供选路/名片使用。
    /// 兼容:旧服务端不带该字段 → None(serde default);旧客户端收到新字段
    /// 按 serde 默认行为忽略未知字段;None 时不序列化,线上字节与旧版一致。
    RegisterAck {
        lease: Option<LeaseInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        observed_addr: Option<String>,
    },
    Roster { rev: u64, devices: Vec<RemoteDevice> },
    PunchNotif { target_fp: [u8; 32], target_lease: SocketAddr },
    PunchResp { ok: bool, reason: Option<String>, #[serde(default)] session_addr: Option<String> },
    Error { code: u32, msg: String },
}

/// [u8;64] 签名的 hex 序列化(JSON 里可读)


mod nonce_sig_serde {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(sig: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(sig))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        use serde::de::Error;
        let s = String::deserialize(d)?;
        let bytes = hex::decode(&s).map_err(|e| Error::custom(format!("invalid hex: {e}")))?;
        if bytes.len() != 64 {
            return Err(Error::custom(format!("签名长度应为 64, 实得 {}", bytes.len())));
        }
        let mut out = [0u8; 64];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

/// 控制面消息编解码(JSON + 4B 长度前缀,风格对齐 protocol.rs)
pub fn encode_relay_msg(msg: &RelayMsg) -> Result<Vec<u8>, String> {
    let body = serde_json::to_vec(msg).map_err(|e| e.to_string())?;
    if body.len() > 64 * 1024 {
        return Err("控制消息过大".into());
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

pub fn decode_relay_msg(bytes: &[u8]) -> Result<RelayMsg, String> {
    if bytes.len() < 4 {
        return Err("消息过短".into());
    }
    let len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
    if bytes.len() != 4 + len {
        return Err("长度不匹配".into());
    }
    serde_json::from_slice(&bytes[4..]).map_err(|e| e.to_string())
}

/// 数据面 18B 头编码
pub fn data_header_encode(out: &mut Vec<u8>, token: &[u8; 16], flag: u8) {
    out.push(DATA_VERSION);
    out.push(flag);
    out.extend_from_slice(token);
}

/// 数据面 18B 头解码;版本不符报错
pub fn data_header_decode(buf: &[u8]) -> Result<([u8; 16], u8), String> {
    if buf.len() < DATA_HEADER_LEN {
        return Err("数据包头不足".into());
    }
    if buf[0] != DATA_VERSION {
        return Err(format!("版本不符: {}", buf[0]));
    }
    let mut token = [0u8; 16];
    token.copy_from_slice(&buf[2..18]);
    Ok((token, buf[1]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_msg_roundtrip() {
        let msgs = vec![
            RelayMsg::Register {
                name: "家里PC".into(),
                fingerprint: [1u8; 32],
                hidden: true,
                cert_der: vec![0xAA, 0xBB],
                nonce_sig: [3u8; 64],
            },
            RelayMsg::ServerNonce { nonce: [5u8; 32] },
            RelayMsg::Ping,
            RelayMsg::Leave,
            RelayMsg::RegisterAck {
                lease: Some(LeaseInfo {
                    data_port: 9001,
                    token: [7u8; 16],
                }),
                observed_addr: None,
            },
            RelayMsg::RegisterAck {
                lease: Some(LeaseInfo {
                    data_port: 9001,
                    token: [7u8; 16],
                }),
                observed_addr: Some("203.0.113.9:51234".into()),
            },
            RelayMsg::Roster {
                rev: 3,
                devices: vec![RemoteDevice {
                    fingerprint: [2u8; 32],
                    name: "公司电脑".into(),
                    lease_addr: "1.2.3.4:9002".parse().unwrap(),
                    relayed_ephemeral: false,
                }],
            },
            RelayMsg::PunchNotif {
                target_fp: [2u8; 32],
                target_lease: "1.2.3.4:9002".parse().unwrap(),
            },
            RelayMsg::PunchResp { ok: true, reason: None, session_addr: None },
            RelayMsg::PunchResp {
                ok: true,
                reason: None,
                session_addr: Some("127.0.0.1:9999".to_string()),
            },
            RelayMsg::Error { code: 1, msg: "psk 不对".into() },
        ];
        for m in msgs {
            let bytes = encode_relay_msg(&m).unwrap();
            let back = decode_relay_msg(&bytes).unwrap();
            assert_eq!(m, back);
        }
    }

    #[test]
    fn data_header_roundtrip() {
        let mut buf = Vec::new();
        data_header_encode(&mut buf, &[9u8; 16], FLAG_KNOCK);
        assert_eq!(buf.len(), DATA_HEADER_LEN);
        let (token, flag) = data_header_decode(&buf).unwrap();
        assert_eq!(token, [9u8; 16]);
        assert_eq!(flag, FLAG_KNOCK);
    }

    #[test]
    fn data_header_rejects_bad_version() {
        let mut buf = Vec::new();
        data_header_encode(&mut buf, &[9u8; 16], 0);
        buf[0] = 0xFF; // 篡改版本
        assert!(data_header_decode(&buf).is_err());
    }

    #[test]
    fn register_without_hidden_decodes_as_visible() {
        // 旧客户端 Register 不带 hidden 字段 → 反序列化为 false(向后兼容)
        let full = RelayMsg::Register { name: "旧端".into(), fingerprint: [1u8; 32], hidden: true, cert_der: vec![], nonce_sig: [0u8; 64] };
        let v = serde_json::to_value(&full).unwrap();
        let mut stripped = v.clone();
        stripped.as_object_mut().unwrap().remove("hidden");
        let body = stripped.to_string();
        let mut framed = Vec::new();
        framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
        framed.extend_from_slice(body.as_bytes());
        match decode_relay_msg(&framed).unwrap() {
            RelayMsg::Register { hidden, .. } => assert!(!hidden, "缺省 hidden 应为 false"),
            other => panic!("期望 Register, 实得 {:?}", other),
        }
    }

    /// 4B 长度前缀封帧(测试辅助)
    fn frame(body: &str) -> Vec<u8> {
        let mut framed = Vec::new();
        framed.extend_from_slice(&(body.len() as u32).to_be_bytes());
        framed.extend_from_slice(body.as_bytes());
        framed
    }

    /// M3a FR2 兼容(新客户端 ← 旧服务端):RegisterAck 不带 observed_addr → None
    #[test]
    fn register_ack_without_observed_addr_decodes_as_none() {
        let body = r#"{"type":"register_ack","lease":{"data_port":9001,"token":[7,7,7,7,7,7,7,7,7,7,7,7,7,7,7,7]}}"#;
        match decode_relay_msg(&frame(body)).unwrap() {
            RelayMsg::RegisterAck { lease, observed_addr } => {
                assert_eq!(lease.map(|l| l.data_port), Some(9001));
                assert_eq!(observed_addr, None);
            }
            other => panic!("期望 RegisterAck, 实得 {:?}", other),
        }
    }

    /// M3a FR2 兼容(旧客户端 ← 新服务端):旧版 RegisterAck 结构(无 observed_addr 字段)
    /// 解码新服务端带 observed_addr 的应答——serde 默认忽略未知字段,不报错。
    /// 同时确认 None 时字段不落盘(线上字节与旧版一致)。
    #[test]
    fn register_ack_with_observed_addr_is_ignored_by_old_client() {
        #[derive(Deserialize, Debug)]
        #[serde(tag = "type", rename_all = "snake_case")]
        enum OldRelayMsg {
            RegisterAck { lease: Option<LeaseInfo> },
        }
        let new_ack = RelayMsg::RegisterAck {
            lease: Some(LeaseInfo { data_port: 9002, token: [1u8; 16] }),
            observed_addr: Some("198.51.100.7:40000".into()),
        };
        let json = serde_json::to_string(&new_ack).unwrap();
        assert!(json.contains("\"observed_addr\":\"198.51.100.7:40000\""));
        let old: OldRelayMsg = serde_json::from_str(&json).expect("旧客户端应忽略未知字段");
        match old {
            OldRelayMsg::RegisterAck { lease } => assert_eq!(lease.map(|l| l.data_port), Some(9002)),
        }

        // None 不序列化:与旧服务端字节形态一致
        let none_ack = RelayMsg::RegisterAck { lease: None, observed_addr: None };
        let json = serde_json::to_string(&none_ack).unwrap();
        assert!(!json.contains("observed_addr"), "None 不应落字段: {json}");
    }
}
