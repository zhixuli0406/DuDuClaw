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

//! # Layout (audit O9, 2026-09-29)
//!
//! This used to be one 2,374-line file. It is now three:
//!
//! - [`rules`] — **the data**: `FROZEN_FIELDS` (which field/section of which
//!   file is frozen, how it is compared, which verdict it produces), the file
//!   kinds, and the basename / write-verb lists.
//! - [`matcher`] — **the comparators**: one generic `diff_frozen` over the
//!   three shapes, plus the shared path / text primitives.
//! - this file — **the entry points** the hook and the MCP front doors call.
//!
//! Freezing a new field is a row in `FROZEN_FIELDS`, not a new `diff_*`
//! function plus a new branch here. The public surface, every decision, and
//! every message string are unchanged by that split.

mod bash_cmd;
mod bash_lane;
mod bash_parse;
mod matcher;
mod real_path;

pub use real_path::resolve_real_path;
use real_path::with_real_path;
mod rules;

pub use rules::{
    AGENT_CAPABILITY_SECTION, AGENT_EDITABLE_SECTIONS, AGENT_ORG_FIELDS, AGENT_SECURITY_KEYS,
    AGENT_SECURITY_SECTIONS,
    CONFIG_PROTECTED_SECTIONS, HOME_WRITABLE_DIRS, ProtectedSurface, ProtectedTomlKind,
};

use rules::{EPHEMERAL_DIR_NAME, FrozenVerdict, HOOK_SETTINGS_FILES, IDENTITY_ENV_KEYS};
use matcher::{components_after_ci, describe_pairs, first_line, identity_env_pairs, owning_agent_dir};

use std::path::Path;

use crate::agent_guard::{lexical_normalize, GuardDecision};

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
///
/// The comparison itself is driven entirely by [`rules::FROZEN_FIELDS`]: each
/// entry for this file kind is diffed by [`matcher::diff_frozen`], entries
/// sharing a verdict are reported together, and the FIRST verdict group with
/// any change wins (so an `[agent]` org-field move is still reported ahead of
/// a `[capabilities]` move when one write does both).
pub fn check_protected_toml_write(
    file_path: &Path,
    home: &Path,
    existing: Option<&str>,
    new_content: &str,
) -> GuardDecision {
    protected_toml_write(file_path, home, false, existing, new_content)
}

/// [`check_protected_toml_write`] plus, for an agent-identified or untrusted
/// `caller`, the G1 rows ([`AGENT_SECURITY_SECTIONS`] /
/// [`AGENT_SECURITY_KEYS`]) of the caller's `agent.toml`. For
/// [`HookCaller::Absent`] (an operator working by hand) the result is
/// exactly [`check_protected_toml_write`]'s. This is what the hook calls.
pub fn check_protected_toml_write_as(
    file_path: &Path,
    home: &Path,
    caller: &HookCaller,
    existing: Option<&str>,
    new_content: &str,
) -> GuardDecision {
    let agent_caller = !matches!(caller, HookCaller::Absent);
    protected_toml_write(file_path, home, agent_caller, existing, new_content)
}

fn protected_toml_write(
    file_path: &Path,
    home: &Path,
    agent_caller: bool,
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

    // Accumulate per verdict group, preserving table order.
    let mut groups: Vec<(FrozenVerdict, Vec<String>)> = Vec::new();
    for entry in rules::frozen_for_caller(kind, agent_caller) {
        let changed = matcher::diff_frozen(&entry.shape, &old_table, &new_table);
        match groups.last_mut() {
            Some((verdict, acc)) if *verdict == entry.verdict => acc.extend(changed),
            _ => groups.push((entry.verdict, changed)),
        }
    }

    for (verdict, changed) in groups {
        if changed.is_empty() {
            continue;
        }
        let file_name = kind.file_name().to_string();
        return match verdict {
            FrozenVerdict::OrgField => GuardDecision::BlockedOrgFieldChange {
                file_name,
                attempted_path,
                changed,
            },
            FrozenVerdict::ProtectedField => GuardDecision::BlockedProtectedField {
                file_name,
                attempted_path,
                changed,
            },
            FrozenVerdict::ProtectedSection => GuardDecision::BlockedProtectedSection {
                file_name,
                attempted_path,
                changed,
            },
            FrozenVerdict::AgentSecuritySection => GuardDecision::BlockedAgentSecuritySection {
                file_name,
                attempted_path,
                changed,
            },
        };
    }
    GuardDecision::AllowedAgentWrite
}

