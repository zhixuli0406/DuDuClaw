//! v1.84.0 (RM-200, review H1): an employee may not write its own
//! `wiki/auto/**` nor the legacy `wiki/.auto-pages*` lock / sidecar through
//! the hook. Split out of `mod.rs` to keep it under the project's 800-line
//! ceiling; same shape as the own-directory exceptions there (`SOUL.md`,
//! `CONTRACT.toml`, `INSTRUCTIONS.md`).
//!
//! Auto pages are filed by the gateway from conversation content, and which
//! conversation may see one is decided from host-owned records. The only
//! writers are the gateway's auto writer and the dashboard's human actions
//! (promote, archive, share-out), none of which runs through this hook.
//! Reads are unaffected.
//!
//! The Write/Edit lane ([`check_own_auto_wiki_write`]) is a real path check
//! (literal and real path, every spelling a file system opens as `auto/`).
//! The Bash lane ([`mentions_own_auto_wiki`]) remains a **speed bump, not a
//! boundary**, exactly as documented for the other protected files on
//! [`super::check_bash_protected_write`]: shell text cannot be analysed
//! reliably, so it only stops the obvious spellings. The gateway does not
//! rely on either lane — a page on disk that its registry did not record is
//! hidden from every conversation.

use std::path::Path;

use crate::agent_guard::{lexical_normalize, GuardDecision};
use crate::wiki_auto_namespace::{is_auto_namespace_segment, is_auto_pages_sidecar, segment_opens_as};

use super::matcher::{agents_dir_segments, components_after_ci, owning_agent_dir};
use super::real_path::with_real_path;
use super::rules::EPHEMERAL_DIR_NAME;
use super::HookCaller;

/// Block an agent-identified caller from writing (Write / Edit / MultiEdit /
/// NotebookEdit — creating a file included) anything under its OWN
/// `wiki/auto/` or the `wiki/.auto-pages*` lock / sidecar.
///
/// `HookCaller::Absent` (operator running by hand) is a no-op, like every
/// other caller-scoped check; `HookCaller::Untrusted` is refused earlier by
/// [`super::check_caller_scope`] for every path under `<home>/agents/`, and a
/// foreign agent's wiki by the same stage.
pub fn check_own_auto_wiki_write(file_path: &Path, home: &Path, caller: &HookCaller) -> GuardDecision {
    let HookCaller::Agent(caller_id) = caller else {
        return GuardDecision::NotAgentFile;
    };
    with_real_path(file_path, home, |p, h| own_auto_wiki_at(p, h, caller_id))
}

fn own_auto_wiki_at(file_path: &Path, home: &Path, caller_id: &str) -> GuardDecision {
    let normalized = lexical_normalize(file_path);
    let Some(owner) = owning_agent_dir(&normalized, home) else {
        return GuardDecision::NotAgentFile;
    };
    if !owner.eq_ignore_ascii_case(caller_id) {
        // A foreign agent's wiki is already blocked by `check_caller_scope`.
        return GuardDecision::NotAgentFile;
    }
    let Some(rest) = components_after_ci(&normalized, &lexical_normalize(&home.join("agents"))) else {
        return GuardDecision::NotAgentFile;
    };
    // Components inside the employee directory (`<id>/…` or `.ephemeral/<id>/…`).
    let base = if rest.first().is_some_and(|c| c.eq_ignore_ascii_case(EPHEMERAL_DIR_NAME)) { 2 } else { 1 };
    if !is_auto_wiki_rel(rest.get(base..).unwrap_or_default()) {
        return GuardDecision::NotAgentFile;
    }
    GuardDecision::BlockedOwnAutoWikiWrite {
        caller: caller_id.to_string(),
        attempted_path: normalized,
    }
}

/// Whether the components below an employee directory name its wiki's auto
/// namespace (the directory itself included, for a rename / delete) or the
/// legacy lock / a sidecar beside it.
fn is_auto_wiki_rel(rel: &[String]) -> bool {
    match rel {
        [wiki, entry, ..] if segment_opens_as(wiki, "wiki") => {
            is_auto_namespace_segment(entry) || is_auto_pages_sidecar(entry)
        }
        _ => false,
    }
}

/// Bash-lane speed bump: whether a token of `normalized_command` (already
/// lowercased, `/` separators) names the caller's own `wiki/auto/…` or
/// `wiki/.auto-pages*` — as a cwd-relative path (`wiki/auto/x.md`,
/// `./wiki/AUTO./x.md`; an employee's Bash cwd is its own directory) or
/// through `agents/<caller>/wiki/auto/…`. A token naming another agent's
/// directory is the foreign-directory rule's case. Evadable by any computed
/// path (`W=wi; rm ${W}ki/auto/x.md`) — see the module documentation.
pub(super) fn mentions_own_auto_wiki(normalized_command: &str, caller_id: &str) -> bool {
    let caller_lower = caller_id.to_ascii_lowercase();
    normalized_command
        .split(|c: char| c.is_whitespace() || matches!(c, '\'' | '"' | ';' | '&' | '|' | '(' | ')' | '<' | '>' | '='))
        .any(|token| {
            let segments = agents_dir_segments(token);
            if segments.iter().any(|s| *s != caller_lower) {
                return false;
            }
            let parts: Vec<String> = token
                .split('/')
                .filter(|p| !p.is_empty() && *p != ".")
                .map(str::to_string)
                .collect();
            (0..parts.len()).any(|i| {
                // `wiki` must be the token's first component (cwd-relative)
                // or follow `agents/<caller>/`.
                let anchored = i == 0
                    || (i >= 2
                        && parts[i - 2] == "agents"
                        && parts[i - 1] == caller_lower);
                anchored && is_auto_wiki_rel(&parts[i..])
            })
        })
}
