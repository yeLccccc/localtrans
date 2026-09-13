// Task M0: 自动化测试 API 服务（spec §7.1 桌面 test-api）
//
// 三重门（spec §7.1.1）：
// - 编译期：本模块由 main.rs 的 `#[cfg(feature = "test-api")] mod test_api;`
//   整体门控，axum 为 optional 依赖，正式构建不进依赖树；
// - 运行期：`LOCALTRANS_TEST_API=1` 才监听，否则 start_test_api 直接返回；
// - 发布期：scripts/check-release-clean.sh 扫描产物特征串 TEST_API_HEADER。
//
// 服务跑在 Tauri 已有 tokio runtime 上（tauri::async_runtime），随应用退出，
// 不引入新进程；端口绑定失败只记错误，绝不拖垮主应用。

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use axum::Router;
use serde_json::{json, Value};
use tauri::Manager;

// Task M1: 日志环形缓冲 layer + logs/tail 数据面（spec §7.1.7）
pub mod ring;
// Task M2: UI 通道桥配对 + ui/* 端点数据面（spec §7.1.4）
pub mod bridge;
// Task M3: 状态快照 + 条件等待数据面（spec §7.1.6 / §7.1.5）
pub mod snapshot;
// Task M4: 截图证据（spec §7.1.8）与 invoke 白名单（spec §7.1.9）
pub mod invoke;
pub mod shot;

/// 默认监听端口（spec §7.1.1）
pub const DEFAULT_PORT: u16 = 39871;

/// 特征响应头（发布期产物扫描目标，spec §7.1.1）。
/// 注意保留此大小写字面量：HeaderName::from_bytes 运行期才归一为小写，
/// 二进制里存的是原样字符串，check-release-clean.sh 靠它识别测试面。
const TEST_API_HEADER: &str = "X-LocalTrans-TestAPI";

/// 免认证端点（spec §7.1.3：仅 /api/health 存活探测）
const AUTH_FREE_PATHS: [&str; 1] = ["/api/health"];

/// 认证失败固定延迟：拖慢 token 爆破
const AUTH_FAIL_DELAY_MS: u64 = 500;

/// 启动测试 API 服务（在 main.rs setup 中调用，仅 test-api 构建可达）。
/// 编译期 feature 门 + 运行期环境变量门都通过才真正监听。
pub fn start_test_api(app: tauri::AppHandle) {
    // 运行期门：环境变量未设/非 "1" → 完全不监听，代码路径不触网
    if !enabled_by_env() {
        tracing::debug!("LOCALTRANS_TEST_API != 1，test-api 服务未启用");
        return;
    }

    // 绑定地址与端口（默认 0.0.0.0:39871）
    let bind = std::env::var("LOCALTRANS_TEST_API_BIND")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "0.0.0.0".to_string());
    let port = match std::env::var("LOCALTRANS_TEST_API_PORT") {
        Ok(s) => match s.trim().parse::<u16>() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("LOCALTRANS_TEST_API_PORT 解析失败({e})，回退默认端口 {DEFAULT_PORT}");
                DEFAULT_PORT
            }
        },
        Err(_) => DEFAULT_PORT,
    };

    // Token：LOCALTRANS_TEST_API_KEY 优先，否则 app 数据目录 test-api.key
    // （不存在则生成 32 字节随机数的 hex 写入）
    let token = match std::env::var("LOCALTRANS_TEST_API_KEY") {
        Ok(k) if !k.trim().is_empty() => k,
        _ => match load_or_create_key(&app) {
            Ok(k) => k,
            Err(e) => {
                tracing::error!("test-api Token 获取失败，服务不启动: {e}");
                return;
            }
        },
    };

    let state = Arc::new(ApiState {
        token,
        app_version: app.package_info().version.to_string(),
        app: app.clone(),
    });

    let router = Router::new()
        .route("/api/health", get(health))
        .route("/api/version", get(version))
        .route("/api/logs/tail", get(logs_tail))
        // Task M2: UI 通道端点（spec §7.1.3，数据面走 bridge 往返）
        .route("/api/ui/tree", get(ui_tree))
        .route("/api/ui/navigate", post(ui_navigate))
        .route("/api/ui/click", post(ui_click))
        .route("/api/ui/toggle", post(ui_toggle))
        .route("/api/ui/input", post(ui_input))
        .route("/api/ui/text", get(ui_text))
        .route("/api/ui/wait", post(ui_wait))
        // Task M3: 状态断言端点（spec §7.1.3，数据面见 snapshot.rs）
        .route("/api/state/transfers", get(state_transfers))
        .route("/api/state/app", get(state_app))
        .route("/api/state/wait", post(state_wait))
        // Task M4: 证据收尾端点（spec §7.1.3 / §7.1.8 / §7.1.9）
        .route("/api/screenshot", get(shot::screenshot))
        .route("/api/invoke", post(invoke::invoke))
        .route("/api/test/begin", post(test_begin))
        .route("/api/test/step", post(test_step))
        .route("/api/test/end", post(test_end))
        // 未知路径 / 方法不匹配：统一 JSON 包络 + 404 NOT_FOUND（M4 遗留小修，
        // 消除 axum 默认 404 无特征头/无包络的不一致）。必须在下方 .layer
        // 之前注册才能套上认证与特征头中间件
        .fallback(not_found)
        .method_not_allowed_fallback(not_found)
        // 内层：Bearer 认证（/api/health 免认证）
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ))
        // 外层：所有响应（含 401）加特征头
        .layer(axum::middleware::from_fn(marker_header_middleware))
        .with_state(state);

    tauri::async_runtime::spawn(async move {
        match tokio::net::TcpListener::bind((bind.as_str(), port)).await {
            Ok(listener) => {
                let local = listener
                    .local_addr()
                    .map(|a| a.to_string())
                    .unwrap_or_else(|_| format!("{bind}:{port}"));
                tracing::info!("test-api 服务已监听 http://{local}");
                let svc =
                    router.into_make_service_with_connect_info::<std::net::SocketAddr>();
                if let Err(e) = axum::serve(listener, svc).await {
                    tracing::error!("test-api 服务异常退出: {e}");
                }
            }
            // 绑定失败（端口被占等）：只报错，应用继续运行
            Err(e) => {
                tracing::error!("test-api 监听绑定失败({bind}:{port}): {e}");
            }
        }
    });
}

