//! WP21 — the single delegation predicate: *may `sender` delegate to `target`?*
//!
//! Before this module the only "who may command whom" rule lived in the CLI MCP
//! server (`check_supervisor_relation`) and recognised exactly one shape:
//! a **direct** parent↔child pair. Every other path into the bus (dispatcher,
//! `tasks_create`, ACP, autopilot) had no rule at all. This module is the one
//! predicate all of those choke points share, so the answer cannot drift per
//! call site.
//!
//! ## Decision order
//!
//! Empty ids, self-delegation and injected boundary refusals always deny first.
//! Existing system senders then retain their operator-authorized access.
//! For AI senders: unrestricted role → explicit pair → delegation mode.
//! [`evaluate_delegation`] requires both an avatar owner and a verified turn
//! requester (or background-job initiator) in `unrestricted_roles`. This pass
//! overrides explicit `none`; neither kind of pair overrides `none`.
//! Unknown policy values and unreadable config fail closed without overrides.
//!
//! Legacy `can_delegate*` wrappers keep their signatures and decisions. They
//! have no role evidence or new overrides; callers must migrate to the new
//! entry point to use them and supply the live boundary checks.
//!
//! Team-as-Agent role hand-offs never reach this predicate: the team composer
//! scaffolds and runs its role members itself (`team_composer` →
//! `ephemeral::dispatch_with`, no bus task), and the members exchange
//! `TaskPacket`s only through the MCP `team_handoff` tool, which is pinned to a
//! scaffolded member's own `(task, round)` and is not one of the tools
//! `Disabled` refuses at the MCP dispatch gate. That is the whole exception.
//!
//! ## What this module is *not*
//!
//! It is not a rate limiter (`dispatch_guard`) and not a tool-permission gate
//! (`capability_grants`). Those two are orthogonal and stay where they are.
//! (A third neighbour, the never-wired `delegation_scope` permission-attenuation
//! module, was removed on 2026-09-29 — see
//! `wiki/reports/feature-audit-2026-09-29.md` T1-D11.) Ranks (`org::OrgRank`)
//! deliberately play no part: the `reports_to` tree remains the only authority
//! on hierarchy (see `org.rs` — rank is derived, never authoritative).
//!
//! Org lookups are injected via [`OrgView`] because the two callers read the
//! registry very differently (the CLI reads `agent.toml` files asynchronously,
//! the gateway holds an in-memory registry). Callers snapshot what they need
//! and hand it over; this module stays a pure, synchronous, testable predicate.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Deserialize;

mod warnings;
pub use warnings::{DelegationWarning, delegation_policy_warning, delegation_section_warnings, delegation_override_warnings};

/// Maximum `reports_to` hops walked when testing ancestry. Mirrors the cycle
/// guard in the CLI's `validate_reports_to`: a chain deeper than this (or one
/// that loops) stops the walk, and an unproven relation is a DENY.
pub const MAX_ANCESTOR_HOPS: usize = 20;

/// Senders that are not agents at all but human interfaces / operator-configured
/// schedulers. They are allowed to address any agent.
///
/// **Inclusion criterion**: the delegation is *initiated by a human interface*
/// (`dashboard`, `webhook`) *or by a schedule the user explicitly configured*
/// (`cron`, `heartbeat`, `goal-loop-driver`, `autopilot` — autopilot rules are
/// hand-written config whose CRUD already passes the dashboard's own
/// authorization). Anything whose origin is another *agent* — or an external
/// party — must earn its permission through the org tree instead.
///
/// Note what is deliberately **absent**: [`ACP_CLIENT_SENDER`]. An inbound A2A
/// call is an outside party, so by default it is denied and must be opened
/// explicitly (design doc §2.6, wired in a later task via
/// [`can_delegate_ext`]).
pub const SYSTEM_SENDERS: [&str; 6] = [
    "dashboard",
    "webhook",
    "goal-loop-driver",
    "cron",
    "heartbeat",
    "autopilot",
];

/// Queue `sender` values the gateway itself writes on `message_queue` rows
/// that are not in [`SYSTEM_SENDERS`] (P2-A third review L3-2). The
/// dispatcher gives these rows meaning (heartbeat wake-up attribution and
/// stop fencing; workflow hand-offs), so no employee may carry one of these
/// names: an employee named `heartbeat-scheduler` would otherwise send
/// queue rows the dispatcher reads as the heartbeat's.
pub const RESERVED_QUEUE_SENDERS: [&str; 2] = ["heartbeat-scheduler", "workflow"];

/// Is this one of [`RESERVED_QUEUE_SENDERS`]? Exact after trimming,
/// ASCII-case-insensitive. Identity checks treat such a name like a system
/// sender: an employee created under it before it was reserved resolves to
/// the untrusted sentinel (P2-A fourth review L4-2).
pub fn is_reserved_queue_sender(name: &str) -> bool {
    let s = name.trim();
    RESERVED_QUEUE_SENDERS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(s))
}

/// Sender id stamped on bus tasks that entered through the ACP `message/send`
/// endpoint. Intentionally **not** in [`SYSTEM_SENDERS`]; exposed as a constant
/// so the opt-in (`config.toml [acp] trusted = true`) has one spelling.
pub const ACP_CLIENT_SENDER: &str = "a2a-client";

/// Prefix of the client ids `duduclaw mcp init` issues standalone MCP keys
/// under (`standalone-claude-code`, `standalone-codex`, …). Reserved at
/// employee creation by [`is_reserved_agent_id`].
pub const STANDALONE_CLIENT_PREFIX: &str = "standalone-";

/// Is this sender a system/human interface rather than an agent?
///
/// Exact match after trimming — never a substring test (project convention #2:
/// an unanchored `contains` would let `evil-dashboard` pass).
pub fn is_system_sender(sender: &str) -> bool {
    let s = sender.trim();
    SYSTEM_SENDERS.iter().any(|known| *known == s)
}

/// May a *real agent* be given this id?
///
/// [`SYSTEM_SENDERS`] are by definition **not agents** (design doc §2.3: "非
/// agent 身分(人類/系統發起)"). Nothing else enforced that: `is_valid_agent_id`
/// happily accepts `cron`, and an agent literally named `cron` would then
/// satisfy [`is_system_sender`] and inherit unconditional delegation reach —
/// clearing every choke point (C1/C2/C3), skipping the C4 subtree rule, and
/// seeing the whole org through the read-side filter. So the id space is
/// reserved here and rejected at agent creation.
///
/// Also reserved:
/// - [`ACP_CLIENT_SENDER`] — the external-A2A identity, which `[acp] trusted`
///   can promote to system reach.
/// - `default` — the dispatcher's alias for the `Main`-role agent; an agent
///   actually named `default` would make the alias ambiguous mid-decision.
/// - `doctor-probe` (WP22 T3) — the id `duduclaw doctor` / `system.doctor`'s
///   MCP cold-start probe signs and spawns under (see
///   `duduclaw_gateway::doctor_probes::mcp_cold_start_probe`). This only
///   blocks *creating* a real agent with that name via `create_agent` /
///   `agents.create`; the probe itself mints its token directly through
///   `agent_identity_env_vars`, which never consults this list, so the probe
///   keeps working unaffected.
/// - [`RESERVED_QUEUE_SENDERS`] — queue sender names the dispatcher acts on.
/// - any `__…` id — the namespace gateway-internal synthetic senders use
///   (e.g. `__deferred_gvu__`, which `is_valid_agent_id` otherwise accepts).
/// - any [`STANDALONE_CLIENT_PREFIX`] id — the client ids `duduclaw mcp init`
///   issues external keys under. Such a key keeps its wiki in
///   `agents/<client_id>/wiki/`; an employee created under the same name
///   would share that directory with the external key.
///
/// Case-insensitive on purpose: it costs nothing and covers the day someone
/// makes [`is_system_sender`] case-insensitive too.
pub fn is_reserved_agent_id(id: &str) -> bool {
    let s = id.trim();
    if s.is_empty() {
        return false;
    }
    if s.starts_with("__") {
        return true;
    }
    if s.get(..STANDALONE_CLIENT_PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(STANDALONE_CLIENT_PREFIX))
    {
        return true;
    }
    SYSTEM_SENDERS
        .iter()
        .chain(std::iter::once(&ACP_CLIENT_SENDER))
        .chain(std::iter::once(&"default"))
        .chain(std::iter::once(&"doctor-probe"))
        .chain(RESERVED_QUEUE_SENDERS.iter())
        .any(|known| known.eq_ignore_ascii_case(s))
}

// ── Policy ───────────────────────────────────────────────────────────────────

/// How permissive the delegation predicate is. Set via
/// `config.toml [delegation] policy`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DelegationPolicy {
    /// Ancestor chain (both directions) **plus** same-department peers.
    /// The default: strictly wider than the pre-WP21 direct parent↔child check,
    /// so the two front-door MCP paths see zero regression while the back doors
    /// tighten from "wide open".
    #[default]
    Department,
    /// Ancestor chain only — no same-department shortcut.
    Hierarchy,
    /// Everything except self-delegation. The escape hatch that restores
    /// pre-WP21 behaviour (runaway guards and tool permissions still apply).
    Open,
    /// `policy = "none"` (RM-137, v1.82): no AI employee may delegate to,
    /// message, spawn, create or assign work to another AI employee. Explicit
    /// `[delegation] allow` pairs do not apply. System senders still pass.
    /// An explicitly configured unrestricted role can pass through
    /// [`evaluate_delegation`] after every always-deny check.
    /// Also what an unrecognised value or an unreadable config resolves to.
    Disabled,
}

impl DelegationPolicy {
    /// Every policy, in the order the dashboard lists them.
    pub const ALL: [DelegationPolicy; 4] = [
        DelegationPolicy::Department,
        DelegationPolicy::Hierarchy,
        DelegationPolicy::Open,
        DelegationPolicy::Disabled,
    ];

    /// Stable wire string (`department` / `hierarchy` / `open` / `none`).
    pub fn as_str(&self) -> &'static str {
        match self {
            DelegationPolicy::Department => "department",
            DelegationPolicy::Hierarchy => "hierarchy",
            DelegationPolicy::Open => "open",
            DelegationPolicy::Disabled => "none",
        }
    }

    /// zh-TW label for operator-facing messages.
    pub fn label_zh(&self) -> &'static str {
        match self {
            DelegationPolicy::Department => "部門(department)",
            DelegationPolicy::Hierarchy => "階級(hierarchy)",
            DelegationPolicy::Open => "開放(open)",
            DelegationPolicy::Disabled => "不允許委派(none)",
        }
    }

    /// Do `[delegation] allow` pairs take part in decisions under this
    /// policy? Only `department` and `hierarchy` consult them (`open` allows
    /// everything anyway; `none` ignores them by design).
    pub fn honors_allow_pairs(&self) -> bool {
        matches!(self, DelegationPolicy::Department | DelegationPolicy::Hierarchy)
    }

    /// Parse a config value. Case- and whitespace-insensitive; unknown ⇒ `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "department" => Some(DelegationPolicy::Department),
            "hierarchy" => Some(DelegationPolicy::Hierarchy),
            "open" => Some(DelegationPolicy::Open),
            "none" => Some(DelegationPolicy::Disabled),
            _ => None,
        }
    }

    /// Parse for config loading. Since v1.82 (RM-137) an unrecognised value
    /// fails closed to [`DelegationPolicy::Disabled`] (it used to fall back to
    /// the wider `department`) and returns a warning string for the caller to
    /// log once ([`warn_policy_once`]) and to show on the dashboard.
    pub fn from_config_value(value: &str) -> (Self, Option<String>) {
        match Self::parse(value) {
            Some(p) => (p, None),
            None => (
                DelegationPolicy::Disabled,
                Some(format!(
                    "config.toml [delegation] policy = \"{}\" 無法辨識,已視為 \"none\"(不允許 AI 員工之間委派)。\
                     可用值:department / hierarchy / open / none。",
                    crate::truncate_chars(value.trim(), 40),
                )),
            ),
        }
    }
}

/// Log a policy-level `[delegation]` problem (unrecognised value, unreadable
/// config) once per process. The gates re-read the config on every decision;
/// without this the same line would repeat for every message.
pub fn warn_policy_once(message: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| tracing::warn!("{message}"));
}

// ── Whitelist pairs (WP21 T11 — design doc §2.1 rule 5b, §2.2) ────────────────

