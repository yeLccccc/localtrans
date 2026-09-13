//! TransferCard 单卡片状态机（纯逻辑，无 IO）
//!
//! 壳层 transfers 表（Task 4 切换）的核心件：每张卡片一个 TransferCard，
//! 所有状态变化经 `apply` 单写者入口，迁移表外的非法事件直接拒绝（返回 false），
//! 终态吸收一切事件——杜绝"幽灵行复活"。

use crate::TransferDto;

/// 卡片状态（对应 dto.state 的字符串形态）
#[derive(Debug, Clone, PartialEq)]
pub enum CardState {
    Pending,
    Active,
    Paused,
    Cancelling,
    Done,
    Failed,
    Interrupted,
}

impl CardState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CardState::Pending => "pending",
            CardState::Active => "active",
            CardState::Paused => "paused",
            CardState::Cancelling => "cancelling",
            CardState::Done => "done",
            CardState::Failed => "failed",
            CardState::Interrupted => "interrupted",
        }
    }
}

/// 从 dto.state 字符串还原状态
pub fn state_from_str(s: &str) -> Option<CardState> {
    match s {
        "pending" => Some(CardState::Pending),
        "active" => Some(CardState::Active),
        "paused" => Some(CardState::Paused),
        "cancelling" => Some(CardState::Cancelling),
        "done" => Some(CardState::Done),
        "failed" => Some(CardState::Failed),
        "interrupted" => Some(CardState::Interrupted),
        _ => None,
    }
}

/// 卡片事件（引擎回执 / 用户操作 / 看门狗）
#[derive(Debug, Clone)]
pub enum CardEvent {
    /// 引擎/对端开始传输
    Started,
    Progress {
        done: u64,
        total: u64,
        speed_bps: u64,
        remote_done: u64,
        health: Option<crate::HealthDto>,
    },
    Paused,
    Resumed,
    Failed {
        reason: Option<String>,
    },
    /// done
    Finished,
    Interrupted,
    /// 用户删除/取消活动任务
    CancelRequested,
    /// 引擎回执（或 5s 超时兜底）
    CancelConfirmed,
    /// pending 60s 未启动
    QueueTimeout,
}

/// 单张传输卡片：dto + 引擎关联 + 删除跟踪
#[derive(Debug, Clone)]
pub struct TransferCard {
    pub dto: TransferDto,
    /// 引擎真实 job_id(None=尚未拿到/无引擎任务;parts 目录名用它)
    pub engine_id: Option<u64>,
    /// 视图移除(两级删除第一级):true 时不出现在活动列表,历史记录可见
    pub removed: bool,
    /// 删除级别跟踪:CancelRequested 后进入 cancelling,等待确认
    pub cancelling_since_ms: Option<i64>,
    /// 最近一次成功迁移 (from, to)，供 history 追加
    last_transition: Option<(String, String)>,
}

impl TransferCard {
    pub fn new(dto: TransferDto) -> Self {
        TransferCard {
            dto,
            engine_id: None,
            removed: false,
            cancelling_since_ms: None,
            last_transition: None,
        }
    }

