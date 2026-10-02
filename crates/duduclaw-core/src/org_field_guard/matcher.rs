//! **The comparators**: one generic frozen-field differ plus the small shared
//! path / text primitives the entry points in [`super`] lean on.
//!
//! Split out of the single 2,374-line `org_field_guard.rs` on 2026-09-29
//! (audit O9). [`diff_frozen`] replaces the three hand-written `diff_*`
//! functions the module used to carry — the *shapes* are still exactly three,
//! but they are now selected by data ([`super::rules::FROZEN_FIELDS`]) rather
//! than by a hand-maintained call chain, so freezing a new field is a table
//! row instead of a new function plus a new branch.
//!
//! Output wording is unchanged from the pre-O9 implementation and is pinned by
//! the regression tests.

use std::path::Path;

use crate::agent_guard::lexical_normalize;

use super::rules::{EPHEMERAL_DIR_NAME, FrozenShape, IDENTITY_ENV_KEYS, WRITE_VERBS};

/// Every change a frozen entry sees between `old` and `new`.
///
/// Empty result = unchanged. Each shape's wording matches the pre-O9
/// `diff_agent_org_fields` / `diff_agent_capability_fields` /
/// `diff_config_sections` byte for byte.
pub(super) fn diff_frozen(
    shape: &FrozenShape,
    old: &toml::Table,
    new: &toml::Table,
) -> Vec<String> {
    match *shape {
        FrozenShape::StringKeys { section, keys } => keys
            .iter()
            .filter_map(|field| {
                let before = string_field(old, section, field);
                let after = string_field(new, section, field);
                (before != after).then(|| format!("{field}：「{before}」→「{after}」"))
            })
            .collect(),
        FrozenShape::TableKeys { section } => diff_table_keys(section, old, new),
        FrozenShape::WholeSection { section } => {
            let before = old.get(section);
            let after = new.get(section);
            if before == after {
                Vec::new()
            } else {
                vec![format!(
                    "[{section}]：{} → {}",
                    describe_section(before),
                    describe_section(after)
                )]
            }
        }
    }
}

/// `[<section>] <field>` as a string; absent / non-string reads as empty, so
/// *deleting* `reports_to` counts as a change (it would silently reset the
/// agent to a root node) and replacing the table with a scalar cannot launder
/// a change past the comparison.
fn string_field(table: &toml::Table, section: &str, field: &str) -> String {
    table
        .get(section)
        .and_then(|v| v.as_table())
        .and_then(|t| t.get(field))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string()
}

/// Union-of-keys walk over `[<section>]`.
///
/// Whole-table comparison over the **union** of both sides' keys, so adding a
/// key, deleting one, changing a value, and replacing the table with a scalar
/// all register. A key list would have to be maintained in lockstep with
/// `CapabilitiesConfig`; the union walk needs no maintenance and is
/// fail-closed for keys that do not exist yet.
///
/// The per-key shape (rather than [`FrozenShape::WholeSection`]'s whole-value
/// one) is what makes the block message name the exact switch that moved — an
/// operator reading the hook's stderr should see `os_native`, not "the section
/// changed".
fn diff_table_keys(section: &str, old: &toml::Table, new: &toml::Table) -> Vec<String> {
    let before = section_table(old, section);
    let after = section_table(new, section);

    // A section that is present but not a table on either side cannot be
    // compared key-wise; report it as a whole rather than letting a reshape
    // launder a change past a key walk.
    if before.is_none() || after.is_none() {
        let (b, a) = (old.get(section), new.get(section));
        if b == a {
            return Vec::new();
        }
        return vec![format!(
            "[{section}]：{} → {}",
            describe_section(b),
            describe_section(a)
        )];
    }
    let (before, after) = (before.unwrap(), after.unwrap());

    let mut keys: Vec<&str> = before.keys().map(String::as_str).collect();
    for k in after.keys() {
        if !keys.contains(&k.as_str()) {
            keys.push(k.as_str());
        }
    }
    keys.sort_unstable();
    keys.into_iter()
        .filter_map(|key| {
            let b = before.get(key);
            let a = after.get(key);
            (b != a).then(|| {
                format!(
                    "{section}.{key}：{} → {}",
                    describe_section(b),
                    describe_section(a)
                )
            })
        })
        .collect()
}

