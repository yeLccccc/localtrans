// Task M4: /api/invoke 白名单命令代理（spec §7.1.9）
//
// 编排器偶尔需要 UI 事件到不了的角落（如磁盘任务表、网络体检），invoke
// 白名单是受控出口：const 清单逐条手写调用分支，**不设全命令代理**。
// 选列原则（宁缺勿滥）：
// - ReadOnly：纯读取（设置/设备/信任表/卡片/磁盘任务/中继/网络/指纹/名片）；
// - Mutating：`clear_completed_transfers`（终态卡片 view 级清理，数据与
//   transfers.json 均保留）+ `connect`/`push_files`/`push_files_rel`（M6
//   场景编排需要，理由见下方"传输发起为何入列"）+ `add_by_card`（M3a T3
//   名片粘贴添加：写 probe_targets 重探表并可触发对端配对弹窗，语义同 connect）；
// - 破坏性命令（删除/解绑/清信任/停止引擎/写配置/弹 UAC/
//   打开资源管理器/文件系统任意读 expand_local_paths）绝不入列。
//
// 传输发起为何入列（M6 决策，记录进 docs/contracts/test-api.md §5.5）：
// - UI 的文件选择走 OS 原生对话框（rfd），WebView DOM 无法驱动，
//   传输场景必须语义注入（connect 建会话 + push_files 给定本地路径）；
// - 路径白名单无法穷举（fixture 目录随仓库变），故不设路径过滤，靠
//   Token 门控（测试网段内持有 token 的编排器本就是"信任的操作者"）；
// - 配对确认**不**入列（grant_consent/deny_consent/submit_pair_code 均
//   排除）：配对是安全敏感交互，走 UI 点击 btn-grant / code-input 保持
//   真实用户流程——这是 PC↔PC 场景脚本的核心断言路径。
//
// 执行方式：直接调用 commands.rs 的命令函数（tauri 命令本质是普通函数）。
// State 参数经 `app.state::<AppState>()` 传递，AppHandle 传 clone，
// 入参从 args 的 serde_json::Value 反序列化——按命令实际签名逐分支
// 手写，类型由编译器保证。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::Manager;

use super::{ok_envelope, parse_json_body, ApiState};

/// 白名单条目分类（spec §7.1.9）。序列化为 "ReadOnly"/"Mutating"。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
pub enum Class {
    ReadOnly,
    Mutating,
}

/// 白名单（封闭清单：新增须同步 docs/contracts/test-api.md §5.5）。
/// 未列入的命令一律 403 INVOKE_NOT_ALLOWED。
pub const ALLOWED: &[(&str, Class)] = &[
    // ---- 只读：设置 / 设备 / 信任 / 卡片 ----
    ("get_settings", Class::ReadOnly),
    ("list_devices", Class::ReadOnly),
    ("list_trusted", Class::ReadOnly),
    ("list_transfers", Class::ReadOnly),
    ("get_pairing_pending", Class::ReadOnly),
    // ---- 只读：磁盘任务 / 恢复候选 ----
    ("list_disk_jobs", Class::ReadOnly),
    ("pending_resume_jobs", Class::ReadOnly),
    // ---- 只读：网络与中继状态 ----
    ("relay_status", Class::ReadOnly),
    ("get_network_status", Class::ReadOnly),
    ("get_device_fingerprint", Class::ReadOnly),
    // ---- 只读：本机名片文本（M3a T3，card-exchange 场景 A 侧取卡） ----
    ("get_business_card", Class::ReadOnly),
    ("remove_trusted", Class::Mutating), // M3a: 测试夹具信任管理
    // ---- 只读：通道探测记录表（M3b T4，routing-probe 场景探测记录断言） ----
    ("list_channels", Class::ReadOnly),
    // ---- 只读：手动单对端快检（M3c T2 通道面板「重新探测」入口；只更新
    //      内存通道表——force-relay / 通道面板场景数据刷新编排用） ----
    ("probe_now_peer", Class::ReadOnly),
    // ---- 只读：秒传分片目录存在性 ----
    ("has_parts", Class::ReadOnly),
    // ---- 变更：终态卡片 view 级清理（数据保留） ----
    ("clear_completed_transfers", Class::Mutating),
    // ---- 变更（M6）：传输发起三命令 ----
    // 原生文件选择对话框无法 DOM 驱动，场景编排必须语义注入（见模块头注释）
    ("connect", Class::Mutating),
    ("push_files", Class::Mutating),
    ("push_files_rel", Class::Mutating),
    // ---- 变更（M3a T3）：名片粘贴添加——写 probe_targets 重探表 + 可触发
    // 对端配对弹窗，语义同 connect（card-exchange 场景 B 侧注卡入口） ----
    ("add_by_card", Class::Mutating),
];

