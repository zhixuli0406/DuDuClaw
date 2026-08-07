//! WP21 choke point **C1** — the delegation gate on the bus-consumption path.
//!
//! Design doc `commercial/docs/design-a2a-dept-rank-isolation.md` §2.4: the
//! front-door MCP checks (`send_to_agent` / `spawn_agent`) only produce a nicer
//! error message; the rule has to be enforced *where the work actually starts*.
//! For the dispatcher that is the moment a queued task is about to become a
//! spawned CLI process — reached by `spawn_agent`, `spawn_ephemeral`, ACP
//! `message/send`, autopilot, the goal loop, and by anyone who can append a
//! line to `bus_queue.jsonl`. Before this module, none of those were checked.
//!
//! The decision itself lives in `duduclaw_core::delegation_policy`
//! ([`can_delegate_ext`]); this module only supplies the two things the
//! predicate needs from the gateway — an [`OrgView`] over the agent registry
//! and the effective policy — plus the DENY consequences: fail the task, write
//! the reason back, record an audit event, never spawn.
//!
//! ## Sender resolution and the v1.52 legacy window
//!
//! The sender is `sender_agent`, falling back to `origin_agent`. A task
//! carrying *neither* predates this field pair (or came from a producer that
//! never stamped it); it is logged with a WARN and **allowed** for one release
//! so an upgrade does not strand in-flight work.
//!
//! **v1.53 (planned): the legacy branch becomes a DENY.** As of v1.52 every
//! in-tree producer stamps a sender, so the flip is unblocked — the branch now
//! only covers rows written before the upgrade and hand-written queue lines.
//! Inventory (re-verified in the v1.52 debt-wave review):
//!
//! - `mcp.rs` `send_to_agent` / `spawn_agent` — caller / parent agent id.
//! - `acp/message_send.rs` — [`duduclaw_core::ACP_CLIENT_SENDER`].
//! - `handlers.rs` (dashboard) / `webhook.rs` / `autopilot_engine.rs` /
//!   `goal_loop.rs` — the matching [`duduclaw_core::SYSTEM_SENDERS`] spelling.
//! - `dispatcher.rs` re-queues — the originating agent, or
//!   `__deferred_gvu__` ([`GATEWAY_INTERNAL_SENDERS`]).
//! - `duduclaw-agent/src/heartbeat.rs` `poll_assigned_tasks` — **fixed**: both
//!   INSERTs now stamp `sender_agent`/`origin_agent = "heartbeat"` (the
//!   `SYSTEM_SENDERS` spelling; the `sender` column keeps its legacy
//!   `heartbeat-scheduler` value for existing queries/UI).
//!
//! Before flipping, re-run that inventory (`rg 'sender_agent'` over the
//! producers) — a new producer added after this note would otherwise start
//! being denied. Until it flips, note what the branch costs: a hand-written
//! `bus_queue.jsonl` line that simply *omits* `sender_agent` is allowed. The
//! gate stops forged senders, not absent ones.
//!
//! ## Why the org view is registry-first, then lazy file reads
//!
//! The registry is already in memory and is authoritative for the `name → config`
//! mapping (agent ids are `[agent] name`, which need not equal the directory
//! name). But it deliberately does not contain **ephemeral** agents
//! (`agents/.ephemeral/eph-*`, see `crate::ephemeral`), and those are dispatched
//! through this same path — so a registry miss falls back to reading that one
//! agent's `agent.toml`. Reads are per-id, cached for the duration of a single
//! decision, and bounded by the predicate's 20-hop ceiling: a decision touches
//! the two agents' ancestor chains, never the whole `agents/` directory.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::RwLock;
use tracing::warn;

use duduclaw_agent::registry::AgentRegistry;
use duduclaw_core::delegation_policy::delegation_rules_from_table;
use duduclaw_core::{
    DelegationDenied, OrgNode, OrgView, can_delegate_rules_ext, is_valid_agent_id, truncate_chars,
};
use duduclaw_security::audit::{AuditEvent, Severity, append_audit_event};

