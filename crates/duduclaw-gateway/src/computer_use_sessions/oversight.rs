//! Authority for a team manager's masked view of a human-started session.

use chrono::{DateTime, Utc};
use duduclaw_knowledge::requester::{RequesterIdentity, is_public_visitor_channel};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

#[derive(Clone)]
pub(crate) struct CachedSnapshot {
    pub(crate) captured: Instant,
    pub(crate) captured_at: i64,
    pub(crate) body: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct SessionOrigin {
    user_id: Option<String>,
    started_at: DateTime<Utc>,
}

impl super::ComputerUseSessions {
    pub(crate) fn team_origin_allows(
        &self,
        agent: &str,
        expected: &str,
        scope: &crate::team_oversight::TeamOversight,
    ) -> bool {
        scope.agent == agent
            && self.live_entry(agent, Some(expected)).is_some_and(|entry| {
                entry
                    .shared
                    .origin
                    .get()
                    .is_some_and(|origin| origin.allows(scope))
            })
    }

    /// At most one capture per five seconds, shared by all viewers. A busy
    /// employee is never queued behind another viewer waiting for its lock.
    pub(crate) async fn masked_snapshot(
        &self,
        agent: &str,
        expected: &str,
    ) -> Result<Value, super::OpError> {
        let entry = self.live_entry(agent, Some(expected)).ok_or_else(|| {
            super::OpError::new(super::ErrorCode::NotFound, "這個瀏覽器工作階段已結束。")
        })?;
        self.snapshot_safety(agent, &entry).await?;
        let cached = || {
            entry
                .shared
                .snapshot
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone()
        };
        let mut snapshot = cached();
        let fresh = snapshot
            .as_ref()
            .is_some_and(|s| s.captured.elapsed() <= Duration::from_secs(5));
        if !fresh
            && entry.shared.live.frozen_since().is_none()
            && let Ok(mut session) = entry.session.try_lock()
        {
            // This check may end an expired/revoked session. Frozen sessions
            // use their previous image and are never resumed by team viewing.
            if entry.shared.live.frozen_since().is_none() {
                tokio::time::timeout(Duration::from_secs(5), async {
                    self.check_alive(&mut session).await?;
                    self.capture_masked(&mut session).await
                })
                .await
                .map_err(|_| {
                    super::OpError::new(
                        super::ErrorCode::ScreenshotFailed,
                        "畫面擷取逾時，請稍後再試。",
                    )
                })??;
            }
            snapshot = cached();
        }
        let snapshot = snapshot.ok_or_else(|| {
            super::OpError::new(
                super::ErrorCode::Unavailable,
                "員工正在操作，暫時沒有可供觀看的遮罩畫面。",
            )
        })?;
        let mut body = snapshot.body;
        #[cfg(test)]
        self.pause_action_boundary("snapshot_final_safety").await;
        self.snapshot_safety(agent, &entry).await?;
        if self.live_entry(agent, Some(expected)).is_none() {
            return Err(snapshot_error());
        }
        // No await after this decision: a hold appearing during the safety
        // read must also mask a previously clean cached frame.
        if entry.shared.live.hold().is_some() {
            let width = body["width"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(snapshot_error)?;
            let height = body["height"]
                .as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or_else(snapshot_error)?;
            body["png_base64"] = json!(
                crate::computer_use::mask_screenshot_regions(
                    body["png_base64"].as_str().ok_or_else(snapshot_error)?,
                    &[[0, 0, width, height]],
                    crate::computer_use::MaskingConfig::default().fill_color,
                )
                .map_err(|_| snapshot_error())?
            );
            body["fully_masked"] = json!(true);
            body["mask_reason"] = json!("injection_suspected");
        }
        // Only the closed screenshot shape crosses the dashboard boundary.
        Ok(json!({"ok":true,"session_id":expected,
            "png_base64":body["png_base64"],"width":body["width"],"height":body["height"],
            "fully_masked":body["fully_masked"],"mask_reason":body["mask_reason"],
            "captured_at":snapshot.captured_at,"stale":snapshot.captured.elapsed()>Duration::from_secs(5)}))
    }

    async fn snapshot_safety(
        &self,
        agent: &str,
        entry: &super::Entry,
    ) -> Result<(), super::OpError> {
        let threat = super::read_threat_level(&self.home).await;
        // All shared proofs are checked after the asynchronous read as well
        // as on entry. None waits for the employee's session mutex.
        if threat == super::ThreatLevel::Red
            || entry
                .control
                .stopped
                .load(std::sync::atomic::Ordering::Acquire)
            || super::capability_problem(&super::agent_capabilities(&self.home, agent)).is_some()
            || entry
                .shared
                .live
                .meta
                .get()
                .is_none_or(|meta| Instant::now() >= meta.deadline)
            || !entry
                .shared
                .login_owner_check
                .get()
                .and_then(Option::as_ref)
                .is_some_and(|check| check())
            || super::workspace::lease_problem(&self.home, agent, &entry.shared).is_some()
        {
            return Err(snapshot_error());
        }
        Ok(())
    }
}

fn snapshot_error() -> super::OpError {
    super::OpError::new(
        super::ErrorCode::ScreenshotFailed,
        "目前無法提供遮罩畫面，請稍後再試。",
    )
}

impl SessionOrigin {
    pub(crate) fn from_requester(requester: &RequesterIdentity, started_at: DateTime<Utc>) -> Self {
        // Only a live, identified person counts. A background job on behalf
        // of a report is not that report starting a browser in a live turn.
        let user_id = match requester {
            RequesterIdentity::DashboardUser { user_id } => Some(user_id.clone()),
            RequesterIdentity::ChannelUser {
                channel, user_id, ..
            } if !is_public_visitor_channel(channel) => user_id.clone(),
            _ => None,
        }
        .filter(|id| !id.is_empty() && id != "system");
        Self {
            user_id,
            started_at,
        }
    }

