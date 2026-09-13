// Task M4: /api/screenshot 窗口级截图证据（spec §7.1.8）
//
// 捕获策略（与任务卡的"xcap Window::all() 按 hwnd/标题匹配"不同，实测
// 偏差，原因记录于 plan 执行记录）：xcap 0.9.8 的 Window::all() 走
// WebRTC 语义的 is_valid_window，**显式排除当前进程自己的窗口**——
// test-api 跑在应用进程内，永远枚举不到自己（冒烟已实证 12 候选无
// 主窗口）。因此改用显示器裁剪路径：
//   tauri 主窗口 outer_position/outer_size（物理像素）
//   → 匹配 xcap Monitor（先按 tauri current_monitor 的原点全等，
//     兜底取包含窗口左上角的显示器）
//   → 窗口矩形 ∩ 显示器矩形，平移到显示器相对坐标（钳制在界内）
//   → Monitor::capture_region（GDI 桌面 BitBlt，物理像素 1:1）。
//
// 代价与缓解：桌面 BitBlt 会带上遮挡窗口——截图仅作证据不作断言
//（spec §5 原则 2），捕获前 set_focus 把窗口拉到前台尽量消除遮挡。
//
// `?restore=true`（默认）在最小化时先 unminimize 等 ~300ms 渲染稳定。
// 响应是全 test-api 唯一的非 JSON 包络端点（spec §7.1.8 明确约定）：
// body 为 `image/png` 原始字节，meta（宽度/高度/DPI 缩放）放响应头
// `X-Width`/`X-Height`/`X-Scale`。失败仍走 500 包络。

use std::collections::HashMap;
use std::io::Cursor;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use tauri::Manager;

use super::{err_envelope_with_detail, ApiState};

/// unminimize 后等待渲染稳定的时间（毫秒）
const RESTORE_SETTLE_MS: u64 = 300;
/// set_focus 后等窗口管理器把窗口提到前台的时间（毫秒）
const FOCUS_SETTLE_MS: u64 = 150;

/// `?restore=` 参数解析：缺省 true（spec §7.1.8）；显式 `false`/`0`
/// （大小写不敏感）才关闭。其余任意值按默认 true 处理（宽容解析，
/// 与 logs/tail 的"空串视为未提供"习惯一致）。
pub fn parse_restore(params: &HashMap<String, String>) -> bool {
    match params.get("restore").map(|s| s.trim()) {
        Some(v) if !v.is_empty() => !matches!(v.to_ascii_lowercase().as_str(), "false" | "0"),
        _ => true,
    }
}

/// 显示器矩形（物理像素，屏幕坐标系）：(x, y, w, h)
type Rect = (i32, i32, u32, u32);

/// 显示器定位纯函数核（单测直测，不触 xcap）：
/// 1. `exact`（tauri current_monitor 的原点）与某显示器原点全等 → 命中；
/// 2. 兜底：包含窗口左上角 `point` 的第一个显示器；
/// 3. 都不中 None（窗口在虚拟桌面外等病态场景）。
pub fn locate_monitor_index(
    monitors: &[Rect],
    exact: Option<(i32, i32)>,
    point: (i32, i32),
) -> Option<usize> {
    if let Some((mx, my)) = exact {
        if let Some(i) = monitors.iter().position(|&(x, y, _, _)| (x, y) == (mx, my)) {
            return Some(i);
        }
    }
    monitors.iter().position(|&(x, y, w, h)| {
        let (right, bottom) = (x + w as i32, y + h as i32);
        point.0 >= x && point.0 < right && point.1 >= y && point.1 < bottom
    })
}

/// 窗口矩形 ∩ 显示器矩形 → 显示器相对坐标的捕获区域。
/// 保证结果在显示器界内（xcap capture_region 越界会报错；最大化窗口
/// 带 DWM 阴影越出屏界的部分被裁掉）。空交集 None。
pub fn clamp_region(win: Rect, mon: Rect) -> Option<(u32, u32, u32, u32)> {
    let (wx, wy, ww, wh) = (win.0 as i64, win.1 as i64, win.2 as i64, win.3 as i64);
    let (mx, my, mw, mh) = (mon.0 as i64, mon.1 as i64, mon.2 as i64, mon.3 as i64);
    let left = wx.max(mx);
    let top = wy.max(my);
    let right = (wx + ww).min(mx + mw);
    let bottom = (wy + wh).min(my + mh);
    if right <= left || bottom <= top {
        return None;
    }
    Some((
        (left - mx) as u32,
        (top - my) as u32,
        (right - left) as u32,
        (bottom - top) as u32,
    ))
}

