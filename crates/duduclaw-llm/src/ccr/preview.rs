//! Preview construction — the lossless / lossy compaction helpers behind
//! [`CcrRuntime::preview`]. Moved verbatim out of `ccr.rs` (file-size split).

use super::find::{find_terms, lexical_find_rank};

/// Remove only JSON's insignificant whitespace. Parsing into `Value` and
/// serializing would collapse duplicate keys and rewrite numeric spellings.
/// Call this only after `serde_json` has validated the input.
pub(super) fn strip_json_whitespace(valid_json: &str) -> String {
    let mut compact = Vec::with_capacity(valid_json.len());
    let mut in_string = false;
    let mut escaped = false;
    for byte in valid_json.bytes() {
        if in_string {
            compact.push(byte);
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
            compact.push(byte);
        } else if !matches!(byte, b' ' | b'\t' | b'\n' | b'\r') {
            compact.push(byte);
        }
    }
    String::from_utf8(compact).expect("removing ASCII whitespace preserves UTF-8")
}

/// Whether `original` is shaped like a whole JSON document — bracket-delimited
/// at both ends (`{…}` or `[…]`) — as opposed to merely *starting* with a
/// bracket.
///
/// Used to decide that content `serde_json` could not validate must keep its
/// exact bytes instead of being reduced to a lossy preview. Testing the first
/// byte alone also caught bracketed log timestamps and Markdown links, which
/// are ordinary prose and belong in CCR like any other long result.
pub(super) fn is_bracket_delimited(original: &str) -> bool {
    let trimmed = original.trim();
    matches!(
        (trimmed.as_bytes().first(), trimmed.as_bytes().last()),
        (Some(b'{'), Some(b'}')) | (Some(b'['), Some(b']'))
    ) && trimmed.len() >= 2
}

pub(super) fn compact_json_lines(original: &str) -> Option<String> {
    let mut compact = String::new();
    let mut count = 0;
    for line in original.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with('{') {
            return None;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
        if !value.is_object() {
            return None;
        }
        if count > 0 {
            compact.push('\n');
        }
        compact.push_str(&strip_json_whitespace(trimmed));
        count += 1;
    }
    (count >= 8).then_some(compact)
}

pub(super) fn diagnostic_rank(line: &str) -> u8 {
    let lower = line.to_ascii_lowercase();
    if lower.contains("fatal") || lower.contains("panic") {
        4
    } else if lower.contains("exception") || lower.contains("traceback") {
        3
    } else if lower.contains("error") {
        2
    } else if lower.contains("warning") || lower.contains("warn:") {
        1
    } else {
        0
    }
}

pub(super) fn markdown_section_highlights(original: &str, query: Option<&str>) -> Vec<String> {
    let mut sections = Vec::new();
    let mut lines = original.lines().enumerate().peekable();
    while let Some((index, line)) = lines.next() {
        let heading = line.trim();
        if !(heading.starts_with("# ") || heading.starts_with("## ") || heading.starts_with("### "))
        {
            continue;
        }
        let mut summary = format!(
            "L{} {}",
            index + 1,
            duduclaw_core::truncate_bytes(heading, 120)
        );
        if let Some((_, next)) = lines
            .clone()
            .take(4)
            .find(|(_, next)| !next.trim().is_empty())
            && !next.trim().starts_with('#')
        {
            summary.push_str(" — ");
            summary.push_str(duduclaw_core::truncate_bytes(next.trim(), 160));
        }
        sections.push(summary);
    }
    if sections.len() <= 6 {
        return sections;
    }
    let mut selected = Vec::new();
    if let Some(query) = query
        .map(str::trim)
        .filter(|query| (3..=128).contains(&query.len()))
    {
        let terms = find_terms(query);
        let query_lower = query.to_lowercase();
        let mut ranked: Vec<_> = sections
            .iter()
            .enumerate()
            .filter_map(|(index, section)| {
                let exact = section.to_lowercase().contains(&query_lower);
                let lexical = lexical_find_rank(section, query, &terms);
                (exact || lexical.is_some()).then_some((index, exact, lexical))
            })
            .collect();
        ranked.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| {
                    b.2.as_ref()
                        .map(|rank| rank.1)
                        .cmp(&a.2.as_ref().map(|rank| rank.1))
                })
                .then_with(|| a.0.cmp(&b.0))
        });
        selected.extend(ranked.into_iter().take(3).map(|(index, _, _)| index));
    }
    // Sample the full document, including its end, when there are more
    // headings than the preview budget permits.
    for slot in [0, 5, 1, 2, 3, 4] {
        let index = slot * (sections.len() - 1) / 5;
        if !selected.contains(&index) {
            selected.push(index);
        }
        if selected.len() == 6 {
            break;
        }
    }
    selected.sort_unstable();
    selected
        .into_iter()
        .map(|index| sections[index].clone())
        .collect()
}

