use serde::{Deserialize, Serialize};

/// Maximum size for control messages (4 MiB)
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Length of chunk stream header in bytes
pub const CHUNK_HEADER_LEN: usize = 12;

/// Protocol errors
#[derive(Debug, PartialEq, Clone)]
pub enum ProtocolError {
    /// Message exceeds maximum size
    MessageTooLarge { declared_len: usize, actual_len: usize },
    /// Declared length does not match actual data length
    LengthMismatch { declared: u32, actual: usize },
    /// Message is truncated
    Truncated,
    /// JSON deserialization error
    JsonError(String),
    /// Invalid data format
    InvalidData(String),
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtocolError::MessageTooLarge { declared_len, actual_len } => {
                write!(
                    f,
                    "消息过大: 声明长度 {}, 实际长度 {}, 最大允许 {} 字节",
                    declared_len,
                    actual_len,
                    CHUNK_SIZE
                )
            }
            ProtocolError::LengthMismatch { declared, actual } => {
                write!(f, "长度不匹配: 声明 {}, 实际 {}", declared, actual)
            }
            ProtocolError::Truncated => write!(f, "消息被截断"),
            ProtocolError::JsonError(msg) => write!(f, "JSON 解析错误: {}", msg),
            ProtocolError::InvalidData(msg) => write!(f, "无效数据: {}", msg),
        }
    }
}

impl std::error::Error for ProtocolError {}

/// v0.5.0 推送拒绝原因（OfferResp.accepted=false 时必给；老对端缺省按 Denied 处理）
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OfferDenyReason {
    /// 对方手动拒绝（或策略为 Deny）
    Denied,
    /// 对方确认超时未响应
    Timeout,
}