/// 主窗口（label "main" 或任一 webview 窗口），与 bridge.rs 同口径
fn main_window(app: &tauri::AppHandle) -> Option<tauri::WebviewWindow> {
    app.get_webview_window("main")
        .or_else(|| app.webview_windows().values().next().cloned())
}

/// GET /api/screenshot?restore=true → image/png 字节 + X-Width/X-Height/X-Scale
pub(super) async fn screenshot(
    State(st): State<Arc<ApiState>>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    // ---- 1. 主窗口 + 最小化还原 + 提前台 ----
    let Some(window) = main_window(&st.app) else {
        return screenshot_error("未找到应用主窗口（webview 窗口表为空）");
    };
    if parse_restore(&params) && window.is_minimized().unwrap_or(false) {
        if let Err(e) = window.unminimize() {
            tracing::warn!(target: "test_api", "screenshot unminimize 失败（继续尝试截图）: {e}");
        }
        tokio::time::sleep(Duration::from_millis(RESTORE_SETTLE_MS)).await;
    }
    // 桌面 BitBlt 会拍到遮挡物：尽力把窗口拉到前台（失败不阻断）
    let _ = window.set_focus();
    tokio::time::sleep(Duration::from_millis(FOCUS_SETTLE_MS)).await;

    // ---- 2. 窗口物理矩形 + 所在显示器 ----
    let pos = match window.outer_position() {
        Ok(p) => p,
        Err(e) => return screenshot_error(&format!("窗口位置获取失败: {e}")),
    };
    let size = match window.outer_size() {
        Ok(s) => s,
        Err(e) => return screenshot_error(&format!("窗口尺寸获取失败: {e}")),
    };
    let win_rect: Rect = (pos.x, pos.y, size.width, size.height);
    let tauri_monitor = window
        .current_monitor()
        .ok()
        .flatten()
        .map(|m| (m.position().x, m.position().y));

    // ---- 3. xcap 显示器定位 + 区域裁剪（同步函数，非 Send 值即建即弃） ----
    let (idx, rx, ry, rw, rh, scale) =
        match locate_capture_args(win_rect, tauri_monitor) {
            Ok(t) => t,
            Err(e) => return screenshot_error(&e),
        };

    // ---- 4. 捕获 + PNG 编码（阻塞调用下放线程池，不占 tokio worker） ----
    let captured = capture_png(idx, rx, ry, rw, rh).await;
    let (png, width, height) = match captured {
        Ok(Ok(t)) => t,
        Ok(Err(e)) => return screenshot_error(&e),
        Err(e) => return screenshot_error(&format!("截图任务失败: {e}")),
    };
    if png.is_empty() {
        return screenshot_error("截图结果为空（PNG 编码异常）");
    }

    // ---- 5. 原始字节响应：本端点唯一豁免 JSON 包络（spec §7.1.8） ----
    let mut resp = (StatusCode::OK, png).into_response();
    let headers = resp.headers_mut();
    headers.insert(header::CONTENT_TYPE, header::HeaderValue::from_static("image/png"));
    if let Ok(v) = header::HeaderValue::from_str(&width.to_string()) {
        headers.insert("x-width", v);
    }
    if let Ok(v) = header::HeaderValue::from_str(&height.to_string()) {
        headers.insert("x-height", v);
    }
    if let Some(s) = scale {
        if let Ok(v) = header::HeaderValue::from_str(&format!("{s:.2}")) {
            headers.insert("x-scale", v);
        }
    }
    resp
}