// ── Identity / enforcement surface ───────────────────────────────────────────

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
/// Rules, all fail-closed and all no-ops for [`HookCaller::Absent`]:
///
/// 1. `<home>/agents/<other>/**` where `<other>` is not the caller → DENY.
///    `<home>/agents/.ephemeral/<eph-id>/**` is owned by `<eph-id>`, so an
///    ephemeral agent may write its own scaffold and nobody else's. An
///    [`HookCaller::Untrusted`] caller is refused everywhere under
///    `<home>/agents/`, its "own" directory included.
/// 2. `<home>/config.toml` → DENY outright (WP22 supersedes WP21's
///    section-level comparison for agent callers; every legitimate writer —
///    dashboard RPC, MCP tools, the gateway itself — goes through Rust and
///    never through this hook).
/// 3. G1 (2026-10): anything else under `<home>` → DENY, except the shared
///    [`HOME_WRITABLE_DIRS`] (`attachments/`). An allow-list rather than a
///    list of protected files, because `<home>` holds dozens of stores the
///    platform treats as evidence or authority — `tool_calls.jsonl`,
///    `evals/` (held-out sets included), `tasks.db`, `approvals.db`,
///    breaker state, licences, `skills/`, `shared/wiki/` — and a new one must
///    not start out writable. Their legitimate writers are the gateway and
///    the gated MCP tools, neither of which runs through this hook.
///
/// Returns [`GuardDecision::NotAgentFile`] when the path is outside `<home>`
/// or is a permitted place, so the caller falls through to the WP21 guards.
///
/// # Symbolic links (G1 round 2)
///
/// Every rule is applied twice: to the literal path against the literal
/// `<home>`, and to the real path ([`resolve_real_path`]) against the real
/// `<home>`; a block from either wins. A link inside the caller's own
/// directory that points at `<home>` state, a `..` after such a link, a
/// dangling link (writing through it creates its target) and a `<home>`
/// reached through a link are all judged on where the write really lands.
/// A path that cannot be resolved for any reason other than "the tail does
/// not exist yet" is refused.
pub fn check_caller_scope(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    if matches!(caller, HookCaller::Absent) {
        return GuardDecision::NotAgentFile;
    }
    with_real_path(file_path, home, |p, h| caller_scope_at(p, h, caller))
}

