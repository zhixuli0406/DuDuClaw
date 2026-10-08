//! Effect classes and `action_rules` for tools of third-party MCP servers
//! (2026-10-08).
//!
//! DuDuClaw's own tools are classified by exact name
//! (`duduclaw_core::effect_of_builtin`) and gated inside `duduclaw
//! mcp-server`. Tools of the other servers in an employee's `.mcp.json` never
//! pass that server; DuDuClaw sees them only where it sits in the stream:
//!
//! - `duduclaw mcp-proxy` (stdio servers, `duduclaw-cli/src/mcp_proxy.rs`),
//!   put in the path by the spawn-time rewrite when redaction is active, when
//!   the employee has `[capabilities] action_rules`, or in the explore lane;
//! - `duduclaw mcp-remote-bridge` (remote servers,
//!   [`crate::remote_mcp::bridge`]), always in the path for them.
//!
//! Both call [`ThirdPartyGate`]: it classifies each tool from the
//! `annotations` of the upstream `tools/list`
//! ([`duduclaw_core::effect_from_annotations`]), hides refused tools from the
//! listing passed to the employee, refuses or asks (ApprovalBroker, fail
//! closed) on `tools/call`, and records what it saw in a snapshot the
//! dashboard reads ([`load_snapshots`]).
//!
//! Annotations are claims a server makes about itself. `destructiveHint` is
//! believed from everyone (it only makes a tool stricter); `readOnlyHint` is
//! believed only for servers the operator lists in `[capabilities]
//! trusted_read_hint_servers`, otherwise the tool is `modify`. The policy is
//! re-read from `agent.toml` for every listing and every call.
//!
//! Not covered: Claude Code built-in tools; `url` / `type` entries in
//! `.mcp.json` (the CLI talks to them directly, no proxy in the path);
//! runtimes other than the Claude CLI that start `.mcp.json` servers
//! themselves (Codex, Gemini, Antigravity, Grok register only DuDuClaw's own
//! server, and the openai-compat tool loop starts only `duduclaw
//! mcp-server`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::warn;

use duduclaw_core::{
    ProcessLane, ThirdPartyBlock, ThirdPartyDecision, ThirdPartyPolicy, ToolAnnotations, ToolEffect,
};

/// JSON-RPC error code for a refused tool (same as the DuDuClaw dispatch gate).
pub const REFUSED_CODE: i64 = -32003;
/// How long an `ask` waits for a person (same as the MCP server's tool approvals).
pub const APPROVAL_TTL_SECONDS: i64 = 300;
const APPROVAL_POLL: Duration = Duration::from_secs(2);
/// Directory under the home holding the per-server `tools/list` snapshots.
pub const SNAPSHOT_DIR: &str = "mcp_tool_effects";
/// Most tools kept in one snapshot.
const SNAPSHOT_MAX_TOOLS: usize = 500;
/// Description characters kept per tool in a snapshot.
const SNAPSHOT_DESCRIPTION_CHARS: usize = 200;

/// One tool as recorded in a snapshot (annotations only; the class and the
/// decision are recomputed from the current policy when read).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SnapshotTool {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub annotations: ToolAnnotations,
}

/// The last `tools/list` DuDuClaw saw for one employee's server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolsSnapshot {
    pub server: String,
    /// `proxy` or `bridge`.
    pub seen_by: String,
    pub observed_at: String,
    pub tools: Vec<SnapshotTool>,
}

/// The gate one proxy / bridge process applies to one server.
pub struct ThirdPartyGate {
    home: PathBuf,
    agent_id: String,
    server: String,
    seen_by: &'static str,
    lane: ProcessLane,
    /// Annotations from the latest listing, by tool name.
    annotations: Mutex<HashMap<String, ToolAnnotations>>,
}