/// 查白名单：命中返回 Class
pub fn lookup(cmd: &str) -> Option<Class> {
    ALLOWED.iter().find(|(c, _)| *c == cmd).map(|(_, k)| *k)
}

/// detail.allowed 载荷：全部允许项（含 Class），403 与契约文档同源
pub fn allowed_detail() -> Value {
    json!(ALLOWED
        .iter()
        .map(|(cmd, class)| json!({ "cmd": cmd, "class": class }))
        .collect::<Vec<_>>())
}

/// /api/invoke 入参：cmd 必填非空字符串；args 可缺省（{}）/null/对象。
pub fn invoke_params(body: &Value) -> Result<(String, Value), String> {
    let cmd = body
        .get("cmd")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "cmd 必须为非空字符串".to_string())?;
    let args = match body.get("args") {
        None | Some(Value::Null) => json!({}),
        Some(v @ Value::Object(_)) => v.clone(),
        Some(_) => return Err("args 必须为对象（命令参数名到值的映射）".to_string()),
    };
    Ok((cmd, args))
}

/// 必填字符串参数（has_parts.job_id 等）
fn str_arg(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("args.{key} 必须为非空字符串"))
}

/// 必填字符串数组参数（push_files.local_paths）
fn str_vec_arg(args: &Value, key: &str) -> Result<Vec<String>, String> {
    let arr = args
        .get(key)
        .and_then(|v| v.as_array())
        .ok_or_else(|| format!("args.{key} 必须为字符串数组"))?;
    if arr.is_empty() {
        return Err(format!("args.{key} 必须至少包含一个路径"));
    }
    arr.iter()
        .enumerate()
        .map(|(i, v)| {
            v.as_str()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| format!("args.{key}[{i}] 必须为非空字符串"))
        })
        .collect()
}

/// 必填 (路径, 相对目录) 对数组参数（push_files_rel.items）
fn pairs_arg(args: &Value, key: &str) -> Result<Vec<(String, String)>, String> {
    serde_json::from_value::<Vec<(String, String)>>(args.get(key).cloned().unwrap_or(Value::Null))
        .map_err(|_| format!("args.{key} 必须为 [路径, 相对目录] 二元组数组"))
}

/// 403 INVOKE_NOT_ALLOWED 包络（抽独立函数便于直测；detail.allowed
/// 列全部允许项，编排器据此自检）
pub fn not_allowed_response(cmd: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(super::err_envelope_with_detail(
            "INVOKE_NOT_ALLOWED",
            &format!("命令 {cmd:?} 不在 invoke 白名单"),
            json!({ "cmd": cmd, "allowed": allowed_detail() }),
        )),
    )
        .into_response()
}

/// 命令执行错误 → 500 INTERNAL 包络，detail 带 cmd 与错误信息
fn command_error_response(cmd: &str, err: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(super::err_envelope_with_detail(
            "INTERNAL",
            &format!("命令 {cmd} 执行失败"),
            json!({ "cmd": cmd, "error": err }),
        )),
    )
        .into_response()
}

/// Result<T: Serialize, String> → Result<Value, String>
fn to_json<T: Serialize>(r: Result<T, String>) -> Result<Value, String> {
    r.and_then(|v| serde_json::to_value(v).map_err(|e| format!("结果序列化失败: {e}")))
}

