//! Per-client protection for direct `[[mcp.external]]` tool-loop mounts.
use duduclaw_llm::ClientCallBoundary;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(crate) struct RegistryBoundary {
    home: PathBuf,
    agent: String,
    server: String,
    turn: Option<String>,
    gate: crate::third_party_tools::ThirdPartyGate,
}

impl RegistryBoundary {
    pub(crate) fn new(home: &Path, agent: &str, server: &str) -> Self {
        Self {
            home: home.into(),
            agent: agent.into(),
            server: server.into(),
            turn: crate::runtime_turn::current().or_else(||
                duduclaw_memory::feedback::CURRENT_TURN_ID.try_with(Clone::clone).ok().flatten()),
            // This registry's public schemas do not retain MCP annotations.
            // The existing decision therefore uses the conservative Modify
            // class, while exact tool action rules remain effective.
            gate: crate::third_party_tools::ThirdPartyGate::new(home, agent, server, "tool_loop"),
        }
    }
}

#[async_trait::async_trait]
impl ClientCallBoundary for RegistryBoundary {
    async fn before_call(&self, tool: &str, args: &Value) -> Result<(), String> {
        let taint = crate::egress_guard::query_turn(&self.agent, self.turn.as_deref());
        self.gate.check_call_with_taint(tool, args, taint).await.map(|_| ())
    }

    async fn begin_call(&self, _tool: &str, _args: &Value) -> Result<Option<String>, String> {
        Err("外送防護需要由工具傳輸端持有呼叫保護，已拒絕執行。".into())
    }

    async fn begin_scoped_call(
        &self, tool: &str, args: &Value,
    ) -> Result<(Option<String>, Option<Box<dyn duduclaw_llm::ClientCallLease>>), String> {
        let label = format!("{}.{tool}", self.server);
        let source = duduclaw_core::truncate_bytes(&label, 256);
        let id = uuid::Uuid::new_v4().to_string();
        let (taint, reserved) = match crate::egress_guard::reserve_turn(&self.home, &self.agent, self.turn.as_deref(), source, &id) {
            Ok(taint) => (taint, true),
            Err(reason) => {
                crate::egress_client::reservation_failure(&self.home, &self.agent, tool, &reason)?;
                (crate::egress_guard::Taint::unknown(), false)
            }
        };
        let read_id = reserved.then_some(id.as_str());
        let approved = match self.gate.check_call_with_taint(tool, args, taint).await {
            Ok(approved) => approved,
            Err(reason) => {
                if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
                return Err(reason);
            }
        };
        // Registry schemas do not retain annotations: every call is both a
        // private read and a conservative sink. Recheck after any approval wait.
        let (lease, taint) = match crate::egress_client::SinkLease::direct(
            &self.home, &self.agent, self.turn.as_deref(), read_id,
        ) {
            Ok(admission) => admission,
            Err(reason) => {
                if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
                crate::egress_client::reservation_failure(&self.home, &self.agent, tool, &reason)?;
                crate::egress_gate::confirm_dispatch(
                    &self.home, &self.agent, &label, true, crate::egress_guard::Taint::unknown(), approved.as_ref(),
                )?;
                return Ok((None, None));
            }
        };
        if let Err(reason) = crate::egress_gate::confirm_dispatch(
            &self.home, &self.agent, &label, true, taint, approved.as_ref(),
        ) {
            if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
            return Err(reason);
        }
        Ok((reserved.then_some(id), Some(Box::new(lease))))
    }

