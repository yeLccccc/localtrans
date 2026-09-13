//! 中继会话自愈:退避状态机 + 自动续传判定(纯逻辑,双壳共用)。

use std::sync::Arc;

/// 回路 1 退避状态机:尝试失败后取下次延迟;连续 3 次失败放弃。
/// 延迟序列 1s → 2s →(翻倍,封顶 30s);MAX_FAILS=3 时第 3 次失败返回 None。
pub struct BackoffState {
    pub fails: u32,
}

const MAX_FAILS: u32 = 3;
const MAX_DELAY_SECS: u64 = 30;

impl BackoffState {
    pub fn new() -> Self {
        Self { fails: 0 }
    }

    /// 记录一次失败,返回下次重试延迟(秒)。None = 连续 3 次失败,放弃。
    pub fn record_failure(&mut self) -> Option<u64> {
        self.fails += 1;
        if self.fails >= MAX_FAILS {
            None
        } else {
            Some((1u64 << (self.fails - 1)).min(MAX_DELAY_SECS))
        }
    }

    pub fn reset(&mut self) {
        self.fails = 0;
    }
}

impl Default for BackoffState {
    fn default() -> Self {
        Self::new()
    }
}

/// 回路 2 判定:传输表里 failed 且对端匹配且 parts 残留且未自动重试过的任务。
/// spec 复审裁定:取宽规则——不区分失败原因,任何 failed 满足其余条件即重试 1 次。
pub fn auto_resumable_jobs(
    transfers: Vec<(u64, String, String)>,
    peer_hex: &str,
    pending_job_ids: &[u64],
    already_retried: &std::collections::HashSet<u64>,
) -> Vec<u64> {
    transfers
        .into_iter()
        .filter(|(id, state, peer)| {
            state == "failed"
                && peer == peer_hex
                && pending_job_ids.contains(id)
                && !already_retried.contains(id)
        })
        .map(|(id, _, _)| id)
        .collect()
}

/// 回路 1 核心:connect_peer 带退避重试,成功返回新连接,3 败返回 None。
/// e2e 与壳层(auto_reconnect)共用。
pub async fn reconnect_peer_conn(
    client: &Arc<crate::relay::client::RelayClient>,
    target_fp: [u8; 32],
) -> Option<quinn::Connection> {
    let mut backoff = BackoffState::new();
    loop {
        match client.connect_peer(target_fp).await {
            Ok(conn) => {
                tracing::info!("中继重连成功: fp={}", hex::encode(target_fp));
                return Some(conn);
            }
            Err(e) => tracing::debug!("中继重连失败(第 {} 次): {}", backoff.fails + 1, e),
        }
        match backoff.record_failure() {
            Some(secs) => tokio::time::sleep(std::time::Duration::from_secs(secs)).await,
            None => {
                tracing::info!("中继重连放弃(连续 3 次失败): fp={}", hex::encode(target_fp));
                return None;
            }
        }
    }
}

/// 回路 1 壳层入口:重连 + adopt 为发起方。true = 会话已恢复。
/// adopt 失败直接 false(会话层异常,重连不解决;等下次 SessionDown 再触发)。
pub async fn auto_reconnect(
    client: &Arc<crate::relay::client::RelayClient>,
    sm: &Arc<crate::session::SessionManager>,
    target_fp: [u8; 32],
) -> bool {
    let Some(conn) = reconnect_peer_conn(client, target_fp).await else {
        return false;
    };
    match sm.adopt_as_initiator(conn).await {
        Ok(_) => true,
        Err(e) => {
            tracing::warn!("中继自愈 adopt 失败: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_sequence_and_giveup() {
        let mut b = BackoffState::new();
        assert_eq!(b.fails, 0);
        assert_eq!(b.record_failure(), Some(1)); // 第 1 败 → 1s 后重试
        assert_eq!(b.record_failure(), Some(2)); // 第 2 败 → 2s 后重试
        assert_eq!(b.record_failure(), None);    // 第 3 败 → 放弃
    }

    #[test]
    fn backoff_reset() {
        let mut b = BackoffState::new();
        b.record_failure();
        b.record_failure();
        b.reset();
        assert_eq!(b.fails, 0);
        assert_eq!(b.record_failure(), Some(1)); // 重置后从头计
    }

    #[test]
    fn auto_resumable_filters() {
        let retried = [9u64].into_iter().collect::<std::collections::HashSet<u64>>();
        let transfers = vec![
            (1u64, "failed".into(), "aa".into()),   // 命中
            (2u64, "done".into(), "aa".into()),     // 非 failed
            (3u64, "failed".into(), "bb".into()),   // 对端不匹配
            (4u64, "failed".into(), "aa".into()),   // 无 parts 残留
            (9u64, "failed".into(), "aa".into()),   // 已自动重试过
        ];
        let pending = [1u64, 2, 3, 9];
        let jobs = auto_resumable_jobs(transfers, "aa", &pending, &retried);
        assert_eq!(jobs, vec![1]);
    }
}
