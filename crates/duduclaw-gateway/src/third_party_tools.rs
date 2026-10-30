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
//!   for Codex always (2026-10-19, `runtime/codex/third_party.rs`), because
//!   Codex auto-approves the servers it is given; for Grok and
//!   the in-process tool loop (openai-compat, Direct API, local inference)
//!   always too (2026-10-20, `runtime/third_party_mcp.rs`), Grok as `--spawn-scoped` proxies in their persistent configs;
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
//! `.mcp.json` on a Claude spawn that needs no gate (the CLI talks to them
//! directly, no proxy in the path; a gated spawn and every other runtime
//! remove them); `agent.toml [[mcp.external]]` servers of the tool loop
//! (mounted directly, unmounted in the explore lane); servers an operator
//! configured for a CLI outside DuDuClaw (Grok also reads the OS user's
//! `~/.grok/config.toml`;
//! Grok's Claude/Cursor-compat sources `~/.claude.json` and
//! `~/.cursor/mcp.json` are switched off per spawn, `runtime/grok.rs`
//! `COMPAT_MCP_SOURCES_OFF`); entries in an employee's
//! `.grok/config.toml` that DuDuClaw did not
//! write (kept, warned on every spawn); a `.mcp.json` changed between the
//! spawn's read and the CLI's own. Grok's project-level sources (`.mcp.json`
//! and `.cursor/mcp.json` of the working directory, and `.mcp.json` /
//! `.grok/config.toml` up to the git root) are shadowed in the working
//! directory's `.grok/config.toml` (`runtime/grok/third_party/sources.rs`).
//! Every runtime registers the
//! employee's `.mcp.json` third-party servers through this gate: the Claude
//! CLI through the spawn-time rewrite, Codex since 2026-10-19, Grok
//! and the tool loop since 2026-10-20 (before that Grok started
//! them itself from `.mcp.json`, ungated). The Gemini CLI runtime was removed
//! in v1.75.0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    pending_reads: Arc<Mutex<HashMap<String, PendingRead>>>,
}

const MAX_PENDING_READS: usize = 512;
const PENDING_READ_TTL: Duration = Duration::from_secs(300);
const BRIDGE_WIRE_NAMESPACE: &str = "duduclaw-egress";

fn bridge_wire_id(generation: &str) -> Value {
    Value::String(format!("{BRIDGE_WIRE_NAMESPACE}:{generation}"))
}

fn bridge_wire_generation(id: &Value) -> Option<&str> {
    let (namespace, generation) = id.as_str()?.split_once(':')?;
    (namespace == BRIDGE_WIRE_NAMESPACE).then_some(generation)
}

/// Keep the wire namespace disjoint from caller-chosen IDs, including requests
/// such as initialize/list/ping whose IDs are otherwise forwarded unchanged.
pub(crate) fn bridge_reserved_id(id: &Value) -> bool {
    bridge_wire_generation(id).is_some()
}

struct PendingRead {
    generation: String,
    deadline: Instant,
    read: Option<crate::egress_client::PrivateRead>,
    source: String,
    done: tokio::sync::oneshot::Sender<()>,
}

/// Owns one correlation slot. Cancellation/timeout removes it without releasing
/// gateway taint: an uncertain upstream call may still produce private data.
pub struct PendingCall {
    pending: Arc<Mutex<HashMap<String, PendingRead>>>,
    key: String,
    generation: String,
    deadline: Instant,
    done: Option<tokio::sync::oneshot::Receiver<()>>,
    sink: Option<Arc<crate::egress_client::SinkLease>>,
    live: Arc<std::sync::atomic::AtomicBool>,
    cancel: tokio::sync::watch::Sender<bool>,
}

/// A queued write must not dispatch after its owning call timed out/cancelled.
#[derive(Clone)]
pub struct DispatchPermit {
    // Retain the lease through the actual queued write/flush.
    _sink: Option<Arc<crate::egress_client::SinkLease>>,
    cancel: tokio::sync::watch::Receiver<bool>,
    live: Arc<std::sync::atomic::AtomicBool>,
    deadline: Instant,
}

