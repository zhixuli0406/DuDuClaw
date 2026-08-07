//! Content-aware guard for the files that the A2A delegation predicate reads
//! (WP21 欠帳 ②).
//!
//! # Why this exists
//!
//! `delegation_policy::can_delegate` decides "who may command whom" from two
//! data sources:
//!
//! - `<home>/agents/<id>/agent.toml` → `[agent] reports_to` (ancestor chain,
//!   both directions), `[agent] department` (same-department lateral rule) and
//!   `[agent] name` (the registry id the whitelist resolves against).
//! - `<home>/config.toml` → `[delegation] policy` / `allow` whitelist and
//!   `[acp] trusted` (which widens the trusted-sender list).
//!
//! …and on two things that decide *who the caller is* and *whether this guard
//! runs at all*:
//!
//! - `<home>/identity.key` + each agent's `.mcp.json` identity env block
//!   (`DUDUCLAW_AGENT_ID` / `DUDUCLAW_AGENT_TOKEN`, see
//!   [`crate::identity_token`]).
//! - `<agent_dir>/.claude/settings.json` — where the `PreToolUse` hook that
//!   invokes this module is registered.
//!
//! See [`check_identity_surface_write`] for that second group.
//!
//! Both are plain files. [`crate::agent_guard::check_agent_file_write`] only
//! checks *where* an agent-structure file is written — an `agent.toml` inside
//! the canonical `<home>/agents/<id>/` tree returns `AllowedAgentWrite`, and
//! `config.toml` is not an agent-structure file at all. So an agent holding
//! Write / Edit could rewrite its own `reports_to` to point at any victim
//! (then claim the "subordinate → ancestor" rule), move itself into another
//! `department`, or simply set `[delegation] policy = "open"`. The judged
//! party owned the evidence.
//!
//! This module closes that: writes to those two files are compared field-wise
//! against what is on disk, and a change to a protected field/section is
//! DENIED. Legitimate changes go through the MCP `agent_update` tool or the
//! dashboard, both of which already carry the WP21 C4 authorization gate.
//!
//! # Enforcement surface
//!
//! Same as `agent_guard`: the Claude Code `PreToolUse` hook
//! (`duduclaw hook agent-file-guard`, registered per agent by
//! `agent_hook_installer`). This module is the pure decision logic; the CLI
//! handler supplies the reconstructed post-write content.
//!
//! # Fail-closed rules
//!
//! - Unparseable *new* content → DENY (writing broken TOML breaks the agent
//!   anyway, and an unparseable file cannot be field-compared).
//! - Unparseable *existing* content → DENY (nothing to compare against).
//! - Unreconstructable write intent (e.g. an Edit whose `old_string` is
//!   missing from the envelope) → DENY, reported by the caller.
//! - File does not exist yet → ALLOW. Creating an agent already goes through
//!   `create_agent`, which carries its own C4 gate; there is no prior
//!   organisational state to protect.

use std::path::Path;

use crate::agent_guard::{lexical_normalize, GuardDecision};

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
const EPHEMERAL_DIR_NAME: &str = ".ephemeral";

/// Which delegation-authority file a path refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtectedTomlKind {
    /// `<home>/agents/<id>/agent.toml` — the authoritative org record.
    AgentToml,
    /// `<home>/config.toml` — delegation policy + ACP trust.
    HomeConfigToml,
}

impl ProtectedTomlKind {
    fn file_name(self) -> &'static str {
        match self {
            Self::AgentToml => "agent.toml",
            Self::HomeConfigToml => "config.toml",
        }
    }
}

/// Classify `file_path` as one of the delegation-authority files, or `None`.
///
/// Only the *authoritative* locations qualify:
/// - `agent.toml` at `<home>/agents/<id>/agent.toml` — the path the registry
///   loads and `DispatchOrgView` falls back to.
/// - `agent.toml` at `<home>/agents/.ephemeral/<eph-id>/agent.toml` —
///   ephemeral scaffolds are invisible to the registry but their `reports_to`
///   is exactly what authorises their dispatch, so the file is authoritative
///   too.
/// - `config.toml` exactly at `<home>/config.toml` — **not** any other
///   `config.toml`, since that basename is extremely common in the user
///   projects agents work on (Rust, Hugo, …).
///
/// Any deeper `agent.toml` is inert (nothing loads it), so guarding it would
/// only produce false positives.
///
/// Comparison is lexical (no filesystem access, no symlink resolution) and
/// case-insensitive, matching `agent_guard`'s handling of macOS / Windows.
pub fn classify_protected_toml(file_path: &Path, home: &Path) -> Option<ProtectedTomlKind> {
    let file_name = file_path.file_name()?.to_str()?;
    let normalized = lexical_normalize(file_path);

    match file_name {
        "agent.toml" => {
            let agents_root = lexical_normalize(&home.join("agents"));
            let rest = components_after_ci(&normalized, &agents_root)?;
            match rest.len() {
                // `<agents_root>/<id>/agent.toml`
                2 => Some(ProtectedTomlKind::AgentToml),
                // `<agents_root>/.ephemeral/<eph-id>/agent.toml`
                3 if rest[0].eq_ignore_ascii_case(EPHEMERAL_DIR_NAME) => {
                    Some(ProtectedTomlKind::AgentToml)
                }
                _ => None,
            }
        }
        "config.toml" => {
            let expected = lexical_normalize(&home.join("config.toml"));
            match components_after_ci(&normalized, &expected) {
                Some(rest) if rest.is_empty() => Some(ProtectedTomlKind::HomeConfigToml),
                _ => None,
            }
        }
        _ => None,
    }
}

/// Decide whether a write of `new_content` to `file_path` may proceed.
///
/// `existing` is the current on-disk content (`None` when the file does not
/// exist yet). Returns [`GuardDecision::NotAgentFile`] when the path is not a
/// delegation-authority file — the caller should then fall through to whatever
/// other guards apply.
pub fn check_protected_toml_write(
    file_path: &Path,
    home: &Path,
    existing: Option<&str>,
    new_content: &str,
) -> GuardDecision {
    let Some(kind) = classify_protected_toml(file_path, home) else {
        return GuardDecision::NotAgentFile;
    };
    let attempted_path = lexical_normalize(file_path);

    let new_table = match new_content.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            return GuardDecision::BlockedUnverifiable {
                file_name: kind.file_name().to_string(),
                attempted_path,
                reason: format!("寫入內容不是合法的 TOML：{}", first_line(&e.to_string())),
            };
        }
    };

    // Brand-new file: no prior organisational state to protect.
    let Some(existing) = existing else {
        return GuardDecision::AllowedAgentWrite;
    };

    let old_table = match existing.parse::<toml::Table>() {
        Ok(t) => t,
        Err(e) => {
            return GuardDecision::BlockedUnverifiable {
                file_name: kind.file_name().to_string(),
                attempted_path,
                reason: format!(
                    "現有檔案不是合法的 TOML，無法比對受保護欄位：{}",
                    first_line(&e.to_string())
                ),
            };
        }
    };

    match kind {
        ProtectedTomlKind::AgentToml => {
            let changed = diff_agent_org_fields(&old_table, &new_table);
            if changed.is_empty() {
                GuardDecision::AllowedAgentWrite
            } else {
                GuardDecision::BlockedOrgFieldChange {
                    file_name: kind.file_name().to_string(),
                    attempted_path,
                    changed,
                }
            }
        }
        ProtectedTomlKind::HomeConfigToml => {
            let changed = diff_config_sections(&old_table, &new_table);
            if changed.is_empty() {
                GuardDecision::AllowedAgentWrite
            } else {
                GuardDecision::BlockedProtectedSection {
                    file_name: kind.file_name().to_string(),
                    attempted_path,
                    changed,
                }
            }
        }
    }
}

// ── Identity / enforcement surface ───────────────────────────────────────────

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
    fn file_name(self) -> &'static str {
        match self {
            Self::AgentMcpJson => ".mcp.json",
            Self::HookSettings => "settings.json",
            Self::IdentityKey => crate::identity_token::IDENTITY_KEY_FILE,
            Self::OrgStore => crate::org_store::ORG_STORE_FILE,
        }
    }
}

/// Env keys inside `.mcp.json` that carry the caller identity.
const IDENTITY_ENV_KEYS: [&str; 2] = [crate::ENV_AGENT_ID, crate::identity_token::ENV_AGENT_TOKEN];

/// Hook-configuration basenames under an agent's `.claude/` directory.
const HOOK_SETTINGS_FILES: [&str; 2] = ["settings.json", "settings.local.json"];