/// 运行期门：LOCALTRANS_TEST_API == "1" 才启用
fn enabled_by_env() -> bool {
    std::env::var("LOCALTRANS_TEST_API")
        .ok()
        .as_deref()
        == Some("1")
}

/// Token 文件：app 数据目录/test-api.key，32 字节随机数的 hex（64 字符）。
/// 不存在则生成写入；类 unix 平台尽力设 0600，Windows 文件权限尽力而为。
fn load_or_create_key(app: &tauri::AppHandle) -> Result<String, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("app_data_dir 获取失败: {e}"))?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("数据目录创建失败 {}: {e}", dir.display()))?;
    let path = dir.join("test-api.key");

    if let Ok(s) = std::fs::read_to_string(&path) {
        let t = s.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let key = generate_key_hex();
    std::fs::write(&path, &key)
        .map_err(|e| format!("key 写入失败 {}: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    tracing::info!("test-api Token 已生成: {}", path.display());
    Ok(key)
}

/// 生成 32 字节随机数的 hex（64 个十六进制字符），test-api.key 的内容格式
pub fn generate_key_hex() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// 常量时间 Token 比较：先比长度（长度本身非敏感），等长部分逐字节
/// XOR 累加，不因前缀匹配程度提前返回。手写实现，避免为一处比较引 subtle。
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes().iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// 成功响应包络（spec §7.1.2）。runId 由后续里程碑的 test/begin 填充，M0 缺省。
pub fn ok_envelope(data: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "ok": true, "data": data, "meta": { "apiVersion": 1 } })
}

/// 失败响应包络（spec §7.1.2 错误码表，code 为封闭集合）
pub fn err_envelope(code: &str, message: &str) -> serde_json::Value {
    serde_json::json!({ "ok": false, "error": { "code": code, "message": message } })
}

/// /api/health 载荷（免认证存活探测）；bridgeReady 读 BridgeRegistry
/// 真实状态（Task M2 起由前端 test_bridge_hello 握手置位）
fn health_payload(bridge_ready: bool) -> serde_json::Value {
    serde_json::json!({ "status": "ok", "bridgeReady": bridge_ready })
}

/// /api/version 载荷（版本握手，spec §7.1.2 末尾）；bridgeReady 同 health
fn version_payload(app_version: &str, bridge_ready: bool) -> serde_json::Value {
    serde_json::json!({
        "appVersion": app_version,
        "apiVersion": 1,
        "bridgeReady": bridge_ready,
        "buildProfile": if cfg!(debug_assertions) { "debug" } else { "release" },
    })
}

/// 查询参数 → TailQuery。afterSeq 非法（非 u64）返回 Err 里的报错文案；
/// level/target/runId 空串或全空白视为未提供。
fn tail_query_from_params(
    params: &std::collections::HashMap<String, String>,
) -> Result<ring::TailQuery, String> {
    let after_seq = match params
        .get("afterSeq")
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
    {
        None => 0u64,
        Some(raw) => raw
            .parse::<u64>()
            .map_err(|_| format!("afterSeq 必须是非负整数，收到: {raw:?}"))?,
    };
    let opt = |key: &str| {
        params
            .get(key)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Ok(ring::TailQuery {
        after_seq,
        level: opt("level"),
        target: opt("target"),
        run_id: opt("runId"),
    })
}

/// GET /api/logs/tail（spec §7.1.3）：环形缓冲增量拉取。
/// 过滤语义（与 docs/contracts/test-api.md §5 一致，勿单边改动）：
/// - afterSeq：只取 seq 严格大于它的条目（缺省 0 = 从头拉）；
/// - level：**精确匹配某一级**（INFO/WARN/ERROR/…，大小写不敏感），
///   不是阈值——查 ERROR 不会带回 WARN/INFO；
/// - target：前缀匹配（`target=ui` 同时命中 `ui` 与 `ui::child`）；
/// - runId：精确匹配（缺省不过滤）。
/// 单次上限 500 条；data.nextSeq 为下次查询应传的 afterSeq 游标
/// （有命中时 = 本次最后一条的 seq，严格大于语义下不重复不丢条）。
async fn logs_tail(
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let query = match tail_query_from_params(&params) {
        Ok(q) => q,
        Err(msg) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(err_envelope("BAD_REQUEST", &msg)),
            )
                .into_response()
        }
    };
    let page = ring::global().tail(&query);
    Json(ok_envelope(serde_json::json!({
        "entries": page.entries,
        "nextSeq": page.next_seq,
    })))
    .into_response()
}

/// 路由共享状态
struct ApiState {
    /// 校验用的 Bearer Token
    token: String,
    /// 应用版本（来自 tauri PackageInfo，即 crate 版本）
    app_version: String,
    /// 应用句柄（ui/* 端点经它拿主 webview 执行 eval）
    app: tauri::AppHandle,
}

// ---------------------------------------------------------------------------
// Task M2: /api/ui/* 端点（spec §7.1.3）。HTTP 侧只做参数校验（失败 400 包
// 络），执行全部经 bridge::dispatch 往返前端 testBridge（spec §7.1.4）。
// 校验抽成纯函数直测，handler 不起真 axum 也可覆盖。
// ---------------------------------------------------------------------------

/// 400 BAD_REQUEST 包络
fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(err_envelope("BAD_REQUEST", message)),
    )
        .into_response()
}

/// 请求体 bytes → JSON；解析失败 400 包络
fn parse_json_body(body: &Bytes) -> Result<Value, Response> {
    serde_json::from_slice::<Value>(body)
        .map_err(|e| bad_request(&format!("请求体 JSON 解析失败: {e}")))
}

/// 必填非空字符串字段（trim 后为空视为缺失）
fn str_field(body: &Value, key: &str) -> Result<String, String> {
    body.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} 必须为非空字符串"))
}

