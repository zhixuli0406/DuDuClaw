//! Lexical search over stored originals: the term/rank helpers plus
//! `CcrStore::find`. Moved verbatim out of `ccr.rs`.

use super::*;

pub(super) fn find_terms(query: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut han = Vec::new();
    let mut has_han = false;
    fn flush_ascii(terms: &mut Vec<String>, ascii: &mut String) {
        if ascii.len() >= 3 && !terms.contains(ascii) {
            terms.push(ascii.clone());
        }
        ascii.clear();
    }
    fn flush_han(terms: &mut Vec<String>, han: &mut Vec<char>) {
        for pair in han.windows(2) {
            let term: String = pair.iter().collect();
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
        han.clear();
    }
    for character in query.chars().chain(std::iter::once(' ')) {
        if character.is_ascii_alphanumeric() {
            flush_han(&mut terms, &mut han);
            ascii.push(character.to_ascii_lowercase());
        } else if matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
        {
            flush_ascii(&mut terms, &mut ascii);
            has_han = true;
            han.push(character);
        } else {
            flush_ascii(&mut terms, &mut ascii);
            flush_han(&mut terms, &mut han);
        }
    }
    let limit = if has_han { 8 } else { 4 };
    if terms.len() <= limit {
        return terms;
    }
    (0..limit)
        .map(|index| terms[index * (terms.len() - 1) / (limit - 1)].clone())
        .collect()
}

/// SQLite's GLOB uses ASCII character classes. Pad the original with spaces
/// so this mirrors the ASCII word-boundary check in `lexical_find_rank` even
/// when a term occurs at the beginning or end of the original.
fn find_term_glob(term: &str) -> String {
    if term.is_ascii() {
        format!("*[^0-9a-z]{term}[^0-9a-z]*")
    } else {
        format!("*{term}*")
    }
}

pub(super) fn lexical_find_rank(
    original: &str,
    query: &str,
    terms: &[String],
) -> Option<(bool, usize, usize, usize)> {
    if let Some(offset) = original.find(query) {
        return Some((true, terms.len(), 0, offset));
    }
    if terms.len() < 2 {
        return None;
    }
    let lowered = original.to_ascii_lowercase();
    let bytes = lowered.as_bytes();
    let positions: Vec<Vec<usize>> = terms
        .iter()
        .map(|term| {
            lowered
                .match_indices(term)
                .filter_map(|(offset, _)| {
                    let before = !term.is_ascii()
                        || offset == 0
                        || !bytes[offset - 1].is_ascii_alphanumeric();
                    let after = !term.is_ascii()
                        || offset + term.len() == bytes.len()
                        || !bytes[offset + term.len()].is_ascii_alphanumeric();
                    (before && after).then_some(offset)
                })
                .take(16)
                .collect()
        })
        .collect();
    let matched_terms = positions.iter().filter(|items| !items.is_empty()).count();
    if matched_terms < 2 {
        return None;
    }
    let mut best: Option<(usize, usize)> = None;
    for (first_index, first_positions) in positions.iter().enumerate() {
        for second_positions in positions.iter().skip(first_index + 1) {
            for first in first_positions {
                for second in second_positions {
                    let candidate = (first.abs_diff(*second), (*first).min(*second));
                    if best.is_none_or(|current| candidate < current) {
                        best = Some(candidate);
                    }
                }
            }
        }
    }
    best.map(|(gap, offset)| (false, matched_terms, gap, offset))
}

/// A bounded lexical signal for *ordering* already authorized historical
/// handles. It deliberately makes no semantic claim: two task terms must
/// occur near each other (unless the task contains only one term), and ASCII
/// terms must have word boundaries. No source text or term leaves this module.
pub(super) fn historical_task_score(original: &str, task: &str) -> u32 {
    let terms = historical_task_terms(task);
    if terms.is_empty() {
        return 0;
    }
    let han_terms = terms
        .iter()
        .enumerate()
        .filter_map(|(index, term)| {
            let mut characters = term.chars();
            match (characters.next(), characters.next(), characters.next()) {
                (Some(a), Some(b), None) if is_han_character(a) && is_han_character(b) => {
                    Some((a, b, index))
                }
                _ => None,
            }
        })
        .collect::<Vec<_>>();
    let mut window = std::collections::VecDeque::new();
    let mut counts = vec![0usize; terms.len()];
    let mut distinct = 0usize;
    let mut best_count = 0usize;
    let mut best_gap = 160;
    let mut ascii = String::new();
    let mut ascii_start = 0usize;
    let mut ascii_overlong = false;
    let mut previous_han = None;
    // Scan every eligible token and pair exactly once. Original length is
    // bounded by MAX_ORIGINAL_BYTES, task terms by 12 and ASCII token memory
    // by 128 bytes; the queue holds only hits within 160 source bytes.
    for (offset, character) in original
        .char_indices()
        .chain(std::iter::once((original.len(), ' ')))
    {
        if character.is_ascii_alphanumeric() {
            previous_han = None;
            if ascii.is_empty() {
                ascii_start = offset;
            }
            if ascii.len() < 128 {
                ascii.push(character.to_ascii_lowercase());
            } else {
                ascii_overlong = true;
            }
            continue;
        }
        if !ascii.is_empty() {
            if !ascii_overlong && let Some(index) = terms.iter().position(|term| term == &ascii) {
                record_historical_hit(
                    &mut window,
                    &mut counts,
                    &mut distinct,
                    &mut best_count,
                    &mut best_gap,
                    ascii_start,
                    index,
                );
            }
            ascii.clear();
            ascii_overlong = false;
        }
        if is_han_character(character) {
            if let Some((previous_offset, previous_character)) = previous_han
                && let Some((_, _, index)) = han_terms
                    .iter()
                    .find(|(a, b, _)| *a == previous_character && *b == character)
            {
                record_historical_hit(
                    &mut window,
                    &mut counts,
                    &mut distinct,
                    &mut best_count,
                    &mut best_gap,
                    previous_offset,
                    *index,
                );
            }
            previous_han = Some((offset, character));
        } else {
            previous_han = None;
        }
    }
    if best_count < 2 && terms.len() > 1 {
        return 0;
    }
    // The count dominates proximity; recency breaks score ties in the caller.
    (best_count as u32) * 1_000 + (160usize.saturating_sub(best_gap) as u32)
}

fn record_historical_hit(
    window: &mut std::collections::VecDeque<(usize, usize)>,
    counts: &mut [usize],
    distinct: &mut usize,
    best_count: &mut usize,
    best_gap: &mut usize,
    offset: usize,
    term: usize,
) {
    while window
        .front()
        .is_some_and(|(old_offset, _)| offset - old_offset > 160)
    {
        let (_, old_term) = window.pop_front().expect("front exists");
        counts[old_term] -= 1;
        if counts[old_term] == 0 {
            *distinct -= 1;
        }
    }
    let near_other = window
        .iter()
        .rev()
        .find(|(_, other_term)| *other_term != term)
        .map(|(other_offset, _)| offset - other_offset);
    if counts[term] == 0 {
        *distinct += 1;
    }
    counts[term] += 1;
    window.push_back((offset, term));
    if *distinct > *best_count {
        *best_count = *distinct;
    }
    if let Some(gap) = near_other {
        *best_gap = (*best_gap).min(gap);
    }
}

fn is_han_character(character: char) -> bool {
    matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
}

fn historical_task_terms(task: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "about", "could", "does", "find", "from", "have", "into", "need", "please", "show", "that",
        "them", "this", "what", "when", "where", "which", "with", "would", "your", "the", "and",
        "for", "are", "you", "can", "how", "why",
    ];
    let mut terms = Vec::new();
    let mut ascii = String::new();
    let mut han = Vec::new();
    fn flush_ascii(terms: &mut Vec<String>, ascii: &mut String) {
        if (3..=128).contains(&ascii.len())
            && !STOP.contains(&ascii.as_str())
            && !terms.contains(ascii)
        {
            terms.push(std::mem::take(ascii));
        } else {
            ascii.clear();
        }
    }
    fn flush_han(terms: &mut Vec<String>, han: &mut Vec<char>) {
        for pair in han.windows(2) {
            let term: String = pair.iter().collect();
            if !terms.contains(&term) {
                terms.push(term);
            }
        }
        han.clear();
    }
    for character in task.chars().take(4_096).chain(std::iter::once(' ')) {
        if character.is_ascii_alphanumeric() {
            flush_han(&mut terms, &mut han);
            ascii.push(character.to_ascii_lowercase());
        } else if matches!(character, '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}' | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2fa1f}')
        {
            flush_ascii(&mut terms, &mut ascii);
            han.push(character);
        } else {
            flush_ascii(&mut terms, &mut ascii);
            flush_han(&mut terms, &mut han);
        }
        if terms.len() >= 128 {
            break;
        }
    }
    if terms.len() <= 12 {
        return terms;
    }
    (0..12)
        .map(|index| terms[index * (terms.len() - 1) / 11].clone())
        .collect()
}