/// Classify `file_path` as one of the identity / enforcement-surface files.
///
/// Scoped exactly like [`classify_protected_toml`]: only paths under
/// `<home>/agents/…` (plus the single `<home>/identity.key`) qualify, so a
/// `settings.json` or `.mcp.json` in a user project the agent is working on is
/// none of this guard's business.
pub fn classify_identity_surface(file_path: &Path, home: &Path) -> Option<ProtectedSurface> {
    let file_name = file_path.file_name()?.to_str()?;
    let normalized = lexical_normalize(file_path);

    if file_name.eq_ignore_ascii_case(crate::identity_token::IDENTITY_KEY_FILE) {
        let expected = lexical_normalize(&home.join(crate::identity_token::IDENTITY_KEY_FILE));
        return match components_after_ci(&normalized, &expected) {
            Some(rest) if rest.is_empty() => Some(ProtectedSurface::IdentityKey),
            _ => None,
        };
    }

    // WP22 T1 — `<home>/org.toml` exactly (not any other `org.toml` in a user
    // project), same shape as the `identity.key` check above. `<home>/
    // .org-seeded` rides along: it is what stops a deleted store from being
    // re-imported from the (possibly tampered) `agent.toml` mirrors, so an
    // agent that could delete *it* could re-open that laundering channel.
    for basename in [
        crate::org_store::ORG_STORE_FILE,
        crate::org_store::ORG_SEEDED_FILE,
    ] {
        if file_name.eq_ignore_ascii_case(basename) {
            let expected = lexical_normalize(&home.join(basename));
            return match components_after_ci(&normalized, &expected) {
                Some(rest) if rest.is_empty() => Some(ProtectedSurface::OrgStore),
                _ => None,
            };
        }
    }

    let agents_root = lexical_normalize(&home.join("agents"));
    let rest = components_after_ci(&normalized, &agents_root)?;
    // `<agent-id>/…/<file>` — at least an agent directory plus the basename.
    if rest.len() < 2 {
        return None;
    }

    if file_name == ".mcp.json" {
        return Some(ProtectedSurface::AgentMcpJson);
    }
    if HOOK_SETTINGS_FILES
        .iter()
        .any(|f| file_name.eq_ignore_ascii_case(f))
        && rest
            .get(rest.len() - 2)
            .is_some_and(|parent| parent.eq_ignore_ascii_case(".claude"))
    {
        return Some(ProtectedSurface::HookSettings);
    }
    None
}

/// Decide whether a write to an identity / enforcement-surface file may proceed.
///
/// `new_content` is `None` when the caller could not reconstruct the post-write
/// content from the tool envelope — treated as unverifiable, i.e. DENIED.
///
/// [`ProtectedSurface::HookSettings`] and [`ProtectedSurface::IdentityKey`] are
/// refused outright: unlike the org fields, there is no "harmless edit" shape
/// for them, and both have a supported out-of-band writer (the gateway's hook
/// installer / `ensure_identity_key`) that does not run through this hook.
/// `.mcp.json` is compared field-wise so an agent may still add an unrelated
/// MCP server (Playwright, …) to its own file — only the identity env pairs are
/// frozen, in **every** server entry, so a second `duduclaw`-shaped server with
/// someone else's id cannot be appended either.
pub fn check_identity_surface_write(
    file_path: &Path,
    home: &Path,
    existing: Option<&str>,
    new_content: Option<&str>,
) -> GuardDecision {
    let Some(surface) = classify_identity_surface(file_path, home) else {
        return GuardDecision::NotAgentFile;
    };
    let attempted_path = lexical_normalize(file_path);

    let blocked = |reason: String| GuardDecision::BlockedIdentitySurface {
        file_name: surface.file_name().to_string(),
        attempted_path: attempted_path.clone(),
        reason,
    };

    match surface {
        ProtectedSurface::IdentityKey => blocked(
            "identity.key 是身分簽章金鑰，覆寫它等同關閉或接管身分驗證；它只由 DuDuClaw 本身產生。"
                .to_string(),
        ),
        ProtectedSurface::OrgStore => blocked(
            "org.toml 是組織關係（誰是誰的主管、屬於哪個部門）的唯一權威來源，\
             委派判定只看它；連同它的建檔標記都不可由 AI 員工改寫或刪除。\
             請透過儀表板的組織圖調整，或由管理者執行 `duduclaw org sync`。"
                .to_string(),
        ),
        ProtectedSurface::HookSettings => blocked(
            "這個檔案登記了檔案保護 hook，若可自行改寫等同解除保護；請透過儀表板或管理者調整。"
                .to_string(),
        ),
        ProtectedSurface::AgentMcpJson => {
            let Some(new_content) = new_content else {
                return GuardDecision::BlockedUnverifiable {
                    file_name: surface.file_name().to_string(),
                    attempted_path,
                    reason: "無法還原寫入後的內容，無法確認身分設定是否被更動".to_string(),
                };
            };
            let new_ids = match identity_env_pairs(new_content) {
                Ok(v) => v,
                Err(e) => {
                    return GuardDecision::BlockedUnverifiable {
                        file_name: surface.file_name().to_string(),
                        attempted_path,
                        reason: format!("寫入內容不是合法的 JSON：{}", first_line(&e)),
                    };
                }
            };
            // Absent file ⇒ nothing declared before, so declaring any identity
            // pair now is a change (fail-closed: `.mcp.json` is written by
            // DuDuClaw, never by an agent).
            let old_ids = match existing.map(identity_env_pairs).transpose() {
                Ok(v) => v.unwrap_or_default(),
                Err(e) => {
                    return GuardDecision::BlockedUnverifiable {
                        file_name: surface.file_name().to_string(),
                        attempted_path,
                        reason: format!("現有檔案不是合法的 JSON，無法比對：{}", first_line(&e)),
                    };
                }
            };
            if new_ids == old_ids {
                GuardDecision::AllowedAgentWrite
            } else {
                blocked(format!(
                    "MCP 身分設定（{}）被更動：{} → {}",
                    IDENTITY_ENV_KEYS.join(" / "),
                    describe_pairs(&old_ids),
                    describe_pairs(&new_ids)
                ))
            }
        }
    }
}

