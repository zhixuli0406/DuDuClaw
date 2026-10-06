//! **The data**: which fields / sections / files this guard freezes, and the
//! file kinds it recognises.
//!
//! Split out of the single 2,374-line `org_field_guard.rs` on 2026-09-29
//! (audit O9). Everything here is a `const` table or a closed enum — no
//! decision logic. The three comparison shapes the table names are
//! implemented once, generically, in [`super::matcher::diff_frozen`]; the
//! entry points that consume the verdicts live in [`super`].
//!
//! Adding a frozen field is therefore a one-row edit to [`FROZEN_FIELDS`],
//! not a fourth hand-written `diff_*` function plus a fourth branch in the
//! entry point.

/// Keys under `[agent]` in `agent.toml` that feed the delegation predicate.
///
/// `name` is here for a less obvious reason than the other two: it is the
/// **registry** id (`AgentRegistry::get`/`list` index by `[agent] name`, which
/// need not equal the directory name), and `delegation.set` resolves an
/// operator-typed whitelist entry through `name → directory name`. An agent
/// that could rewrite its own `name` to a value the operator believes belongs
/// to someone else would silently receive that whitelist pair, and would also
/// blur every `name`-keyed lookup the gateway performs. Renames are legitimate
/// — they just have to go through `agent_update` / the dashboard, which own the
/// rest of the rename (SOUL.md/IDENTITY.md sync, trigger) anyway.
pub const AGENT_ORG_FIELDS: &[&str] = &["reports_to", "department", "name"];

/// The `agent.toml` section that holds the permission envelope.
///
/// # Why the whole section, not a key list
///
/// Team-as-Agent review P1: a team's role member (planner / executor /
/// verifier) is a *separate principal* — often a cheap third-party model — but
/// it runs with the **employee's** workspace as its cwd, so the `PreToolUse`
/// hook installed in that directory identifies the caller as the employee
/// itself and `check_caller_scope` returns `NotAgentFile`. The platform
/// promises a role member "can never hold a tool its employee does not"
/// (`check_tool_subset` reads this very section), so letting the member
/// rewrite `[capabilities]` would let it widen the envelope that is supposed
/// to contain it — one `Edit` and the next round's subset check waves anything
/// through.
///
/// The keys that made the finding concrete are `allowed_tools`,
/// `denied_tools`, `computer_use`, `browser_via_bash`, `os_native`,
/// `db_sources`, `approval_required_tools`, `irreversible_tools`,
/// `maybe_irreversible_tools`, `scoped_tools`, `autonomy_level`,
/// `git_credentials` and `wiki_visible_to` — but the diff is taken over the
/// **whole table** rather than that list, so a capability key added in a later
/// release is frozen the day it lands instead of the day someone remembers to
/// extend a list here. Fail-closed by construction (`CLAUDE.md` convention 4).
///
/// Legitimate changes keep every route they had: the MCP `agent_update` tool
/// (which writes the file from Rust and carries the delegation-policy gate)
/// and the dashboard `agents.update` RPC. Neither passes through this hook —
/// the hook only sees Claude Code's own `Write` / `Edit` / `Bash` tool calls.
pub const AGENT_CAPABILITY_SECTION: &str = "capabilities";

