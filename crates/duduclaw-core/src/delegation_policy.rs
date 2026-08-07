//! WP21 — the single delegation predicate: *may `sender` delegate to `target`?*
//!
//! Before this module the only "who may command whom" rule lived in the CLI MCP
//! server (`check_supervisor_relation`) and recognised exactly one shape:
//! a **direct** parent↔child pair. Every other path into the bus (dispatcher,
//! `tasks_create`, ACP, autopilot) had no rule at all. This module is the one
//! predicate all of those choke points share, so the answer cannot drift per
//! call site.
//!
//! ## The rule (design doc §2.1, evaluated in this order, short-circuiting)
//!
//! 1. `sender == target` → **DENY** (self-delegation is an echo loop; denied
//!    under every policy, including [`DelegationPolicy::Open`]).
//! 2. `sender` is a **system sender** ([`SYSTEM_SENDERS`]) → ALLOW.
//! 3. `sender` is an ancestor of `target` on the `reports_to` chain, at any
//!    depth (≤ [`MAX_ANCESTOR_HOPS`] hops, cycle-safe) → ALLOW. This is what
//!    makes skip-level assignment (grandparent → grandchild) work.
//! 4. `target` is an ancestor of `sender` → ALLOW (escalation / reporting up,
//!    not limited to the direct manager).
//! 5. Policy is [`DelegationPolicy::Department`] and both agents carry the
//!    **same non-empty** department → ALLOW (peer collaboration).
//! 6. Otherwise → **DENY**, fail-closed (project convention #4).
//!
//! [`DelegationPolicy::Hierarchy`] skips rule 5; [`DelegationPolicy::Open`]
//! skips rules 3–5 and allows everything except rule 1 (the escape hatch that
//! restores pre-WP21 behaviour for the back-door paths).
//!
//! ## What this module is *not*
//!
//! It is not a rate limiter (`dispatch_guard`), not a tool-permission gate
//! (`capability_grants`), and not permission attenuation (`delegation_scope`).
//! Those three are orthogonal and stay where they are. Ranks (`org::OrgRank`)
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

/// Sender id stamped on bus tasks that entered through the ACP `message/send`
/// endpoint. Intentionally **not** in [`SYSTEM_SENDERS`]; exposed as a constant
/// so the opt-in (`config.toml [acp] trusted = true`) has one spelling.
pub const ACP_CLIENT_SENDER: &str = "a2a-client";

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
/// - any `__…` id — the namespace gateway-internal synthetic senders use
///   (e.g. `__deferred_gvu__`, which `is_valid_agent_id` otherwise accepts).
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
    SYSTEM_SENDERS
        .iter()
        .chain(std::iter::once(&ACP_CLIENT_SENDER))
        .chain(std::iter::once(&"default"))
        .chain(std::iter::once(&"doctor-probe"))
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
}

impl DelegationPolicy {
    /// Stable wire string (`department` / `hierarchy` / `open`).
    pub fn as_str(&self) -> &'static str {
        match self {
            DelegationPolicy::Department => "department",
            DelegationPolicy::Hierarchy => "hierarchy",
            DelegationPolicy::Open => "open",
        }
    }

    /// zh-TW label for operator-facing messages.
    pub fn label_zh(&self) -> &'static str {
        match self {
            DelegationPolicy::Department => "部門(department)",
            DelegationPolicy::Hierarchy => "階級(hierarchy)",
            DelegationPolicy::Open => "開放(open)",
        }
    }

    /// Parse a config value. Case- and whitespace-insensitive; unknown ⇒ `None`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "department" => Some(DelegationPolicy::Department),
            "hierarchy" => Some(DelegationPolicy::Hierarchy),
            "open" => Some(DelegationPolicy::Open),
            _ => None,
        }
    }

    /// Parse for config loading: an unrecognised value falls back to the
    /// (stricter) default and returns a warning string for the caller to log,
    /// rather than silently widening or narrowing what the operator asked for.
    pub fn from_config_value(value: &str) -> (Self, Option<String>) {
        match Self::parse(value) {
            Some(p) => (p, None),
            None => (
                Self::default(),
                Some(format!(
                    "config.toml [delegation] policy = \"{}\" 無法辨識,已改用預設值 \"{}\"。\
                     可用值:department / hierarchy / open。",
                    crate::truncate_chars(value.trim(), 40),
                    Self::default().as_str()
                )),
            ),
        }
    }
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
}