/// 定位阶段：窗口矩形 → (显示器下标, 显示器相对捕获区域, DPI 缩放)。
/// 纯同步——xcap Monitor 持原生 HMONITOR 非 Send，收拢在同步函数里
/// 创建即弃，handler future 不跨越任何非 Send 值（Handler 要求 Send）。
fn locate_capture_args(
    win_rect: Rect,
    tauri_monitor: Option<(i32, i32)>,
) -> Result<(usize, u32, u32, u32, u32, Option<f32>), String> {
    let monitors = xcap::Monitor::all().map_err(|e| format!("显示器枚举失败: {e}"))?;
    let mon_rects: Vec<Rect> = monitors
        .iter()
        .filter_map(|m| match (m.x(), m.y(), m.width(), m.height()) {
            (Ok(x), Ok(y), Ok(w), Ok(h)) => Some((x, y, w, h)),
            _ => None,
        })
        .collect();
    let idx = locate_monitor_index(&mon_rects, tauri_monitor, (win_rect.0, win_rect.1))
        .ok_or_else(|| {
            format!(
                "未定位到窗口所在显示器（窗口 {:?}，显示器 {:?}）",
                win_rect, mon_rects
            )
        })?;
    let (rx, ry, rw, rh) = clamp_region(win_rect, mon_rects[idx]).ok_or_else(|| {
        format!(
            "窗口与显示器无交集（窗口 {:?}，显示器 {:?}）",
            win_rect,
            mon_rects[idx]
        )
    })?;
    let scale = monitors[idx].scale_factor().ok();
    Ok((idx, rx, ry, rw, rh, scale))
}

/// xcap Monitor 非 Send（原生 HMONITOR），无法跨线程搬运——阻塞闭包内
/// 按下标重新枚举取同一显示器（枚举廉价；期间拓扑变化则报错）。
async fn capture_png(
    idx: usize,
    rx: u32,
    ry: u32,
    rw: u32,
    rh: u32,
) -> Result<Result<(Vec<u8>, u32, u32), String>, tokio::task::JoinError> {
    tokio::task::spawn_blocking(move || -> Result<(Vec<u8>, u32, u32), String> {
        let monitors = xcap::Monitor::all().map_err(|e| format!("显示器枚举失败: {e}"))?;
        let target = monitors
            .get(idx)
            .ok_or_else(|| "显示器拓扑在截图期间变化（下标越界）".to_string())?;
        let img = target
            .capture_region(rx, ry, rw, rh)
            .map_err(|e| format!("显示器区域截图失败: {e}"))?;
        let (w, h) = (img.width(), img.height());
        let mut buf = Cursor::new(Vec::new());
        // xcap 重导出 image（其 png feature 已开），无需另挂编码依赖
        xcap::image::DynamicImage::ImageRgba8(img)
            .write_to(&mut buf, xcap::image::ImageFormat::Png)
            .map_err(|e| format!("PNG 编码失败: {e}"))?;
        Ok((buf.into_inner(), w, h))
    })
    .await
}

/// 截图失败 500 包络（spec §7.1.8：截图端点失败仍走 JSON 包络；抽独立函数便于直测）
pub fn screenshot_error(message: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        axum::response::Json(err_envelope_with_detail(
            "INTERNAL",
            message,
            serde_json::json!({ "endpoint": "screenshot" }),
        )),
    )
        .into_response()
}

