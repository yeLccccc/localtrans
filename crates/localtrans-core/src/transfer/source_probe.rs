//! Sender-side 速度 / 健康度采样任务(500ms 周期)

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::oneshot;
use quinn::Connection;

use crate::transfer::ProgressEvent;

/// M-C3: 空闲判定纯函数,便于单测。返回更新后的空闲 tick 数与是否应自杀。
/// 判据:字节无进展且无在途流 → idle+1;否则清零;满 120 tick(60s)→ 退出。
pub(crate) fn idle_tick_step(idle_ticks: u64, byte_idle: bool) -> (u64, bool) {
    const IDLE_EXIT_TICKS: u64 = 120;
    if !byte_idle {
        return (0, false);
    }
    let t = idle_ticks + 1;
    (t, t >= IDLE_EXIT_TICKS)
}

pub async fn run_source_probe(
    conn: Connection,
    job_id: u64,
    progress: tokio::sync::mpsc::Sender<ProgressEvent>,
    bytes_counter: Arc<std::sync::atomic::AtomicU64>,
    active_streams: Arc<std::sync::atomic::AtomicU32>,
    remote_done: Arc<std::sync::atomic::AtomicU64>,
    mut stop_rx: oneshot::Receiver<()>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
    interval.tick().await; // 跳过首个立即 tick
    let mut last_bytes = 0u64;
    let mut last_time = Instant::now();

    // v0.2.9 空闲自杀：小文件批流推送不产生 sender 任务（无 MetaReq），
    // 收尾没有任何组件调 fire_probe_stop；大文件推送 v0.6.x 起回 RecvAck
    // 已能即时停探针，但小推送路径仍靠这里兜底。探针 500ms 循环空转——
    // Speed 事件持续驱动壳层锁表+置脏+写盘，"传输完 CPU 下不来"的元凶。
    // 判据：无在途流且字节计数持续不增长 → 退出。
    // 阈值取 60s 而非更短：暂停中的任务（对端 Pause 后 bytes 停增）探针
    // 也会被杀——恢复传输不受影响（FetchReq 服务不依赖探针），代价只是
    // 长暂停后速度/健康度面板停更，换常驻 CPU 归零。
    // M-C3: 空闲计数按 tick 计数(500ms/次,120 tick = 60s)。旧实现
    // `idle_secs += elapsed.max(0.5) as u64` 把 0.5s 截断成 0——空闲
    // 时间几乎永不累加,探针实际不会自杀,CPU 空转问题回归。
    const IDLE_EXIT_SECS: u64 = 60;
    const IDLE_EXIT_TICKS: u64 = IDLE_EXIT_SECS * 2; // 500ms tick × 2/s
    let mut idle_ticks: u64 = 0;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = &mut stop_rx => break,
            _ = conn.closed() => {
                tracing::debug!("sender 连接已关闭,探测任务退出 job={}", job_id);
                break;
            }
        }

        let stats = conn.stats().path;
        let now = Instant::now();
        let elapsed = now.duration_since(last_time).as_secs_f64();
        let cur = bytes_counter.load(Ordering::Relaxed);
        let bps = if elapsed > 0.0 && cur > last_bytes {
            ((cur - last_bytes) as f64 * 8.0 / elapsed) as u64
        } else {
            0
        };

        // 空闲判定与自杀（不发 SourceDone——"空闲"不等于"完成"，
        // 终态语义只属于 RecvAck/JobDone/Failed 路径）
        let streams_now = active_streams.load(Ordering::Relaxed);
        if cur == last_bytes && streams_now == 0 {
            let (t, exit) = idle_tick_step(idle_ticks, true);
            idle_ticks = t;
            if exit {
                tracing::info!("sender 探针空闲 {}s 自杀: job {}", IDLE_EXIT_SECS, job_id);
                break;
            }
        } else {
            idle_ticks = idle_tick_step(idle_ticks, false).0; // 清零
        }

        let loss_ratio = if stats.sent_packets > 0 {
            stats.lost_packets as f64 / stats.sent_packets as f64
        } else {
            0.0
        };

        let _ = progress
            .send(ProgressEvent::SourceSpeed {
                job_id,
                bps,
                loss_ratio,
                rtt_ms: stats.rtt.as_millis() as u64,
                cwnd: stats.cwnd,
                streams: streams_now,
                remote_done: remote_done.load(Ordering::Relaxed),
            })
            .await;

        last_bytes = cur;
        last_time = now;
    }
}

#[cfg(test)]
mod tests {
    use super::idle_tick_step;

    #[test]
    fn idle_accumulates_per_tick_and_exits_at_120() {
        // M-C3: 旧实现 0.5s 截断成 0,空闲永不累加;新实现按 tick 累加,
        // 第 120 个 tick(=60s)恰好触发自杀
        let mut t = 0u64;
        for _ in 0..119 {
            let (nt, exit) = idle_tick_step(t, true);
            assert!(!exit, "第 {} tick 不应退出", nt);
            t = nt;
        }
        let (t120, exit) = idle_tick_step(t, true);
        assert_eq!(t120, 120);
        assert!(exit, "第 120 tick(60s)应触发自杀");
    }

    #[test]
    fn activity_resets_idle_counter() {
        // 中途有字节进展 → 计数清零重新累计
        let (t1, _) = idle_tick_step(0, true);
        let (t2, _) = idle_tick_step(t1, true);
        let (t3, exit) = idle_tick_step(t2, false);
        assert_eq!(t3, 0);
        assert!(!exit);
    }
}