/// Every `(server, key, value)` identity triple declared in a `.mcp.json`,
/// sorted so the comparison ignores JSON key ordering.
///
/// Errors only on invalid JSON. A structurally odd but valid document (no
/// `mcpServers`, a non-object entry, …) simply declares no identity, which is
/// correct: there is nothing to protect and nothing to compare.
fn identity_env_pairs(content: &str) -> Result<Vec<(String, String, String)>, String> {
    let doc: serde_json::Value = serde_json::from_str(content).map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    if let Some(servers) = doc.get("mcpServers").and_then(|v| v.as_object()) {
        for (server, def) in servers {
            let Some(env) = def.get("env").and_then(|v| v.as_object()) else {
                continue;
            };
            for (k, v) in env {
                if IDENTITY_ENV_KEYS.iter().any(|known| known == k) {
                    out.push((
                        server.clone(),
                        k.clone(),
                        v.as_str().unwrap_or_default().to_string(),
                    ));
                }
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Render for the block message. The **token value is never printed** — it is
/// a credential, and the message is surfaced back into the agent's transcript;
/// only its presence/absence is informative anyway.
fn describe_pairs(pairs: &[(String, String, String)]) -> String {
    if pairs.is_empty() {
        return "（未設定）".to_string();
    }
    crate::truncate_chars(
        &pairs
            .iter()
            .map(|(server, key, value)| {
                if key == crate::identity_token::ENV_AGENT_TOKEN {
                    format!("{server}.{key}=（已隱藏）")
                } else {
                    format!("{server}.{key}={}", crate::truncate_chars(value, 64))
                }
            })
            .collect::<Vec<_>>()
            .join(", "),
        160,
    )
}

// ── Caller-scoped directory isolation (WP22 T2) ──────────────────────────────

/// Who is performing the hook-mediated write.
///
/// The WP21 guards above are *content*-scoped: they compare what a write would
/// produce against what is on disk, with no notion of who is writing. That left
/// a whole class open — agent A holding Write/Edit could rewrite agent B's
/// `SOUL.md`, `MEMORY.md`, `CLAUDE.md`, or any other file in B's directory,
/// none of which is an org field or an identity surface.
///
/// The hook resolves this from the ambient `DUDUCLAW_AGENT_ID` (+
/// `DUDUCLAW_AGENT_TOKEN` when `<home>/identity.key` exists, verified through
/// [`crate::identity_token::verify_claim`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HookCaller {
    /// No `DUDUCLAW_AGENT_ID` in the environment.
    ///
    /// **Deliberately unrestricted.** This hook is registered only in an
    /// agent's own `.claude/settings.json`, so an invocation without any agent
    /// identity is an operator running `claude` (or the tooling) by hand —
    /// blocking them would be a false positive on the one caller who is
    /// entitled to touch every agent directory. The WP21 content guards still
    /// apply to them; only the directory-scope rule is skipped.
    Absent,
    /// A claimed caller id that may be used for scoping: verified, or
    /// unverified in soft mode, or an install with no `identity.key` at all.
    ///
    /// Soft mode counts as scopable on purpose — it matches
    /// [`crate::IdentityVerdict::is_trusted`], and the alternative (ignore the
    /// claim) would make the rule inert on every install that has not enabled
    /// strict mode, i.e. almost all of them. A caller who forges a *different*
    /// id in soft mode gains nothing here: it only moves which single directory
    /// they may write, and `.mcp.json` — where the id lives — is itself frozen
    /// by [`check_identity_surface_write`].
    Agent(String),
    /// `require_identity_token = true` and the claim failed verification.
    Untrusted(String),
}

impl HookCaller {
    fn claimed(&self) -> &str {
        match self {
            Self::Absent => "",
            Self::Agent(id) | Self::Untrusted(id) => id.as_str(),
        }
    }
}

/// Decide whether `caller` may write `file_path` **at all**, before any
/// content comparison.
///
/// Two rules, both fail-closed and both no-ops for [`HookCaller::Absent`]:
///
/// 1. `<home>/agents/<other>/**` where `<other>` is not the caller → DENY.
///    `<home>/agents/.ephemeral/<eph-id>/**` is owned by `<eph-id>`, so an
///    ephemeral agent may write its own scaffold and nobody else's.
/// 2. `<home>/config.toml` → DENY outright (WP22 supersedes WP21's
///    section-level comparison for agent callers; every legitimate writer —
///    dashboard RPC, MCP tools, the gateway itself — goes through Rust and
///    never through this hook).
///
/// Returns [`GuardDecision::NotAgentFile`] when the path is outside both
/// scopes, so the caller falls through to the WP21 guards.
pub fn check_caller_scope(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    if matches!(caller, HookCaller::Absent) {
        return GuardDecision::NotAgentFile;
    }
    let normalized = lexical_normalize(file_path);

    // Rule 2 — the home config.toml, whole file.
    let home_config = lexical_normalize(&home.join("config.toml"));
    if matches!(components_after_ci(&normalized, &home_config), Some(rest) if rest.is_empty()) {
        return GuardDecision::BlockedHomeConfigWrite {
            caller: caller.claimed().to_string(),
            attempted_path: normalized,
        };
    }

    // Rule 1 — someone else's agent directory.
    let Some(owner) = owning_agent_dir(&normalized, home) else {
        return GuardDecision::NotAgentFile;
    };
    match caller {
        HookCaller::Absent => unreachable!("handled above"),
        HookCaller::Untrusted(claimed) => GuardDecision::BlockedUntrustedCaller {
            caller: claimed.clone(),
            attempted_path: normalized,
        },
        HookCaller::Agent(id) if owner.eq_ignore_ascii_case(id) => GuardDecision::NotAgentFile,
        HookCaller::Agent(id) => GuardDecision::BlockedForeignAgentDir {
            caller: id.clone(),
            owner,
            attempted_path: normalized,
        },
    }
}

/// The agent directory name that owns `normalized`, or `None` when the path is
/// not inside any agent directory.
///
/// `<home>/agents/<id>/…` → `<id>`; `<home>/agents/.ephemeral/<eph>/…` →
/// `<eph>`. A path directly at `<home>/agents/<x>` (no further component) is
/// **not** owned — it is either the agent directory itself or a stray file at
/// the agents root, and `check_agent_file_write` already covers the latter.
fn owning_agent_dir(normalized: &Path, home: &Path) -> Option<String> {
    let agents_root = lexical_normalize(&home.join("agents"));
    let rest = components_after_ci(normalized, &agents_root)?;
    if rest.first()?.eq_ignore_ascii_case(EPHEMERAL_DIR_NAME) {
        // `<agents>/.ephemeral/<eph-id>/<something>` — need the trailing
        // component, otherwise this is the scaffold directory itself.
        return (rest.len() >= 3).then(|| rest[1].clone());
    }
    (rest.len() >= 2).then(|| rest[0].clone())
}

/// WP1.1 C3 (SOUL.md 唯讀化) — block an agent-identified caller from writing
/// its OWN `SOUL.md` via Write/Edit/MultiEdit.
///
/// [`check_caller_scope`] above only rejects writes into *another* agent's
/// directory; a caller writing inside its own directory returns
/// [`GuardDecision::NotAgentFile`] there and falls through to the location /
/// content guards, none of which know about SOUL.md specifically. This closes
/// that gap: SOUL.md is no longer self-writable through the raw Write/Edit
/// tools by anyone, including its own owning agent. `HookCaller::Absent`
/// (operator running by hand) is a no-op, matching every other
/// caller-scoped check in this module. Deliberately **not** gated by
/// `can_modify_own_soul` — see [`crate::GuardDecision::BlockedOwnSoulWrite`]'s
/// doc comment for why the flag only applies to the MCP-side gate.
pub fn check_own_soul_write(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    let HookCaller::Agent(caller_id) = caller else {
        return GuardDecision::NotAgentFile;
    };
    match file_path.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.eq_ignore_ascii_case("SOUL.md") => {}
        _ => return GuardDecision::NotAgentFile,
    }
    let normalized = lexical_normalize(file_path);
    let Some(owner) = owning_agent_dir(&normalized, home) else {
        return GuardDecision::NotAgentFile;
    };
    if owner.eq_ignore_ascii_case(caller_id) {
        GuardDecision::BlockedOwnSoulWrite {
            caller: caller_id.clone(),
            attempted_path: normalized,
        }
    } else {
        // A foreign agent's SOUL.md is already caught (and blocked) by
        // `check_caller_scope`, which runs first in the hook pipeline.
        GuardDecision::NotAgentFile
    }
}

/// Bash-side companion: block obviously write-shaped shell commands that
/// mention a delegation-authority file.
///
/// This is a **speed bump, not a boundary** — shell text cannot be analysed
/// reliably (`T=agent.toml; echo x > $T`, base64, `python3 -c`, … all evade
/// it). It exists so the naive `echo … > agent.toml` bypass named in the WP21
/// review does not silently work, and so the agent gets a message pointing at
/// `agent_update`. The real containment for Bash is the per-agent
/// `[capabilities]` tool policy (deny Bash for agents that do not need it).
///
/// Conservative by design, matching `check_bash_command`'s philosophy: a
/// read-only command that happens to combine a write verb with the filename
/// (`cp agent.toml backup.toml`) is blocked too.
///
/// # WP22 T2 follow-up — caller-scoped directory isolation for Bash
///
/// `check_caller_scope` closes the Write/Edit/MultiEdit gap where agent A
/// could rewrite agent B's `SOUL.md` / `MEMORY.md` / anything else in B's
/// directory that is not a DuDuClaw-specific basename this function already
/// recognises. Bash reaches the same files (`cat > ../ceo/SOUL.md`, `sed -i
/// … ../ceo/notes.txt`, …), so when `caller` carries a verified-or-soft
/// agent identity, a command that both (a) mentions `agents/<other>/` for an
/// `<other>` that is not the caller and looks like a real agent id, and (b)
/// contains a write verb, is blocked the same way — same conservative
/// philosophy, same false-positive tolerance. `HookCaller::Absent` and
/// `HookCaller::Untrusted` are no-ops here: an absent identity is an operator
/// running by hand (unrestricted, matching `check_caller_scope`), and an
/// untrusted claim is already refused outright by the Write/Edit lane before
/// this Bash check would even be reached for the same caller.
pub fn check_bash_protected_write(command: &str, home: &Path, caller: &HookCaller) -> GuardDecision {
    let normalized: String = command
        .chars()
        .map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() })
        .collect();

    if let HookCaller::Agent(caller_id) = caller {
        if let Some(owner) = mentions_other_agent_dir(&normalized, caller_id) {
            if write_verb(&normalized).is_some() {
                return GuardDecision::BlockedForeignAgentDir {
                    caller: caller_id.clone(),
                    owner: owner.clone(),
                    attempted_path: home.join("agents").join(&owner),
                };
            }
        }

        // WP1.1 C3 — same speed-bump philosophy as the delegation-authority
        // checks below: a write-shaped command targeting the caller's OWN
        // `SOUL.md` — either an explicit `agents/<caller_id>/…/SOUL.md` path,
        // or a bare/relative reference with no `agents/` path segment at all
        // (an agent's Bash cwd is its own agent directory, so `echo … >
        // SOUL.md` targets its own file) — is blocked. Deliberately does
        // NOT fire on a command that mentions `agents/` at all without also
        // matching the caller's own prefix: that is either the foreign-dir
        // rule above (already handled), a false positive like
        // `myagents/ceo/SOUL.md` (must not match `agents/` at all — same
        // boundary caveat as `mentions_other_agent_dir`), or simply none of
        // this rule's business. See `check_own_soul_write` for the precise
        // Write/Edit-lane version.
        if mentions_own_soul_md(&normalized, caller_id) && write_verb(&normalized).is_some() {
            return GuardDecision::BlockedOwnSoulWrite {
                caller: caller_id.clone(),
                attempted_path: home.join("agents").join(caller_id).join("SOUL.md"),
            };
        }
    }

    // `agent.toml` anywhere: the basename is DuDuClaw-specific, and writing
    // one outside the canonical tree is already forbidden by `agent_guard`.
    let mentions_agent_toml = normalized.contains("agent.toml");

    // `config.toml`: only the DuDuClaw home one. Any other `config.toml`
    // belongs to a user project the agent may legitimately be editing.
    let home_config = lexical_normalize(&home.join("config.toml"))
        .to_string_lossy()
        .to_ascii_lowercase()
        .replace('\\', "/");
    let mentions_home_config =
        normalized.contains(&home_config) || normalized.contains(".duduclaw/config.toml");

    // WP22 T5 — the authoritative org store and its bootstrap marker. Same
    // scoping rule as `config.toml`: only the DuDuClaw home ones, because
    // `org.toml` could plausibly be a file in a user project. Deleting either
    // is as damaging as rewriting them (`rm org.toml` degrades every
    // delegation decision back to the `agent.toml` mirrors), and `rm ` is
    // already in `WRITE_VERBS`.
    let mentions_org_store = [
        crate::org_store::ORG_STORE_FILE,
        crate::org_store::ORG_SEEDED_FILE,
    ]
    .iter()
    .any(|basename| {
        let home_path = lexical_normalize(&home.join(basename))
            .to_string_lossy()
            .to_ascii_lowercase()
            .replace('\\', "/");
        normalized.contains(&home_path) || normalized.contains(&format!(".duduclaw/{basename}"))
    });

    // Identity / enforcement surface (see `check_identity_surface_write`).
    // `.mcp.json` and `identity.key` are DuDuClaw-specific basenames; the hook
    // settings file is matched only through its `.claude/` parent so a
    // project's own `settings.json` is untouched.
    let mentions_mcp_json = normalized.contains(".mcp.json");
    let mentions_identity_key = normalized.contains(crate::identity_token::IDENTITY_KEY_FILE);
    let mentions_hook_settings = HOOK_SETTINGS_FILES
        .iter()
        .any(|f| normalized.contains(&format!(".claude/{f}")));

    if !mentions_agent_toml
        && !mentions_home_config
        && !mentions_org_store
        && !mentions_mcp_json
        && !mentions_identity_key
        && !mentions_hook_settings
    {
        return GuardDecision::NotAgentFile;
    }

    let Some(verb) = write_verb(&normalized) else {
        return GuardDecision::NotAgentFile;
    };

    let file_name = if mentions_agent_toml {
        "agent.toml"
    } else if mentions_org_store {
        crate::org_store::ORG_STORE_FILE
    } else if mentions_home_config {
        "config.toml"
    } else if mentions_identity_key {
        crate::identity_token::IDENTITY_KEY_FILE
    } else if mentions_mcp_json {
        ".mcp.json"
    } else {
        "settings.json"
    };

    GuardDecision::BlockedBashProtectedWrite {
        file_name: file_name.to_string(),
        verb: verb.to_string(),
    }
}