/// Senders that are internal to the gateway itself rather than agents.
///
/// [`duduclaw_core::SYSTEM_SENDERS`] covers the human / operator-configured
/// interfaces (dashboard, webhook, cron, heartbeat, autopilot, goal-loop-driver).
/// `__deferred_gvu__` is different in kind: it is the dispatcher re-queueing an
/// agent's *own* deferred GVU retry to itself (see `poll_deferred_gvu`), a
/// synthetic id that never crosses an org boundary. It is listed here — not in
/// core — because it is a gateway implementation detail, and passing it through
/// [`can_delegate_ext`] keeps the core predicate unchanged (design doc §2.3
/// leaves the constant set closed).
const GATEWAY_INTERNAL_SENDERS: [&str; 1] = ["__deferred_gvu__"];

/// Read + parse `<home>/config.toml` once.
///
/// WP21 欠帳⑤: `gate_bus_dispatch` needs two independent sections out of the
/// same file — `[delegation]` (via `delegation_rules_from_table`) and `[acp]
/// trusted` (via `acp_trusted_from_table`). Before this helper each decision
/// read and re-parsed the file twice (once per section); this reads it once
/// and lets both extractors work off the same in-memory `toml::Table`. The
/// per-message re-read semantics are unchanged — this is called fresh on
/// every `gate_bus_dispatch` call, so a config write still takes effect on
/// the very next message, exactly as before.
///
/// A missing file or a file that fails to parse as TOML resolves to an empty
/// table — same fail-closed default every extractor here already assumed
/// (missing/malformed `[delegation]` ⇒ department policy + empty whitelist;
/// missing/malformed `[acp]` ⇒ `trusted = false`).
fn read_config_table(home_dir: &Path) -> toml::Table {
    let path = home_dir.join("config.toml");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|content| content.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

/// `config.toml [acp] trusted` — WP21 T7, design doc §2.6.
///
/// Default `false`: an inbound ACP `message/send` call stamps
/// `sender_agent = "a2a-client"` (`duduclaw_core::ACP_CLIENT_SENDER`), which is
/// deliberately absent from [`duduclaw_core::SYSTEM_SENDERS`] — so by default
/// this gate denies every A2A-originated delegation (fail-closed, per the
/// module docs). Setting `trusted = true` adds `ACP_CLIENT_SENDER` to the
/// extra-system-senders list passed to `can_delegate_rules_ext`, which grants
/// it the *same reach as a system sender*: dispatch to any agent in this
/// home, bypassing the org-tree hierarchy/department checks entirely.
///
/// **Risk**: turning this on trusts *every* client able to reach the ACP
/// stdio endpoint (design doc §2.6 notes inbound HTTP/bearer auth for
/// `message/send` is explicitly out of scope for this WP — stdio's process
/// boundary is the only authentication today). Only flip this on when the ACP
/// endpoint is wired to internal tooling you already trust with full
/// delegation reach.
///
/// Takes an already-parsed table (see [`read_config_table`]) rather than
/// reading the file itself — same convention as
/// [`DelegationConfig::from_home`](duduclaw_core::DelegationConfig::from_home)
/// and `DispatchGuardConfig::from_home` for the missing-file/missing-section
/// fallback: a missing `[acp]` section, or a wrong-typed `trusted` value (not
/// a bool), both resolve to `false`. Fail-closed — a malformed config section
/// must never silently widen delegation reach.
fn acp_trusted_from_table(table: &toml::Table) -> bool {
    table
        .get("acp")
        .and_then(|v| v.as_table())
        .and_then(|section| section.get("trusted"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

/// What the gate decided about one queued task.
#[derive(Debug)]
pub(crate) enum GateOutcome {
    /// Cleared — dispatch as usual.
    Allow,
    /// No `sender_agent` and no `origin_agent` (v1.52 transitional): allowed,
    /// already logged. Becomes a DENY in v1.53.
    LegacyNoSender,
    /// Refused. The caller must fail the task, write the reason back, and not
    /// spawn anything. The audit event has already been recorded.
    Deny(DelegationDenied),
}

// ── Org view ────────────────────────────────────────────────────────────────

/// [`OrgView`] over the gateway's registry with a lazy `agent.toml` fallback.
///
/// Lookups are memoised (misses included) in a [`RefCell`], so one decision
/// reads each agent at most once. Synchronous by construction — the predicate
/// is sync, and the reads are single small files already in the page cache.
pub(crate) struct DispatchOrgView<'a> {
    agents_dir: PathBuf,
    registry: Option<&'a AgentRegistry>,
    /// WP22 T1 — `<home>/org.toml`, read once per decision. Consulted *before*
    /// the registry and the `agent.toml` fallback: both of those ultimately
    /// derive from a file inside the agent's own working directory, and this
    /// one does not.
    store: duduclaw_core::OrgStore,
    cache: RefCell<HashMap<String, Option<OrgNode>>>,
}

impl<'a> DispatchOrgView<'a> {
    pub(crate) fn new(home_dir: &Path, registry: Option<&'a AgentRegistry>) -> Self {
        Self {
            agents_dir: home_dir.join("agents"),
            registry,
            store: duduclaw_core::org_store::load(home_dir),
            cache: RefCell::new(HashMap::new()),
        }
    }

    /// Canonicalise an id before it reaches the predicate: `"default"` is an
    /// alias the dispatcher resolves to the `Main`-role agent (see
    /// `dispatch_to_agent`), and comparing the alias against real ids would
    /// both miss self-delegation and walk the wrong chain.
    pub(crate) fn canonical_id(&self, id: &str) -> String {
        let id = id.trim();
        if id == "default" {
            if let Some(main) = self.registry.and_then(|r| r.main_agent()) {
                return main.config.agent.name.clone();
            }
        }
        id.to_string()
    }

    fn node(&self, agent_id: &str) -> Option<OrgNode> {
        let key = agent_id.trim().to_string();
        if let Some(hit) = self.cache.borrow().get(&key) {
            return hit.clone();
        }
        let resolved = self.load(&key);
        self.cache.borrow_mut().insert(key, resolved.clone());
        resolved
    }

    fn load(&self, agent_id: &str) -> Option<OrgNode> {
        // WP22 T1 — the authoritative store wins outright when it has a record
        // for this agent. Only agents it has *no* record for (pre-WP22 homes,
        // hand-created agents, anything added between seeding and the next
        // `duduclaw org sync`) fall through to the registry / `agent.toml`
        // pair below, which is byte-for-byte the pre-WP22 behaviour.
        if let Some(entry) = self.store.get(agent_id) {
            return Some(OrgNode {
                reports_to: entry.reports_to.clone(),
                department: entry.department.clone(),
            });
        }
        // Registry first — no I/O, and authoritative for name → config.
        if let Some(reg) = self.registry {
            if let Some(agent) = reg.get(agent_id) {
                return Some(OrgNode {
                    reports_to: agent.config.agent.reports_to.clone(),
                    department: agent.config.agent.department.clone(),
                });
            }
        }
        // Fallback: this one agent's `agent.toml`. The id is attacker-supplied
        // (anyone who can append to `bus_queue.jsonl` picks it), so it is
        // charset-validated before it is ever joined onto a path — no `..`, no
        // separators (project convention: never build a path from raw input).
        if !is_valid_agent_id(agent_id) {
            return None;
        }
        let direct = self.agents_dir.join(agent_id).join("agent.toml");
        let path = if direct.exists() {
            direct
        } else if crate::ephemeral::is_ephemeral_id(agent_id) {
            // Ephemeral scaffolds are invisible to the registry by design but
            // do carry `reports_to = <parent>`, which is exactly the relation
            // that authorises their dispatch.
            self.agents_dir
                .join(crate::ephemeral::EPHEMERAL_DIR_NAME)
                .join(agent_id)
                .join("agent.toml")
        } else {
            return None;
        };
        parse_org_node(&path)
    }
}

/// Read `[agent] reports_to` / `department` out of an `agent.toml`.
///
/// Parsed as a generic `toml::Table` rather than `AgentConfig`: a partially
/// written or forward-versioned file must still yield the org fields it does
/// have instead of failing the whole lookup (and a failed lookup means DENY).
fn parse_org_node(path: &Path) -> Option<OrgNode> {
    let content = std::fs::read_to_string(path).ok()?;
    let table = content.parse::<toml::Table>().ok()?;
    let agent = table.get("agent")?.as_table()?;
    let field = |k: &str| {
        agent
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    Some(OrgNode {
        reports_to: field("reports_to"),
        department: field("department"),
    })
}

impl OrgView for DispatchOrgView<'_> {
    fn reports_to(&self, agent_id: &str) -> Option<String> {
        self.node(agent_id).map(|n| n.reports_to)
    }

    fn department(&self, agent_id: &str) -> Option<String> {
        self.node(agent_id).map(|n| n.department)
    }
}

// ── The gate ────────────────────────────────────────────────────────────────

/// Pick the sender to judge: `sender_agent`, else `origin_agent`, else nothing.
fn resolve_sender<'a>(
    sender_agent: Option<&'a str>,
    origin_agent: Option<&'a str>,
) -> Option<&'a str> {
    [sender_agent, origin_agent]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|s| !s.is_empty())
}

