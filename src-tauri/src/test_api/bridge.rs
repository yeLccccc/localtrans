// Task M2: UI 通道桥配对（spec §7.1.4 往返时序）
//
// 解决 webview.eval 单向性：HTTP 侧 eval
// `window.__testBridge.exec(<请求 JSON>)` 后注册 oneshot 等待；前端执行完
// invoke('test_bridge_result')（commands.rs 无条件注册的命令），按 id 唤醒
// 对应 oneshot，HTTP 响应才落定。请求 id 全局单调递增，允许多请求并发在途。
//
// 就绪握手：前端启动早期 invoke('test_bridge_hello') 置位 ready——
// feature 开而前端忘了测试构建时，所有 ui/* 端点立刻 409 BRIDGE_NOT_READY
// 并附一句修复提示，而不是每次傻等到超时。
//
// 本模块整体位于 test_api（feature = "test-api"）门控内；无条件注册的
// 两个 Tauri 命令在 commands.rs，函数体经 cfg 分支转到这里。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde_json::Value;
use tokio::sync::oneshot;

use super::{err_envelope, ok_envelope};

/// 主窗口 label：tauri.conf.json windows[0] 未显式指定时的 Tauri 2 默认值
const MAIN_WINDOW_LABEL: &str = "main";

/// 前端回包兜底超时下限与松弛量（plan Task M2：max(15s, wait.timeoutMs + 5s)）。
/// wait 动作的前端内部超时缺省 10s，非 wait 动作一律 15s。
pub const BRIDGE_TIMEOUT_FLOOR_MS: u64 = 15_000;
pub const BRIDGE_TIMEOUT_SLACK_MS: u64 = 5_000;

/// 计算等待前端回包的超时（毫秒）。纯函数便于直测。
pub fn bridge_timeout_ms(wait_timeout_ms: Option<u64>) -> u64 {
    match wait_timeout_ms {
        Some(t) => BRIDGE_TIMEOUT_FLOOR_MS.max(t.saturating_add(BRIDGE_TIMEOUT_SLACK_MS)),
        None => BRIDGE_TIMEOUT_FLOOR_MS,
    }
}

/// 在途请求登记表：id → 回包 oneshot。ready 由 test_bridge_hello 置位。
pub struct BridgeRegistry {
    ready: AtomicBool,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
}

