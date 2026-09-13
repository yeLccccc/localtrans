// Task M1: 日志环形缓冲 layer（spec §7.1.7-1）
//
// 挂在现有 tracing subscriber 上，把每条经过 EnvFilter 的事件镜像进
// 容量 10000 的 VecDeque（满则弹出最老一条），`/api/logs/tail` 用
// seq 游标做增量拉取，单次上限 500 条。
//
// seq 由 AtomicU64 单调分配（从 1 开始，缺省 afterSeq=0 语义为"全部"），
// 与"条目是否已被淘汰"解耦：客户端 afterSeq 落后于缓冲头部时，拉到的
// 是从幸存条目开始的增量，游标照常前进。
//
// runId/step 从共享 TestContext 读取（spec §7.1.7-3 run/step 关联）：
// M1 阶段恒为 None，M4 的 test/begin、test/step 端点负责写入，
// 事后按 runId 过滤即可提取按步骤分段的完整时间线。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

/// 环形缓冲容量（spec §7.1.7：1 万条）
pub const RING_CAPACITY: usize = 10_000;

/// logs/tail 单次返回上限（spec §7.1.7）
pub const TAIL_MAX_ENTRIES: usize = 500;

/// 单条日志。HTTP 序列化字段名与 spec §7.1.7 对齐：
/// `{seq, ts, level, target, message, runId, step}`（ts 为 epoch 毫秒，
/// runId/step 为 null 时整个键缺省）
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LogEntry {
    pub seq: u64,
    #[serde(rename = "ts")]
    pub ts_ms: u64,
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(rename = "runId", skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
}

/// 测试上下文（run/step 关联）：layer 读取，M4 的 test/begin、test/step 写入。
/// Arc 共享给 ring 与后续端点，读写均为短临界区。
#[derive(Default)]
pub struct TestContext {
    run_id: Mutex<Option<String>>,
    step: Mutex<Option<String>>,
}

impl TestContext {
    /// 当前 (runId, step) 快照
    pub fn snapshot(&self) -> (Option<String>, Option<String>) {
        let run = self.run_id.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let step = self.step.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (run, step)
    }

    /// 设置当前 runId（M4 test/begin/end 接入；None 表示结束 run）
    pub fn set_run_id(&self, v: Option<String>) {
        *self.run_id.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }

    /// 设置当前 step 标记（M4 test/step 接入）
    pub fn set_step(&self, v: Option<String>) {
        *self.step.lock().unwrap_or_else(|e| e.into_inner()) = v;
    }
}

/// logs/tail 查询参数（已归一：空串视为未提供）
#[derive(Debug, Default, Clone)]
pub struct TailQuery {
    /// 只取 seq 严格大于它的条目（缺省 0 = 全部）
    pub after_seq: u64,
    /// level 精确匹配某一级（大小写不敏感；不是阈值）
    pub level: Option<String>,
    /// target 前缀匹配（`ui` 同时命中 `ui` 与 `ui::child`）
    pub target: Option<String>,
    /// runId 精确匹配（缺省不过滤）
    pub run_id: Option<String>,
}

/// tail 结果：命中条目 + 下次查询应传的 afterSeq 游标
#[derive(Debug, Clone)]
pub struct TailPage {
    pub entries: Vec<LogEntry>,
    pub next_seq: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 过滤规则（与 docs/contracts/test-api.md §5 logs/tail 语义一致）
fn entry_matches(e: &LogEntry, q: &TailQuery) -> bool {
    if e.seq <= q.after_seq {
        return false;
    }
    if let Some(level) = &q.level {
        if !e.level.eq_ignore_ascii_case(level) {
            return false;
        }
    }
    if let Some(target) = &q.target {
        if !e.target.starts_with(target.as_str()) {
            return false;
        }
    }
    if let Some(run_id) = &q.run_id {
        if e.run_id.as_deref() != Some(run_id.as_str()) {
            return false;
        }
    }
    true
}

/// 环形缓冲本体。layer（on_event）与 logs/tail 端点各持 Arc 共享。
pub struct RingBuffer {
    entries: Mutex<VecDeque<LogEntry>>,
    next_seq: AtomicU64,
    ctx: Arc<TestContext>,
}

impl Default for RingBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl RingBuffer {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(VecDeque::with_capacity(RING_CAPACITY)),
            // 从 1 分配：afterSeq 缺省 0 语义为"从头拉全部"
            next_seq: AtomicU64::new(1),
            ctx: Arc::new(TestContext::default()),
        }
    }

    /// 共享测试上下文（M4 的 test/begin、test/step、test/end 端点写入 run/step 用）
    pub fn test_context(&self) -> Arc<TestContext> {
        self.ctx.clone()
    }

    /// 写入一条（on_event 与单元测试共用路径）。
    /// 返回分配到的 seq；满了弹最老一条。
    pub fn push(&self, level: &str, target: &str, message: String) -> u64 {
        let (run_id, step) = self.ctx.snapshot();
        let seq = self.next_seq.fetch_add(1, Ordering::Relaxed);
        let entry = LogEntry {
            seq,
            ts_ms: now_ms(),
            level: level.to_string(),
            target: target.to_string(),
            message,
            run_id,
            step,
        };
        let mut q = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if q.len() >= RING_CAPACITY {
            q.pop_front();
        }
        q.push_back(entry);
        seq
    }

    /// 当前缓冲条数（仅测试用于直接校验容量语义）
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// 过滤 + 游标拉取，最多 TAIL_MAX_ENTRIES 条。
    /// nextSeq 语义（与"seq 严格大于 afterSeq"配套）：
    /// - 有命中 → 本次返回的最后一条 seq。下次传 afterSeq=nextSeq，
    ///   严格大于语义既不重复也不丢条（不能用 last+1，否则整页翻页会漏一条）；
    /// - 无命中 → 跳到 ring 已分配的最高 seq（被过滤/淘汰的无需重扫），
    ///   且不低于调用方传入的 afterSeq（客户端领先时不倒退）。
    pub fn tail(&self, q: &TailQuery) -> TailPage {
        let guard = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let mut entries = Vec::new();
        let mut last_matched: Option<u64> = None;
        for e in guard.iter() {
            if entry_matches(e, q) {
                entries.push(e.clone());
                last_matched = Some(e.seq);
                if entries.len() >= TAIL_MAX_ENTRIES {
                    break;
                }
            }
        }
        drop(guard);
        let next_seq = match last_matched {
            Some(last) => last,
            None => self
                .next_seq
                .load(Ordering::Relaxed)
                .saturating_sub(1)
                .max(q.after_seq),
        };
        TailPage { entries, next_seq }
    }

    /// 当前缓冲中 runId 匹配的条数（test/end 的统计载荷，M4）。
    /// 全量计数不受 TAIL_MAX_ENTRIES 限制；被容量淘汰的不计（幸存条目口径）。
    pub fn count_run(&self, run_id: &str) -> usize {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|e| e.run_id.as_deref() == Some(run_id))
            .count()
    }
}

