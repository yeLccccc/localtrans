// Task M3: 状态快照 + 条件等待（spec §7.1.6 / §7.1.5 / §7.1.3 state 端点）
//
// 显式 TestSnapshot（ADR-3）：不序列化 AppState 内部结构（内部类型一重构
// 测试全红），映射为裁剪后的显式契约结构。spec §7.1.6 的字段是基线——
// 内部有的映射，没有的置 null 并在 docs/contracts/test-api.md "暂缺"登记
// （discovery_stats：core 发现层未暴露广播收发计数，恒 null）。
//
// 取锁习惯（与 commands.rs 只读命令一致）：持锁只做克隆/收集，
// 组装与序列化全部在锁外；锁是 tokio 锁，await 期间尽快释放。

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;

use crate::AppState;

/// 快照 schema 版本（独立于 apiVersion 演进，spec NFR-3；破坏性变更必须升版）
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

// ---------------------------------------------------------------------------
// 显式快照结构（serde camelCase 对齐前端/契约）
// ---------------------------------------------------------------------------

/// Rust 侧测试快照（spec §7.1.6 基线，字段表见契约文档 §5.3）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TestSnapshot {
    pub schema_version: u32,
    pub self_device: SelfDeviceBrief,
    /// 合并视图（本地发现 + 中结名册 + 信任表兜底），与 UI 设备页同源同序
    pub devices: Vec<DeviceBrief>,
    /// 已建立 QUIC 会话的对端（connected_fps 视角 + 信任表可信判定）
    pub sessions: Vec<SessionBrief>,
    /// 活动传输（未 removed 的卡片 dto 视图，与前端 TransferDto 同源）
    pub transfers: Vec<TransferBrief>,
    /// 全部卡片（含 removed），card 引擎关联与终态判定
    pub cards: Vec<CardBrief>,
    /// 发现层广播收发计数——core 未暴露，恒 null（契约"暂缺"登记）
    pub discovery_stats: Option<DiscoveryStats>,
}

/// 本机设备（identity + 配置拼装；spec 基线的 self_device: DeviceInfo 裁剪）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SelfDeviceBrief {
    /// 指纹 hex（64 字符）
    pub id: String,
    /// 设备名（config.device_name）
    pub name: String,
    /// 配对短码（identity.short_code）
    pub short_code: String,
    /// 是否隐身
    pub hidden: bool,
}

/// 对端设备摘要（id/名称/状态/地址）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceBrief {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub online: bool,
    pub connected: bool,
    pub via_relay: bool,
}

/// 会话摘要（对端/可信与否；"连接真的活着"= QUIC 会话已建立）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionBrief {
    /// 对端指纹 hex
    pub peer: String,
    pub trusted: bool,
    /// 展示名（信任表别名>配对名>设备表名），查不到为 null
    pub name: Option<String>,
}

/// 活动传输摘要（id/方向/状态/bytesDone/bytesTotal/速率等）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct TransferBrief {
    /// job_id（即 card_id，16 位小写 hex 字符串——与前端 TransferDto.job_id
    /// 序列化口径一致，u64 直接出 JSON 数字会丢精度风险且与 DTO 割裂）
    pub id: String,
    pub name: String,
    /// pull | push
    pub direction: String,
    /// pending/active/paused/cancelling/done/failed/interrupted
    pub state: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub speed_bps: u64,
    /// 对端指纹 hex
    pub peer: String,
    /// push 方向：对端累计已确认字节
    pub remote_done: u64,
    /// 秒传命中
    pub instant: bool,
    pub fail_reason: Option<String>,
}

/// 卡片摘要（cardId/engineId/计数器/终态）
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CardBrief {
    /// card_id（16 位小写 hex 字符串，与 transfers[].id 同值）
    pub card_id: String,
    /// 引擎 job_id（None=尚未绑定/无引擎任务）
    pub engine_id: Option<String>,
    pub state: String,
    pub bytes_done: u64,
    pub bytes_total: u64,
    /// 终态（done/failed/interrupted）——终态吸收一切事件不再迁移
    pub terminal: bool,
    /// 两级删除第一级：true = 不在活动列表（历史可见）
    pub removed: bool,
}

/// 发现层统计（spec 基线：广播收发计数，网络问题排查抓手）。
/// core 的 DiscoveryHandle 未暴露计数——结构先立契约，当前恒 None。
#[derive(Serialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryStats {
    pub broadcasts_sent: u64,
    pub broadcasts_received: u64,
}

// ---------------------------------------------------------------------------
// 纯映射函数（单测直测；AppState 在单测中不可构造，见模块测试注释）
// ---------------------------------------------------------------------------

/// id 统一口径：16 位小写 hex（与 localtrans_core::serde_compat::u64_hex_string 一致）
fn hex_id(v: u64) -> String {
    format!("{v:016x}")
}

/// 合并设备表（本地发现 + 中结名册 + 信任表兜底）→ DeviceBrief[]。
/// 直接复用 core::device_merge（与 UI device-list 事件同源同序同判定，
/// 壳层 commands::merge_devices 同款转发口径）。
pub fn map_devices(
    local: &[localtrans_core::discovery::DeviceInfo],
    roster: &[localtrans_core::relay::proto::RemoteDevice],
    connected: &HashSet<String>,
    aliases: &HashMap<String, String>,
    trusted: &[(String, String, u64)],
) -> Vec<DeviceBrief> {
    localtrans_core::device_merge::merge_devices(local, roster, connected, aliases, trusted)
        .into_iter()
        .map(|m| DeviceBrief {
            id: m.fingerprint,
            name: m.name,
            addr: m.addr,
            online: m.online,
            connected: m.connected,
            via_relay: m.via_relay,
        })
        .collect()
}