impl BridgeRegistry {
    pub fn new() -> Self {
        Self {
            ready: AtomicBool::new(false),
            next_id: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// 就绪握手落点（test_bridge_hello 命令转发到这里）
    pub fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
        tracing::info!(target: "test_api", "testBridge 就绪（前端已握手，ui/* 端点可用）");
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// 分配单调递增 id 并注册 oneshot（从 1 起，0 保留给"非法请求"）
    pub fn register(&self) -> (u64, oneshot::Receiver<Result<Value, String>>) {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        (id, rx)
    }

    /// 前端回包落点（test_bridge_result 命令转发到这里）。
    /// 返回是否命中在途请求（未命中 = 回包迟到/未知 id，调用方记 warn）。
    pub fn complete(&self, id: u64, ok: bool, payload: Value) -> bool {
        let sender = self.pending.lock().unwrap().remove(&id);
        match sender {
            Some(tx) => {
                let result = if ok {
                    Ok(payload)
                } else {
                    // payload 为 {code, message}；缺字段按 INTERNAL 兜底
                    let code = payload
                        .get("code")
                        .and_then(|v| v.as_str())
                        .unwrap_or("INTERNAL")
                        .to_string();
                    let message = payload
                        .get("message")
                        .and_then(|v| v.as_str())
                        .unwrap_or("前端执行失败")
                        .to_string();
                    Err(format!("{code}: {message}"))
                };
                tx.send(result).is_ok()
            }
            None => false,
        }
    }

    /// 当前在途数（诊断/测试用）
    pub fn pending_len(&self) -> usize {
        self.pending.lock().unwrap().len()
    }

    /// 仅测试用：还原全局状态
    #[cfg(test)]
    pub fn reset_for_test(&self) {
        self.ready.store(false, Ordering::SeqCst);
        self.next_id.store(0, Ordering::SeqCst);
        self.pending.lock().unwrap().clear();
    }
}

impl Default for BridgeRegistry {
    fn default() -> Self {
        Self::new()
    }
}

static REGISTRY: OnceLock<BridgeRegistry> = OnceLock::new();

/// 进程级唯一 registry（HTTP 线程与 IPC 命令线程共享）
pub fn registry() -> &'static BridgeRegistry {
    REGISTRY.get_or_init(BridgeRegistry::new)
}

/// 构造 eval 脚本：`window.__testBridge&&window.__testBridge.exec(<请求JSON字符串>)`。
/// 请求先序列化为 JSON 文本，再整体作为一个 **JS 字符串字面量** 嵌入
/// （spec §7.1.4：payload 以单个 JSON 字符串字面量嵌入 eval，杜绝转义歧义）。
/// 注意必须二次序列化成字符串——若直接嵌 JSON 对象字面量，exec 收到的
/// 是已求值的对象而非字符串（冒烟实测踩坑：JSON.parse(object) 必炸）；
/// serde_json 对字符串的转义恰为合法 JS 转义，不会过度转义。纯函数便于直测。
pub fn build_eval_js(request: &Value) -> Result<String, String> {
    let json =
        serde_json::to_string(request).map_err(|e| format!("桥请求序列化失败: {e}"))?;
    let literal = serde_json::to_string(&json).map_err(|e| format!("字面量化失败: {e}"))?;
    debug_assert!(literal.starts_with('"') && literal.ends_with('"'));
    Ok(format!("window.__testBridge&&window.__testBridge.exec({literal})"))
}

/// 在主 webview 执行 JS（ExecuteScript 通道，不受页面 CSP 限制）
fn eval_in_main_webview(app: &tauri::AppHandle, js: &str) -> Result<(), String> {
    use tauri::Manager;
    let window = app
        .get_webview_window(MAIN_WINDOW_LABEL)
        .or_else(|| app.webview_windows().values().next().cloned());
    match window {
        Some(w) => w.eval(js).map_err(|e| e.to_string()),
        None => Err("未找到 webview 窗口".to_string()),
    }
}

/// 前端桥未就绪的 409 包络（抽独立函数便于直测；dispatch 未就绪分支调用）
pub fn not_ready_response() -> Response {
    (
        StatusCode::CONFLICT,
        Json(err_envelope(
            "BRIDGE_NOT_READY",
            "前端 testBridge 未就绪：前端需以测试模式构建/启动（npm --prefix ui run dev:test 或 build:test），Rust 需 --features test-api",
        )),
    )
        .into_response()
}

/// 前端桥错误串（"CODE: message"）→ HTTP 响应。
/// code 只认封闭集合（spec §7.1.2 错误码表），未知按 INTERNAL 包装。
pub fn bridge_error_response(code_and_message: &str) -> Response {
    let (code, message) = code_and_message
        .split_once(": ")
        .map(|(c, m)| (c.to_string(), m.to_string()))
        .unwrap_or_else(|| ("INTERNAL".to_string(), code_and_message.to_string()));
    let status = match code.as_str() {
        "ELEMENT_NOT_FOUND" => StatusCode::NOT_FOUND,
        "WAIT_TIMEOUT" => StatusCode::REQUEST_TIMEOUT,
        "BAD_REQUEST" => StatusCode::BAD_REQUEST,
        "INTERNAL" => StatusCode::INTERNAL_SERVER_ERROR,
        other => {
            tracing::warn!(target: "test_api", "前端桥未知错误码 {other}，按 INTERNAL 处理");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(err_envelope("INTERNAL", &format!("[{other}] {message}"))),
            )
                .into_response();
        }
    };
    (status, Json(err_envelope(&code, &message))).into_response()
}

fn internal(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(err_envelope("INTERNAL", message)),
    )
        .into_response()
}

/// ui/* 端点共用流程（spec §7.1.4 时序图）：
/// ready 检查 → 分配 id → eval → await oneshot（超时 max(15s, timeout+5s)）。
/// `wait_timeout_ms` 仅为 wait 动作的前端内部超时（用于放宽兜底），其余传 None。
pub async fn dispatch(
    app: &tauri::AppHandle,
    action: &str,
    params: Value,
    wait_timeout_ms: Option<u64>,
) -> Response {
    match bridge_eval(app, action, params, wait_timeout_ms).await {
        Ok(data) => Json(ok_envelope(data)).into_response(),
        Err(resp) => resp,
    }
}