/// Evaluate one queued task against the delegation policy.
///
/// `path_kind` names the rail for the audit record (`bus_dispatch` for
/// `bus_queue.jsonl`, `sqlite_queue` for `message_queue.db`); `task_id` is the
/// message id, recorded so a denial can be traced back to a concrete task.
///
/// Denials are logged and audited here; the caller owns the task-status
/// write-back because the two rails record failure differently.
pub(crate) async fn gate_bus_dispatch(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    sender_agent: Option<&str>,
    origin_agent: Option<&str>,
    target: &str,
    task_id: &str,
    path_kind: &str,
) -> GateOutcome {
    let Some(sender) = resolve_sender(sender_agent, origin_agent) else {
        // v1.52 transitional window — see the module docs. Loud, not silent:
        // this line is the inventory of producers that must be fixed before
        // the branch turns into a DENY.
        warn!(
            task_id = %truncate_chars(task_id, 64),
            target = %truncate_chars(target, 64),
            path_kind,
            "Bus task carries neither sender_agent nor origin_agent — delegation \
             check skipped (legacy task, allowed in v1.52; will be DENIED from v1.53)"
        );
        return GateOutcome::LegacyNoSender;
    };

    // WP21 欠帳⑤: one read + one parse of config.toml serves both extractors
    // below — previously `delegation_rules_from_home` and
    // `acp_trusted_from_home` each re-read and re-parsed the whole file.
    let config_table = read_config_table(home_dir);
    let rules = delegation_rules_from_table(&config_table);
    // WP21 T7: `[acp] trusted = true` widens the extra-system-senders list for
    // this decision only — see `acp_trusted_from_table` for the risk this opts
    // into. Default (false) leaves `GATEWAY_INTERNAL_SENDERS` untouched, which
    // is exactly pre-T7 behaviour (`a2a-client` stays denied).
    let mut extra_senders: Vec<&str> = GATEWAY_INTERNAL_SENDERS.to_vec();
    if acp_trusted_from_table(&config_table) {
        extra_senders.push(duduclaw_core::ACP_CLIENT_SENDER);
    }
    let guard = registry.read().await;
    let view = DispatchOrgView::new(home_dir, Some(&guard));
    let sender_id = view.canonical_id(sender);
    let target_id = view.canonical_id(target);
    let decision = can_delegate_rules_ext(&rules, &view, &sender_id, &target_id, &extra_senders);
    drop(guard);

    match decision {
        Ok(()) => GateOutcome::Allow,
        Err(denied) => {
            warn!(
                task_id = %truncate_chars(task_id, 64),
                sender = %truncate_chars(&denied.sender, 64),
                target = %truncate_chars(&denied.target, 64),
                reason = denied.reason.as_str(),
                policy = denied.policy.as_str(),
                path_kind,
                "Delegation denied — task failed, no agent spawned (WP21 C1)"
            );
            audit_denial(home_dir, &denied, task_id, path_kind);
            GateOutcome::Deny(denied)
        }
    }
}

