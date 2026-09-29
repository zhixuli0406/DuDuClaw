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
    /// `<home>/agents/<id>/.mcp.json` — carries `DUDUCLAW_AGENT_ID` and its
    /// `DUDUCLAW_AGENT_TOKEN`. Rewriting the id (soft mode) or pasting a peer's
    /// id+token pair lifted from *their* `.mcp.json` (strict mode) makes the
    /// next MCP server for this agent claim to be someone else.
    AgentMcpJson,
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
            Self::HookSettings => "settings.json",
            Self::IdentityKey => crate::identity_token::IDENTITY_KEY_FILE,
            Self::OrgStore => crate::org_store::ORG_STORE_FILE,
        }
    }
}

/// Env keys inside `.mcp.json` that carry the caller identity.
pub(super) const IDENTITY_ENV_KEYS: [&str; 2] = [crate::ENV_AGENT_ID, crate::identity_token::ENV_AGENT_TOKEN];

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
}

/// One frozen entry: which file it lives in, how it is compared, and which
/// verdict a change produces.
#[derive(Debug, Clone, Copy)]
pub(super) struct FrozenField {
    pub kind: ProtectedTomlKind,
    pub shape: FrozenShape,
    pub verdict: FrozenVerdict,
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
    },
    FrozenField {
        kind: ProtectedTomlKind::AgentToml,
        shape: FrozenShape::TableKeys {
            section: AGENT_CAPABILITY_SECTION,
        },
        verdict: FrozenVerdict::ProtectedField,
    },
    FrozenField {
        kind: ProtectedTomlKind::HomeConfigToml,
        shape: FrozenShape::WholeSection {
            section: CONFIG_PROTECTED_SECTIONS[0],
        },
        verdict: FrozenVerdict::ProtectedSection,
    },
    FrozenField {
        kind: ProtectedTomlKind::HomeConfigToml,
        shape: FrozenShape::WholeSection {
            section: CONFIG_PROTECTED_SECTIONS[1],
        },
        verdict: FrozenVerdict::ProtectedSection,
    },
];

/// The `agent.toml` table the org fields live under.
pub(super) const AGENT_SECTION: &str = "agent";

/// Keep the `[delegation]` / `[acp]` rows above indexable: they read
/// [`CONFIG_PROTECTED_SECTIONS`] by index so the table cannot silently drift
/// out of sync with the constant other code imports.
const _: () = assert!(CONFIG_PROTECTED_SECTIONS.len() == 2);

/// Every frozen entry that applies to `kind`, in table order.
pub(super) fn frozen_for(kind: ProtectedTomlKind) -> impl Iterator<Item = &'static FrozenField> {
    FROZEN_FIELDS.iter().filter(move |f| f.kind == kind)
}
