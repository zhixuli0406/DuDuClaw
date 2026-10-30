//! Host-owned turn registration shared by channel and background executions.

pub(crate) fn register_current(agent: &str) {
    if let Ok(Some(turn)) = duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone) {
        if duduclaw_core::is_valid_agent_id(agent)
            && crate::computer_use_sessions::turns::valid_turn_id(&turn)
        {
            crate::egress_guard::register_turn(agent, &turn);
        }
    }
}

/// Register the actual live execution independently of requester identity.
/// A background run has no channel origin; its synthetic session is used only
/// to keep the memory provenance pair complete. Unknown client turns are
/// never registered through the internal HTTP route.
pub(crate) fn scope<F: std::future::Future>(agent: &str, future: F) -> impl std::future::Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let existing = duduclaw_memory::feedback::CURRENT_TURN_ID
            .try_with(Clone::clone).ok().flatten();
        if existing.is_some() {
            register_current(agent);
            return future.await;
        }
        let turn = format!("runtime-{}", uuid::Uuid::new_v4().simple());
        let session = duduclaw_memory::feedback::CURRENT_SESSION_ID
            .try_with(Clone::clone).ok().flatten()
            .unwrap_or_else(|| format!("runtime:{agent}:{turn}"));
        if duduclaw_core::is_valid_agent_id(agent) {
            crate::egress_guard::register_turn(agent, &turn);
        }
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session), future),
        ).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn background_kinds_and_resolved_default_alias_get_distinct_live_turns() {
        let agent = format!("resolved-{}", uuid::Uuid::new_v4().simple());
        let mut seen = std::collections::HashSet::new();
        for kind in ["cron", "dispatch", "reminder", "mail", "heartbeat", "ephemeral"] {
            scope("default", async {
                register_current(&agent);
                let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.with(Clone::clone).unwrap();
                assert!(seen.insert(turn.clone()), "{kind}");
                assert!(!crate::egress_guard::query_turn(&agent, Some(&turn)).tainted, "{kind}");
                crate::egress_guard::mark_turn(&agent, Some(&turn), "memory_search");
                register_current(&agent);
                assert!(crate::egress_guard::query_turn(&agent, Some(&turn)).tainted);
            }).await;
        }
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("invalid\nturn".into()),
            scope(&agent, async {
                assert!(crate::egress_guard::query_turn(&agent, Some("invalid\nturn")).tainted);
            }),
        ).await;
    }

    #[tokio::test]
    async fn egress_cron_run_has_registered_turn_without_requester() {
        let agent = format!("cron-{}", uuid::Uuid::new_v4().simple());
        crate::memory_provenance::DISPATCH_RUN.scope(Some((format!("cron:{agent}"), "cron-run".into())),
            scope(&agent, async {
                let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.with(Clone::clone).unwrap();
                assert!(!crate::egress_guard::query_turn(&agent, Some(&turn)).tainted);
                let pairs = crate::memory_provenance::turn_source_env_pairs();
                assert!(pairs.iter().any(|(key, value)| key == duduclaw_core::ENV_TRUST_TURN_ID && value == &turn));
                assert!(pairs.iter().any(|(key, value)| key == duduclaw_core::ENV_DISPATCH_RUN_ID && value == "cron-run"));
                assert!(matches!(crate::knowledge::turn_requesters::current_requester(), duduclaw_knowledge::requester::RequesterIdentity::Unknown));
                crate::egress_guard::mark_turn(&agent, Some(&turn), "memory_search");
                scope(&agent, async {
                    assert!(crate::egress_guard::query_turn(&agent, Some(&turn)).tainted);
                }).await;
            }),
        ).await;
        assert!(crate::egress_guard::query_turn(&agent, Some("unknown-turn")).tainted);
        assert!(duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn egress_hook_subprocess_inherits_spawn_turn() {
        scope("hook-inheritance", async {
            let turn = duduclaw_memory::feedback::CURRENT_TURN_ID.with(Clone::clone).unwrap();
            let mut command = tokio::process::Command::new("sh");
            command.env_clear().env(duduclaw_core::ENV_TRUST_TURN_ID, &turn)
                .args(["-c", "sh -c 'printf %s \"$DUDUCLAW_TURN_ID\"'"]);
            let output = command.output().await.unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), turn);
        }).await;
    }
}
