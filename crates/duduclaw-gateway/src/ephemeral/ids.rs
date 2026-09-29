//! Identifiers, metadata sidecars and the role-member value types, plus the
//! capability-subset checks every spawn path shares.
//! Moved verbatim out of `ephemeral.rs` (file-size split).

use super::*;

/// Metadata sidecar (`ephemeral.toml`) written next to the scaffold's
/// `agent.toml`. Kept separate from `AgentConfig` so no core-schema change
/// is needed.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EphemeralMeta {
    /// The synthesizing (parent) agent id.
    pub parent: String,
    /// Requested model tier: "cheap" | "standard" | "preferred".
    pub tier: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 expiry (creation + TTL).
    pub expires_at: String,
}

/// Root directory for ephemeral scaffolds.
pub fn ephemeral_root(home_dir: &Path) -> PathBuf {
    home_dir.join("agents").join(EPHEMERAL_DIR_NAME)
}

/// Whether `id` is shaped like an ephemeral agent id (prefix + the same
/// charset rules as every other agent id — no dots, no slashes).
pub fn is_ephemeral_id(id: &str) -> bool {
    id.len() > EPHEMERAL_ID_PREFIX.len()
        && id.starts_with(EPHEMERAL_ID_PREFIX)
        && duduclaw_core::is_valid_agent_id(id)
}

/// Mint a fresh ephemeral agent id (`eph-` + 12 hex chars).
pub fn new_ephemeral_id() -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    // 12 ASCII hex chars — char == byte here, no multi-byte hazard.
    let short: String = hex.chars().take(12).collect();
    format!("{EPHEMERAL_ID_PREFIX}{short}")
}

/// Mint a role-member agent id: `eph-<parent>-r<round>-<role>-<rand>`.
///
/// Keeps the `eph-` prefix on purpose — cost attribution folds `eph-*` rows
/// back onto the parent employee through the `ephemeral_parents` mapping
/// (`cost_telemetry.rs`), and a role member's spend belongs to its employee
/// exactly as an ordinary ephemeral's does. The role and round are in the id
/// only so a human reading a log, an audit row or a directory listing can tell
/// three members of the same round apart; nothing parses them back out (the
/// authoritative copy is the `[team_member]` section — see
/// [`ROLE_MEMBER_SECTION`]).
///
/// Length: `eph-`(4) + parent(≤24) + `-r`(2) + round(≤4) + `-`(1) +
/// role(≤8) + `-`(1) + rand(6) = **≤50** of the 64 characters
/// [`duduclaw_core::is_valid_agent_id`] allows. The result is asserted against
/// that predicate before it is returned, so a future change to either bound
/// fails loudly here instead of producing an id the registry would reject.
pub fn new_role_member_id(parent: &str, round: u32, role: Role) -> Result<String, String> {
    if !duduclaw_core::is_valid_agent_id(parent) {
        return Err("invalid parent agent id".to_string());
    }
    if round > ROLE_MEMBER_ROUND_MAX {
        return Err(format!(
            "round {round} exceeds {ROLE_MEMBER_ROUND_MAX} — a role-member id cannot encode it"
        ));
    }
    // `is_valid_agent_id` already guarantees ASCII, so this cannot split a
    // multi-byte char; `truncate_chars` is used anyway (project convention #1:
    // never hand-slice, even where it would happen to be safe today).
    let stem = duduclaw_core::truncate_chars(parent, ROLE_MEMBER_PARENT_FRAGMENT_MAX);
    let hex = uuid::Uuid::new_v4().simple().to_string();
    let rand: String = hex.chars().take(6).collect();
    let id = format!(
        "{EPHEMERAL_ID_PREFIX}{stem}-r{round}-{role}-{rand}",
        role = role.as_str()
    );
    if !is_ephemeral_id(&id) {
        return Err(format!(
            "minted role-member id is not a valid agent id: {id:?}"
        ));
    }
    Ok(id)
}