impl ThirdPartyGate {
    /// Build the gate for `agent_id`'s `server`. The lane is read from this
    /// process's environment (`DUDUCLAW_LANE`, inherited from the CLI).
    pub fn new(home: &Path, agent_id: &str, server: &str, seen_by: &'static str) -> Self {
        Self::with_lane(home, agent_id, server, seen_by, ProcessLane::current())
    }

    /// [`Self::new`] with an explicit lane (tests).
    pub fn with_lane(
        home: &Path,
        agent_id: &str,
        server: &str,
        seen_by: &'static str,
        lane: ProcessLane,
    ) -> Self {
        Self {
            home: home.to_path_buf(),
            agent_id: agent_id.to_string(),
            server: server.to_string(),
            seen_by,
            lane,
            annotations: Mutex::new(HashMap::new()),
        }
    }

    /// The employee's current policy. An agent id that is not a valid id
    /// reads as a malformed rule list (every side effect asks).
    pub fn policy(&self) -> ThirdPartyPolicy {
        if !duduclaw_core::is_valid_agent_id(&self.agent_id) {
            return ThirdPartyPolicy {
                rules: duduclaw_core::ActionRules::unreadable(),
                trusted_read_hint_servers: Vec::new(),
            };
        }
        duduclaw_core::agent_toml::load_third_party_policy(&self.home.join("agents").join(&self.agent_id))
    }

    fn annotations_of(&self, tool: &str) -> ToolAnnotations {
        self.annotations
            .lock()
            .map(|m| m.get(tool).copied().unwrap_or_default())
            .unwrap_or_default()
    }

    /// Class and decision for one tool under the current policy.
    pub fn decide(&self, tool: &str) -> (ToolEffect, ThirdPartyDecision) {
        let policy = self.policy();
        let ann = self.annotations_of(tool);
        (
            policy.effect(&self.server, &ann),
            policy.decide(&self.server, tool, &ann, &self.lane),
        )
    }

    /// Filter one `tools/list` result in place: remember every tool's
    /// annotations, drop the tools this employee may not call here, and write
    /// the snapshot. A result without a `tools` array is left alone.
    pub fn filter_list_result(&self, result: &mut Value) {
        let Some(tools) = result.get_mut("tools").and_then(|t| t.as_array_mut()) else {
            return;
        };
        let policy = self.policy();
        let mut seen = Vec::new();
        {
            let mut map = match self.annotations.lock() {
                Ok(m) => m,
                Err(p) => p.into_inner(),
            };
            for t in tools.iter() {
                let Some(name) = t.get("name").and_then(|n| n.as_str()) else {
                    continue;
                };
                let ann = ToolAnnotations::from_tool(t);
                map.insert(name.to_string(), ann);
                if seen.len() < SNAPSHOT_MAX_TOOLS {
                    seen.push(SnapshotTool {
                        name: name.to_string(),
                        description: duduclaw_core::truncate_chars(
                            t.get("description").and_then(|d| d.as_str()).unwrap_or(""),
                            SNAPSHOT_DESCRIPTION_CHARS,
                        )
                        .to_string(),
                        annotations: ann,
                    });
                }
            }
        }
        let before = tools.len();
        tools.retain(|t| {
            let Some(name) = t.get("name").and_then(|n| n.as_str()) else {
                // A nameless entry cannot be called; pass it through untouched.
                return true;
            };
            let ann = ToolAnnotations::from_tool(t);
            !matches!(policy.decide(&self.server, name, &ann, &self.lane), ThirdPartyDecision::Block(_))
        });
        if tools.len() != before {
            tracing::debug!(
                server = %self.server,
                hidden = before - tools.len(),
                "third-party tools hidden by action_rules / explore lane"
            );
        }
        self.write_snapshot(seen);
    }