/// Order-independent two-string key: `("a", "b")` and `("b", "a")` are the same
/// pair. Used both for the config-driven allow-list and for matching a
/// candidate `(sender, target)` against it.
fn sorted_pair(a: &str, b: &str) -> (String, String) {
    if a <= b {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

/// Validate and normalise one `[delegation] allow` entry (already split into
/// individual strings by [`DelegationConfig::from_home`]).
///
/// `Err` carries a human-readable reason for the caller to fold into a
/// warning — the entry is dropped, never fatal to the rest of the list
/// (design doc §2.2: "壞項忽略並 warn，不整段失效").
fn normalize_allow_entry(item: &[String]) -> Result<(String, String), String> {
    if item.len() != 2 {
        return Err(format!("必須恰好包含 2 個字串,實際 {} 個", item.len()));
    }
    let a = item[0].trim();
    let b = item[1].trim();
    if a.is_empty() || b.is_empty() {
        return Err("配對成員不可為空字串".to_string());
    }
    if a == b {
        return Err(format!("忽略自我配對(\"{a}\" 與自己)"));
    }
    Ok(sorted_pair(a, b))
}

/// Parse `[delegation] allow` entries into normalised (unordered, deduped)
/// pairs plus one warning string per dropped entry. Never fails outright —
/// this is the "壞項忽略並 warn" half of design doc §2.2.
pub fn resolve_allow_pairs(entries: &[Vec<String>]) -> (Vec<(String, String)>, Vec<String>) {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries {
        match normalize_allow_entry(entry) {
            Ok(pair) => {
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
            Err(reason) => warnings.push(format!(
                "config.toml [delegation] allow 項目已忽略:{reason}(原始值:{entry:?})。"
            )),
        }
    }
    (pairs, warnings)
}

/// The effective policy plus the cross-department whitelist (design doc §2.1
/// rule 5b) — everything [`can_delegate_rules`] needs to decide a delegation
/// that the ancestor-chain and same-department rules alone would refuse.
///
/// [`can_delegate`] / [`can_delegate_ext`] remain thin wrappers around the
/// same predicate with an empty whitelist, so every pre-T11 call site keeps
/// its exact behaviour without being rewritten.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationRules {
    pub policy: DelegationPolicy,
    /// Normalised (sorted, deduped, unordered) pairs. Build via
    /// [`resolve_allow_pairs`] / [`DelegationConfig::resolve_rules`] rather
    /// than pushing raw strings — `allows_pair` assumes this invariant.
    pub allow_pairs: Vec<(String, String)>,
}

impl DelegationRules {
    /// Does the whitelist contain `(a, b)`, in either order?
    ///
    /// Also used by the read-side visibility filter (design doc §2.5): "A can
    /// delegate to B" and "A can see B" are the same question for whitelist
    /// partners.
    pub fn allows_pair(&self, a: &str, b: &str) -> bool {
        let a = a.trim();
        let b = b.trim();
        if a.is_empty() || b.is_empty() || a == b {
            return false;
        }
        self.allow_pairs.contains(&sorted_pair(a, b))
    }
}

/// New opt-in rules, kept separate so existing `DelegationRules` literals
/// and legacy callers remain source-compatible. Resolve through
/// [`DelegationConfig::resolve_overrides`]; defaults grant no extra access.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DelegationOverrides {
    pub allow_one_way: Vec<(String, String)>,
    pub unrestricted_roles: Vec<String>,
}

/// Matches the existing dashboard's pair-entry limit. Legacy `allow` parsing
/// is unchanged; the new directional list accepts only its first 200 entries.
pub const MAX_ONE_WAY_PAIRS: usize = 200;

/// Coupled to `duduclaw_auth::models::UserRole`. Auth depends on core, so core
/// must keep a local closed list rather than introduce a dependency cycle.
pub const UNRESTRICTABLE_ROLE_NAMES: [&str; 2] = ["admin", "manager"];

/// Same validation and trimming as `allow`, with ordered deduplication.
/// Entries beyond the first 200 are ignored with one limit warning.
pub fn resolve_allow_one_way(entries: &[Vec<String>]) -> (Vec<(String, String)>, Vec<String>) {
    let mut pairs = Vec::new();
    let mut warnings = Vec::new();
    for entry in entries.iter().take(MAX_ONE_WAY_PAIRS) {
        match normalize_allow_entry(entry) {
            Ok(_) => {
                let pair = (entry[0].trim().to_string(), entry[1].trim().to_string());
                if !pairs.contains(&pair) {
                    pairs.push(pair);
                }
            }
            Err(reason) => warnings.push(format!(
                "config.toml [delegation] allow_one_way 項目已忽略:{reason}(原始值:{entry:?})。"
            )),
        }
    }
    if entries.len() > MAX_ONE_WAY_PAIRS {
        warnings.push(format!(
            "config.toml [delegation] allow_one_way 超過 {MAX_ONE_WAY_PAIRS} 項,超出部分已忽略。"
        ));
    }
    (pairs, warnings)
}

/// A verified account's current role, with explicit background-job provenance.
/// Bare background/system status is not evidence of a qualifying initiator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DelegationRequesterRole {
    Account(String),
    BackgroundStartedBy(String),
}

/// Lookup failure grants no unrestricted pass (pairs/mode still apply).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OwnerRoleLookupError;

/// Trusted host-side identity snapshot for this decision. Never read users.db
/// in core. Implementations must resolve `users.avatar_agent_id`, not generic
/// Owner bindings, and verify the current requester or persisted job initiator.
/// Return `Ok(None)` for unknown identity/provenance, `Err` for read failures.
/// Role strings are matched exactly against the closed list and config.
pub trait OwnerRoleView: Send + Sync {
    fn avatar_owner_role(&self, agent_id: &str) -> Result<Option<String>, OwnerRoleLookupError>;
    fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError>;
}

/// Host-computed always-deny facts for the actual sender/target/turn. Unknown
/// or failed checks must be false. Defaults deny; no role or pair bypasses them.
/// These booleans attest checks, not user-configurable permission overrides.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DelegationBoundaries {
    pub personal_target_allowed: bool,
    pub main_target_allowed: bool,
    pub can_send_cross_agent: bool,
}

/// Inputs that legacy callers cannot supply. The host must compute boundaries
/// before evaluation, including for system senders; do not infer them from role.
pub struct DelegationContext<'a> {
    pub overrides: &'a DelegationOverrides,
    pub owner_roles: Option<&'a dyn OwnerRoleView>,
    pub boundaries: DelegationBoundaries,
    pub extra_system_senders: &'a [&'a str],
}

/// The first rule that allowed the decision, for audit/effective-settings UIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationAllowSource {
    SystemSender,
    UnrestrictedRole,
    OneWayPair,
    TwoWayPair,
    Mode,
}

impl DelegationAllowSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SystemSender => "system_sender",
            Self::UnrestrictedRole => "unrestricted_role",
            Self::OneWayPair => "one_way_pair",
            Self::TwoWayPair => "two_way_pair",
            Self::Mode => "mode",
        }
    }
}

fn unrestricted_sender(view: &dyn OwnerRoleView, overrides: &DelegationOverrides, sender: &str) -> bool {
    let configured = |role: &str| UNRESTRICTABLE_ROLE_NAMES.contains(&role)
        && overrides.unrestricted_roles.iter().any(|r| r == role);
    let Ok(Some(owner_role)) = view.avatar_owner_role(sender) else { return false };
    if !configured(&owner_role) { return false; }
    let Ok(Some(requester)) = view.requester_role() else { return false };
    let role = match requester {
        DelegationRequesterRole::Account(role) | DelegationRequesterRole::BackgroundStartedBy(role) => role,
    };
    configured(&role)
}

// ── Org lookups (dependency injection) ───────────────────────────────────────

/// Read-only view of the org tree, implemented by each caller over whatever
/// registry it already holds.
///
/// Both methods return `None` for "unknown agent" *and* for "no value"; the
/// predicate treats an empty / `"none"` `reports_to` as "root" and an empty
/// department as "no department", so an implementation may return either shape.
pub trait OrgView {
    /// The agent this agent reports to, if any.
    fn reports_to(&self, agent_id: &str) -> Option<String>;
    /// The agent's department (`[agent] department` in `agent.toml`), if any.
    fn department(&self, agent_id: &str) -> Option<String>;
}

/// One agent's org placement.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OrgNode {
    /// Parent agent id; empty (or `"none"`) means root.
    pub reports_to: String,
    /// Department name; empty means none.
    pub department: String,
}

/// In-memory [`OrgView`] over a snapshot map. Used by tests and by callers that
/// already materialise the whole registry (the gateway dispatcher does).
#[derive(Debug, Clone, Default)]
pub struct MapOrgView {
    nodes: HashMap<String, OrgNode>,
}

impl MapOrgView {
    pub fn new() -> Self {
        Self::default()
    }

    /// Builder-style insert.
    pub fn with_agent(
        mut self,
        agent_id: impl Into<String>,
        reports_to: impl Into<String>,
        department: impl Into<String>,
    ) -> Self {
        self.insert(agent_id, reports_to, department);
        self
    }

    pub fn insert(
        &mut self,
        agent_id: impl Into<String>,
        reports_to: impl Into<String>,
        department: impl Into<String>,
    ) {
        self.nodes.insert(
            agent_id.into(),
            OrgNode {
                reports_to: reports_to.into(),
                department: department.into(),
            },
        );
    }

    /// Number of agents in the snapshot.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

impl OrgView for MapOrgView {
    fn reports_to(&self, agent_id: &str) -> Option<String> {
        self.nodes.get(agent_id).map(|n| n.reports_to.clone())
    }

    fn department(&self, agent_id: &str) -> Option<String> {
        self.nodes.get(agent_id).map(|n| n.department.clone())
    }
}

/// Normalise a `reports_to` value: both `""` and `"none"` mean "root"
/// (sentinel inherited from `agent.toml`, matching the CLI's
/// `normalize_reports_to`).
fn normalize_parent(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Result of walking the `reports_to` chain upward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Walk {
    /// The sought ancestor was found on the chain.
    Found,
    /// The chain ended (root reached, unknown agent, or a cycle closed) without
    /// finding it.
    NotFound,
    /// The walk hit [`MAX_ANCESTOR_HOPS`] before concluding — the relation is
    /// unproven, which fails closed.
    Truncated,
}

fn walk_up(org: &impl OrgView, start: &str, looking_for: &str) -> Walk {
    let start = start.trim();
    let looking_for = looking_for.trim();
    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.to_string());
    let mut current = start.to_string();

    for _ in 0..MAX_ANCESTOR_HOPS {
        let Some(parent) = org.reports_to(&current).and_then(|p| normalize_parent(&p)) else {
            return Walk::NotFound; // root, or agent not in the registry
        };
        if parent == looking_for {
            return Walk::Found;
        }
        if !seen.insert(parent.clone()) {
            // Cycle: every distinct node on this chain has been visited and the
            // sought ancestor was not among them. Terminating, not infinite.
            return Walk::NotFound;
        }
        current = parent;
    }
    Walk::Truncated
}

/// Is `ancestor` somewhere above `descendant` on the `reports_to` chain
/// (any depth, ≤ [`MAX_ANCESTOR_HOPS`], cycle-safe)?
///
/// `false` when the chain is truncated or cyclic — an unproven relation is
/// treated as absent. Exposed because the "may only attach under your own
/// subtree" rule needs exactly this question.
pub fn is_ancestor(org: &impl OrgView, ancestor: &str, descendant: &str) -> bool {
    matches!(walk_up(org, descendant, ancestor), Walk::Found)
}

// ── Denial ───────────────────────────────────────────────────────────────────

/// Why a delegation was refused. Stable machine codes for audit records
/// (`delegation_denied` events) plus zh-TW rendering for humans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    /// Only the reverse one-way pair exists, and no mode rule allowed this call.
    ReversePairOnly,
    /// The host refused another person's personal employee (or could not check).
    PersonalTargetNotPermitted,
    /// The host refused the main-agent boundary (or could not check).
    MainTargetNotPermitted,
    /// The sender's own cross-agent permission is false or could not be checked.
    CrossAgentSendDisabled,
    /// Rule 1 — an agent may not delegate to itself.
    SelfDelegation,
    /// Defensive: an empty sender or target id; nothing can be proven about a
    /// nameless party, so it fails closed.
    InvalidAgentId,
    /// No ancestor relation in either direction (policy has no department rule).
    NoHierarchyRelation,
    /// No ancestor relation, and the two agents sit in different departments.
    DifferentDepartment,
    /// No ancestor relation, and at least one side has no department set —
    /// blank is never "the same department".
    MissingDepartment,
    /// The `reports_to` chain exceeded [`MAX_ANCESTOR_HOPS`] (or loops), so
    /// ancestry could not be established.
    ChainTooDeep,
    /// The employee tree allowed it, but the person the work is for
    /// (`on_behalf_of`, `[delegation] human_principal = "enforce"`) may not
    /// hand work to the target, or could not be resolved.
    HumanPrincipalNotPermitted,
    /// The policy is [`DelegationPolicy::Disabled`] (`none`, an unrecognised
    /// value, or an unreadable config): no AI employee delegates to another.
    DelegationDisabled,
}

impl DenyReason {
    /// Stable machine code for audit logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            DenyReason::ReversePairOnly => "reverse_pair_only",
            DenyReason::PersonalTargetNotPermitted => "personal_target_not_permitted",
            DenyReason::MainTargetNotPermitted => "main_target_not_permitted",
            DenyReason::CrossAgentSendDisabled => "cross_agent_send_disabled",
            DenyReason::DelegationDisabled => "delegation_disabled",
            DenyReason::SelfDelegation => "self_delegation",
            DenyReason::InvalidAgentId => "invalid_agent_id",
            DenyReason::NoHierarchyRelation => "no_hierarchy_relation",
            DenyReason::DifferentDepartment => "different_department",
            DenyReason::MissingDepartment => "missing_department",
            DenyReason::ChainTooDeep => "chain_too_deep",
            DenyReason::HumanPrincipalNotPermitted => "human_principal_not_permitted",
        }
    }
}

/// A refused delegation, carrying everything an audit record or an operator
/// needs.
///
/// The rendered message names only the two agent ids the caller already
/// supplied — never a filesystem path, registry location, or any third agent's
/// existence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationDenied {
    pub sender: String,
    pub target: String,
    pub policy: DelegationPolicy,
    pub reason: DenyReason,
}

/// Agent ids are user-controlled; cap them in messages with the char-safe
/// truncator (project convention #1 — never slice by byte index).
fn short_id(id: &str) -> String {
    crate::truncate_chars(id.trim(), 64)
}

/// The recipient of a delegation refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenialAudience {
    Ai,
    User,
}

impl DelegationDenied {
    /// Plain zh-TW rule and reason, with actionable next steps for AI callers.
    /// This value does not prove that messaging is allowed, so it never suggests
    /// contacting the target as a workaround.
    pub fn explain(&self, audience: DenialAudience) -> String {
        let s = short_id(&self.sender);
        let t = short_id(&self.target);
        let rule = match self.policy {
            DelegationPolicy::Department => "目前是「部門模式」，依上下級、同部門與跨部門配對判定委派範圍。",
            DelegationPolicy::Hierarchy => "目前是「階層模式」，依上下級與跨部門配對判定委派範圍。",
            // Use the dashboard's help text: the mode name is deliberately
            // omitted to avoid presenting it as a suggested remedy.
            DelegationPolicy::Open => "目前的委派範圍不設限制，任何 AI 員工都能指派工作給任何人，但仍須通過個別權限檢查。",
            DelegationPolicy::Disabled => "目前是「不允許委派」，只有符合條件的「不受委派範圍限制的角色」例外可通過。",
        };
        let reason = match self.reason {
            DenyReason::ReversePairOnly => format!(
                "單向配對只允許「{t}」委派給「{s}」，不允許「{s}」反向交辦給「{t}」，目前模式也未允許這個方向。"
            ),
            DenyReason::PersonalTargetNotPermitted => format!(
                "「{s}」不能把工作交給「{t}」，因為「{t}」是另一位成員的個人員工，或無法確認是否有權使用。"
            ),
            DenyReason::MainTargetNotPermitted => format!(
                "「{s}」不能把工作交給「{t}」，因為主代理的使用權限未通過確認。"
            ),
            DenyReason::CrossAgentSendDisabled => format!(
                "「{s}」不能把工作交給「{t}」，因為「允許跨 AI 員工通訊」開關已關閉，或無法確認是否啟用。"
            ),
            DenyReason::SelfDelegation => format!("「{s}」不可委派給自己，所有委派模式都禁止自我委派。"),
            DenyReason::InvalidAgentId => "發送者或委派對象的資訊不完整，無法確認雙方的委派範圍。".to_string(),
            DenyReason::NoHierarchyRelation => format!(
                "「{s}」與「{t}」沒有上下級關係，也沒有適用的跨部門配對。"
            ),
            DenyReason::DifferentDepartment => format!(
                "「{s}」與「{t}」沒有上下級關係、分屬不同部門，也沒有適用的跨部門配對。"
            ),
            DenyReason::MissingDepartment => format!(
                "「{s}」與「{t}」沒有上下級關係，至少一方未設定部門，也沒有適用的跨部門配對。"
            ),
            DenyReason::ChainTooDeep => format!(
                "「{s}」與「{t}」的上下級關係超過 {MAX_ANCESTOR_HOPS} 層或形成循環，無法確認委派範圍。"
            ),
            DenyReason::HumanPrincipalNotPermitted => format!(
                "雖然「{s}」可以委派，但發出這個指令的人沒有權限把工作交給「{t}」，或無法確認其權限。"
            ),
            DenyReason::DelegationDisabled => {
                let parties = if s.is_empty() || t.is_empty() {
                    "這項跨員工操作".to_string()
                } else {
                    format!("「{s}」對「{t}」的這項操作")
                };
                format!("{parties}不符合角色例外，不能互相指派工作、傳訊或建立員工，跨部門配對在此模式下也不生效。")
            }
        };
        match audience {
            DenialAudience::Ai => format!(
                "委派遭拒：{rule}{reason}請向你正在協助的人說明，這件事需要另請有權限的人處理；也可請管理員查看「設定 › 委派權限」或員工的「委派範圍」卡片。"
            ),
            DenialAudience::User => format!("⛔ 這項委派未執行：{rule}{reason}"),
        }
    }