impl DispatchPermit {
    /// The transport calls this before any bytes may leave the process.
    pub fn dispatched(&self) {
        if let Some(sink) = &self._sink { sink.dispatched(); }
    }

    /// End an in-progress pipe write when the call is cancelled or expires.
    pub async fn cancelled(&mut self) {
        let _ = tokio::time::timeout_at(tokio::time::Instant::from_std(self.deadline), async {
            loop {
                let live = *self.cancel.borrow_and_update();
                if !live || self.cancel.changed().await.is_err() { break; }
            }
        }).await;
    }

    pub fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire) && Instant::now() < self.deadline
    }
}

impl Drop for PendingCall {
    fn drop(&mut self) {
        self.live.store(false, std::sync::atomic::Ordering::Release);
        self.cancel.send_replace(false);
        if let Ok(mut pending) = self.pending.lock() {
            if pending.get(&self.key).is_some_and(|p| p.generation == self.generation) {
                pending.remove(&self.key);
            }
        }
    }
}

impl PendingCall {
    /// The bridge must send this generation-specific ID, never the caller's
    /// reusable JSON-RPC ID, including when an answer arrives on the GET stream.
    pub(crate) fn bridge_wire_id(&self) -> Value {
        bridge_wire_id(&self.generation)
    }

    pub fn dispatch_permit(&self) -> DispatchPermit {
        DispatchPermit { live: self.live.clone(), deadline: self.deadline, _sink: self.sink.clone(), cancel: self.cancel.subscribe() }
    }

    pub async fn wait(mut self) {
        if let Some(done) = self.done.take() {
            if matches!(tokio::time::timeout_at(tokio::time::Instant::from_std(self.deadline), done).await, Ok(Ok(()))) {
                if let Some(sink) = &self.sink { sink.terminal_response(); }
            }
        }
    }

    /// Pre-dispatch failure cleanup. Never attests upstream cancellation.
    pub async fn finish_empty(&self) {
        let entry = self.pending.lock().ok().and_then(|mut pending| {
            if pending.get(&self.key).is_some_and(|p| p.generation == self.generation) {
                pending.remove(&self.key)
            } else { None }
        });
        if let Some(entry) = entry {
            if let Some(read) = entry.read { let _ = read.finish(false).await; }
        }
    }
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
            pending_reads: Arc::new(Mutex::new(HashMap::new())),
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

    /// Upstream EOF/write failure ends all correlations. Unknown completions
    /// keep their gateway reservations tainted.
    pub fn close_pending(&self) {
        if let Ok(mut pending) = self.pending_reads.lock() { pending.clear(); }
    }

    fn claim_call(&self, id: &Value) -> Result<PendingCall, String> {
        if !id.is_string() && !id.is_number() { return Err("Invalid in-flight request id".into()); }
        let key = id.to_string();
        if key.len() > 1024 { return Err("Request id is too long".into()); }
        let mut pending = self.pending_reads.lock().map_err(|_| "Pending calls unavailable")?;
        // Expired slots retain their gateway reservation, so a late payload
        // cannot make a previously private turn clean.
        pending.retain(|_, p| p.deadline > Instant::now());
        if pending.contains_key(&key) { return Err("Duplicate in-flight request id".into()); }
        if pending.len() >= MAX_PENDING_READS { return Err("Too many in-flight private reads".into()); }
        let generation = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = tokio::sync::oneshot::channel();
        let deadline = Instant::now() + PENDING_READ_TTL;
        pending.insert(key.clone(), PendingRead {
            generation: generation.clone(), deadline,
            read: None, source: "unclassified_external_tool".into(), done: tx,
        });
        let (cancel, _) = tokio::sync::watch::channel(true);
        Ok(PendingCall { pending: self.pending_reads.clone(), key, generation, deadline, done: Some(rx), sink: None, live: Arc::new(std::sync::atomic::AtomicBool::new(true)), cancel })
    }

