//! WP21 choke point **C1** — the delegation gate on the bus-consumption path.
//!
//! Design doc `commercial/docs/specs/design-a2a-dept-rank-isolation.md` §2.4: the
//! front-door MCP checks (`send_to_agent` / `spawn_agent`) only produce a nicer
//! error message; the rule has to be enforced *where the work actually starts*.
//! For the dispatcher that is the moment a queued task is about to become a
//! spawned CLI process — reached by `spawn_agent`, `spawn_ephemeral`, ACP
//! `message/send`, autopilot, the goal loop, and by anyone who can append a
//! line to `bus_queue.jsonl`. Before this module, none of those were checked.
//!
//! The decision itself lives in `duduclaw_core::delegation_policy`
//! ([`evaluate_delegation`]); this module only supplies the two things the
//! predicate needs from the gateway — an [`OrgView`] over the agent registry
//! and the effective policy — plus the DENY consequences: fail the task, write
//! the reason back, record an audit event, never spawn.
//!
//! ## Sender resolution
//!
//! `sender_agent`, falling back to `origin_agent`, is required on both queue
//! rails. Missing or blank senders are refused and audited. Principal-free
//! work targeting Main/default additionally requires an established system
//! sender; employee delegation cannot manufacture Background authority.
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
use duduclaw_core::delegation_policy::{delegation_config_read_from_table, DelegationConfigRead,
    evaluate_delegation, DelegationContext, DelegationBoundaries, DelegationAllowSource};
use duduclaw_core::{
    DelegationDenied, OrgNode, OrgView, is_valid_agent_id, truncate_chars,
};
use duduclaw_security::audit::{AuditEvent, Severity};

/// Senders that are internal to the gateway itself rather than agents.
///
/// [`duduclaw_core::SYSTEM_SENDERS`] covers the human / operator-configured
/// interfaces (dashboard, webhook, cron, heartbeat, autopilot, goal-loop-driver).
/// `__deferred_gvu__` is different in kind: it is the dispatcher re-queueing an
/// agent's *own* deferred GVU retry to itself (see `poll_deferred_gvu`), a
/// synthetic id that never crosses an org boundary. It is listed here — not in
/// core — because it is a gateway implementation detail, and passing it through
/// [`evaluate_delegation`] keeps the system-sender list explicit (design doc §2.3
/// leaves the constant set closed).
const GATEWAY_INTERNAL_SENDERS: [&str; 1] = ["__deferred_gvu__"];

pub(crate) fn is_background_system_sender(sender: &str) -> bool {
    duduclaw_core::is_system_sender(sender)
        || GATEWAY_INTERNAL_SENDERS.contains(&sender.trim())
}

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
/// A missing file resolves to an empty table (department policy, empty
/// whitelist, `[acp] trusted = false`, human_principal record). A file that
/// exists but cannot be read or parsed resolves to the fail-closed answer for
/// every extractor: delegation policy `none` (RM-137, v1.82 — it used to fall
/// back to `department`), `trusted = false`, human_principal enforce.
fn read_config_table(
    home_dir: &Path,
) -> (DelegationConfigRead, toml::Table, duduclaw_core::HumanPrincipalMode) {
    let path = home_dir.join("config.toml");
    let table = match std::fs::read_to_string(&path) {
        Ok(content) => content.parse::<toml::Table>().ok(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let table = toml::Table::new();
            return (
                delegation_config_read_from_table(&table),
                table,
                duduclaw_core::HumanPrincipalMode::Record,
            );
        }
        Err(_) => None,
    };
    let Some(table) = table else {
        warn!("config.toml unreadable or malformed; human_principal treated as enforce");
        duduclaw_core::delegation_policy::warn_policy_once(
            "config.toml 無法讀取或不是有效的 TOML;在修正前 AI 員工之間一律不能委派(視為 [delegation] policy = \"none\")。",
        );
        let mut denied_table = toml::Table::new();
        let mut section = toml::Table::new();
        section.insert("policy".into(), toml::Value::String("none".into()));
        denied_table.insert("delegation".into(), toml::Value::Table(section));
        return (delegation_config_read_from_table(&denied_table), toml::Table::new(), duduclaw_core::HumanPrincipalMode::Enforce);
    };
    let (mode, warning) = duduclaw_core::HumanPrincipalMode::from_table(&table);
    if let Some(warning) = warning {
        warn!("{warning}");
    }
    (delegation_config_read_from_table(&table), table, mode)
}