/// `agent.toml` sections a security gate reads, frozen whole against the
/// agent's own Write/Edit (G1 / C, 2026-10; agent callers only — see
/// [`FrozenCallers`]). Each entry, and the gate that reads it:
///
/// - `permissions` — `mcp_dispatch::permission_for_call` (`can_create_agents`
///   / `can_send_cross_agent` / `can_modify_own_skills` /
///   `can_schedule_tasks`) and the `agent_update_soul` gate
///   (`can_modify_own_soul`).
/// - `container` — the task sandbox (`sandbox_enabled`, `network_access`) and
///   the team gate's `sandbox_enabled` Solo rule.
/// - `budget` — `budget::check_agent_budget` (limits, `hard_stop`).
/// - `guardrails` — the output guardrail (`block_secrets`, `redact_pii`,
///   `block_injection_echo`, `deny_phrases`).
/// - `runtime` — `provider` / `fallback` pick the CLI; only the Claude runtime
///   runs this very hook, and codex/gemini derive their sandbox flag per
///   runtime. `minimal_context` decides the `--tools` narrowing.
/// - `fork` — `[fork] enabled` gates the fork tools in `tools/list`; budget
///   caps.
/// - `evolution` — the AEE commit gate's `[evolution.noise_band]`, the
///   `require_causal_evidence` gate, and the spend switches (`gvu_enabled`,
///   `skill_synthesis_enabled`).
/// - `task_forward_model` — the held-out rule gate (`held_out_gate_enabled`).
/// - `channels` — per-agent bot credentials.
/// - `odoo` — per-agent ERP credentials and the `allowed_models` /
///   `allowed_actions` filter.
/// - `mcp` — `[[mcp.external]]` servers and their `allowed_tools` /
///   `denied_tools`.
/// - `redaction` — per-agent redaction blocks (and through them the
///   data-file guard's arming).
/// - `ptc` — programmatic tool calling switch and its tool list.
/// - `night_engine` — idle-time LLM spend and its per-pass cost cap.
/// - `os_watch` — which host paths the OS-native watcher observes.
/// - `team` — `[team.roles.*]` picks the runtime/model of each role,
///   including the verifier that judges this employee's own output.
///
/// Since G1 round 3 the freeze itself is an allow-list
/// ([`AGENT_EDITABLE_SECTIONS`]): every section not listed editable is frozen,
/// so this list is the documented reason for the known ones, and a test keeps
/// it disjoint from the editable list.
pub const AGENT_SECURITY_SECTIONS: &[&str] = &[
    "permissions",
    "container",
    "budget",
    "guardrails",
    "runtime",
    "fork",
    "evolution",
    "task_forward_model",
    "channels",
    "odoo",
    "mcp",
    "redaction",
    "ptc",
    "night_engine",
    "os_watch",
    "team",
];

/// The only `agent.toml` sections the agent may still edit with Write/Edit
/// (G1 round 3: an allow-list — every other top-level section, including any
/// added later, is frozen for agent callers).
///
/// - `agent` / `model` / `prompt` — editable except the keys in
///   [`AGENT_SECURITY_KEYS`] (and `[agent]`'s org fields, frozen by WP21).
/// - `heartbeat` — wake-up schedule.
/// - `proactive` — proactive check cadence and where its notices go.
/// - `research` — opt-in evening self-study (spends LLM; no gate reads it).
/// - `goal_intent` — channel-side goal detection thresholds.
/// - `memory` — per-agent memory knobs.
/// - `skills` — recommended skill list.
/// - `sticker`, `cultural_context` — reply style.
/// - `preset` — informational mirror; the binding authority is
///   `<home>/preset_bindings.toml`.
/// - `planner` — clarify-first planning knobs.
pub const AGENT_EDITABLE_SECTIONS: &[&str] = &[
    "agent",
    "model",
    "prompt",
    "heartbeat",
    "proactive",
    "research",
    "goal_intent",
    "memory",
    "skills",
    "sticker",
    "cultural_context",
    "preset",
    "planner",
];

/// Single keys frozen inside otherwise editable `agent.toml` sections:
///
/// - `[agent] role` — `shared_wiki_delete` lets the `main` role delete any
///   shared page.
/// - `[prompt] cli_bare_mode` — adds `--bare`, which makes the Claude CLI skip
///   hooks (this one included).
/// - `[model] account_pool` — which rotator accounts (whose credentials and
///   quota) the agent may spend.
pub const AGENT_SECURITY_KEYS: &[(&str, &[&str])] = &[
    ("agent", &["role"]),
    ("prompt", &["cli_bare_mode"]),
    ("model", &["account_pool"]),
];

/// Top-level directories under `<home>` that an agent caller may still write
/// besides its own agent directory (G1, 2026-10). Only `attachments/`: the
/// documented fallback location for a `📎DELIVER:` deliverable
/// (`office_docs::validate_deliver_path`) and a read root of the data-file
/// tools (`mcp_files::allowed_roots`). Writing the directory entry itself
/// (replacing or deleting the whole folder) is not covered.
pub const HOME_WRITABLE_DIRS: &[&str] = &["attachments"];

/// DuDuClaw-specific evidence file names that the Bash lane recognises by
/// basename alone (any directory, rotated suffixes included), because an
/// agent can reach them through a `cd` the token scan cannot follow.
pub(super) const HOME_EVIDENCE_BASENAMES: &[&str] = &["tool_calls.jsonl", "security_audit.jsonl"];

/// Sections of `<home>/config.toml` that feed the delegation predicate.
///
/// `[delegation]` carries the policy + whitelist; `[acp] trusted` widens the
/// system-sender allowlist to the external A2A client.
pub const CONFIG_PROTECTED_SECTIONS: &[&str] = &["delegation", "acp"];