    pub(crate) fn allows(&self, scope: &crate::team_oversight::TeamOversight) -> bool {
        self.started_at >= scope.since
            && self
                .user_id
                .as_ref()
                .is_some_and(|user| scope.reports.contains_key(user))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_auth::{AccessLevel, OverseenReport};

    fn scope(since: DateTime<Utc>) -> crate::team_oversight::TeamOversight {
        crate::team_oversight::TeamOversight {
            viewer_user_id: "manager-a".into(),
            agent: "employee-a".into(),
            since,
            generation: 1,
            reports: [(
                "report-a".into(),
                OverseenReport {
                    user_id: "report-a".into(),
                    display_name: "Report A".into(),
                    depth: 1,
                    level: AccessLevel::Operator,
                },
            )]
            .into_iter()
            .collect(),
        }
    }

    #[test]
    fn team_view_requires_this_sessions_identified_report_and_current_window() {
        let now = Utc::now();
        let mut scope = scope(now - chrono::Duration::seconds(1));
        let own = SessionOrigin::from_requester(
            &RequesterIdentity::DashboardUser {
                user_id: "report-a".into(),
            },
            now,
        );
        assert!(own.allows(&scope));
        let other = SessionOrigin::from_requester(
            &RequesterIdentity::DashboardUser {
                user_id: "report-b".into(),
            },
            now,
        );
        assert!(
            !other.allows(&scope),
            "sharing an employee does not share another person's session"
        );
        scope.since = now + chrono::Duration::microseconds(1);
        assert!(
            !own.allows(&scope),
            "an off/on activation never exposes an older session"
        );
        scope.since = now - chrono::Duration::seconds(1);
        scope.reports.clear();
        assert!(
            !own.allows(&scope),
            "removed report/binding closes access immediately"
        );
    }

    #[test]
    fn public_visitors_background_jobs_and_unbound_senders_never_become_reports() {
        let now = Utc::now();
        let scope = scope(now - chrono::Duration::seconds(1));
        for requester in [
            RequesterIdentity::Unknown,
            RequesterIdentity::System {
                kind: "schedule".into(),
                on_behalf_of: Some("report-a".into()),
            },
            RequesterIdentity::ChannelUser {
                channel: "widget".into(),
                chat: "c".into(),
                sender: "s".into(),
                user_id: Some("report-a".into()),
            },
            RequesterIdentity::ChannelUser {
                channel: "odoolivechat".into(),
                chat: "c".into(),
                sender: "s".into(),
                user_id: Some("report-a".into()),
            },
            RequesterIdentity::ChannelUser {
                channel: "telegram".into(),
                chat: "c".into(),
                sender: "s".into(),
                user_id: None,
            },
            RequesterIdentity::DashboardUser {
                user_id: "system".into(),
            },
        ] {
            assert!(
                !SessionOrigin::from_requester(&requester, now).allows(&scope),
                "{requester:?}"
            );
        }
        let bound = RequesterIdentity::ChannelUser {
            channel: "telegram".into(),
            chat: "c".into(),
            sender: "s".into(),
            user_id: Some("report-a".into()),
        };
        assert!(SessionOrigin::from_requester(&bound, now).allows(&scope));
    }
}
