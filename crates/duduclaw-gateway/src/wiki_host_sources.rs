//! Host-recorded sources of employee-written wiki pages (P2-B H-4).
//!
//! When an employee writes a wiki page with `wiki_write` during a turn or a
//! dispatch run, the MCP server records where that write came from in the
//! page's frontmatter as `host_sources: [...]` — entries in the same form as
//! auto pages' `sources` (`conversation:<session>:<message key>`). The key is
//! owned by the host: a value the employee wrote itself is dropped, and the
//! entries already on the page are kept across rewrites (newest last, at most
//! [`HOST_SOURCES_MAX`]).
//!
//! Forget by source never deletes such a page (the employee may have written
//! things that did not come from the forgotten conversation); a plan lists
//! the matching pages as needing a human look.

use std::path::Path;

use duduclaw_memory::{SourceKind, SourceRef};

use crate::memory_forget_steps::{SelectorView, page_source_matches};

/// Frontmatter key of the host-recorded sources.
pub const HOST_SOURCES_KEY: &str = "host_sources";
/// v1.84.0: frontmatter key set (`host_promoted: true`) only by the human
/// promotion path (`auto_wiki_page::promote_page`). Like [`HOST_SOURCES_KEY`]
/// it is owned by the host: every MCP wiki write passes through
/// [`stamp_host_sources`], which drops a value the writer supplied and never
/// adds one, so a page rewritten over MCP is no longer promoted.
pub const HOST_PROMOTED_KEY: &str = "host_promoted";
/// Most entries kept on one page.
pub const HOST_SOURCES_MAX: usize = 20;

/// The page `sources`-style entries for host-made sources. External MCP
/// calls and system writes have no conversation and give none.
pub fn entries_for(sources: &[SourceRef]) -> Vec<String> {
    sources
        .iter()
        .filter(|s| {
            !matches!(
                s.kind,
                SourceKind::McpExternal | SourceKind::System | SourceKind::UpstreamUnknown
            )
        })
        .map(|s| crate::memory_provenance::wiki_source_entry(&s.session, &s.message))
        .collect()
}

/// `(frontmatter, body)` when `content` opens with a `---` block.
fn split_frontmatter(content: &str) -> Option<(&str, &str)> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))?;
    let end = rest.find("\n---")?;
    let fm = &rest[..end];
    let after = &rest[end + 4..];
    let body = after
        .strip_prefix("\r\n")
        .or_else(|| after.strip_prefix('\n'))
        .unwrap_or(after);
    Some((fm, body))
}

fn is_key_line(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .is_some_and(|r| r.trim_start().starts_with(':'))
}

fn is_host_line(line: &str) -> bool {
    is_key_line(line, HOST_SOURCES_KEY)
}

/// A frontmatter line of a key the host owns ([`HOST_SOURCES_KEY`],
/// [`HOST_PROMOTED_KEY`]).
fn is_host_owned_line(line: &str) -> bool {
    is_host_line(line) || is_key_line(line, HOST_PROMOTED_KEY)
}

/// A legacy promotion marker, deliberately outside every conversation format.
pub const LEGACY_PROMOTION_SOURCE: &str = "legacy:human-confirmed";

/// Presence, not parsed value: malformed/empty stamps must not make a page
/// eligible for the legacy promotion exception.
pub fn page_has_host_stamp(content: &str) -> bool {
    split_frontmatter(content).is_some_and(|(fm, _)| fm.lines().any(is_host_owned_line))
}

/// The host-recorded sources of a page (empty when none or unreadable).
pub fn page_host_sources(content: &str) -> Vec<String> {
    let Some((fm, _)) = split_frontmatter(content) else {
        return Vec::new();
    };
    fm.lines()
        .find(|l| is_host_line(l))
        .and_then(|l| l.split_once(':'))
        .and_then(|(_, v)| serde_json::from_str::<Vec<String>>(v.trim()).ok())
        .unwrap_or_default()
}