/// Directory holding ephemeral agent scaffolds under `<home>/agents/`.
///
/// Ephemeral agents are invisible to the registry, so `DispatchOrgView` falls
/// back to reading `<home>/agents/.ephemeral/<eph-id>/agent.toml` directly —
/// which makes that file just as authoritative as a regular agent's. Pinned
/// against `duduclaw_gateway::ephemeral::EPHEMERAL_DIR_NAME` by a test there
/// (duduclaw-core cannot depend on the gateway).
pub(super) const EPHEMERAL_DIR_NAME: &str = ".ephemeral";

/// Which delegation-authority file a path refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedTomlKind {
    /// `<home>/agents/<id>/agent.toml` — the authoritative org record.
    AgentToml,
    /// `<home>/config.toml` — delegation policy + ACP trust.
    HomeConfigToml,
}

impl ProtectedTomlKind {
    pub(super) fn file_name(self) -> &'static str {
        match self {
            Self::AgentToml => "agent.toml",
            Self::HomeConfigToml => "config.toml",
        }
    }
}

/// Files that decide *who the caller is* and *whether this guard runs at all*.
///
/// Distinct from [`ProtectedTomlKind`] (which is about the org record's
/// content): here the file is protected as a whole, because a write to it does
/// not change an authorization *input* — it changes the authorization
/// *machinery*.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedSurface {
    /// `<home>/agents/<id>/.mcp.json`, or a `.mcp.json` at any depth below the
    /// employee directory (names compared case-insensitively) — carries `DUDUCLAW_AGENT_ID` and its
    /// `DUDUCLAW_AGENT_TOKEN`. Rewriting the id (soft mode) or pasting a peer's
    /// id+token pair lifted from *their* `.mcp.json` (strict mode) makes the
    /// next MCP server for this agent claim to be someone else.
    AgentMcpJson,
    /// CLI configuration inside an employee directory: anything under a
    /// `.claude/` directory or named `.claude.json` at **any** depth (a
    /// project cloned into the employee directory included, since the CLI
    /// loads it when it starts there), anything under a directory named
    /// `.mcp.json`, and the top-level [`AGENT_RUNTIME_CONFIG_DIRS`]. Slash
    /// commands, subagents, skills, hooks and the non-Claude runtimes' MCP
    /// settings are loaded from there at the next spawn. Refused for agent and
    /// untrusted callers only; reading is unaffected.
    AgentRuntimeConfig,
    /// `<agent_dir>/.claude/settings.json` (or `settings.local.json`) — where
    /// the `duduclaw hook agent-file-guard` `PreToolUse` entry lives. An agent
    /// that may delete this entry may then rewrite everything above.
    HookSettings,
    /// `<home>/identity.key` — the HMAC key. Overwriting it with a
    /// wrong-length blob makes [`crate::identity_token::load_key`] return
    /// `None`, which silently downgrades `require_identity_token = true` to
    /// [`crate::IdentityVerdict::Disabled`]; overwriting it with a *known* key
    /// lets the writer mint tokens for any id.
    IdentityKey,
    /// `<home>/org.toml` — WP22 T1's authoritative org record (see
    /// [`crate::org_store`]). `agent.toml`'s org fields are only a display
    /// mirror now; this file is what the delegation predicate reads, so it is
    /// refused outright like [`Self::IdentityKey`]. Legitimate writes go
    /// through the gated MCP / dashboard paths or `duduclaw org sync`, none of
    /// which run through this hook.
    OrgStore,
}

impl ProtectedSurface {
    pub(super) fn file_name(self) -> &'static str {
        match self {
            Self::AgentMcpJson => ".mcp.json",
            Self::AgentRuntimeConfig => "CLI 設定檔",
            Self::HookSettings => "settings.json",
            Self::IdentityKey => crate::identity_token::IDENTITY_KEY_FILE,
            Self::OrgStore => crate::org_store::ORG_STORE_FILE,
        }
    }
}

/// Env keys inside `.mcp.json` that carry the caller identity.
pub(super) const IDENTITY_ENV_KEYS: [&str; 2] = [crate::ENV_AGENT_ID, crate::identity_token::ENV_AGENT_TOKEN];

/// Top-level directories of an employee directory whose files a CLI loads
/// as configuration at the next spawn — and can turn into executed commands
/// (Claude Code: `.claude/` settings, hooks, slash commands, subagents and
/// skills; the Codex, Gemini, Grok and Antigravity runtimes' own MCP and
/// settings files). Frozen for agent and untrusted callers; DuDuClaw writes
/// them itself, outside the hook.
pub(super) const AGENT_RUNTIME_CONFIG_DIRS: [&str; 5] =
    [".claude", ".codex", ".gemini", ".grok", ".agents"];