/// 执行白名单内命令（调用方已 lookup 通过）。逐分支手写转发，
/// 与 commands.rs 签名一一对应，类型安全。
async fn dispatch_allowed(
    app: &tauri::AppHandle,
    cmd: &str,
    args: &Value,
) -> Result<Value, String> {
    let state = app.state::<crate::AppState>();
    match cmd {
        "get_settings" => to_json(crate::commands::get_settings(state).await),
        "list_devices" => to_json(crate::commands::list_devices(state).await),
        "list_trusted" => to_json(crate::commands::list_trusted(state).await),
        "list_transfers" => to_json(crate::commands::list_transfers(state).await),
        "get_pairing_pending" => to_json(crate::commands::get_pairing_pending(state).await),
        "list_disk_jobs" => to_json(crate::commands::list_disk_jobs(state).await),
        "pending_resume_jobs" => to_json(crate::commands::pending_resume_jobs(state).await),
        "relay_status" => to_json(crate::commands::relay_status(state).await),
        "get_device_fingerprint" => to_json(crate::commands::get_device_fingerprint(state).await),
        // M3a FR2 起带 State(public_exit 取自中继客户端);其余仍为纯查询
        "get_network_status" => to_json(crate::commands::get_network_status(state).await),
        // M3a T3 名片：本机名片文本（只读）
        "get_business_card" => to_json(crate::commands::get_business_card(state).await),
        "remove_trusted" => {
            let fp_str = str_arg(args, "fingerprint")?;
            to_json(crate::commands::remove_trusted(state, fp_str).await)
        }
        // M3b T4 通道表只读查询（探测记录/评分观测；routing-probe 场景数据源）
        "list_channels" => to_json(crate::commands::list_channels(state).await),
        // M3c T2 手动单对端快检（只更新内存通道表；面板「重新探测」同款路径）
        "probe_now_peer" => {
            let fingerprint = str_arg(args, "fingerprint")?;
            to_json(crate::commands::probe_now_peer(state, fingerprint).await)
        }
        // 带参数的只读命令
        "has_parts" => {
            let job_id = str_arg(args, "job_id")?;
            to_json(crate::commands::has_parts(state, job_id).await)
        }
        // Mutating（handler 已 warn 高亮）
        "clear_completed_transfers" => {
            to_json(crate::commands::clear_completed_transfers(state).await)
        }
        // Mutating（M6 传输发起）：参数按 commands.rs 实际签名反序列化，
        // AppHandle 传 clone（connect 内部经 app.state 刷新 connected 徽章）
        "connect" => {
            let fingerprint = str_arg(args, "fingerprint")?;
            to_json(crate::commands::connect(state, app.clone(), fingerprint).await)
        }
        "push_files" => {
            let fingerprint = str_arg(args, "fingerprint")?;
            let local_paths = str_vec_arg(args, "local_paths")?;
            to_json(
                crate::commands::push_files(state, app.clone(), fingerprint, local_paths)
                    .await
                    .map(|card_id| json!(format!("{card_id:016x}"))),
            )
        }
        "push_files_rel" => {
            let fingerprint = str_arg(args, "fingerprint")?;
            let items = pairs_arg(args, "items")?;
            to_json(
                crate::commands::push_files_rel(state, app.clone(), fingerprint, items)
                    .await
                    .map(|card_id| json!(format!("{card_id:016x}"))),
            )
        }
        // Mutating（M3a T3 名片粘贴添加）：名片文本按 commands.rs 实际签名
        // 反序列化，AppHandle 传 clone（5s 回查任务 emit manual-probe-result）
        "add_by_card" => {
            let text = str_arg(args, "text")?;
            to_json(crate::commands::add_by_card(app.clone(), state, text).await)
        }
        // 白名单与 match 必须同步（编译器保证：lookup 只放行本清单）
        other => Err(format!("白名单分支缺失: {other}")),
    }
}

/// POST /api/invoke {cmd, args} → 白名单内命令结果。
/// Mutating 调用 tracing::warn! 高亮（带 cmd 与当前 run/step 上下文）。
pub(super) async fn invoke(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (cmd, args) = match invoke_params(&v) {
        Ok(t) => t,
        Err(m) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(super::err_envelope("BAD_REQUEST", &m)),
            )
                .into_response()
        }
    };
    let Some(class) = lookup(&cmd) else {
        tracing::warn!(target: "test_api", "invoke 拒绝白名单外命令: cmd={cmd}");
        return not_allowed_response(&cmd);
    };
    if class == Class::Mutating {
        let (run, step) = super::ring::global().test_context().snapshot();
        tracing::warn!(
            target: "test_api",
            "invoke Mutating 命令调用: cmd={cmd} runId={:?} step={:?}",
            run,
            step
        );
    }
    match dispatch_allowed(&st.app, &cmd, &args).await {
        Ok(data) => Json(ok_envelope(data)).into_response(),
        Err(e) => command_error_response(&cmd, &e),
    }
}