    fn write_snapshot(&self, tools: Vec<SnapshotTool>) {
        if !duduclaw_core::is_valid_agent_id(&self.agent_id)
            || !crate::mcp_scan::is_valid_mcp_server_name(&self.server)
        {
            return;
        }
        let snap = ToolsSnapshot {
            server: self.server.clone(),
            seen_by: self.seen_by.to_string(),
            observed_at: chrono::Utc::now().to_rfc3339(),
            tools,
        };
        let dir = self.home.join(SNAPSHOT_DIR).join(&self.agent_id);
        let path = dir.join(format!("{}.json", self.server));
        let result = (|| -> std::io::Result<()> {
            std::fs::create_dir_all(&dir)?;
            let body = serde_json::to_vec_pretty(&snap).map_err(std::io::Error::other)?;
            let tmp = dir.join(format!(".{}.{}.tmp", self.server, std::process::id()));
            std::fs::write(&tmp, body)?;
            std::fs::rename(&tmp, &path)
        })();
        if let Err(e) = result {
            warn!(server = %self.server, error = %e, "could not record the third-party tools snapshot");
        }
    }

    /// Gate one `tools/call`. `Ok(())` ⇒ forward; `Err(message)` ⇒ answer
    /// the call with a JSON-RPC error carrying the message. `ask` files an
    /// ApprovalBroker request and waits; an unavailable broker, a failed
    /// request, a denial and an expiry all refuse (fail closed).
    pub async fn check_call(&self, tool: &str) -> Result<(), String> {
        let (effect, decision) = self.decide(tool);
        match decision {
            ThirdPartyDecision::Allow => Ok(()),
            ThirdPartyDecision::Block(why) => {
                let msg = match why {
                    ThirdPartyBlock::Rule => format!(
                        "Tool \"{}.{tool}\" ({effect}) is blocked by this employee's [capabilities] action_rules.",
                        self.server
                    ),
                    ThirdPartyBlock::Lane => format!(
                        "Tool \"{}.{tool}\" ({effect}) is not available in the read-only explore lane.",
                        self.server
                    ),
                };
                self.audit("third_party_tool_refused", json!({ "reason": why.as_str() }), tool, effect);
                Err(msg)
            }
            ThirdPartyDecision::Ask => self.ask(tool, effect).await,
        }
    }

    async fn ask(&self, tool: &str, effect: ToolEffect) -> Result<(), String> {
        use crate::approval::{ApprovalBroker, ApprovalStatus};
        let label = format!("{}.{tool}", self.server);
        let broker = match ApprovalBroker::open(&self.home) {
            Ok(b) => b,
            Err(e) => {
                warn!(error = %e, "ApprovalBroker unavailable — refusing third-party tool call (fail-closed)");
                self.audit("third_party_tool_approval", json!({ "outcome": "broker_unavailable" }), tool, effect);
                return Err(format!(
                    "The approval system is unavailable, so the call to \"{label}\" was refused."
                ));
            }
        };
        let summary = format!(
            "工具「{label}」（第三方 MCP 伺服器，{effect} 類動作）依此代理的 action_rules 需經管理員核可後才能執行"
        );
        let payload = duduclaw_core::with_host_task_id(
            json!({ "tool": label, "server": self.server, "effect": effect.as_str(), "third_party": true }),
            duduclaw_core::host_task_id().as_deref(),
        );
        let id = match broker
            .request(&self.agent_id, "mcp_call", &summary, payload, APPROVAL_TTL_SECONDS)
            .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, "third-party tool approval request failed — refusing");
                self.audit("third_party_tool_approval", json!({ "outcome": "request_failed" }), tool, effect);
                return Err(format!("The approval request for \"{label}\" could not be filed, so the call was refused."));
            }
        };
        let status = broker.await_decision(&id, APPROVAL_POLL).await;
        let outcome = match &status {
            Ok(s) => s.as_str().to_string(),
            Err(_) => "wait_failed".to_string(),
        };
        self.audit(
            "third_party_tool_approval",
            json!({ "outcome": outcome, "approval_id": id.to_string() }),
            tool,
            effect,
        );
        match status {
            Ok(ApprovalStatus::Approved) => Ok(()),
            Ok(ApprovalStatus::Expired) => Err(format!(
                "The call to \"{label}\" was not approved in time and was refused (approval {id})."
            )),
            _ => Err(format!("The call to \"{label}\" was refused by an administrator (approval {id}).")),
        }
    }

    fn audit(&self, event: &str, mut details: Value, tool: &str, effect: ToolEffect) {
        if let Some(obj) = details.as_object_mut() {
            obj.insert("server".into(), json!(self.server));
            obj.insert("tool".into(), json!(duduclaw_core::truncate_chars(tool, 128)));
            obj.insert("effect".into(), json!(effect.as_str()));
            obj.insert("seen_by".into(), json!(self.seen_by));
            obj.insert(
                "lane".into(),
                json!(match self.lane {
                    ProcessLane::Normal => "normal",
                    ProcessLane::Explore => "explore",
                    ProcessLane::Invalid => "invalid",
                }),
            );
        }
        duduclaw_security::audit::append_audit_event(
            &self.home,
            &duduclaw_security::audit::AuditEvent::new(
                event,
                self.agent_id.clone(),
                duduclaw_security::audit::Severity::Warning,
                details,
            ),
        );
    }
}

