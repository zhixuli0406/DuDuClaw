//! C8 — responsibility notifications (design §7).
//!
//! Every notice is written to the Activity Feed first, so the dashboard always
//! shows it. A push additionally needs every gate to pass, in this order, and
//! any refusal stops it (fail closed):
//! 1. the responsibility's `notification_policy_json` is enabled and lists
//!    this kind (`result` / `needs_decision` / `paused`) — opt-in, off by default;
//! 2. the employee's `agent.toml [proactive] enabled` (default off);
//! 3. a durable per-window cap counted in `responsibility_notice_log`, a
//!    table only the gateway writes (S-M6: Activity rows can be posted by
//!    employees, so they never count), because the proactive gate's hourly
//!    limit lives in memory and resets on restart; every notice has a key
//!    and is pushed at most once;
//! 4. `ProactiveGate` scoring — one utility-model call, run inside the
//!    occurrence's cost attribution scope so it is charged to that occurrence.
//!
//! Then quiet hours / delivery go through `goal_notify::notify_agent_plain`
//! (L2 Confirm: deferred out of quiet hours, never escalated to L3).
//! A push carries only the responsibility name, its state and a dashboard
//! link — never a result summary or event content (D13), and the scoring
//! prompt sees the same server-built text.

use std::path::Path;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tracing::warn;

use super::ResponsibilityConfig;
use crate::proactive_gate::{GateDecision, ProactiveConfig, ProactiveGate};
use crate::task_store::{ActivityRow, ResponsibilityRow, TaskStore, period_key, resp_ts};

pub const NOTICE: &str = "responsibility.notice";
pub const NOTIFIED: &str = "responsibility.notified";

/// What happened. The kind decides which policy flag it needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeEvent {
    /// An occurrence finished (`done` / `failed` / `cancelled` / `stopped`).
    Result { task_id: String, outcome: String },
    /// The employee asked the operator to decide something (`decision_id`
    /// is the decision subscription; the question text is never pushed).
    NeedsDecision {
        task_id: String,
        decision_id: String,
    },
    /// The responsibility paused itself or ended: `budget_paused`,
    /// `failure_paused` or `expired`.
    Paused { state: String },
    /// A stop ended with an external action whose result is unknown.
    StopUncertain { root_task_id: String },
}

impl NoticeEvent {
    pub fn policy_kind(&self) -> &'static str {
        match self {
            Self::Result { .. } => "result",
            Self::NeedsDecision { .. } => "needs_decision",
            Self::Paused { .. } | Self::StopUncertain { .. } => "paused",
        }
    }

    fn task_id(&self) -> Option<&str> {
        match self {
            Self::Result { task_id, .. } | Self::NeedsDecision { task_id, .. } => Some(task_id),
            Self::StopUncertain { root_task_id } => Some(root_task_id),
            Self::Paused { .. } => None,
        }
    }

    /// One key per real-world notice: the same event is pushed at most once.
    fn notice_key(&self, resp: &ResponsibilityRow) -> String {
        match self {
            Self::Result { task_id, .. } => format!("result:{task_id}"),
            Self::NeedsDecision { decision_id, .. } => format!("decision:{decision_id}"),
            Self::Paused { state } => format!(
                "paused:{state}:{}",
                resp.state_changed_at.as_deref().unwrap_or("")
            ),
            Self::StopUncertain { root_task_id } => format!("stop_uncertain:{root_task_id}"),
        }
    }

    fn state_text(&self) -> String {
        match self {
            Self::Result { outcome, .. } => match outcome.as_str() {
                "done" => "這一次的工作已完成".into(),
                "stopped" | "stopped_counted" => "這一次的工作已被停止".into(),
                _ => "這一次的工作沒有完成".into(),
            },
            Self::NeedsDecision { .. } => "需要你做一個決定".into(),
            Self::Paused { state } => match state.as_str() {
                "budget_paused" => "本期額度已用完，暫停到下一期".into(),
                "failure_paused" => "連續失敗，已暫停，等你處理".into(),
                _ => "已到停止時間，不會再執行".into(),
            },
            Self::StopUncertain { .. } => "已停止，但有外部動作結果不明，需要你確認".into(),
        }
    }
}