static GLOBAL_RING: OnceLock<Arc<RingBuffer>> = OnceLock::new();

/// 全局共享实例：main.rs 挂 layer、logs/tail 端点读取，各取同一份。
pub fn global() -> Arc<RingBuffer> {
    GLOBAL_RING.get_or_init(|| Arc::new(RingBuffer::new())).clone()
}

/// 环形缓冲 layer：所有经 EnvFilter 的事件同步镜像进 RingBuffer。
pub struct RingLayer {
    ring: Arc<RingBuffer>,
}

impl RingLayer {
    /// 从指定缓冲构造（测试可用独立实例，避免污染全局）
    pub fn new(ring: Arc<RingBuffer>) -> Self {
        Self { ring }
    }
}

/// 挂载全局实例的 layer（main.rs subscriber 组装入口）
pub fn layer() -> RingLayer {
    RingLayer::new(global())
}

impl<S> Layer<S> for RingLayer
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        let mut extractor = FieldExtractor::default();
        event.record(&mut extractor);
        let meta = event.metadata();
        self.ring
            .push(meta.level().as_str(), meta.target(), extractor.into_message());
    }
}

/// 字段提取：`message` 字段为正文，其余非空字段以 ` k=v` 追加到尾部
/// （spec §7.1.7：message 取 record 的 message 字段，其余字段附加）。
/// Visit 的各 record_* 默认都转发到 record_debug，实现它即可全覆盖。
#[derive(Default)]
struct FieldExtractor {
    message: String,
    extras: Vec<String>,
}