/// `[<section>]` as a table, or `None` when it is present with a non-table
/// shape. Absent reads as an empty table so adding the section from scratch
/// still registers as a change.
fn section_table<'a>(
    table: &'a toml::Table,
    section: &str,
) -> Option<std::borrow::Cow<'a, toml::Table>> {
    match table.get(section) {
        None => Some(std::borrow::Cow::Owned(toml::Table::new())),
        Some(toml::Value::Table(t)) => Some(std::borrow::Cow::Borrowed(t)),
        Some(_) => None,
    }
}

pub(super) fn describe_section(value: Option<&toml::Value>) -> String {
    match value {
        None => "（不存在）".to_string(),
        Some(v) => crate::truncate_chars(&v.to_string().replace('\n', " "), 80),
    }
}

pub(super) fn first_line(s: &str) -> String {
    crate::truncate_chars(s.lines().next().unwrap_or_default(), 120)
}

/// Case-insensitive component-wise prefix check returning the components of
/// `path` that follow `prefix`.
///
/// Sibling of `agent_guard::strip_prefix_ci` (private there) — kept private
/// here too so the two guards can evolve independently.
pub(super) fn components_after_ci(path: &Path, prefix: &Path) -> Option<Vec<String>> {
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

/// Every `(server, key, value)` identity triple declared in a `.mcp.json`,
/// sorted so the comparison ignores JSON key ordering.
///
/// Errors only on invalid JSON. A structurally odd but valid document (no
/// `mcpServers`, a non-object entry, …) simply declares no identity, which is
/// correct: there is nothing to protect and nothing to compare.
pub(super) fn identity_env_pairs(content: &str) -> Result<Vec<(String, String, String)>, String> {
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
pub(super) fn describe_pairs(pairs: &[(String, String, String)]) -> String {
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

/// The agent directory name that owns `normalized`, or `None` when the path is
/// not inside any agent directory.
///
/// `<home>/agents/<id>/…` → `<id>`; `<home>/agents/.ephemeral/<eph>/…` →
/// `<eph>`. A path directly at `<home>/agents/<x>` (no further component) is
/// **not** owned — it is either the agent directory itself or a stray file at
/// the agents root, and `check_agent_file_write` already covers the latter.
pub(super) fn owning_agent_dir(normalized: &Path, home: &Path) -> Option<String> {
    let agents_root = lexical_normalize(&home.join("agents"));
    let rest = components_after_ci(normalized, &agents_root)?;
    if rest.first()?.eq_ignore_ascii_case(EPHEMERAL_DIR_NAME) {
        // `<agents>/.ephemeral/<eph-id>/<something>` — need the trailing
        // component, otherwise this is the scaffold directory itself.
        return (rest.len() >= 3).then(|| rest[1].clone());
    }
    (rest.len() >= 2).then(|| rest[0].clone())
}

pub(super) fn write_verb(normalized_command: &str) -> Option<&'static str> {
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
pub(super) fn agents_dir_segments(normalized_command: &str) -> Vec<&str> {
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

/// Whether any path in `normalized_command` (already lowercased, `/`
/// separators) has a component that is exactly the removed-employee directory
/// name (`_trash`). Component equality, never a substring test, so
/// `my_trash/` or `_trash_old` do not count. Catches the spellings the
/// `agents/<id>/` scan cannot see, notably the cwd-relative `../_trash/…`
/// (an agent's Bash cwd is its own agent directory). Speed bump only, like the
/// rest of this module: `T=_tr; rm -rf ../${T}ash` evades it.
pub(super) fn mentions_removed_agent_area(normalized_command: &str) -> bool {
    let trash = crate::agent_trash::AGENT_TRASH_DIR.to_ascii_lowercase();
    normalized_command
        .split(|c: char| c.is_whitespace() || matches!(c, '/' | '\'' | '"' | ';' | '&' | '|' | '(' | ')' | '<' | '>' | '='))
        .any(|component| component == trash)
}

/// Find a `agents/<id>/` path segment in `normalized_command` whose `<id>`
/// is not `caller_id`. Returns the first match. See
/// [`agents_dir_segments`] for the boundary-check contract.
pub(super) fn mentions_other_agent_dir(normalized_command: &str, caller_id: &str) -> Option<String> {
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
/// - it is a bare/relative filename whose directory part lexically
///   normalises to nothing (`SOUL.md`, `./SOUL.md`, `././SOUL.md`,
///   `.//SOUL.md`, `x/../SOUL.md`) — an agent's Bash cwd is its own agent
///   directory, so such a spelling can only resolve to its own file
///   (`../SOUL.md` keeps its `..` and does not count); or
/// - its directory portion contains a boundary-checked `agents/<caller_id>/`
///   segment (see [`agents_dir_segments`] — `myagents/<caller_id>/SOUL.md`
///   does NOT count, same false-positive guard as `mentions_other_agent_dir`
///   uses, and is why this is directory-scoped per-token rather than a
///   whole-string substring search).
///
/// A token naming a *different* agent's directory is not "own" here — that
/// case is caught earlier by [`mentions_other_agent_dir`] when a write verb
/// is also present; in isolation it is simply not this caller's file.
pub(super) fn mentions_own_soul_md(normalized_command: &str, caller_id: &str) -> bool {
    mentions_own_agent_file(normalized_command, caller_id, "soul.md")
}

/// Contract-lock companion of [`mentions_own_soul_md`]: whether
/// `normalized_command` (already lowercased) targets `caller_id`'s OWN
/// `CONTRACT.toml` via Bash. Same per-token rules, same caveats.
pub(super) fn mentions_own_contract_toml(normalized_command: &str, caller_id: &str) -> bool {
    mentions_own_agent_file(normalized_command, caller_id, "contract.toml")
}

/// Shared body of the "own file" Bash matchers. `basename_lower` must be
/// lowercase, matching the already-lowercased command.
fn mentions_own_agent_file(normalized_command: &str, caller_id: &str, basename_lower: &str) -> bool {
    let caller_lower = caller_id.to_ascii_lowercase();
    normalized_command.split_whitespace().any(|raw_tok| {
        let tok = raw_tok.trim_matches(|c: char| matches!(c, '\'' | '"' | '>' | '<'));
        // Only a token whose filename component is *exactly* the basename
        // qualifies — `strip_suffix` on e.g. "old_soul.md" leaves a
        // non-empty, non-`/`-terminated remainder ("old_"), which the match
        // below correctly rejects instead of treating it as a directory.
        let Some(rest) = tok.strip_suffix(basename_lower) else {
            return false;
        };
        match rest.strip_suffix('/') {
            None if rest.is_empty() => true, // bare "soul.md" ⇒ own file (cwd is the agent's own dir)
            None => false,                   // e.g. "old_soul.md" — not a real path boundary
            Some(dir) => {
                // Lexical only — the filesystem is never consulted. `./`,
                // `././`, `.//` and `x/../` collapse to nothing, i.e. the
                // same cwd-relative spelling as a bare `soul.md`; "/soul.md"
                // (no agent dir named at all) also lands here.
                let dir = lexical_dir(dir);
                dir.is_empty()
                    || agents_dir_segments(&dir).into_iter().any(|seg| seg == caller_lower)
            }
        }
    })
}

/// Lexically normalise the directory part of a `/`-separated command token:
/// drop empty and `.` segments, and let `..` cancel the preceding ordinary
/// segment. A `..` with nothing to cancel is kept, so `../soul.md` (the
/// agents root, not the caller's own directory) never collapses to the empty
/// "cwd" spelling. Leading `/` is not preserved — callers only ask "is
/// anything left" and "which `agents/<id>/` segments are named", and
/// [`agents_dir_segments`] already treats a string start as a boundary.
fn lexical_dir(dir: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for seg in dir.split('/') {
        match seg {
            "" | "." => {}
            ".." => match out.last() {
                Some(last) if *last != ".." => {
                    out.pop();
                }
                _ => out.push(".."),
            },
            other => out.push(other),
        }
    }
    out.join("/")
}