/// Shell fragments that indicate the command may modify a file.
///
/// `>` covers `>` / `>>` / `1>` / `&>`. The rest are the common in-place or
/// copy-shaped mutators. Deliberately short: every entry must be something an
/// agent has no business running against `agent.toml` / the home `config.toml`.
const WRITE_VERBS: &[&str] = &[
    ">", "tee", "sed -i", "sed --in-place", "perl -i", "perl -pi", "mv ", "cp ", "rm ", "dd ",
    "truncate", "install ", "python", "ruby", "node ", "cat <<", "printf",
];

fn write_verb(normalized_command: &str) -> Option<&'static str> {
    WRITE_VERBS
        .iter()
        .find(|v| normalized_command.contains(**v))
        .copied()
}

/// Every `<id>` in a boundary-checked `agents/<id>/` path segment found in
/// `normalized_command` (already lowercased), in order of appearance.
///
/// Anchored with a lightweight boundary check on the character preceding
/// `agents/` (must not be alnum/`-`/`_`) so `myagents/foo` does not falsely
/// match — the rest is deliberately unanchored text search, same tradeoff as
/// the rest of this speed bump.
///
/// Does not special-case `agents/.ephemeral/<eph-id>/` — the segment right
/// after `agents/` there is `.ephemeral`, which fails the id-shape check (a
/// leading `.` is not alnum/`-`/`_`) and is simply skipped. Reaching into an
/// ephemeral scaffold via Bash is therefore not caught by this heuristic; the
/// Write/Edit lane's `check_caller_scope` still is, and Bash is a speed bump
/// by design, not a boundary.
fn agents_dir_segments(normalized_command: &str) -> Vec<&str> {
    let bytes = normalized_command.as_bytes();
    let mut search_from = 0usize;
    let mut out = Vec::new();
    while let Some(rel_idx) = normalized_command[search_from..].find("agents/") {
        let match_start = search_from + rel_idx;
        let boundary_ok = match_start == 0
            || !matches!(bytes[match_start - 1], b'a'..=b'z' | b'0'..=b'9' | b'_' | b'-');
        let seg_start = match_start + "agents/".len();
        let rest = &normalized_command[seg_start..];
        let seg_end = rest
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'))
            .unwrap_or(rest.len());
        let seg = &rest[..seg_end];
        // Always advance past this match — `seg_start` is strictly greater
        // than `search_from` (it adds at least "agents/".len()), so the loop
        // terminates regardless of whether this iteration matches.
        search_from = seg_start;
        if boundary_ok && !seg.is_empty() {
            out.push(seg);
        }
    }
    out
}

/// Find a `agents/<id>/` path segment in `normalized_command` whose `<id>`
/// is not `caller_id`. Returns the first match. See
/// [`agents_dir_segments`] for the boundary-check contract.
fn mentions_other_agent_dir(normalized_command: &str, caller_id: &str) -> Option<String> {
    let caller_lower = caller_id.to_ascii_lowercase();
    agents_dir_segments(normalized_command)
        .into_iter()
        .find(|seg| *seg != caller_lower)
        .map(str::to_string)
}

/// WP1.1 C3 — whether `normalized_command` (already lowercased) targets
/// `caller_id`'s OWN `SOUL.md` via Bash.
///
/// Judged per whitespace-separated token rather than the whole command
/// string, so a `soul.md`-ending token can be classified by its OWN
/// directory prefix (or lack thereof) independent of unrelated `agents/`
/// text elsewhere in the command. A token is "own" when either:
///
/// - it is a bare/relative filename (no `/` at all) — an agent's Bash cwd is
///   its own agent directory, so an unqualified `SOUL.md` can only resolve
///   to its own file; or
/// - its directory portion contains a boundary-checked `agents/<caller_id>/`
///   segment (see [`agents_dir_segments`] — `myagents/<caller_id>/SOUL.md`
///   does NOT count, same false-positive guard as `mentions_other_agent_dir`
///   uses, and is why this is directory-scoped per-token rather than a
///   whole-string substring search).
///
/// A token naming a *different* agent's directory is not "own" here — that
/// case is caught earlier by [`mentions_other_agent_dir`] when a write verb
/// is also present; in isolation it is simply not this caller's file.
fn mentions_own_soul_md(normalized_command: &str, caller_id: &str) -> bool {
    let caller_lower = caller_id.to_ascii_lowercase();
    normalized_command.split_whitespace().any(|raw_tok| {
        let tok = raw_tok.trim_matches(|c: char| matches!(c, '\'' | '"' | '>' | '<'));
        // Only a token whose filename component is *exactly* `soul.md`
        // qualifies — `strip_suffix` on e.g. "old_soul.md" leaves a
        // non-empty, non-`/`-terminated remainder ("old_"), which the match
        // below correctly rejects instead of treating it as a directory.
        let Some(rest) = tok.strip_suffix("soul.md") else {
            return false;
        };
        match rest.strip_suffix('/') {
            None if rest.is_empty() => true, // bare "soul.md" ⇒ own file (cwd is the agent's own dir)
            None => false,                   // e.g. "old_soul.md" — not a real path boundary
            Some(dir) if dir.is_empty() => true, // "/soul.md" — no agent dir named at all
            Some(dir) => agents_dir_segments(dir).into_iter().any(|seg| seg == caller_lower),
        }
    })
}

/// Compare `[agent] reports_to` / `[agent] department` between two documents.
///
/// A missing `[agent]` table, a missing key, or a non-string value all read as
/// the empty string, so *deleting* `reports_to` counts as a change (it would
/// silently reset the agent to a root node) and replacing the table with a
/// scalar cannot launder a change past the comparison.
fn diff_agent_org_fields(old: &toml::Table, new: &toml::Table) -> Vec<String> {
    AGENT_ORG_FIELDS
        .iter()
        .filter_map(|field| {
            let before = agent_field(old, field);
            let after = agent_field(new, field);
            (before != after).then(|| format!("{field}：「{before}」→「{after}」"))
        })
        .collect()
}

fn agent_field(table: &toml::Table, field: &str) -> String {
    table
        .get("agent")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(field))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Compare the protected `config.toml` sections structurally.
///
/// Whole-value equality (not key-by-key) so adding, removing, or reshaping a
/// section all register. Absent on both sides = unchanged.
fn diff_config_sections(old: &toml::Table, new: &toml::Table) -> Vec<String> {
    CONFIG_PROTECTED_SECTIONS
        .iter()
        .filter_map(|section| {
            let before = old.get(*section);
            let after = new.get(*section);
            (before != after).then(|| {
                format!(
                    "[{section}]：{} → {}",
                    describe_section(before),
                    describe_section(after)
                )
            })
        })
        .collect()
}

fn describe_section(value: Option<&toml::Value>) -> String {
    match value {
        None => "（不存在）".to_string(),
        Some(v) => crate::truncate_chars(&v.to_string().replace('\n', " "), 80),
    }
}

fn first_line(s: &str) -> String {
    crate::truncate_chars(s.lines().next().unwrap_or_default(), 120)
}