/// A JSON-RPC error answer for a refused call.
pub fn refusal_frame(id: &Value, message: &str) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": REFUSED_CODE, "message": message }
    })
    .to_string()
}

/// One tool as the dashboard shows it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolEffectView {
    pub name: String,
    pub description: String,
    pub annotations: ToolAnnotations,
    /// The class under the current policy.
    pub effect: ToolEffect,
    /// `allow` / `ask` / `block` outside the explore lane.
    pub verdict: &'static str,
    /// Listed and callable in the explore lane.
    pub explore_visible: bool,
}

/// One server's snapshot, re-evaluated against the current policy.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ServerEffectsView {
    pub server: String,
    pub seen_by: String,
    pub observed_at: String,
    pub read_hint_trusted: bool,
    pub tools: Vec<ToolEffectView>,
}

fn verdict_token(d: ThirdPartyDecision) -> &'static str {
    match d {
        ThirdPartyDecision::Allow => "allow",
        ThirdPartyDecision::Ask => "ask",
        ThirdPartyDecision::Block(_) => "block",
    }
}

/// Every snapshot recorded for `agent_id`, re-evaluated against its current
/// policy. Unreadable snapshot files are skipped. A server appears only
/// after its tools were listed once through the proxy or the bridge.
pub fn load_snapshots(home: &Path, agent_id: &str) -> Vec<ServerEffectsView> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return Vec::new();
    }
    let policy = duduclaw_core::agent_toml::load_third_party_policy(&home.join("agents").join(agent_id));
    let dir = home.join(SNAPSHOT_DIR).join(agent_id);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read(&p) else { continue };
        let Ok(snap) = serde_json::from_slice::<ToolsSnapshot>(&raw) else { continue };
        if !crate::mcp_scan::is_valid_mcp_server_name(&snap.server) {
            continue;
        }
        let tools = snap
            .tools
            .iter()
            .map(|t| {
                let normal = policy.decide(&snap.server, &t.name, &t.annotations, &ProcessLane::Normal);
                let explore = policy.decide(&snap.server, &t.name, &t.annotations, &ProcessLane::Explore);
                ToolEffectView {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    annotations: t.annotations,
                    effect: policy.effect(&snap.server, &t.annotations),
                    verdict: verdict_token(normal),
                    explore_visible: !matches!(explore, ThirdPartyDecision::Block(_)),
                }
            })
            .collect();
        out.push(ServerEffectsView {
            read_hint_trusted: policy.trusts_read_hint(&snap.server),
            server: snap.server,
            seen_by: snap.seen_by,
            observed_at: snap.observed_at,
            tools,
        });
    }
    out.sort_by(|a, b| a.server.cmp(&b.server));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with_rules(toml: &str) -> tempfile::TempDir {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("agents").join("a1");
        std::fs::create_dir_all(&a).unwrap();
        std::fs::write(a.join("agent.toml"), toml).unwrap();
        d
    }

    fn listing() -> Value {
        json!({ "tools": [
            { "name": "get", "annotations": { "readOnlyHint": true } },
            { "name": "put" },
            { "name": "drop", "annotations": { "destructiveHint": true } },
        ]})
    }

    fn names(v: &Value) -> Vec<&str> {
        v["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect()
    }

    #[tokio::test]
    async fn rules_hide_and_refuse_and_snapshot_is_recorded() {
        let home = home_with_rules(
            "[capabilities]\naction_rules = [{ effect = \"delete\", verdict = \"block\" }, { tool = \"crm.put\", verdict = \"allow\" }]\ntrusted_read_hint_servers = [\"crm\"]\n",
        );
        let g = ThirdPartyGate::with_lane(home.path(), "a1", "crm", "proxy", ProcessLane::Normal);
        let mut r = listing();
        g.filter_list_result(&mut r);
        assert_eq!(names(&r), vec!["get", "put"]);
        assert!(g.check_call("get").await.is_ok());
        assert!(g.check_call("put").await.is_ok());
        let err = g.check_call("drop").await.unwrap_err();
        assert!(err.contains("action_rules"), "{err}");
        let views = load_snapshots(home.path(), "a1");
        assert_eq!(views.len(), 1);
        assert!(views[0].read_hint_trusted);
        let by: HashMap<_, _> = views[0].tools.iter().map(|t| (t.name.as_str(), t)).collect();
        assert_eq!(by["get"].effect, ToolEffect::Read);
        assert_eq!(by["put"].effect, ToolEffect::Modify);
        assert_eq!(by["drop"].verdict, "block");
        assert!(by["get"].explore_visible && !by["put"].explore_visible);
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("third_party_tool_refused"));
    }

    #[tokio::test]
    async fn explore_lane_lists_only_trusted_reads_and_unknown_tools_are_modify() {
        let home = home_with_rules("[capabilities]\n");
        let g = ThirdPartyGate::with_lane(home.path(), "a1", "crm", "bridge", ProcessLane::Explore);
        let mut r = listing();
        g.filter_list_result(&mut r);
        // Not trusted: the read-only claim is not believed.
        assert!(names(&r).is_empty());
        assert!(g.check_call("get").await.is_err());
        let home = home_with_rules("[capabilities]\ntrusted_read_hint_servers = [\"crm\"]\n");
        let g = ThirdPartyGate::with_lane(home.path(), "a1", "crm", "bridge", ProcessLane::Explore);
        let mut r = listing();
        g.filter_list_result(&mut r);
        assert_eq!(names(&r), vec!["get"]);
        assert!(g.check_call("get").await.is_ok());
        // Never listed ⇒ no annotations ⇒ modify ⇒ refused in the lane.
        assert!(g.check_call("secret_tool").await.is_err());
    }

    #[tokio::test]
    async fn no_rules_change_nothing_and_ask_fails_closed_without_a_decision() {
        let home = home_with_rules("[capabilities]\n");
        let g = ThirdPartyGate::with_lane(home.path(), "a1", "crm", "proxy", ProcessLane::Normal);
        let mut r = listing();
        g.filter_list_result(&mut r);
        assert_eq!(names(&r).len(), 3);
        assert!(g.check_call("drop").await.is_ok());
        assert_eq!(g.decide("put").1, ThirdPartyDecision::Allow);
        // An ask whose request expires refuses; run with a broker in a home
        // where the decision never comes is covered by the bridge
        // integration test. Here: an invalid agent id reads as malformed.
        let g = ThirdPartyGate::with_lane(home.path(), "../x", "crm", "proxy", ProcessLane::Normal);
        assert_eq!(g.decide("put").1, ThirdPartyDecision::Ask);
        assert_eq!(g.decide("get").1, ThirdPartyDecision::Ask);
    }
}