    /// Atomic gateway query-and-reserve precedes approval and upstream execution.
    /// The snapshot excludes only this call's own reservation.
    pub async fn begin_call(&self, id: &Value, tool: &str, args: &Value) -> Result<PendingCall, String> {
        let mut call = self.claim_call(id)?;
        let label = format!("{}.{tool}", self.server);
        let source = duduclaw_core::truncate_bytes(&label, 256);
        if let Ok(mut pending) = self.pending_reads.lock() {
            if let Some(entry) = pending.get_mut(&call.key) { entry.source = source.into(); }
        }
        let taint = match crate::egress_client::reserve(&self.home, &self.agent_id, source).await {
            Ok((read, taint)) => {
                let mut pending = self.pending_reads.lock().map_err(|_| "Pending calls unavailable")?;
                let entry = pending.get_mut(&call.key).filter(|p| p.generation == call.generation)
                    .ok_or("Private read expired before dispatch")?;
                entry.read = Some(read);
                taint
            }
            Err(reason) => {
                crate::egress_client::reservation_failure(&self.home, &self.agent_id, tool, &reason)?;
                crate::egress_guard::Taint::unknown()
            }
        };
        let approved = match self.check_call_with_taint(tool, args, taint).await {
            Ok(approved) => approved,
            Err(reason) => {
                call.finish_empty().await;
                return Err(reason);
            }
        };
        if self.decide(tool).0 != ToolEffect::Read {
            let own_read = self.pending_reads.lock().map_err(|_| "Pending calls unavailable")?
                .get(&call.key).and_then(|entry| entry.read.as_ref().map(|read| read.id().to_string()));
            match crate::egress_client::SinkLease::acquire_id(&self.home, &self.agent_id, own_read.as_deref()).await {
                Ok((lease, taint)) => {
                    call.sink = Some(Arc::new(lease));
                    if let Err(reason) = crate::egress_gate::confirm_dispatch(&self.home, &self.agent_id, &label, true, taint, approved.as_ref()) {
                        call.finish_empty().await;
                        return Err(reason);
                    }
                }
                Err(reason) => {
                    if let Err(message) = crate::egress_client::reservation_failure(&self.home, &self.agent_id, tool, &reason) {
                        call.finish_empty().await;
                        return Err(message);
                    }
                    if let Err(message) = crate::egress_gate::confirm_dispatch(
                        &self.home, &self.agent_id, &label, true, crate::egress_guard::Taint::unknown(), approved.as_ref(),
                    ) {
                        call.finish_empty().await;
                        return Err(message);
                    }
                }
            }
        }
        if Instant::now() >= call.deadline {
            call.finish_empty().await;
            return Err("Private read expired before dispatch".into());
        }
        Ok(call)
    }

    pub async fn observe_response(&self, response: &mut Value) {
        if response.get("method").is_some()
            || (response.get("result").is_none() && response.get("error").is_none()) { return; }
        let Some(id) = response.get("id").cloned() else { return; };
        let entry = self.pending_reads.lock().ok().and_then(|mut p| p.remove(&id.to_string()));
        self.finish_response(response, id, entry).await;
    }

    /// Remap cancellation to the currently admitted call. This only forwards a
    /// request; it never attests that upstream cancellation has completed.
    pub(crate) fn bridge_cancel_id(&self, id: &Value) -> Option<Value> {
        self.pending_reads.lock().ok()?.get(&id.to_string())
            .filter(|entry| entry.deadline > Instant::now())
            .map(|entry| bridge_wire_id(&entry.generation))
    }

    /// Return false for stale bridge responses so they cannot be delivered as
    /// the answer to a newer call. Lookup and removal share the same lock.
    pub(crate) async fn observe_bridge_response(&self, response: &mut Value) -> bool {
        if response.get("method").is_some()
            || (response.get("result").is_none() && response.get("error").is_none()) { return true; }
        let Some(generation) = response.get("id").and_then(bridge_wire_generation) else { return true; };
        if uuid::Uuid::parse_str(generation).is_err() { return false; }
        let entry = self.pending_reads.lock().ok().and_then(|mut pending| {
            let key = pending.iter().find(|(_, entry)| entry.generation == generation
                && entry.deadline > Instant::now()).map(|(key, _)| key.clone())?;
            let entry = pending.remove(&key)?;
            Some((key, entry))
        });
        let Some((key, entry)) = entry else { return false; };
        let Ok(id) = serde_json::from_str::<Value>(&key) else { return false; };
        response["id"] = id.clone();
        self.finish_response(response, id, Some(entry)).await;
        true
    }