/// connected_fps（QUIC 会话在连对端）× 信任指纹集 → SessionBrief[]（按指纹排序）。
/// trusted_fps 为信任表指纹 hex 快照（调用方在信任锁内收集，锁外传纯数据）；
/// display_names 供"查得到就叫得出名字"（来自设备合并视图）。
pub fn map_sessions(
    connected: &HashSet<String>,
    trusted_fps: &HashSet<String>,
    display_names: &HashMap<String, String>,
) -> Vec<SessionBrief> {
    let mut peers: Vec<&String> = connected.iter().collect();
    peers.sort();
    peers
        .into_iter()
        .map(|fp| SessionBrief {
            peer: fp.clone(),
            trusted: trusted_fps.contains(fp),
            name: display_names.get(fp).cloned(),
        })
        .collect()
}

/// 卡片表 → (活动传输 briefs, 全部卡片 briefs)，均按 card_id 升序（确定性）。
/// transfers 视图 = 未 removed 的卡片（与 AppState::snapshot_dtos 同过滤）；
/// cards 视图 = 全部卡片（含 removed，断言两级删除行为用）。
pub fn map_cards(
    cards: &HashMap<u64, crate::transfer_state::TransferCard>,
) -> (Vec<TransferBrief>, Vec<CardBrief>) {
    let mut ids: Vec<u64> = cards.keys().copied().collect();
    ids.sort();
    let mut transfers = Vec::new();
    let mut briefs = Vec::new();
    for id in ids {
        let card = &cards[&id];
        let dto = &card.dto;
        briefs.push(CardBrief {
            card_id: hex_id(id),
            engine_id: card.engine_id.map(hex_id),
            state: dto.state.clone(),
            bytes_done: dto.done,
            bytes_total: dto.total,
            terminal: matches!(
                dto.state.as_str(),
                "done" | "failed" | "interrupted"
            ),
            removed: card.removed,
        });
        if !card.removed {
            transfers.push(TransferBrief {
                id: hex_id(id),
                name: dto.name.clone(),
                direction: dto.direction.clone(),
                state: dto.state.clone(),
                bytes_done: dto.done,
                bytes_total: dto.total,
                speed_bps: dto.speed_bps,
                peer: dto.peer.clone(),
                remote_done: dto.remote_done,
                instant: dto.instant,
                fail_reason: dto.fail_reason.clone(),
            });
        }
    }
    (transfers, briefs)
}

/// 从 AppState 组装 TestSnapshot。持锁只做克隆/收集（与 commands.rs
/// 只读命令同习惯），组装在锁外；不可构造失败——全部是内存态读取。
pub async fn build_snapshot(state: &AppState) -> TestSnapshot {
    // ---- 逐锁收集（每把锁 await 内只 clone/收集，立刻释放） ----
    let devices_raw = state.devices.lock().await.clone();
    let connected = state.connected_fps.lock().await.clone();
    let roster = state.relay_roster.lock().await.clone();
    let trust = state.trust.lock().await;
    let aliases = localtrans_core::device_merge::alias_map(&trust);
    let trusted = localtrans_core::device_merge::trusted_pairs(&trust);
    let trusted_fps: HashSet<String> =
        trust.all_peers().iter().map(|p| hex::encode(p.fingerprint)).collect();
    drop(trust);
    let (transfers, cards) = map_cards(&*state.transfers.lock().await);
    let (device_name, hidden_cfg) = {
        let cfg = state.config.read().await;
        (cfg.device_name.clone(), cfg.hidden)
    };
    let hidden = state.hidden.load(std::sync::atomic::Ordering::Relaxed) || hidden_cfg;

    // ---- 锁外组装 ----
    let devices = map_devices(&devices_raw, &roster, &connected, &aliases, &trusted);
    let display_names: HashMap<String, String> =
        devices.iter().map(|d| (d.id.clone(), d.name.clone())).collect();
    let sessions = map_sessions(&connected, &trusted_fps, &display_names);

    TestSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        self_device: SelfDeviceBrief {
            id: hex::encode(state.identity.fingerprint()),
            name: device_name,
            short_code: state.identity.short_code(),
            hidden,
        },
        devices,
        sessions,
        transfers,
        cards,
        // core 发现层未暴露广播收发计数（DiscoveryHandle 仅 watch 表 + 命令通道），
        // 契约"暂缺"登记；core 暴露后在此接真值
        discovery_stats: None,
    }
}

// ---------------------------------------------------------------------------
// /api/state/wait：路径求值器 + 轮询循环（spec §7.1.5）
// ---------------------------------------------------------------------------