// ---------------------------------------------------------------------------
// 单元测试（feature 门控，`cargo test -p localtrans --features test-api`
// 覆盖）。真实命令执行需 AppHandle，由冒烟 smoke-m4.mjs 端到端覆盖；
// 此处直测白名单/入参/包络形态。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 白名单_命中与未命中() {
        assert_eq!(lookup("get_settings"), Some(Class::ReadOnly));
        assert_eq!(lookup("clear_completed_transfers"), Some(Class::Mutating));
        // M6 传输发起三命令入列（Mutating）
        assert_eq!(lookup("connect"), Some(Class::Mutating));
        assert_eq!(lookup("push_files"), Some(Class::Mutating));
        assert_eq!(lookup("push_files_rel"), Some(Class::Mutating));
        // M3a T3 名片两命令入列（读=ReadOnly / 粘贴添加=Mutating）
        assert_eq!(lookup("get_business_card"), Some(Class::ReadOnly));
        assert_eq!(lookup("add_by_card"), Some(Class::Mutating));
        // M3b T4 通道表只读查询入列（ReadOnly；探测选路观测面）
        assert_eq!(lookup("list_channels"), Some(Class::ReadOnly));
        // M3c T2 手动单对端快检入列（ReadOnly；通道面板重测入口）
        assert_eq!(lookup("probe_now_peer"), Some(Class::ReadOnly));
        // M3c T3 强制走中继开关不入列（set_* 写配置排除原则不变；e2e 经 ⋮ 菜单 UI 点击驱动）
        assert_eq!(lookup("set_force_relay"), None);
        assert_eq!(lookup("remove_transfer"), None, "破坏性命令不得入列");
        assert_eq!(lookup("remove_trusted"), Some(Class::Mutating));
        assert_eq!(lookup("destroy_disk_job"), None);
        assert_eq!(lookup("prepare_shutdown"), None);
        assert_eq!(lookup("save_settings"), None, "写配置不入列");
        assert_eq!(lookup("expand_local_paths"), None, "文件系统任意读不入列（NFR-8）");
        // 配对确认不入列：走 UI 点击保真实流程（模块头注释）
        assert_eq!(lookup("grant_consent"), None, "配对同意不入列（UI 真实流程）");
        assert_eq!(lookup("deny_consent"), None);
        assert_eq!(lookup("submit_pair_code"), None);
        assert_eq!(lookup("start_download"), None, "下载发起维持不入列（浏览页可 UI 驱动）");
        assert_eq!(lookup(""), None);
        // 白名单内不允许重复条目（重复会让 403 的 allowed 语义含混）
        let mut seen = std::collections::HashSet::new();
        for (cmd, _) in ALLOWED {
            assert!(seen.insert(*cmd), "白名单重复条目: {cmd}");
        }
    }

    #[test]
    fn 白名单_Mutating清单() {
        let mutating: Vec<&str> = ALLOWED
            .iter()
            .filter(|(_, c)| *c == Class::Mutating)
            .map(|(cmd, _)| *cmd)
            .collect();
        assert_eq!(
            mutating,
            vec![
                "clear_completed_transfers",
                "connect",
                "push_files",
                "push_files_rel",
                "add_by_card",
            ],
            "Mutating = 终态清理 + M6 传输发起三命令 + M3a 名片粘贴添加，顺序与 ALLOWED 一致"
        );
    }

    #[test]
    fn Class序列化_精确字符串() {
        assert_eq!(serde_json::to_value(Class::ReadOnly).unwrap(), json!("ReadOnly"));
        assert_eq!(serde_json::to_value(Class::Mutating).unwrap(), json!("Mutating"));
        // 反向不在契约内（仅序列化输出），不提供 Deserialize
    }

    #[test]
    fn allowed列表_形态与全集() {
        let v = allowed_detail();
        let arr = v.as_array().expect("allowed 为数组");
        assert_eq!(arr.len(), ALLOWED.len(), "与白名单同源同长");
        for (i, item) in arr.iter().enumerate() {
            assert_eq!(item["cmd"], json!(ALLOWED[i].0));
            assert!(
                item["class"] == json!("ReadOnly") || item["class"] == json!("Mutating"),
                "class 取值合法: {}",
                item["class"]
            );
        }
        assert!(arr.iter().any(|i| i["cmd"] == json!("get_settings")));
    }

    #[test]
    fn 入参_cmd必填_args对象或缺省() {
        let (cmd, args) = invoke_params(&json!({ "cmd": "get_settings" })).unwrap();
        assert_eq!((cmd.as_str(), args), ("get_settings", json!({})));
        // args null 等同缺省；显式对象透传
        let (_, args) = invoke_params(&json!({ "cmd": "has_parts", "args": null })).unwrap();
        assert_eq!(args, json!({}));
        let (_, args) =
            invoke_params(&json!({ "cmd": "has_parts", "args": { "job_id": "0000000000000001" } }))
                .unwrap();
        assert_eq!(args["job_id"], json!("0000000000000001"));
        // cmd 缺失 / 非字符串 / 空白 → Err
        assert!(invoke_params(&json!({})).is_err());
        assert!(invoke_params(&json!({ "cmd": 42 })).is_err());
        assert!(invoke_params(&json!({ "cmd": "  " })).is_err());
        // args 非对象 → Err
        assert!(invoke_params(&json!({ "cmd": "x", "args": [1] })).is_err());
        assert!(invoke_params(&json!({ "cmd": "x", "args": "y" })).is_err());
    }

    #[test]
    fn 入参_字符串数组参数形态() {
        let ok = json!({ "local_paths": ["C:/a.bin", "C:/dir/b.txt"] });
        assert_eq!(
            str_vec_arg(&ok, "local_paths").unwrap(),
            vec!["C:/a.bin", "C:/dir/b.txt"]
        );
        // 缺 key / 非数组 / 空数组 / 元素非字符串 / 空白字符串 → Err（报错点名 key）
        assert!(str_vec_arg(&json!({}), "local_paths").is_err());
        assert!(str_vec_arg(&json!({ "local_paths": "C:/a" }), "local_paths").is_err());
        let empty_err = str_vec_arg(&json!({ "local_paths": [] }), "local_paths").unwrap_err();
        assert!(empty_err.contains("local_paths"), "报错点名参数: {empty_err}");
        let bad = str_vec_arg(&json!({ "local_paths": ["ok", 42] }), "local_paths").unwrap_err();
        assert!(bad.contains("local_paths[1]"), "报错点名下标: {bad}");
        assert!(str_vec_arg(&json!({ "local_paths": ["  "] }), "local_paths").is_err());
    }

    #[test]
    fn 入参_二元组数组参数形态() {
        let ok = json!({ "items": [["C:/a.bin", "sub/dir"], ["C:/b.txt", ""]] });
        assert_eq!(
            pairs_arg(&ok, "items").unwrap(),
            vec![("C:/a.bin".into(), "sub/dir".into()), ("C:/b.txt".into(), "".into())]
        );
        // 缺 key / 非二元组 / 元素非字符串 → Err
        assert!(pairs_arg(&json!({}), "items").is_err());
        assert!(pairs_arg(&json!({ "items": [["only-path"]] }), "items").is_err());
        assert!(pairs_arg(&json!({ "items": [["p", "r", "x"]] }), "items").is_err());
        assert!(pairs_arg(&json!({ "items": [["p", 7]] }), "items").is_err());
        assert!(pairs_arg(&json!({ "items": "nope" }), "items").is_err());
    }

    /// Response → (status, 包络 JSON)
    async fn envelope_of(resp: Response) -> (StatusCode, Value) {
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX).await.expect("响应体可读");
        let v = serde_json::from_slice(&bytes).expect("响应体为 JSON");
        (parts.status, v)
    }

    #[tokio::test]
    async fn 未入列_403包络与allowed() {
        let (status, v) = envelope_of(not_allowed_response("remove_transfer")).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["error"]["code"], json!("INVOKE_NOT_ALLOWED"));
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap()
                .contains("remove_transfer"),
            "message 点名被拒命令"
        );
        let allowed = v["error"]["detail"]["allowed"]
            .as_array()
            .expect("detail.allowed 为数组");
        assert!(!allowed.is_empty(), "allowed 不得为空");
        assert_eq!(allowed.len(), ALLOWED.len());
        assert_eq!(v["error"]["detail"]["cmd"], json!("remove_transfer"));
    }

    #[tokio::test]
    async fn 命令执行错误_500包络带cmd与错误() {
        let (status, v) = envelope_of(command_error_response("get_settings", "配置读失败")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], json!("INTERNAL"));
        assert_eq!(v["error"]["detail"]["cmd"], json!("get_settings"));
        assert_eq!(v["error"]["detail"]["error"], json!("配置读失败"));
    }
}