    /// 单写者入口:应用事件,返回是否接受。非法迁移返回 false(调用方 warn+丢弃)。
    pub fn apply(&mut self, ev: CardEvent, now_ms: i64) -> bool {
        let from = match state_from_str(&self.dto.state) {
            Some(s) => s,
            None => return false,
        };
        // 终态吸收:任何事件不改变状态
        if matches!(
            from,
            CardState::Done | CardState::Failed | CardState::Interrupted
        ) {
            return false;
        }

        let to: CardState = match (&from, ev) {
            // ---- Pending ----
            (CardState::Pending, CardEvent::Started) => {
                self.dto.started_at_ms = Some(now_ms);
                CardState::Active
            }
            (CardState::Pending, CardEvent::QueueTimeout) => {
                self.dto.fail_reason = Some("排队超时".into());
                CardState::Failed
            }
            (CardState::Pending, CardEvent::Failed { reason }) => {
                self.record_fail(reason, now_ms);
                CardState::Failed
            }
            (CardState::Pending, CardEvent::CancelRequested) => {
                self.cancelling_since_ms = Some(now_ms);
                CardState::Cancelling
            }
            // ---- Active ----
            (CardState::Active, CardEvent::Progress { done, total, speed_bps, remote_done, health }) => {
                self.dto.done = done;
                self.dto.total = total;
                self.dto.speed_bps = speed_bps;
                self.dto.remote_done = remote_done;
                if let Some(h) = health {
                    self.dto.health = Some(h);
                }
                return true; // 自环:不记录迁移、不改时间戳
            }
            (CardState::Active, CardEvent::Paused) => {
                self.dto.speed_bps = 0;
                CardState::Paused
            }
            (CardState::Active, CardEvent::Failed { reason }) => {
                self.record_fail(reason, now_ms);
                CardState::Failed
            }
            (CardState::Active, CardEvent::Finished) => {
                self.dto.done = self.dto.total; // 兜底
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Done
            }
            (CardState::Active, CardEvent::Interrupted) => {
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Interrupted
            }
            (CardState::Active, CardEvent::CancelRequested) => {
                // 不立即改 state?——表要求迁到 Cancelling 但等待确认;
                // brief 表 from=Active|CancelRequested → to=Cancelling
                self.cancelling_since_ms = Some(now_ms);
                CardState::Cancelling
            }
            // ---- Paused ----
            (CardState::Paused, CardEvent::Resumed) => CardState::Active,
            (CardState::Paused, CardEvent::Failed { reason }) => {
                self.record_fail(reason, now_ms);
                CardState::Failed
            }
            (CardState::Paused, CardEvent::Interrupted) => {
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Interrupted
            }
            (CardState::Paused, CardEvent::Finished) => {
                self.dto.done = self.dto.total;
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Done
            }
            (CardState::Paused, CardEvent::CancelRequested) => {
                self.cancelling_since_ms = Some(now_ms);
                CardState::Cancelling
            }
            // ---- Cancelling ----
            (CardState::Cancelling, CardEvent::CancelConfirmed) => {
                // 返回 true 且保持 cancelling;删除动作在命令层
                return true;
            }
            (CardState::Cancelling, CardEvent::Failed { reason }) => {
                self.cancelling_since_ms = None;
                self.record_fail(reason, now_ms);
                CardState::Failed
            }
            (CardState::Cancelling, CardEvent::Interrupted) => {
                self.cancelling_since_ms = None;
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Interrupted
            }
            (CardState::Cancelling, CardEvent::Finished) => {
                self.cancelling_since_ms = None;
                self.dto.done = self.dto.total;
                self.dto.speed_bps = 0;
                self.dto.finished_at_ms = Some(now_ms);
                CardState::Done
            }
            // 表外一律拒绝
            _ => return false,
        };

        let from_s = from.as_str().to_string();
        let to_s = to.as_str().to_string();
        self.dto.state = to_s.clone();
        self.last_transition = Some((from_s, to_s));
        true
    }

    fn record_fail(&mut self, reason: Option<String>, now_ms: i64) {
        if let Some(r) = reason {
            self.dto.fail_reason = Some(r);
        }
        self.dto.speed_bps = 0;
        self.dto.finished_at_ms = Some(now_ms);
    }

    /// 状态迁移成功后返回 (from_state, to_state) 供 history 追加——由 apply 内部记录
    pub fn last_transition(&self) -> Option<(String, String)> {
        self.last_transition.clone()
    }