/// Append a `delegation_denied` event to `security_audit.jsonl`.
///
/// Uses the project's existing audit writer, which takes an exclusive advisory
/// lock around the append (cross-process safety, project convention #3).
fn audit_denial(home_dir: &Path, denied: &DelegationDenied, task_id: &str, path_kind: &str) {
    let event = AuditEvent::new(
        "delegation_denied",
        truncate_chars(&denied.target, 64),
        Severity::Warning,
        serde_json::json!({
            "sender": truncate_chars(&denied.sender, 64),
            "target": truncate_chars(&denied.target, 64),
            "reason": denied.reason.as_str(),
            "policy": denied.policy.as_str(),
            "path_kind": path_kind,
            "task_id": truncate_chars(task_id, 64),
            "message": denied.message_zh(),
        }),
    );
    append_audit_event(home_dir, &event);
}

/// zh-TW text written back to the task / forwarded to the requester when a
/// delegation is refused.
///
/// Deliberately **not** [`DelegationDenied::message_zh()`] — that text names
/// `config.toml [delegation] policy`, `reports_to`, `agent.toml`, and the
/// three exact ways to reconfigure the rule. That is operator vocabulary for
/// this AI Agent Platform's own admin console; it must never reach a Telegram
/// / Discord / Slack / LINE / ... end user through `forward_delegation_response`
/// or a task-status write-back (project convention: internal implementation
/// details — file names, config keys, technical terms — do not leak into
/// user-facing surfaces). This is the short, human sentence for that path.
/// The full `message_zh()` text, reason code, and policy stay reserved for
/// `audit_denial`'s `security_audit.jsonl` record and the `warn!` log line in
/// [`gate_bus_dispatch`] — both operator-only surfaces, unchanged.
pub(crate) fn denial_notice(denied: &DelegationDenied) -> String {
    let s = truncate_chars(denied.sender.trim(), 64);
    let t = truncate_chars(denied.target.trim(), 64);
    match denied.reason {
        duduclaw_core::DelegationDenyReason::SelfDelegation => {
            "⛔ 這項委派未執行：AI 員工不能把工作指派給自己。".to_string()
        }
        duduclaw_core::DelegationDenyReason::InvalidAgentId => {
            "⛔ 這項委派未執行：委派對象資訊不完整，請確認後重試。".to_string()
        }
        _ => format!(
            "⛔ 這項委派未執行：「{s}」沒有指派工作給「{t}」的權限。\
             如需開通，請管理員到儀表板「委派權限」設定。"
        ),
    }
}

