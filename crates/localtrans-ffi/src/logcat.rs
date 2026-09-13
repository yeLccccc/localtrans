//! Android logcat 桥(M7,spec §7.3):自实现 tracing Layer,把 core/ffi 的
//! tracing 日志经 NDK `__android_log_write` 写入 logcat。
//!
//! 选型:不走 android_logger + tracing-log(那套是 log facade 的反向桥,
//! 需要两个新 crate 且版本耦合);自定义 Layer + `#[link(name = "log")]`
//! 直接绑 liblog(NDK sysroot 自带,android_log-sys 同款做法),依赖增量为
//! 零——tracing-subscriber 本就是 workspace 依赖(仅 android target 引入)。
//!
//! tag 规则:`LT::` + target 简化——`localtrans_core::foo::bar` →
//! `LT::core::foo`(crate 名缩写 + 至多保留一段子模块,防超长截断)。
//! level 映射:TRACE→VERBOSE(2) DEBUG→DEBUG(3) INFO→INFO(4) WARN→WARN(5)
//! ERROR→ERROR(6)。默认过滤 INFO(logcat 量可控;横幅为 INFO)。
//!
//! 仅 `cfg(target_os = "android")` 编译,host 构建零行为变化。

use std::ffi::CString;
use std::os::raw::c_char;

/// android/log.h 优先级常量
const PRIO_VERBOSE: i32 = 2;
const PRIO_DEBUG: i32 = 3;
const PRIO_INFO: i32 = 4;
const PRIO_WARN: i32 = 5;
const PRIO_ERROR: i32 = 6;

/// tag 上限:logd 现代实现接受 127,保守取 64 兼容老设备(超出按 UTF-8
/// 字符边界截断,不会 panic)
const TAG_MAX: usize = 64;
const TAG_PREFIX: &str = "LT::";

// NDK liblog 直绑。liblog.so 在 Android sysroot 中,无需额外链接参数。
// (普通注释而非 ///:rustdoc 不为 extern 块生成文档,doc 注释只会告警)
#[link(name = "log")]
extern "C" {
    fn __android_log_write(prio: i32, tag: *const c_char, text: *const c_char) -> i32;
}

/// 单条写入。多行消息按行拆分(logcat 每条一行,嵌入式 \n 会破坏按行过滤);
/// 含 NUL 的行跳过(__android_log_write 以 NUL 结尾)。
fn write_logcat(prio: i32, tag: &str, msg: &str) {
    let full = format!("{TAG_PREFIX}{tag}");
    let Ok(tag_c) = CString::new(truncate_utf8(&full, TAG_MAX)) else { return };
    for line in msg.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.is_empty() {
            continue;
        }
        let Ok(line_c) = CString::new(line) else { continue };
        unsafe {
            __android_log_write(prio, tag_c.as_ptr(), line_c.as_ptr());
        }
    }
}