impl tracing::field::Visit for FieldExtractor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            use std::fmt::Write as _;
            let mut buf = String::new();
            // Debug 渲染 message：tracing 对 message 字段实际传 fmt::Arguments，
            // 走 record_str/record_debug 默认链，Debug 输出与 Display 等价
            let _ = write!(buf, "{value:?}");
            self.message = buf;
        } else {
            let v = format!("{value:?}");
            if !v.is_empty() {
                self.extras.push(format!(" {}={}", field.name(), v));
            }
        }
    }
}

impl FieldExtractor {
    fn into_message(mut self) -> String {
        for extra in self.extras {
            self.message.push_str(&extra);
        }
        self.message
    }
}

// ---------------------------------------------------------------------------
// 单元测试（模块整体被 feature 门控，由 `cargo test -p localtrans
// --features test-api` 覆盖）。直接构造 RingBuffer / RingLayer 测，
// 不起全局 subscriber。
// ---------------------------------------------------------------------------
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seq_从1开始且连续() {
        let ring = RingBuffer::new();
        for i in 0..5 {
            assert_eq!(ring.push("INFO", "t", format!("m{i}")), (i + 1) as u64);
        }
        let page = ring.tail(&TailQuery::default());
        let seqs: Vec<u64> = page.entries.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
        // nextSeq = 最后一条命中的 seq；下次传 afterSeq=nextSeq（严格大于）不重复
        assert_eq!(page.next_seq, 5);
    }

    #[test]
    fn 容量淘汰_保留最新一万条() {
        let ring = RingBuffer::new();
        for i in 0..=(RING_CAPACITY as u64) {
            ring.push("INFO", "t", format!("m{i}"));
        }
        // 恰好封顶 10000，最老的 seq=1 被弹出
        assert_eq!(ring.len(), RING_CAPACITY);
        // tail 受单次上限约束，但首屏第一条即最老幸存条目：seq=2（m1）
        let page = ring.tail(&TailQuery::default());
        assert_eq!(page.entries.len(), TAIL_MAX_ENTRIES);
        assert_eq!(page.entries.first().unwrap().seq, 2);
        assert_eq!(page.entries.first().unwrap().message, "m1");
        // 末尾游标续读可到达最新一条 seq=10001（m10000）
        let mut after = 0u64;
        let mut last: Option<LogEntry> = None;
        loop {
            let p = ring.tail(&TailQuery { after_seq: after, ..Default::default() });
            if p.entries.is_empty() {
                break;
            }
            last = p.entries.last().cloned();
            after = p.next_seq;
        }
        let last = last.expect("存在最后一条");
        assert_eq!(last.seq, (RING_CAPACITY + 1) as u64);
        assert_eq!(last.message, format!("m{}", RING_CAPACITY));
    }

    #[test]
    fn 过滤_level精确匹配_大小写不敏感() {
        let ring = RingBuffer::new();
        ring.push("INFO", "ui", "a".into());
        ring.push("WARN", "ui", "b".into());
        ring.push("ERROR", "ui", "c".into());
        ring.push("info", "ui", "d".into()); // 非规范大小写入参也能命中

        let q = |level: &str| TailQuery { level: Some(level.into()), ..Default::default() };
        assert_eq!(ring.tail(&q("WARN")).entries.iter().map(|e| e.message.as_str()).collect::<Vec<_>>(), vec!["b"]);
        // 大小写不敏感
        assert_eq!(ring.tail(&q("warn")).entries.len(), 1);
        // INFO 与 info 视为同级的两条都命中
        assert_eq!(ring.tail(&q("info")).entries.len(), 2);
        // 精确匹配而非阈值：查 ERROR 不含 WARN/INFO
        assert_eq!(ring.tail(&q("ERROR")).entries.len(), 1);
        // 不存在的级别：空结果而非报错
        assert!(ring.tail(&q("TRACE")).entries.is_empty());
    }

    #[test]
    fn 过滤_target前缀匹配() {
        let ring = RingBuffer::new();
        ring.push("INFO", "ui", "a".into());
        ring.push("INFO", "ui::sub", "b".into());
        ring.push("INFO", "uix", "c".into());
        ring.push("INFO", "localtrans_core::discovery", "d".into());

        let q = |target: &str| TailQuery { target: Some(target.into()), ..Default::default() };
        let msgs = |p: &TailPage| -> Vec<String> {
            p.entries.iter().map(|e| e.message.clone()).collect()
        };
        // 前缀匹配按字符串前缀：ui 命中 ui、ui::sub 与 uix（三者都以 "ui" 开头）
        assert_eq!(msgs(&ring.tail(&q("ui"))), vec!["a", "b", "c"]);
        assert_eq!(msgs(&ring.tail(&q("ui::"))), vec!["b"]);
        assert_eq!(msgs(&ring.tail(&q("uix"))), vec!["c"]);
        assert_eq!(msgs(&ring.tail(&q("localtrans_core::discovery"))), vec!["d"]);
        assert!(ring.tail(&q("nope")).entries.is_empty());
    }

    #[test]
    fn 过滤_runId精确匹配与缺省不过滤() {
        let ring = RingBuffer::new();
        let ctx = ring.test_context();

        ctx.set_run_id(Some("run-1".into()));
        ring.push("INFO", "t", "in-run-1".into());
        ctx.set_run_id(Some("run-2".into()));
        ring.push("INFO", "t", "in-run-2".into());
        ctx.set_run_id(None);
        ring.push("INFO", "t", "outside-run".into());

        let q = |run: Option<&str>| TailQuery { run_id: run.map(str::to_string), ..Default::default() };
        let msgs = |p: &TailPage| -> Vec<String> {
            p.entries.iter().map(|e| e.message.clone()).collect()
        };
        assert_eq!(msgs(&ring.tail(&q(Some("run-1")))), vec!["in-run-1"]);
        assert_eq!(msgs(&ring.tail(&q(Some("run-2")))), vec!["in-run-2"]);
        // runId 缺省不过滤，全量 3 条
        assert_eq!(msgs(&ring.tail(&q(None))).len(), 3);
    }

    #[test]
    fn 过滤_afterSeq严格大于与组合() {
        let ring = RingBuffer::new();
        ring.push("INFO", "ui", "a".into()); // seq 1
        ring.push("WARN", "ui", "b".into()); // seq 2
        ring.push("WARN", "ui::x", "c".into()); // seq 3
        ring.push("WARN", "core", "d".into()); // seq 4

        // afterSeq=2 → 只剩 3、4
        let p = ring.tail(&TailQuery { after_seq: 2, ..Default::default() });
        assert_eq!(p.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![3, 4]);
        assert_eq!(p.next_seq, 4);

        // 组合：afterSeq=1 + level=WARN + target=ui → 命中 2（WARN+ui）、3（WARN+ui::x）
        let p = ring.tail(&TailQuery {
            after_seq: 1,
            level: Some("WARN".into()),
            target: Some("ui".into()),
            run_id: None,
        });
        assert_eq!(p.entries.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(p.next_seq, 3);

        // afterSeq 指向最后一条 → 空，游标不倒退不重复
        let p = ring.tail(&TailQuery { after_seq: 4, ..Default::default() });
        assert!(p.entries.is_empty());
        assert_eq!(p.next_seq, 4);
    }

    #[test]
    fn tail单次上限500条_游标续读() {
        let ring = RingBuffer::new();
        for i in 0..600u64 {
            ring.push("INFO", "ui", format!("m{i}"));
        }
        let p1 = ring.tail(&TailQuery { target: Some("ui".into()), ..Default::default() });
        assert_eq!(p1.entries.len(), TAIL_MAX_ENTRIES);
        assert_eq!(p1.entries.last().unwrap().seq, 500);
        assert_eq!(p1.next_seq, 500);

        // 第二次从游标续读（afterSeq=500，严格大于），不重复不丢条，余量 100 条
        let p2 = ring.tail(&TailQuery {
            after_seq: p1.next_seq,
            target: Some("ui".into()),
            ..Default::default()
        });
        assert_eq!(p2.entries.len(), 100);
        assert!(p2.entries.iter().all(|e| e.seq > 500));
        assert_eq!(p2.entries.first().unwrap().seq, 501);
        assert_eq!(p2.next_seq, 600);
    }

    #[test]
    fn 无命中_nextSeq跳到已分配高位() {
        let ring = RingBuffer::new();
        ring.push("INFO", "core", "a".into());
        ring.push("INFO", "core", "b".into());
        // 过滤掉全部（target 不匹配）→ nextSeq = ring 已分配的最高 seq（=2），
        // 被过滤的条目下轮无需重扫
        let p = ring.tail(&TailQuery { target: Some("ui".into()), ..Default::default() });
        assert!(p.entries.is_empty());
        assert_eq!(p.next_seq, 2);

        // 客户端游标领先（afterSeq=100 > 高位）→ 原样返回不倒退
        let p = ring.tail(&TailQuery {
            after_seq: 100,
            target: Some("ui".into()),
            ..Default::default()
        });
        assert_eq!(p.next_seq, 100);
    }

    #[test]
    fn layer_字段提取_message与kv附加() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let ring = Arc::new(RingBuffer::new());
        let subscriber = tracing_subscriber::registry().with(RingLayer::new(ring.clone()));
        tracing::dispatcher::with_default(&tracing::Dispatch::new(subscriber), || {
            tracing::warn!(target: "ui", level = "warn", route = "/settings", "{}", "logBridge attached");
            tracing::info!(target: "localtrans::main", "启动 {} v{}", "LocalTrans", "0.12.0");
        });
        let page = ring.tail(&TailQuery { target: Some("ui".into()), ..Default::default() });
        assert_eq!(page.entries.len(), 1);
        let e = &page.entries[0];
        assert_eq!(e.level, "WARN");
        assert_eq!(e.target, "ui");
        // 字面量字段走 record_str→Debug（带引号）；ui_log 命令用 %/? 记录则不带
        assert_eq!(e.message, "logBridge attached level=\"warn\" route=\"/settings\"");
        assert_eq!(e.run_id, None);
        assert_eq!(e.step, None);
        assert!(e.ts_ms > 1_500_000_000, "ts 为 epoch 毫秒");

        // 多字段按出现顺序拼接
        let page = ring.tail(&TailQuery { target: Some("localtrans::main".into()), ..Default::default() });
        assert_eq!(page.entries[0].message, "启动 LocalTrans v0.12.0");
    }

    #[test]
    fn 序列化_字段名与spec对齐() {
        let e = LogEntry {
            seq: 7,
            ts_ms: 1_700_000_000_000,
            level: "INFO".into(),
            target: "ui".into(),
            message: "m".into(),
            run_id: Some("run-1".into()),
            step: Some("s1".into()),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["seq"], serde_json::json!(7));
        assert_eq!(v["ts"], serde_json::json!(1_700_000_000_000u64));
        assert_eq!(v["runId"], serde_json::json!("run-1"));
        assert_eq!(v["step"], serde_json::json!("s1"));

        // runId/step 为 None 时整个键缺省
        let e2 = LogEntry { run_id: None, step: None, ..e };
        let v2 = serde_json::to_value(&e2).unwrap();
        assert!(v2.get("runId").is_none());
        assert!(v2.get("step").is_none());
    }

    #[test]
    fn 全局实例_跨调用共享() {
        let a = global();
        a.push("INFO", "ui", "shared".into());
        let b = global();
        let p = b.tail(&TailQuery { target: Some("ui".into()), ..Default::default() });
        assert!(p.entries.iter().any(|e| e.message == "shared"));
    }
}