/// eval 往返核心（Task M3 抽出，dispatch 与 state/wait 的 source=app
/// 轮询共用）：成功返回桥回传的数据本体；失败返回完整 HTTP 响应
/// （409 未就绪 / 40x 前端错误码映射 / 408 兜底超时 / 500 内部）。
/// 调用方决定错误是直接回给客户端（端点）还是中止轮询（wait）。
pub async fn bridge_eval(
    app: &tauri::AppHandle,
    action: &str,
    params: Value,
    wait_timeout_ms: Option<u64>,
) -> Result<Value, Response> {
    if !registry().is_ready() {
        return Err(not_ready_response());
    }

    let reg = registry();
    let (id, rx) = reg.register();
    let request = serde_json::json!({ "id": id, "action": action, "params": params });
    let js = match build_eval_js(&request) {
        Ok(js) => js,
        Err(e) => return Err(internal(&e)),
    };
    if let Err(e) = eval_in_main_webview(app, &js) {
        tracing::warn!(target: "test_api", "ui/{action} eval 失败: {e}");
        return Err(internal(&format!("webview eval 失败: {e}")));
    }

    let timeout = Duration::from_millis(bridge_timeout_ms(wait_timeout_ms));
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(Ok(data))) => Ok(data),
        Ok(Ok(Err(code_message))) => Err(bridge_error_response(&code_message)),
        // sender 被丢弃（前端页签重载等）：通道已断
        Ok(Err(_dropped)) => Err(internal("前端通道中断（oneshot 发送端被丢弃）")),
        Err(_) => {
            tracing::warn!(target: "test_api", "ui/{action} id={id} 等待前端回包超时（{timeout:?}）");
            Err((
                StatusCode::REQUEST_TIMEOUT,
                Json(err_envelope(
                    "WAIT_TIMEOUT",
                    &format!("等待前端 testBridge 回包超时（action={action}, id={id}）"),
                )),
            )
                .into_response())
        }
    }
}