/// [`check_caller_scope`]'s rules on one (path, home) pair.
fn caller_scope_at(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    let normalized = lexical_normalize(file_path);

    // Rule 2 — the home config.toml, whole file.
    let home_config = lexical_normalize(&home.join("config.toml"));
    if matches!(components_after_ci(&normalized, &home_config), Some(rest) if rest.is_empty()) {
        return GuardDecision::BlockedHomeConfigWrite {
            caller: caller.claimed().to_string(),
            attempted_path: normalized,
        };
    }

    let Some(rest) = components_after_ci(&normalized, &lexical_normalize(home)) else {
        // Outside `<home>`: a user project, `/tmp`, … — none of this rule's
        // business.
        return GuardDecision::NotAgentFile;
    };
    let home_state = || GuardDecision::BlockedHomeStateWrite {
        caller: caller.claimed().to_string(),
        attempted_path: normalized.clone(),
    };

    let under_agents = rest.first().is_some_and(|c| c.eq_ignore_ascii_case("agents"));
    if !under_agents {
        // Rule 3 — `<home>` state, except the shared writable directories
        // (their contents, not the directory entry itself).
        let writable = rest.len() >= 2
            && HOME_WRITABLE_DIRS
                .iter()
                .any(|d| rest[0].eq_ignore_ascii_case(d));
        return if writable {
            GuardDecision::NotAgentFile
        } else {
            home_state()
        };
    }

    // Rule 1 — someone else's agent directory.
    let Some(owner) = owning_agent_dir(&normalized, home) else {
        // `<home>/agents` itself, a stray file at the agents root, or the
        // `.ephemeral` root: no owner, so nobody's own directory.
        return match caller {
            HookCaller::Untrusted(claimed) => GuardDecision::BlockedUntrustedCaller {
                caller: claimed.clone(),
                attempted_path: normalized,
            },
            _ => home_state(),
        };
    };
    match caller {
        HookCaller::Absent => unreachable!("handled above"),
        // Removed-name reservation: `_trash` is not an agent directory but the
        // place removed employees are kept; reported as such rather than as
        // "another employee's files".
        HookCaller::Agent(id) if owner.eq_ignore_ascii_case(crate::agent_trash::AGENT_TRASH_DIR) => {
            GuardDecision::BlockedRemovedAgentArea {
                caller: id.clone(),
                attempted_path: normalized,
            }
        }
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
    // G1 round 2: a link named anything (`persona.md`) pointing at the own
    // SOUL.md is judged on its target too.
    with_real_path(file_path, home, |p, h| own_soul_at(p, h, caller_id))
}

fn own_soul_at(file_path: &Path, home: &Path, caller_id: &String) -> GuardDecision {
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

/// Contract lock — block an agent-identified caller from writing its OWN
/// `CONTRACT.toml` via Write/Edit/MultiEdit.
///
/// Same shape as [`check_own_soul_write`] and the same gap it closes:
/// [`check_caller_scope`] only refuses *another* agent's directory, and the
/// location guard allows `CONTRACT.toml` at its canonical path, so an agent
/// holding Write could delete its own `must_not` boundaries. The contract is
/// the operator's boundary on the agent, so there is **no** opt-in flag;
/// operators change it through the dashboard (`contract.update`, admin only),
/// which never passes through this hook. `HookCaller::Absent` (operator running
/// by hand) is a no-op; `HookCaller::Untrusted` is refused earlier by
/// [`check_caller_scope`] for every path under `<home>/agents/`.
pub fn check_own_contract_write(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    let HookCaller::Agent(caller_id) = caller else {
        return GuardDecision::NotAgentFile;
    };
    with_real_path(file_path, home, |p, h| own_contract_at(p, h, caller_id))
}

fn own_contract_at(file_path: &Path, home: &Path, caller_id: &String) -> GuardDecision {
    match file_path.file_name().and_then(|n| n.to_str()) {
        Some(n) if n.eq_ignore_ascii_case("CONTRACT.toml") => {}
        _ => return GuardDecision::NotAgentFile,
    }
    let normalized = lexical_normalize(file_path);
    let Some(owner) = owning_agent_dir(&normalized, home) else {
        return GuardDecision::NotAgentFile;
    };
    if owner.eq_ignore_ascii_case(caller_id) {
        GuardDecision::BlockedOwnContractWrite {
            caller: caller_id.clone(),
            attempted_path: normalized,
        }
    } else {
        // A foreign agent's CONTRACT.toml is already blocked by
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
/// philosophy, same false-positive tolerance. `HookCaller::Absent` is a
/// no-op here: an absent identity is an operator running by hand
/// (unrestricted, matching `check_caller_scope`).
///
/// # G1 (2026-10) — untrusted callers and `<home>` state
///
/// - An [`HookCaller::Untrusted`] caller used to pass this lane for any path
///   under `agents/`, while the Write/Edit lane refused the same path. It is
///   now refused the same way here: a write-shaped command that names any
///   `agents/<id>/` path, the removed-employee area, a cwd-relative peer
///   directory, or the claimed id's own `SOUL.md` / `CONTRACT.toml`.
/// - For agent and untrusted callers, a write-shaped command that names a
///   place under `<home>` other than the caller's own directory and
///   `attachments/` is refused ([`GuardDecision::BlockedHomeStateWrite`]);
///   see [`matcher::bash_home_targets`] for which spellings are recognised.
///   This runs after the established basename checks, so `config.toml`,
///   `org.toml`, `.mcp.json`, … keep their older messages.
pub fn check_bash_protected_write(command: &str, home: &Path, caller: &HookCaller) -> GuardDecision {
    check_bash_protected_write_in(command, home, caller, None)
}

/// [`check_bash_protected_write`] with the shell's `cwd` from the hook
/// envelope; `None` falls back to `<home>/agents/<caller>` (the gateway's
/// spawn directory).
pub fn check_bash_protected_write_in(
    command: &str,
    home: &Path,
    caller: &HookCaller,
    cwd: Option<&Path>,
) -> GuardDecision {
    // G1 round 3: shell escapes are undone before any rule reads the command
    // (`r\m`, `S\OUL.md`, a `\` line continuation). The text rules run over
    // two spellings — escapes undone, and the round-1 `\` → `/` form that
    // Windows paths need — and the positional rules tokenise the raw command
    // themselves. `2>/dev/null`, `2>&1`, `>&N` are removed first everywhere.
    let lower = |s: &str| s.to_ascii_lowercase();
    let unescaped = bash_parse::strip_fd_redirects(&lower(&bash_parse::shell_unescape(command)));
    let slashed: String = command
        .chars()
        .map(|c| if c == '\\' { '/' } else { c.to_ascii_lowercase() })
        .collect();
    let slashed = bash_parse::strip_fd_redirects(&slashed);
    for normalized in [unescaped.as_str(), slashed.as_str()] {
        let d = bash_lane::bash_name_rules(normalized, home, caller);
        if !d.is_allowed() {
            return d;
        }
    }
    for normalized in [unescaped.as_str(), slashed.as_str()] {
        let d = bash_lane::bash_protected_basenames(normalized, home);
        if !d.is_allowed() {
            return d;
        }
    }
    let positional = bash_parse::strip_fd_redirects(command);
    bash_lane::bash_home_state_write(&positional, home, caller, cwd)
        .unwrap_or(GuardDecision::NotAgentFile)
}

#[cfg(test)]
mod tests;