/// Control messages exchanged between peers
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMsg {
    Hello {
        name: String,
        fingerprint: String,
    },
    PairCodeSubmit {
        code: String,
    },
    PairResult {
        ok: bool,
    },
    ConsentGrant {
        name: String,
    },
    ConsentDeny,
    ConsentCancel,
    ListReq {
        share_id: String,
        #[serde(default)]
        path: String,
        cursor: u64,
        /// M-B5: RPC 多路化——请求关联 ID,响应原样回填(default 兼容老对端)
        #[serde(default)]
        msg_id: u64,
    },
    ListResp {
        entries: Vec<FileEntry>,
        next_cursor: Option<u64>,
        /// 响应对应的请求 msg_id(0 = 无关联的推送类消息)
        #[serde(default)]
        msg_id: u64,
    },
    SharesReq {
        /// M-B5: RPC 多路化关联 ID(default 兼容老对端发来的无字段消息)
        #[serde(default)]
        msg_id: u64,
    },
    SharesResp {
        shares: Vec<ShareInfo>,
        #[serde(default)]
        msg_id: u64,
    },
    /// 共享区内容变化通知（数据方 watchdog 检测到目录指纹变化后主动推送，
    /// 浏览方据此自动刷新列表；无需回复）
    SharesChanged {
        share_id: String,
    },
    MetaReq {
        share_id: String,
        path: String,
        /// M-B5: RPC 多路化关联 ID(default 兼容老对端)
        #[serde(default)]
        msg_id: u64,
    },
    MetaResp {
        job_id: u64,
        file_name: String,
        total_size: u64,
        chunk_hashes: Vec<String>,
        /// v0.10.0 大文件整体 hash(发送方后台算完存注册表,此处回填)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_hash: Option<String>,
        /// M-B5: 响应对应的请求 msg_id
        #[serde(default)]
        msg_id: u64,
    },
    FetchReq {
        job_id: u64,
        chunk: u32,
    },
    OfferReq {
        job_id: u64,
        files: Vec<OfferFile>,
    },
    OfferResp {
        accepted: bool,
        save_dir: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<OfferDenyReason>,
        /// v0.11.0 T18:小文件秒传位图——与请求 files 等长逐位对应,
        /// true=发送方可跳过该文件。不再回显 hash(A5 oracle 隐私修复)
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skip_bitmap: Vec<bool>,
    },
    BitmapReq {
        job_id: u64,
    },
    BitmapResp {
        job_id: u64,
        bits: Vec<u8>,
    },
    TransferCtl {
        job_id: u64,
        action: TransferAction,
    },
    /// 推送模式大文件传输完成信号（接收方 finalize 后回发；offer_id 关联 OfferReq 任务）
    JobDone {
        job_id: u64,
        offer_id: u64,
    },
    /// 拉取模式接收回执（接收方 finalize 落盘成功后回发；发送方据此发 SourceDone、
    /// 清理 sender 任务——"收到对端确认才算完成"，与 push 模式 JobDone 对齐）
    RecvAck {
        job_id: u64,
    },
    /// v0.10.0 推送模式逐窗接收进度(接收方每收完一个块窗口回发;
    /// 发送方据此在 UI 呈现"对方已收"第二条进度。旧端收到按未知消息忽略)
    RecvProgress {
        job_id: u64,
        cumulative_bytes: u64,
    },
    /// 推送模式接收失败信号（接收方任一阶段失败时回发；发送方立即 Failed 收场，
    /// 不再等 120s JobDone 超时）
    JobFailed {
        offer_id: u64,
        reason: String,
    },
    /// v0.6.0 远程文件操作请求(安卓端文件浏览器;老版本收到按未知消息忽略)
    ShareRename {
        share_id: String,
        path: String,
        new_name: String,
        /// M-B5: 请求关联 ID,响应原样回填(default 兼容老对端)
        #[serde(default)]
        msg_id: u64,
    },
    ShareDelete {
        share_id: String,
        path: String,
        #[serde(default)]
        msg_id: u64,
    },
    ShareMkdir {
        share_id: String,
        path: String,
        #[serde(default)]
        msg_id: u64,
    },
    /// 文件操作结果(ok=false 时 error 给用户可读原因)
    ShareOpResult {
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default)]
        msg_id: u64,
    },
    Goodbye,
    /// M3a FR6:对端已移除对本机的信任(移除信任即时断连降级)。无载荷——
    /// 收端以控制连接的 TLS 对端指纹定位会话,与 Goodbye 同为纯信号。
    /// wire 兼容:老版本枚举无此变体 → serde "unknown variant" 解码失败 →
    /// ctrl_loop 走既有"解码失败断连"路径收场。连接本就要被发送方关闭,
    /// 老端终态等价(干净断连,不崩溃),只是少了"被移除信任"的 toast 提示。
    TrustBroken,
    /// ===== M3b FR2 通道质量探测消息(设计 16 §2) =====
    ///
    /// **wire 兼容定案:这四个变体只允许出现在独立探测 bi 流上,绝不从
    /// ctrl 流发送**(routing::probe 模块注释有完整权衡)。要点:
    /// - 老版本对端对探测流没有任何 accept_bi 消费者(会话连接上 ctrl 流
    ///   已在握手期被取走),探测数据只会静静缓冲,老端不解析、不断连、
    ///   无感知;探测方等不到响应 → 超时 → 通道记录拉黑探测。
    /// - 新版本 ctrl_loop 收到这四个变体(异常/第三方实现误投 ctrl 流)
    ///   一律忽略不断连——回包或断连都会伤及混版本组网,忽略最稳。
    /// RTT 探测:发 Ping{nonce},对端原样回 Pong{nonce},×3 取中位。
    Ping {
        nonce: u64,
    },
    /// Ping 的应答(nonce 原样回显)
    Pong {
        nonce: u64,
    },
    /// 带宽阶梯探测:发端先发本消息,紧跟 `size` 字节裸数据(不走帧),
    /// 对端读满即丢、回 ProbeResp{nonce}。计时=发出到收到应答全程。
    ProbeReq {
        size: u32,
        nonce: u64,
    },
    /// ProbeReq 的应答(nonce 原样回显;数据已丢弃,不回灌——出站速率版)
    ProbeResp {
        nonce: u64,
    },
}

/// Transfer control actions
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TransferAction {
    Pause,
    Resume,
    Cancel,
    Throttle { max_streams: u32 },
}

/// File entry in a directory listing
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
}

/// Information about a share
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ShareInfo {
    pub id: String,
    pub alias: String,
}

/// File offered for transfer
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct OfferFile {
    pub name: String,
    pub size: u64,
    pub rel_dir: String,
    /// v0.10.0 整体 SHA-256(小文件 offer 时即带;大文件后台算、经 MetaResp 补)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
}