    async fn finish_call(&self, tool: &str, reservation: Option<&str>, payload: bool) -> Result<(), String> {
        if let Some(id) = reservation {
            if let Err(reason) = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), id, payload) {
                return crate::egress_client::reservation_failure(&self.home, &self.agent, tool, &reason);
            }
        } else if payload {
            return self.after_success(tool).await;
        }
        Ok(())
    }

    async fn after_success(&self, tool: &str) -> Result<(), String> {
        let source = format!("{}.{tool}", self.server);
        let source = duduclaw_core::truncate_bytes(&source, 256);
        let source = if source.chars().any(char::is_control) || source.is_empty() {
            "unclassified_external_tool"
        } else {
            source
        };
        if self.turn.is_none() {
            return crate::egress_client::reservation_failure(
                &self.home, &self.agent, source, crate::egress_guard::UNTRACKED_TURN,
            );
        }
        let state = crate::egress_guard::mark_turn(&self.agent, self.turn.as_deref(), source);
        let confirmed = self
            .turn
            .as_deref()
            .is_some_and(crate::computer_use_sessions::turns::valid_turn_id)
            && state.tainted
            && state
                .sources
                .iter()
                .any(|s| s == source || s == "additional_private_sources");
        let dir = crate::ephemeral::resolve_agent_dir(&self.home, &self.agent)
            .unwrap_or_else(|| self.home.join("agents").join(&self.agent));
        if confirmed
            || duduclaw_core::egress_guard::effective_mode(&self.home, Some(&dir))
                < duduclaw_core::egress_guard::Mode::Ask
        {
            Ok(())
        } else {
            Err("Private result could not be registered for this turn".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn external_mount_marks_the_same_turn_and_blocks_next_call() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = "external-boundary-test";
        let turn = "external-boundary-turn";
        let dir = tmp.path().join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(
            tmp.path().join("config.toml"),
            "[security]\negress_guard='block'\n",
        )
        .unwrap();
        crate::egress_guard::register_turn(agent, turn);
        let boundary = RegistryBoundary {
            home: tmp.path().into(),
            agent: agent.into(),
            server: "crm".into(),
            turn: Some(turn.into()),
            gate: crate::third_party_tools::ThirdPartyGate::new(
                tmp.path(),
                agent,
                "crm",
                "tool_loop",
            ),
        };
        let (held, lease) = boundary.begin_scoped_call("fetch", &serde_json::json!({})).await.unwrap();
        assert!(crate::egress_guard::query_turn(agent, Some(turn)).tainted);
        assert!(boundary.begin_scoped_call("send", &serde_json::json!({"text":"private"})).await.is_err());
        boundary.finish_call("fetch", held.as_deref(), true).await.unwrap();
        drop(lease);
        let state = crate::egress_guard::query_turn(agent, Some(turn));
        assert_eq!(state.sources, ["crm.fetch"]);
        assert!(boundary
            .before_call("send", &serde_json::json!({"text":"private"}))
            .await
            .is_err());
    }
    #[tokio::test]
    async fn egress_registry_not_dispatched_scope_releases_on_completion_and_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let agent = "registry-lease-test";
        let turn = "registry-lease-turn";
        let dir = tmp.path().join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='block'\n").unwrap();
        crate::egress_guard::register_turn(agent, turn);
        let boundary = std::sync::Arc::new(RegistryBoundary {
            home: tmp.path().into(), agent: agent.into(), server: "crm".into(), turn: Some(turn.into()),
            gate: crate::third_party_tools::ThirdPartyGate::new(tmp.path(), agent, "crm", "tool_loop"),
        });
        let (read, lease) = boundary.begin_scoped_call("send", &serde_json::json!({})).await.unwrap();
        boundary.finish_call("send", read.as_deref(), false).await.unwrap();
        assert!(crate::egress_guard::reserve_turn(tmp.path(), agent, Some(turn), "memory_read", "later").is_err());
        drop(lease);
        assert!(!crate::egress_guard::reserve_turn(tmp.path(), agent, Some(turn), "memory_read", "later").unwrap().tainted);
        crate::egress_guard::finish_turn(agent, Some(turn), "later", false).unwrap();

        let task_boundary = boundary.clone();
        let (ready, waiting) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let (read, lease) = task_boundary.begin_scoped_call("send", &serde_json::json!({})).await.unwrap();
            task_boundary.finish_call("send", read.as_deref(), false).await.unwrap();
            let _ = ready.send(());
            std::future::pending::<()>().await;
            drop(lease);
        });
        waiting.await.unwrap();
        assert!(crate::egress_guard::reserve_turn(tmp.path(), agent, Some(turn), "memory_read", "cancelled").is_err());
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        crate::egress_guard::reserve_turn(tmp.path(), agent, Some(turn), "memory_read", "cancelled").unwrap();
        crate::egress_guard::finish_turn(agent, Some(turn), "cancelled", true).unwrap();
        assert_eq!(crate::egress_guard::query_turn(agent, Some(turn)).sources, ["memory_read"]);
    }

    #[tokio::test]
    async fn egress_registry_dispatched_timeout_fences_reads_and_terminal_releases() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/bot")).unwrap();
        std::fs::write(home.path().join("agents/bot/agent.toml"), "").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='block'\n").unwrap();
        for terminal in [false, true] {
            let agent = format!("registry-{}", uuid::Uuid::new_v4());
            let turn = uuid::Uuid::new_v4().to_string();
            crate::egress_guard::register_turn(&agent, &turn);
            let boundary = RegistryBoundary {
                home: home.path().into(), agent: agent.clone(), server: "crm".into(), turn: Some(turn.clone()),
                gate: crate::third_party_tools::ThirdPartyGate::new(home.path(), &agent, "crm", "tool_loop"),
            };
            let (read, lease) = boundary.begin_scoped_call("send", &serde_json::json!({})).await.unwrap();
            let lease = lease.unwrap();
            lease.dispatched();
            if terminal {
                lease.terminal_response();
                boundary.finish_call("send", read.as_deref(), true).await.unwrap();
                drop(lease);
            } else {
                let _ = tokio::time::timeout(std::time::Duration::from_millis(1), async move {
                    let _lease = lease;
                    std::future::pending::<()>().await;
                }).await;
            }
            assert_eq!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "next").is_err(), !terminal);
        }
    }

    #[tokio::test]
    async fn egress_registry_rechecks_after_real_approval_wait_and_releases_refused_call() {
        use crate::approval::ApprovalBroker;
        use std::time::Duration;

        let tmp = tempfile::tempdir().unwrap();
        let agent = "registry-approval-lease-test";
        let turn = "registry-approval-lease-turn";
        let dir = tmp.path().join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\naction_rules=[{tool='crm.send',verdict='ask'}]\n").unwrap();
        std::fs::write(tmp.path().join("config.toml"), "[security]\negress_guard='ask'\n").unwrap();
        crate::egress_guard::register_turn(agent, turn);
        let boundary = RegistryBoundary {
            home: tmp.path().into(), agent: agent.into(), server: "crm".into(), turn: Some(turn.into()),
            gate: crate::third_party_tools::ThirdPartyGate::new(tmp.path(), agent, "crm", "tool_loop"),
        };
        let broker = ApprovalBroker::open(tmp.path()).unwrap();
        let call = tokio::spawn(async move {
            boundary.begin_scoped_call("send", &serde_json::json!({"text":"outbound"})).await
        });
        let request = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let mut requests = broker.list_pending(Some(agent)).await.unwrap();
                if let Some(request) = requests.pop() { break request; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.expect("call must reach the real approval wait");
        assert!(request.payload.get("egress_guard").is_none(), "the turn was clean before approval");
        assert_eq!(request.payload["reason"], "action_rule");
        assert!(!call.is_finished());
        assert_eq!(crate::egress_guard::query_turn(agent, Some(turn)).sources, ["pending_private_read"]);

        // A different transport completes a private read while this approval waits.
        crate::egress_guard::mark_turn(agent, Some(turn), "memory_read");
        broker.decide(&request.id, true, "dashboard:test").await.unwrap();
        let outcome = tokio::time::timeout(Duration::from_secs(5), call).await
            .expect("approved call must resume").expect("call task must not panic");
        let reason = outcome.err().expect("new private data must refuse the approved sink");
        assert!(reason.contains("等待核准期間"), "{reason}");
        assert_eq!(crate::egress_guard::query_turn(agent, Some(turn)).sources, ["memory_read"],
            "the refused call's private-read reservation must be removed");

        let read = uuid::Uuid::new_v4().to_string();
        crate::egress_guard::reserve_turn(tmp.path(), agent, Some(turn), "next_read", &read)
            .expect("the refused call's sink lease must be released");
        crate::egress_guard::finish_turn(agent, Some(turn), &read, true).unwrap();
        assert_eq!(crate::egress_guard::query_turn(agent, Some(turn)).sources, ["memory_read", "next_read"]);
    }

    #[tokio::test]
    async fn egress_registry_untracked_sink_holds_agent_lease() {
        let home = tempfile::tempdir().unwrap();
        let agent = format!("untracked-{}", uuid::Uuid::new_v4());
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='shadow'\n").unwrap();
        let boundary = RegistryBoundary {
            home: home.path().into(), agent: agent.clone(), server: "crm".into(), turn: None,
            gate: crate::third_party_tools::ThirdPartyGate::new(home.path(), &agent, "crm", "tool_loop"),
        };
        let (read, lease) = boundary.begin_scoped_call("send", &serde_json::json!({})).await.unwrap();
        assert!(read.is_none());
        let lease = lease.expect("untracked sinks also acquire an agent fence");
        lease.dispatched();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='ask'\n").unwrap();
        let other_turn = uuid::Uuid::new_v4().to_string();
        crate::egress_guard::register_turn(&agent, &other_turn);
        assert_eq!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&other_turn), "memory_search", "read").unwrap_err(), crate::egress_guard::READ_DURING_SINK);
        lease.terminal_response();
        drop(lease);
        assert!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&other_turn), "memory_search", "read").is_ok());
    }

    #[tokio::test]
    async fn egress_unattributed_private_results_are_allowed_and_audited() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents/unknown-turn-agent");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        let mut boundary = RegistryBoundary::new(tmp.path(), "unknown-turn-agent", "crm");
        boundary.turn = None;
        for (mode, denied) in [
            ("off", false),
            ("shadow", false),
            ("ask", false),
            ("block", false),
        ] {
            std::fs::write(
                tmp.path().join("config.toml"),
                format!("[security]\negress_guard='{mode}'\n"),
            )
            .unwrap();
            assert_eq!(
                boundary.after_success("fetch").await.is_err(),
                denied,
                "{mode}"
            );
        }
    }
}