/// 可选字符串字段：缺失/null/空白 → None
fn opt_str_field(body: &Value, key: &str) -> Result<Option<String>, String> {
    match body.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let s = v
                .as_str()
                .ok_or_else(|| format!("{key} 必须为字符串"))?
                .trim()
                .to_string();
            Ok(if s.is_empty() { None } else { Some(s) })
        }
    }
}

/// /api/ui/navigate 入参：path 必须为以 / 开头的非空字符串
fn navigate_params(body: &Value) -> Result<String, String> {
    let path = str_field(body, "path")?;
    if !path.starts_with('/') {
        return Err(format!("path 必须以 / 开头（应用内路由路径），收到: {path:?}"));
    }
    Ok(path)
}

/// /api/ui/click 入参：selector 必填
fn click_params(body: &Value) -> Result<String, String> {
    str_field(body, "selector")
}

/// /api/ui/input 入参：selector 必填、value 必须为字符串（可为空串）、clear 可选布尔、
/// events 可选字符串数组（设值后追加派发的事件，如 blur/enter——程序化设值不产生
/// 真实焦点，@blur/@keyup.enter 类保存流必须显式派发）
fn input_params(body: &Value) -> Result<(String, String, bool, Vec<String>), String> {
    let selector = str_field(body, "selector")?;
    let value = body
        .get("value")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "value 必须为字符串（清空请传空串）".to_string())?
        .to_string();
    match body.get("clear") {
        None | Some(Value::Null) => {}
        Some(v) if v.is_boolean() => {}
        Some(_) => return Err("clear 必须为布尔值".to_string()),
    }
    let clear = body
        .get("clear")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let events = match body.get("events") {
        None | Some(Value::Null) => Vec::new(),
        Some(v) => {
            let arr = v
                .as_array()
                .ok_or_else(|| "events 必须为字符串数组".to_string())?;
            let mut out = Vec::with_capacity(arr.len());
            for e in arr {
                out.push(
                    e.as_str()
                        .ok_or_else(|| "events 必须为字符串数组".to_string())?
                        .to_string(),
                );
            }
            out
        }
    };
    Ok((selector, value, clear, events))
}

/// /api/ui/wait 入参：selector/text 二选一，timeoutMs 可选非负整数。
/// 返回 (selector, text, timeoutMs)
fn wait_params(body: &Value) -> Result<(Option<String>, Option<String>, Option<u64>), String> {
    let selector = opt_str_field(body, "selector")?;
    let text = opt_str_field(body, "text")?;
    match (&selector, &text) {
        (Some(_), Some(_)) => return Err("selector 与 text 只能二选一".to_string()),
        (None, None) => return Err("必须提供 selector 或 text 之一".to_string()),
        _ => {}
    }
    let timeout_ms = match body.get("timeoutMs") {
        None | Some(Value::Null) => None,
        Some(v) => match v.as_u64() {
            Some(t) => Some(t),
            None => return Err("timeoutMs 必须为非负整数".to_string()),
        },
    };
    Ok((selector, text, timeout_ms))
}

/// GET /api/ui/tree → 元素摘要数组（spec §7.1.4）
async fn ui_tree(State(st): State<Arc<ApiState>>) -> Response {
    bridge::dispatch(&st.app, "tree", json!({}), None).await
}

/// POST /api/ui/navigate `{path}` → 路由结果
async fn ui_navigate(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let path = match navigate_params(&v) {
        Ok(p) => p,
        Err(m) => return bad_request(&m),
    };
    bridge::dispatch(&st.app, "navigate", json!({ "path": path }), None).await
}

/// POST /api/ui/click `{selector}` → 元素文本回传
async fn ui_click(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let selector = match click_params(&v) {
        Ok(s) => s,
        Err(m) => return bad_request(&m),
    };
    bridge::dispatch(&st.app, "click", json!({ "selector": selector }), None).await
}

/// POST /api/ui/toggle `{selector}` — checkbox/radio 翻转(合成 click 不触发
/// label→input 激活转发,force-relay 场景实证;P2 期间加 testBridge toggle 动作)
async fn ui_toggle(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let selector = match click_params(&v) {
        Ok(s) => s,
        Err(m) => return bad_request(&m),
    };
    bridge::dispatch(&st.app, "toggle", json!({ "selector": selector }), None).await
}

/// POST /api/ui/input `{selector, value, clear?}`
async fn ui_input(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (selector, value, clear, events) = match input_params(&v) {
        Ok(t) => t,
        Err(m) => return bad_request(&m),
    };
    let mut p = json!({ "selector": selector, "value": value, "clear": clear });
    if !events.is_empty() {
        p["events"] = json!(events);
    }
    bridge::dispatch(&st.app, "input", p, None).await
}

/// GET /api/ui/text `?selector=` → innerText（缺省全文）
async fn ui_text(
    State(st): State<Arc<ApiState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let selector = params
        .get("selector")
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let mut p = json!({});
    if let Some(s) = selector {
        p["selector"] = json!(s);
    }
    bridge::dispatch(&st.app, "text", p, None).await
}

/// POST /api/ui/wait `{selector | text, timeoutMs?}` → 200 / 408
async fn ui_wait(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (selector, text, timeout_ms) = match wait_params(&v) {
        Ok(t) => t,
        Err(m) => return bad_request(&m),
    };
    let mut p = json!({});
    if let Some(s) = selector {
        p["selector"] = json!(s);
    }
    if let Some(t) = text {
        p["text"] = json!(t);
    }
    if let Some(t) = timeout_ms {
        p["timeoutMs"] = json!(t);
    }
    // 前端内部超时用于放宽 Rust 侧兜底：max(15s, timeoutMs + 5s)
    bridge::dispatch(&st.app, "wait", p, timeout_ms).await
}

async fn health() -> Json<serde_json::Value> {
    Json(ok_envelope(health_payload(
        bridge::registry().is_ready(),
    )))
}

// ---------------------------------------------------------------------------
// Task M3: /api/state/* 端点（spec §7.1.3 / §7.1.5 / §7.1.6，数据面在 snapshot.rs）
// ---------------------------------------------------------------------------

/// 500 INTERNAL 包络
fn internal_server(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(err_envelope("INTERNAL", message)),
    )
        .into_response()
}