/// Why a push did not happen (the notice itself is always recorded).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoticeOutcome {
    Pushed,
    Suppressed(&'static str),
    /// Push attempted; destination missing, send failed, or deferred.
    NotDelivered(&'static str),
}

/// The proactive scoring gate (production: `ProactiveGate::evaluate`).
#[async_trait]
pub trait NoticeScorer: Send + Sync {
    async fn decide(
        &self,
        home: &Path,
        agent: &str,
        cfg: &ProactiveConfig,
        text: &str,
    ) -> GateDecision;
}

#[async_trait]
impl NoticeScorer for ProactiveGate {
    async fn decide(
        &self,
        home: &Path,
        agent: &str,
        cfg: &ProactiveConfig,
        text: &str,
    ) -> GateDecision {
        let dir = home.join("agents").join(agent);
        self.evaluate(agent, Some(&dir), cfg, NOTICE, text, &[])
            .await
            .decision
    }
}

/// Delivery (production: `goal_notify::notify_agent_plain`, L2).
#[async_trait]
pub trait NoticeSender: Send + Sync {
    async fn send(&self, home: &Path, agent: &str, text: &str)
    -> crate::goal_notify::NotifyOutcome;
}

pub struct ChannelSender;

#[async_trait]
impl NoticeSender for ChannelSender {
    async fn send(
        &self,
        home: &Path,
        agent: &str,
        text: &str,
    ) -> crate::goal_notify::NotifyOutcome {
        crate::goal_notify::notify_agent_plain(
            home,
            agent,
            crate::notify_governance::NotifyLevel::Confirm,
            NOTICE,
            text,
        )
        .await
    }
}

pub struct Notifier<'a> {
    pub home: &'a Path,
    pub store: &'a TaskStore,
    pub scorer: &'a dyn NoticeScorer,
    pub sender: &'a dyn NoticeSender,
}

fn policy_allows(resp: &ResponsibilityRow, kind: &str) -> bool {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&resp.notification_policy_json) else {
        return false;
    };
    v.get("enabled").and_then(|e| e.as_bool()) == Some(true)
        && v.get("on")
            .and_then(|o| o.as_array())
            .is_some_and(|on| on.iter().any(|k| k.as_str() == Some(kind)))
}

async fn record(
    store: &TaskStore,
    kind: &str,
    resp: &ResponsibilityRow,
    task: Option<&str>,
    summary: &str,
    meta: serde_json::Value,
    now: DateTime<Utc>,
) {
    let _ = store
        .append_activity(&ActivityRow {
            id: uuid::Uuid::new_v4().to_string(),
            event_type: kind.into(),
            agent_id: resp.owner_agent_id.clone(),
            task_id: task.map(str::to_string),
            summary: summary.into(),
            timestamp: resp_ts(now),
            metadata: Some(meta.to_string()),
        })
        .await;
}

