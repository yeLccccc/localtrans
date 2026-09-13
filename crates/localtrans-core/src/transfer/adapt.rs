// T14: 自适应并发流（T15 补全探测逻辑）

/// 自适应并发流控制器
/// R2 裁定：T14 仅实现最小常量版本，T15 补充探测逻辑
#[derive(Clone)]
pub struct AdaptiveStreams {
    /// 当前并发流数量
    pub current: usize,
    /// 上次测量速率（bps）
    pub last_bps: u64,
}

impl AdaptiveStreams {
    /// 创建新的自适应流控制器。
    /// v0.9.1: 初始 4(原 16)——窄带中继(如 5Mbps≈625KB/s)下 16 流瓜分带宽,
    /// 单个 4MiB 块要 ~107s 才能凑齐,超过接收侧任何合理等待窗口,表现为
    /// 大文件 0 字节超时(2026-08-24 跨公网实测根因)。4 起步让首块 ~27s
    /// 完成;带宽充足时 on_probe 每 500ms +1 爬坡,千兆内网约 7s 到 32 封顶,
    /// 不同带宽的中继/局域网都能自动收敛到合适并发。
    pub fn new() -> Self {
        Self { current: 4, last_bps: 0 }
    }

    /// 获取当前并发流数量
    pub fn current(&self) -> usize {
        self.current
    }

    /// 探测回调：根据当前速率和丢包率调整并发流数
    ///
    /// 规则：
    /// - loss_ratio > 0.05 → current = max(2, current-2)
    /// - bps_now > last_bps*105/100 → current+1 (≤32)
    /// - 否则不变
    ///
    /// 返回新值
    pub fn on_probe(&mut self, bps_now: u64, loss_ratio: f64) -> usize {
        const MIN_STREAMS: usize = 2;
        const MAX_STREAMS: usize = 32;
        const GROWTH_THRESHOLD: u64 = 105; // 105%

        // 丢包率过高 → 减少流数
        if loss_ratio > 0.05 {
            self.current = self.current.saturating_sub(2).max(MIN_STREAMS);
        } else if self.last_bps == 0 || bps_now > self.last_bps * GROWTH_THRESHOLD / 100 {
            // 带宽增长 > 5% → 增加流数（不超过封顶）
            // last_bps == 0 时视为初始探测，允许增长
            self.current = self.current.saturating_add(1).min(MAX_STREAMS);
        }

        self.last_bps = bps_now;
        self.current
    }
}

impl Default for AdaptiveStreams {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapt_rules() {
        let mut a = AdaptiveStreams::default();
        // v0.9.1: 初始 4——窄带中继(5Mbps)下 16 流瓜分带宽导致单块
        // 传输时间超过旧 60s 硬超时,大文件 0 字节失败(实测根因)。
        // 4 起步 + 500ms 探测爬坡,千兆内网约 7s 到 32,窄带自然停在低位。
        assert_eq!(a.current, 4);
        a.on_probe(100, 0.0);
        assert_eq!(a.current, 5); // 增长>5% → +1
        let cur = a.on_probe(101, 0.0);
        assert_eq!(cur, 5); // 增长<5% → 保持
        a.on_probe(101, 0.10);
        assert_eq!(a.current, 3); // 丢包10% → -2

        // 模拟持续增长的带宽（每次 >5% 增长）
        // 从 200 开始，每次增长 10%，直到达到封顶
        let mut bps = 200u64;
        for _ in 0..40 {
            bps = (bps as f64 * 1.1) as u64;
            a.on_probe(bps, 0.0);
        }
        assert_eq!(a.current, 32); // 封顶
    }
}
