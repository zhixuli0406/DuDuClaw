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

fn is_host_line(line: &str) -> bool {
    line.strip_prefix(HOST_SOURCES_KEY)
        .is_some_and(|r| r.trim_start().starts_with(':'))
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

/// `new_content` with its `host_sources` set to the page's previous entries
/// (from `old_content`) plus `entries`. Unchanged when there is nothing to
/// record.
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
        split_frontmatter(new_content).is_some_and(|(fm, _)| fm.lines().any(is_host_line));
    if all.is_empty() && !supplied {
        return new_content.to_string();
    }
    let line = if all.is_empty() {
        None
    } else {
        Some(format!(
            "{HOST_SOURCES_KEY}: {}",
            serde_json::to_string(&all).unwrap_or_else(|_| "[]".into())
        ))
    };
    match split_frontmatter(new_content) {
        Some((fm, body)) => {
            let mut lines: Vec<&str> = fm.lines().filter(|l| !is_host_line(l)).collect();
            if let Some(l) = &line {
                lines.push(l);
            }
            format!("---\n{}\n---\n{body}", lines.join("\n"))
        }
        None => match line {
            Some(l) => format!("---\n{l}\n---\n\n{new_content}"),
            None => new_content.to_string(),
        },
    }
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