    /// Compatibility alias for AI-facing tool errors and audit messages.
    pub fn message_zh(&self) -> String {
        self.explain(DenialAudience::Ai)
    }
}

impl std::fmt::Display for DelegationDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message_zh())
    }
}

impl std::error::Error for DelegationDenied {}

// ── The predicate ────────────────────────────────────────────────────────────

/// May `sender` delegate work to `target`? See the module docs for the rule.
///
/// `Ok(())` = allowed. `Err(DelegationDenied)` carries the structured reason —
/// callers must surface it (fail-visible) and record a `delegation_denied`
/// audit entry.
pub fn can_delegate(
    policy: DelegationPolicy,
    org: &impl OrgView,
    sender: &str,
    target: &str,
) -> Result<(), DelegationDenied> {
    can_delegate_ext(policy, org, sender, target, &[])
}

/// [`can_delegate`] with additional sender ids treated as system senders for
/// this call only.
///
/// The hook for design doc §2.6: with `config.toml [acp] trusted = true` the
/// ACP entry point passes `&[ACP_CLIENT_SENDER]`. Everything else stays
/// identical, so the opt-in cannot accidentally widen any other rule.
pub fn can_delegate_ext(
    policy: DelegationPolicy,
    org: &impl OrgView,
    sender: &str,
    target: &str,
    extra_system_senders: &[&str],
) -> Result<(), DelegationDenied> {
    can_delegate_ext_impl(policy, None, org, sender, target, extra_system_senders, None).map(|_| ())
}

/// [`can_delegate`], plus the explicit cross-department/cross-hierarchy
/// whitelist from `[delegation] allow` (design doc §2.1 rule 5b).
///
/// `rules.policy` is the policy used for the whole decision — there is no
/// separate `policy` parameter, so a caller cannot accidentally evaluate the
/// whitelist against a different policy than the one it was resolved under.
pub fn can_delegate_rules(
    rules: &DelegationRules,
    org: &impl OrgView,
    sender: &str,
    target: &str,
) -> Result<(), DelegationDenied> {
    can_delegate_rules_ext(rules, org, sender, target, &[])
}

/// [`can_delegate_rules`] with additional sender ids treated as system
/// senders for this call only — the rules-aware twin of [`can_delegate_ext`],
/// for call sites (the gateway dispatcher) that need both the whitelist and a
/// gateway-internal system sender in the same call.
pub fn can_delegate_rules_ext(
    rules: &DelegationRules,
    org: &impl OrgView,
    sender: &str,
    target: &str,
    extra_system_senders: &[&str],
) -> Result<(), DelegationDenied> {
    can_delegate_ext_impl(
        rules.policy,
        Some(rules),
        org,
        sender,
        target,
        extra_system_senders,
        None,
    ).map(|_| ())
}

/// Evaluate always-deny → unrestricted role → pair → mode, returning the
/// winning rule. Empty ids and self-delegation are checked before every grant;
/// personal/main boundaries and `can_send_cross_agent` come from the trusted
/// caller and also run before system senders, roles, pairs or `open`.
///
/// D-Q2: a qualifying avatar AND qualifying requester/job initiator may pass
/// explicit `none`. Pairs never pass `none`. A reverse-only pair is explanatory,
/// not a veto of an otherwise allowed mode. Two-way wins if both pair kinds
/// match. Unknown/error role lookups grant no pass, then pairs/mode are tried.
/// Target ownership is never consulted to grant sender authority.
///
/// Load rules and overrides from the same `DelegationConfigRead` snapshot.
/// Legacy wrappers omit the new overrides and leave boundary checks to callers.
pub fn evaluate_delegation(
    rules: &DelegationRules,
    org: &impl OrgView,
    sender: &str,
    target: &str,
    context: &DelegationContext<'_>,
) -> Result<DelegationAllowSource, DelegationDenied> {
    can_delegate_ext_impl(
        rules.policy, Some(rules), org, sender, target,
        context.extra_system_senders, Some(context),
    )
}

/// Shared implementation behind [`can_delegate_ext`] and
/// [`can_delegate_rules_ext`]. `rules == None` is exactly "empty whitelist" —
/// it changes nothing else, which is what keeps the pre-T11 wrappers'
/// behaviour byte-for-byte identical.
fn can_delegate_ext_impl(
    policy: DelegationPolicy,
    rules: Option<&DelegationRules>,
    org: &impl OrgView,
    sender: &str,
    target: &str,
    extra_system_senders: &[&str],
    context: Option<&DelegationContext<'_>>,
) -> Result<DelegationAllowSource, DelegationDenied> {
    let s = sender.trim();
    let t = target.trim();

    let deny = |reason: DenyReason| {
        Err(DelegationDenied {
            sender: s.to_string(),
            target: t.to_string(),
            policy,
            reason,
        })
    };

    // Defensive (fail-closed): a nameless party proves nothing.
    if s.is_empty() || t.is_empty() {
        return deny(DenyReason::InvalidAgentId);
    }

    // ① Self-delegation — denied under every policy, Open included.
    if s == t {
        return deny(DenyReason::SelfDelegation);
    }

    // Always-deny facts are resolved outside core and never inferred from roles.
    if let Some(context) = context {
        if !context.boundaries.personal_target_allowed {
            return deny(DenyReason::PersonalTargetNotPermitted);
        }
        if !context.boundaries.main_target_allowed {
            return deny(DenyReason::MainTargetNotPermitted);
        }
        if !context.boundaries.can_send_cross_agent {
            return deny(DenyReason::CrossAgentSendDisabled);
        }
    }

    if is_system_sender(s) || extra_system_senders.iter().any(|extra| extra.trim() == s) {
        return Ok(DelegationAllowSource::SystemSender);
    }

    if let Some(context) = context {
        if !context.overrides.unrestricted_roles.is_empty()
            && context.owner_roles.is_some_and(|view| unrestricted_sender(view, context.overrides, s))
        {
            return Ok(DelegationAllowSource::UnrestrictedRole);
        }
    }

    // RM-137: `none` (and every value that resolves to it) stops every agent
    // sender here — before the ancestor rules and before the whitelist, so an
    // explicit `[delegation] allow` pair does not pass either.
    if policy == DelegationPolicy::Disabled {
        return deny(DenyReason::DelegationDisabled);
    }

    // Explicit pairs precede mode rules, but never override `none`.
    if rules.is_some_and(|rules| rules.allows_pair(s, t)) {
        return Ok(DelegationAllowSource::TwoWayPair);
    }
    let mut reverse_pair = false;
    if let Some(context) = context {
        if context.overrides.allow_one_way.iter().take(MAX_ONE_WAY_PAIRS)
            .any(|(from, to)| from == s && to == t)
        {
            return Ok(DelegationAllowSource::OneWayPair);
        }
        reverse_pair = context.overrides.allow_one_way.iter().take(MAX_ONE_WAY_PAIRS)
            .any(|(from, to)| from == t && to == s);
    }

    // Open skips rules ③–⑤b entirely (escape hatch = pre-WP21 behaviour).
    if policy == DelegationPolicy::Open {
        return Ok(DelegationAllowSource::Mode);
    }

    // ③ sender is an ancestor of target (skip-level command).
    let down = walk_up(org, t, s);
    if down == Walk::Found {
        return Ok(DelegationAllowSource::Mode);
    }

    // ④ target is an ancestor of sender (escalation / reporting up).
    let up = walk_up(org, s, t);
    if up == Walk::Found {
        return Ok(DelegationAllowSource::Mode);
    }

    // ⑤ same non-empty department (Department policy only).
    let sender_dept = org.department(s).unwrap_or_default();
    let target_dept = org.department(t).unwrap_or_default();
    let sender_dept = sender_dept.trim();
    let target_dept = target_dept.trim();
    if policy == DelegationPolicy::Department
        && !sender_dept.is_empty()
        && !target_dept.is_empty()
        && sender_dept == target_dept
    {
        return Ok(DelegationAllowSource::Mode);
    }

    if reverse_pair {
        return deny(DenyReason::ReversePairOnly);
    }

    // ⑥ Fail closed, with the most informative reason available.
    if down == Walk::Truncated || up == Walk::Truncated {
        return deny(DenyReason::ChainTooDeep);
    }
    match policy {
        DelegationPolicy::Hierarchy => deny(DenyReason::NoHierarchyRelation),
        DelegationPolicy::Department => {
            if sender_dept.is_empty() || target_dept.is_empty() {
                deny(DenyReason::MissingDepartment)
            } else {
                deny(DenyReason::DifferentDepartment)
            }
        }
        // Unreachable: Open returned Ok and Disabled denied above. Kept
        // exhaustive rather than wildcarded so a future policy variant must
        // state its denial reason.
        DelegationPolicy::Open => deny(DenyReason::NoHierarchyRelation),
        DelegationPolicy::Disabled => deny(DenyReason::DelegationDisabled),
    }
}

// ── Config ───────────────────────────────────────────────────────────────────

/// `config.toml [delegation]`.
///
/// The raw string is kept (rather than a parsed enum) so an unrecognised value
/// can be reported as a warning instead of failing the whole config load; use
/// [`DelegationConfig::resolve`] to get the policy plus that warning. Likewise
/// `allow` keeps each entry as a raw `Vec<String>` — a malformed pair (wrong
/// length, blank member, self-pair) is a per-entry warning via
/// [`resolve_allow_pairs`], never a reason to drop `policy` too.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DelegationConfig {
    /// `department` (default) / `hierarchy` / `open` / `none`. Anything else
    /// resolves to `none` (RM-137).
    pub policy: String,
    /// `allow = [["a", "b"], ...]` — unordered whitelist pairs (design doc
    /// §2.2). Defaults to empty when the key is absent.
    pub allow: Vec<Vec<String>>,
    /// Ordered pairs: only `from` may delegate to `to`, at most 200 entries.
    pub allow_one_way: Vec<Vec<String>>,
    /// Exact lowercase account roles; unknown values are dropped with a warning.
    pub unrestricted_roles: Vec<String>,
    /// Require a valid DuDuClaw-issued identity token (default true).
    /// Authorization reads this through `require_identity_token_from_home`,
    /// whose errors must reject the caller. Soft mode also requires a valid, bounded grace deadline.
    pub require_identity_token: bool,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            policy: DelegationPolicy::default().as_str().to_string(),
            allow: Vec::new(),
            allow_one_way: Vec::new(),
            unrestricted_roles: Vec::new(),
            require_identity_token: true,
        }
    }
}

/// Extract `[delegation]` fields from an already-parsed config table.
///
/// Factored out of [`DelegationConfig::from_home`] so a call site that also
/// needs *other* top-level sections from the same `config.toml` (e.g. the
/// gateway's C1 delegation gate, which reads `[delegation]` and `[acp]
/// trusted` on every dispatched task) can read and parse the file once and
/// extract every section from that one `toml::Table`, instead of each
/// extractor re-reading and re-parsing the whole file independently.
///
/// A missing `[delegation]` section or a missing `policy` key ⇒ the default
/// (`department`, empty `allow`). Since v1.82 (RM-137) a `[delegation]` that
/// is present but not a table, and a `policy` that is not a string, keep
/// their TOML rendering as the raw policy value, so they resolve like any
/// other unrecognised value: to `none`, with a warning.
pub fn delegation_config_from_table(table: &toml::Table) -> DelegationConfig {
    let section = match table.get("delegation") {
        None => return DelegationConfig::default(),
        Some(value) => match value.as_table() {
            Some(section) => section,
            None => {
                return DelegationConfig {
                    policy: value.to_string(),
                    ..DelegationConfig::default()
                };
            }
        },
    };

    let policy = match section.get("policy") {
        None => DelegationPolicy::default().as_str().to_string(),
        Some(toml::Value::String(s)) => s.clone(),
        // Not a string: unrecognised (fail closed), never the default.
        Some(other) => other.to_string(),
    };

    let parse_pairs = |key: &str| section
        .get(key)
        .and_then(|v| v.as_array())
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    entry
                        .as_array()
                        .map(|pair| {
                            pair.iter()
                                .map(|member| member.as_str().unwrap_or_default().to_string())
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default() // not-an-array entry ⇒ empty
                                             // Vec, which normalize_allow_entry
                                             // rejects with a clear "0 個" warning
                                             // rather than a parse failure.
                })
                .collect()
        })
        .unwrap_or_default();

    // Authorization uses the fallible reader below; this compatibility view
    // never weakens the default on malformed values.
    let require_identity_token = section
        .get("require_identity_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    DelegationConfig {
        policy,
        allow: parse_pairs("allow"),
        allow_one_way: match section.get("allow_one_way") {
            Some(value) if !value.is_array() => vec![Vec::new()],
            _ => parse_pairs("allow_one_way"),
        },
        unrestricted_roles: match section.get("unrestricted_roles") {
            None => Vec::new(),
            Some(toml::Value::Array(roles)) => roles.iter()
                .map(|role| role.as_str().unwrap_or_default().to_string()).collect(),
            Some(_) => vec![String::new()],
        },
        require_identity_token,
    }
}

