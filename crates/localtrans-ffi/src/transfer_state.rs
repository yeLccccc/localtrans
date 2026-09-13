//! TransferCard 单卡片状态机（纯逻辑,无 IO）
//!
//! M2 T2:等价移植桌面壳 src-tauri/src/transfer_state.rs 的语义——
//! 每张传输卡一个 TransferCard,所有状态变化经 `apply` 单写者入口,
//! 迁移表外的非法事件直接拒绝(返回 false),终态吸收一切事件——
//! 杜绝"幽灵行复活"。
//!
//! 与 PC 版的差异(有意,均为 DTO 形态适配,迁移表逐边一致):
//! - ffi TransferDto.fail_reason 是 String(非 Option),record_fail 空原因不覆盖;
//! - Progress.health 保持 Option<HealthDto>(ffi T1 契约已有 HealthDto,与 PC 同构);
//! - PC 版 last_transition 供 parts/history 追加(壳层消费),ffi 侧字段同构保留。

use crate::dto::{HealthDto, TransferDto};

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

/// 终态判定(done/failed/interrupted)——终态吸收一切事件
pub fn is_terminal(state: &str) -> bool {
    matches!(state, "done" | "failed" | "interrupted")
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
        health: Option<HealthDto>,
    },
    Paused,
    Resumed,
    Failed {
        reason: Option<String>,
    },
    /// done
    Finished,
    // PC 同构保留(迁移表已定义;ffi 壳层由引擎终态事件确认取消)
    #[allow(dead_code)]
    Interrupted,
    /// 用户删除/取消活动任务
    CancelRequested,
    // PC 同构保留(迁移表已定义;ffi 取消确认走引擎终态/5s 看门狗)
    #[allow(dead_code)]
    CancelConfirmed,
    /// pending 60s 未启动
    QueueTimeout,
}

/// 单张传输卡片：dto + 引擎关联 + 删除跟踪
#[derive(Debug, Clone)]
pub struct TransferCard {
    pub dto: TransferDto,
    /// 引擎真实 job_id(None=尚未拿到/无引擎任务;ffi 侧 job_id 即卡键,此字段
    /// 为 PC 同构保留,当前恒等于 dto.job_id 或 None)
    #[allow(dead_code)]
    pub engine_id: Option<u64>,
    /// 视图移除(两级删除第一级):true 时不出现在活动列表,历史记录可见
    pub removed: bool,
    /// 删除级别跟踪:CancelRequested 后进入 cancelling,等待确认
    pub cancelling_since_ms: Option<i64>,
    /// 最近一次成功迁移 (from, to)
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
        if is_terminal(from.as_str()) {
            return false;
        }

