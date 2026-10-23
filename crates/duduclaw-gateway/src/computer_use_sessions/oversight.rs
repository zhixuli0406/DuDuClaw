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
        self.team_report_for(agent, expected, scope).is_some()
    }

    /// Only the actual starter is recorded in who-read notices, even if
    /// several reports happen to share an employee binding.
    pub(crate) fn team_report_for(
        &self,
        agent: &str,
        expected: &str,
        scope: &crate::team_oversight::TeamOversight,
    ) -> Option<(String, u8)> {
        if scope.agent != agent {
            return None;
        }
        let entry = self.live_entry(agent, Some(expected))?;
        let origin = entry.shared.origin.get()?;
        if !origin.allows(scope) {
            return None;
        }
        let report = scope.reports.get(origin.user_id.as_ref()?)?;
        Some((report.user_id.clone(), report.depth))
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
    /// Capture the host-proven fact at admission, before long approval or
    /// container-start waits. A durable fact is not a live presence grant.
    pub(crate) fn capture_live_turn(agent: &str, turn: Option<&str>) -> Self {
        let who = turn
            .filter(|turn| {
                super::attended_call::current_turn(agent).as_deref() == Some(*turn)
                    && super::turns::attended_live_for(agent, turn)
            })
            .map(|turn| crate::knowledge::turn_requesters::resolve(agent, Some(turn)))
            .unwrap_or(RequesterIdentity::Unknown);
        Self::from_requester(&who, Utc::now())
    }

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

    #[tokio::test]
    async fn an_unattended_caller_cannot_borrow_another_live_runtime_turn_as_its_origin() {
        let agent = "employee-a";
        let turn = format!("other-runtime-{}", uuid::Uuid::new_v4());
        let session = "webchat:other-runtime";
        let scope = scope(Utc::now() - chrono::Duration::seconds(1));
        let _requester = crate::knowledge::turn_requesters::register(agent, &turn,
            RequesterIdentity::DashboardUser { user_id: "report-a".into() }).unwrap();
        super::super::attended::verified_webchat_message(session, Some("report-a"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.into()),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn.clone()), async {
                    assert!(super::super::attended::bind_final_route(session, session, agent, &turn));
                    let _live = super::super::turns::register_current_turn(agent).unwrap();
                    assert!(super::super::turns::attended_live_for(agent, &turn));
                    super::super::attended_call::request_scope(None, async {
                        assert!(!SessionOrigin::capture_live_turn(agent, Some(&turn)).allows(&scope),
                            "the calling runtime has no attended proof of its own");
                    }).await;
                })),
        ).await;
    }

    #[tokio::test]
    async fn requester_identity_alone_cannot_create_a_human_session_origin() {
        let agent = "employee-a";
        let turn = format!("origin-{}", uuid::Uuid::new_v4());
        let session = "webchat:origin-fixture";
        let scope = scope(Utc::now() - chrono::Duration::seconds(1));
        let _requester = crate::knowledge::turn_requesters::register(
            agent,
            &turn,
            RequesterIdentity::DashboardUser {
                user_id: "report-a".into(),
            },
        )
        .unwrap();
        let untrusted = super::super::turns::register(agent, &turn, session).unwrap();
        assert!(!SessionOrigin::capture_live_turn(agent, Some(&turn)).allows(&scope));
        drop(untrusted);
        let home = tempfile::tempdir().unwrap();
        let origin = duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
            Some(turn.clone()), super::super::attended::verified_webchat_message(
            session,
            Some("report-a"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.into()), async {
                assert!(super::super::attended::bind_final_route(
                    session, session, agent, &turn
                ));
                let _live = super::super::turns::register(agent, &turn, session).unwrap();
                let runtime = crate::on_behalf_of::context::register(home.path(), agent).unwrap();
                let call = crate::on_behalf_of::context::attended_call(home.path(), agent, runtime.reference()).unwrap();
                super::super::attended_call::request_scope(Some(call), async {
                    SessionOrigin::capture_live_turn(agent, Some(&turn))
                }).await
            }),
        ))
        .await;
        assert!(
            origin.allows(&scope),
            "the recorded fact survives the model turn ending"
        );
        assert!(!SessionOrigin::capture_live_turn(agent, Some(&turn)).allows(&scope));
    }

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