/// 失败包络 + detail（spec §7.1.2 的 error.detail，如 WAIT_TIMEOUT 末次观测值）
fn err_envelope_with_detail(
    code: &str,
    message: &str,
    detail: serde_json::Value,
) -> serde_json::Value {
    let mut env = err_envelope(code, message);
    env["error"]["detail"] = detail;
    env
}

/// GET /api/state/transfers → `{ok:true,data:<TestSnapshot>}`。
/// AppState 在 main.rs setup 中先 manage 再启动本服务（line 序：
/// app.manage(state) → test_api::start_test_api），state() 取到的必在。
async fn state_transfers(State(st): State<Arc<ApiState>>) -> Response {
    let app_state = st.app.state::<crate::AppState>();
    let snap = snapshot::build_snapshot(&app_state).await;
    match serde_json::to_value(&snap) {
        Ok(v) => Json(ok_envelope(v)).into_response(),
        Err(e) => internal_server(&format!("TestSnapshot 序列化失败: {e}")),
    }
}

/// GET /api/state/app → 前端 Pinia 聚合快照（经 bridge `state` 动作往返，
/// 四个 store 的 toTestSnapshot()，契约见 docs/contracts/test-api.md §5.3）
async fn state_app(State(st): State<Arc<ApiState>>) -> Response {
    bridge::dispatch(&st.app, "state", json!({}), None).await
}

/// wait 求值器算子的名字（错误 detail 用；与入参 op 字符串同集）
fn op_name(op: snapshot::WaitOp) -> &'static str {
    match op {
        snapshot::WaitOp::Eq => "eq",
        snapshot::WaitOp::Ne => "ne",
        snapshot::WaitOp::Gte => "gte",
        snapshot::WaitOp::Lte => "lte",
        snapshot::WaitOp::Contains => "contains",
        snapshot::WaitOp::Exists => "exists",
        snapshot::WaitOp::Empty => "empty",
    }
}

fn source_name(source: snapshot::WaitSource) -> &'static str {
    match source {
        snapshot::WaitSource::Transfers => "transfers",
        snapshot::WaitSource::App => "app",
    }
}

/// POST /api/state/wait 超时响应：408 WAIT_TIMEOUT，detail 带末次观测值
/// （spec §7.1.2 / §7.1.5）。抽独立函数便于直测包络形态。
fn wait_timeout_response(req: &snapshot::WaitRequest, outcome: &snapshot::WaitOutcome) -> Response {
    (
        StatusCode::REQUEST_TIMEOUT,
        Json(err_envelope_with_detail(
            "WAIT_TIMEOUT",
            &format!(
                "等待条件超时（{} {} {}，{}ms 内未满足）",
                source_name(req.source),
                req.path,
                op_name(req.op),
                req.timeout_ms
            ),
            json!({
                "source": source_name(req.source),
                "path": req.path,
                "op": op_name(req.op),
                "value": req.value,
                // 末次观测值：路径从未存在则为 null（与"观测到 null"同形，
                // 区分要靠 exists 语义本身，见契约 §5.3）
                "lastValue": outcome.observed,
                "polls": outcome.polls,
                "timeoutMs": req.timeout_ms,
                "intervalMs": req.interval_ms,
            }),
        )),
    )
        .into_response()
}

/// POST /api/state/wait `{source, path, op, value?, timeoutMs?=10000, intervalMs?=500}`
///（spec §7.1.5：默认 500ms 轮询；intervalMs 可调小是给单测用的）。
/// source=transfers 每拍重建 TestSnapshot（Rust 内存态）；source=app 每拍经
/// bridge 取前端快照——桥错误（未就绪/超时）不靠轮询自愈，直接回错误响应。
async fn state_wait(State(st): State<Arc<ApiState>>, body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let req = match snapshot::wait_params(&v) {
        Ok(r) => r,
        Err(m) => return bad_request(&m),
    };
    let app = st.app.clone();
    let source = req.source;
    let fetch = move || {
        // 每拍克隆句柄再 move 进 async 块（闭包本体保持 FnMut 可重复调用）
        let app = app.clone();
        async move {
            match source {
                snapshot::WaitSource::Transfers => {
                    let app_state = app.state::<crate::AppState>();
                    let snap = snapshot::build_snapshot(&app_state).await;
                    serde_json::to_value(&snap)
                        .map_err(|e| internal_server(&format!("TestSnapshot 序列化失败: {e}")))
                }
                snapshot::WaitSource::App => {
                    bridge::bridge_eval(&app, "state", json!({}), None).await
                }
            }
        }
    };
    match snapshot::wait_loop(
        fetch,
        &req.path,
        req.op,
        req.value.clone(),
        req.timeout_ms,
        req.interval_ms,
    )
    .await
    {
        Ok(outcome) if outcome.matched => Json(ok_envelope(json!({
            "matched": true,
            "path": req.path,
            "op": op_name(req.op),
            "observed": outcome.observed,
            "polls": outcome.polls,
            "elapsedMs": outcome.elapsed_ms,
        })))
        .into_response(),
        Ok(outcome) => wait_timeout_response(&req, &outcome),
        // fetch 失败：transfers 序列化 INTERNAL / app 桥 409、408、500——原样回
        Err(resp) => resp,
    }
}

async fn version(State(st): State<Arc<ApiState>>) -> Json<serde_json::Value> {
    Json(ok_envelope(version_payload(
        &st.app_version,
        bridge::registry().is_ready(),
    )))
}

// ---------------------------------------------------------------------------
// Task M4: run/step 关联端点 + 未知路径 fallback（spec §7.1.3 / §7.1.7-3）
// ---------------------------------------------------------------------------

/// 未知路径 / 方法不匹配的统一 404 包络（M0 遗留小修：默认 404 无包络
/// 无特征头）。抽独立函数便于直测形态。
async fn not_found() -> Response {
    not_found_response()
}

/// not_found 的同步形态（直测用）
fn not_found_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(err_envelope("NOT_FOUND", "未知路径或方法（端点清单见 docs/contracts/test-api.md §5）")),
    )
        .into_response()
}