/// Encode a control message with 4-byte big-endian length prefix
pub fn encode_control(m: &ControlMsg) -> Vec<u8> {
    let json = serde_json::to_vec(m).expect("控制消息序列化失败");
    let len = json.len() as u32;
    let mut buf = Vec::with_capacity(4 + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    buf
}

/// Decode a control message from bytes with length prefix validation
pub fn decode_control(bytes: &[u8]) -> Result<ControlMsg, ProtocolError> {
    if bytes.len() < 4 {
        return Err(ProtocolError::Truncated);
    }

    let declared_len = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;

    // Check if declared length matches actual length
    if declared_len != bytes.len() - 4 {
        return Err(ProtocolError::LengthMismatch {
            declared: declared_len as u32,
            actual: bytes.len() - 4,
        });
    }

    decode_control_body(&bytes[4..])
}

/// Decode a control message body (length prefix already stripped, e.g. when the
/// prefix was consumed separately while reading from a stream)
pub fn decode_control_body(body: &[u8]) -> Result<ControlMsg, ProtocolError> {
    // Check if message exceeds maximum size
    if body.len() > CHUNK_SIZE {
        return Err(ProtocolError::MessageTooLarge {
            declared_len: body.len(),
            actual_len: body.len(),
        });
    }

    serde_json::from_slice(body).map_err(|e| ProtocolError::JsonError(e.to_string()))
}

/// Chunk stream header (12 bytes little-endian)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkStreamHeader {
    pub job_id: u64,
    pub chunk: u32,
}

impl ChunkStreamHeader {
    /// Encode header to 12-byte little-endian buffer
    pub fn encode(&self) -> [u8; CHUNK_HEADER_LEN] {
        let mut buf = [0u8; CHUNK_HEADER_LEN];
        buf[..8].copy_from_slice(&self.job_id.to_le_bytes());
        buf[8..12].copy_from_slice(&self.chunk.to_le_bytes());
        buf
    }

    /// Decode header from 12-byte little-endian buffer
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < CHUNK_HEADER_LEN {
            return Err(ProtocolError::Truncated);
        }