impl DelegationConfig {
    /// Load `[delegation]` from `<home>/config.toml`. Parsed in isolation from a
    /// generic `toml::Table` (same shape as `DispatchGuardConfig::from_home`).
    /// No file ⇒ defaults (`department`, empty `allow`). Since v1.82
    /// (RM-137) a file that exists but cannot be read or parsed ⇒ policy
    /// `none` (a gate that cannot read its config denies);
    /// [`read_delegation_config`] also reports why.
    ///
    /// `policy` and `allow` are each pulled out of the raw table independently
    /// (rather than one `try_into::<DelegationConfig>()` call) so a
    /// wrong-shaped `allow` entry — e.g. a bare number instead of an array —
    /// can never make a *valid* `policy` disappear along with it; the whole
    /// point of "壞項忽略,不整段失效" (design doc §2.2) applies to the section
    /// as a whole, not just to individual `allow` pairs.
    pub fn from_home(home_dir: &Path) -> Self {
        match read_config_table(home_dir) {
            Ok(table) => delegation_config_from_table(&table),
            Err(_) => Self::unreadable(),
        }
    }

    /// The fail-closed stand-in for a config that could not be read.
    fn unreadable() -> Self {
        Self {
            policy: DelegationPolicy::Disabled.as_str().to_string(),
            ..Self::default()
        }
    }

    /// Resolved policy plus an optional warning about an unrecognised value.
    pub fn resolve(&self) -> (DelegationPolicy, Option<String>) {
        DelegationPolicy::from_config_value(&self.policy)
    }

    /// Resolved policy, dropping the warning.
    pub fn policy(&self) -> DelegationPolicy {
        self.resolve().0
    }

    /// Resolve the opt-in additions independently of the legacy rule shape.
    /// Unknown roles produce one aggregate warning and never widen access.
    /// An invalid policy discards all overrides (explicit `none` is different).
    pub fn resolve_overrides(&self) -> (DelegationOverrides, Vec<String>) {
        let (allow_one_way, mut warnings) = resolve_allow_one_way(&self.allow_one_way);
        let mut unrestricted_roles = Vec::new();
        let mut unknown = false;
        for role in &self.unrestricted_roles {
            if UNRESTRICTABLE_ROLE_NAMES.contains(&role.as_str()) {
                if !unrestricted_roles.contains(role) {
                    unrestricted_roles.push(role.clone());
                }
            } else {
                unknown = true;
            }
        }
        if unknown {
            warnings.push("config.toml [delegation] unrestricted_roles 含無法辨識的角色,已忽略;僅接受 admin / manager。".into());
        }
        if self.resolve().1.is_some() {
            return (DelegationOverrides::default(), warnings);
        }
        (DelegationOverrides { allow_one_way, unrestricted_roles }, warnings)
    }

    /// Resolved [`DelegationRules`] (policy + whitelist) plus every warning
    /// collected along the way — an unrecognised `policy` value first, then
    /// one line per dropped `allow` entry.
    pub fn resolve_rules(&self) -> (DelegationRules, Vec<String>) {
        let (policy, policy_warning) = self.resolve();
        let (allow_pairs, mut warnings) = resolve_allow_pairs(&self.allow);
        if let Some(w) = policy_warning {
            warnings.insert(0, w);
        }
        (
            DelegationRules {
                policy,
                allow_pairs,
            },
            warnings,
        )
    }
}

/// [`DelegationConfigRead::config_error_kind`]: `config.toml` exists but
/// could not be read (I/O error, permissions, not UTF-8).
pub const CONFIG_ERROR_UNREADABLE: &str = "unreadable";
/// [`DelegationConfigRead::config_error_kind`]: `config.toml` was read but is
/// not valid TOML.
pub const CONFIG_ERROR_UNPARSABLE: &str = "unparsable";