    async fn finish_response(&self, response: &mut Value, id: Value, entry: Option<PendingRead>) {
        if let Some(entry) = entry {
            // Every correlated third-party response is model-visible, even
            // isError, error.data, or a server-controlled error message.
            let marked = if let Some(read) = entry.read {
                read.finish(true).await
            } else {
                // Shadow may have admitted while the gateway was unreachable.
                crate::egress_client::mark(&self.home, &self.agent_id, &entry.source).await
            };
            if let Err(reason) = marked {
                if let Err(message) = crate::egress_client::reservation_failure(&self.home, &self.agent_id, "third_party_response", &reason) {
                    *response = json!({"jsonrpc":"2.0", "id":id, "error": {
                        "code": REFUSED_CODE, "message": message
                    }});
                }
            }
            let _ = entry.done.send(());
        }
    }

    /// Checks the exact arguments that will leave the proxy after restoration.
    pub async fn check_call_with_args(&self, tool: &str, args: &Value) -> Result<(), String> {
        let (effect, decision) = self.decide(tool);
        if matches!(decision, ThirdPartyDecision::Block(_)) {
            return Err(self.check_call(tool).await.err()
                .unwrap_or_else(|| "Tool policy changed; retry the call.".into()));
        }
        let label = format!("{}.{tool}", self.server);
        let egress = crate::egress_gate::check(&self.home, &self.agent_id, &label, effect != ToolEffect::Read).await?;
        if decision == ThirdPartyDecision::Ask || egress.is_some() {
            self.ask_bound(tool, effect, args, egress).await
        } else { Ok(()) }
    }

    pub(crate) async fn check_call_with_taint(
        &self, tool: &str, args: &Value, taint: crate::egress_guard::Taint,
    ) -> Result<Option<Value>, String> {
        let (effect, decision) = self.decide(tool);
        if matches!(decision, ThirdPartyDecision::Block(_)) {
            return Err(self.check_call(tool).await.err()
                .unwrap_or_else(|| "Tool policy changed; retry the call.".into()));
        }
        let label = format!("{}.{tool}", self.server);
        let egress = crate::egress_gate::evaluate(&self.home, &self.agent_id, &label, effect != ToolEffect::Read, taint)?;
        if decision == ThirdPartyDecision::Ask || egress.is_some() {
            self.ask_bound(tool, effect, args, egress.clone()).await?;
        }
        Ok(egress)
    }