/// Top-level files of an employee directory frozen the same way.
pub(super) const AGENT_RUNTIME_CONFIG_FILES: [&str; 1] = [".claude.json"];

/// Hook-configuration basenames under an agent's `.claude/` directory.
pub(super) const HOOK_SETTINGS_FILES: [&str; 2] = ["settings.json", "settings.local.json"];

/// Shell fragments that indicate the command may modify a file.
///
/// `>` covers `>` / `>>` / `1>` / `&>`. The rest are the common in-place or
/// copy-shaped mutators. Deliberately short: every entry must be something an
/// agent has no business running against `agent.toml` / the home `config.toml`.
pub(super) const WRITE_VERBS: &[&str] = &[
    ">", "tee", "sed -i", "sed --in-place", "perl -i", "perl -pi", "mv ", "cp ", "rm ", "dd ",
    "truncate", "install ", "python", "ruby", "node ", "cat <<", "printf",
    // G1 round 2: link creation (a link is the way around a path check),
    // permission / ownership / timestamp changes, sync and database CLIs.
    "ln ", "chmod", "chown", "unlink", "rmdir", "shred", "rsync", "sqlite3", "touch ", "ditto",
];

/// How a frozen entry is compared, and how a change is worded.
///
/// The three variants are exactly the three comparison shapes the pre-O9
/// module carried as three hand-written `diff_*` functions. The wording is
/// part of the contract — these strings are surfaced verbatim into the
/// blocked agent's transcript and are pinned by the regression tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrozenShape {
    /// Per-key **string** comparison inside a named table (`[agent]`).
    ///
    /// A missing table, a missing key, or a non-string value all read as the
    /// empty string, so *deleting* a key counts as a change and replacing the
    /// table with a scalar cannot launder one past the comparison.
    /// Message: `{key}：「{before}」→「{after}」`.
    StringKeys {
        section: &'static str,
        keys: &'static [&'static str],
    },
    /// Union-of-keys walk over a whole table (`[capabilities]`), so a key
    /// added in a later release is frozen the day it lands. A non-table shape
    /// on either side degrades to a whole-section report rather than letting a
    /// reshape launder a change past a key walk.
    /// Message: `{section}.{key}：{before} → {after}`.
    TableKeys { section: &'static str },
    /// Whole-value equality of one top-level section (`[delegation]`,
    /// `[acp]`), so adding, removing or reshaping it all register.
    /// Message: `[{section}]：{before} → {after}`.
    WholeSection { section: &'static str },
    /// Whole-value equality of named keys inside a table (`[prompt]
    /// cli_bare_mode`, `[model] account_pool`), for sections where only some
    /// keys feed a gate. A missing table, a missing key and a non-table
    /// section all read as "absent", so adding, deleting or reshaping
    /// registers. Message: `{section}.{key}：{before} → {after}`.
    ValueKeys {
        section: &'static str,
        keys: &'static [&'static str],
    },
    /// Every top-level key of the file except `editable`, each diffed like
    /// [`Self::TableKeys`] (a non-table value on either side is reported
    /// whole). An allow-list: anything not named editable is frozen.
    AllSectionsExcept { editable: &'static [&'static str] },
}

/// Which [`crate::GuardDecision`] a changed entry produces.
///
/// Entries that share a verdict **and sit next to each other** in
/// [`FROZEN_FIELDS`] are reported together in one decision — that is how
/// `[delegation]` + `[acp]` stay a single `BlockedProtectedSection` listing
/// both changed sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrozenVerdict {
    /// `GuardDecision::BlockedOrgFieldChange`
    OrgField,
    /// `GuardDecision::BlockedProtectedField`
    ProtectedField,
    /// `GuardDecision::BlockedProtectedSection`
    ProtectedSection,
    /// `GuardDecision::BlockedAgentSecuritySection`
    AgentSecuritySection,
}

/// Which callers a frozen entry applies to.
///
/// The WP21 rows predate caller identity and have always applied to every
/// hook invocation, the operator included; they keep doing so. The G1 rows
/// (2026-10) apply only to an agent-identified or untrusted caller, so an
/// operator running `claude` by hand in an agent directory is unaffected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FrozenCallers {
    Everyone,
    AgentsOnly,
}