/// Read and parse `<home>/config.toml`. `Ok(empty)` when the file does not
/// exist; `Err((kind, reason))` when it exists but cannot be read or is not
/// valid TOML — `kind` is [`CONFIG_ERROR_UNREADABLE`] /
/// [`CONFIG_ERROR_UNPARSABLE`] (content-free), `reason` is zh-TW with no path
/// but MAY quote raw config text (the TOML parse excerpt).
fn read_config_table(home_dir: &Path) -> Result<toml::Table, (&'static str, String)> {
    match std::fs::read_to_string(home_dir.join("config.toml")) {
        Ok(content) => content.parse::<toml::Table>().map_err(|e| {
            (
                CONFIG_ERROR_UNPARSABLE,
                format!("config.toml 不是有效的 TOML:{}", crate::truncate_chars(&e.to_string(), 200)),
            )
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err((CONFIG_ERROR_UNREADABLE, format!("config.toml 無法讀取:{e}"))),
    }
}

/// Everything one read of `[delegation]` produced — the rules the gates
/// enforce plus what the dashboard needs to explain them (RM-137).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelegationConfigRead {
    /// New opt-in rules from the same config snapshot as `rules`.
    pub overrides: DelegationOverrides,
    pub override_warnings: Vec<String>,
    /// What every gate enforces.
    pub rules: DelegationRules,
    /// The `policy` value as written, when the key is present (non-string
    /// values in their TOML rendering).
    pub configured_policy: Option<String>,
    /// Set when the configured policy was not recognised (enforced as `none`).
    pub policy_warning: Option<String>,
    /// Content-free classification of the same policy problem for UI responses.
    pub classified_policy_warning: Option<DelegationWarning>,
    /// One line per dropped `[delegation] allow` entry.
    pub allow_warnings: Vec<String>,
    /// Set when `config.toml` exists but could not be read or parsed
    /// (enforced as `none`). May quote raw config text (the TOML parse
    /// excerpt): show it to an Admin only; everyone else gets
    /// [`Self::config_error_kind`].
    pub config_error: Option<String>,
    /// Content-free category of [`Self::config_error`]
    /// ([`CONFIG_ERROR_UNREADABLE`] / [`CONFIG_ERROR_UNPARSABLE`]); `Some`
    /// exactly when `config_error` is.
    pub config_error_kind: Option<&'static str>,
}

impl DelegationConfigRead {
    fn from_table(table: &toml::Table) -> Self {
        let cfg = delegation_config_from_table(table);
        let configured_policy = match table.get("delegation") {
            None => None,
            Some(v) => match v.as_table() {
                None => Some(v.to_string()),
                Some(section) => section.get("policy").map(|p| match p {
                    toml::Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            },
        };
        let (policy, policy_warning) = cfg.resolve();
        let (allow_pairs, allow_warnings) = resolve_allow_pairs(&cfg.allow);
        let (overrides, override_warnings) = cfg.resolve_overrides();
        Self {
            overrides,
            override_warnings,
            rules: DelegationRules {
                policy,
                allow_pairs,
            },
            configured_policy,
            policy_warning,
            classified_policy_warning: delegation_policy_warning(table),
            allow_warnings,
            config_error: None,
            config_error_kind: None,
        }
    }

    fn unreadable(kind: &'static str, reason: String) -> Self {
        Self {
            overrides: DelegationOverrides::default(),
            override_warnings: Vec::new(),
            rules: DelegationRules {
                policy: DelegationPolicy::Disabled,
                allow_pairs: Vec::new(),
            },
            configured_policy: None,
            policy_warning: None,
            classified_policy_warning: None,
            allow_warnings: Vec::new(),
            config_error: Some(format!(
                "{reason}。在修正前,AI 員工之間一律不能委派(視為 \"none\")。"
            )),
            config_error_kind: Some(kind),
        }
    }

    /// Log the policy-level problem once per process and every dropped
    /// `allow` entry (the pre-v1.82 behaviour for those).
    fn log(&self) {
        if let Some(e) = self.config_error.as_deref().or(self.policy_warning.as_deref()) {
            warn_policy_once(e);
        }
        for w in self.allow_warnings.iter().chain(&self.override_warnings) {
            tracing::warn!("{w}");
        }
    }
}

/// Read `<home>/config.toml [delegation]` for a gate or the dashboard.
/// No file ⇒ defaults; an unreadable or unparsable file ⇒ policy `none` with
/// [`DelegationConfigRead::config_error`] set. Never logs.
pub fn read_delegation_config(home_dir: &Path) -> DelegationConfigRead {
    match read_config_table(home_dir) {
        Ok(table) => DelegationConfigRead::from_table(&table),
        Err((kind, reason)) => DelegationConfigRead::unreadable(kind, reason),
    }
}

/// [`read_delegation_config`] over an already-parsed table (no
/// `config_error` is possible). Never logs.
pub fn delegation_config_read_from_table(table: &toml::Table) -> DelegationConfigRead {
    DelegationConfigRead::from_table(table)
}

/// Convenience for call sites that just need the effective policy: reads
/// `<home>/config.toml [delegation] policy` and logs a warning once per
/// process if the value is unrecognised or the file unreadable (both ⇒ `none`).
pub fn policy_from_home(home_dir: &Path) -> DelegationPolicy {
    let read = read_delegation_config(home_dir);
    read.log();
    read.rules.policy
}

/// Strict by default. Explicit false also requires a future UTC grace deadline
/// no more than 30 days away. An invalid grace deadline restores strict mode.
/// Once observed invalid or expired, that home's grace stays disabled for the
/// lifetime of this process, even after clock rollback or a config edit.
/// Unreadable files, invalid TOML and wrong-shaped settings are errors; callers
/// must reject rather than replacing an error with either boolean value.
pub fn require_identity_token_from_home(home_dir: &Path) -> std::io::Result<bool> {
    require_identity_token_from_home_at(home_dir, chrono::Utc::now())
}

fn require_identity_token_from_home_at(
    home_dir: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<bool> {
    let path = home_dir.join("config.toml");
    match std::fs::symlink_metadata(&path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(true),
        Err(error) => return Err(error),
        Ok(_) => {},
    }
    let content = std::fs::read_to_string(path)?;
    let invalid = || std::io::Error::new(std::io::ErrorKind::InvalidData,
        "invalid delegation identity-token setting");
    let table = content.parse::<toml::Table>().map_err(|_| invalid())?;
    let Some(section) = table.get("delegation") else { return Ok(true) };
    let section = section.as_table().ok_or_else(invalid)?;
    match section.get("require_identity_token") {
        None => Ok(true),
        Some(value) => {
            let require = value.as_bool().ok_or_else(invalid)?;
            if require { return Ok(true) }
            let valid = identity_grace_enabled(home_dir, section, now)?;
            if !valid {
                IDENTITY_GRACE_ERROR_LOGGED.call_once(|| tracing::error!(
                    "Ignoring require_identity_token=false: identity_token_grace_until must be an RFC3339 UTC string in the future and at most 30 days ahead; enforcing strict identity tokens for this process lifetime."
                ));
            }
            Ok(!valid)
        },
    }
}

static IDENTITY_GRACE_ERROR_LOGGED: std::sync::Once = std::sync::Once::new();

// Keep separate installations independent, but resolve path aliases so an
// alternate spelling of the same home cannot reopen its expired grace.
static IDENTITY_GRACE_DISABLED: std::sync::LazyLock<
    std::sync::Mutex<HashSet<std::path::PathBuf>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(HashSet::new()));

fn identity_grace_enabled(
    home_dir: &Path,
    section: &toml::Table,
    now: chrono::DateTime<chrono::Utc>,
) -> std::io::Result<bool> {
    let home = std::fs::canonicalize(home_dir)?;
    let mut disabled = IDENTITY_GRACE_DISABLED.lock()
        .map_err(|_| std::io::Error::other("identity grace latch unavailable"))?;
    if disabled.contains(&home) {
        return Ok(false);
    }
    if !identity_grace_valid(section, now) {
        disabled.insert(home);
        return Ok(false);
    }
    Ok(true)
}

fn identity_grace_valid(section: &toml::Table, now: chrono::DateTime<chrono::Utc>) -> bool {
    let Some(deadline) = section.get("identity_token_grace_until")
        .and_then(toml::Value::as_str)
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()) else { return false };
    deadline.offset().local_minus_utc() == 0
        && deadline > now
        && deadline.signed_duration_since(now) <= chrono::Duration::days(30)
}

/// Same as [`delegation_rules_from_home`], but takes an already-parsed table
/// — see [`delegation_config_from_table`] for why a call site would want
/// that (reading `[delegation]` alongside other sections in one file pass).
pub fn delegation_rules_from_table(table: &toml::Table) -> DelegationRules {
    let read = DelegationConfigRead::from_table(table);
    read.log();
    read.rules
}

/// Convenience for call sites that need the full [`DelegationRules`] (policy
/// + whitelist): reads `<home>/config.toml [delegation]`, logs the policy
/// problem (unrecognised value / unreadable file ⇒ `none`) once per process
/// and a warning for each dropped `allow` entry.
pub fn delegation_rules_from_home(home_dir: &Path) -> DelegationRules {
    let read = read_delegation_config(home_dir);
    read.log();
    read.rules
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_explanations_cover_every_reason_and_audience() {
        let cases = [
            (DenyReason::ReversePairOnly, "單向配對只允許「乙」委派給「甲」"),
            (DenyReason::PersonalTargetNotPermitted, "另一位成員的個人員工"),
            (DenyReason::MainTargetNotPermitted, "主代理的使用權限"),
            (DenyReason::CrossAgentSendDisabled, "允許跨 AI 員工通訊"),
            (DenyReason::SelfDelegation, "不可委派給自己"),
            (DenyReason::InvalidAgentId, "資訊不完整"),
            (DenyReason::NoHierarchyRelation, "沒有上下級關係"),
            (DenyReason::DifferentDepartment, "分屬不同部門"),
            (DenyReason::MissingDepartment, "至少一方未設定部門"),
            (DenyReason::ChainTooDeep, "層或形成循環"),
            (DenyReason::HumanPrincipalNotPermitted, "發出這個指令的人沒有權限"),
            (DenyReason::DelegationDisabled, "不符合角色例外"),
        ];
        for policy in DelegationPolicy::ALL {
            for (reason, expected) in cases {
                let denied = DelegationDenied {
                    sender: "甲".into(), target: "乙".into(), policy, reason,
                };
                for audience in [DenialAudience::Ai, DenialAudience::User] {
                    let text = denied.explain(audience);
                    assert!(!text.is_empty());
                    assert!(text.contains(expected), "{reason:?}: {text}");
                    for forbidden in ["開放", "open", "config.toml", "[delegation]", "policy", "reports_to", "agent.toml", "send_to_agent"] {
                        assert!(!text.contains(forbidden), "{reason:?}: {text}");
                    }
                    if policy == DelegationPolicy::Disabled {
                        assert!(text.contains("不允許委派"), "{text}");
                    }
                }
                let ai = denied.explain(DenialAudience::Ai);
                let user = denied.explain(DenialAudience::User);
                assert_eq!(denied.message_zh(), ai);
                assert!(ai.contains("請向你正在協助的人說明"));
                assert!(ai.contains("設定 › 委派權限") && ai.contains("委派範圍"));
                assert!(!user.contains("請向") && user.len() < ai.len());
            }
        }
    }

    /// ceo ─ sales_lead ─ sales_rep      (department 業務)
    ///     └ mkt_lead   ─ mkt_rep        (department 行銷)
    /// loner: no parent, no department.
    fn org() -> MapOrgView {
        MapOrgView::new()
            .with_agent("ceo", "", "")
            .with_agent("sales_lead", "ceo", "業務")
            .with_agent("sales_rep", "sales_lead", "業務")
            .with_agent("sales_rep2", "sales_lead", "業務")
            .with_agent("mkt_lead", "ceo", "行銷")
            .with_agent("mkt_rep", "mkt_lead", "行銷")
            .with_agent("loner", "", "")
    }

    const ALL: [DelegationPolicy; 3] = [
        DelegationPolicy::Department,
        DelegationPolicy::Hierarchy,
        DelegationPolicy::Open,
    ];

    fn allowed(policy: DelegationPolicy, sender: &str, target: &str) -> bool {
        can_delegate(policy, &org(), sender, target).is_ok()
    }

    fn denial(policy: DelegationPolicy, sender: &str, target: &str) -> DelegationDenied {
        can_delegate(policy, &org(), sender, target).expect_err("expected DENY")
    }

    #[test]
    fn direct_parent_child_both_directions_under_every_policy() {
        for p in ALL {
            assert!(allowed(p, "sales_lead", "sales_rep"), "{p:?} parent→child");
            assert!(allowed(p, "sales_rep", "sales_lead"), "{p:?} child→parent");
        }
    }

    #[test]
    fn skip_level_grandparent_to_grandchild_is_allowed() {
        // The regression the old direct-only check could not express.
        assert!(allowed(DelegationPolicy::Department, "ceo", "sales_rep"));
        assert!(allowed(DelegationPolicy::Hierarchy, "ceo", "sales_rep"));
    }

    #[test]
    fn grandchild_to_grandparent_is_allowed() {
        assert!(allowed(DelegationPolicy::Department, "sales_rep", "ceo"));
        assert!(allowed(DelegationPolicy::Hierarchy, "sales_rep", "ceo"));
    }

    #[test]
    fn same_department_peers_depend_on_policy() {
        // Siblings: no ancestor relation in either direction.
        assert!(allowed(
            DelegationPolicy::Department,
            "sales_rep",
            "sales_rep2"
        ));
        let d = denial(DelegationPolicy::Hierarchy, "sales_rep", "sales_rep2");
        assert_eq!(d.reason, DenyReason::NoHierarchyRelation);
        assert!(d.message_zh().contains("sales_rep2"));
    }

    #[test]
    fn cross_department_without_relation_is_denied() {
        let d = denial(DelegationPolicy::Department, "sales_rep", "mkt_rep");
        assert_eq!(d.reason, DenyReason::DifferentDepartment);
        assert_eq!(d.reason.as_str(), "different_department");
        assert!(
            denial(DelegationPolicy::Hierarchy, "sales_rep", "mkt_rep").reason
                == DenyReason::NoHierarchyRelation
        );
        // ...but the shared ancestor still commands both sides.
        assert!(allowed(DelegationPolicy::Department, "ceo", "mkt_rep"));
    }

    #[test]
    fn empty_department_is_not_a_department_match() {
        // Two department-less agents must NOT be treated as colleagues.
        let view = MapOrgView::new()
            .with_agent("a", "", "")
            .with_agent("b", "", "")
            .with_agent("c", "", "   "); // whitespace-only counts as empty too
        for (s, t) in [("a", "b"), ("a", "c"), ("c", "b")] {
            let err = can_delegate(DelegationPolicy::Department, &view, s, t)
                .expect_err("blank departments must not match");
            assert_eq!(err.reason, DenyReason::MissingDepartment);
        }
    }

    #[test]
    fn self_delegation_denied_under_every_policy() {
        for p in ALL {
            let d = denial(p, "sales_rep", "sales_rep");
            assert_eq!(d.reason, DenyReason::SelfDelegation, "{p:?}");
            assert!(d.message_zh().contains("不可委派給自己"));
        }
    }

    /// Standalone profile: `duduclaw mcp init` client ids are not employee
    /// names. Prefix match, case-insensitive; the bare word stays free.
    #[test]
    fn standalone_client_prefix_is_reserved() {
        for id in [
            "standalone-claude-code",
            "standalone-codex",
            "Standalone-X",
            " standalone-a ",
        ] {
            assert!(is_reserved_agent_id(id), "{id:?}");
        }
        for id in ["standalone", "my-standalone-bot", "standalon-x", "嘟嘟"] {
            assert!(!is_reserved_agent_id(id), "{id:?}");
        }
    }

    #[test]
    fn system_senders_are_allowed_everywhere() {
        for sender in SYSTEM_SENDERS {
            for p in ALL {
                assert!(allowed(p, sender, "mkt_rep"), "{sender} under {p:?}");
            }
            assert!(is_system_sender(sender));
            assert!(is_system_sender(&format!("  {sender}  ")));
            // ...and no real agent may claim the id in the first place.
            assert!(is_reserved_agent_id(sender));
        }
        // Not substring-matchable, and a2a-client is deliberately excluded.
        assert!(!is_system_sender("evil-dashboard"));
        assert!(!is_system_sender("dashboard-x"));
        assert!(!is_system_sender(ACP_CLIENT_SENDER));
        assert!(!allowed(
            DelegationPolicy::Department,
            ACP_CLIENT_SENDER,
            "mkt_rep"
        ));
        // ...unless explicitly trusted for this call (§2.6 hook).
        assert!(can_delegate_ext(
            DelegationPolicy::Department,
            &org(),
            ACP_CLIENT_SENDER,
            "mkt_rep",
            &[ACP_CLIENT_SENDER]
        )
        .is_ok());
        // Even a trusted system sender may not talk to itself.
        assert_eq!(
            denial(DelegationPolicy::Department, "cron", "cron").reason,
            DenyReason::SelfDelegation
        );
    }

    /// An agent named `cron` (or any other system-sender id) would inherit
    /// unconditional delegation reach, so the id space is closed to agents.
    #[test]
    fn system_sender_ids_are_reserved_against_agent_creation() {
        for id in SYSTEM_SENDERS {
            assert!(is_reserved_agent_id(id), "{id}");
            assert!(is_reserved_agent_id(&id.to_ascii_uppercase()), "{id}");
            assert!(is_reserved_agent_id(&format!("  {id} ")), "{id}");
        }
        assert!(is_reserved_agent_id(ACP_CLIENT_SENDER));
        assert!(is_reserved_agent_id("default"));
        assert!(is_reserved_agent_id("__deferred_gvu__"));
        // Ordinary ids — including ones that merely *contain* a reserved word —
        // stay creatable (no unanchored matching, project convention #2).
        for id in ["agnes", "sales-lead", "cron-helper", "my_dashboard", "defaults"] {
            assert!(!is_reserved_agent_id(id), "{id}");
        }
        assert!(!is_reserved_agent_id(""));
    }

    /// WP22 T3: `doctor-probe` is the id the doctor MCP cold-start probe
    /// spawns under (`duduclaw_gateway::doctor_probes::mcp_cold_start_probe`).
    /// It must be reserved against real agent creation the same way
    /// `cron`/`dashboard`/etc. are, case-insensitively and trim-insensitively.
    #[test]
    fn doctor_probe_id_is_reserved_against_agent_creation() {
        assert!(is_reserved_agent_id("doctor-probe"));
        assert!(is_reserved_agent_id("DOCTOR-PROBE"));
        assert!(is_reserved_agent_id("  doctor-probe  "));
        // Not an unanchored/substring match — a real agent can still be named
        // something that merely contains the reserved word.
        assert!(!is_reserved_agent_id("doctor-probe-2"));
        assert!(!is_reserved_agent_id("my-doctor-probe"));
    }

    /// P2-A L3-2: queue sender names the dispatcher acts on cannot be
    /// employee names (an employee called `heartbeat-scheduler` would send
    /// rows the dispatcher reads as heartbeat wake-ups).
    #[test]
    fn queue_sender_names_are_reserved() {
        for name in RESERVED_QUEUE_SENDERS {
            assert!(is_reserved_agent_id(name), "{name}");
            assert!(is_reserved_agent_id(&name.to_ascii_uppercase()), "{name}");
        }
        assert!(is_reserved_agent_id("goal-loop-driver"));
        assert!(!is_reserved_agent_id("heartbeat-scheduler-2"));
    }

    #[test]
    fn reports_to_cycle_terminates_and_decides_correctly() {
        // a → b → a. Any walk must terminate.
        let view = MapOrgView::new()
            .with_agent("a", "b", "")
            .with_agent("b", "a", "")
            .with_agent("outsider", "", "");
        // Mutual parents: both directions are "ancestor", so both are allowed.
        assert!(can_delegate(DelegationPolicy::Hierarchy, &view, "a", "b").is_ok());
        assert!(can_delegate(DelegationPolicy::Hierarchy, &view, "b", "a").is_ok());
        // An unrelated agent is denied without looping forever.
        let d = can_delegate(DelegationPolicy::Hierarchy, &view, "a", "outsider")
            .expect_err("unrelated must be denied");
        assert_eq!(d.reason, DenyReason::NoHierarchyRelation);
        assert!(!is_ancestor(&view, "outsider", "a"));
    }

    #[test]
    fn chain_longer_than_max_hops_is_truncated_and_fails_closed() {
        // n0 (root) ← n1 ← n2 … ← n30, each reporting to its predecessor.
        let mut view = MapOrgView::new();
        view.insert("n0", "", "");
        for i in 1..=30u32 {
            view.insert(format!("n{i}"), format!("n{}", i - 1), "");
        }
        // Inside the ceiling: proven.
        assert!(can_delegate(DelegationPolicy::Hierarchy, &view, "n0", "n20").is_ok());
        assert!(is_ancestor(&view, "n0", "n20"));
        // Beyond it: unproven ⇒ DENY (fail-closed), not an infinite walk.
        let d = can_delegate(DelegationPolicy::Hierarchy, &view, "n0", "n30")
            .expect_err("beyond the hop ceiling must fail closed");
        assert_eq!(d.reason, DenyReason::ChainTooDeep);
        assert!(d.message_zh().contains("20"));
        assert!(!is_ancestor(&view, "n0", "n30"));
        // Same in the escalation direction.
        assert_eq!(
            can_delegate(DelegationPolicy::Hierarchy, &view, "n30", "n0")
                .expect_err("escalation beyond the ceiling")
                .reason,
            DenyReason::ChainTooDeep
        );
    }

    #[test]
    fn open_policy_allows_everything_but_self() {
        assert!(allowed(DelegationPolicy::Open, "sales_rep", "mkt_rep"));
        assert!(allowed(DelegationPolicy::Open, "loner", "ceo"));
        // Unknown agents too — Open performs no org lookups at all.
        assert!(allowed(DelegationPolicy::Open, "ghost1", "ghost2"));
        assert_eq!(
            denial(DelegationPolicy::Open, "loner", "loner").reason,
            DenyReason::SelfDelegation
        );
    }

    #[test]
    fn unknown_agents_fail_closed_under_restrictive_policies() {
        for p in [DelegationPolicy::Department, DelegationPolicy::Hierarchy] {
            assert!(!allowed(p, "ghost", "sales_rep"), "{p:?}");
            assert!(!allowed(p, "sales_rep", "ghost"), "{p:?}");
        }
        // Empty ids never pass.
        for p in ALL {
            assert_eq!(
                denial(p, "", "sales_rep").reason,
                DenyReason::InvalidAgentId
            );
            assert_eq!(
                denial(p, "sales_rep", "   ").reason,
                DenyReason::InvalidAgentId
            );
        }
    }

    #[test]
    fn root_relation_normalisation() {
        // "none" and "" both mean root: a "none"-parented agent has no ancestor.
        let view = MapOrgView::new()
            .with_agent("root", "none", "")
            .with_agent("kid", "root", "")
            .with_agent("other", "NONE", "");
        assert!(can_delegate(DelegationPolicy::Hierarchy, &view, "root", "kid").is_ok());
        assert!(can_delegate(DelegationPolicy::Hierarchy, &view, "root", "other").is_err());
    }

    #[test]
    fn policy_string_parsing_including_unknown_values() {
        assert_eq!(
            DelegationPolicy::parse("department"),
            Some(DelegationPolicy::Department)
        );
        assert_eq!(
            DelegationPolicy::parse(" Hierarchy "),
            Some(DelegationPolicy::Hierarchy)
        );
        assert_eq!(
            DelegationPolicy::parse("OPEN"),
            Some(DelegationPolicy::Open)
        );
        assert_eq!(DelegationPolicy::parse("closed"), None);
        assert_eq!(
            DelegationPolicy::parse(" None "),
            Some(DelegationPolicy::Disabled)
        );
        assert_eq!(DelegationPolicy::default(), DelegationPolicy::Department);
        for p in DelegationPolicy::ALL {
            assert_eq!(DelegationPolicy::parse(p.as_str()), Some(p));
        }

        // RM-137: an unrecognised value fails closed to `none`, not the
        // wider default.
        let (p, warn) = DelegationPolicy::from_config_value("nonsense");
        assert_eq!(p, DelegationPolicy::Disabled);
        let warn = warn.expect("warning");
        assert!(warn.contains("nonsense") && warn.contains("none"), "{warn}");
        assert!(DelegationPolicy::from_config_value("open").1.is_none());
        assert!(DelegationPolicy::from_config_value("none").1.is_none());
    }

    #[test]
    fn config_section_parsing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // No config file at all ⇒ default.
        assert_eq!(
            DelegationConfig::from_home(home).policy(),
            DelegationPolicy::Department
        );

        // Section absent ⇒ default.
        std::fs::write(
            home.join("config.toml"),
            "[general]\nlog_level = \"info\"\n",
        )
        .unwrap();
        assert_eq!(
            DelegationConfig::from_home(home).policy(),
            DelegationPolicy::Department
        );

        // Explicit value.
        std::fs::write(
            home.join("config.toml"),
            "[general]\nlog_level = \"info\"\n\n[delegation]\npolicy = \"open\"\n",
        )
        .unwrap();
        assert_eq!(
            DelegationConfig::from_home(home).policy(),
            DelegationPolicy::Open
        );
        assert_eq!(policy_from_home(home), DelegationPolicy::Open);

        // Unknown value ⇒ `none` + warning (RM-137: fail closed).
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"anarchy\"\n",
        )
        .unwrap();
        let (p, warn) = DelegationConfig::from_home(home).resolve();
        assert_eq!(p, DelegationPolicy::Disabled);
        assert!(warn.is_some());
        assert_eq!(policy_from_home(home), DelegationPolicy::Disabled);

        // Wrong type / malformed section / unparsable file ⇒ `none`, never a
        // panic and never the wider default.
        for body in [
            "[delegation]\npolicy = 42\n",
            "[delegation]\npolicy = \"\"\n",
            "delegation = false\n",
            "not = valid = toml",
        ] {
            std::fs::write(home.join("config.toml"), body).unwrap();
            assert_eq!(
                DelegationConfig::from_home(home).policy(),
                DelegationPolicy::Disabled,
                "{body:?}"
            );
            assert_eq!(policy_from_home(home), DelegationPolicy::Disabled, "{body:?}");
        }
    }

    // ── RM-137: `policy = "none"` ────────────────────────────────────────────

    /// The full matrix under `none`: every agent pair is denied with
    /// `delegation_disabled` — parent→child, child→parent, same department,
    /// cross-department, and an explicit allow pair in both directions.
    #[test]
    fn none_policy_denies_every_agent_pair_including_allow_pairs() {
        let rules = rules_with(
            DelegationPolicy::Disabled,
            &[("sales_lead", "mkt_lead"), ("sales_rep", "mkt_rep")],
        );
        for (s, t) in [
            ("sales_lead", "sales_rep"),
            ("sales_rep", "sales_lead"),
            ("ceo", "sales_rep"),
            ("sales_rep", "ceo"),
            ("sales_rep", "sales_rep2"),
            ("sales_rep", "mkt_rep"),
            ("sales_lead", "mkt_lead"),
            ("mkt_lead", "sales_lead"),
            ("mkt_rep", "sales_rep"),
            ("ghost", "sales_rep"),
        ] {
            let d = can_delegate_rules(&rules, &org(), s, t)
                .expect_err("`none` must deny every agent pair");
            assert_eq!(d.reason, DenyReason::DelegationDisabled, "{s} -> {t}");
            assert_eq!(d.reason.as_str(), "delegation_disabled");
            assert_eq!(d.policy, DelegationPolicy::Disabled);
            // The bare predicate agrees with the rules-aware one.
            assert!(can_delegate(DelegationPolicy::Disabled, &org(), s, t).is_err());
        }
        // The same pairs under the other policies are what the matrix says
        // they are — `none` is the only one that refuses the allow pair.
        for p in [DelegationPolicy::Department, DelegationPolicy::Hierarchy] {
            let r = rules_with(p, &[("sales_lead", "mkt_lead")]);
            assert!(can_delegate_rules(&r, &org(), "sales_lead", "mkt_lead").is_ok(), "{p:?}");
        }
        assert!(DelegationPolicy::Department.honors_allow_pairs());
        assert!(DelegationPolicy::Hierarchy.honors_allow_pairs());
        assert!(!DelegationPolicy::Disabled.honors_allow_pairs());
    }

    /// People and configured schedules are not AI employees: they still
    /// address any employee under `none`. Self-delegation keeps its own
    /// reason; a trusted ACP client (an outside party, not an employee)
    /// keeps the reach `[acp] trusted` gives it.
    #[test]
    fn none_policy_still_lets_system_senders_through() {
        for sender in SYSTEM_SENDERS {
            assert!(
                can_delegate(DelegationPolicy::Disabled, &org(), sender, "mkt_rep").is_ok(),
                "{sender}"
            );
        }
        assert!(can_delegate_ext(
            DelegationPolicy::Disabled,
            &org(),
            ACP_CLIENT_SENDER,
            "mkt_rep",
            &[ACP_CLIENT_SENDER]
        )
        .is_ok());
        assert_eq!(
            denial(DelegationPolicy::Disabled, "sales_rep", "sales_rep").reason,
            DenyReason::SelfDelegation
        );
        assert_eq!(
            denial(DelegationPolicy::Disabled, ACP_CLIENT_SENDER, "mkt_rep").reason,
            DenyReason::DelegationDisabled
        );
    }

    #[test]
    fn none_denial_message_explains_the_policy_without_paths() {
        let d = denial(DelegationPolicy::Disabled, "sales_rep", "mkt_rep");
        let msg = d.message_zh();
        assert!(msg.contains("sales_rep") && msg.contains("mkt_rep"), "{msg}");
        assert!(msg.contains("不允許委派") && msg.contains("委派權限"), "{msg}");
        assert!(!msg.contains("agent.toml") && !msg.contains('/'), "{msg}");
    }

    /// `read_delegation_config` — the reader behind the gates and
    /// `delegation.effective`: the raw value, the warning and the config
    /// error are reported, and each of them enforces `none`.
    #[test]
    fn read_delegation_config_reports_why_the_policy_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // No file: defaults, no error.
        let read = read_delegation_config(home);
        assert_eq!(read.rules.policy, DelegationPolicy::Department);
        assert!(read.config_error.is_none() && read.policy_warning.is_none());
        assert!(read.config_error_kind.is_none());
        assert!(read.configured_policy.is_none());

        // Explicit `none` with pairs: the pairs are still parsed (the
        // dashboard shows them as inactive) but the policy is `none`.
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"none\"\nallow = [[\"a\", \"b\"]]\n",
        )
        .unwrap();
        let read = read_delegation_config(home);
        assert_eq!(read.rules.policy, DelegationPolicy::Disabled);
        assert_eq!(read.configured_policy.as_deref(), Some("none"));
        assert_eq!(read.rules.allow_pairs, vec![("a".to_string(), "b".to_string())]);
        assert!(read.policy_warning.is_none() && read.config_error.is_none());
        assert!(can_delegate_rules(&read.rules, &MapOrgView::new(), "a", "b").is_err());

        // Unknown value: warning, enforced as none.
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy = \"closed\"\n").unwrap();
        let read = read_delegation_config(home);
        assert_eq!(read.rules.policy, DelegationPolicy::Disabled);
        assert_eq!(read.configured_policy.as_deref(), Some("closed"));
        assert!(read.policy_warning.as_deref().is_some_and(|w| w.contains("closed")));
        assert!(read.config_error.is_none());

        // Unparsable file: config_error, enforced as none, no pairs.
        std::fs::write(home.join("config.toml"), "[delegation\npolicy = \"open\"\n").unwrap();
        let read = read_delegation_config(home);
        assert_eq!(read.rules.policy, DelegationPolicy::Disabled);
        assert!(read.rules.allow_pairs.is_empty());
        assert!(read.config_error.is_some());
        assert_eq!(read.config_error_kind, Some(CONFIG_ERROR_UNPARSABLE));
        assert_eq!(delegation_rules_from_home(home).policy, DelegationPolicy::Disabled);

        // Unreadable (a directory where the file should be): same.
        std::fs::remove_file(home.join("config.toml")).unwrap();
        std::fs::create_dir(home.join("config.toml")).unwrap();
        let read = read_delegation_config(home);
        assert_eq!(read.rules.policy, DelegationPolicy::Disabled);
        assert!(read.config_error.is_some());
        assert_eq!(read.config_error_kind, Some(CONFIG_ERROR_UNREADABLE));
    }

    #[test]
    fn require_identity_token_defaults_strict_and_rejects_invalid_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert!(require_identity_token_from_home(home).unwrap());
        for body in ["", "[delegation]\npolicy = 'open'\n", "[general]\nlog_level = 'info'\n",
            "[delegation]\nrequire_identity_token = true\nallow = [42]\n"] {
            std::fs::write(home.join("config.toml"), body).unwrap();
            assert!(require_identity_token_from_home(home).unwrap());
        }
        std::fs::write(home.join("config.toml"), "[delegation]\nrequire_identity_token = false\n").unwrap();
        assert!(require_identity_token_from_home(home).unwrap());
        for body in ["[", "delegation = false", "[delegation]\nrequire_identity_token = 'false'",
            "[delegation]\nrequire_identity_token = 0"] {
            std::fs::write(home.join("config.toml"), body).unwrap();
            assert!(require_identity_token_from_home(home).is_err());
        }
        std::fs::remove_file(home.join("config.toml")).unwrap();
        std::fs::create_dir(home.join("config.toml")).unwrap();
        assert!(require_identity_token_from_home(home).is_err());
    }

    #[test]
    fn identity_grace_has_exact_utc_time_bounds() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z").unwrap().to_utc();
        for (deadline, expected) in [
            ("2026-11-05T00:00:00Z", false),
            ("2026-11-04T23:59:59Z", false),
            ("2026-11-05T00:00:01Z", true),
            ("2026-12-05T00:00:00Z", true),
            ("2026-12-05T00:00:00.000000001Z", false),
            ("2026-11-06T00:00:00+00:00", true),
            ("2026-11-06T08:00:00+08:00", false),
            ("2026-11-06", false), ("invalid", false),
        ] {
            let mut section = toml::Table::new();
            section.insert("identity_token_grace_until".into(), toml::Value::String(deadline.into()));
            assert_eq!(identity_grace_valid(&section, now), expected, "{deadline}");
        }
    }

    #[test]
    fn identity_grace_expiry_is_irreversible_after_clock_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(home.join("config.toml"),
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = '2026-11-06T00:00:00Z'\n").unwrap();
        let before = chrono::DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z").unwrap().to_utc();
        let expired = before + chrono::Duration::days(1);
        assert!(!require_identity_token_from_home_at(home, before).unwrap());
        assert!(require_identity_token_from_home_at(home, expired).unwrap());
        assert!(require_identity_token_from_home_at(home, before).unwrap());
        assert!(require_identity_token_from_home_at(&home.join("."), before).unwrap());

        // Even replacing the expired deadline cannot reopen grace in this process.
        std::fs::write(home.join("config.toml"),
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = '2026-11-07T00:00:00Z'\n").unwrap();
        assert!(require_identity_token_from_home_at(home, before).unwrap());
    }

    #[test]
    fn identity_grace_invalid_deadlines_latch_without_affecting_other_homes() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-05T00:00:00Z").unwrap().to_utc();
        for deadline in [None, Some("invalid"), Some("2026-11-06T08:00:00+08:00"),
            Some("2026-12-06T00:00:00Z")] {
            let dir = tempfile::tempdir().unwrap();
            let mut body = "[delegation]\nrequire_identity_token = false\n".to_string();
            if let Some(deadline) = deadline {
                body.push_str(&format!("identity_token_grace_until = '{deadline}'\n"));
            }
            std::fs::write(dir.path().join("config.toml"), body).unwrap();
            assert!(require_identity_token_from_home_at(dir.path(), now).unwrap());
            std::fs::write(dir.path().join("config.toml"),
                "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = '2026-11-06T00:00:00Z'\n").unwrap();
            assert!(require_identity_token_from_home_at(dir.path(), now).unwrap());
        }
        let other = tempfile::tempdir().unwrap();
        std::fs::write(other.path().join("config.toml"),
            "[delegation]\nrequire_identity_token = false\nidentity_token_grace_until = '2026-11-06T00:00:00Z'\n").unwrap();
        assert!(!require_identity_token_from_home_at(other.path(), now).unwrap());
    }

    #[test]
    fn denied_message_names_both_parties_and_a_remedy() {
        let d = denial(DelegationPolicy::Department, "sales_rep", "mkt_rep");
        let msg = d.message_zh();
        assert!(msg.contains("sales_rep") && msg.contains("mkt_rep"));
        assert!(msg.contains("部門模式") && msg.contains("不同部門"));
        assert!(msg.contains("設定 › 委派權限") && msg.contains("委派範圍"));
        // No internal paths leak into operator-facing text.
        assert!(!msg.contains("agent.toml") && !msg.contains('/'));
        assert_eq!(d.to_string(), msg);
    }

    // ── WP21 T11: whitelist (design doc §2.1 rule 5b, §2.2) ────────────────

    fn rules_with(policy: DelegationPolicy, pairs: &[(&str, &str)]) -> DelegationRules {
        DelegationRules {
            policy,
            allow_pairs: pairs.iter().map(|(a, b)| sorted_pair(a, b)).collect(),
        }
    }

    /// The scenario the design doc names explicitly: two parallel department
    /// leads (no ancestor relation, different departments) get an explicit
    /// exception. Must work in both directions from one config entry.
    #[test]
    fn whitelist_allows_cross_department_leads_bidirectionally() {
        let rules = rules_with(DelegationPolicy::Department, &[("sales_lead", "mkt_lead")]);
        assert!(can_delegate_rules(&rules, &org(), "sales_lead", "mkt_lead").is_ok());
        assert!(can_delegate_rules(&rules, &org(), "mkt_lead", "sales_lead").is_ok());
    }

    /// `hierarchy` drops the same-department shortcut but must still respect
    /// an explicit whitelist pair (design doc: "hierarchy 政策也尊重白名單").
    #[test]
    fn whitelist_respected_under_hierarchy_policy() {
        let rules = rules_with(DelegationPolicy::Hierarchy, &[("sales_lead", "mkt_lead")]);
        assert!(can_delegate_rules(&rules, &org(), "sales_lead", "mkt_lead").is_ok());
        assert!(can_delegate_rules(&rules, &org(), "mkt_lead", "sales_lead").is_ok());
        // Siblings not on the list are still denied under hierarchy.
        assert!(can_delegate_rules(&rules, &org(), "sales_rep", "sales_rep2").is_err());
    }

    /// A third party — including one on the *same* department axis as a
    /// whitelisted pair — earns nothing from someone else's exception.
    #[test]
    fn whitelist_does_not_leak_to_a_third_party() {
        let rules = rules_with(DelegationPolicy::Department, &[("sales_lead", "mkt_lead")]);
        // sales_rep is sales_lead's own child, not part of the whitelisted
        // pair itself, and mkt_lead is not its ancestor nor same department.
        let d = can_delegate_rules(&rules, &org(), "sales_rep", "mkt_lead")
            .expect_err("whitelist must not extend past the exact pair");
        assert_eq!(d.reason, DenyReason::DifferentDepartment);
        let d = can_delegate_rules(&rules, &org(), "mkt_rep", "sales_lead")
            .expect_err("whitelist must not extend past the exact pair");
        assert_eq!(d.reason, DenyReason::DifferentDepartment);
    }

    /// An empty whitelist is exactly [`can_delegate_ext`]'s behaviour — the
    /// rules-aware predicate must not diverge when there is nothing to add.
    #[test]
    fn empty_whitelist_matches_can_delegate_ext_exactly() {
        let rules = DelegationRules::default(); // Department, no pairs
        for (s, t) in [
            ("sales_lead", "sales_rep"),
            ("ceo", "mkt_rep"),
            ("sales_rep", "mkt_rep"),
            ("sales_rep", "sales_rep2"),
        ] {
            assert_eq!(
                can_delegate_rules(&rules, &org(), s, t).is_ok(),
                can_delegate_ext(DelegationPolicy::Department, &org(), s, t, &[]).is_ok(),
                "{s} -> {t} diverged"
            );
        }
    }

    /// Self-delegation is denied even for a pair that is otherwise
    /// whitelisted — rule ① still runs before the whitelist is ever
    /// consulted. `normalize_allow_entry` already refuses to produce a
    /// self-pair from config (covered by
    /// `resolve_allow_pairs_drops_malformed_entries_with_warnings`); this
    /// test constructs one directly to prove rule ① is still the reason a
    /// hand-built `DelegationRules` cannot be used to bypass it.
    #[test]
    fn whitelist_pair_still_denies_self_delegation() {
        let rules = DelegationRules {
            policy: DelegationPolicy::Department,
            allow_pairs: vec![("loner".to_string(), "loner".to_string())],
        };
        assert_eq!(
            can_delegate_rules(&rules, &org(), "loner", "loner")
                .unwrap_err()
                .reason,
            DenyReason::SelfDelegation
        );
    }

    #[test]
    fn allows_pair_is_order_independent_and_trims_and_rejects_blank() {
        let rules = rules_with(DelegationPolicy::Department, &[("a", "b")]);
        assert!(rules.allows_pair("a", "b"));
        assert!(rules.allows_pair("b", "a"));
        assert!(rules.allows_pair(" a ", " b ")); // trimmed
        assert!(!rules.allows_pair("a", "c"));
        assert!(!rules.allows_pair("a", "a"));
        assert!(!rules.allows_pair("", "b"));
    }

    #[test]
    fn resolve_allow_pairs_drops_malformed_entries_with_warnings() {
        let entries = vec![
            vec!["sales-lead".to_string(), "warehouse-lead".to_string()], // ok
            vec!["a".to_string()],                                        // wrong length
            vec!["a".to_string(), "b".to_string(), "c".to_string()],      // wrong length
            vec![String::new(), "x".to_string()],                         // blank member
            vec!["x".to_string(), "   ".to_string()],                     // whitespace-only
            vec!["dup".to_string(), "loop".to_string()],                  // self-pair below
            vec!["me".to_string(), "me".to_string()],                     // self-pair
            vec!["loop".to_string(), "dup".to_string()], // duplicate of #1 (unordered)
        ];
        let (pairs, warnings) = resolve_allow_pairs(&entries);
        assert_eq!(
            pairs,
            vec![
                ("sales-lead".to_string(), "warehouse-lead".to_string()),
                ("dup".to_string(), "loop".to_string()),
            ],
            "bad entries dropped, good ones kept, unordered duplicate deduped"
        );
        assert_eq!(
            warnings.len(),
            5,
            "one warning per dropped entry: {warnings:?}"
        );
        assert!(warnings
            .iter()
            .any(|w| w.contains('2') && w.contains("實際 1")));
        assert!(warnings.iter().any(|w| w.contains("實際 3")));
        assert!(warnings.iter().any(|w| w.contains("空字串")));
        assert!(warnings.iter().any(|w| w.contains("自我配對")));
    }

    #[test]
    fn config_without_allow_key_resolves_to_empty_whitelist() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"department\"\n",
        )
        .unwrap();
        let cfg = DelegationConfig::from_home(home);
        assert!(cfg.allow.is_empty());
        let (rules, warnings) = cfg.resolve_rules();
        assert!(rules.allow_pairs.is_empty());
        assert!(warnings.is_empty());
        assert_eq!(delegation_rules_from_home(home), rules);
    }

    #[test]
    fn config_parses_allow_and_ignores_bad_entries_without_losing_policy() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.toml"),
            r#"[delegation]
