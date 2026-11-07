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
            gate: crate::third_party_tools::ThirdPartyGate::with_lane(
                home, agent, server, "tool_loop",
                if crate::explore_lane::in_explore() {
                    duduclaw_core::ProcessLane::Explore
                } else {
                    duduclaw_core::ProcessLane::Normal
                },
            ),
        }
    }
}

#[async_trait::async_trait]
impl ClientCallBoundary for RegistryBoundary {
    fn filter_tools(&self, tools: &mut Vec<duduclaw_llm::McpToolDef>) -> Result<(), String> {
        let mut result = serde_json::json!({"tools": tools.iter().map(|tool| {
            serde_json::json!({
                "name": tool.name, "description": tool.description,
                "annotations": tool.annotations,
            })
        }).collect::<Vec<_>>()});
        self.gate.filter_list_result(&mut result);
        let allowed: std::collections::HashSet<&str> = result["tools"].as_array()
            .ok_or_else(|| "Third-party MCP listing could not be gated".to_string())?
            .iter().filter_map(|tool| tool["name"].as_str()).collect();
        tools.retain(|tool| allowed.contains(tool.name.as_str()));
        Ok(())
    }

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
        let approved = match self.gate.check_call_with_taint_and_id(tool, args, taint, &id).await {
            Ok(approved) => approved,
            Err(reason) => {
                if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
                return Err(reason);
            }
        };
        let effect = self.gate.decide(tool).0;
        if let Err(reason) = crate::recipient_guard::confirm_unadapted_effect(
            &self.home, &self.agent, &label, effect, approved.as_ref(),
        ) {
            if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
            return Err(reason);
        }
        // Read tools keep the private-read reservation without acquiring a
        // sink lease. Upstream hints always remain at least Modify in ThirdPartyGate.
        if effect == duduclaw_core::ToolEffect::Read {
            crate::credential_declaration::observe_arguments(&self.home, &self.agent, args).await;
            return Ok((reserved.then_some(id), None));
        }
        // Recheck sinks after any approval wait.
        let (lease, taint) = match crate::egress_client::SinkLease::direct(
            &self.home, &self.agent, self.turn.as_deref(), read_id,
        ) {
            Ok(admission) => admission,
            Err(reason) => {
                if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
                crate::egress_client::reservation_failure(&self.home, &self.agent, tool, &reason)?;
                crate::egress_gate::confirm_dispatch_with_call(
                    &self.home, &self.agent, &label, true, crate::egress_guard::Taint::unknown(), approved.as_ref(), &id,
                )?;
                crate::credential_declaration::observe_arguments(&self.home, &self.agent, args).await;
                return Ok((None, None));
            }
        };
        if let Err(reason) = crate::egress_gate::confirm_dispatch_with_call(
            &self.home, &self.agent, &label, true, taint, approved.as_ref(), &id,
        ) {
            if reserved { let _ = crate::egress_guard::finish_turn(&self.agent, self.turn.as_deref(), &id, false); }
            return Err(reason);
        }
        crate::credential_declaration::observe_arguments(&self.home, &self.agent, args).await;
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

    fn external_listing() -> Vec<duduclaw_llm::McpToolDef> {
        duduclaw_llm::parse_tools_list_response(&serde_json::json!({"result": {"tools": [
            {"name": "get", "annotations": {"readOnlyHint": true}},
            {"name": "put"},
            {"name": "drop", "annotations": {"destructiveHint": true}}
        ]}})).unwrap()
    }

    #[tokio::test]
    async fn external_stdio_and_http_annotations_gate_listing_and_calls() {
        for transport in ["command='never-spawn-test'", "url='https://example.invalid/mcp'"] {
            let home = tempfile::tempdir().unwrap();
            let dir = home.path().join("agents/bot");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='off'\nrecipient_guard='off'\n").unwrap();
            std::fs::write(dir.join("agent.toml"), format!(
                "[capabilities]\ntrusted_read_hint_servers=['crm']\naction_rules=[{{effect='delete',verdict='block'}},{{effect='modify',verdict='block'}}]\n[[mcp.external]]\nname='crm'\n{transport}\n",
            )).unwrap();
            let servers = crate::mcp_external::load_external_mcp_servers(&dir);
            assert_eq!(servers.len(), 1);
            let boundary = RegistryBoundary::new(home.path(), "bot", &servers[0].name);
            let mut tools = external_listing();
            boundary.filter_tools(&mut tools).unwrap();
            assert!(tools.is_empty());
            assert_eq!(boundary.gate.decide("get").0, duduclaw_core::ToolEffect::Modify);
            assert_eq!(boundary.gate.decide("drop").0, duduclaw_core::ToolEffect::Delete);
            for name in ["get", "put", "drop", "invented"] {
                assert!(boundary.before_call(name, &serde_json::json!({})).await.is_err(), "{name}");
            }
            // Removing the inert legacy trust list cannot weaken enforcement.
            std::fs::write(dir.join("agent.toml"), "[capabilities]\naction_rules=[{effect='modify',verdict='block'}]\n").unwrap();
            assert!(boundary.before_call("get", &serde_json::json!({})).await.is_err());
        }
    }

    #[tokio::test]
    async fn external_explore_rejects_read_claims_even_with_legacy_trust() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\ntrusted_read_hint_servers=['crm']\n").unwrap();
        let boundary = crate::explore_lane::EXPLORE.scope(true, async {
            RegistryBoundary::new(home.path(), "bot", "crm")
        }).await;
        let mut tools = external_listing();
        boundary.filter_tools(&mut tools).unwrap();
        assert!(tools.is_empty());
        let untrusted = crate::explore_lane::EXPLORE.scope(true, async {
            RegistryBoundary::new(home.path(), "bot", "other")
        }).await;
        let mut tools = external_listing();
        untrusted.filter_tools(&mut tools).unwrap();
        assert!(tools.is_empty());
    }

    #[tokio::test]
    async fn external_read_claim_with_legacy_trust_still_holds_a_sink_lease() {
        let home = tempfile::tempdir().unwrap();
        let agent = format!("read-claim-{}", uuid::Uuid::new_v4());
        let turn = uuid::Uuid::new_v4().to_string();
        let dir = home.path().join("agents").join(&agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\ntrusted_read_hint_servers=['crm']\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='block'\nrecipient_guard='off'\n").unwrap();
        crate::egress_guard::register_turn(&agent, &turn);
        let boundary = RegistryBoundary {
            home: home.path().into(), agent: agent.clone(), server: "crm".into(), turn: Some(turn.clone()),
            gate: crate::third_party_tools::ThirdPartyGate::new(home.path(), &agent, "crm", "tool_loop"),
        };
        boundary.filter_tools(&mut external_listing()).unwrap();
        let (read, lease) = boundary.begin_scoped_call("get", &serde_json::json!({})).await.unwrap();
        let lease = lease.expect("upstream read-only claims must acquire a sink lease");
        assert!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_read", "during").is_err());
        boundary.finish_call("get", read.as_deref(), false).await.unwrap();
        drop(lease);
        crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_read", "after").unwrap();
        crate::egress_guard::finish_turn(&agent, Some(&turn), "after", false).unwrap();
    }

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