/// 点分路径求值：`a.b.c` 走对象键、纯数字段走数组下标（`a.0.b`）、
/// 数组上的 `length` 段合成数组长度（`a.length`）。
/// 返回叶节点克隆（含 length 合成值）；路径不存在（含对标量继续下钻、
/// 数组越界、空段）返回 None。快照体量小，克隆换实现简单。
pub fn resolve_path(root: &Value, path: &str) -> Option<Value> {
    let mut cur = root.clone();
    for seg in path.split('.') {
        if seg.is_empty() {
            return None;
        }
        cur = match cur {
            Value::Object(map) => map.get(seg)?.clone(),
            Value::Array(arr) => {
                if seg == "length" {
                    Value::from(arr.len())
                } else {
                    let idx: usize = seg.parse().ok()?;
                    arr.get(idx)?.clone()
                }
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// wait 断言算子（spec §7.1.5 封闭集合）
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitOp {
    Eq,
    Ne,
    Gte,
    Lte,
    Contains,
    Exists,
    Empty,
}

pub fn parse_op(s: &str) -> Option<WaitOp> {
    match s {
        "eq" => Some(WaitOp::Eq),
        "ne" => Some(WaitOp::Ne),
        "gte" => Some(WaitOp::Gte),
        "lte" => Some(WaitOp::Lte),
        "contains" => Some(WaitOp::Contains),
        "exists" => Some(WaitOp::Exists),
        "empty" => Some(WaitOp::Empty),
        _ => None,
    }
}

/// 标量相等（string/number/bool + null≡null；数字经 f64 比较，1 与 1.0 相等）。
/// 数组/对象或类型不匹配 → false。
fn scalar_eq(obs: &Value, exp: &Value) -> bool {
    match (obs, exp) {
        (Value::String(a), Value::String(b)) => a == b,
        (Value::Number(a), Value::Number(b)) => match (a.as_f64(), b.as_f64()) {
            (Some(x), Some(y)) => x == y,
            _ => a == b,
        },
        (Value::Bool(a), Value::Bool(b)) => a == b,
        (Value::Null, Value::Null) => true,
        _ => false,
    }
}

/// 观测值 × 期望值 → 断言结果。observed None = 路径不存在：
/// exists 为 false，其余 op 一律 false（契约语义，勿单边改动）。
pub fn apply_op(op: WaitOp, observed: Option<&Value>, expected: Option<&Value>) -> bool {
    let Some(obs) = observed else {
        return false;
    };
    match op {
        WaitOp::Exists => true, // 路径存在即真（含显式 null 值）
        WaitOp::Empty => match obs {
            Value::Null => true,
            Value::String(s) => s.is_empty(),
            Value::Array(a) => a.is_empty(),
            _ => false,
        },
        WaitOp::Eq => expected.map(|e| scalar_eq(obs, e)).unwrap_or(false),
        // ne 是 eq 的取反（类型不匹配视为不相等）
        WaitOp::Ne => expected.map(|e| !scalar_eq(obs, e)).unwrap_or(false),
        WaitOp::Gte | WaitOp::Lte => {
            // 仅 number：任一侧非数字 → false
            let (Some(x), Some(y)) = (obs.as_f64(), expected.and_then(|e| e.as_f64())) else {
                return false;
            };
            if op == WaitOp::Gte { x >= y } else { x <= y }
        }
        WaitOp::Contains => match (obs, expected) {
            (Value::String(hay), Some(Value::String(needle))) => hay.contains(needle.as_str()),
            _ => false,
        },
    }
}

/// /api/state/wait 数据源
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WaitSource {
    /// Rust TestSnapshot（本模块 build_snapshot）
    Transfers,
    /// 前端 Pinia 聚合快照（bridge `state` 动作）
    App,
}

pub const DEFAULT_WAIT_TIMEOUT_MS: u64 = 10_000;
pub const DEFAULT_WAIT_INTERVAL_MS: u64 = 500;

/// /api/state/wait 入参（校验抽纯函数直测）。
/// intervalMs 可调小是给单测用的，缺省 500（契约文档注明）。
#[derive(Clone, Debug, PartialEq)]
pub struct WaitRequest {
    pub source: WaitSource,
    pub path: String,
    pub op: WaitOp,
    pub value: Option<Value>,
    pub timeout_ms: u64,
    pub interval_ms: u64,
}

pub fn wait_params(body: &Value) -> Result<WaitRequest, String> {
    let source = match body.get("source").and_then(|v| v.as_str()) {
        Some("transfers") => WaitSource::Transfers,
        Some("app") => WaitSource::App,
        Some(other) => return Err(format!("source 必须为 transfers 或 app，收到: {other:?}")),
        None => return Err("source 必须为 transfers 或 app".to_string()),
    };
    let path = body
        .get("path")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "path 必须为非空点分路径（如 devices.length）".to_string())?;
    let op_str = body
        .get("op")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "op 必须为 eq/ne/gte/lte/contains/exists/empty 之一".to_string())?;
    let op = parse_op(&op_str)
        .ok_or_else(|| format!("op 必须为 eq/ne/gte/lte/contains/exists/empty 之一，收到: {op_str:?}"))?;

    let has_value = !matches!(body.get("value"), None | Some(Value::Null));
    let value = body.get("value").cloned().filter(|v| !v.is_null());
    let needs_value = matches!(op, WaitOp::Eq | WaitOp::Ne | WaitOp::Gte | WaitOp::Lte | WaitOp::Contains);
    if needs_value && !has_value {
        return Err(format!("op {op_str} 需要提供 value"));
    }
    if !needs_value && has_value {
        return Err(format!("op {op_str} 不接受 value"));
    }

    let opt_u64 = |key: &str, default: u64| -> Result<u64, String> {
        match body.get(key) {
            None | Some(Value::Null) => Ok(default),
            Some(v) => v.as_u64().ok_or_else(|| format!("{key} 必须为非负整数")),
        }
    };
    let timeout_ms = opt_u64("timeoutMs", DEFAULT_WAIT_TIMEOUT_MS)?;
    let interval_ms = opt_u64("intervalMs", DEFAULT_WAIT_INTERVAL_MS)?;
    if interval_ms == 0 {
        return Err("intervalMs 必须 >= 1（0 会热轮询）".to_string());
    }
    Ok(WaitRequest { source, path, op, value, timeout_ms, interval_ms })
}

/// 一次 wait 的观测结果
#[derive(Clone, Debug, PartialEq)]
pub struct WaitOutcome {
    pub matched: bool,
    /// 命中时的观测值；未命中/超时为最后一拍的观测值（路径从未存在则 None）
    pub observed: Option<Value>,
    pub polls: u32,
    pub elapsed_ms: u64,
}

/// 轮询直至条件满足或超时（spec §7.1.5：默认 500ms 一拍）。
/// fetch 每拍返回最新快照；Err 立即中止上抛（source=app 的桥错误
/// ——未就绪/超时——不靠轮询自愈，由调用方直接回错误响应）。
/// 首拍即查再判超时：timeoutMs=0 表示"只查一次"。
pub async fn wait_loop<F, Fut, E>(
    mut fetch: F,
    path: &str,
    op: WaitOp,
    value: Option<Value>,
    timeout_ms: u64,
    interval_ms: u64,
) -> Result<WaitOutcome, E>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value, E>>,
{
    let started = std::time::Instant::now();
    let deadline = started + Duration::from_millis(timeout_ms);
    let mut polls: u32 = 0;
    // 末次观测值（超时 detail 用）：每拍未命中后赋值，首拍前无读
    let mut last: Option<Value>;
    loop {
        let snap = fetch().await?;
        polls += 1;
        let observed = resolve_path(&snap, path);
        let hit = apply_op(op, observed.as_ref(), value.as_ref());
        if hit {
            return Ok(WaitOutcome {
                matched: true,
                observed,
                polls,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        last = observed;
        if std::time::Instant::now() >= deadline {
            return Ok(WaitOutcome {
                matched: false,
                observed: last,
                polls,
                elapsed_ms: started.elapsed().as_millis() as u64,
            });
        }
        tokio::time::sleep(Duration::from_millis(interval_ms)).await;
    }
}

// ---------------------------------------------------------------------------
// 单元测试（feature 门控，`cargo test -p localtrans --features test-api` 覆盖）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use crate::transfer_state::TransferCard;
    use serde_json::json;

    fn dto(state: &str) -> crate::TransferDto {
        crate::TransferDto {
            job_id: 1,
            name: "t.bin".into(),
            total: 100,
            done: 0,
            state: state.into(),
            speed_bps: 0,
            peer: "ab".repeat(32),
            direction: "pull".into(),
            local_role: "destination".into(),
            health: None,
            started_at_ms: None,
            finished_at_ms: None,
            source_path: None,
            fail_reason: None,
            remote_done: 0,
            instant: false,
            queue_pos: None,
            batch_id: None,
            children: vec![],
            parts_id: None,
        }
    }

    // -----------------------------------------------------------------
    // 路径求值器（全形态：对象/数组下标/length/不存在/链式下钻）
    // -----------------------------------------------------------------

    #[test]
    fn 路径_对象嵌套与标量() {
        let root = json!({ "a": { "b": { "c": "hello" }, "n": 7, "t": true, "z": null } });
        assert_eq!(resolve_path(&root, "a.b.c"), Some(json!("hello")));
        assert_eq!(resolve_path(&root, "a.n"), Some(json!(7)));
        assert_eq!(resolve_path(&root, "a.t"), Some(json!(true)));
        assert_eq!(resolve_path(&root, "a.z"), Some(json!(null)));
        assert_eq!(resolve_path(&root, "a"), Some(json!({ "b": { "c": "hello" }, "n": 7, "t": true, "z": null })));
    }

    #[test]
    fn 路径_数组下标与length() {
        let root = json!({ "devices": [
            { "id": "aa", "name": "A" },
            { "id": "bb", "name": "B" },
        ]});
        assert_eq!(resolve_path(&root, "devices.0.name"), Some(json!("A")));
        assert_eq!(resolve_path(&root, "devices.1.id"), Some(json!("bb")));
        assert_eq!(resolve_path(&root, "devices.length"), Some(json!(2)));
        // length 后继续下钻 → 数字上无键 → None
        assert_eq!(resolve_path(&root, "devices.length.x"), None);
        // 越界 / 非数字下标 / 对象用 length（对象无 length 语义）
        assert_eq!(resolve_path(&root, "devices.2"), None);
        assert_eq!(resolve_path(&root, "devices.abc"), None);
        assert_eq!(resolve_path(&root, "a.length"), None, "对象上的 length 不是数组长度");
        // 顶层数组
        let arr_root = json!([10, 20]);
        assert_eq!(resolve_path(&arr_root, "1"), Some(json!(20)));
        assert_eq!(resolve_path(&arr_root, "length"), Some(json!(2)));
    }

    #[test]
    fn 路径_不存在形态一律None() {
        let root = json!({ "a": { "b": 1 }, "s": "str", "empty": [] });
        assert_eq!(resolve_path(&root, "x"), None, "顶层键不存在");
        assert_eq!(resolve_path(&root, "a.x"), None, "嵌套键不存在");
        assert_eq!(resolve_path(&root, "s.y"), None, "标量继续下钻");
        assert_eq!(resolve_path(&root, ""), None, "空路径");
        assert_eq!(resolve_path(&root, "a..b"), None, "空段");
        assert_eq!(resolve_path(&root, "a."), None, "尾空段");
        assert_eq!(resolve_path(&root, "empty.0"), None, "空数组取 0");
        // 显式 null 与不存在可区分（exists 语义的根基）
        let with_null = json!({ "a": null });
        assert_eq!(resolve_path(&with_null, "a"), Some(json!(null)));
        assert_eq!(resolve_path(&with_null, "b"), None);
    }

    // -----------------------------------------------------------------
    // apply_op 全 op 矩阵
    // -----------------------------------------------------------------

    #[test]
    fn op_eq_标量三型与类型不匹配() {
        let (s, n, b) = (Some(&json!("abc")), Some(&json!(5)), Some(&json!(true)));
        assert!(apply_op(WaitOp::Eq, s, Some(&json!("abc"))));
        assert!(!apply_op(WaitOp::Eq, s, Some(&json!("abd"))));
        assert!(apply_op(WaitOp::Eq, n, Some(&json!(5))));
        assert!(apply_op(WaitOp::Eq, n, Some(&json!(5.0))), "1 与 1.0 经 f64 相等");
        assert!(!apply_op(WaitOp::Eq, n, Some(&json!(6))));
        assert!(apply_op(WaitOp::Eq, b, Some(&json!(true))));
        assert!(!apply_op(WaitOp::Eq, b, Some(&json!(false))));
        // 类型不匹配 → false
        assert!(!apply_op(WaitOp::Eq, s, n));
        assert!(!apply_op(WaitOp::Eq, n, Some(&json!("5"))));
        // null ≡ null（failReason eq null 类断言）
        assert!(apply_op(WaitOp::Eq, Some(&json!(null)), Some(&json!(null))));
        assert!(!apply_op(WaitOp::Eq, Some(&json!(null)), s));
        // 数组/对象不参与 eq
        assert!(!apply_op(WaitOp::Eq, Some(&json!([1])), Some(&json!([1]))));
    }

    #[test]
    fn op_ne_eq取反_含类型不匹配() {
        assert!(apply_op(WaitOp::Ne, Some(&json!("abc")), Some(&json!("abd"))));
        assert!(!apply_op(WaitOp::Ne, Some(&json!("abc")), Some(&json!("abc"))));
        // 类型不匹配视为不相等
        assert!(apply_op(WaitOp::Ne, Some(&json!("5")), Some(&json!(5))));
    }

    #[test]
    fn op_gte_lte_仅数字() {
        let v = Some(&json!(5));
        assert!(apply_op(WaitOp::Gte, v, Some(&json!(5))));
        assert!(apply_op(WaitOp::Gte, v, Some(&json!(4))));
        assert!(!apply_op(WaitOp::Gte, v, Some(&json!(6))));
        assert!(apply_op(WaitOp::Lte, v, Some(&json!(5))));
        assert!(apply_op(WaitOp::Lte, v, Some(&json!(6))));
        assert!(!apply_op(WaitOp::Lte, v, Some(&json!(4))));
        // 任一侧非 number → false
        assert!(!apply_op(WaitOp::Gte, Some(&json!("5")), Some(&json!(4))));
        assert!(!apply_op(WaitOp::Gte, v, Some(&json!("4"))));
        assert!(!apply_op(WaitOp::Lte, Some(&json!(true)), Some(&json!(4))));
        assert!(!apply_op(WaitOp::Gte, Some(&json!(null)), Some(&json!(4))));
    }

    #[test]
    fn op_contains_仅字符串包含() {
        assert!(apply_op(WaitOp::Contains, Some(&json!("传输任务")), Some(&json!("传输"))));
        assert!(!apply_op(WaitOp::Contains, Some(&json!("设置")), Some(&json!("传输"))));
        assert!(apply_op(WaitOp::Contains, Some(&json!("")), Some(&json!(""))));
        // 非字符串观测/期望 → false
        assert!(!apply_op(WaitOp::Contains, Some(&json!(5)), Some(&json!("5"))));
        assert!(!apply_op(WaitOp::Contains, Some(&json!("abc")), Some(&json!(3))));
    }

    #[test]
    fn op_exists_存在即真含null_缺失为假() {
        assert!(apply_op(WaitOp::Exists, Some(&json!(null)), None));
        assert!(apply_op(WaitOp::Exists, Some(&json!(0)), None));
        assert!(apply_op(WaitOp::Exists, Some(&json!("")), None));
        assert!(apply_op(WaitOp::Exists, Some(&json!([])), None));
        // 路径不存在：所有 op 一律 false（含 exists）
        for op in [WaitOp::Eq, WaitOp::Ne, WaitOp::Gte, WaitOp::Lte, WaitOp::Contains, WaitOp::Exists, WaitOp::Empty] {
            assert!(!apply_op(op, None, Some(&json!(1))), "{op:?} 路径不存在应为 false");
        }
    }

    #[test]
    fn op_empty_空串空数组null_其余假() {
        assert!(apply_op(WaitOp::Empty, Some(&json!("")), None));
        assert!(apply_op(WaitOp::Empty, Some(&json!([])), None));
        assert!(apply_op(WaitOp::Empty, Some(&json!(null)), None));
        assert!(!apply_op(WaitOp::Empty, Some(&json!("x")), None));
        assert!(!apply_op(WaitOp::Empty, Some(&json!([1])), None));
        assert!(!apply_op(WaitOp::Empty, Some(&json!(0)), None), "0 是数字不是空");
        assert!(!apply_op(WaitOp::Empty, Some(&json!({})), None), "空对象不匹配 empty");
    }

    // -----------------------------------------------------------------
    // wait 入参校验
    // -----------------------------------------------------------------

    #[test]
    fn wait参数_合法形态与缺省() {
        let r = wait_params(&json!({
            "source": "transfers", "path": "devices.length", "op": "gte", "value": 1
        }))
        .unwrap();
        assert_eq!(r.source, WaitSource::Transfers);
        assert_eq!(r.path, "devices.length");
        assert_eq!(r.op, WaitOp::Gte);
        assert_eq!(r.value, Some(json!(1)));
        assert_eq!(r.timeout_ms, DEFAULT_WAIT_TIMEOUT_MS, "缺省 10s");
        assert_eq!(r.interval_ms, DEFAULT_WAIT_INTERVAL_MS, "缺省 500ms");

        let r = wait_params(&json!({
            "source": "app", "path": "toast.toasts.length", "op": "empty",
            "timeoutMs": 1500, "intervalMs": 50
        }))
        .unwrap();
        assert_eq!((r.source, r.op, r.value.as_ref()), (WaitSource::App, WaitOp::Empty, None));
        assert_eq!((r.timeout_ms, r.interval_ms), (1500, 50));

        // value 显式 null 视为未提供（exists/empty 合法）
        assert!(wait_params(&json!({ "source": "app", "path": "a.b", "op": "exists", "value": null })).is_ok());
    }

    #[test]
    fn wait参数_非法op_source_路径_数值() {
        // 非法 op（冒烟负路径同款）
        let err = wait_params(&json!({ "source": "transfers", "path": "a", "op": "pfx", "value": 1 }))
            .unwrap_err();
        assert!(err.contains("op"), "报错应点名 op: {err}");
        // 非法 source
        let err = wait_params(&json!({ "source": "rust", "path": "a", "op": "eq", "value": 1 }))
            .unwrap_err();
        assert!(err.contains("source"), "报错应点名 source: {err}");
        // 缺 path / 空 path
        assert!(wait_params(&json!({ "source": "app", "op": "exists" })).is_err());
        assert!(wait_params(&json!({ "source": "app", "path": "  ", "op": "exists" })).is_err());
        // 需要 value 的 op 缺 value
        assert!(wait_params(&json!({ "source": "app", "path": "a", "op": "eq" })).is_err());
        assert!(wait_params(&json!({ "source": "app", "path": "a", "op": "contains" })).is_err());
        // 不接受 value 的 op 带 value
        assert!(wait_params(&json!({ "source": "app", "path": "a", "op": "empty", "value": "" })).is_err());
        assert!(wait_params(&json!({ "source": "app", "path": "a", "op": "exists", "value": 1 })).is_err());
        // 数值非法
        assert!(wait_params(&json!({
            "source": "app", "path": "a", "op": "eq", "value": 1, "timeoutMs": "8s"
        }))
        .is_err());
        assert!(wait_params(&json!({
            "source": "app", "path": "a", "op": "eq", "value": 1, "intervalMs": 0
        }))
        .is_err());
    }

    // -----------------------------------------------------------------
    // wait_loop：首拍即中 / 中途翻转 / 超时带末次观测 / fetch 错误上抛
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn wait_首拍即中() {
        let mut calls = 0;
        let snap = json!({ "devices": [ { "id": "a" }, { "id": "b" } ] });
        let out = wait_loop(
            || {
                calls += 1;
                std::future::ready(Ok::<_, String>(snap.clone()))
            },
            "devices.length",
            WaitOp::Gte,
            Some(json!(2)),
            5_000,
            50,
        )
        .await
        .unwrap();
        assert!(out.matched);
        assert_eq!(out.observed, Some(json!(2)));
        assert_eq!(out.polls, 1, "首拍即中不再轮询");
        assert_eq!(calls, 1);
    }

    #[tokio::test]
    async fn wait_中途翻转短间隔轮询() {
        // 50ms 一拍，第 3 拍 devices 从 0 翻转到 2（按拍数翻转，不依赖真实时刻）
        let mut ticks = 0;
        let out = wait_loop(
            || {
                ticks += 1;
                let n = if ticks >= 3 { 2 } else { 0 };
                std::future::ready(Ok::<_, String>(json!({ "devices": vec![json!({}); n] })))
            },
            "devices.length",
            WaitOp::Gte,
            Some(json!(2)),
            5_000,
            50,
        )
        .await
        .unwrap();
        assert!(out.matched, "第 3 拍翻转后命中");
        assert_eq!(out.polls, 3);
        assert_eq!(out.observed, Some(json!(2)));
    }

    #[tokio::test]
    async fn wait_超时带末次观测值() {
        let started = std::time::Instant::now();
        let out = wait_loop(
            || std::future::ready(Ok::<_, String>(json!({ "devices": vec![json!({}); 3] }))),
            "devices.length",
            WaitOp::Gte,
            Some(json!(999)),
            250,
            50,
        )
        .await
        .unwrap();
        assert!(!out.matched);
        assert_eq!(out.observed, Some(json!(3)), "detail 应带末次观测值 3");
        assert!(out.polls >= 3, "250ms/50ms 至少 3 拍: {}", out.polls);
        assert!(started.elapsed() >= Duration::from_millis(250), "必须等满超时才判负");
    }

    #[tokio::test]
    async fn wait_超时路径从未存在则观测为None() {
        let out = wait_loop(
            || std::future::ready(Ok::<_, String>(json!({ "devices": [] }))),
            "nope.nope",
            WaitOp::Exists,
            None,
            150,
            50,
        )
        .await
        .unwrap();
        assert!(!out.matched);
        assert_eq!(out.observed, None, "路径从未存在 → lastValue null");
    }

    #[tokio::test]
    async fn wait_fetch错误立即上抛不自愈() {
        let err = wait_loop::<_, _, String>(
            || std::future::ready(Err("BRIDGE_NOT_READY: 前端未就绪".to_string())),
            "a",
            WaitOp::Exists,
            None,
            5_000,
            50,
        )
        .await
        .unwrap_err();
        assert!(err.contains("BRIDGE_NOT_READY"));
    }

    // -----------------------------------------------------------------
    // 快照映射纯函数（AppState 在单测中不可构造——SessionManager/发现线程/
    // 信号量等组装过重，build_snapshot 的组装层由冒烟 smoke-m3.mjs 端到端覆盖，
    // 此处直测三个纯映射函数）
    // -----------------------------------------------------------------

    fn device_info(name: &str, fp_byte: u8) -> localtrans_core::discovery::DeviceInfo {
        localtrans_core::discovery::DeviceInfo {
            name: name.to_string(),
            fingerprint: [fp_byte; 32],
            addr: format!("192.168.1.10:{}", localtrans_core::ports::quic_port()).parse().unwrap(),
            last_seen: std::time::Instant::now(),
        }
    }

    /// M5 执行记录曾报"devices[].address 序列化为 undefined"——排查结论：
    /// 字段名就是 `addr`（core discovery::DeviceInfo.addr → device_merge →
    /// DeviceBrief.addr，蛇形 rust → camelCase 序列化后同名），查 `.address`
    /// 自然 undefined。本测试把字段名与真值钉死在序列化层，防回归 +
    /// 防再误读（契约 §5.3.1 字段表即 `addr`）。
    #[test]
    fn 映射_devices_addr字段名与真值钉死() {
        let local = vec![device_info("B 机", 0x22)];
        let briefs = map_devices(&local, &[], &HashSet::new(), &HashMap::new(), &[]);
        assert_eq!(briefs.len(), 1);
        let v = serde_json::to_value(&briefs[0]).unwrap();
        // 字段名钉死：addr（不是 address）
        assert!(v.get("addr").is_some(), "字段名是 addr: {v}");
        assert!(v.get("address").is_none(), "不存在 address 字段（M5 误读源）");
        // 真值钉死：发现层地址原样透传（ip:quic端口）
        assert_eq!(v["addr"], json!(format!("192.168.1.10:{}", localtrans_core::ports::quic_port())));
        // 其余契约字段齐全
        for key in ["id", "name", "online", "connected", "viaRelay"] {
            assert!(v.get(key).is_some(), "缺字段 {key}: {v}");
        }
    }

    #[test]
    fn 映射_devices_合并排序与连接态() {
        let local = vec![device_info("B 机", 0x22), device_info("A 机", 0x11)];
        let mut connected = HashSet::new();
        connected.insert("11".repeat(32));
        let aliases = HashMap::new();
        let trusted = vec![];
        let briefs = map_devices(&local, &[], &connected, &aliases, &trusted);
        // core merge 排序：connected 优先
        assert_eq!(briefs.len(), 2);
        assert_eq!(briefs[0].id, "11".repeat(32));
        assert!(briefs[0].connected);
        assert_eq!(briefs[0].name, "A 机");
        assert!(!briefs[0].via_relay);
        assert_eq!(briefs[1].id, "22".repeat(32));
        assert!(!briefs[1].connected);
        assert!(briefs[0].online, "last_seen 刚刚 → online");

        // 信任表兜底条目（离线已配对设备常驻）
        let trusted = vec![("33".repeat(32), "C 机".to_string())];
        let briefs = map_devices(&local, &[], &connected, &aliases, &trusted);
        assert_eq!(briefs.len(), 3);
        assert!(briefs.iter().any(|b| b.id == "33".repeat(32) && !b.online && b.addr.is_empty()));
    }

    #[test]
    fn 映射_sessions_可信判定与名字解析() {
        let dir = std::env::temp_dir().join(format!("localtrans-test-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut trust = localtrans_core::identity::TrustStore::load(&dir);
        let mut peer = localtrans_core::identity::TrustedPeer {
            fingerprint: [0x11; 32],
            name: "A 机配对名".into(),
            alias: String::new(),
            paired_at: 0,
            perms: Default::default(),
        };
        trust.upsert(peer.clone());
        peer.fingerprint = [0x22; 32];
        peer.name = "B 机".into();
        trust.upsert(peer);
        let _ = std::fs::remove_dir_all(&dir);
        // 调用方在信任锁内收集的指纹 hex 快照
        let trusted_fps: HashSet<String> =
            trust.all_peers().iter().map(|p| hex::encode(p.fingerprint)).collect();

        let mut connected = HashSet::new();
        connected.insert("11".repeat(32)); // 已配对
        connected.insert("ff".repeat(32)); // 未配对（会话在连但信任表无）
        let mut display = HashMap::new();
        display.insert("ff".repeat(32), "路过设备".to_string());

        let sessions = map_sessions(&connected, &trusted_fps, &display);
        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions[0].peer, "11".repeat(32), "按指纹排序");
        assert!(sessions[0].trusted);
        assert_eq!(sessions[0].name, None, "展示名只来自设备表，此测试未提供");
        assert_eq!(sessions[1].peer, "ff".repeat(32));
        assert!(!sessions[1].trusted);
        assert_eq!(sessions[1].name.as_deref(), Some("路过设备"));
    }

    #[test]
    fn 映射_cards_活动与全量视图及终态() {
        let mut cards = HashMap::new();
        // 活动卡（有引擎绑定）
        let mut active = TransferCard::new(dto("active"));
        active.dto.done = 40;
        active.dto.total = 100;
        active.dto.speed_bps = 1024;
        active.engine_id = Some(7);
        cards.insert(0x4000_0000_0002, active);
        // 已完成未删除（活动列表仍可见）
        let mut done = TransferCard::new(dto("done"));
        done.dto.done = 100;
        done.dto.total = 100;
        cards.insert(0x4000_0000_0001, done);
        // removed 的失败卡（历史可见，活动列表不可见）
        let mut removed = TransferCard::new(dto("failed"));
        removed.removed = true;
        removed.engine_id = Some(9);
        cards.insert(0x4000_0000_0003, removed);

        let (transfers, briefs) = map_cards(&cards);
        // cards 全量 3 张、按 card_id 升序
        assert_eq!(briefs.len(), 3);
        assert_eq!(briefs[0].card_id, format!("{:016x}", 0x4000_0000_0001u64));
        assert!(briefs[0].terminal, "done 是终态");
        assert!(!briefs[0].removed);
        assert_eq!(briefs[0].engine_id, None);
        assert_eq!(briefs[1].card_id, format!("{:016x}", 0x4000_0000_0002u64));
        assert!(!briefs[1].terminal, "active 非终态");
        assert_eq!(briefs[1].engine_id, Some(format!("{:016x}", 7)));
        assert_eq!((briefs[1].bytes_done, briefs[1].bytes_total), (40, 100));
        assert_eq!(briefs[2].removed, true);
        assert!(briefs[2].terminal, "failed 是终态");
        // transfers 活动视图 = 未 removed 的 2 张（去掉失败卡）
        assert_eq!(transfers.len(), 2);
        assert_eq!(transfers[0].id, briefs[0].card_id, "transfers[].id == cards[].cardId");
        assert_eq!(transfers[0].state, "done");
        assert_eq!(transfers[1].state, "active");
        assert_eq!(transfers[1].direction, "pull");
        assert_eq!(transfers[1].speed_bps, 1024);
        assert_eq!(transfers[1].peer, "ab".repeat(32));
        assert_eq!(transfers[1].fail_reason, None);
    }

    #[test]
    fn 快照_序列化camelCase与discoveryStats暂缺() {
        let snap = TestSnapshot {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            self_device: SelfDeviceBrief {
                id: "ab".repeat(32),
                name: "本机".into(),
                short_code: "1234".into(),
                hidden: false,
            },
            devices: vec![],
            sessions: vec![],
            transfers: vec![],
            cards: vec![],
            discovery_stats: None,
        };
        let v = serde_json::to_value(&snap).unwrap();
        assert_eq!(v["schemaVersion"], json!(1));
        assert_eq!(v["selfDevice"]["id"], json!("ab".repeat(32)));
        assert_eq!(v["selfDevice"]["shortCode"], json!("1234"));
        assert_eq!(v["devices"], json!([]));
        assert_eq!(v["discoveryStats"], json!(null), "core 未暴露计数 → null（契约暂缺）");
        // camelCase 全字段抽查
        let t = TransferBrief {
            id: "1".repeat(16),
            name: "x".into(),
            direction: "pull".into(),
            state: "active".into(),
            bytes_done: 1,
            bytes_total: 2,
            speed_bps: 3,
            peer: "p".into(),
            remote_done: 0,
            instant: false,
            fail_reason: None,
        };
        let tv = serde_json::to_value(&t).unwrap();
        for key in ["id", "bytesDone", "bytesTotal", "speedBps", "remoteDone", "failReason"] {
            assert!(tv.get(key).is_some(), "缺字段 {key}: {tv}");
        }
        let c = CardBrief {
            card_id: "1".repeat(16),
            engine_id: None,
            state: "done".into(),
            bytes_done: 1,
            bytes_total: 1,
            terminal: true,
            removed: false,
        };
        let cv = serde_json::to_value(&c).unwrap();
        for key in ["cardId", "engineId", "terminal", "removed"] {
            assert!(cv.get(key).is_some(), "缺字段 {key}: {cv}");
        }
    }
}