    async fn ask_bound(&self, tool: &str, effect: ToolEffect, args: &Value, egress: Option<Value>) -> Result<(), String> {
        use crate::approval::{ApprovalBroker, ApprovalStatus};
        use crate::approval::action_grant::{ActionGrant, ACTION_GRANT_KEY, verify_and_consume};
        let label = format!("{}.{tool}", self.server);
        let broker = ApprovalBroker::open(&self.home).map_err(|e| format!("Approval unavailable: {e}"))?;
        let mut payload = duduclaw_core::with_host_task_id(json!({
            "name": label, "tool": label, "server": self.server, "effect": effect.as_str(),
            "third_party": true, "arguments": args,
            "reason": if egress.is_some() { "egress_guard" } else { "action_rule" }
        }), duduclaw_core::host_task_id().as_deref());
        if let Some(reason) = egress { payload["egress_guard"] = reason; }
        payload["action_rule_matches"] = json!(self.policy().rules.approval_matches_for_call(
            &duduclaw_core::tool_effect::third_party_tool_ref(&self.server, tool), effect, args,
        ));
        let mut grant = ActionGrant::build(&self.agent_id, &label, &payload);
        grant.effect = effect;
        payload[ACTION_GRANT_KEY] = grant.to_json();
        let summary = format!("工具「{label}」需要管理員確認完整內容與目的地（{}）。", payload["reason"].as_str().unwrap_or("action_rule"));
        let id = broker.request(&self.agent_id, "mcp_call", &summary, payload, APPROVAL_TTL_SECONDS).await?;
        match broker.await_decision(&id, APPROVAL_POLL).await {
            Ok(ApprovalStatus::Approved) => verify_and_consume(&broker, &id, &grant).await
                .map_err(|e| format!("Approval binding refused: {e:?}")),
            _ => Err(format!("工具「{label}」未獲核准，已拒絕。")),
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
    async fn third_party_dispatch_permit_keeps_fence_after_local_termination() {
        let home = home_with_rules("");
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='block'\n").unwrap();
        for case in ["not_dispatched", "terminal", "timeout", "cancel", "partial_write", "eof"] {
            let agent = format!("lease-{}", uuid::Uuid::new_v4());
            let agent_dir = home.path().join("agents").join(&agent);
            std::fs::create_dir_all(&agent_dir).unwrap();
            std::fs::write(agent_dir.join("agent.toml"), "").unwrap();
            let gate = ThirdPartyGate::new(home.path(), &agent, "crm", "proxy");
            let turn = uuid::Uuid::new_v4().to_string();
            crate::egress_guard::register_turn(&agent, &turn);
            let mut call = gate.claim_call(&json!(case)).unwrap();
            let (lease, _) = crate::egress_client::SinkLease::direct(home.path(), &agent, Some(&turn), None).unwrap();
            call.sink = Some(Arc::new(lease));
            let permit = call.dispatch_permit();
            if case != "not_dispatched" { permit.dispatched(); }
            match case {
                "terminal" => {
                    let mut reply = json!({"id":case,"error":{"code":-1,"message":"terminal"}});
                    gate.observe_response(&mut reply).await;
                    call.wait().await;
                }
                "timeout" => { call.deadline = Instant::now(); call.wait().await; }
                "eof" => { gate.close_pending(); call.wait().await; }
                "partial_write" => { call.finish_empty().await; drop(call); }
                _ => drop(call),
            }
            drop(permit);
            let refused = crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "next").is_err();
            assert_eq!(refused, !matches!(case, "not_dispatched" | "terminal"), "{case}");
        }
    }

    #[tokio::test]
    async fn third_party_bridge_normal_response_ids_pass_through_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let gate = ThirdPartyGate::new(home.path(), "bot", "crm", "bridge");
        for id in [json!(1), json!("initialize"), json!("duduclaw-egress"), json!("other:123"), json!("duduclaw-egress-other:123")] {
            assert!(!bridge_reserved_id(&id));
            let mut response = json!({"jsonrpc":"2.0","id":id,"result":{}});
            let before = response.clone();
            assert!(gate.observe_bridge_response(&mut response).await);
            assert_eq!(response, before);
        }
        for id in [json!("duduclaw-egress:"), json!("duduclaw-egress:invalid"), bridge_wire_id(&uuid::Uuid::new_v4().to_string())] {
            assert!(bridge_reserved_id(&id));
            let mut response = json!({"id":id,"result":{}});
            assert!(!gate.observe_bridge_response(&mut response).await);
        }
    }