/// 生成 runId：16 字节随机数的 hex（32 字符）。test/begin 与冒烟按
/// `^[0-9a-f]{32}$` 断言。
fn generate_run_id() -> String {
    use rand::RngCore;
    let mut buf = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buf);
    hex::encode(buf)
}

/// begin 生命周期核：置 runId、清 step、打开始标记。
/// 置位在日志之前——开始标记本身就会带 runId 入环形缓冲。
/// 抽离纯逻辑（不触 HTTP），单测用独立 RingBuffer 的 ctx 直测。
fn begin_run(ctx: &ring::TestContext, scenario: &str) -> String {
    let run_id = generate_run_id();
    ctx.set_run_id(Some(run_id.clone()));
    ctx.set_step(None);
    tracing::info!(
        target: "test_api",
        scenario = scenario,
        "test run 开始 ▶ {scenario}"
    );
    run_id
}

/// step 生命周期核：无活动 run 报错；置 step 并打标记（带当前 runId）。
fn step_run(ctx: &ring::TestContext, name: &str) -> Result<(), String> {
    let (run, _) = ctx.snapshot();
    if run.is_none() {
        return Err("没有活动 test run（先调 POST /api/test/begin）".to_string());
    }
    ctx.set_step(Some(name.to_string()));
    tracing::info!(target: "test_api", "test step ▶ {name}");
    Ok(())
}

/// end 生命周期核：校验 runId 匹配当前 run → 打结束标记（在清空前打，
/// 该条日志带 runId 入缓冲）→ 统计该 runId 条数 → 清空 run/step。
/// 返回统计条数。runId 不匹配/无活动 run → Err（ctx 保持不变）。
fn end_run(ring: &ring::RingBuffer, ctx: &ring::TestContext, run_id: &str, outcome: &str) -> Result<usize, String> {
    let (current, _) = ctx.snapshot();
    match current {
        None => Err("没有活动 test run（先调 POST /api/test/begin）".to_string()),
        Some(cur) if cur != run_id => Err(format!(
            "runId 不匹配：当前活动 run 为 {cur:?}，请求收尾的是 {run_id:?}"
        )),
        Some(_) => {
            tracing::info!(target: "test_api", outcome = outcome, "test run 结束 ■ outcome={outcome}");
            let entries = ring.count_run(run_id);
            ctx.set_run_id(None);
            ctx.set_step(None);
            Ok(entries)
        }
    }
}

/// POST /api/test/begin {scenario} → data.runId（spec §7.1.3 / §7.1.7-3）。
/// 之后所有日志（Rust + 前端桥）带 runId 直至 test/end。
async fn test_begin(body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let scenario = match str_field(&v, "scenario") {
        Ok(s) => s,
        Err(m) => return bad_request(&m),
    };
    let ctx = ring::global().test_context();
    let run_id = begin_run(&ctx, &scenario);
    Json(ok_envelope(serde_json::json!({ "runId": run_id }))).into_response()
}

/// POST /api/test/step {name} → 步骤标记入日志流（step 字段同时落到
/// 后续日志条目上）
async fn test_step(body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let name = match str_field(&v, "name") {
        Ok(s) => s,
        Err(m) => return bad_request(&m),
    };
    let ctx = ring::global().test_context();
    match step_run(&ctx, &name) {
        Ok(()) => Json(ok_envelope(serde_json::json!({ "name": name }))).into_response(),
        Err(m) => bad_request(&m),
    }
}

/// POST /api/test/end {runId, outcome} → 清空 run/step + 该 runId 统计。
/// outcome 为编排器自定义（pass/fail/aborted…），服务端只透传进日志。
async fn test_end(body: Bytes) -> Response {
    let v = match parse_json_body(&body) {
        Ok(v) => v,
        Err(resp) => return resp,
    };
    let (run_id, outcome) = match (str_field(&v, "runId"), str_field(&v, "outcome")) {
        (Ok(r), Ok(o)) => (r, o),
        (Err(m), _) | (_, Err(m)) => return bad_request(&m),
    };
    let g = ring::global();
    let ctx = g.test_context();
    match end_run(&g, &ctx, &run_id, &outcome) {
        Ok(entries) => Json(ok_envelope(serde_json::json!({
            "runId": run_id,
            "outcome": outcome,
            "entries": entries,
        })))
        .into_response(),
        Err(m) => bad_request(&m),
    }
}


/// 全局特征响应头中间件：所有响应加 `X-LocalTrans-TestAPI: 1`
/// （发布期产物扫描的特征串，见模块头注释）
async fn marker_header_middleware(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    if let Ok(name) = HeaderName::from_bytes(TEST_API_HEADER.as_bytes()) {
        resp.headers_mut().insert(name, HeaderValue::from_static("1"));
    }
    resp
}