/// v1.84.0: whether the page carries the host's promotion mark
/// (`host_promoted: true` in its frontmatter, see [`HOST_PROMOTED_KEY`]).
pub fn page_host_promoted(content: &str) -> bool {
    let Some((fm, _)) = split_frontmatter(content) else {
        return false;
    };
    fm.lines()
        .find(|l| is_key_line(l, HOST_PROMOTED_KEY))
        .and_then(|l| l.split_once(':'))
        .is_some_and(|(_, v)| v.trim() == "true")
}

/// `content` with exactly the given host-owned frontmatter: `host_sources`
/// set to `entries` (no line when empty) and `host_promoted: true` when
/// `promoted`. Every host-owned line already in `content` is dropped first,
/// so the result never carries a value the content's writer supplied.
pub fn set_host_fields(content: &str, entries: &[String], promoted: bool) -> String {
    let mut host_lines: Vec<String> = Vec::new();
    if !entries.is_empty() {
        host_lines.push(format!(
            "{HOST_SOURCES_KEY}: {}",
            serde_json::to_string(entries).unwrap_or_else(|_| "[]".into())
        ));
    }
    if promoted {
        host_lines.push(format!("{HOST_PROMOTED_KEY}: true"));
    }
    match split_frontmatter(content) {
        Some((fm, body)) => {
            let mut lines: Vec<&str> = fm.lines().filter(|l| !is_host_owned_line(l)).collect();
            lines.extend(host_lines.iter().map(String::as_str));
            format!("---\n{}\n---\n{body}", lines.join("\n"))
        }
        None if host_lines.is_empty() => content.to_string(),
        None => format!("---\n{}\n---\n\n{content}", host_lines.join("\n")),
    }
}

/// `new_content` with its `host_sources` set to the page's previous entries
/// (from `old_content`) plus `entries`. Unchanged when there is nothing to
/// record. A writer-supplied host-owned key (`host_sources`,
/// `host_promoted`) is always dropped, and the promotion mark is never
/// carried over: content rewritten by this path is not what a human promoted.
pub fn stamp_host_sources(
    new_content: &str,
    old_content: Option<&str>,
    entries: &[String],
) -> String {
    let mut all: Vec<String> = old_content.map(page_host_sources).unwrap_or_default();
    for e in entries {
        all.retain(|x| x != e);
        all.push(e.clone());
    }
    if all.len() > HOST_SOURCES_MAX {
        all.drain(..all.len() - HOST_SOURCES_MAX);
    }
    let supplied =
        split_frontmatter(new_content).is_some_and(|(fm, _)| fm.lines().any(is_host_owned_line));
    if all.is_empty() && !supplied {
        return new_content.to_string();
    }
    set_host_fields(new_content, &all, false)
}

/// Employee-written pages (any page of the employee's wiki outside `auto/`,
/// and shared wiki pages) whose host-recorded sources the selector forgets.
/// Returned as display paths (`wiki/<path>` / `shared/<path>`).
pub fn pages_needing_review(home: &Path, agent_id: &str, sel: &SelectorView) -> Vec<String> {
    let mut out = Vec::new();
    if duduclaw_core::is_valid_agent_id(agent_id) {
        scan(
            &home.join("agents").join(agent_id).join("wiki"),
            "wiki",
            true,
            sel,
            &mut out,
        );
    }
    scan(
        &home.join("shared").join("wiki"),
        "shared",
        false,
        sel,
        &mut out,
    );
    out.sort();
    out
}

fn scan(root: &Path, label: &str, skip_auto: bool, sel: &SelectorView, out: &mut Vec<String>) {
    let mut stack = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in rd.flatten() {
            seen += 1;
            if seen > 20_000 {
                return;
            }
            let path = entry.path();
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if ft.is_dir() {
                if !(skip_auto && rel == "auto") {
                    stack.push(path);
                }
                continue;
            }
            if !rel.ends_with(".md") || rel.starts_with('_') {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            if page_host_sources(&text)
                .iter()
                .any(|s| page_source_matches(s, sel))
            {
                out.push(format!("{label}/{rel}"));
            }
        }
    }
}

#[cfg(test)]
#[path = "wiki_host_sources_tests.rs"]
mod tests;