    #[tokio::test]
    async fn third_party_bridge_late_generation_cannot_finish_reused_id_sink() {
        let home = tempfile::tempdir().unwrap();
        let agent = format!("bridge-{}", uuid::Uuid::new_v4());
        let dir = home.path().join("agents").join(&agent);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\negress_guard='block'\n").unwrap();
        let gate = ThirdPartyGate::new(home.path(), &agent, "crm", "bridge");
        let id = json!("reused");
        let old = gate.claim_call(&id).unwrap();
        let old_wire = old.bridge_wire_id();
        gate.pending_reads.lock().unwrap().get_mut(&id.to_string()).unwrap().deadline = Instant::now();
        let mut current = gate.claim_call(&id).unwrap();
        let current_wire = current.bridge_wire_id();
        assert_ne!(old_wire, current_wire);
        drop(old);
        assert_eq!(gate.bridge_cancel_id(&id), Some(current_wire.clone()));
        let turn = uuid::Uuid::new_v4().to_string();
        crate::egress_guard::register_turn(&agent, &turn);
        let (lease, _) = crate::egress_client::SinkLease::direct(home.path(), &agent, Some(&turn), None).unwrap();
        current.sink = Some(Arc::new(lease));
        current.dispatch_permit().dispatched();

        let mut late = json!({"id":old_wire,"result":{"content":[]}});
        assert!(!gate.observe_bridge_response(&mut late).await);
        // A server replying with the reusable caller ID cannot bypass the
        // generation correlation either.
        let mut wrong_id = json!({"id":id,"result":{}});
        assert!(gate.observe_bridge_response(&mut wrong_id).await);
        assert!(matches!(current.done.as_mut().unwrap().try_recv(), Err(tokio::sync::oneshot::error::TryRecvError::Empty)));
        assert_eq!(gate.pending_reads.lock().unwrap().len(), 1);
        assert!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "next").is_err());

        let mut terminal = json!({"id":current_wire,"error":{"code":-1,"message":"done"}});
        assert!(gate.observe_bridge_response(&mut terminal).await);
        assert_eq!(terminal["id"], id);
        current.wait().await;
        assert!(crate::egress_guard::reserve_turn(home.path(), &agent, Some(&turn), "memory_search", "next").is_ok());
        assert!(!gate.observe_bridge_response(&mut late).await);
        assert!(gate.bridge_cancel_id(&id).is_none());
    }

    #[tokio::test]
    async fn third_party_pending_cap_duplicates_deadline_and_terminal_cleanup() {
        let home = tempfile::tempdir().unwrap();
        let gate = ThirdPartyGate::new(home.path(), "bot", "crm", "proxy");
        let mut calls = Vec::new();
        for id in 0..MAX_PENDING_READS { calls.push(gate.claim_call(&json!(id)).unwrap()); }
        assert!(gate.claim_call(&json!(0)).is_err());
        assert!(gate.claim_call(&json!(MAX_PENDING_READS)).is_err());
        drop(calls.pop());
        let call = gate.claim_call(&json!("new")).unwrap();
        let permit = call.dispatch_permit();
        assert!(permit.is_live());
        call.finish_empty().await; // Confirmed pre-dispatch terminal failure.
        assert_eq!(gate.pending_reads.lock().unwrap().len(), MAX_PENDING_READS - 1);
        drop(call);
        assert!(!permit.is_live());
        let mut permit = permit;
        tokio::time::timeout(Duration::from_millis(100), permit.cancelled()).await.unwrap();
        let mut expiring = calls[0].dispatch_permit();
        expiring.deadline = Instant::now();
        tokio::time::timeout(Duration::from_millis(100), expiring.cancelled()).await.unwrap();
        drop(calls);
        assert!(gate.pending_reads.lock().unwrap().is_empty());
        let old = gate.claim_call(&json!("expired")).unwrap();
        gate.pending_reads.lock().unwrap().get_mut(&json!("expired").to_string()).unwrap().deadline = Instant::now();
        let replacement = gate.claim_call(&json!("expired")).unwrap();
        drop(old);
        assert_eq!(gate.pending_reads.lock().unwrap().len(), 1, "old guard cannot remove a replacement");
        drop(replacement);
        for mut response in [
            json!({"id":1,"result":{"isError":true,"content":[{"text":"private"}]}}),
            json!({"id":1,"error":{"code":-1,"data":{"private":42}}}),
        ] {
            let call = gate.claim_call(&json!(1)).unwrap();
            gate.observe_response(&mut response).await;
            assert!(gate.pending_reads.lock().unwrap().is_empty());
            call.wait().await;
        }
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