policy = "hierarchy"
allow = [["sales-lead", "warehouse-lead"], ["a"], ["x", "x"]]
"#,
        )
        .unwrap();
        let cfg = DelegationConfig::from_home(home);
        let (rules, warnings) = cfg.resolve_rules();
        // The malformed entries did not take `policy` down with them.
        assert_eq!(rules.policy, DelegationPolicy::Hierarchy);
        assert_eq!(
            rules.allow_pairs,
            vec![("sales-lead".to_string(), "warehouse-lead".to_string())]
        );
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(delegation_rules_from_home(home), rules);
    }

    /// A wrong-shaped `allow` value at the TOML level (not even an array of
    /// arrays) must not take `policy` down with it either.
    #[test]
    fn config_survives_allow_of_the_wrong_toml_shape() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"open\"\nallow = \"not-a-list\"\n",
        )
        .unwrap();
        let cfg = DelegationConfig::from_home(home);
        assert_eq!(cfg.policy(), DelegationPolicy::Open);
        assert!(cfg.allow.is_empty());

        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"open\"\nallow = [1, 2, [\"a\", \"b\"]]\n",
        )
        .unwrap();
        let cfg = DelegationConfig::from_home(home);
        assert_eq!(cfg.policy(), DelegationPolicy::Open);
        let (rules, warnings) = cfg.resolve_rules();
        assert_eq!(rules.allow_pairs, vec![("a".to_string(), "b".to_string())]);
        assert_eq!(warnings.len(), 2, "{warnings:?}"); // the two non-array entries
    }

    const CHECKED_BOUNDARIES: DelegationBoundaries = DelegationBoundaries {
        personal_target_allowed: true,
        main_target_allowed: true,
        can_send_cross_agent: true,
    };

    struct TestOwnerRoles {
        avatar: &'static str,
        owner: Result<Option<String>, OwnerRoleLookupError>,
        requester: Result<Option<DelegationRequesterRole>, OwnerRoleLookupError>,
    }

    impl OwnerRoleView for TestOwnerRoles {
        fn avatar_owner_role(&self, agent_id: &str) -> Result<Option<String>, OwnerRoleLookupError> {
            if agent_id == self.avatar { self.owner.clone() } else { Ok(None) }
        }
        fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError> {
            self.requester.clone()
        }
    }

    fn qualifying_roles() -> TestOwnerRoles {
        TestOwnerRoles {
            avatar: "sales_rep",
            owner: Ok(Some("admin".into())),
            requester: Ok(Some(DelegationRequesterRole::Account("admin".into()))),
        }
    }

    fn evaluate_test(
        rules: &DelegationRules,
        overrides: &DelegationOverrides,
        roles: Option<&dyn OwnerRoleView>,
        sender: &str,
        target: &str,
        boundaries: DelegationBoundaries,
    ) -> Result<DelegationAllowSource, DelegationDenied> {
        evaluate_delegation(rules, &org(), sender, target, &DelegationContext {
            overrides, owner_roles: roles, boundaries, extra_system_senders: &[],
        })
    }

    #[test]
    fn delegation_roles_pairs_modes_and_always_deny_matrix() {
        // 4 policies × 4 sender kinds (including attributed background) ×
        // 5 relations × 7 boundary cases = 560 decisions, with exact reasons.
        let mut decisions = 0;
        for policy in DelegationPolicy::ALL {
            for kind in 0..4 {
                let mut roles = qualifying_roles();
                match kind {
                    1 => roles.requester = Ok(Some(DelegationRequesterRole::Account("employee".into()))),
                    2 => roles.owner = Ok(None), // Ordinary AI, even with an admin requester.
                    3 => roles.requester = Ok(Some(DelegationRequesterRole::BackgroundStartedBy("admin".into()))),
                    _ => {}
                }
                for relation in 0..5 {
                    let target = if relation == 0 { "sales_rep2" } else { "mkt_rep" };
                    let rules = rules_with(policy, if relation == 2 { &[("sales_rep", "mkt_rep")] } else { &[] });
                    let overrides = DelegationOverrides {
                        unrestricted_roles: vec!["admin".into()],
                        allow_one_way: match relation {
                            3 => vec![("sales_rep".into(), target.into())],
                            4 => vec![(target.into(), "sales_rep".into())],
                            _ => Vec::new(),
                        },
                    };
                    let expected = if kind == 0 || kind == 3 {
                        Ok(DelegationAllowSource::UnrestrictedRole)
                    } else if policy == DelegationPolicy::Disabled {
                        Err(DenyReason::DelegationDisabled)
                    } else if relation == 2 {
                        Ok(DelegationAllowSource::TwoWayPair)
                    } else if relation == 3 {
                        Ok(DelegationAllowSource::OneWayPair)
                    } else if policy == DelegationPolicy::Open
                        || (policy == DelegationPolicy::Department && relation == 0) {
                        Ok(DelegationAllowSource::Mode)
                    } else if relation == 4 {
                        Err(DenyReason::ReversePairOnly)
                    } else if policy == DelegationPolicy::Hierarchy {
                        Err(DenyReason::NoHierarchyRelation)
                    } else {
                        Err(DenyReason::DifferentDepartment)
                    };
                    for boundary in 0..7 {
                        let mut checked = CHECKED_BOUNDARIES;
                        let (sender, target, result) = match boundary {
                            1 => ("sales_rep", "sales_rep", Err(DenyReason::SelfDelegation)),
                            2 => (" ", target, Err(DenyReason::InvalidAgentId)),
                            3 => ("sales_rep", " ", Err(DenyReason::InvalidAgentId)),
                            4 => {
                                checked.personal_target_allowed = false;
                                ("sales_rep", target, Err(DenyReason::PersonalTargetNotPermitted))
                            }
                            5 => {
                                checked.main_target_allowed = false;
                                ("sales_rep", target, Err(DenyReason::MainTargetNotPermitted))
                            }
                            6 => {
                                checked.can_send_cross_agent = false;
                                ("sales_rep", target, Err(DenyReason::CrossAgentSendDisabled))
                            }
                            _ => ("sales_rep", target, expected),
                        };
                        assert_eq!(
                            evaluate_test(&rules, &overrides, Some(&roles), sender, target, checked)
                                .map_err(|d| d.reason),
                            result, "{policy:?}, sender kind {kind}, relation {relation}, boundary {boundary}",
                        );
                        decisions += 1;
                    }
                }
            }
        }
        assert_eq!(decisions, 560);
    }

    #[test]
    fn delegation_role_lookup_failures_and_unknowns_fall_back_to_pairs_and_mode() {
        let overrides = DelegationOverrides {
            unrestricted_roles: vec!["admin".into(), "invented".into()],
            allow_one_way: vec![("sales_rep".into(), "mkt_rep".into())],
        };
        for failure in 0..9 {
            let mut roles = qualifying_roles();
            match failure {
                0 => roles.owner = Err(OwnerRoleLookupError),
                1 => roles.requester = Err(OwnerRoleLookupError),
                2 => roles.owner = Ok(None),
                3 => roles.requester = Ok(None),
                4 => roles.owner = Ok(Some("invented".into())),
                5 => roles.requester = Ok(Some(DelegationRequesterRole::Account("invented".into()))),
                6 => roles.requester = Ok(Some(DelegationRequesterRole::BackgroundStartedBy("employee".into()))),
                7 => roles.owner = Ok(Some("Admin".into())),
                _ => roles.requester = Ok(Some(DelegationRequesterRole::Account(" admin ".into()))),
            }
            for policy in DelegationPolicy::ALL {
                let rules = rules_with(policy, &[]);
                let result = evaluate_test(&rules, &overrides, Some(&roles), "sales_rep", "mkt_rep", CHECKED_BOUNDARIES);
                if policy == DelegationPolicy::Disabled {
                    assert_eq!(result.unwrap_err().reason, DenyReason::DelegationDisabled);
                } else {
                    assert_eq!(result.unwrap(), DelegationAllowSource::OneWayPair);
                    let no_pair = DelegationOverrides { allow_one_way: Vec::new(), ..overrides.clone() };
                    let result = evaluate_test(&rules, &no_pair, Some(&roles), "sales_rep", "sales_rep2", CHECKED_BOUNDARIES);
                    assert_eq!(result.is_ok(), policy != DelegationPolicy::Hierarchy);
                }
            }
        }
        let rules = rules_with(DelegationPolicy::Disabled, &[]);
        assert_eq!(evaluate_test(&rules, &overrides, None, "sales_rep", "mkt_rep", CHECKED_BOUNDARIES)
            .unwrap_err().reason, DenyReason::DelegationDisabled);
    }

    #[test]
    fn delegation_unrestricted_target_never_confers_reverse_authority() {
        let roles = qualifying_roles();
        let overrides = DelegationOverrides {
            unrestricted_roles: vec!["admin".into()], ..DelegationOverrides::default()
        };
        for policy in DelegationPolicy::ALL {
            let rules = rules_with(policy, &[]);
            assert_eq!(evaluate_test(&rules, &overrides, Some(&roles), "sales_rep", "mkt_rep", CHECKED_BOUNDARIES)
                .unwrap(), DelegationAllowSource::UnrestrictedRole);
            let reverse = evaluate_test(&rules, &overrides, Some(&roles), "mkt_rep", "sales_rep", CHECKED_BOUNDARIES);
            assert_eq!(reverse.map(|_| ()), can_delegate_rules(&rules, &org(), "mkt_rep", "sales_rep"));
        }
        // Avatar owner and requester may be different qualifying role kinds.
        let roles = TestOwnerRoles {
            requester: Ok(Some(DelegationRequesterRole::Account("manager".into()))),
            ..qualifying_roles()
        };
        let overrides = DelegationOverrides {
            unrestricted_roles: vec!["admin".into(), "manager".into()], ..overrides
        };
        assert_eq!(evaluate_test(&rules_with(DelegationPolicy::Disabled, &[]), &overrides, Some(&roles),
            "sales_rep", "mkt_rep", CHECKED_BOUNDARIES).unwrap(), DelegationAllowSource::UnrestrictedRole);
    }

    #[test]
    fn delegation_boundaries_precede_system_and_role_lookups() {
        struct MustNotLookup;
        impl OwnerRoleView for MustNotLookup {
            fn avatar_owner_role(&self, _: &str) -> Result<Option<String>, OwnerRoleLookupError> {
                panic!("always-deny must precede lookups")
            }
            fn requester_role(&self) -> Result<Option<DelegationRequesterRole>, OwnerRoleLookupError> {
                panic!("always-deny must precede lookups")
            }
        }
        let overrides = DelegationOverrides { unrestricted_roles: vec!["admin".into()], ..Default::default() };
        for sender in ["sales_rep", "cron", "trusted"] {
            let rules = rules_with(DelegationPolicy::Open, &[(sender, "mkt_rep")]);
            for (checked, expected) in [
                (DelegationBoundaries::default(), DenyReason::PersonalTargetNotPermitted),
                (DelegationBoundaries { main_target_allowed: false, ..CHECKED_BOUNDARIES }, DenyReason::MainTargetNotPermitted),
                (DelegationBoundaries { can_send_cross_agent: false, ..CHECKED_BOUNDARIES }, DenyReason::CrossAgentSendDisabled),
            ] {
                let result = evaluate_delegation(&rules, &org(), sender, "mkt_rep", &DelegationContext {
                    overrides: &overrides, owner_roles: Some(&MustNotLookup), boundaries: checked,
                    extra_system_senders: &["trusted"],
                });
                assert_eq!(result.unwrap_err().reason, expected);
            }
        }
    }

    #[test]
    fn delegation_pair_sources_precede_mode_and_preserve_exact_direction() {
        let roles = qualifying_roles();
        let overrides = DelegationOverrides {
            allow_one_way: vec![("sales_rep".into(), "mkt_rep".into())],
            unrestricted_roles: Vec::new(),
        };
        for policy in [DelegationPolicy::Department, DelegationPolicy::Hierarchy, DelegationPolicy::Open] {
            let rules = rules_with(policy, &[("sales_rep", "mkt_rep")]);
            assert_eq!(evaluate_test(&rules, &overrides, Some(&roles), "sales_rep", "mkt_rep", CHECKED_BOUNDARIES)
                .unwrap(), DelegationAllowSource::TwoWayPair);
            assert_eq!(evaluate_test(&rules, &overrides, Some(&roles), "mkt_rep", "sales_rep", CHECKED_BOUNDARIES)
                .unwrap(), DelegationAllowSource::TwoWayPair);
        }
        let rules = rules_with(DelegationPolicy::Hierarchy, &[]);
        for sender in ["sales_rep_extra", "Sales_rep", "x-sales_rep"] {
            assert!(evaluate_test(&rules, &overrides, Some(&roles), sender, "mkt_rep", CHECKED_BOUNDARIES).is_err());
        }
        let reversed = evaluate_test(&rules, &overrides, None, "mkt_rep", "sales_rep", CHECKED_BOUNDARIES).unwrap_err();
        assert_eq!(reversed.reason.as_str(), "reverse_pair_only");
        assert!(reversed.message_zh().contains("只允許「sales_rep」委派給「mkt_rep」"));
        // A reverse pair does not veto an independent same-department/hierarchy grant.
        let overrides = DelegationOverrides {
            allow_one_way: vec![("sales_rep2".into(), "sales_rep".into()), ("ceo".into(), "sales_rep".into())],
            ..overrides
        };
        assert_eq!(evaluate_test(&rules_with(DelegationPolicy::Department, &[]), &overrides, None,
            "sales_rep", "sales_rep2", CHECKED_BOUNDARIES).unwrap(), DelegationAllowSource::Mode);
        assert_eq!(evaluate_test(&rules, &overrides, None, "sales_rep", "ceo", CHECKED_BOUNDARIES)
            .unwrap(), DelegationAllowSource::Mode);
    }

    #[test]
    fn delegation_one_way_normalization_limits_and_unicode() {
        let entries = vec![
            vec![" 甲 ".into(), " 乙 ".into()], vec!["乙".into(), "甲".into()],
            vec!["甲".into(), "乙".into()], vec!["甲".into()],
            vec!["甲".into(), "乙".into(), "丙".into()], vec![" ".into(), "乙".into()],
            vec!["甲 ".into(), " 甲".into()],
        ];
        let (pairs, warnings) = resolve_allow_one_way(&entries);
        assert_eq!(pairs, vec![("甲".into(), "乙".into()), ("乙".into(), "甲".into())]);
        assert_eq!(warnings.len(), 4);
        let entries: Vec<_> = (0..201).map(|i| vec![format!("a{i}"), "b".into()]).collect();
        let (pairs, warnings) = resolve_allow_one_way(&entries);
        assert_eq!(pairs.len(), 200);
        assert_eq!(warnings.len(), 1);
        assert!(!pairs.iter().any(|(from, _)| from == "a200"));
        assert!(resolve_allow_one_way(&entries[..200]).1.is_empty());
        let overrides = DelegationOverrides { allow_one_way: pairs, ..Default::default() };
        assert_eq!(evaluate_test(&rules_with(DelegationPolicy::Hierarchy, &[]), &overrides, None,
            "a199", "b", CHECKED_BOUNDARIES).unwrap(), DelegationAllowSource::OneWayPair);
        assert!(evaluate_test(&rules_with(DelegationPolicy::Hierarchy, &[]), &overrides, None,
            "a200", "b", CHECKED_BOUNDARIES).is_err());
    }

    #[test]
    fn delegation_unrestrictable_roles_are_privileged_account_roles() {
        // Auth depends on core: compare its source declaration without building
        // auth or creating a circular dev-dependency. New roles require review.
        let source = include_str!("../../duduclaw-auth/src/models.rs");
        let body = source.split("pub enum UserRole {").nth(1).unwrap().split('}').next().unwrap();
        let roles: Vec<_> = body.lines().map(str::trim).filter(|s| !s.is_empty())
            .map(|s| s.trim_end_matches(',').to_ascii_lowercase()).collect();
        assert_eq!(UNRESTRICTABLE_ROLE_NAMES, ["admin", "manager"]);
        assert!(UNRESTRICTABLE_ROLE_NAMES.iter().all(|role| roles.iter().any(|r| r == role)));
        assert!(source.contains("#[serde(rename_all = \"lowercase\")]"));
    }

    #[test]
    fn delegation_new_config_keys_validate_independently_and_fail_closed() {
        let table: toml::Table = toml::from_str(r#"
[delegation]
policy = "none"
allow = [["b", "a"]]
allow_one_way = [[" a ", " b "], ["b", "a"], ["a", "b"], ["a"], ["a", 2], ["a", "a"]]
unrestricted_roles = ["admin", "manager", "employee", "admin", "Admin", " admin ", "root", 4]
"#).unwrap();
        let read = delegation_config_read_from_table(&table);
        assert_eq!(read.rules.allow_pairs, vec![("a".into(), "b".into())]);
        assert_eq!(read.overrides.allow_one_way, vec![("a".into(), "b".into()), ("b".into(), "a".into())]);
        assert_eq!(read.overrides.unrestricted_roles, UNRESTRICTABLE_ROLE_NAMES);
        assert_eq!(read.override_warnings.len(), 4); // 3 invalid pairs + 1 role warning.
        assert!(read.override_warnings.iter().any(|w| w.contains("僅接受 admin / manager。")));
        let employee: toml::Table = "[delegation]\nunrestricted_roles=['employee']".parse().unwrap();
        let employee_read = delegation_config_read_from_table(&employee);
        assert!(employee_read.overrides.unrestricted_roles.is_empty());
        assert_eq!(employee_read.override_warnings.len(), 1);
        for value in ["false", "42", "'not-a-list'"] {
            let table = toml::from_str(&format!(
                "[delegation]\npolicy = 'hierarchy'\nallow = [['a', 'b']]\nallow_one_way = {value}\nunrestricted_roles = {value}"
            )).unwrap();
            let read = delegation_config_read_from_table(&table);
            assert_eq!(read.rules.policy, DelegationPolicy::Hierarchy);
            assert_eq!(read.rules.allow_pairs.len(), 1);
            assert_eq!(read.overrides, DelegationOverrides::default());
            assert_eq!(read.override_warnings.len(), 2);
        }
        for policy in ["'typo'", "false", "42"] {
            let table = toml::from_str(&format!(
                "[delegation]\npolicy = {policy}\nallow_one_way = [['a', 'b']]\nunrestricted_roles = ['admin']"
            )).unwrap();
            let read = delegation_config_read_from_table(&table);
            assert_eq!(read.rules.policy, DelegationPolicy::Disabled);
            assert_eq!(read.overrides, DelegationOverrides::default());
            assert!(read.policy_warning.is_some());
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), toml::to_string(&table).unwrap()).unwrap();
        assert_eq!(read_delegation_config(dir.path()).overrides, read.overrides);
        std::fs::write(dir.path().join("config.toml"), "[delegation\nunrestricted_roles=['admin']").unwrap();
        let read = read_delegation_config(dir.path());
        assert!(read.config_error.is_some());
        assert_eq!(read.overrides, DelegationOverrides::default());
        std::fs::remove_file(dir.path().join("config.toml")).unwrap();
        std::fs::create_dir(dir.path().join("config.toml")).unwrap();
        assert_eq!(read_delegation_config(dir.path()).overrides, DelegationOverrides::default());
    }

    #[test]
    fn delegation_absent_and_empty_keys_match_frozen_legacy_matrix_byte_for_byte() {
        let mut org = org();
        org.insert("cycle_a", "cycle_b", "");
        org.insert("cycle_b", "cycle_a", "");
        let ids = ["ceo", "sales_lead", "sales_rep", "sales_rep2", "mkt_lead", "mkt_rep",
            "loner", "unknown", "", "  ", " sales_rep ", "cron", "a2a-client", "cycle_a", "嘟嘟"];
        let roles = qualifying_roles();
        let mut decisions = 0;
        for policy in DelegationPolicy::ALL {
            for pairs in ["[]", "[['sales_rep','mkt_rep']]"] {
                for extras in [Vec::new(), vec![ACP_CLIENT_SENDER]] {
                    for new_keys in ["", "allow_one_way = []\nunrestricted_roles = []\n"] {
                        let table = toml::from_str(&format!(
                            "[delegation]\npolicy = '{}'\nallow = {pairs}\n{new_keys}", policy.as_str()
                        )).unwrap();
                        let read = delegation_config_read_from_table(&table);
                        for sender in ids {
                            for target in ids {
                                let before = legacy_decision(policy, Some(&read.rules), &org, sender, target, &extras);
                                let after = evaluate_delegation(&read.rules, &org, sender, target, &DelegationContext {
                                    overrides: &read.overrides, owner_roles: Some(&roles),
                                    boundaries: CHECKED_BOUNDARIES, extra_system_senders: &extras,
                                }).map(|_| ());
                                let legacy_wrapper = can_delegate_rules_ext(&read.rules, &org, sender, target, &extras);
                                assert_eq!(before, after, "{policy:?}: {sender:?} -> {target:?}");
                                assert_eq!(before, legacy_wrapper);
                                assert_eq!(format!("{before:?}").as_bytes(), format!("{after:?}").as_bytes());
                                if let (Err(before), Err(after)) = (before, after) {
                                    assert_eq!(before.message_zh().as_bytes(), after.message_zh().as_bytes());
                                }
                                decisions += 1;
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(decisions, 7200);
    }

    // Frozen pre-RM-183 oracle: preserve all legacy decisions and denial messages.
    fn legacy_decision(
        policy: DelegationPolicy,
        rules: Option<&DelegationRules>,
        org: &impl OrgView,
        sender: &str,
        target: &str,
        extra_system_senders: &[&str],
    ) -> Result<(), DelegationDenied> {
        let s = sender.trim();
        let t = target.trim();
    
        let deny = |reason: DenyReason| {
            Err(DelegationDenied {
                sender: s.to_string(),
                target: t.to_string(),
                policy,
                reason,
            })
        };
    
        // Defensive (fail-closed): a nameless party proves nothing.
        if s.is_empty() || t.is_empty() {
            return deny(DenyReason::InvalidAgentId);
        }
    
        // ① Self-delegation — denied under every policy, Open included.
        if s == t {
            return deny(DenyReason::SelfDelegation);
        }
    
        // ② System / human-interface senders.
        if is_system_sender(s) || extra_system_senders.iter().any(|extra| extra.trim() == s) {
            return Ok(());
        }
    
        // RM-137: `none` (and every value that resolves to it) stops every agent
        // sender here — before the ancestor rules and before the whitelist, so an
        // explicit `[delegation] allow` pair does not pass either.
        if policy == DelegationPolicy::Disabled {
            return deny(DenyReason::DelegationDisabled);
        }
    
        // Open skips rules ③–⑤b entirely (escape hatch = pre-WP21 behaviour).
        if policy == DelegationPolicy::Open {
            return Ok(());
        }
    
        // ③ sender is an ancestor of target (skip-level command).
        let down = walk_up(org, t, s);
        if down == Walk::Found {
            return Ok(());
        }
    
        // ④ target is an ancestor of sender (escalation / reporting up).
        let up = walk_up(org, s, t);
        if up == Walk::Found {
            return Ok(());
        }
    
        // ⑤ same non-empty department (Department policy only).
        let sender_dept = org.department(s).unwrap_or_default();
        let target_dept = org.department(t).unwrap_or_default();
        let sender_dept = sender_dept.trim();
        let target_dept = target_dept.trim();
        if policy == DelegationPolicy::Department
            && !sender_dept.is_empty()
            && !target_dept.is_empty()
            && sender_dept == target_dept
        {
            return Ok(());
        }
    
        // ⑤b explicit whitelist pair (design doc §2.1 rule 5b) — both Department
        // and Hierarchy respect it; Open already returned above.
        if let Some(rules) = rules {
            if rules.allows_pair(s, t) {
                return Ok(());
            }
        }
    
        // ⑥ Fail closed, with the most informative reason available.
        if down == Walk::Truncated || up == Walk::Truncated {
            return deny(DenyReason::ChainTooDeep);
        }
        match policy {
            DelegationPolicy::Hierarchy => deny(DenyReason::NoHierarchyRelation),
            DelegationPolicy::Department => {
                if sender_dept.is_empty() || target_dept.is_empty() {
                    deny(DenyReason::MissingDepartment)
                } else {
                    deny(DenyReason::DifferentDepartment)
                }
            }
            // Unreachable: Open returned Ok and Disabled denied above. Kept
            // exhaustive rather than wildcarded so a future policy variant must
            // state its denial reason.
            DelegationPolicy::Open => deny(DenyReason::NoHierarchyRelation),
            DelegationPolicy::Disabled => deny(DenyReason::DelegationDisabled),
        }
    }
}