        let job_id = u64::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]);
        let chunk = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);

        Ok(Self { job_id, chunk })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_roundtrip_all_variants() {
        let msgs = vec![
            ControlMsg::Hello {
                name: "甲".into(),
                fingerprint: "ab".repeat(32),
            },
            ControlMsg::PairCodeSubmit {
                code: "123456".into(),
            },
            ControlMsg::PairResult { ok: true },
            ControlMsg::ListReq {
                share_id: "s1".into(),
                path: "subdir".to_string(),
                cursor: 0,
                msg_id: 7,
            },
            ControlMsg::ListResp {
                entries: vec![FileEntry {
                    name: "test.txt".into(),
                    is_dir: false,
                    size: 1024,
                    mtime: 1234567890,
                }],
                next_cursor: Some(100),
                msg_id: 7,
            },
            ControlMsg::SharesReq { msg_id: 8 },
            ControlMsg::SharesResp {
                shares: vec![ShareInfo {
                    id: "share1".into(),
                    alias: "My Share".into(),
                }],
                msg_id: 8,
            },
            ControlMsg::SharesChanged {
                share_id: "share1".into(),
            },
            ControlMsg::MetaReq {
                share_id: "share1".into(),
                path: "/file.txt".into(),
                msg_id: 9,
            },
            ControlMsg::MetaResp {
                job_id: 9,
                file_name: "a.iso".into(),
                total_size: 1 << 40,
                chunk_hashes: vec!["cd".repeat(32)],
                file_hash: None,
                msg_id: 0,
            },
            ControlMsg::FetchReq {
                job_id: 5,
                chunk: 10,
            },
            ControlMsg::OfferReq {
                job_id: 3,
                files: vec![OfferFile {
                    name: "video.mp4".into(),
                    size: 1024 * 1024 * 500,
                    rel_dir: "/movies".into(),
                    hash: None,
                }],
            },
            ControlMsg::OfferResp {
                accepted: false,
                save_dir: None,
                reason: Some(OfferDenyReason::Denied),
                skip_bitmap: vec![],
            },
            ControlMsg::BitmapReq { job_id: 7 },
            ControlMsg::BitmapResp {
                job_id: 1,
                bits: vec![0b1010_0001],
            },
            ControlMsg::TransferCtl {
                job_id: 2,
                action: TransferAction::Pause,
            },
            ControlMsg::TransferCtl {
                job_id: 3,
                action: TransferAction::Resume,
            },
            ControlMsg::TransferCtl {
                job_id: 4,
                action: TransferAction::Cancel,
            },
            ControlMsg::JobDone { job_id: 9, offer_id: 4 },
            ControlMsg::RecvAck { job_id: 11 },
            ControlMsg::JobFailed { offer_id: 4, reason: "IO 错误".into() },
            ControlMsg::Goodbye,
            ControlMsg::TrustBroken,
        ];

        for m in &msgs {
            let encoded = encode_control(m);
            let decoded = decode_control(&encoded).unwrap();
            assert_eq!(&decoded, m, "Roundtrip failed for {:?}", m);
        }
    }

    #[test]
    fn chunk_header_roundtrip() {
        let h = ChunkStreamHeader {
            job_id: u64::MAX,
            chunk: 42,
        };
        let b = h.encode();
        assert_eq!(b.len(), CHUNK_HEADER_LEN);
        assert_eq!(ChunkStreamHeader::decode(&b).unwrap(), h);
    }

    #[test]
    fn chunk_header_edge_cases() {
        // Test with zero values
        let h = ChunkStreamHeader {
            job_id: 0,
            chunk: 0,
        };
        let b = h.encode();
        assert_eq!(b.len(), CHUNK_HEADER_LEN);
        assert_eq!(ChunkStreamHeader::decode(&b).unwrap(), h);

        // Test with large chunk number
        let h = ChunkStreamHeader {
            job_id: 12345,
            chunk: u32::MAX,
        };
        let b = h.encode();
        assert_eq!(b.len(), CHUNK_HEADER_LEN);
        assert_eq!(ChunkStreamHeader::decode(&b).unwrap(), h);
    }

    #[test]
    fn oversize_and_truncated_rejected() {
        let big = vec![0u8; 5 * 1024 * 1024];
        assert!(decode_control(&big).is_err());

        let enc = encode_control(&ControlMsg::Goodbye);
        assert!(decode_control(&enc[..enc.len() - 1]).is_err());
    }

    #[test]
    fn length_mismatch_rejected() {
        // Create a valid message
        let msg = ControlMsg::Hello {
            name: "test".into(),
            fingerprint: "ab".repeat(32),
        };
        let mut enc = encode_control(&msg);

        // Modify the length prefix to be wrong
        enc[0] = 0xFF;
        enc[1] = 0xFF;
        enc[2] = 0xFF;
        enc[3] = 0xFF;

        let result = decode_control(&enc);
        assert!(result.is_err());
        match result.unwrap_err() {
            ProtocolError::LengthMismatch { .. } => (),
            _ => panic!("Expected LengthMismatch error"),
        }
    }

    #[test]
    fn serialize_uses_snake_case() {
        let msg = ControlMsg::TransferCtl {
            job_id: 1,
            action: TransferAction::Pause,
        };
        let encoded = encode_control(&msg);
        let json_str = String::from_utf8_lossy(&encoded[4..]);

        // Verify the serialized JSON uses snake_case
        assert!(json_str.contains("\"type\":\"transfer_ctl\""));
        assert!(json_str.contains("\"action\":\"pause\""));
    }

    #[test]
    fn list_req_backward_compatibility() {
        // Old format without path field (should default to empty string)
        let old_json = r#"{"type":"list_req","share_id":"s1","cursor":0}"#;

        let decoded = serde_json::from_str::<ControlMsg>(old_json);
        assert!(decoded.is_ok());

        match decoded.unwrap() {
            ControlMsg::ListReq { share_id, path, cursor, .. } => {
                assert_eq!(share_id, "s1");
                assert_eq!(path, ""); // Should default to empty string
                assert_eq!(cursor, 0);
            }
            _ => panic!("Expected ListReq"),
        }

        // New format with path field
        let new_json = r#"{"type":"list_req","share_id":"s1","path":"subdir","cursor":0}"#;

        let decoded = serde_json::from_str::<ControlMsg>(new_json);
        assert!(decoded.is_ok());

        match decoded.unwrap() {
            ControlMsg::ListReq { share_id, path, cursor, .. } => {
                assert_eq!(share_id, "s1");
                assert_eq!(path, "subdir");
                assert_eq!(cursor, 0);
            }
            _ => panic!("Expected ListReq"),
        }
    }

    #[test]
    fn offer_resp_reason_snake_case_roundtrip() {
        // 拒绝带原因：序列化必须是小写 "timeout"/"denied"
        let msg = ControlMsg::OfferResp {
            accepted: false,
            save_dir: None,
            reason: Some(OfferDenyReason::Timeout),
            skip_bitmap: vec![],
        };
        let enc = encode_control(&msg);
        let json = String::from_utf8_lossy(&enc[4..]).to_string();
        assert!(json.contains("\"reason\":\"timeout\""), "实际: {}", json);
        assert_eq!(decode_control(&enc).unwrap(), msg);

        let msg2 = ControlMsg::OfferResp {
            accepted: false,
            save_dir: None,
            reason: Some(OfferDenyReason::Denied),
            skip_bitmap: vec![],
        };
        let enc2 = encode_control(&msg2);
        assert!(String::from_utf8_lossy(&enc2[4..]).contains("\"reason\":\"denied\""));
        assert_eq!(decode_control(&enc2).unwrap(), msg2);
    }

    #[test]
    fn offer_resp_reason_omitted_when_none() {
        // 接受时 reason 必须不出现在 JSON 里；且老格式（无 reason 键）能解析
        let msg = ControlMsg::OfferResp {
            accepted: true,
            save_dir: Some("D:/dl".into()),
            reason: None,
            skip_bitmap: vec![],
        };
        let enc = encode_control(&msg);
        let json = String::from_utf8_lossy(&enc[4..]).to_string();
        assert!(!json.contains("reason"), "实际: {}", json);

        let old_json = r#"{"type":"offer_resp","accepted":true,"save_dir":"D:/dl"}"#;
        let decoded: ControlMsg = serde_json::from_str(old_json).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn recv_progress_roundtrip() {
        let m = ControlMsg::RecvAck { job_id: 42 };
        let _ = m; // 既有锚

        let msg = ControlMsg::RecvProgress { job_id: 7, cumulative_bytes: 123456 };
        let buf = encode_control(&msg);
        let decoded = decode_control(&buf).unwrap();
        assert_eq!(decoded, msg);
    }

    #[test]
    fn offer_file_hash_default_compat() {
        // 旧端不带 hash 字段 → None(向后兼容)
        let old = r#"{"name":"a.mp4","size":10,"rel_dir":""}"#;
        let f: OfferFile = serde_json::from_str(old).unwrap();
        assert_eq!(f.hash, None);
        let new = r#"{"name":"a.mp4","size":10,"rel_dir":"","hash":"abc"}"#;
        let f2: OfferFile = serde_json::from_str(new).unwrap();
        assert_eq!(f2.hash.as_deref(), Some("abc"));
    }

    #[test]
    fn meta_resp_and_offer_resp_defaults_compat() {
        // MetaResp 旧 JSON(无 file_hash/msg_id) 能解 + 新字段 default
        let old_mr = r#"{"type":"meta_resp","job_id":1,"file_name":"a","total_size":1,"chunk_hashes":[]}"#;
        let mr: ControlMsg = serde_json::from_str(old_mr).unwrap();
        match mr {
            ControlMsg::MetaResp { job_id, file_name, total_size, chunk_hashes, file_hash, .. } => {
                assert_eq!(job_id, 1);
                assert_eq!(file_name, "a");
                assert_eq!(total_size, 1);
                assert_eq!(chunk_hashes, Vec::<String>::new());
                assert_eq!(file_hash, None);
            }
            _ => panic!("Expected MetaResp"),
        }

        // OfferResp 空 skip_bitmap 序列化不含 "skip" 键
        let resp = ControlMsg::OfferResp { accepted: true, save_dir: None, reason: None, skip_bitmap: vec![] };
        let buf = encode_control(&resp);
        assert!(!String::from_utf8_lossy(&buf[4..]).contains("skip"));
    }

    #[test]
    fn trust_broken_wire_form_and_old_peer_compat() {
        // M3a FR6:TrustBroken 的线上形态钉死为 {"type":"trust_broken"}(纯信号无载荷)
        let enc = encode_control(&ControlMsg::TrustBroken);
        assert_eq!(
            String::from_utf8_lossy(&enc[4..]),
            r#"{"type":"trust_broken"}"#,
            "TrustBroken 线上形态变更属协议破坏,须走契约先行流程"
        );
        assert_eq!(decode_control(&enc).unwrap(), ControlMsg::TrustBroken);

        // 老端兼容路径实证:老版本枚举没有 trust_broken 变体,收到该消息时
        // serde 报 unknown variant(此处用同样未知的变体名模拟老端视角),
        // 错误干净返回——ctrl_loop 按既有"解码失败断连"收场,不 panic 不半包
        let old_peer_view: Result<ControlMsg, _> =
            serde_json::from_str(r#"{"type":"a_variant_older_versions_never_knew"}"#);
        assert!(old_peer_view.is_err(), "未知变体必须报解码错误(老端据此断连)");
    }

    #[test]
    fn probe_msg_roundtrip_and_wire_form() {
        // M3b FR2:探测四变体 roundtrip + 线上形态钉死(type 蛇形、nonce 字段)
        let msgs = [
            ControlMsg::Ping { nonce: 7 },
            ControlMsg::Pong { nonce: 7 },
            ControlMsg::ProbeReq { size: 4 * 1024 * 1024, nonce: u64::MAX },
            ControlMsg::ProbeResp { nonce: 42 },
        ];
        for m in msgs {
            let enc = encode_control(&m);
            assert_eq!(decode_control(&enc).unwrap(), m, "{:?} roundtrip", m);
        }
        let json = |m: &ControlMsg| String::from_utf8_lossy(&encode_control(m)[4..]).to_string();
        assert_eq!(json(&ControlMsg::Ping { nonce: 1 }), r#"{"type":"ping","nonce":1}"#);
        assert_eq!(json(&ControlMsg::Pong { nonce: 2 }), r#"{"type":"pong","nonce":2}"#);
        assert_eq!(
            json(&ControlMsg::ProbeReq { size: 65536, nonce: 3 }),
            r#"{"type":"probe_req","size":65536,"nonce":3}"#
        );
        assert_eq!(json(&ControlMsg::ProbeResp { nonce: 4 }), r#"{"type":"probe_resp","nonce":4}"#);
    }

    #[test]
    fn rpc_msg_id_backward_compat() {
        // M-B5: 请求/响应消息老 JSON(无 msg_id) 能解,msg_id 默认 0
        let cases = [
            r#"{"type":"list_req","share_id":"s","cursor":0}"#,
            r#"{"type":"list_resp","entries":[],"next_cursor":null}"#,
            r#"{"type":"shares_req"}"#,
            r#"{"type":"shares_resp","shares":[]}"#,
            r#"{"type":"meta_req","share_id":"s","path":"p"}"#,
            r#"{"type":"meta_resp","job_id":1,"file_name":"a","total_size":1,"chunk_hashes":[]}"#,
            r#"{"type":"share_op_result","ok":true}"#,
            r#"{"type":"shares_req","msg_id":42}"#,
            r#"{"type":"shares_resp","shares":[],"msg_id":42}"#,
        ];
        for j in cases {
            let m: ControlMsg = serde_json::from_str(j)
                .unwrap_or_else(|e| panic!("解析失败 {}: {}", j, e));
            let s = serde_json::to_string(&m).unwrap();
            let back: ControlMsg = serde_json::from_str(&s).unwrap();
            assert_eq!(back, m);
        }
        // 带字段时回填正确
        let req: ControlMsg =
            serde_json::from_str(r#"{"type":"meta_req","share_id":"s","path":"p","msg_id":99}"#).unwrap();
        match req {
            ControlMsg::MetaReq { msg_id, .. } => assert_eq!(msg_id, 99),
            _ => panic!("Expected MetaReq"),
        }
    }
}