impl Notifier<'_> {
    /// Record a notice and push it if every gate passes. The scoring call is
    /// charged to the event's occurrence (else the latest one).
    pub async fn notify(
        &self,
        resp: &ResponsibilityRow,
        event: &NoticeEvent,
        now: DateTime<Utc>,
    ) -> NoticeOutcome {
        let name = duduclaw_core::truncate_chars(&resp.objective, 40);
        let link = event.task_id().and_then(|t| {
            crate::deep_link::deep_link(self.home, crate::deep_link::DeepLinkKind::Task, t)
        });
        let mut text = format!("持續任務「{name}」：{}", event.state_text());
        if let Some(l) = &link {
            text.push_str(&format!("\n{l}"));
        }
        // A window that cannot be computed counts as its own window, never
        // as "no cap" (the period cap still applies to it).
        let window = period_key(&resp.budget_period, &resp.budget_timezone, now)
            .unwrap_or_else(|_| "unknown_window".to_string());
        let key = event.notice_key(resp);
        match self
            .store
            .claim_notice(&resp.responsibility_id, &key, &window, now)
            .await
        {
            Ok(true) => {}
            Ok(false) => return NoticeOutcome::Suppressed("already_handled"),
            Err(e) => {
                warn!(error = %e, "notice log unwritable - not pushing");
                return NoticeOutcome::Suppressed("log_unwritable");
            }
        }
        let meta = serde_json::json!({
            "responsibility_id": resp.responsibility_id,
            "kind": event.policy_kind(),
            "period_key": window,
        });
        record(
            self.store,
            NOTICE,
            resp,
            event.task_id(),
            &text,
            meta.clone(),
            now,
        )
        .await;
        let outcome = self
            .gate_and_send(resp, event, &text, &window, meta, now)
            .await;
        let stored = match &outcome {
            NoticeOutcome::Pushed => "pushed".to_string(),
            NoticeOutcome::NotDelivered("deferred_quiet_hours") => "deferred".to_string(),
            NoticeOutcome::NotDelivered(r) => format!("not_delivered:{r}"),
            NoticeOutcome::Suppressed(r) => format!("suppressed:{r}"),
        };
        if let Err(e) = self
            .store
            .set_notice_outcome(&resp.responsibility_id, &key, &stored)
            .await
        {
            warn!(error = %e, "could not record the notice outcome");
        }
        outcome
    }

    async fn gate_and_send(
        &self,
        resp: &ResponsibilityRow,
        event: &NoticeEvent,
        text: &str,
        window: &str,
        meta: serde_json::Value,
        now: DateTime<Utc>,
    ) -> NoticeOutcome {
        if !policy_allows(resp, event.policy_kind()) {
            return NoticeOutcome::Suppressed("policy_off");
        }
        let agent_dir = self.home.join("agents").join(&resp.owner_agent_id);
        let cfg = crate::proactive_gate::read_proactive_config(&agent_dir);
        if !cfg.enabled {
            return NoticeOutcome::Suppressed("proactive_disabled");
        }
        let cap = ResponsibilityConfig::from_home(self.home).max_notifications_per_period;
        match self
            .store
            .pushed_notices_in_window(&resp.responsibility_id, window)
            .await
        {
            Ok(n) if n < cap => {}
            Ok(_) => return NoticeOutcome::Suppressed("period_cap"),
            Err(_) => return NoticeOutcome::Suppressed("cap_unreadable"),
        }
        let episode = match event.task_id() {
            Some(t) => Some(t.to_string()),
            None => self
                .store
                .last_occurrence_task(&resp.responsibility_id)
                .await
                .ok()
                .flatten(),
        };
        let decide = self
            .scorer
            .decide(self.home, &resp.owner_agent_id, &cfg, text);
        let decision = match episode {
            Some(episode_id) => {
                crate::runtime::GOAL_ROUND_ATTRIBUTION
                    .scope(
                        crate::runtime::GoalRoundAttribution {
                            episode_id,
                            round: None,
                        },
                        decide,
                    )
                    .await
            }
            None => decide.await,
        };
        if let GateDecision::Suppress { reason } = decision {
            return NoticeOutcome::Suppressed(reason);
        }
        use crate::goal_notify::NotifyOutcome as N;
        match self
            .sender
            .send(self.home, &resp.owner_agent_id, text)
            .await
        {
            N::Sent => {
                record(self.store, NOTIFIED, resp, event.task_id(), text, meta, now).await;
                NoticeOutcome::Pushed
            }
            N::Deferred => {
                record(self.store, NOTIFIED, resp, event.task_id(), text, meta, now).await;
                NoticeOutcome::NotDelivered("deferred_quiet_hours")
            }
            N::NoTarget => NoticeOutcome::NotDelivered("no_target"),
            N::SendFailed => NoticeOutcome::NotDelivered("send_failed"),
        }
    }
}
