//! The two halves of [`super::check_bash_protected_write`] that do not
//! depend on who the caller is (the established basename speed bump) or that
//! were added for G1 (2026-10, `<home>` state on the Bash lane). Split out so
//! `mod.rs` stays under the project's 800-line ceiling; the entry point and
//! its caller-specific checks stay in [`super`].

use std::path::Path;

use crate::agent_guard::{lexical_normalize, GuardDecision};

use super::bash_parse::{refused_targets, Ctx};
use super::matcher::{write_verb, HomeTarget};
use super::rules::HOOK_SETTINGS_FILES;
use super::matcher::{
    agents_dir_segments, mentions_other_agent_dir, mentions_own_contract_toml, mentions_own_soul_md,
    mentions_removed_agent_area,
};
use super::HookCaller;

/// G1 — the `<home>`-state half of [`check_bash_protected_write`] for agent
/// and untrusted callers; `None` when nothing is refused. The positional
/// rules are in [`super::bash_parse`].
pub(super) fn bash_home_state_write(
    positional: &str,
    home: &Path,
    caller: &HookCaller,
    cwd: Option<&Path>,
) -> Option<GuardDecision> {
    let claimed = match caller {
        HookCaller::Absent => return None,
        HookCaller::Agent(id) | HookCaller::Untrusted(id) => id,
    };
    let untrusted = matches!(caller, HookCaller::Untrusted(_));
    let home = lexical_normalize(home);
    let mut ctx = Ctx {
        home: &home,
        home_real: super::resolve_real_path(&home).ok(),
        caller: claimed.to_ascii_lowercase(),
        untrusted,
        strict_relative: false,
    };
    let start = match cwd {
        Some(c) if c.is_absolute() => Some(c.to_path_buf()),
        Some(_) => None,
        None => Some(home.join("agents").join(claimed)),
    };
    let target = refused_targets(positional, &mut ctx, start)
        .into_iter()
        .next()?;
    Some(match target {
        HomeTarget::OwnAgentDir | HomeTarget::ForeignAgentDir(_) | HomeTarget::RemovedArea
            if untrusted =>
        {
            GuardDecision::BlockedUntrustedCaller {
                caller: claimed.clone(),
                attempted_path: home.join("agents"),
            }
        }
        HomeTarget::ForeignAgentDir(owner) => GuardDecision::BlockedForeignAgentDir {
            caller: claimed.clone(),
            attempted_path: home.join("agents").join(&owner),
            owner,
        },
        HomeTarget::RemovedArea => GuardDecision::BlockedRemovedAgentArea {
            caller: claimed.clone(),
            attempted_path: home.join("agents").join(crate::agent_trash::AGENT_TRASH_DIR),
        },
        HomeTarget::State(rest) => GuardDecision::BlockedHomeStateWrite {
            caller: claimed.clone(),
            attempted_path: rest.iter().fold(home.clone(), |p, c| p.join(c)),
        },
        // Only refused for an untrusted caller, handled above.
        HomeTarget::OwnAgentDir => return None,
    })
}

/// The pre-G1 caller-independent half of [`check_bash_protected_write`]:
/// a write-shaped command naming one of the delegation-authority or
/// identity-surface files.
pub(super) fn bash_protected_basenames(normalized: &str, home: &Path) -> GuardDecision {
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

/// The caller-specific text rules of [`super::check_bash_protected_write`]
/// (removed-employee area, another agent's directory, own `SOUL.md` /
/// `CONTRACT.toml`, and the untrusted caller's refusal under `agents/`), run
/// over one normalised spelling of the command.
pub(super) fn bash_name_rules(normalized: &str, home: &Path, caller: &HookCaller) -> GuardDecision {
    if let HookCaller::Untrusted(claimed) = caller {
        let names_agents_area = mentions_removed_agent_area(normalized)
            || !agents_dir_segments(normalized).is_empty()
            || mentions_own_soul_md(normalized, claimed)
            || mentions_own_contract_toml(normalized, claimed);
        if names_agents_area && write_verb(normalized).is_some() {
            return GuardDecision::BlockedUntrustedCaller {
                caller: claimed.clone(),
                attempted_path: home.join("agents"),
            };
        }
    }

    if let HookCaller::Agent(caller_id) = caller {
        // Removed-name reservation — the removed-employee area, under any
        // spelling that names it (`agents/_trash/…`, `../_trash/…`). Checked
        // ahead of the foreign-directory rule so it is reported for what it
        // is. Moving a *live* agent directory away by hand (`mv agents/x
        // agents/x.bak`) is the foreign-directory rule's case below; a
        // follow-up `create_agent x` is refused by the MCP side anyway (name
        // collision with the moved copy, or the dangling `org.toml` record).
        if mentions_removed_agent_area(normalized) && write_verb(normalized).is_some() {
            return GuardDecision::BlockedRemovedAgentArea {
                caller: caller_id.clone(),
                attempted_path: home.join("agents").join(crate::agent_trash::AGENT_TRASH_DIR),
            };
        }
        if let Some(owner) = mentions_other_agent_dir(normalized, caller_id) {
            if write_verb(normalized).is_some() {
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
        if mentions_own_soul_md(normalized, caller_id) && write_verb(normalized).is_some() {
            return GuardDecision::BlockedOwnSoulWrite {
                caller: caller_id.clone(),
                attempted_path: home.join("agents").join(caller_id).join("SOUL.md"),
            };
        }

        // Contract lock — identical matcher, own `CONTRACT.toml`.
        if mentions_own_contract_toml(normalized, caller_id) && write_verb(normalized).is_some() {
            return GuardDecision::BlockedOwnContractWrite {
                caller: caller_id.clone(),
                attempted_path: home.join("agents").join(caller_id).join("CONTRACT.toml"),
            };
        }
    }

    GuardDecision::NotAgentFile
}