/// One frozen entry: which file it lives in, how it is compared, and which
/// verdict a change produces.
#[derive(Debug, Clone, Copy)]
pub(super) struct FrozenField {
    pub kind: ProtectedTomlKind,
    pub shape: FrozenShape,
    pub verdict: FrozenVerdict,
    pub callers: FrozenCallers,
}

/// **The** frozen-field table — the single place that says what this guard
/// freezes.
///
/// Order is load-bearing in exactly one way: entries are evaluated top to
/// bottom and the FIRST verdict group with any change wins, so org fields are
/// reported ahead of `[capabilities]` when one write moves both ("you tried to
/// re-parent yourself" is the more serious of the two). That keeps the
/// decision for every pre-existing attack byte-identical to the pre-O9
/// hand-written chain.
pub(super) const FROZEN_FIELDS: &[FrozenField] = &[
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::StringKeys {
            section: AGENT_SECTION,
            keys: AGENT_ORG_FIELDS,
        },
        verdict: FrozenVerdict::OrgField,
        callers: FrozenCallers::Everyone,
    },
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::TableKeys {
            section: AGENT_CAPABILITY_SECTION,
        },
        verdict: FrozenVerdict::ProtectedField,
        callers: FrozenCallers::Everyone,
    },
    FrozenField {
        kind: ProtectedTomlKind::HomeConfigToml,
        shape: FrozenShape::WholeSection {
            section: CONFIG_PROTECTED_SECTIONS[0],
        },
        verdict: FrozenVerdict::ProtectedSection,
        callers: FrozenCallers::Everyone,
    },
    FrozenField {
        kind: ProtectedTomlKind::HomeConfigToml,
        shape: FrozenShape::WholeSection {
            section: CONFIG_PROTECTED_SECTIONS[1],
        },
        verdict: FrozenVerdict::ProtectedSection,
        callers: FrozenCallers::Everyone,
    },
    // G1 round 3: every top-level section that is not on the editable
    // allow-list, so a section added in a later release is frozen the day it
    // lands. `AGENT_SECURITY_SECTIONS` documents why the known ones matter.
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::AllSectionsExcept {
            editable: AGENT_EDITABLE_SECTIONS,
        },
        verdict: FrozenVerdict::AgentSecuritySection,
        callers: FrozenCallers::AgentsOnly,
    },
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::ValueKeys {
            section: AGENT_SECURITY_KEYS[0].0,
            keys: AGENT_SECURITY_KEYS[0].1,
        },
        verdict: FrozenVerdict::AgentSecuritySection,
        callers: FrozenCallers::AgentsOnly,
    },
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::ValueKeys {
            section: AGENT_SECURITY_KEYS[1].0,
            keys: AGENT_SECURITY_KEYS[1].1,
        },
        verdict: FrozenVerdict::AgentSecuritySection,
        callers: FrozenCallers::AgentsOnly,
    },
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::ValueKeys {
            section: AGENT_SECURITY_KEYS[2].0,
            keys: AGENT_SECURITY_KEYS[2].1,
        },
        verdict: FrozenVerdict::AgentSecuritySection,
        callers: FrozenCallers::AgentsOnly,
    },
];

/// The `agent.toml` table the org fields live under.
pub(super) const AGENT_SECTION: &str = "agent";

/// Keep the `[delegation]` / `[acp]` rows above indexable: they read
/// [`CONFIG_PROTECTED_SECTIONS`] by index so the table cannot silently drift
/// out of sync with the constant other code imports.
const _: () = assert!(CONFIG_PROTECTED_SECTIONS.len() == 2);

const _: () = assert!(AGENT_SECURITY_KEYS.len() == 3);

/// Every frozen entry that applies to `kind` for **every** caller, in table
/// order — the pre-G1 contract of [`super::check_protected_toml_write`].
/// Production goes through [`frozen_for_caller`]; this name pins the table
/// in the regression tests.
#[cfg(test)]
pub(super) fn frozen_for(kind: ProtectedTomlKind) -> impl Iterator<Item = &'static FrozenField> {
    frozen_for_caller(kind, false)
}

/// Every frozen entry that applies to `kind`; `agent_caller` adds the
/// [`FrozenCallers::AgentsOnly`] rows.
pub(super) fn frozen_for_caller(
    kind: ProtectedTomlKind,
    agent_caller: bool,
) -> impl Iterator<Item = &'static FrozenField> {
    FROZEN_FIELDS.iter().filter(move |f| {
        f.kind == kind && (agent_caller || f.callers == FrozenCallers::Everyone)
    })
}