/// A team role member to scaffold — one `(task, round, role)` slot with its
/// own runtime, model and (optional) reasoning effort.
///
/// The composer builds this from a validated
/// [`duduclaw_core::types::ResolvedTeam`]; `runtime` / `model` therefore arrive
/// already chosen by an operator's `[team.roles.*]` config, never by a model.
#[derive(Debug, Clone)]
pub struct RoleMemberSpec {
    /// The employee this role belongs to — capability envelope source, cost
    /// attribution target, and `reports_to` parent.
    pub parent_agent: String,
    /// Owning goal task id (recorded, not parsed back out of the member id).
    pub task_id: String,
    /// Round number within that task.
    pub round: u32,
    /// Which of the four roles this member fills.
    pub role: Role,
    /// Runtime id or catalog alias (e.g. `agy`). Must be in
    /// [`TEAM_ROLE_RUNTIME_ALLOWLIST`]; canonicalised before it is written.
    pub runtime: String,
    /// Model id. Must belong to a family `runtime` serves.
    pub model: String,
    /// Reasoning effort. `None` ⇒ the `[model] effort` key is **not written**,
    /// so the spawn passes no flag and the provider's own default depth
    /// applies (WP-3's contract — see
    /// [`duduclaw_core::effort::read_agent_effort`]).
    pub effort: Option<Effort>,
    /// Per-turn role instruction → dispatch prompt. The role's `SOUL.md` is
    /// stable across tasks so a new member can reuse its system prefix.
    pub instruction: String,
    /// Tool subset → `[capabilities] allowed_tools`. Subject to the same
    /// [`check_tool_subset`] containment as any ephemeral: a role can never
    /// hold a tool its employee does not.
    pub tools: Vec<String>,
}

/// The `[team_member]` section read back off a scaffold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleMemberRecord {
    pub role: Role,
    pub task_id: String,
    pub round: u32,
    pub parent: String,
}

/// Terminal state of a role member's round, recorded in the teardown audit row.
///
/// A closed enum with fixed tokens rather than a free string: this value lands
/// in `tool_calls.jsonl`, and every stable token in this workspace is written
/// out explicitly (never `format!("{:?}", …).to_lowercase()` — that idiom has
/// already produced one silently-wrong column here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleMemberOutcome {
    /// The role produced its packet and the round accepted it.
    Accepted,
    /// The round ran but its output was rejected by the verifier / judge.
    Rejected,
    /// The role's own invocation failed (spawn, timeout, empty reply).
    Failed,
    /// The round was abandoned before this role finished (budget exhausted,
    /// human abort, task cancelled).
    Cancelled,
}

impl RoleMemberOutcome {
    /// Stable snake_case audit token.
    pub fn as_str(&self) -> &'static str {
        match self {
            RoleMemberOutcome::Accepted => "accepted",
            RoleMemberOutcome::Rejected => "rejected",
            RoleMemberOutcome::Failed => "failed",
            RoleMemberOutcome::Cancelled => "cancelled",
        }
    }
}

impl std::fmt::Display for RoleMemberOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Parse a caller-supplied tier keyword. Only the three tier names are
/// accepted — never a raw model id (multi-model doctrine).
pub fn parse_tier(s: &str) -> Option<ModelTier> {
    match s.trim().to_ascii_lowercase().as_str() {
        "cheap" => Some(ModelTier::Cheap),
        "standard" | "" => Some(ModelTier::Standard),
        "preferred" => Some(ModelTier::Preferred),
        _ => None,
    }
}

/// Charset guard for requested tool names (plain tool names or Claude-style
/// qualified patterns like `mcp__duduclaw__wiki_read` or `Bash(git:*)`).
fn is_valid_tool_name(t: &str) -> bool {
    !t.is_empty()
        && t.chars().count() <= 128
        && t.chars().all(|c| {
            c.is_ascii_alphanumeric()
                || matches!(c, '_' | '-' | '(' | ')' | ':' | '*' | ',' | '.' | ' ')
        })
}

/// Tools a **team role member** may hold even when its employee's
/// `[capabilities] allowed_tools` does not list them (P1/WP-2 ↔ WP-5 seam).
///
/// Exactly one entry, and the rationale is why the list stays that short:
/// `team_handoff` is the team's own handoff channel, not a capability. It
/// writes one [`duduclaw_core::task_packet::TaskPacket`] to a derived path
/// under `<home>/team_packets/` and reaches nothing else — the same blast
/// radius as `working_state_*`, which is likewise a mechanism the platform
/// injects rather than a tool an employee grants. Without the exemption every
/// employee with a non-empty allowlist would fail to form a team at all: the
/// composer injects `team_handoff` into each member's tool subset, and an
/// allowlist written before teams existed cannot name it.
///
/// Scope of the exemption:
/// * applies **only** through [`scaffold_role_member`] (the gateway composer);
///   the MCP `spawn_ephemeral` path passes no intrinsics, so an agent can
///   never obtain this tool by synthesizing an ordinary ephemeral;
/// * every other tool a role requests stays subject to the subset rule;
/// * an explicit `denied_tools` entry still wins — an operator who denies
///   `team_handoff` gets a visibly failed round, never a silent escalation.
pub const TEAM_INTRINSIC_TOOLS: &[&str] = &["team_handoff"];

