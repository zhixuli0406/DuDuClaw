//! Host-owned turn registration shared by channel and background executions.

tokio::task_local! {
    /// Minted inside this process, never populated from an upstream message.
    static EXECUTION_TURN: String;
}

pub(crate) fn current() -> Option<String> {
    EXECUTION_TURN.try_with(Clone::clone).ok()
}

pub(crate) fn register_current(agent: &str) {
    if let Some(turn) = current() {
        if duduclaw_core::is_valid_agent_id(agent) {
            crate::egress_guard::register_turn(agent, &turn);
        }
    }
}

/// Each outer execution gets a fresh egress identity. Inherited task-local
/// turns remain provenance only; nested runtime calls share the execution's
/// monotonic taint instead of minting a clean identity.
pub(crate) fn scope<F: std::future::Future>(agent: &str, future: F) -> impl std::future::Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        if current().is_some() {
            register_current(agent);
            return future.await;
        }
        let turn = format!("runtime-{}", uuid::Uuid::new_v4().simple());
        EXECUTION_TURN.scope(turn.clone(), async move {
            register_current(agent);
            if duduclaw_memory::feedback::CURRENT_TURN_ID
                .try_with(Clone::clone).ok().flatten().is_some()
            {
                return future.await;
            }
            let session = duduclaw_memory::feedback::CURRENT_SESSION_ID
                .try_with(Clone::clone).ok().flatten()
                .unwrap_or_else(|| format!("runtime:{agent}:{turn}"));
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(turn),
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session), future),
            ).await
        }).await
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
                let turn = current().unwrap();
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
                let turn = current().unwrap();
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

    #[tokio::test]
    async fn inherited_pre_restart_turn_is_never_registered_as_clean() {
        let agent = format!("restart-{}", uuid::Uuid::new_v4().simple());
        // This row came from a previous process: no execution task-local or
        // ledger registration survives, only its persisted provenance id.
        let inherited = "runtime-prior-process";
        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some(inherited.into()),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some("prior-session".into()), async {
                register_current(&agent);
                let _requester = crate::knowledge::turn_requesters::register(
                    &agent, inherited, duduclaw_knowledge::requester::RequesterIdentity::Unknown,
                );
                assert!(crate::egress_guard::query_turn(&agent, Some(inherited)).tainted);
                for kind in ["dispatch", "cron", "reminder", "mail", "heartbeat", "ephemeral"] {
                    scope(&agent, async {
                        let execution = current().unwrap();
                        assert_ne!(execution, inherited, "{kind}");
                        assert!(!crate::egress_guard::query_turn(&agent, Some(&execution)).tainted);
                        let pairs = crate::memory_provenance::turn_source_env_pairs();
                        assert!(pairs.iter().any(|(key, value)| key == duduclaw_core::ENV_TRUST_TURN_ID && value == inherited));
                        assert!(pairs.iter().any(|(key, value)| key == duduclaw_core::egress_guard::ENV_TURN_ALIAS && value == &execution));
                        crate::egress_guard::mark_turn(&agent, Some(&execution), "memory_search");
                        scope(&agent, async {
                            assert_eq!(current().as_deref(), Some(execution.as_str()));
                            register_current(&agent);
                            assert!(crate::egress_guard::query_turn(&agent, Some(&execution)).tainted);
                            assert!(crate::egress_guard::query_turn(&agent, Some(inherited)).tainted);
                        }).await;
                    }).await;
                }
            }),
        ).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn egress_hook_subprocess_inherits_spawn_turn() {
        scope("hook-inheritance", async {
            let turn = current().unwrap();
            let mut command = tokio::process::Command::new("sh");
            command.env_clear().env(duduclaw_core::ENV_TRUST_TURN_ID, "persisted-upstream")
                .env(duduclaw_core::egress_guard::ENV_TURN_ALIAS, &turn)
                .args(["-c", "sh -c 'printf %s \"$DUDUCLAW_EGRESS_TURN_ID\"'"]);
            let output = command.output().await.unwrap();
            assert!(output.status.success());
            assert_eq!(String::from_utf8(output.stdout).unwrap(), turn);
        }).await;
    }
}