impl CcrStore {
    /// Search saved originals in the caller's exact scope. SQL filters phrase
    /// and lexical candidates before the bounded scan; every returned handle
    /// also passes integrity, revocation, expiry and current route checks.
    ///
    /// Memory trade-off: the query materialises one lowercased copy of each
    /// in-scope original in SQLite's temp store so the eight term globs cost a
    /// single `lower()` per row instead of eight. The peak is bounded by
    /// [`MAX_STORE_BYTES`] (the whole file's `original` budget), and the scope
    /// index plus `tool_loop`'s per-loop `find` cap bound it further in
    /// practice. See the comment on the statement for why this beats paying
    /// the CPU instead.
    pub(super) fn find(
        &self,
        scope: &CcrScope,
        query: &str,
        allowed_sources: Option<&HashSet<String>>,
    ) -> Result<Vec<(CcrFindHit, Option<CcrBoundSource>)>, CcrError> {
        if !scope.valid() {
            return Err(CcrError::InvalidScope);
        }
        if !(3..=128).contains(&query.len()) || query.trim().is_empty() {
            return Err(CcrError::InvalidQuery);
        }
        let terms = find_terms(query);
        let patterns: Vec<String> = terms.iter().map(|term| find_term_glob(term)).collect();
        let pattern = |index: usize| patterns.get(index).map(String::as_str).unwrap_or("");
        let allowed_json = allowed_sources
            .map(|sources| serde_json::to_string(sources).expect("serializing source-key strings"));
        let conn = self.open()?;
        // Two `MATERIALIZED` CTEs. `scoped` applies every scope/revocation
        // filter and projects the lowercased, space-padded haystack ONCE per
        // surviving row; `ranked` then scores against that column and projects
        // only small integers, so the outer `WHERE`/`ORDER BY` never rebuild a
        // full-string expression. Scans per row: ~17 inline → ~9 (single CTE,
        // W3-1) → 1 `lower()` here.
        //
        // Trade-off, deliberately taken: `scoped` holds a lowercased copy of
        // every original that passes the scope filter in SQLite's temp store.
        // That is bounded by `MAX_STORE_BYTES` (64 MiB of `original` bytes for
        // the whole file, so at most that much for one scope's subset, plus
        // whatever case folding widens), and narrowed further by the
        // `(tenant_id, agent_id, session_id, source_acl)` index and the
        // per-loop `find` cap in `tool_loop`. The previous shape avoided that
        // memory by paying 8 `lower()` passes per row — the CPU half of the
        // same DoS, and the unbounded half, since the term count is attacker
        // -shaped while the byte ceiling is not.
        //
        // `exact_hit` stays `instr(original, ?6)` on the RAW column on
        // purpose: the exact-phrase signal is case-sensitive, and scoring it
        // against `original_lc` would silently widen it.
        let mut stmt = conn.prepare(
            "WITH scoped AS MATERIALIZED (
                SELECT id AS entry_id, created_at AS ranked_created_at, rowid AS ranked_rowid,
                       CASE WHEN instr(original, ?6)>0 THEN 1 ELSE 0 END AS exact_hit,
                       ' ' || lower(original) || ' ' AS original_lc
                FROM ccr_entries WHERE tenant_id=?1 AND agent_id=?2 AND session_id=?3
                AND source_acl=?4 AND expires_at>?5
                AND (binding_required=0 OR EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                     WHERE b.entry_id=ccr_entries.id AND b.tenant_id=ccr_entries.tenant_id))
                AND NOT EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                     WHERE b.entry_id=ccr_entries.id AND b.tenant_id!=ccr_entries.tenant_id)
                AND (?15 IS NULL OR source_tool IN (SELECT value FROM json_each(?15)))
                AND NOT EXISTS (SELECT 1 FROM ccr_revoked_scopes r WHERE
                   r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                   AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl)
                AND NOT EXISTS (SELECT 1 FROM ccr_revoked_sources r WHERE
                   r.tenant_id=ccr_entries.tenant_id AND r.agent_id=ccr_entries.agent_id
                   AND r.session_id=ccr_entries.session_id AND r.source_acl=ccr_entries.source_acl
                   AND r.source_tool=ccr_entries.source_tool AND r.source_call_id=ccr_entries.source_call_id)
                AND NOT EXISTS (SELECT 1 FROM ccr_artifact_bindings b
                   JOIN ccr_revoked_artifact_versions r ON r.tenant_id=b.tenant_id
                    AND r.connector=b.connector AND r.artifact_id=b.artifact_id AND r.version=b.version
                   WHERE b.entry_id=ccr_entries.id)
             ), ranked AS MATERIALIZED (
                SELECT entry_id, ranked_created_at, ranked_rowid, exact_hit,
                       ((?7!='' AND original_lc GLOB ?7)
                        + (?8!='' AND original_lc GLOB ?8)
                        + (?9!='' AND original_lc GLOB ?9)
                        + (?10!='' AND original_lc GLOB ?10)
                        + (?11!='' AND original_lc GLOB ?11)
                        + (?12!='' AND original_lc GLOB ?12)
                        + (?13!='' AND original_lc GLOB ?13)
                        + (?14!='' AND original_lc GLOB ?14)) AS term_hits
                FROM scoped
             )
             SELECT e.id, e.source_tool, e.original, e.content_sha256, e.content_bytes,
                    e.transform_version,
                    (SELECT connector FROM ccr_artifact_bindings WHERE entry_id=e.id),
                    (SELECT artifact_id FROM ccr_artifact_bindings WHERE entry_id=e.id),
                    (SELECT version FROM ccr_artifact_bindings WHERE entry_id=e.id),
                    (SELECT acl_revision FROM ccr_artifact_bindings WHERE entry_id=e.id),
                    e.binding_required
             FROM ranked JOIN ccr_entries e ON e.id=ranked.entry_id
             WHERE exact_hit>0 OR term_hits>0
             ORDER BY CASE WHEN exact_hit>0 THEN 0 ELSE 1 END, term_hits DESC,
                      ranked_created_at DESC, ranked_rowid DESC LIMIT 1000",
        )?;
        let rows = stmt.query_map(
            params![
                scope.tenant_id,
                scope.agent_id,
                scope.session_id,
                scope.source_acl,
                unix_now(),
                query,
                pattern(0),
                pattern(1),
                pattern(2),
                pattern(3),
                pattern(4),
                pattern(5),
                pattern(6),
                pattern(7),
                allowed_json
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, Option<String>>(9)?,
                    row.get::<_, i64>(10)?,
                ))
            },
        )?;
        let mut ranked = Vec::new();
        for (recency_index, row) in rows.enumerate() {
            let (
                id,
                source,
                original,
                digest,
                bytes,
                version,
                connector,
                artifact_id,
                artifact_version,
                acl_revision,
                binding_required,
            ) = row?;
            if allowed_sources.is_some_and(|allowed| !allowed.contains(&source))
                || version != CCR_ENTRY_VERSION
                || (binding_required != 0 && binding_required != 1)
                || bytes != original.len() as i64
                || original.len() > MAX_ORIGINAL_BYTES
                || digest != format!("{:x}", Sha256::digest(original.as_bytes()))
            {
                continue;
            }
            if let Some((exact, matched_terms, gap, byte_offset)) =
                lexical_find_rank(&original, query, &terms)
            {
                let bound = match (connector, artifact_id, artifact_version, acl_revision) {
                    (None, None, None, None) if binding_required == 0 => None,
                    (Some(connector), Some(artifact_id), Some(version), Some(acl_revision)) => {
                        Some(CcrBoundSource {
                            artifact: CcrSourceArtifact {
                                connector,
                                artifact_id,
                                version,
                                acl_revision,
                            },
                            saved_sha256: digest,
                        })
                    }
                    // A malformed binding must never be treated as unbound.
                    _ => continue,
                };
                ranked.push((
                    exact,
                    matched_terms,
                    gap,
                    recency_index,
                    CcrFindHit {
                        id,
                        byte_offset,
                        total_bytes: original.len(),
                        exact_phrase: exact,
                        matched_terms,
                    },
                    bound,
                ));
            }
        }
        ranked.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
                .then_with(|| a.3.cmp(&b.3))
        });
        Ok(ranked
            .into_iter()
            .map(|(_, _, _, _, hit, bound)| (hit, bound))
            .collect())
    }
}
