//! Turn outcome capture: canned replies and deliberate silence are not successes.
use super::*;

type Captured = Arc<std::sync::Mutex<Option<(String, Option<&'static str>)>>>;
tokio::task_local! {
    pub(super) static OUTCOME: Captured;
}

pub(super) fn is_channel(session: &str) -> bool {
    session.split_once(':').is_some_and(|(channel, _)|
        channel == "webchat" || duduclaw_core::SUPPORTED_CHANNEL_TYPES.iter().any(|known| *known == channel))
}

pub(super) async fn receive(ctx: &ReplyContext, agent: &str, session: &str) {
    if !agent.is_empty() && is_channel(session) {
        crate::employee_health::record_received(&ctx.home_dir, agent).await;
    }
}

pub(super) fn mark(agent: &str, reason: Option<&'static str>) {
    if agent.is_empty() { return; }
    let _ = OUTCOME.try_with(|slot| {
        if let Ok(mut value) = slot.lock() {
            *value = Some((agent.to_string(), reason));
        }
    });
}

pub(super) fn reason(reason: FailureReason) -> &'static str {
    match reason {
        FailureReason::EmptyResponse => "empty_reply",
        FailureReason::Billing | FailureReason::AccountsCoolingDownLong => "quota",
        FailureReason::RateLimited | FailureReason::AccountsCoolingDownShort => "rate_limited",
        FailureReason::AuthFailed => "auth_dead",
        FailureReason::Timeout => "timeout",
        _ => "other",
    }
}

pub(super) async fn finish(home: &Path, outcome: Option<(String, Option<&'static str>)>,
    blocked: bool, reply: &GuardedReply) {
    if let Some((agent, reason)) = outcome {
        let reason = if blocked || !reply.still_valid().await {
            Some("gate_refused")
        } else if reason.is_none() && reply.text.trim().is_empty() {
            Some("empty_reply")
        } else { reason };
        crate::employee_health::record_turn(home, &agent, reason).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn health_receive_ignores_internal_sessions() {
        let dir = tempfile::tempdir().unwrap();
        let registry = Arc::new(RwLock::new(AgentRegistry::new(dir.path().join("agents"))));
        let sessions = Arc::new(SessionManager::new(&dir.path().join("sessions.db")).unwrap());
        let statuses = Arc::new(RwLock::new(std::collections::HashMap::new()));
        let (tx, _) = tokio::sync::broadcast::channel(16);
        let ctx = ReplyContext::new(registry, dir.path().to_path_buf(), sessions, statuses, tx);
        receive(&ctx, "agnes", "goal:agnes").await;
        assert!(!dir.path().join("state/employee_health/agnes.json").exists());
        receive(&ctx, "agnes", "line:U1").await;
        assert!(crate::employee_health::state(dir.path(), "agnes").unwrap().waiting_since.is_some());
    }

    #[tokio::test]
    async fn health_capture_is_per_turn_and_commands_are_unobserved() {
        let slot = Arc::new(std::sync::Mutex::new(None));
        OUTCOME.scope(slot.clone(), async { mark("agnes", None); }).await;
        assert_eq!(*slot.lock().unwrap(), Some(("agnes".to_string(), None)));
        let next = Arc::new(std::sync::Mutex::new(None));
        OUTCOME.scope(next.clone(), async {}).await;
        assert!(next.lock().unwrap().is_none());
        assert_eq!(reason(FailureReason::AuthFailed), "auth_dead");
        assert_eq!(reason(FailureReason::Billing), "quota");
        assert_eq!(reason(FailureReason::EmptyResponse), "empty_reply");
        assert!(is_channel("line:U1#agent:agnes"));
        assert!(is_channel("webchat:user-1"));
        assert!(!is_channel("webchatevil:user-1"));
        assert!(!is_channel("lineevil:U1"));
        assert!(!is_channel("goal:agnes"));
    }
}