/// Case-insensitive component-wise prefix check returning the components of
/// `path` that follow `prefix`.
///
/// Sibling of `agent_guard::strip_prefix_ci` (private there) — kept private
/// here too so the two guards can evolve independently.
fn components_after_ci(path: &Path, prefix: &Path) -> Option<Vec<String>> {
    let mut path_comps = path.components();
    for pc in prefix.components() {
        let actual = path_comps.next()?;
        if !pc
            .as_os_str()
            .to_string_lossy()
            .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy())
        {
            return None;
        }
    }
    Some(
        path_comps
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn home() -> PathBuf {
        PathBuf::from("/Users/alice/.duduclaw")
    }

    fn agent_toml() -> PathBuf {
        home().join("agents/agnes/agent.toml")
    }

    const BASE: &str = r#"
[agent]
name = "agnes"
display_name = "Agnes"
role = "assistant"
status = "active"
trigger = "@agnes"
reports_to = "ceo"
icon = "🐾"
department = "engineering"

[model]
preferred = "sonnet"
"#;

    // ── classification ─────────────────────────────────────────────

    #[test]
    fn classifies_canonical_agent_toml() {
        assert_eq!(
            classify_protected_toml(&agent_toml(), &home()),
            Some(ProtectedTomlKind::AgentToml)
        );
    }

    #[test]
    fn ignores_nested_non_authoritative_agent_toml() {
        // `<home>/agents/agnes/sub/agent.toml` is never loaded by the
        // registry, so guarding it would only produce false positives.
        let p = home().join("agents/agnes/sub/agent.toml");
        assert_eq!(classify_protected_toml(&p, &home()), None);
    }

    #[test]
    fn classifies_ephemeral_agent_toml() {
        // `DispatchOrgView` reads this file directly for `eph-*` ids, so its
        // `reports_to` authorises dispatch just like a registry agent's.
        let p = home().join("agents/.ephemeral/eph-abc123/agent.toml");
        assert_eq!(
            classify_protected_toml(&p, &home()),
            Some(ProtectedTomlKind::AgentToml)
        );
    }

    #[test]
    fn ephemeral_reports_to_change_is_denied() {
        let p = home().join("agents/.ephemeral/eph-abc123/agent.toml");
        let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
        assert!(matches!(
            check_protected_toml_write(&p, &home(), Some(BASE), &new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn ignores_other_depth3_agent_toml() {
        // Only `.ephemeral` gets the depth-3 exemption; a random nested dir
        // is inert and must not be guarded.
        let p = home().join("agents/team/agnes/agent.toml");
        assert_eq!(classify_protected_toml(&p, &home()), None);
    }

    #[test]
    fn ignores_agent_toml_outside_home() {
        // Already handled (blocked) by `agent_guard::check_agent_file_write`.
        let p = PathBuf::from("/Users/alice/Project/x/agent.toml");
        assert_eq!(classify_protected_toml(&p, &home()), None);
    }

    #[test]
    fn classifies_home_config_toml() {
        assert_eq!(
            classify_protected_toml(&home().join("config.toml"), &home()),
            Some(ProtectedTomlKind::HomeConfigToml)
        );
    }

    #[test]
    fn ignores_project_config_toml() {
        let p = PathBuf::from("/Users/alice/Project/rustapp/config.toml");
        assert_eq!(classify_protected_toml(&p, &home()), None);
    }

    #[test]
    fn classification_is_case_insensitive_and_normalizing() {
        let p = PathBuf::from("/users/ALICE/.duduclaw/agents/agnes/./x/../agent.toml");
        assert_eq!(
            classify_protected_toml(&p, &home()),
            Some(ProtectedTomlKind::AgentToml)
        );
    }

    // ── agent.toml org fields ──────────────────────────────────────

    #[test]
    fn reports_to_change_is_denied() {
        let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
        let d = check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new);
        match &d {
            GuardDecision::BlockedOrgFieldChange { changed, .. } => {
                assert_eq!(changed.len(), 1);
                assert!(changed[0].contains("reports_to"));
                assert!(changed[0].contains("victim"));
            }
            other => panic!("expected BlockedOrgFieldChange, got {other:?}"),
        }
        assert!(!d.is_allowed());
        let msg = d.block_message().unwrap();
        assert!(msg.contains("組織欄位"));
        assert!(msg.contains("agent_update"));
        assert!(msg.contains("儀表板"));
    }

    #[test]
    fn name_change_is_denied() {
        // `[agent] name` is the registry id the `delegation.set` whitelist
        // resolves against — an agent that could rename itself to a value the
        // operator believes belongs to someone else would inherit that pair.
        let new = BASE.replace(r#"name = "agnes""#, r#"name = "ceo-assistant""#);
        match check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new) {
            GuardDecision::BlockedOrgFieldChange { changed, .. } => {
                assert_eq!(changed.len(), 1);
                assert!(changed[0].contains("name"));
                assert!(changed[0].contains("ceo-assistant"));
            }
            other => panic!("expected BlockedOrgFieldChange, got {other:?}"),
        }
    }

    #[test]
    fn deleting_name_is_denied() {
        let new = BASE.replace("name = \"agnes\"\n", "");
        assert!(matches!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn department_change_is_denied() {
        let new = BASE.replace(r#"department = "engineering""#, r#"department = "finance""#);
        assert!(matches!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn deleting_reports_to_is_denied() {
        let new = BASE.replace("reports_to = \"ceo\"\n", "");
        assert!(matches!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn replacing_agent_table_with_scalar_is_denied() {
        // Cannot launder a change by destroying the table shape.
        let new = "agent = \"agnes\"\n";
        assert!(matches!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn both_fields_changed_lists_both() {
        let new = BASE
            .replace(r#"reports_to = "ceo""#, r#"reports_to = "x""#)
            .replace(r#"department = "engineering""#, r#"department = "y""#);
        match check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new) {
            GuardDecision::BlockedOrgFieldChange { changed, .. } => {
                assert_eq!(changed.len(), 2);
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn unrelated_field_change_is_allowed() {
        let new = BASE.replace(r#"preferred = "sonnet""#, r#"preferred = "opus""#);
        assert_eq!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), &new),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn reordering_and_reformatting_is_allowed() {
        // Field-wise comparison, not textual — a reformat must not trip it.
        let new = r#"
[model]
preferred = "sonnet"

[agent]
department = "engineering"
reports_to = "ceo"
name = "agnes"
display_name = "Agnes"
role = "assistant"
status = "active"
trigger = "@agnes"
icon = "🐾"
"#;
        assert_eq!(
            check_protected_toml_write(&agent_toml(), &home(), Some(BASE), new),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn new_file_is_allowed() {
        assert_eq!(
            check_protected_toml_write(&agent_toml(), &home(), None, BASE),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn invalid_new_toml_is_denied() {
        let d = check_protected_toml_write(&agent_toml(), &home(), Some(BASE), "[agent\nname =");
        match &d {
            GuardDecision::BlockedUnverifiable { reason, .. } => {
                assert!(reason.contains("TOML"));
            }
            other => panic!("expected BlockedUnverifiable, got {other:?}"),
        }
        assert!(!d.is_allowed());
    }

    #[test]
    fn invalid_new_toml_on_new_file_is_also_denied() {
        // Fail-closed: broken TOML breaks the agent regardless of prior state.
        assert!(matches!(
            check_protected_toml_write(&agent_toml(), &home(), None, "not = = toml"),
            GuardDecision::BlockedUnverifiable { .. }
        ));
    }

    #[test]
    fn invalid_existing_toml_is_denied() {
        let d = check_protected_toml_write(&agent_toml(), &home(), Some("[agent"), BASE);
        match &d {
            GuardDecision::BlockedUnverifiable { reason, .. } => {
                assert!(reason.contains("現有檔案"));
            }
            other => panic!("expected BlockedUnverifiable, got {other:?}"),
        }
    }

    #[test]
    fn non_protected_path_is_not_our_concern() {
        let p = PathBuf::from("/Users/alice/Project/x/Cargo.toml");
        assert_eq!(
            check_protected_toml_write(&p, &home(), Some(""), "x = 1"),
            GuardDecision::NotAgentFile
        );
    }

    // ── config.toml protected sections ─────────────────────────────

    const CONFIG: &str = r#"
[general]
log_level = "info"

[delegation]
policy = "department"
allow = [["a", "b"]]
"#;

    #[test]
    fn delegation_policy_change_is_denied() {
        let new = CONFIG.replace(r#"policy = "department""#, r#"policy = "open""#);
        let path = home().join("config.toml");
        let d = check_protected_toml_write(&path, &home(), Some(CONFIG), &new);
        match &d {
            GuardDecision::BlockedProtectedSection { changed, .. } => {
                assert_eq!(changed.len(), 1);
                assert!(changed[0].contains("[delegation]"));
            }
            other => panic!("expected BlockedProtectedSection, got {other:?}"),
        }
        let msg = d.block_message().unwrap();
        assert!(msg.contains("delegation"));
        assert!(msg.contains("儀表板"));
    }

    #[test]
    fn delegation_allow_whitelist_change_is_denied() {
        let new = CONFIG.replace(r#"allow = [["a", "b"]]"#, r#"allow = [["a", "ceo"]]"#);
        assert!(matches!(
            check_protected_toml_write(
                &home().join("config.toml"),
                &home(),
                Some(CONFIG),
                &new
            ),
            GuardDecision::BlockedProtectedSection { .. }
        ));
    }

    #[test]
    fn adding_acp_trusted_is_denied() {
        let new = format!("{CONFIG}\n[acp]\ntrusted = true\n");
        assert!(matches!(
            check_protected_toml_write(
                &home().join("config.toml"),
                &home(),
                Some(CONFIG),
                &new
            ),
            GuardDecision::BlockedProtectedSection { .. }
        ));
    }

    #[test]
    fn deleting_delegation_section_is_denied() {
        let new = "[general]\nlog_level = \"info\"\n";
        assert!(matches!(
            check_protected_toml_write(
                &home().join("config.toml"),
                &home(),
                Some(CONFIG),
                new
            ),
            GuardDecision::BlockedProtectedSection { .. }
        ));
    }

    #[test]
    fn unrelated_config_change_is_allowed() {
        let new = CONFIG.replace(r#"log_level = "info""#, r#"log_level = "debug""#);
        assert_eq!(
            check_protected_toml_write(
                &home().join("config.toml"),
                &home(),
                Some(CONFIG),
                &new
            ),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn project_config_toml_is_untouched() {
        let p = PathBuf::from("/Users/alice/Project/app/config.toml");
        assert_eq!(
            check_protected_toml_write(&p, &home(), Some(CONFIG), "[delegation]\npolicy = \"open\""),
            GuardDecision::NotAgentFile
        );
    }

    // ── bash speed bump ────────────────────────────────────────────

    #[test]
    fn bash_echo_redirect_into_agent_toml_is_blocked() {
        let cmd = "echo 'reports_to = \"ceo\"' > /Users/alice/.duduclaw/agents/agnes/agent.toml";
        match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
            GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
                assert_eq!(file_name, "agent.toml");
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn bash_sed_inplace_on_agent_toml_is_blocked() {
        let cmd = "sed -i '' 's/ceo/victim/' agents/agnes/agent.toml";
        assert!(matches!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::BlockedBashProtectedWrite { .. }
        ));
    }

    #[test]
    fn bash_write_to_home_config_is_blocked() {
        let cmd = "printf '[delegation]\\npolicy=\"open\"\\n' >> ~/.duduclaw/config.toml";
        match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
            GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
                assert_eq!(file_name, "config.toml");
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn bash_write_to_project_config_is_allowed() {
        let cmd = "echo 'x=1' > /Users/alice/Project/app/config.toml";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_read_only_agent_toml_is_allowed() {
        assert_eq!(
            check_bash_protected_write("grep reports_to agents/agnes/agent.toml", &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_unrelated_command_is_allowed() {
        assert_eq!(
            check_bash_protected_write("cargo test -p duduclaw-core", &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    // ── identity / enforcement surface ─────────────────────────────

    fn mcp_json(id: &str, token: Option<&str>) -> String {
        let env = match token {
            Some(t) => format!(
                "{{\"DUDUCLAW_AGENT_ID\":\"{id}\",\"DUDUCLAW_AGENT_TOKEN\":\"{t}\"}}"
            ),
            None => format!("{{\"DUDUCLAW_AGENT_ID\":\"{id}\"}}"),
        };
        format!("{{\"mcpServers\":{{\"duduclaw\":{{\"command\":\"/usr/local/bin/duduclaw\",\"args\":[\"mcp-server\"],\"env\":{env}}}}}}}")
    }

    #[test]
    fn classifies_the_three_identity_surfaces() {
        let h = home();
        assert_eq!(
            classify_identity_surface(&h.join("agents/agnes/.mcp.json"), &h),
            Some(ProtectedSurface::AgentMcpJson)
        );
        assert_eq!(
            classify_identity_surface(&h.join("agents/agnes/.claude/settings.json"), &h),
            Some(ProtectedSurface::HookSettings)
        );
        assert_eq!(
            classify_identity_surface(&h.join("agents/agnes/.claude/settings.local.json"), &h),
            Some(ProtectedSurface::HookSettings)
        );
        assert_eq!(
            classify_identity_surface(&h.join("identity.key"), &h),
            Some(ProtectedSurface::IdentityKey)
        );
        // Out of scope: a user project's own files, and a settings.json that
        // is not under an agent's `.claude/`.
        for p in [
            PathBuf::from("/Users/alice/Project/app/.mcp.json"),
            PathBuf::from("/Users/alice/Project/app/.claude/settings.json"),
            PathBuf::from("/Users/alice/Project/app/identity.key"),
            h.join("agents/agnes/settings.json"),
            h.join("agents/identity.key"),
        ] {
            assert_eq!(classify_identity_surface(&p, &h), None, "{}", p.display());
        }
    }

    #[test]
    fn stealing_a_peer_identity_into_own_mcp_json_is_denied() {
        // The attack the token alone does not stop: `sales-rep` reads the
        // CEO's `.mcp.json` (Read is not gated) and pastes the *valid* pair
        // into its own, so its next MCP server authenticates as `ceo`.
        let p = home().join("agents/sales-rep/.mcp.json");
        let existing = mcp_json("sales-rep", Some("aa11"));
        let stolen = mcp_json("ceo", Some("bb22"));
        let d = check_identity_surface_write(&p, &home(), Some(&existing), Some(&stolen));
        match &d {
            GuardDecision::BlockedIdentitySurface { reason, .. } => {
                assert!(reason.contains("DUDUCLAW_AGENT_ID"));
                // The token value is a credential — never echoed back.
                assert!(!reason.contains("bb22"), "token leaked into the message");
                assert!(!reason.contains("aa11"), "token leaked into the message");
            }
            other => panic!("expected BlockedIdentitySurface, got {other:?}"),
        }
        assert!(!d.is_allowed());
        assert!(d.block_message().unwrap().contains(".mcp.json"));
    }

    #[test]
    fn appending_a_second_duduclaw_server_with_another_id_is_denied() {
        let p = home().join("agents/sales-rep/.mcp.json");
        let existing = mcp_json("sales-rep", Some("aa11"));
        let added = existing.replace(
            "\"mcpServers\":{",
            "\"mcpServers\":{\"duduclaw2\":{\"command\":\"/usr/local/bin/duduclaw\",\"args\":[\"mcp-server\"],\"env\":{\"DUDUCLAW_AGENT_ID\":\"ceo\"}},",
        );
        assert!(matches!(
            check_identity_surface_write(&p, &home(), Some(&existing), Some(&added)),
            GuardDecision::BlockedIdentitySurface { .. }
        ));
    }

    #[test]
    fn adding_an_unrelated_mcp_server_is_allowed() {
        // Agents may still wire up Playwright etc. in their own file; only the
        // identity pairs are frozen.
        let p = home().join("agents/sales-rep/.mcp.json");
        let existing = mcp_json("sales-rep", Some("aa11"));
        let added = existing.replace(
            "\"mcpServers\":{",
            "\"mcpServers\":{\"playwright\":{\"command\":\"npx\",\"args\":[\"-y\",\"@playwright/mcp\"],\"env\":{\"FOO\":\"1\"}},",
        );
        assert_eq!(
            check_identity_surface_write(&p, &home(), Some(&existing), Some(&added)),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn dropping_the_identity_block_is_denied_and_reordering_is_not() {
        let p = home().join("agents/sales-rep/.mcp.json");
        let existing = mcp_json("sales-rep", Some("aa11"));
        // Removing the token downgrades a strict-mode caller to Rejected —
        // and removing the id falls through to `default_agent`.
        assert!(matches!(
            check_identity_surface_write(
                &p,
                &home(),
                Some(&existing),
                Some(&mcp_json("sales-rep", None))
            ),
            GuardDecision::BlockedIdentitySurface { .. }
        ));
        // Key order / formatting is not content.
        let reordered = "{\"mcpServers\":{\"duduclaw\":{\"env\":{\"DUDUCLAW_AGENT_TOKEN\":\"aa11\",\"DUDUCLAW_AGENT_ID\":\"sales-rep\"},\"args\":[\"mcp-server\"],\"command\":\"/usr/local/bin/duduclaw\"}}}";
        assert_eq!(
            check_identity_surface_write(&p, &home(), Some(&existing), Some(reordered)),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn creating_an_mcp_json_that_declares_an_identity_is_denied() {
        // No prior file ⇒ nothing was declared before, so declaring an
        // identity now is a change. `.mcp.json` is written by DuDuClaw, never
        // by an agent.
        let p = home().join("agents/sales-rep/.mcp.json");
        assert!(matches!(
            check_identity_surface_write(&p, &home(), None, Some(&mcp_json("ceo", Some("bb22")))),
            GuardDecision::BlockedIdentitySurface { .. }
        ));
        // …but a file that declares no identity at all is not our concern.
        assert_eq!(
            check_identity_surface_write(
                &p,
                &home(),
                None,
                Some("{\"mcpServers\":{\"playwright\":{\"command\":\"npx\"}}}")
            ),
            GuardDecision::AllowedAgentWrite
        );
    }

    #[test]
    fn unreconstructable_or_malformed_mcp_json_fails_closed() {
        let p = home().join("agents/sales-rep/.mcp.json");
        let existing = mcp_json("sales-rep", Some("aa11"));
        assert!(matches!(
            check_identity_surface_write(&p, &home(), Some(&existing), None),
            GuardDecision::BlockedUnverifiable { .. }
        ));
        assert!(matches!(
            check_identity_surface_write(&p, &home(), Some(&existing), Some("{not json")),
            GuardDecision::BlockedUnverifiable { .. }
        ));
        assert!(matches!(
            check_identity_surface_write(&p, &home(), Some("{not json"), Some(&existing)),
            GuardDecision::BlockedUnverifiable { .. }
        ));
    }

    #[test]
    fn hook_settings_and_identity_key_are_refused_outright() {
        // Disarming the PreToolUse hook would un-protect everything above it.
        let s = home().join("agents/agnes/.claude/settings.json");
        let d = check_identity_surface_write(&s, &home(), Some("{}"), Some("{\"hooks\":{}}"));
        assert!(matches!(d, GuardDecision::BlockedIdentitySurface { .. }));
        assert!(!d.is_allowed());
        // Even a no-op-looking write is refused: there is no legitimate
        // agent-authored shape for this file.
        assert!(matches!(
            check_identity_surface_write(&s, &home(), Some("{}"), Some("{}")),
            GuardDecision::BlockedIdentitySurface { .. }
        ));

        // A wrong-length identity.key silently downgrades strict mode to
        // `Disabled` — the cheapest possible disable of the whole feature.
        let k = home().join("identity.key");
        assert!(matches!(
            check_identity_surface_write(&k, &home(), None, Some("x")),
            GuardDecision::BlockedIdentitySurface { .. }
        ));
    }

    #[test]
    fn identity_surface_ignores_unrelated_paths() {
        assert_eq!(
            check_identity_surface_write(
                &PathBuf::from("/Users/alice/Project/app/.mcp.json"),
                &home(),
                None,
                Some("{}")
            ),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_write_to_identity_surfaces_is_blocked() {
        for cmd in [
            "echo x > /Users/alice/.duduclaw/identity.key",
            "printf '{}' > /Users/alice/.duduclaw/agents/agnes/.claude/settings.json",
            "cp stolen.json /Users/alice/.duduclaw/agents/agnes/.mcp.json",
        ] {
            assert!(
                matches!(
                    check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
                    GuardDecision::BlockedBashProtectedWrite { .. }
                ),
                "not blocked: {cmd}"
            );
        }
        // Read-only inspection stays allowed (same philosophy as the rest).
        assert_eq!(
            check_bash_protected_write("ls -l ~/.duduclaw/agents/agnes/.mcp.json", &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    // ── WP22 T2: caller-scoped directory isolation ─────────────────

    fn agent(id: &str) -> HookCaller {
        HookCaller::Agent(id.to_string())
    }

    #[test]
    fn writing_another_agents_files_is_denied() {
        // The gap WP21 left open: none of these are org fields or identity
        // surfaces, so the content guards had nothing to say about them.
        for rel in [
            "agents/ceo/SOUL.md",
            "agents/ceo/MEMORY.md",
            "agents/ceo/CLAUDE.md",
            "agents/ceo/notes/scratch.txt",
            "agents/ceo/.claude/hooks/whatever.sh",
        ] {
            let p = home().join(rel);
            match check_caller_scope(&p, &home(), &agent("sales-rep")) {
                GuardDecision::BlockedForeignAgentDir { owner, caller, .. } => {
                    assert_eq!(owner, "ceo", "{rel}");
                    assert_eq!(caller, "sales-rep", "{rel}");
                }
                other => panic!("expected block for {rel}, got {other:?}"),
            }
        }
        // The victim's id is in the message so the model learns what it hit.
        let d = check_caller_scope(&home().join("agents/ceo/SOUL.md"), &home(), &agent("sales-rep"));
        assert!(!d.is_allowed());
        let msg = d.block_message().unwrap();
        assert!(msg.contains("ceo"), "message must name the other agent: {msg}");
        assert!(msg.contains("其他 AI 員工"));
    }

    #[test]
    fn writing_own_files_falls_through_to_the_content_guards() {
        // `NotAgentFile` here means "this rule has no opinion" — the caller
        // then applies the WP21 org-field / identity-surface guards, which is
        // what keeps `reports_to` frozen even in the agent's own directory.
        for rel in [
            "agents/sales-rep/SOUL.md",
            "agents/sales-rep/agent.toml",
            "agents/sales-rep/wiki/sop.md",
        ] {
            assert_eq!(
                check_caller_scope(&home().join(rel), &home(), &agent("sales-rep")),
                GuardDecision::NotAgentFile,
                "{rel}"
            );
        }
    }

    #[test]
    fn own_org_field_change_is_still_denied_by_the_content_guard() {
        // Regression pin for the two stages together: directory scope allows
        // it, content scope still refuses.
        let p = agent_toml();
        assert_eq!(
            check_caller_scope(&p, &home(), &agent("agnes")),
            GuardDecision::NotAgentFile
        );
        let new = BASE.replace(r#"reports_to = "ceo""#, r#"reports_to = "victim""#);
        assert!(matches!(
            check_protected_toml_write(&p, &home(), Some(BASE), &new),
            GuardDecision::BlockedOrgFieldChange { .. }
        ));
    }

    #[test]
    fn ephemeral_dir_is_owned_by_its_own_id() {
        let mine = home().join("agents/.ephemeral/eph-abc123/SOUL.md");
        assert_eq!(
            check_caller_scope(&mine, &home(), &agent("eph-abc123")),
            GuardDecision::NotAgentFile
        );
        // A regular agent may not reach into an ephemeral scaffold…
        match check_caller_scope(&mine, &home(), &agent("sales-rep")) {
            GuardDecision::BlockedForeignAgentDir { owner, .. } => {
                assert_eq!(owner, "eph-abc123");
            }
            other => panic!("expected block, got {other:?}"),
        }
        // …nor may one ephemeral agent reach into another's.
        assert!(matches!(
            check_caller_scope(&mine, &home(), &agent("eph-zzz999")),
            GuardDecision::BlockedForeignAgentDir { .. }
        ));
    }

    #[test]
    fn caller_scope_ignores_paths_outside_any_agent_dir() {
        for p in [
            PathBuf::from("/Users/alice/Project/app/src/main.rs"),
            home().join("shared/wiki/policies/x.md"),
            // The agents root itself has no owner component.
            home().join("agents/README.md"),
            // The ephemeral root likewise.
            home().join("agents/.ephemeral/notes.txt"),
        ] {
            assert_eq!(
                check_caller_scope(&p, &home(), &agent("sales-rep")),
                GuardDecision::NotAgentFile,
                "{}",
                p.display()
            );
        }
    }

    #[test]
    fn no_caller_identity_changes_nothing() {
        // The hook is only registered inside agent directories, so an
        // invocation without an identity is an operator working by hand.
        for rel in ["agents/ceo/SOUL.md", "config.toml", "agents/ceo/agent.toml"] {
            assert_eq!(
                check_caller_scope(&home().join(rel), &home(), &HookCaller::Absent),
                GuardDecision::NotAgentFile,
                "{rel}"
            );
        }
    }

    #[test]
    fn strict_mode_rejection_denies_every_agent_dir_write() {
        let untrusted = HookCaller::Untrusted("sales-rep".to_string());
        // Even its "own" directory: a rejected claim proves nothing about
        // whose directory this is.
        for rel in [
            "agents/sales-rep/SOUL.md",
            "agents/ceo/SOUL.md",
            "agents/.ephemeral/eph-abc123/agent.toml",
        ] {
            let d = check_caller_scope(&home().join(rel), &home(), &untrusted);
            assert!(
                matches!(d, GuardDecision::BlockedUntrustedCaller { .. }),
                "{rel}: {d:?}"
            );
            assert!(!d.is_allowed());
        }
        let d = check_caller_scope(&home().join("agents/ceo/SOUL.md"), &home(), &untrusted);
        assert!(d.block_message().unwrap().contains("require_identity_token"));
        // …and the home config too.
        assert!(matches!(
            check_caller_scope(&home().join("config.toml"), &home(), &untrusted),
            GuardDecision::BlockedHomeConfigWrite { .. }
        ));
    }

    #[test]
    fn caller_scope_is_case_insensitive_and_normalizing() {
        // Same filesystem-case tolerance as the rest of the guards, and `..`
        // must not launder a foreign path into an own-looking one.
        assert_eq!(
            check_caller_scope(
                &PathBuf::from("/users/ALICE/.duduclaw/agents/Sales-Rep/./SOUL.md"),
                &home(),
                &agent("sales-rep")
            ),
            GuardDecision::NotAgentFile
        );
        assert!(matches!(
            check_caller_scope(
                &home().join("agents/sales-rep/../ceo/SOUL.md"),
                &home(),
                &agent("sales-rep")
            ),
            GuardDecision::BlockedForeignAgentDir { .. }
        ));
    }

    #[test]
    fn home_config_toml_is_wholly_locked_for_agents() {
        // WP22 supersedes WP21's section-level comparison for agent callers:
        // even a `[general] log_level` tweak is refused.
        let p = home().join("config.toml");
        let d = check_caller_scope(&p, &home(), &agent("sales-rep"));
        match &d {
            GuardDecision::BlockedHomeConfigWrite { caller, .. } => {
                assert_eq!(caller, "sales-rep");
            }
            other => panic!("expected BlockedHomeConfigWrite, got {other:?}"),
        }
        assert!(!d.is_allowed());
        assert!(d.block_message().unwrap().contains("config.toml"));
        // A user project's own config.toml is untouched.
        assert_eq!(
            check_caller_scope(
                &PathBuf::from("/Users/alice/Project/app/config.toml"),
                &home(),
                &agent("sales-rep")
            ),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn home_config_without_identity_keeps_the_wp21_section_behaviour() {
        // Operator (no identity): the whole-file lock does not apply, and the
        // WP21 content guard still allows an unrelated section change while
        // refusing `[delegation]`.
        let p = home().join("config.toml");
        assert_eq!(
            check_caller_scope(&p, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
        let benign = CONFIG.replace(r#"log_level = "info""#, r#"log_level = "debug""#);
        assert_eq!(
            check_protected_toml_write(&p, &home(), Some(CONFIG), &benign),
            GuardDecision::AllowedAgentWrite
        );
        let hostile = CONFIG.replace(r#"policy = "department""#, r#"policy = "open""#);
        assert!(matches!(
            check_protected_toml_write(&p, &home(), Some(CONFIG), &hostile),
            GuardDecision::BlockedProtectedSection { .. }
        ));
    }

    #[test]
    fn bash_speed_bump_already_covers_the_whole_home_config() {
        // T2 requires the Bash lane to be whole-file too; the WP21 heuristic
        // matches on the *path*, not the section, so it already is. Pinned so
        // a future narrowing to `[delegation]` text would fail here.
        for cmd in [
            "echo 'log_level = \"debug\"' > /Users/alice/.duduclaw/config.toml",
            "sed -i '' 's/info/debug/' ~/.duduclaw/config.toml",
        ] {
            assert!(
                matches!(
                    check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
                    GuardDecision::BlockedBashProtectedWrite { .. }
                ),
                "not blocked: {cmd}"
            );
        }
    }

    #[test]
    fn bash_backslash_path_is_normalized() {
        let cmd = r"copy x C:\Users\alice\.duduclaw\agents\agnes\agent.toml";
        // `cp ` is not present, but `copy` contains no verb — use mv instead.
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
        let cmd2 = r"mv x C:\Users\alice\.duduclaw\agents\agnes\agent.toml";
        assert!(matches!(
            check_bash_protected_write(cmd2, &home(), &HookCaller::Absent),
            GuardDecision::BlockedBashProtectedWrite { .. }
        ));
    }

    // ── WP22 T2 follow-up: Bash cross-agent-directory speed bump ───

    #[test]
    fn bash_cross_agent_dir_write_is_blocked_for_identified_caller() {
        // Neither `SOUL.md` nor any other basename here is DuDuClaw-specific
        // enough for the checks above to catch — this is exactly the gap
        // `check_caller_scope` closes for Write/Edit, now closed for Bash too.
        let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
        match check_bash_protected_write(cmd, &home(), &agent("sales-rep")) {
            GuardDecision::BlockedForeignAgentDir { caller, owner, .. } => {
                assert_eq!(caller, "sales-rep");
                assert_eq!(owner, "ceo");
            }
            other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
        }
    }

    #[test]
    fn bash_write_to_own_agent_dir_is_not_blocked_by_the_cross_agent_rule() {
        let cmd = "echo 'note' > /Users/alice/.duduclaw/agents/sales-rep/notes.txt";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_cross_agent_dir_mention_without_write_verb_is_allowed() {
        // Read-only inspection of a peer's directory is none of this rule's
        // business — same philosophy as the rest of this speed bump.
        let cmd = "cat /Users/alice/.duduclaw/agents/ceo/SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_cross_agent_dir_write_without_caller_identity_is_not_blocked() {
        // No `DUDUCLAW_AGENT_ID` ⇒ operator running by hand, same as
        // `check_caller_scope`'s `HookCaller::Absent` handling — this rule is
        // deliberately a no-op, though the DuDuClaw-specific-basename checks
        // above it (agent.toml / config.toml / …) still apply regardless of
        // caller.
        let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_cross_agent_dir_rule_ignores_untrusted_claims() {
        // An `Untrusted` claim is not `HookCaller::Agent`, so this
        // particular rule stays silent on it — the untrusted caller is
        // already refused wholesale on the Write/Edit lane, and Bash
        // enforcement for that caller state is out of this rule's scope
        // (documented above `check_bash_protected_write`).
        let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
        let untrusted = HookCaller::Untrusted("sales-rep".to_string());
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &untrusted),
            GuardDecision::NotAgentFile
        );
    }

    // ── WP22 T5: the org store on the Bash lane ────────────────────

    #[test]
    fn bash_write_or_delete_of_the_org_store_is_blocked() {
        // The whole point of T1 is that `org.toml` is the authority. Deleting
        // it degrades every delegation decision back to the `agent.toml`
        // mirrors, so `rm` matters as much as `>` here — and neither was
        // covered before: this file was on the Write/Edit lane only.
        for cmd in [
            "rm /Users/alice/.duduclaw/org.toml",
            "rm -f ~/.duduclaw/org.toml",
            "echo 'schema = 1' > /Users/alice/.duduclaw/org.toml",
            "rm ~/.duduclaw/.org-seeded",
            "mv /tmp/fake.toml /Users/alice/.duduclaw/org.toml",
        ] {
            match check_bash_protected_write(cmd, &home(), &HookCaller::Absent) {
                GuardDecision::BlockedBashProtectedWrite { file_name, .. } => {
                    assert_eq!(file_name, "org.toml", "{cmd}");
                }
                other => panic!("not blocked: {cmd} → {other:?}"),
            }
        }
        // Read-only inspection stays allowed, and a project's own `org.toml`
        // is none of this rule's business.
        assert_eq!(
            check_bash_protected_write("cat ~/.duduclaw/org.toml", &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
        assert_eq!(
            check_bash_protected_write(
                "rm /Users/alice/Project/app/org.toml",
                &home(),
                &HookCaller::Absent
            ),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_mentions_other_agent_dir_does_not_false_positive_on_similar_words() {
        // `myagents/` must not match the `agents/` boundary check.
        let cmd = "echo x > /Users/alice/myagents/ceo/SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    // ── WP1.1 C3: SOUL.md self-write guard ──────────────────────────

    #[test]
    fn own_soul_write_is_blocked() {
        let p = home().join("agents/sales-rep/SOUL.md");
        match check_own_soul_write(&p, &home(), &agent("sales-rep")) {
            GuardDecision::BlockedOwnSoulWrite { caller, .. } => {
                assert_eq!(caller, "sales-rep");
            }
            other => panic!("expected BlockedOwnSoulWrite, got {other:?}"),
        }
        let d = check_own_soul_write(&p, &home(), &agent("sales-rep"));
        assert!(!d.is_allowed());
        let msg = d.block_message().unwrap();
        assert!(msg.contains("SOUL.md"));
        assert!(msg.contains("can_modify_own_soul"));
    }

    #[test]
    fn own_soul_write_is_case_insensitive_and_normalizing() {
        let p = PathBuf::from("/users/ALICE/.duduclaw/agents/Sales-Rep/./SOUL.md");
        assert!(matches!(
            check_own_soul_write(&p, &home(), &agent("sales-rep")),
            GuardDecision::BlockedOwnSoulWrite { .. }
        ));
    }

    #[test]
    fn foreign_soul_write_is_not_this_rules_business() {
        // `check_caller_scope` (Stage 0) already blocks this as
        // `BlockedForeignAgentDir` before Stage 0.5 would even run; this
        // function alone has nothing to say about a directory it does not
        // own.
        let p = home().join("agents/ceo/SOUL.md");
        assert_eq!(
            check_own_soul_write(&p, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn own_soul_write_ignores_non_soul_files_and_absent_caller() {
        let soul = home().join("agents/sales-rep/SOUL.md");
        assert_eq!(
            check_own_soul_write(&soul, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile,
            "operator convention — unrestricted"
        );
        let other = home().join("agents/sales-rep/MEMORY.md");
        assert_eq!(
            check_own_soul_write(&other, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "only SOUL.md is in scope for this rule"
        );
        let outside = PathBuf::from("/Users/alice/Project/app/SOUL.md");
        assert_eq!(
            check_own_soul_write(&outside, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile,
            "a project file that happens to be named SOUL.md is none of this rule's business"
        );
    }

    #[test]
    fn own_soul_write_not_gated_by_can_modify_own_soul_flag() {
        // C4: the flag is the C2 (MCP) gate's only escape hatch. The
        // filesystem lane has no notion of the flag at all — it always
        // blocks the raw Write/Edit tool regardless of agent.toml content,
        // matching `check_own_soul_write`'s signature (no flag parameter).
        let p = home().join("agents/sales-rep/SOUL.md");
        assert!(matches!(
            check_own_soul_write(&p, &home(), &agent("sales-rep")),
            GuardDecision::BlockedOwnSoulWrite { .. }
        ));
    }

    #[test]
    fn bash_own_soul_write_is_blocked_for_explicit_and_relative_paths() {
        for cmd in [
            "echo hacked > /Users/alice/.duduclaw/agents/sales-rep/SOUL.md",
            "echo hacked > SOUL.md",
            "cat notes.txt >> soul.md",
            "sed -i '' 's/x/y/' SOUL.md",
        ] {
            assert!(
                matches!(
                    check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
                    GuardDecision::BlockedOwnSoulWrite { .. }
                ),
                "not blocked: {cmd}"
            );
        }
    }

    #[test]
    fn bash_own_soul_write_read_only_is_allowed() {
        let cmd = "cat SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_own_soul_write_no_caller_identity_is_not_blocked() {
        let cmd = "echo hacked > SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &HookCaller::Absent),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_own_soul_write_does_not_false_positive_on_lookalike_directory() {
        // Same boundary caveat as `mentions_other_agent_dir`: `myagents/`
        // must not be treated as a real `agents/` path, own or foreign.
        let cmd = "echo x > /Users/alice/myagents/sales-rep/SOUL.md";
        assert_eq!(
            check_bash_protected_write(cmd, &home(), &agent("sales-rep")),
            GuardDecision::NotAgentFile
        );
    }

    #[test]
    fn bash_foreign_agent_dir_rule_still_wins_over_soul_md_check() {
        // Regression pin: the pre-existing cross-agent rule must still fire
        // first when a write targets ANOTHER agent's SOUL.md — this rule
        // must never downgrade that into a same-caller "own soul" story.
        let cmd = "cat header.txt > /Users/alice/.duduclaw/agents/ceo/SOUL.md";
        match check_bash_protected_write(cmd, &home(), &agent("sales-rep")) {
            GuardDecision::BlockedForeignAgentDir { owner, .. } => assert_eq!(owner, "ceo"),
            other => panic!("expected BlockedForeignAgentDir, got {other:?}"),
        }
    }
}