/// 按 UTF-8 字符边界截断(不可用 Stringtruncate 的 panic 路径)。
fn truncate_utf8(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// target 简化:crate 名缩写 + 至多一段子模块。
/// `localtrans_core::discovery::xxx` → `core::discovery`;
/// `localtrans_ffi::state` → `ffi::state`;`localtrans_ffi` → `ffi`;
/// 其余 target(依赖库)原样取前两段。
pub(crate) fn simplify_target(target: &str) -> String {
    let mapped = if let Some(rest) = target.strip_prefix("localtrans_core::") {
        format!("core::{rest}")
    } else if target == "localtrans_core" {
        "core".to_string()
    } else if let Some(rest) = target.strip_prefix("localtrans_ffi::") {
        format!("ffi::{rest}")
    } else if target == "localtrans_ffi" {
        "ffi".to_string()
    } else {
        target.to_string()
    };
    let mut segs = mapped.split("::");
    match (segs.next(), segs.next()) {
        (Some(a), Some(b)) => format!("{a}::{b}"),
        (Some(a), None) => a.to_string(),
        _ => mapped,
    }
}

/// tracing 字段访问器:提取 message 字段,其余字段以 `k=v` 追加(无则只留消息)。
#[derive(Default)]
struct FieldsVisitor {
    message: String,
    extra: String,
}

impl FieldsVisitor {
    fn push_extra(&mut self, name: &str, value: String) {
        if !self.extra.is_empty() {
            self.extra.push(' ');
        }
        self.extra.push_str(name);
        self.extra.push('=');
        self.extra.push_str(&value);
    }
    fn finish(mut self) -> String {
        if self.extra.is_empty() {
            self.message
        } else {
            if !self.message.is_empty() {
                self.message.push(' ');
            }
            self.message.push_str(&self.extra);
            self.message
        }
    }
}

impl tracing::field::Visit for FieldsVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_owned();
        } else {
            self.push_extra(field.name(), value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            // message 经 record_str 走不到这里才会落 Debug(带引号,可接受)
            self.message = format!("{value:?}");
        } else {
            self.push_extra(field.name(), format!("{value:?}"));
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.push_extra(field.name(), value.to_string());
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.push_extra(field.name(), value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.push_extra(field.name(), value.to_string());
    }

    fn record_f64(&mut self, field: &tracing::field::Field, value: f64) {
        self.push_extra(field.name(), value.to_string());
    }

    fn record_error(
        &mut self,
        field: &tracing::field::Field,
        value: &(dyn std::error::Error + 'static),
    ) {
        self.push_extra(field.name(), value.to_string());
    }
}

/// logcat Layer:每个 tracing 事件 → 一条(或多行多条)logcat 记录。
struct LogcatLayer;

impl<S> tracing_subscriber::Layer<S> for LogcatLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let meta = event.metadata();
        let prio = match *meta.level() {
            tracing::Level::TRACE => PRIO_VERBOSE,
            tracing::Level::DEBUG => PRIO_DEBUG,
            tracing::Level::INFO => PRIO_INFO,
            tracing::Level::WARN => PRIO_WARN,
            tracing::Level::ERROR => PRIO_ERROR,
        };
        let mut visitor = FieldsVisitor::default();
        event.record(&mut visitor);
        write_logcat(prio, &simplify_target(meta.target()), &visitor.finish());
    }
}

/// 初始化全局 subscriber(进程一次)。try_init 而非 init:壳层若已装过
/// 全局 subscriber 不 panic,保持 FFI 永不因日志崩进程的原则。
pub(crate) fn init() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        // P0-7 排障:Rust panic 默认写 stderr,Android 上不可见——tokio 任务
        // panic 只杀任务不杀进程,表现为"某条事件链静默死亡"。装 panic 钩子
        // 把 panic 转入 logcat(tag LT::panic),让任务级 panic 可观测。
        std::panic::set_hook(Box::new(|info| {
            write_logcat(6 /* ERROR */, "panic", &format!("RUST PANIC: {}", info));
        }));

        let subscriber = tracing_subscriber::registry()
            .with(tracing_subscriber::filter::LevelFilter::INFO)
            .with(LogcatLayer);
        let _ = subscriber.try_init();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simplify_target_rules() {
        assert_eq!(simplify_target("localtrans_core"), "core");
        assert_eq!(
            simplify_target("localtrans_core::discovery::inner::x"),
            "core::discovery"
        );
        assert_eq!(simplify_target("localtrans_ffi::state"), "ffi::state");
        assert_eq!(simplify_target("localtrans_ffi"), "ffi");
        assert_eq!(simplify_target("quinn::endpoint"), "quinn::endpoint");
        assert_eq!(simplify_target("hyper::server::conn::http1"), "hyper::server");
    }

    #[test]
    fn truncate_respects_char_boundary() {
        assert_eq!(truncate_utf8("abcd", 10), "abcd");
        assert_eq!(truncate_utf8("abcdefgh", 3), "abc");
        // 多字节字符不被切半
        let s = "ab中c";
        assert_eq!(truncate_utf8(s, 3), "ab");
    }
}