    /// 取走最近一次迁移记录（读取并清零，防重复消费）
    pub fn take_last_transition(&mut self) -> Option<(String, String)> {
        self.last_transition.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TransferDto;

    fn dto(state: &str) -> TransferDto {
        TransferDto {
            job_id: 1, name: "t.bin".into(), total: 100, done: 0,
            state: state.into(), speed_bps: 0, peer: "aa".repeat(32),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: None, remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
        }
    }
    const NOW: i64 = 1_770_000_000_000;

    #[test]
    fn pending_to_active_to_done_happy_path() {
        let mut c = TransferCard::new(dto("pending"));
        assert!(c.apply(CardEvent::Started, NOW));
        assert_eq!(c.dto.state, "active");
        assert_eq!(c.dto.started_at_ms, Some(NOW));
        assert!(c.apply(CardEvent::Progress { done: 40, total: 100, speed_bps: 10, remote_done: 0, health: None }, NOW));
        assert_eq!(c.dto.done, 40);
        assert!(c.apply(CardEvent::Finished, NOW + 5));
        assert_eq!(c.dto.state, "done");
        assert_eq!(c.dto.finished_at_ms, Some(NOW + 5));
    }

    #[test]
    fn terminal_absorbs_everything_no_revive() {
        let mut c = TransferCard::new(dto("failed"));
        assert!(!c.apply(CardEvent::Started, NOW));
        assert!(!c.apply(CardEvent::Progress { done: 1, total: 1, speed_bps: 1, remote_done: 0, health: None }, NOW));
        assert_eq!(c.dto.state, "failed", "终态不复活");
    }

    #[test]
    fn active_cancel_keeps_state_until_confirmed() {
        let mut c = TransferCard::new(dto("active"));
        assert!(c.apply(CardEvent::CancelRequested, NOW));
        assert_eq!(c.dto.state, "cancelling");
        assert!(c.cancelling_since_ms.is_some());
        assert!(c.apply(CardEvent::CancelConfirmed, NOW));
        // 确认后由命令层负责移除;状态保持 cancelling
        assert_eq!(c.dto.state, "cancelling");
    }

    #[test]
    fn engine_terminal_also_clears_cancelling() {
        let mut c = TransferCard::new(dto("active"));
        c.apply(CardEvent::CancelRequested, NOW);
        assert!(c.apply(CardEvent::Interrupted, NOW));
        assert_eq!(c.dto.state, "interrupted");
        assert!(c.cancelling_since_ms.is_none());
    }

    #[test]
    fn paused_resume_rejected_from_pending() {
        let mut c = TransferCard::new(dto("pending"));
        assert!(!c.apply(CardEvent::Resumed, NOW), "pending 不能 resume");
        let mut p = TransferCard::new(dto("paused"));
        assert!(p.apply(CardEvent::Resumed, NOW));
        assert_eq!(p.dto.state, "active");
    }

    #[test]
    fn queue_timeout_fails_pending() {
        let mut c = TransferCard::new(dto("pending"));
        c.dto.queue_pos = Some(2);
        // 修复轮 1:超时路径先清 queue_pos 残留(card_mutate),再 QueueTimeout 落 failed
        c.dto.queue_pos = None;
        assert!(c.apply(CardEvent::QueueTimeout, NOW));
        assert_eq!(c.dto.state, "failed");
        assert_eq!(c.dto.fail_reason.as_deref(), Some("排队超时"));
        assert_eq!(c.dto.queue_pos, None, "超时后 queue_pos 不残留");
    }

    #[test]
    fn failed_records_reason_and_freezes_time() {
        let mut c = TransferCard::new(dto("active"));
        assert!(c.apply(CardEvent::Failed { reason: Some("对端拒绝".into()) }, NOW));
        assert_eq!(c.dto.fail_reason.as_deref(), Some("对端拒绝"));
        assert_eq!(c.dto.finished_at_ms, Some(NOW));
    }

    /// M6 实测回归锚：秒传卡以 pending 落地后直收 Finished 会被状态机拒绝
    /// （卡片永远卡"等待中"）——调用方必须先 Started 再 Finished
    /// （main.rs InstantHit 泵与 recheck_parent_terminal 同款约定）。
    #[test]
    fn instant_hit_pending_requires_started_before_finished() {
        // 错误序：pending 直收 Finished → 拒绝，状态不变
        let mut c = TransferCard::new(dto("pending"));
        c.dto.done = c.dto.total;
        assert!(!c.apply(CardEvent::Finished, NOW), "pending+Finished 是非法迁移");
        assert_eq!(c.dto.state, "pending");
        // 正确序：Started 激活后 Finished 收敛 done
        let mut c2 = TransferCard::new(dto("pending"));
        assert!(c2.apply(CardEvent::Started, NOW));
        assert!(c2.apply(CardEvent::Finished, NOW + 1));
        assert_eq!(c2.dto.state, "done");
        assert_eq!(c2.dto.done, c2.dto.total, "Finished 兜底 done=total");
    }

    #[test]
    fn last_transition_reports_edge() {
        let mut c = TransferCard::new(dto("pending"));
        c.apply(CardEvent::Started, NOW);
        assert_eq!(c.last_transition().unwrap(), ("pending".to_string(), "active".to_string()));
    }

    #[test]
    fn state_str_roundtrip() {
        for s in ["pending","active","paused","cancelling","done","failed","interrupted"] {
            assert_eq!(state_from_str(s).unwrap().as_str(), s);
        }
        assert!(state_from_str("weird").is_none());
    }

    #[test]
    fn progress_updates_remote_done_for_sender() {
        let mut c = TransferCard::new(dto("active"));
        c.dto.local_role = "source-push".into();
        assert!(c.apply(CardEvent::Progress { done: 80, total: 100, speed_bps: 5, remote_done: 60, health: None }, NOW));
        assert_eq!(c.dto.remote_done, 60);
    }

    #[test]
    fn progress_rejected_when_paused() {
        let mut c = TransferCard::new(dto("paused"));
        assert!(!c.apply(CardEvent::Progress { done: 1, total: 100, speed_bps: 1, remote_done: 0, health: None }, NOW));
        assert_eq!(c.dto.state, "paused");
    }
}