/// Conservative CSV/TSV detector. Quoted or irregular rows fall back to the
/// generic preview; only wholly numeric measurement columns are considered.
pub(super) fn tabular_outlier_rows(original: &str) -> Vec<String> {
    let first_line = original.lines().next().unwrap_or_default();
    let delimiter = if first_line.contains('\t') {
        '\t'
    } else if first_line.contains(',') {
        ','
    } else {
        return Vec::new();
    };
    let lines: Vec<&str> = original.lines().collect();
    if !(33..=20_000).contains(&lines.len()) || lines.iter().any(|line| line.contains('"')) {
        return Vec::new();
    }
    let headers: Vec<&str> = lines[0].split(delimiter).map(str::trim).collect();
    if !(2..=16).contains(&headers.len()) || headers.iter().any(|header| header.is_empty()) {
        return Vec::new();
    }
    let rows: Vec<Vec<&str>> = lines[1..]
        .iter()
        .map(|line| line.split(delimiter).map(str::trim).collect())
        .collect();
    if rows
        .iter()
        .any(|row: &Vec<&str>| row.len() != headers.len())
    {
        return Vec::new();
    }
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    for (column, header) in headers.iter().enumerate() {
        let name = header.to_ascii_lowercase();
        if ![
            "latency", "wait", "duration", "cost", "amount", "backlog", "count", "rate", "score",
            "value", "sla", "tickets", "capacity", "arrival",
        ]
        .iter()
        .any(|term| name.contains(term))
        {
            continue;
        }
        let values: Option<Vec<f64>> = rows
            .iter()
            .map(|row| {
                row[column]
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
            })
            .collect();
        let Some(values) = values else { continue };
        let mut ordered = values.clone();
        ordered.sort_by(f64::total_cmp);
        let q1 = ordered[ordered.len() / 4];
        let q3 = ordered[ordered.len() * 3 / 4];
        let iqr = q3 - q1;
        let (lower, upper, scale) = if iqr > 0.0 {
            (q1 - 1.5 * iqr, q3 + 1.5 * iqr, iqr)
        } else {
            (q1, q3, q1.abs().max(1.0))
        };
        for (row_index, value) in values.into_iter().enumerate() {
            let distance = if value < lower {
                lower - value
            } else if value > upper {
                value - upper
            } else {
                continue;
            };
            candidates.push((distance / scale, row_index, column));
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
    let mut seen = std::collections::HashSet::new();
    let mut selected: Vec<(usize, usize)> = candidates
        .into_iter()
        .filter_map(|(_, row, column)| seen.insert(row).then_some((row, column)))
        .take(6)
        .collect();
    selected.sort_by_key(|(row, _)| *row);
    selected
        .into_iter()
        .map(|(row, column)| {
            format!(
                "L{} {} [{}]",
                row + 2,
                duduclaw_core::truncate_bytes(lines[row + 1], 300),
                headers[column]
            )
        })
        .collect()
}