        let to: CardState = match (&from, ev) {
            // ---- Pending ----
            (CardState::Pending, CardEvent::Started) => {
                self.dto.started_at_ms = Some(now_ms);
                CardState::Active
            }
            (CardState::Pending, CardEvent::QueueTimeout) => {
                self.dto.fail_reason = "排队超时".into();
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
                // 迁到 Cancelling 等待确认;确认/看门狗由命令层收尾
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
        // ffi 形态差异:fail_reason 是 String,空原因不覆盖旧值
        if let Some(r) = reason {
            self.dto.fail_reason = r;
        }
        self.dto.speed_bps = 0;
        self.dto.finished_at_ms = Some(now_ms);
    }

    /// 状态迁移成功后返回 (from_state, to_state)
    // PC 同构保留(PC 用于 parts/history 追加,M3 接入;ffi 单测消费)
    #[allow(dead_code)]
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

    fn dto(state: &str) -> TransferDto {
        TransferDto {
            job_id: 1, name: "t.bin".into(), total: 100, done: 0,
            state: state.into(), speed_bps: 0, peer: "aa".repeat(32),
            direction: "pull".into(), local_role: "destination".into(),
            health: None, started_at_ms: None, finished_at_ms: None,
            source_path: None, fail_reason: String::new(), remote_done: 0, instant: false,
            queue_pos: None, batch_id: None, children: vec![], parts_id: None,
            ..Default::default()
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
        // 超时路径先清 queue_pos 残留(card_mutate),再 QueueTimeout 落 failed
        c.dto.queue_pos = None;
        assert!(c.apply(CardEvent::QueueTimeout, NOW));
        assert_eq!(c.dto.state, "failed");
        assert_eq!(c.dto.fail_reason, "排队超时");
        assert_eq!(c.dto.queue_pos, None, "超时后 queue_pos 不残留");
    }

    #[test]
    fn failed_records_reason_and_freezes_time() {
        let mut c = TransferCard::new(dto("active"));
        assert!(c.apply(CardEvent::Failed { reason: Some("对端拒绝".into()) }, NOW));
        assert_eq!(c.dto.fail_reason, "对端拒绝");
        assert_eq!(c.dto.finished_at_ms, Some(NOW));
    }

    /// M6 实测回归锚：秒传卡以 pending 落地后直收 Finished 会被状态机拒绝
    /// （卡片永远卡"等待中"）——调用方必须先 Started 再 Finished
    /// （PC main.rs InstantHit 泵与 recheck_parent_terminal 同款约定）。
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

    // ===== ffi 补充:迁移表剩余边(PC 测试集未逐一覆盖的边) =====

    #[test]
    fn pending_cancelling_then_cancel_confirmed_stays() {
        // pending → cancelling(仲裁入口);CancelConfirmed 吸收但保持 cancelling
        let mut c = TransferCard::new(dto("pending"));
        assert!(c.apply(CardEvent::CancelRequested, NOW));
        assert_eq!(c.dto.state, "cancelling");
        assert!(c.cancelling_since_ms.is_some());
        assert!(c.apply(CardEvent::CancelConfirmed, NOW + 1));
        assert_eq!(c.dto.state, "cancelling");
        // cancelling 态再收 Started/Resumed 等活动事件 → 拒绝
        assert!(!c.apply(CardEvent::Started, NOW + 2));
        assert!(!c.apply(CardEvent::Resumed, NOW + 2));
    }

    #[test]
    fn paused_cancel_enters_cancelling_and_paused_rejects_edges() {
        let mut c = TransferCard::new(dto("paused"));
        assert!(c.apply(CardEvent::CancelRequested, NOW));
        assert_eq!(c.dto.state, "cancelling");
        let mut p = TransferCard::new(dto("paused"));
        // paused 的非法边:Started/Progress/Paused/QueueTimeout/CancelConfirmed
        assert!(!p.apply(CardEvent::Started, NOW), "paused+Started 非法");
        assert!(!p.apply(CardEvent::QueueTimeout, NOW), "paused+QueueTimeout 非法");
        assert!(!p.apply(CardEvent::CancelConfirmed, NOW), "非 cancelling 态收确认非法");
        assert_eq!(p.dto.state, "paused");
    }

    #[test]
    fn paused_finished_and_interrupted_edges() {
        let mut f = TransferCard::new(dto("paused"));
        assert!(f.apply(CardEvent::Finished, NOW));
        assert_eq!(f.dto.state, "done");
        assert_eq!(f.dto.done, f.dto.total);
        let mut i = TransferCard::new(dto("paused"));
        assert!(i.apply(CardEvent::Interrupted, NOW));
        assert_eq!(i.dto.state, "interrupted");
        assert_eq!(i.dto.speed_bps, 0);
        assert_eq!(i.dto.finished_at_ms, Some(NOW));
    }

    #[test]
    fn cancelling_finished_failed_edges_clear_arbitration() {
        // cancelling 收 Failed → failed + 清仲裁戳
        let mut c = TransferCard::new(dto("cancelling"));
        assert!(c.apply(CardEvent::Failed { reason: Some("已取消".into()) }, NOW));
        assert_eq!(c.dto.state, "failed");
        assert!(c.cancelling_since_ms.is_none());
        // cancelling 收 Finished → done + 清仲裁戳
        let mut d = TransferCard::new(dto("cancelling"));
        assert!(d.apply(CardEvent::Finished, NOW));
        assert_eq!(d.dto.state, "done");
        assert!(d.cancelling_since_ms.is_none());
    }

    #[test]
    fn queue_timeout_rejected_when_not_pending() {
        let mut c = TransferCard::new(dto("active"));
        assert!(!c.apply(CardEvent::QueueTimeout, NOW), "QueueTimeout 只对 pending 合法");
        assert_eq!(c.dto.state, "active");
    }

    #[test]
    fn progress_self_loop_keeps_state_and_no_transition_record() {
        let mut c = TransferCard::new(dto("pending"));
        c.apply(CardEvent::Started, NOW);
        // Started 迁移记录先取走
        assert_eq!(c.take_last_transition().unwrap(), ("pending".to_string(), "active".to_string()));
        assert!(c.apply(CardEvent::Progress { done: 10, total: 100, speed_bps: 1, remote_done: 0, health: None }, NOW));
        assert!(c.last_transition().is_none(), "Progress 自环不记录迁移");
        assert_eq!(c.dto.state, "active");
        // health 透传(ffi 同构 Option<HealthDto>)
        c.apply(CardEvent::Progress { done: 20, total: 100, speed_bps: 2, remote_done: 5, health: Some(HealthDto::default()) }, NOW);
        assert!(c.dto.health.is_some());
    }
}