impl DenyReason {
    /// Stable machine code for audit logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            DenyReason::SelfDelegation => "self_delegation",
            DenyReason::InvalidAgentId => "invalid_agent_id",
            DenyReason::NoHierarchyRelation => "no_hierarchy_relation",
            DenyReason::DifferentDepartment => "different_department",
            DenyReason::MissingDepartment => "missing_department",
            DenyReason::ChainTooDeep => "chain_too_deep",
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

impl DelegationDenied {
    /// Operator-facing zh-TW explanation: who was refused, what relation is
    /// missing, and the concrete ways to fix it.
    pub fn message_zh(&self) -> String {
        let s = short_id(&self.sender);
        let t = short_id(&self.target);
        match self.reason {
            DenyReason::SelfDelegation => format!(
                "委派遭拒:「{s}」不可委派給自己。自我委派會形成回音迴圈,三種委派政策皆禁止。"
            ),
            DenyReason::InvalidAgentId => {
                "委派遭拒:發送者或目標的 agent 名稱為空,無法判定從屬關係,依 fail-closed 原則拒絕。"
                    .to_string()
            }
            DenyReason::ChainTooDeep => format!(
                "委派遭拒:「{s}」對「{t}」的從屬關係無法確認 — reports_to 從屬鏈超過 \
                 {MAX_ANCESTOR_HOPS} 層或形成循環,已中止追溯並依 fail-closed 原則拒絕。\
                 請檢查組織樹是否過深或成環(reports_to 互指)。"
            ),
            DenyReason::NoHierarchyRelation => format!(
                "委派遭拒:「{s}」無權指派「{t}」(委派政策:{})。兩者不在同一條 reports_to \
                 從屬鏈上(任一方為另一方的上級皆可)。可行處理:① 建立從屬關係 — 把「{t}」的 \
                 reports_to 指向「{s}」或其上級;② 若只是同事橫向協作,把 config.toml 的 \
                 [delegation] policy 設為 \"department\" 並讓兩者屬於同一部門;\
                 ③ 需要完全開放時設為 \"open\"。",
                self.policy.label_zh()
            ),
            DenyReason::DifferentDepartment => format!(
                "委派遭拒:「{s}」無權指派「{t}」(委派政策:{})。兩者既不在同一條 reports_to \
                 從屬鏈上,也不屬於同一個部門。可行處理:① 建立從屬關係(把其中一方的 reports_to \
                 指向另一方);② 把兩者的 [agent] department 設為同一部門;\
                 ③ 在 config.toml 將 [delegation] policy 設為 \"open\" 解除限制。",
                self.policy.label_zh()
            ),
            DenyReason::MissingDepartment => format!(
                "委派遭拒:「{s}」無權指派「{t}」(委派政策:{})。兩者沒有 reports_to 從屬關係,\
                 且至少一方未設定部門 — 空白部門不視為同部門。可行處理:① 為雙方的 [agent] \
                 department 填上同一個部門;② 建立從屬關係;\
                 ③ 在 config.toml 將 [delegation] policy 設為 \"open\" 解除限制。",
                self.policy.label_zh()
            ),
        }
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
    can_delegate_ext_impl(policy, None, org, sender, target, extra_system_senders)
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
        // Unreachable: Open returned Ok above. Kept exhaustive rather than
        // wildcarded so a future policy variant must state its denial reason.
        DelegationPolicy::Open => deny(DenyReason::NoHierarchyRelation),
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
    /// `department` (default) / `hierarchy` / `open`.
    pub policy: String,
    /// `allow = [["a", "b"], ...]` — unordered whitelist pairs (design doc
    /// §2.2). Defaults to empty when the key is absent.
    pub allow: Vec<Vec<String>>,
    /// WP21 debt ⑧ — refuse MCP callers whose `DUDUCLAW_AGENT_ID` is not backed
    /// by a valid `DUDUCLAW_AGENT_TOKEN` (see
    /// [`crate::identity_token`]). **Default `false`** (soft mode: warn once and
    /// proceed) so upgrading an existing install changes nothing; flip it on
    /// once every agent's MCP config has been re-issued with a token.
    ///
    /// Inert unless `~/.duduclaw/identity.key` exists — no key means no claim
    /// can be verified *or* refused, and the verdict is `Disabled`.
    pub require_identity_token: bool,
}

impl Default for DelegationConfig {
    fn default() -> Self {
        Self {
            policy: DelegationPolicy::default().as_str().to_string(),
            allow: Vec::new(),
            require_identity_token: false,
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
/// Same fail-closed defaults as `from_home`: a missing/non-table
/// `[delegation]` section ⇒ [`DelegationConfig::default`] (`department`,
/// empty `allow`).
pub fn delegation_config_from_table(table: &toml::Table) -> DelegationConfig {
    let Some(section) = table.get("delegation").and_then(|v| v.as_table()) else {
        return DelegationConfig::default();
    };

    let policy = section
        .get("policy")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| DelegationPolicy::default().as_str().to_string());

    let allow = section
        .get("allow")
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

    // Same independent-extraction discipline as `policy` / `allow`: a
    // wrong-typed value (string "true", a number) is ignored in favour of the
    // safe default rather than taking the whole section down with it. Note the
    // default is `false`, so a typo here degrades to *soft* mode — the choice
    // that preserves an existing deployment, matching this section's
    // "壞項忽略,不整段失效" rule.
    let require_identity_token = section
        .get("require_identity_token")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    DelegationConfig {
        policy,
        allow,
        require_identity_token,
    }
}

impl DelegationConfig {
    /// Load `[delegation]` from `<home>/config.toml`. Parsed in isolation from a
    /// generic `toml::Table` (same shape as `DispatchGuardConfig::from_home`), so
    /// unrelated or malformed config elsewhere can never make this fail —
    /// absent / malformed section ⇒ defaults (`department`, empty `allow`).
    ///
    /// `policy` and `allow` are each pulled out of the raw table independently
    /// (rather than one `try_into::<DelegationConfig>()` call) so a
    /// wrong-shaped `allow` entry — e.g. a bare number instead of an array —
    /// can never make a *valid* `policy` disappear along with it; the whole
    /// point of "壞項忽略,不整段失效" (design doc §2.2) applies to the section
    /// as a whole, not just to individual `allow` pairs.
    pub fn from_home(home_dir: &Path) -> Self {
        let path = home_dir.join("config.toml");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        delegation_config_from_table(&table)
    }

    /// Resolved policy plus an optional warning about an unrecognised value.
    pub fn resolve(&self) -> (DelegationPolicy, Option<String>) {
        DelegationPolicy::from_config_value(&self.policy)
    }

    /// Resolved policy, dropping the warning.
    pub fn policy(&self) -> DelegationPolicy {
        self.resolve().0
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

/// Convenience for call sites that just need the effective policy: reads
/// `<home>/config.toml [delegation] policy` and logs a warning once if the
/// value is unrecognised.
pub fn policy_from_home(home_dir: &Path) -> DelegationPolicy {
    let (policy, warning) = DelegationConfig::from_home(home_dir).resolve();
    if let Some(w) = warning {
        tracing::warn!("{w}");
    }
    policy
}

/// `config.toml [delegation] require_identity_token` (WP21 debt ⑧), defaulting
/// to `false` when the file, section or key is absent.
pub fn require_identity_token_from_home(home_dir: &Path) -> bool {
    DelegationConfig::from_home(home_dir).require_identity_token
}

/// Same as [`delegation_rules_from_home`], but takes an already-parsed table
/// — see [`delegation_config_from_table`] for why a call site would want
/// that (reading `[delegation]` alongside other sections in one file pass).
pub fn delegation_rules_from_table(table: &toml::Table) -> DelegationRules {
    let (rules, warnings) = delegation_config_from_table(table).resolve_rules();
    for w in warnings {
        tracing::warn!("{w}");
    }
    rules
}

/// Convenience for call sites that need the full [`DelegationRules`] (policy
/// + whitelist): reads `<home>/config.toml [delegation]` and logs a warning
/// for the policy value (if unrecognised) and for each dropped `allow` entry.
pub fn delegation_rules_from_home(home_dir: &Path) -> DelegationRules {
    let (rules, warnings) = DelegationConfig::from_home(home_dir).resolve_rules();
    for w in warnings {
        tracing::warn!("{w}");
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(DelegationPolicy::default(), DelegationPolicy::Department);
        for p in ALL {
            assert_eq!(DelegationPolicy::parse(p.as_str()), Some(p));
        }

        let (p, warn) = DelegationPolicy::from_config_value("nonsense");
        assert_eq!(p, DelegationPolicy::Department);
        assert!(warn.expect("warning").contains("nonsense"));
        assert!(DelegationPolicy::from_config_value("open").1.is_none());
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

        // Unknown value ⇒ default + warning.
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"anarchy\"\n",
        )
        .unwrap();
        let (p, warn) = DelegationConfig::from_home(home).resolve();
        assert_eq!(p, DelegationPolicy::Department);
        assert!(warn.is_some());

        // Wrong type / malformed section ⇒ defaults, never a panic.
        std::fs::write(home.join("config.toml"), "[delegation]\npolicy = 42\n").unwrap();
        assert_eq!(
            DelegationConfig::from_home(home).policy(),
            DelegationPolicy::Department
        );
        std::fs::write(home.join("config.toml"), "not = valid = toml").unwrap();
        assert_eq!(
            DelegationConfig::from_home(home).policy(),
            DelegationPolicy::Department
        );
    }

    /// WP21 debt ⑧ — `require_identity_token` parses independently of `policy`
    /// and defaults to `false` (soft mode) in every degenerate case, so an
    /// upgrade cannot lock an existing install out of its own MCP server.
    #[test]
    fn require_identity_token_defaults_off_and_parses_independently() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        assert!(!require_identity_token_from_home(home), "no config ⇒ soft mode");

        for body in [
            "[delegation]\npolicy = \"open\"\n",                      // key absent
            "[delegation]\nrequire_identity_token = false\n",          // explicit off
            "[delegation]\nrequire_identity_token = \"true\"\n",       // wrong type (string)
            "[delegation]\nrequire_identity_token = 1\n",              // wrong type (int)
            "[general]\nlog_level = \"info\"\n",                       // section absent
        ] {
            std::fs::write(home.join("config.toml"), body).unwrap();
            assert!(!require_identity_token_from_home(home), "{body:?} must stay soft");
        }

        // Explicit opt-in, alongside an unrelated bad `allow` entry: the bad
        // entry is dropped, the flag survives, and so does `policy`.
        std::fs::write(
            home.join("config.toml"),
            "[delegation]\npolicy = \"hierarchy\"\nrequire_identity_token = true\nallow = [42]\n",
        )
        .unwrap();
        let cfg = DelegationConfig::from_home(home);
        assert!(cfg.require_identity_token);
        assert_eq!(cfg.policy(), DelegationPolicy::Hierarchy);
    }

    #[test]
    fn denied_message_names_both_parties_and_a_remedy() {
        let d = denial(DelegationPolicy::Department, "sales_rep", "mkt_rep");
        let msg = d.message_zh();
        assert!(msg.contains("sales_rep") && msg.contains("mkt_rep"));
        assert!(msg.contains("reports_to") && msg.contains("department"));
        assert!(msg.contains("open"));
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
}