/// Effective policy for this home — re-exported for call sites that need to
/// report it without repeating the config lookup shape.
#[cfg(test)]
pub(crate) fn effective_policy(home_dir: &Path) -> duduclaw_core::DelegationPolicy {
    duduclaw_core::delegation_policy_from_home(home_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::{DelegationDenyReason, DelegationPolicy};

    /// ceo ─ sales_lead ─ sales_rep / sales_rep2   (department 業務)
    ///     └ mkt_lead   ─ mkt_rep                  (department 行銷)
    fn seed_home() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let agents = tmp.path().join("agents");
        let write = |name: &str, reports_to: &str, department: &str| {
            let dir = agents.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("agent.toml"),
                format!(
                    "[agent]\nname = \"{name}\"\nrole = \"worker\"\n\
                     reports_to = \"{reports_to}\"\ndepartment = \"{department}\"\n"
                ),
            )
            .unwrap();
        };
        write("ceo", "", "");
        write("sales_lead", "ceo", "業務");
        write("sales_rep", "sales_lead", "業務");
        write("sales_rep2", "sales_lead", "業務");
        write("mkt_lead", "ceo", "行銷");
        write("mkt_rep", "mkt_lead", "行銷");
        tmp
    }

    fn registry_for(home: &Path) -> Arc<RwLock<AgentRegistry>> {
        // Unscanned: every lookup exercises the lazy `agent.toml` fallback,
        // which is the path ephemeral agents depend on.
        Arc::new(RwLock::new(AgentRegistry::new(home.join("agents"))))
    }

    /// `true` when the outcome lets the task proceed to a spawn.
    fn allows_dispatch(outcome: &GateOutcome) -> bool {
        !matches!(outcome, GateOutcome::Deny(_))
    }

    async fn gate(home: &Path, sender: Option<&str>, target: &str) -> GateOutcome {
        gate_bus_dispatch(
            home,
            &registry_for(home),
            sender,
            None,
            target,
            "task-1",
            "bus_dispatch",
        )
        .await
    }

    fn set_policy(home: &Path, policy: &str) {
        std::fs::write(
            home.join("config.toml"),
            format!("[delegation]\npolicy = \"{policy}\"\n"),
        )
        .unwrap();
    }

    fn audit_lines(home: &Path) -> Vec<serde_json::Value> {
        let path = home.join("security_audit.jsonl");
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// WP22 T1 — the C1 gate reads `<home>/org.toml` first.
    ///
    /// Same escalation as the MCP-side test: `sales_rep` rewrites its own
    /// `agent.toml` to claim the marketing department (and a marketing
    /// parent), which pre-WP22 would have cleared the department check. The
    /// seeded store is outside its workspace, so the decision does not move.
    #[tokio::test]
    async fn org_store_beats_a_tampered_agent_toml() {
        let tmp = seed_home();
        let home = tmp.path();
        duduclaw_core::org_store::seed_if_absent(home).unwrap();

        std::fs::write(
            home.join("agents").join("sales_rep").join("agent.toml"),
            "[agent]\nname = \"sales_rep\"\nrole = \"worker\"\n\
             reports_to = \"mkt_lead\"\ndepartment = \"行銷\"\n",
        )
        .unwrap();

        assert!(
            !allows_dispatch(&gate(home, Some("sales_rep"), "mkt_rep").await),
            "a self-rewritten agent.toml must not buy cross-department reach"
        );
        // The real relations still work — this is a redirect of authority, not
        // a lockout.
        assert!(allows_dispatch(&gate(home, Some("sales_rep"), "sales_rep2").await));
        assert!(allows_dispatch(&gate(home, Some("sales_lead"), "sales_rep").await));
    }

    /// The fallback half: agents with no store record keep resolving from
    /// their `agent.toml`, so an un-migrated home behaves exactly as before.
    #[tokio::test]
    async fn agents_without_a_store_entry_still_resolve_from_agent_toml() {
        let tmp = seed_home();
        let home = tmp.path();
        // Bootstrapped store, marketing branch hand-created afterwards (so it
        // never got a record) — the realistic shape of the fallback case.
        duduclaw_core::org_store::seed_if_absent(home).unwrap();
        for stray in ["mkt_lead", "mkt_rep"] {
            duduclaw_core::org_store::remove(home, stray).unwrap();
        }
        assert!(allows_dispatch(&gate(home, Some("mkt_lead"), "mkt_rep").await));
        assert!(!allows_dispatch(&gate(home, Some("mkt_rep"), "sales_rep").await));
    }

    #[tokio::test]
    async fn parent_grandparent_and_same_department_are_allowed() {
        let tmp = seed_home();
        let home = tmp.path();
        // Default policy (no config.toml at all) = department.
        assert_eq!(effective_policy(home), DelegationPolicy::Department);

        for (sender, target, what) in [
            ("sales_lead", "sales_rep", "parent → child"),
            ("ceo", "sales_rep", "grandparent → grandchild"),
            ("sales_rep", "ceo", "grandchild → grandparent"),
            ("sales_rep", "sales_rep2", "same-department peers"),
        ] {
            assert!(
                allows_dispatch(&gate(home, Some(sender), target).await),
                "{what} must be allowed"
            );
        }
        // Nothing allowed leaves an audit trail.
        assert!(audit_lines(home).is_empty());
    }

    #[tokio::test]
    async fn cross_department_stranger_is_denied_with_zh_reason_and_audit() {
        let tmp = seed_home();
        let home = tmp.path();

        let outcome = gate(home, Some("sales_rep"), "mkt_rep").await;
        let GateOutcome::Deny(denied) = outcome else {
            panic!("cross-department delegation must be denied");
        };
        assert!(!allows_dispatch(&GateOutcome::Deny(denied.clone())));
        assert_eq!(denied.reason, DelegationDenyReason::DifferentDepartment);

        // The channel/task-status notice is now the short human sentence —
        // no internal jargon, just who/what and where to fix it.
        let notice = denial_notice(&denied);
        assert!(
            notice.contains("sales_rep") && notice.contains("mkt_rep"),
            "notice should still name both parties: {notice}"
        );
        assert!(notice.contains("委派權限"), "should point at the dashboard tab: {notice}");
        for leaked_term in ["config.toml", "reports_to", "agent.toml", "policy", "委派遭拒"] {
            assert!(
                !notice.contains(leaked_term),
                "user-facing notice must not leak internal term '{leaked_term}': {notice}"
            );
        }

        // The full operator-facing explanation still lands in the audit trail.
        let events = audit_lines(home);
        assert_eq!(events.len(), 1, "exactly one audit event");
        let e = &events[0];
        assert_eq!(e["event_type"], "delegation_denied");
        assert_eq!(e["agent_id"], "mkt_rep");
        assert_eq!(e["details"]["sender"], "sales_rep");
        assert_eq!(e["details"]["target"], "mkt_rep");
        assert_eq!(e["details"]["reason"], "different_department");
        assert_eq!(e["details"]["policy"], "department");
        assert_eq!(e["details"]["path_kind"], "bus_dispatch");
        assert_eq!(e["details"]["task_id"], "task-1");
        let audited_message = e["details"]["message"].as_str().unwrap_or("");
        assert!(
            audited_message.contains("委派遭拒") && audited_message.contains("config.toml"),
            "audit record must keep the full message_zh() detail: {audited_message}"
        );
    }

    #[tokio::test]
    async fn hierarchy_policy_drops_the_department_shortcut() {
        let tmp = seed_home();
        let home = tmp.path();
        set_policy(home, "hierarchy");

        assert!(allows_dispatch(&gate(home, Some("ceo"), "sales_rep").await));
        let outcome = gate(home, Some("sales_rep"), "sales_rep2").await;
        let GateOutcome::Deny(d) = outcome else {
            panic!("same-department peers must be denied under hierarchy");
        };
        assert_eq!(d.reason, DelegationDenyReason::NoHierarchyRelation);
    }

    /// WP21 T11: the C1 gate must read `[delegation] allow` too, not just
    /// `policy` — cross-department leads with an explicit whitelist entry
    /// get through even though neither ancestry nor same-department applies.
    #[tokio::test]
    async fn whitelist_pair_from_config_is_honored_bidirectionally() {
        let tmp = seed_home();
        let home = tmp.path();
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"department\"\nallow = [[\"sales_lead\", \"mkt_lead\"]]\n",
        )
        .unwrap();

        assert!(allows_dispatch(
            &gate(home, Some("sales_lead"), "mkt_lead").await
        ));
        assert!(allows_dispatch(
            &gate(home, Some("mkt_lead"), "sales_lead").await
        ));
        // A third party gets nothing from someone else's whitelist entry.
        let outcome = gate(home, Some("sales_rep"), "mkt_lead").await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    #[tokio::test]
    async fn system_senders_pass_and_a2a_client_does_not() {
        let tmp = seed_home();
        let home = tmp.path();

        for sender in [
            "dashboard",
            "goal-loop-driver",
            "cron",
            "webhook",
            "autopilot",
        ] {
            assert!(
                allows_dispatch(&gate(home, Some(sender), "mkt_rep").await),
                "{sender} must be allowed"
            );
        }
        // Gateway-internal deferred-GVU re-queue keeps working.
        assert!(allows_dispatch(
            &gate(home, Some("__deferred_gvu__"), "mkt_rep").await
        ));
        // An inbound A2A client is not a system sender (design doc §2.6 —
        // opening it up is T7's `[acp] trusted` switch, not this gate).
        let outcome = gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    #[tokio::test]
    async fn legacy_task_without_sender_warns_and_passes() {
        let tmp = seed_home();
        let home = tmp.path();

        assert!(matches!(
            gate(home, None, "mkt_rep").await,
            GateOutcome::LegacyNoSender
        ));
        // Blank strings count as absent, not as an unknown agent.
        assert!(matches!(
            gate_bus_dispatch(
                home,
                &registry_for(home),
                Some("  "),
                Some(""),
                "mkt_rep",
                "task-legacy",
                "bus_dispatch",
            )
            .await,
            GateOutcome::LegacyNoSender
        ));
        // A legacy pass is not a denial, so nothing is audited.
        assert!(audit_lines(home).is_empty());
        // ...but origin_agent alone is enough to be judged.
        let outcome = gate_bus_dispatch(
            home,
            &registry_for(home),
            None,
            Some("sales_rep"),
            "mkt_rep",
            "task-origin",
            "bus_dispatch",
        )
        .await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    #[tokio::test]
    async fn open_policy_restores_pre_wp21_behaviour() {
        let tmp = seed_home();
        let home = tmp.path();
        set_policy(home, "open");

        assert!(allows_dispatch(
            &gate(home, Some("sales_rep"), "mkt_rep").await
        ));
        assert!(allows_dispatch(&gate(home, Some("ghost"), "mkt_rep").await));
        assert!(allows_dispatch(
            &gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await
        ));
        assert!(audit_lines(home).is_empty());
    }

    #[tokio::test]
    async fn unknown_and_traversal_ids_fail_closed() {
        let tmp = seed_home();
        let home = tmp.path();

        // Unproven relation ⇒ DENY (never "unknown ⇒ allow").
        assert!(matches!(
            gate(home, Some("ghost"), "mkt_rep").await,
            GateOutcome::Deny(_)
        ));
        // A path-shaped id must not escape `agents/` — it simply resolves to
        // nothing and is denied.
        assert!(matches!(
            gate(home, Some("../../etc"), "mkt_rep").await,
            GateOutcome::Deny(_)
        ));
    }

    #[tokio::test]
    async fn ephemeral_target_is_authorised_by_its_parent_link() {
        let tmp = seed_home();
        let home = tmp.path();
        let eph_id = "eph-abc123def456";
        let dir = home
            .join("agents")
            .join(crate::ephemeral::EPHEMERAL_DIR_NAME)
            .join(eph_id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!(
                "[agent]\nname = \"{eph_id}\"\nrole = \"worker\"\nreports_to = \"sales_rep\"\n"
            ),
        )
        .unwrap();

        // Parent spawning its own ephemeral worker: allowed.
        assert!(allows_dispatch(
            &gate(home, Some("sales_rep"), eph_id).await
        ));
        // Someone else's ephemeral worker: denied.
        assert!(matches!(
            gate(home, Some("mkt_rep"), eph_id).await,
            GateOutcome::Deny(_)
        ));
    }

    #[test]
    fn malformed_agent_toml_yields_no_relation_rather_than_a_panic() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("agents").join("broken");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "not = valid = toml").unwrap();
        let view = DispatchOrgView::new(tmp.path(), None);
        assert!(view.reports_to("broken").is_none());
        assert!(view.department("missing").is_none());

        // Partial file: the fields that exist are still read.
        let dir2 = tmp.path().join("agents").join("partial");
        std::fs::create_dir_all(&dir2).unwrap();
        std::fs::write(dir2.join("agent.toml"), "[agent]\nname = \"partial\"\n").unwrap();
        assert_eq!(view.reports_to("partial").as_deref(), Some(""));
    }

    /// WP21 T7: default (no `[acp]` section at all) — `a2a-client` stays
    /// denied, matching the pre-T7 `system_senders_pass_and_a2a_client_does_not`
    /// expectation.
    #[tokio::test]
    async fn acp_untrusted_by_default_denies_a2a_client() {
        let tmp = seed_home();
        let home = tmp.path();

        let outcome = gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    /// A malformed `trusted` value (wrong type) must fail closed, same as an
    /// absent section — never silently trust.
    #[tokio::test]
    async fn acp_malformed_trusted_value_fails_closed() {
        let tmp = seed_home();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), "[acp]\ntrusted = \"yes\"\n").unwrap();

        let outcome = gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    /// `[acp] trusted = true` opens the gate for `a2a-client` specifically.
    #[tokio::test]
    async fn acp_trusted_true_allows_a2a_client() {
        let tmp = seed_home();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), "[acp]\ntrusted = true\n").unwrap();

        assert!(allows_dispatch(
            &gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await
        ));
    }

    /// `[acp] trusted = true` must not widen anything else — an unrelated
    /// stranger sender is still denied under the org-tree rules.
    #[tokio::test]
    async fn acp_trusted_true_does_not_widen_other_strangers() {
        let tmp = seed_home();
        let home = tmp.path();
        std::fs::write(home.join("config.toml"), "[acp]\ntrusted = true\n").unwrap();

        let outcome = gate(home, Some("ghost"), "mkt_rep").await;
        assert!(matches!(outcome, GateOutcome::Deny(_)));
    }

    #[test]
    fn sender_resolution_prefers_sender_agent() {
        assert_eq!(resolve_sender(Some("a"), Some("b")), Some("a"));
        assert_eq!(resolve_sender(None, Some("b")), Some("b"));
        assert_eq!(resolve_sender(Some(""), Some("b")), Some("b"));
        assert_eq!(resolve_sender(Some(" "), None), None);
        assert_eq!(resolve_sender(None, None), None);
    }
}