// ---------------------------------------------------------------------------
// 单元测试（feature 门控，`cargo test -p localtrans --features test-api` 覆盖）
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn 超时计算_下限与松弛() {
        // 缺省/小于下限 → 15s
        assert_eq!(bridge_timeout_ms(None), 15_000);
        assert_eq!(bridge_timeout_ms(Some(0)), 15_000);
        assert_eq!(bridge_timeout_ms(Some(10_000)), 15_000);
        assert_eq!(bridge_timeout_ms(Some(10_001)), 15_001); // 10_001 + 5_000 > 15_000
        // wait.timeoutMs + 5s 超过下限时取大者
        assert_eq!(bridge_timeout_ms(Some(20_000)), 25_000);
        assert_eq!(bridge_timeout_ms(Some(115_000)), 120_000);
        // 饱和保护：u64::MAX + 5s 不得溢出回绕
        assert_eq!(bridge_timeout_ms(Some(u64::MAX)), u64::MAX);
    }

    #[test]
    fn registry_ready门控() {
        let reg = BridgeRegistry::new();
        assert!(!reg.is_ready(), "新建 registry 未就绪");
        reg.mark_ready();
        assert!(reg.is_ready(), "hello 之后就绪");
    }

    #[tokio::test]
    async fn registry_配对成功路径() {
        let reg = BridgeRegistry::new();
        let (id, rx) = reg.register();
        assert_eq!(id, 1, "id 从 1 起单调递增");
        assert_eq!(reg.pending_len(), 1);

        let hit = reg.complete(id, true, json!({ "text": "设置" }));
        assert!(hit, "命中在途请求");
        assert_eq!(reg.pending_len(), 0, "回包后清理登记");

        let result = rx.await.expect("oneshot 未被丢弃");
        assert_eq!(result.expect("ok=true 走 Ok"), json!({ "text": "设置" }));
    }

    #[tokio::test]
    async fn registry_错误回包编码() {
        let reg = BridgeRegistry::new();
        let (id, rx) = reg.register();
        reg.complete(
            id,
            false,
            json!({ "code": "ELEMENT_NOT_FOUND", "message": "选择器无匹配: #nope" }),
        );
        let err = rx
            .await
            .expect("oneshot 未被丢弃")
            .expect_err("ok=false 走 Err");
        assert_eq!(err, "ELEMENT_NOT_FOUND: 选择器无匹配: #nope");
    }

    #[tokio::test]
    async fn registry_错误回包缺字段兜底() {
        let reg = BridgeRegistry::new();
        let (id, rx) = reg.register();
        reg.complete(id, false, json!("乱七八糟"));
        let err = rx
            .await
            .expect("oneshot 未被丢弃")
            .expect_err("ok=false 走 Err");
        assert!(
            err.starts_with("INTERNAL: "),
            "缺 code/message 按 INTERNAL: {err}"
        );
    }

    #[tokio::test]
    async fn registry_未知id回包不命中() {
        let reg = BridgeRegistry::new();
        assert!(!reg.complete(999, true, json!(null)), "未知 id 不命中");
        let (id, _rx) = reg.register();
        reg.complete(id, true, json!(null));
        assert!(!reg.complete(id, true, json!(null)), "重复回包不命中（已消费）");
    }

    #[tokio::test]
    async fn registry_不回包则超时() {
        let reg = BridgeRegistry::new();
        let (_id, rx) = reg.register();
        let started = std::time::Instant::now();
        let outcome = tokio::time::timeout(Duration::from_millis(50), rx).await;
        assert!(outcome.is_err(), "无人回包 → 超时");
        assert!(started.elapsed() >= Duration::from_millis(40));
    }

    #[tokio::test]
    async fn registry_并发多请求在途且乱序回包() {
        let reg = BridgeRegistry::new();
        let (id1, rx1) = reg.register();
        let (id2, rx2) = reg.register();
        let (id3, rx3) = reg.register();
        assert_eq!((id1, id2, id3), (1, 2, 3), "id 单调递增");
        assert_eq!(reg.pending_len(), 3);

        // 乱序回包：3 → 1 → 2，各回各的
        assert!(reg.complete(id3, true, json!("third")));
        assert!(reg.complete(id1, true, json!("first")));
        assert!(reg.complete(id2, false, json!({ "code": "WAIT_TIMEOUT", "message": "超时" })));

        assert_eq!(rx3.await.unwrap().unwrap(), json!("third"));
        assert_eq!(rx1.await.unwrap().unwrap(), json!("first"));
        assert_eq!(
            rx2.await.unwrap().unwrap_err(),
            "WAIT_TIMEOUT: 超时",
            "各自通道互不串扰"
        );
        assert_eq!(reg.pending_len(), 0);
    }

    #[test]
    fn eval脚本_JSON字符串字面量嵌入() {
        let req = json!({
            "id": 7,
            "action": "click",
            "params": { "selector": "[testid=nav-settings-link]" }
        });
        let js = build_eval_js(&req).expect("序列化成功");
        // 以短路保护 + exec(" 开头、") 结尾：payload 是单个 JS 字符串字面量
        assert!(
            js.starts_with("window.__testBridge&&window.__testBridge.exec(\"")
                && js.ends_with("\")"),
            "exec 收到字符串字面量: {js}"
        );
        // 字面量内部是转义过的 JSON 文本（对象字面量直嵌会让 exec 收到对象，
        // JSON.parse(object) 失败——冒烟踩坑，见函数注释）
        assert!(
            js.contains("\\\"id\\\":7"),
            "JSON 文本在字符串字面量内单层转义: {js}"
        );
        assert!(js.contains("\\\"action\\\":\\\"click\\\""), "action 同上: {js}");
        // 单层转义即可，不得出现过度转义
        assert!(!js.contains("\\\\\\\\\""), "不得双重转义: {js}");
    }

    /// Response → (status, 包络 JSON)（与 mod.rs 测试同款断言辅助）
    async fn envelope_of(resp: Response) -> (StatusCode, serde_json::Value) {
        let (parts, body) = resp.into_parts();
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .expect("响应体可读");
        let v = serde_json::from_slice(&bytes).expect("响应体为 JSON");
        (parts.status, v)
    }

    #[tokio::test]
    async fn 未就绪响应_409包络与提示语() {
        let (status, v) = envelope_of(not_ready_response()).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"]["code"], json!("BRIDGE_NOT_READY"));
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 为字符串")
                .contains("dev:test"),
            "报错要提示测试构建的启动方式"
        );
    }

    #[tokio::test]
    async fn 桥错误码映射_HTTP状态() {
        // 封闭集合内逐码映射
        let (status, v) = envelope_of(bridge_error_response("ELEMENT_NOT_FOUND: 无匹配")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["code"], json!("ELEMENT_NOT_FOUND"));

        let (status, v) = envelope_of(bridge_error_response("WAIT_TIMEOUT: 等待超时")).await;
        assert_eq!(status, StatusCode::REQUEST_TIMEOUT);
        assert_eq!(v["error"]["code"], json!("WAIT_TIMEOUT"));

        let (status, v) = envelope_of(bridge_error_response("BAD_REQUEST: 参数缺失")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(v["error"]["code"], json!("BAD_REQUEST"));

        let (status, v) = envelope_of(bridge_error_response("INTERNAL: 内部错误")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], json!("INTERNAL"));
    }

    #[tokio::test]
    async fn 桥错误码_未知码与裸消息按INTERNAL() {
        let (status, v) = envelope_of(bridge_error_response("STRANGE_CODE: boom")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], json!("INTERNAL"));
        assert!(
            v["error"]["message"]
                .as_str()
                .expect("message 为字符串")
                .contains("STRANGE_CODE"),
            "原始码保留在 message 中"
        );

        // 无 "CODE: " 前缀的裸字符串同样按 INTERNAL
        let (status, v) = envelope_of(bridge_error_response("只是一段话")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], json!("INTERNAL"));
    }
}