/// Bearer 认证中间件：除 AUTH_FREE_PATHS 外全部校验
/// `Authorization: Bearer <token>`；失败：warn（含来源 IP）+ 500ms 延迟 + 401 包络
async fn auth_middleware(
    State(st): State<Arc<ApiState>>,
    ConnectInfo(addr): ConnectInfo<std::net::SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if AUTH_FREE_PATHS.contains(&req.uri().path()) {
        return next.run(req).await;
    }
    let provided = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let passed = provided
        .map(|p| constant_time_eq(p, &st.token))
        .unwrap_or(false);
    if passed {
        return next.run(req).await;
    }
    // 认证失败：记来源 IP + 固定延迟拖慢爆破 + 401 包络（spec §7.1.2）
    tracing::warn!(
        "test-api 认证失败（来源 {} {}）",
        addr.ip(),
        req.uri().path()
    );
    tokio::time::sleep(std::time::Duration::from_millis(AUTH_FAIL_DELAY_MS)).await;
    (
        axum::http::StatusCode::UNAUTHORIZED,
        Json(err_envelope("AUTH_FAILED", "token 错误或缺失")),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 单元测试（模块整体被 feature 门控，默认 cargo test 不编译本节，
// 由 `cargo test -p localtrans --features test-api` 覆盖）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_比较_等长相同() {
        assert!(constant_time_eq("abcdef0123456789", "abcdef0123456789"));
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn token_比较_等长不同() {
        assert!(!constant_time_eq("abcdef0123456789", "abcdef012345678X"));
        // 只差最后一位也必须拒绝
        assert!(!constant_time_eq(
            "ffeeddccbbaa99887766554433221100",
            "ffeeddccbbaa99887766554433221101"
        ));
    }

    #[test]
    fn token_比较_不等长() {
        assert!(!constant_time_eq("abc", "abcd"));
        assert!(!constant_time_eq("", "a"));
    }

    #[test]
    fn token_比较_前缀相同长度不同() {
        // 前缀完全匹配但不能因前缀短路放行
        let token = "0123456789abcdef0123456789abcdef";
        assert!(!constant_time_eq(token, &token[..31]));
        assert!(!constant_time_eq(&token[..31], token));
        assert!(constant_time_eq(token, token));
    }

    #[test]
    fn key_生成_hex格式() {
        let k = generate_key_hex();
        assert_eq!(k.len(), 64, "32 字节 → 64 个 hex 字符");
        assert!(
            k.bytes().all(|b| b.is_ascii_hexdigit()),
            "只含十六进制字符: {k}"
        );
        let raw = hex::decode(&k).expect("可解码");
        assert_eq!(raw.len(), 32);
        // 两次生成必须不同（2^256 撞车概率忽略不计）
        assert_ne!(generate_key_hex(), generate_key_hex());
    }

    #[test]
    fn 包络_成功结构() {
        let env = ok_envelope(serde_json::json!({ "status": "ok" }));
        assert_eq!(env["ok"], serde_json::json!(true));
        assert_eq!(env["data"]["status"], serde_json::json!("ok"));
        assert_eq!(env["meta"]["apiVersion"], serde_json::json!(1));
    }

    #[test]
    fn 包络_失败结构() {
        let env = err_envelope("AUTH_FAILED", "token 错误或缺失");
        assert_eq!(env["ok"], serde_json::json!(false));
        assert_eq!(env["error"]["code"], serde_json::json!("AUTH_FAILED"));
        assert_eq!(
            env["error"]["message"],
            serde_json::json!("token 错误或缺失")
        );
    }

    #[test]
    fn 载荷_health() {
        // 未握手（前端未加载 testBridge）
        let p = health_payload(false);
        assert_eq!(p["status"], serde_json::json!("ok"));
        assert_eq!(p["bridgeReady"], serde_json::json!(false));
        // 前端 hello 之后
        let p = health_payload(true);
        assert_eq!(p["bridgeReady"], serde_json::json!(true));
    }

    #[test]
    fn 载荷_version_版本握手字段齐全() {
        let p = version_payload("0.12.0", true);
        assert_eq!(p["appVersion"], serde_json::json!("0.12.0"));
        assert_eq!(p["apiVersion"], serde_json::json!(1));
        assert_eq!(p["bridgeReady"], serde_json::json!(true));
        let profile = p["buildProfile"].as_str().expect("buildProfile 为字符串");
        assert!(
            profile == "debug" || profile == "release",
            "buildProfile 取值合法: {profile}"
        );
    }

    fn params(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn tail参数_afterSeq缺省与合法解析() {
        // 缺省 → 0
        let q = tail_query_from_params(&params(&[])).unwrap();
        assert_eq!(q.after_seq, 0);
        assert_eq!(q.level, None);
        // 合法数值（含空白）与各过滤键
        let q = tail_query_from_params(&params(&[
            ("afterSeq", " 42 "),
            ("level", "warn"),
            ("target", "ui"),
            ("runId", "run-9"),
        ]))
        .unwrap();
        assert_eq!(q.after_seq, 42);
        assert_eq!(q.level.as_deref(), Some("warn"));
        assert_eq!(q.target.as_deref(), Some("ui"));
        assert_eq!(q.run_id.as_deref(), Some("run-9"));
        // 空串/全空白 → 视为未提供
        let q = tail_query_from_params(&params(&[
            ("afterSeq", ""),
            ("level", "  "),
            ("target", ""),
            ("runId", ""),
        ]))
        .unwrap();
        assert_eq!(q.after_seq, 0);
        assert_eq!(q.level, None);
        assert_eq!(q.target, None);
        assert_eq!(q.run_id, None);
    }

    #[test]
    fn tail参数_afterSeq非法报错() {
        for bad in ["abc", "-1", "3.5", "1e3"] {
            let err = tail_query_from_params(&params(&[("afterSeq", bad)])).unwrap_err();
            assert!(err.contains("afterSeq"), "报错应点名 afterSeq: {err}");
        }
    }

    /// 断言辅助：handler 的 Response → (status, 包络 JSON)
    async fn envelope_of(resp: Response) -> (StatusCode, serde_json::Value) {
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("响应体可读");
        let v = serde_json::from_slice(&bytes).expect("响应体为 JSON");
        (parts.status, v)
    }

    #[tokio::test]
    async fn logs_tail端点_包络与游标语义() {
        // 用独一 target 隔离全局 ring 上其他测试的并发写入
        let target = "smoke_m1_endpoint_test";
        let g = ring::global();
        g.push("INFO", target, "first".into());
        g.push("ERROR", target, "second".into());

        let (status, v) = envelope_of(logs_tail(Query(params(&[("target", target)]))).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(v["ok"], serde_json::json!(true));
        assert_eq!(v["meta"]["apiVersion"], serde_json::json!(1));
        let entries = v["data"]["entries"].as_array().expect("entries 数组");
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["message"], serde_json::json!("first"));
        assert_eq!(entries[0]["level"], serde_json::json!("INFO"));
        assert_eq!(entries[0]["target"], serde_json::json!(target));
        let next = v["data"]["nextSeq"].as_u64().expect("nextSeq");

        // 游标续读：afterSeq=nextSeq 不重复
        let (_, v2) = envelope_of(logs_tail(Query(params(&[
            ("target", target),
            ("afterSeq", &next.to_string()),
        ])))
        .await)
        .await;
        assert_eq!(v2["ok"], serde_json::json!(true));
        assert!(
            v2["data"]["entries"]
                .as_array()
                .expect("entries 数组")
                .is_empty(),
            "游标之后无重复条目"
        );

        // level 过滤：精确匹配 ERROR，不带 INFO
        let (_, v3) = envelope_of(logs_tail(Query(params(&[
            ("target", target),
            ("level", "error"),
        ])))
        .await)
        .await;
        let entries = v3["data"]["entries"].as_array().expect("entries 数组");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["level"], serde_json::json!("ERROR"));
    }

    #[tokio::test]
    async fn logs_tail端点_非法afterSeq返400包络() {
        let (status, v) =
            envelope_of(logs_tail(Query(params(&[("afterSeq", "not-a-number")]))).await).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!("BAD_REQUEST"));
    }

    // -----------------------------------------------------------------
    // Task M2: ui/* 端点参数校验（纯函数直测，spec §7.1.3）
    // -----------------------------------------------------------------

    #[test]
    fn ui参数_navigate路径校验() {
        assert_eq!(
            navigate_params(&serde_json::json!({ "path": "/settings" })).unwrap(),
            "/settings"
        );
        // 空白被 trim 拒绝
        assert!(navigate_params(&serde_json::json!({ "path": "  " })).is_err());
        // 非字符串/缺失
        assert!(navigate_params(&serde_json::json!({ "path": 42 })).is_err());
        assert!(navigate_params(&serde_json::json!({})).is_err());
        // 必须是应用内路由路径
        let err = navigate_params(&serde_json::json!({ "path": "https://evil.example" }))
            .unwrap_err();
        assert!(err.contains("path"), "报错应点名 path: {err}");
    }

    #[test]
    fn ui参数_click必填selector() {
        assert_eq!(
            click_params(&serde_json::json!({ "selector": "[testid=nav-settings-link]" }))
                .unwrap(),
            "[testid=nav-settings-link]"
        );
        assert!(click_params(&serde_json::json!({})).is_err());
        assert!(click_params(&serde_json::json!({ "selector": "" })).is_err());
    }

    #[test]
    fn ui参数_input四元组() {
        let (sel, val, clear, events) = input_params(&serde_json::json!({
            "selector": "[testid=x]", "value": "hello", "clear": true
        }))
        .unwrap();
        assert_eq!((sel.as_str(), val.as_str(), clear), ("[testid=x]", "hello", true));
        assert!(events.is_empty());

        // value 可为空串（清空语义），clear 可缺省
        let (_, val, clear, _) =
            input_params(&serde_json::json!({ "selector": "#a", "value": "" })).unwrap();
        assert_eq!((val.as_str(), clear), ("", false));
        // value 非字符串 / selector 缺失 / clear 非布尔
        assert!(input_params(&serde_json::json!({ "selector": "#a", "value": 7 })).is_err());
        assert!(input_params(&serde_json::json!({ "value": "x" })).is_err());
        assert!(
            input_params(&serde_json::json!({ "selector": "#a", "value": "x", "clear": "yes" }))
                .is_err()
        );
        // events：字符串数组合法，非数组/含非字符串非法
        let (_, _, _, events) = input_params(&serde_json::json!({
            "selector": "#a", "value": "x", "events": ["blur", "enter"]
        }))
        .unwrap();
        assert_eq!(events, vec!["blur".to_string(), "enter".to_string()]);
        assert!(
            input_params(&serde_json::json!({ "selector": "#a", "value": "x", "events": "blur" }))
                .is_err()
        );
        assert!(input_params(&serde_json::json!({
            "selector": "#a", "value": "x", "events": [7]
        }))
        .is_err());
    }

    #[test]
    fn ui参数_wait二选一与timeout() {
        // selector 形态
        let (sel, text, tmo) = wait_params(&serde_json::json!({
            "selector": "[testid^=transfer-item-]", "timeoutMs": 8000
        }))
        .unwrap();
        assert_eq!(
            (sel.as_deref(), text, tmo),
            (Some("[testid^=transfer-item-]"), None, Some(8000))
        );
        // text 形态，timeoutMs 缺省
        let (_, text, tmo) =
            wait_params(&serde_json::json!({ "text": "设置" })).unwrap();
        assert_eq!((text.as_deref(), tmo), (Some("设置"), None));
        // 都给 / 都不给 / timeoutMs 非法
        assert!(wait_params(&serde_json::json!({ "selector": "#a", "text": "x" })).is_err());
        assert!(wait_params(&serde_json::json!({})).is_err());
        assert!(
            wait_params(&serde_json::json!({ "text": "x", "timeoutMs": -1 })).is_err(),
            "负数经 serde_json 不认 u64"
        );
        assert!(wait_params(&serde_json::json!({ "text": "x", "timeoutMs": "8s" })).is_err());
        // 空白 selector 视为未提供（与"都给"互补的边界）
        assert!(wait_params(&serde_json::json!({ "selector": "  " })).is_err());
    }

    #[tokio::test]
    async fn ui端点_非法请求体返400包络() {
        // JSON 解析失败（parse_json_body 抽出的纯函数直测，
        // handler 层走真 axum 由冒烟 tests/e2e/smoke-m2.mjs 覆盖）
        let (status, v) = envelope_of(
            parse_json_body(&Bytes::from_static(b"{not json"))
                .expect_err("非法 JSON 应返回 Err 响应"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!("BAD_REQUEST"));
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 为字符串")
                .contains("JSON"),
            "报错应点名 JSON 解析: {}",
            v["error"]["message"]
        );

        // 合法 JSON 直通
        let v = parse_json_body(&Bytes::from_static(b"{\"path\":\"/settings\"}"))
            .expect("合法 JSON 应通过");
        assert_eq!(v["path"], serde_json::json!("/settings"));
    }

    // -----------------------------------------------------------------
    // Task M3: state/wait 超时包络形态（求值器矩阵见 snapshot.rs；
    // handler 级（持 AppHandle）由冒烟 tests/e2e/smoke-m3.mjs 覆盖）
    // -----------------------------------------------------------------

    #[tokio::test]
    async fn wait超时包络_detail带末次观测值() {
        let req = snapshot::wait_params(&serde_json::json!({
            "source": "transfers", "path": "devices.length",
            "op": "gte", "value": 999, "timeoutMs": 1500, "intervalMs": 500
        }))
        .expect("合法入参");
        let outcome = snapshot::WaitOutcome {
            matched: false,
            observed: Some(serde_json::json!(0)),
            polls: 4,
            elapsed_ms: 1500,
        };
        let (status, v) = envelope_of(wait_timeout_response(&req, &outcome)).await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!("WAIT_TIMEOUT"));
        let detail = &v["error"]["detail"];
        assert_eq!(detail["lastValue"], serde_json::json!(0), "末次观测值");
        assert_eq!(detail["path"], serde_json::json!("devices.length"));
        assert_eq!(detail["op"], serde_json::json!("gte"));
        assert_eq!(detail["value"], serde_json::json!(999));
        assert_eq!(detail["timeoutMs"], serde_json::json!(1500));
        assert_eq!(detail["intervalMs"], serde_json::json!(500));
        assert_eq!(detail["polls"], serde_json::json!(4));
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 为字符串")
                .contains("devices.length"),
            "报错应带条件描述"
        );

        // 路径从未存在 → lastValue null
        let outcome_none = snapshot::WaitOutcome {
            matched: false,
            observed: None,
            polls: 1,
            elapsed_ms: 10,
        };
        let (_, v2) = envelope_of(wait_timeout_response(&req, &outcome_none)).await;
        assert_eq!(v2["error"]["detail"]["lastValue"], serde_json::json!(null));
    }

    #[test]
    fn wait非法入参报op与source() {
        // 与冒烟负路径同款：非法 op
        let err = snapshot::wait_params(&serde_json::json!({
            "source": "transfers", "path": "devices.length", "op": "pfx", "value": 0
        }))
        .unwrap_err();
        assert!(err.contains("op"));
        // 非法 source
        let err = snapshot::wait_params(&serde_json::json!({
            "source": "elsewhere", "path": "a", "op": "eq", "value": 1
        }))
        .unwrap_err();
        assert!(err.contains("source"));
    }

    // -----------------------------------------------------------------
    // Task M4: run/step 生命周期 + 未知路径 fallback。生命周期用独立
    // RingBuffer + TestContext 直测（不碰 global，避免并行测试互扰）。
    // -----------------------------------------------------------------

    #[test]
    fn runId_生成_32位hex且不重复() {
        let a = generate_run_id();
        assert_eq!(a.len(), 32, "16 字节 → 32 个 hex 字符");
        assert!(a.bytes().all(|b| b.is_ascii_hexdigit()), "只含十六进制: {a}");
        assert_ne!(a, generate_run_id(), "两次生成必不同");
    }

    #[test]
    fn 生命周期_begin置位step清空() {
        let ring = ring::RingBuffer::new();
        let ctx = ring.test_context();
        // 预置脏 step（上一 run 残留场景）
        ctx.set_step(Some("stale".into()));
        let run_id = begin_run(&ctx, "smoke");
        assert!(ctx.snapshot().0.as_deref() == Some(run_id.as_str()));
        assert_eq!(ctx.snapshot().1, None, "begin 必须清掉旧 step");
    }

    #[test]
    fn 生命周期_step要求活动run() {
        let ring = ring::RingBuffer::new();
        let ctx = ring.test_context();
        // 无 run → Err
        let err = step_run(&ctx, "s1").unwrap_err();
        assert!(err.contains("begin"), "报错应指向 test/begin: {err}");
        // 有 run → 置位
        begin_run(&ctx, "sc");
        step_run(&ctx, "s1").unwrap();
        assert_eq!(ctx.snapshot().1.as_deref(), Some("s1"));
    }

    #[test]
    fn 生命周期_end校验与统计() {
        let ring = ring::RingBuffer::new();
        let ctx = ring.test_context();

        // 无活动 run → Err
        assert!(end_run(&ring, &ctx, "run-x", "pass").is_err());

        let run_id = begin_run(&ctx, "sc");
        // begin 打了标记但环形缓冲要靠 push/layer 才有条目（单测无全局
        // subscriber）——手动 push 两条带 runId 的条目模拟
        ring.push("INFO", "t", "a".into());
        ring.push("INFO", "t", "b".into());

        // runId 不匹配 → Err 且 ctx 不变
        let err = end_run(&ring, &ctx, "deadbeef", "pass").unwrap_err();
        assert!(err.contains("不匹配"), "报错应点名不匹配: {err}");
        assert_eq!(ctx.snapshot().0.as_deref(), Some(run_id.as_str()));

        // 匹配 → 清空 + 统计（含 push 时 ctx 已置位而带上 runId 的 2 条
        // 加 begin/step 标记——标记经 tracing 无 subscriber 不进 ring，
        // 此处统计的就是 2 条）
        let entries = end_run(&ring, &ctx, &run_id, "pass").unwrap();
        assert_eq!(entries, 2, "count_run 统计该 runId 的幸存条目");
        assert_eq!(ctx.snapshot(), (None, None), "end 后 run/step 全清");
        // end 后再 step → 又无活动 run
        assert!(step_run(&ctx, "s").is_err());
    }

    #[test]
    fn 环形缓冲_count_run只数命中run() {
        let ring = ring::RingBuffer::new();
        let ctx = ring.test_context();
        ctx.set_run_id(Some("run-1".into()));
        ring.push("INFO", "t", "in-1".into());
        ctx.set_run_id(Some("run-2".into()));
        ring.push("INFO", "t", "in-2a".into());
        ring.push("INFO", "t", "in-2b".into());
        ctx.set_run_id(None);
        ring.push("INFO", "t", "orphan".into());
        assert_eq!(ring.count_run("run-1"), 1);
        assert_eq!(ring.count_run("run-2"), 2);
        assert_eq!(ring.count_run("run-x"), 0, "无命中计 0 而非报错");
    }

    #[tokio::test]
    async fn fallback_404包络NOT_FOUND() {
        let (status, v) = envelope_of(not_found_response()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!("NOT_FOUND"));
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 为字符串")
                .contains("未知路径"),
            "报错应描述未知路径语义"
        );
    }
}