/// `config.toml [acp] trusted` — WP21 T7, design doc §2.6.
///
/// Default `false`: an inbound ACP `message/send` call stamps
/// `sender_agent = "a2a-client"` (`duduclaw_core::ACP_CLIENT_SENDER`), which is
/// deliberately absent from [`duduclaw_core::SYSTEM_SENDERS`] — so by default
/// this gate denies every A2A-originated delegation (fail-closed, per the
/// module docs). Setting `trusted = true` adds `ACP_CLIENT_SENDER` to the
/// extra-system-senders list passed to `evaluate_delegation`, which grants
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
pub(crate) fn resolve_sender<'a>(
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
#[allow(clippy::too_many_arguments)] // Keeps both durable queue rails on the same gate.
pub(crate) async fn gate_bus_dispatch(
    home_dir: &Path,
    registry: &Arc<RwLock<AgentRegistry>>,
    sender_agent: Option<&str>,
    origin_agent: Option<&str>,
    target: &str,
    task_id: &str,
    path_kind: &str,
    on_behalf_of: Option<&str>,
) -> GateOutcome {
    let Some(sender) = resolve_sender(sender_agent, origin_agent) else {
        let denied = DelegationDenied {
            sender: String::new(),
            target: target.to_string(),
            policy: duduclaw_core::delegation_policy_from_home(home_dir),
            reason: duduclaw_core::DelegationDenyReason::InvalidAgentId,
        };
        warn!(task_id = %truncate_chars(task_id, 64), path_kind,
            "Queue task without a sender refused");
        audit_denial(home_dir, &denied, task_id, path_kind, on_behalf_of);
        return GateOutcome::Deny(denied);
    };
    // Residual: queue files have no authenticated admission signature. A same-UID
    // writer can forge a system sender; gateway-signed admission is v1.80 work.
    let background_system = is_background_system_sender(sender);
    // This restriction is independent of delegation mode and sender type:
    // old bindings and queued goal/heartbeat work must be checked at use.
    let machine_access = {
        let guard = registry.read().await;
        let target_id = DispatchOrgView::new(home_dir, Some(&guard)).canonical_id(target);
        match guard.get(&target_id) {
            Some(agent) => {
                crate::on_behalf_of::access::check_loaded_queue_main_target(home_dir, on_behalf_of, agent, background_system)
                    .await.and_then(|()| crate::machine_employee::check_loaded(home_dir, agent, on_behalf_of))
            }
            None => {
                crate::on_behalf_of::access::check_queue_main_target(home_dir, on_behalf_of, &target_id, background_system)
                    .await.and_then(|()| crate::machine_employee::check_target(home_dir, &target_id, on_behalf_of))
            }
        }
    };
    if machine_access.is_err() {
        let denied = DelegationDenied {
            sender: sender.to_string(),
            target: target.to_string(),
            policy: duduclaw_core::delegation_policy_from_home(home_dir),
            reason: duduclaw_core::DelegationDenyReason::HumanPrincipalNotPermitted,
        };
        audit_denial(home_dir, &denied, task_id, path_kind, on_behalf_of);
        return GateOutcome::Deny(denied);
    }
    // WP21 欠帳⑤: one read + one parse of config.toml serves both extractors
    // below — previously `delegation_rules_from_home` and
    // `acp_trusted_from_home` each re-read and re-parsed the whole file.
    let (read, config_table, human_mode) = read_config_table(home_dir);
    let rules = &read.rules;
    // WP21 T7: `[acp] trusted = true` widens the extra-system-senders list for
    // this decision only — see `acp_trusted_from_table` for the risk this opts
    // into. Default (false) leaves `GATEWAY_INTERNAL_SENDERS` untouched, which
    // is exactly pre-T7 behaviour (`a2a-client` stays denied).
    let mut extra_senders: Vec<&str> = GATEWAY_INTERNAL_SENDERS.to_vec();
    if acp_trusted_from_table(&config_table) {
        extra_senders.push(duduclaw_core::ACP_CLIENT_SENDER);
    }
    // H13b: the only self-delegation that is not refused — a protected
    // `spawn_ephemeral` child, released from `isolation-ephemeral-jobs.db`,
    // whose employee id + incarnation equal the live ones. It replaces only
    // the org-tree predicate; the person check below still applies. Boxed:
    // the queue poll futures have a stack budget (`dispatcher_stack_tests`).
    let ephemeral_child = !background_system && sender.trim() == target.trim()
        && Box::pin(crate::isolation_ephemeral_start::self_delegation_allowed(
            home_dir, task_id, target.trim(), on_behalf_of)).await;
    let guard = registry.read().await;
    let view = DispatchOrgView::new(home_dir, Some(&guard));
    let sender_id = view.canonical_id(sender);
    let target_id = view.canonical_id(target);
    // RM-137: under `none` the protected child is not released either —
    // `spawn_ephemeral` is employee-created work, and the policy stops it.
    let ephemeral_release = ephemeral_child && sender_id == target_id;
    // RM-137 H3: an ACP session is not bound to one agent (the server is
    // started with the home only and `contextId` names the target per
    // message), so under `none` a trusted `a2a-client` row may reach only the
    // installation's ACP default agent — the Main-role agent `"default"`
    // resolves to. No Main-role agent ⇒ no target qualifies (fail closed).
    let acp_cross_target_under_none = rules.policy == duduclaw_core::DelegationPolicy::Disabled
        && sender_id == duduclaw_core::ACP_CLIENT_SENDER
        && guard.main_agent().map(|m| m.config.agent.name.trim()) != Some(target_id.trim());
    let mut requester_account = None;
    let boundaries = DelegationBoundaries {
        personal_target_allowed: crate::personal_visibility::fire_time_refusal(home_dir,
            crate::runtime_readiness::isolation_active(), &[on_behalf_of], Some(&sender_id), &target_id).is_none(),
        // The live registry/main and machine checks above already succeeded.
        main_target_allowed: true,
        // The MCP permission gate checks can_send_cross_agent before enqueue.
        can_send_cross_agent: true,
    };
    let evaluate = |roles: Option<&dyn duduclaw_core::delegation_policy::OwnerRoleView>| (if acp_cross_target_under_none {
            Err(DelegationDenied {
                sender: sender_id.clone(),
                target: target_id.clone(),
                policy: rules.policy,
                reason: duduclaw_core::DelegationDenyReason::DelegationDisabled,
            })
        } else if ephemeral_release && rules.policy == duduclaw_core::DelegationPolicy::Disabled {
            Err(DelegationDenied {
                sender: sender_id.clone(),
                target: target_id.clone(),
                policy: rules.policy,
                reason: duduclaw_core::DelegationDenyReason::DelegationDisabled,
            })
        } else if ephemeral_release { Ok(DelegationAllowSource::Mode) }
        else { evaluate_delegation(rules, &view, &sender_id, &target_id, &DelegationContext {
            overrides: &read.overrides, owner_roles: roles, boundaries,
            extra_system_senders: &extra_senders,
        }) })
        .and_then(|source| {
            // The employee tree allowed it; in enforce mode the person the
            // work is for must be allowed too. System and gateway-internal
            // senders (goal rounds, heartbeat wake-ups, cron) are not an
            // employee handing work on, so they add no person check here:
            // their task was authorized when it was created.
            if duduclaw_core::is_system_sender(&sender_id)
                || extra_senders.iter().any(|s| s.trim() == sender_id)
            {
                return Ok(source);
            }
            let principal = on_behalf_of
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(|value| duduclaw_core::HumanPrincipal::parse(value).inspect_err(|rejection| {
                    crate::on_behalf_of::audit_rejected(home_dir, &sender_id, path_kind, rejection);
                }))
                .transpose();
            crate::on_behalf_of::access::check(
                home_dir,
                human_mode,
                &principal,
                &sender_id,
                &target_id,
                path_kind,
                rules.policy,
            ).map(|()| source)
        });
    // Ordinary mode/pair/system allows leave an unused receipt intact.
    let ordinary = evaluate(None);
    let decision = if ordinary.is_ok() { ordinary } else {
        crate::delegation_roles::initiators::consume_if(home_dir, task_id, &sender_id, &target_id,
            on_behalf_of, |requester| {
                let roles = crate::delegation_roles::GatewayOwnerRoles::new(home_dir, requester);
                let source = evaluate(Some(&roles)).ok()?;
                if source != DelegationAllowSource::UnrestrictedRole { return None; }
                requester_account = roles.snapshot(&sender_id).ok()?.requester_account_id;
                Some(source)
            }).map(Ok).unwrap_or(ordinary)
    };
    drop(guard);

    match decision {
        Ok(source) => {
            crate::delegation_roles::audit_allow(home_dir, &sender_id, &target_id, source,
                requester_account.as_deref(), path_kind);
            GateOutcome::Allow
        },
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
            audit_denial(home_dir, &denied, task_id, path_kind, on_behalf_of);
            GateOutcome::Deny(denied)
        }
    }
}