/// Version-stable role system prefix. Task titles, packets, acceptance and
/// working state stay in the dispatch prompt, where they cannot invalidate
/// this member-role cache prefix on every round.
pub(super) fn stable_role_soul(role: Role) -> &'static str {
    match role {
        Role::Planner => {
            "你是團隊的規劃角色。只拆解工作，不執行子任務。每個可獨立完成的子任務都透過 team_handoff 交給執行角色；依賴與阻礙必須如實標示。遵守本次派工中的驗收標準與風險邊界。"
        }
        Role::Executor => {
            "你是團隊的執行角色。只處理本次派工給你的子任務，保留工具證據，不宣稱未完成的動作已完成。結果透過 team_handoff 交給審核角色，缺證據的推論列為待確認。遵守本次派工中的驗收標準與風險邊界。"
        }
        Role::Verifier => {
            "你是團隊的審核角色。依本次派工中的凍結驗收標準與獨立證據審核，不接受無證據的完成宣稱；缺口要具體指出。"
        }
        Role::Utility => {
            "你是團隊的合成角色。只整理已審核的產物，不補造事實或證據；遵守本次派工中的驗收標準與風險邊界。"
        }
    }
}

/// Fail-closed capability subsetting: every requested tool must sit inside
/// the PARENT agent's own capability envelope.
///
/// Rules (deny wins, ambiguity rejects):
/// 1. Empty request → reject (an ephemeral agent must declare its tool
///    subset explicitly; an empty `allowed_tools` would mean *unrestricted*
///    under [`CapabilitiesConfig`] semantics, which is the opposite of
///    deny-by-default).
/// 2. Any tool in the parent's `denied_tools` → reject.
/// 3. Parent has a non-empty `allowed_tools` allowlist → every requested
///    tool must appear in it (case-insensitive, trimmed).
/// 4. Malformed tool names → reject.
pub fn check_tool_subset(parent: &CapabilitiesConfig, requested: &[String]) -> Result<(), String> {
    check_tool_subset_with_intrinsics(parent, requested, &[])
}

/// [`check_tool_subset`] plus a caller-supplied list of always-permitted tool
/// names — the only difference being that rule 3 (the parent's `allowed_tools`
/// allowlist) does not apply to a tool named in `intrinsics`.
///
/// Rules 1, 2 and 4 are unchanged for every tool, intrinsic or not: an empty
/// request is still a refusal, a malformed name is still a refusal, and an
/// explicit `denied_tools` entry still vetoes. Pass `&[]` for the ordinary
/// ephemeral path; only the composer passes [`TEAM_INTRINSIC_TOOLS`].
pub fn check_tool_subset_with_intrinsics(
    parent: &CapabilitiesConfig,
    requested: &[String],
    intrinsics: &[&str],
) -> Result<(), String> {
    if requested.is_empty() {
        return Err("tools must list at least one tool (deny-by-default: an \
                    empty allowlist would mean unrestricted)"
            .to_string());
    }
    let eq_ci = |a: &str, b: &str| a.trim().eq_ignore_ascii_case(b.trim());
    for tool in requested {
        if !is_valid_tool_name(tool) {
            return Err(format!("invalid tool name: {tool:?}"));
        }
        if parent.denied_tools.iter().any(|d| eq_ci(d, tool)) {
            return Err(format!(
                "privilege escalation rejected: tool '{tool}' is in the \
                 parent agent's denied_tools"
            ));
        }
        // An intrinsic skips the allowlist check only — never the deny list
        // above, never the charset guard.
        if intrinsics.iter().any(|i| eq_ci(i, tool)) {
            continue;
        }
        if !parent.allowed_tools.is_empty() && !parent.allowed_tools.iter().any(|a| eq_ci(a, tool))
        {
            return Err(format!(
                "privilege escalation rejected: tool '{tool}' is not in the \
                 parent agent's allowed_tools"
            ));
        }
    }
    Ok(())
}