// ---------------------------------------------------------------------------
// 单元测试（feature 门控，`cargo test -p localtrans --features test-api`
// 覆盖）。真截图（xcap 捕获 + PNG 魔数 + X-* 头）由冒烟 smoke-m4.mjs
// 端到端覆盖（单测环境无 GUI 窗口）；此处直测定位/裁剪/参数纯函数。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore参数_缺省真_显式假() {
        let p = |pairs: &[(&str, &str)]| -> HashMap<String, String> {
            pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
        };
        // 缺省 / 空串 → true
        assert!(parse_restore(&p(&[])));
        assert!(parse_restore(&p(&[("restore", "")])));
        assert!(parse_restore(&p(&[("restore", "  ")])));
        // true/1/任意其他值 → true
        assert!(parse_restore(&p(&[("restore", "true")])));
        assert!(parse_restore(&p(&[("restore", "1")])));
        assert!(parse_restore(&p(&[("restore", "yes")])));
        // false/0，大小写不敏感 → false
        assert!(!parse_restore(&p(&[("restore", "false")])));
        assert!(!parse_restore(&p(&[("restore", "FALSE")])));
        assert!(!parse_restore(&p(&[("restore", "0")])));
        // 无关参数不影响
        assert!(parse_restore(&p(&[("foo", "false")])));
    }

    #[test]
    fn 显示器定位_原点全等优先() {
        let monitors: Vec<Rect> = vec![
            (-1920, 0, 1920, 1080), // 主屏左侧副屏
            (0, 0, 2560, 1440),    // 主屏
            (2560, 0, 1920, 1080), // 右侧副屏
        ];
        // tauri current_monitor 原点全等命中（即使窗口左上角落在别处）
        assert_eq!(locate_monitor_index(&monitors, Some((-1920, 0)), (100, 100)), Some(0));
        assert_eq!(locate_monitor_index(&monitors, Some((2560, 0)), (100, 100)), Some(2));
        // 无 exact（current_monitor 取不到）→ 包含窗口左上角者
        assert_eq!(locate_monitor_index(&monitors, None, (3000, 100)), Some(2));
        assert_eq!(locate_monitor_index(&monitors, None, (-1000, 100)), Some(0));
        // exact 不匹配任何原点 → 回落包含点
        assert_eq!(locate_monitor_index(&monitors, Some((999, 999)), (100, 100)), Some(1));
        // 点在所有显示器外 → None
        assert_eq!(locate_monitor_index(&monitors, None, (5000, 5000)), None);
        // 边界：左上角恰在显示器边缘（半开区间，不含右/下边）
        assert_eq!(locate_monitor_index(&monitors, None, (2560, 0)), Some(2));
        assert_eq!(locate_monitor_index(&monitors, None, (4480, 0)), None, "右屏右边缘外");
        // 空显示器表
        assert_eq!(locate_monitor_index(&[], Some((0, 0)), (0, 0)), None);
    }

    #[test]
    fn 区域裁剪_窗口在显示器内() {
        let mon: Rect = (0, 0, 2560, 1440);
        // 完全在内：原样平移（此处已相对）
        assert_eq!(clamp_region((100, 200, 800, 600), mon), Some((100, 200, 800, 600)));
        // 原点窗口
        assert_eq!(clamp_region((0, 0, 2560, 1440), mon), Some((0, 0, 2560, 1440)));
    }

    #[test]
    fn 区域裁剪_越界钳制与负坐标() {
        // 右下越出显示器 → 裁到显示器边界
        let mon: Rect = (0, 0, 1920, 1080);
        assert_eq!(clamp_region((1800, 1000, 400, 300), mon), Some((1800, 1000, 120, 80)));
        // 最大化窗口带 DWM 阴影越出左/上（负坐标显示器同样处理）
        let mon_left: Rect = (-1920, 0, 1920, 1080);
        assert_eq!(
            clamp_region((-1928, -8, 1936, 1096), mon_left),
            Some((0, 0, 1920, 1080))
        );
        // 窗口跨界两显示器：只留所匹配显示器内的部分
        let mon: Rect = (0, 0, 1280, 720);
        assert_eq!(clamp_region((1000, 100, 800, 500), mon), Some((1000, 100, 280, 500)));
    }

    #[test]
    fn 区域裁剪_无交集None() {
        let mon: Rect = (0, 0, 1920, 1080);
        assert_eq!(clamp_region((2000, 0, 100, 100), mon), None);
        assert_eq!(clamp_region((-200, -200, 100, 100), mon), None);
        // 恰好贴边不算相交（半开区间）
        assert_eq!(clamp_region((1920, 0, 10, 10), mon), None);
        assert_eq!(clamp_region((0, 1080, 10, 10), mon), None);
        // 零尺寸窗口
        assert_eq!(clamp_region((100, 100, 0, 0), mon), None);
    }

    #[tokio::test]
    async fn 失败包络_500_INTERNAL() {
        let (parts, body) = screenshot_error("boom").into_parts();
        assert_eq!(parts.status, StatusCode::INTERNAL_SERVER_ERROR);
        let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ok"], serde_json::json!(false));
        assert_eq!(v["error"]["code"], serde_json::json!("INTERNAL"));
        assert_eq!(v["error"]["detail"]["endpoint"], serde_json::json!("screenshot"));
        assert!(
            v["error"]["message"].as_str().unwrap().contains("boom"),
            "message 应透传错误信息"
        );
    }
}