/// Append a `delegation_denied` event to `security_audit.jsonl`.
///
/// Uses the project's existing audit writer, which takes an exclusive advisory
/// lock around the append (cross-process safety, project convention #3).
fn audit_denial(
    home_dir: &Path,
    denied: &DelegationDenied,
    task_id: &str,
    path_kind: &str,
    on_behalf_of: Option<&str>,
) {
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
            "on_behalf_of": on_behalf_of.map(|v| truncate_chars(v, 96)),
        }),
    );
    crate::security_autopilot::audit_and_emit(home_dir, &event);
}

/// Short shared explanation written back to the task or requester.
pub(crate) fn denial_notice(denied: &DelegationDenied) -> String {
    denied.explain(duduclaw_core::DenialAudience::User)
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
        // Main-target admission requires readable gateway configuration.
        std::fs::write(tmp.path().join("config.toml"), "").unwrap();
        let agents = tmp.path().join("agents");
        let write = |name: &str, reports_to: &str, department: &str| {
            write_agent(&agents, name, reports_to, department);
        };
        write("ceo", "", "");
        write("sales_lead", "ceo", "業務");
        write("sales_rep", "sales_lead", "業務");
        write("sales_rep2", "sales_lead", "業務");
        write("mkt_lead", "ceo", "行銷");
        write("mkt_rep", "mkt_lead", "行銷");
        tmp
    }

    fn write_agent(root: &Path, name: &str, reports_to: &str, department: &str) {
        crate::trigger_route::tests::write_agent(root, name, name, "", "worker", "");
        let path = root.join(name).join("agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("reports_to = \"\"",
            &format!("reports_to = \"{reports_to}\"\ndepartment = \"{department}\""))
            .replace("can_send_cross_agent = false", "can_send_cross_agent = true")).unwrap();
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
            None,
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

        write_agent(&home.join("agents"), "sales_rep", "mkt_lead", "行銷");

        assert!(
            !allows_dispatch(&gate(home, Some("sales_rep"), "mkt_rep").await),
            "a self-rewritten agent.toml must not buy cross-department reach"
        );
        // The real relations still work — this is a redirect of authority, not
        // a lockout.
        assert!(allows_dispatch(
            &gate(home, Some("sales_rep"), "sales_rep2").await
        ));
        assert!(allows_dispatch(
            &gate(home, Some("sales_lead"), "sales_rep").await
        ));
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
            duduclaw_core::org_store::with_org_lock(home, |org| duduclaw_core::org_store::remove(org, stray)).unwrap();
        }
        assert!(allows_dispatch(
            &gate(home, Some("mkt_lead"), "mkt_rep").await
        ));
        assert!(!allows_dispatch(
            &gate(home, Some("mkt_rep"), "sales_rep").await
        ));
    }

    #[tokio::test]
    async fn parent_grandparent_and_same_department_are_allowed() {
        let tmp = seed_home();
        let home = tmp.path();
        // Default policy (no delegation section) = department.
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
        assert!(
            notice.contains("部門模式") && notice.contains("不同部門"),
            "should explain the effective rule and reason: {notice}"
        );
        for leaked_term in [
            "config.toml",
            "reports_to",
            "agent.toml",
            "policy",
            "委派遭拒",
        ] {
            assert!(
                !notice.contains(leaked_term),
                "user-facing notice must not leak internal term '{leaked_term}': {notice}"
            );
        }

        // The shared AI explanation still lands in the audit trail.
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
            audited_message == denied.explain(duduclaw_core::DenialAudience::Ai),
            "audit record must keep the shared AI explanation: {audited_message}"
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
    async fn missing_sender_is_refused_and_audited() {
        let tmp = seed_home();
        let home = tmp.path();

        assert!(matches!(
            gate(home, None, "mkt_rep").await,
            GateOutcome::Deny(DelegationDenied { reason: DelegationDenyReason::InvalidAgentId, .. })
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
                None,
            )
            .await,
            GateOutcome::Deny(DelegationDenied { reason: DelegationDenyReason::InvalidAgentId, .. })
        ));
        let events = audit_lines(home);
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e["details"]["reason"] == "invalid_agent_id"));
        // ...but origin_agent alone is enough to be judged.
        let outcome = gate_bus_dispatch(
            home,
            &registry_for(home),
            None,
            Some("sales_rep"),
            "mkt_rep",
            "task-origin",
            "bus_dispatch",
            None,
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

    /// RM-137: `none` refuses every employee pair at C1 — parent→child,
    /// same department, and an explicit allow pair — with the
    /// `delegation_disabled` reason and the short user-facing notice; people
    /// and schedules still dispatch.
    #[tokio::test]
    async fn none_policy_refuses_every_employee_pair_but_not_system_senders() {
        let tmp = seed_home();
        let home = tmp.path();
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"none\"\nallow = [[\"sales_lead\", \"mkt_lead\"]]\n",
        )
        .unwrap();

        for (sender, target) in [
            ("sales_lead", "sales_rep"),
            ("sales_rep", "sales_lead"),
            ("sales_rep", "sales_rep2"),
            ("sales_lead", "mkt_lead"),
            ("mkt_lead", "sales_lead"),
        ] {
            let GateOutcome::Deny(d) = gate(home, Some(sender), target).await else {
                panic!("{sender} -> {target} must be denied under none");
            };
            assert_eq!(d.reason, DelegationDenyReason::DelegationDisabled, "{sender} -> {target}");
            let notice = denial_notice(&d);
            assert!(notice.contains("不允許委派") && !notice.contains("config.toml"), "{notice}");
        }
        let events = audit_lines(home);
        assert_eq!(events.len(), 5);
        assert!(events.iter().all(|e| e["details"]["reason"] == "delegation_disabled"
            && e["details"]["policy"] == "none"));

        for sender in ["dashboard", "cron", "heartbeat", "goal-loop-driver", "__deferred_gvu__"] {
            assert!(allows_dispatch(&gate(home, Some(sender), "mkt_rep").await), "{sender}");
        }
    }

    /// RM-137: an unrecognised value and an unreadable config are enforced
    /// as `none` at C1 (they used to fall back to `department`, which let
    /// same-department peers through).
    #[tokio::test]
    async fn unknown_policy_and_unreadable_config_are_none_at_c1() {
        let tmp = seed_home();
        let home = tmp.path();
        for body in [
            "[delegation]\npolicy = \"closed\"\n",
            "[delegation]\npolicy = 1\n",
            "delegation = \"open\"\n",
            "[delegation\npolicy = \"open\"\n",
        ] {
            std::fs::write(home.join("config.toml"), body).unwrap();
            let GateOutcome::Deny(d) = gate(home, Some("sales_rep"), "sales_rep2").await else {
                panic!("{body:?} must be enforced as none");
            };
            assert_eq!(effective_policy(home), DelegationPolicy::Disabled, "{body:?}");
            // Parseable configs only for the exact reason and the schedule
            // check: other admission checks may refuse first on an
            // unparsable file (still a denial, which is what matters).
            if body.parse::<toml::Table>().is_ok() {
                assert_eq!(d.reason, DelegationDenyReason::DelegationDisabled, "{body:?}");
                assert!(allows_dispatch(&gate(home, Some("cron"), "sales_rep").await), "{body:?}");
            }
        }
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
        write_agent(&home.join("agents").join(crate::ephemeral::EPHEMERAL_DIR_NAME),
            eph_id, "sales_rep", "");

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

    /// RM-137 H3: under `none`, even a trusted `a2a-client` row reaches only
    /// the ACP default (Main-role) agent; every other target is refused
    /// `delegation_disabled`. ACP sessions have no per-agent binding.
    #[tokio::test]
    async fn none_refuses_trusted_a2a_client_rows_for_any_non_default_target() {
        let tmp = seed_home();
        let home = tmp.path();
        // `ceo` is the Main-role agent (the ACP default).
        let ceo = home.join("agents").join("ceo").join("agent.toml");
        let raw = std::fs::read_to_string(&ceo).unwrap();
        std::fs::write(&ceo, raw.replace("role = \"worker\"", "role = \"main\"")).unwrap();
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"none\"\n[acp]\ntrusted = true\n",
        )
        .unwrap();
        let mut reg = AgentRegistry::new(home.join("agents"));
        reg.scan().await.unwrap();
        let registry = Arc::new(RwLock::new(reg));
        let run = |target: &'static str| {
            let registry = registry.clone();
            async move {
                gate_bus_dispatch(
                    home,
                    &registry,
                    Some(duduclaw_core::ACP_CLIENT_SENDER),
                    None,
                    target,
                    "task-acp",
                    "bus_dispatch",
                    None,
                )
                .await
            }
        };
        for target in ["mkt_rep", "sales_rep", "sales_lead"] {
            match run(target).await {
                GateOutcome::Deny(d) => {
                    assert_eq!(d.reason, DelegationDenyReason::DelegationDisabled, "{target}")
                }
                GateOutcome::Allow => panic!("{target}: a2a-client must not reach it under none"),
            }
        }
        // The default agent is not refused by this rule (other gates may
        // still apply to it).
        assert!(!matches!(
            run("ceo").await,
            GateOutcome::Deny(ref d) if d.reason == DelegationDenyReason::DelegationDisabled
        ));

        // A registry with no Main-role agent resolved (the unscanned one the
        // other tests use): nothing qualifies (fail closed).
        assert!(matches!(
            gate(home, Some(duduclaw_core::ACP_CLIENT_SENDER), "mkt_rep").await,
            GateOutcome::Deny(ref d) if d.reason == DelegationDenyReason::DelegationDisabled
        ));

        // Under another policy the trusted client keeps its reach.
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"department\"\n[acp]\ntrusted = true\n",
        )
        .unwrap();
        assert!(allows_dispatch(&run("mkt_rep").await));
    }

    #[test]
    fn sender_resolution_prefers_sender_agent() {
        assert_eq!(resolve_sender(Some("a"), Some("b")), Some("a"));
        assert_eq!(resolve_sender(None, Some("b")), Some("b"));
        assert_eq!(resolve_sender(Some(""), Some("b")), Some("b"));
        assert_eq!(resolve_sender(Some(" "), None), None);
        assert_eq!(resolve_sender(None, None), None);
    }
    #[tokio::test]
    async fn rm183_roles_none_and_one_way_pairs_at_queue_gate() {
        let tmp = seed_home();
        let home = tmp.path();
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", duduclaw_auth::UserRole::Admin).unwrap().id;
        let employee = db.create_user("employee@test.invalid", "Employee", "test-password", duduclaw_auth::UserRole::Employee).unwrap().id;
        let conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
        conn.execute("UPDATE users SET avatar_agent_id='sales_rep' WHERE id=?1", [&admin]).unwrap();
        let reg = registry_for(home);
        let principal = format!("user:{admin}");
        duduclaw_core::ensure_identity_key(home).unwrap();
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']").unwrap();
        for (id, sender, target, principal) in [
            ("rm183", "sales_rep", "mkt_rep", principal.clone()),
            ("rm183-reverse", "mkt_rep", "sales_rep", principal.clone()),
            ("ordinary", "sales_rep", "mkt_rep", format!("user:{employee}")),
            ("disabled", "sales_rep", "mkt_rep", principal.clone()),
        ] {
            crate::delegation_roles::initiators::record(home, "message", sender, target, id,
                &crate::delegation_roles::Requester::attended(
                    duduclaw_core::HumanPrincipal::parse(&principal).unwrap().user_id().unwrap())).unwrap();
        }
        for policy in ["hierarchy", "none"] {
            std::fs::write(home.join("config.toml"), format!("[delegation]\npolicy='{policy}'\nunrestricted_roles=['admin']")).unwrap();
            crate::delegation_roles::initiators::record(home, "message", "sales_rep", "mkt_rep", "rm183",
                &crate::delegation_roles::Requester::attended(&admin)).unwrap();
            for (sender, target) in [("mkt_rep", "mkt_rep"), ("sales_rep", "sales_mgr")] {
                let _ = gate_bus_dispatch(home, &reg, Some(sender), None, target,
                    "rm183", "bus_dispatch", Some(&principal)).await;
                assert!(crate::delegation_roles::initiators::lookup(home, "message", "rm183", Some("sales_rep"),
                    "mkt_rep", Some(&principal)).is_some());
            }
            let outcome = gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
                "rm183", "bus_dispatch", Some(&principal)).await;
            assert!(allows_dispatch(&outcome));
            assert!(crate::delegation_roles::initiators::lookup(home, "message", "rm183", Some("sales_rep"),
                "mkt_rep", Some(&principal)).is_none());
            assert!(!allows_dispatch(&gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
                "rm183", "bus_dispatch", Some(&principal)).await));
            assert!(!allows_dispatch(&gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
                "forged-unrecorded", "bus_dispatch", Some(&principal)).await));
            let reverse = gate_bus_dispatch(home, &reg, Some("mkt_rep"), None, "sales_rep",
                "rm183-reverse", "bus_dispatch", Some(&principal)).await;
            assert!(!allows_dispatch(&reverse));
            let ordinary = format!("user:{employee}");
            assert!(!allows_dispatch(&gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
                "ordinary", "bus_dispatch", Some(&ordinary)).await));
        }
        let audit = std::fs::read_to_string(home.join("security_audit.jsonl")).unwrap();
        assert!(audit.contains("unrestricted_role") && audit.contains(&admin));
        for config in [
            "[delegation]\npolicy='open'\nunrestricted_roles=['admin']",
            "[delegation]\npolicy='hierarchy'\nunrestricted_roles=['admin']\nallow_one_way=[['sales_rep','mkt_rep']]",
        ] {
            std::fs::write(home.join("config.toml"), config).unwrap();
            assert!(allows_dispatch(&gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
                "disabled", "bus_dispatch", Some(&principal)).await));
            assert!(crate::delegation_roles::initiators::lookup(home, "message", "disabled", Some("sales_rep"),
                "mkt_rep", Some(&principal)).is_some());
        }
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']").unwrap();
        conn.execute("UPDATE users SET status='disabled' WHERE id=?1", [&admin]).unwrap();
        assert!(!allows_dispatch(&gate_bus_dispatch(home, &reg, Some("sales_rep"), None, "mkt_rep",
            "disabled", "bus_dispatch", Some(&principal)).await));
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy='hierarchy'\nallow_one_way=[['sales_rep','mkt_rep']]").unwrap();
        assert!(allows_dispatch(&gate(home, Some("sales_rep"), "mkt_rep").await));
        let reverse = gate(home, Some("mkt_rep"), "sales_rep").await;
        assert!(matches!(reverse, GateOutcome::Deny(DelegationDenied { reason: DelegationDenyReason::ReversePairOnly, .. })));
        let path = home.join("agents/sales_rep/agent.toml");
        let mut table: toml::Table = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        table.entry("permissions".to_string()).or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut().unwrap().insert("can_send_cross_agent".into(), toml::Value::Boolean(false));
        std::fs::write(path, toml::to_string(&table).unwrap()).unwrap();
        // C1 preserves v1.85: permissions are checked at the MCP front door.
        assert!(allows_dispatch(&gate(home, Some("sales_rep"), "mkt_rep").await));
    }

    #[tokio::test]
    async fn rm183_manager_pass_never_bypasses_main_boundary() {
        let tmp = seed_home();
        let home = tmp.path();
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let manager = db.create_user("manager@test.invalid", "Manager", "test-password", duduclaw_auth::UserRole::Manager).unwrap().id;
        rusqlite::Connection::open(home.join("users.db")).unwrap()
            .execute("UPDATE users SET avatar_agent_id='sales_rep' WHERE id=?1", [&manager]).unwrap();
        let path = home.join("agents/ceo/agent.toml");
        let raw = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, raw.replace("role = \"worker\"", "role = \"main\"")).unwrap();
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['manager']").unwrap();
        duduclaw_core::ensure_identity_key(home).unwrap();
        let principal = format!("user:{manager}");
        crate::delegation_roles::initiators::record(home, "message", "sales_rep", "ceo", "main-test",
            &crate::delegation_roles::Requester::attended(
                    duduclaw_core::HumanPrincipal::parse(&principal).unwrap().user_id().unwrap())).unwrap();
        let outcome = gate_bus_dispatch(home, &registry_for(home), Some("sales_rep"), None, "ceo",
            "main-test", "bus_dispatch", Some(&principal)).await;
        assert!(matches!(outcome, GateOutcome::Deny(DelegationDenied {
            reason: DelegationDenyReason::HumanPrincipalNotPermitted, .. })));
    }

    #[cfg(unix)]
    #[test]
    fn rm183_admin_pass_never_bypasses_personal_queue_boundary() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = seed_home();
        let home = tmp.path();
        let db = duduclaw_auth::UserDb::new(&home.join("users.db")).unwrap();
        let admin = db.create_user("admin@test.invalid", "Admin", "test-password", duduclaw_auth::UserRole::Admin).unwrap().id;
        let owner = db.create_user("owner@test.invalid", "Owner", "test-password", duduclaw_auth::UserRole::Employee).unwrap().id;
        let mut conn = rusqlite::Connection::open(home.join("users.db")).unwrap();
        conn.execute("UPDATE users SET avatar_agent_id='sales_rep' WHERE id=?1", [&admin]).unwrap();
        let tx = conn.transaction().unwrap();
        assert!(duduclaw_auth::db::insert_personal_agent_in_transaction(&tx, "mkt_rep", &owner).unwrap());
        assert!(duduclaw_auth::db::bind_personal_owner_in_transaction(&tx, "mkt_rep", &owner).unwrap());
        tx.commit().unwrap();
        drop(conn);
        drop(db);
        for name in ["users.db", "users.db-wal", "users.db-shm", "users.db-journal"] {
            let file = home.join(name);
            if file.exists() { std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap(); }
        }
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy='none'\nunrestricted_roles=['admin']").unwrap();
        duduclaw_core::ensure_identity_key(home).unwrap();
        let principal = format!("user:{admin}");
        crate::delegation_roles::initiators::record(home, "message", "sales_rep", "mkt_rep", "personal-test",
            &crate::delegation_roles::Requester::attended(
                    duduclaw_core::HumanPrincipal::parse(&principal).unwrap().user_id().unwrap())).unwrap();
        let isolation = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(isolation.path().join("opt/duduclaw-isolated")).unwrap();
        std::os::unix::fs::symlink("generations/missing", isolation.path().join("opt/duduclaw-isolated/current")).unwrap();
        let marker = isolation.path().join("protected-mode");
        std::fs::write(&marker, "native-systemd\n").unwrap();
        let sources = duduclaw_core::protected_mode::Sources { env: None, marker,
            service_home: isolation.path().join("no-service-home") };
        duduclaw_core::protected_mode::with_thread_sources(sources, || {
            duduclaw_core::isolation_install::with_thread_root(isolation.path().to_path_buf(), || {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
                let outcome = rt.block_on(gate_bus_dispatch(home, &registry_for(home), Some("sales_rep"), None,
                    "mkt_rep", "personal-test", "bus_dispatch", Some(&principal)));
                assert!(matches!(outcome, GateOutcome::Deny(DelegationDenied {
                    reason: DelegationDenyReason::PersonalTargetNotPermitted, .. })));
            });
        });
    }

}
