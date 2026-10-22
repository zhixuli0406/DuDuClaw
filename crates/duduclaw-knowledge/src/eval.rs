//! Retrieval evaluation (`duduclaw knowledge eval`): Recall@1/5/10 and
//! MRR@10 over a JSONL question set, per question type, plus how often an
//! unanswerable question correctly gets no result.
//!
//! Question line:
//! `{"id":"q01","question":"…","type":"factoid","answerable":true,
//!   "gold":[{"source":"員工手冊.pdf","page":12,"contains":"七日"}]}`
//!
//! A hit matches a gold entry when the source title is the same (case
//! insensitive), the position agrees (page within the chunk's page range,
//! same sheet and row within range, or same slide) and, when `contains` is
//! given, the chunk text (NFKC) contains it.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::locator::LocatorKind;
use crate::search::Hit;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gold {
    pub source: String,
    #[serde(default)]
    pub page: Option<u32>,
    #[serde(default)]
    pub slide: Option<u32>,
    #[serde(default)]
    pub sheet: Option<String>,
    #[serde(default)]
    pub row: Option<u32>,
    #[serde(default)]
    pub contains: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub question: String,
    #[serde(rename = "type", default)]
    pub qtype: String,
    #[serde(default = "default_true")]
    pub answerable: bool,
    #[serde(default)]
    pub gold: Vec<Gold>,
}

/// Parse a JSONL question file; blank lines and `#` comments are skipped.
pub fn parse_questions(text: &str) -> Result<Vec<Question>, String> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let q: Question =
            serde_json::from_str(line).map_err(|e| format!("第 {} 行格式錯誤：{e}", i + 1))?;
        if q.answerable && q.gold.is_empty() {
            return Err(format!(
                "第 {} 行（{}）標為可回答但沒有 gold。",
                i + 1,
                q.id
            ));
        }
        out.push(q);
    }
    Ok(out)
}

fn nfkc(s: &str) -> String {
    s.nfkc().collect::<String>().to_lowercase()
}

fn within(v: u32, start: Option<u32>, end: Option<u32>) -> bool {
    match start {
        Some(s) => v >= s && v <= end.unwrap_or(s),
        None => false,
    }
}

/// Does `hit` satisfy `gold`?
pub fn hit_matches(hit: &Hit, gold: &Gold) -> bool {
    if nfkc(&hit.title) != nfkc(&gold.source) {
        return false;
    }
    let l = &hit.locator;
    // Every position constraint that is given must hold.
    let position_ok = gold
        .page
        .is_none_or(|p| l.kind == LocatorKind::Page && within(p, l.page_start, l.page_end))
        && gold
            .slide
            .is_none_or(|n| l.kind == LocatorKind::Slide && within(n, l.page_start, l.page_end))
        && gold
            .sheet
            .as_deref()
            .is_none_or(|sheet| l.sheet.as_deref() == Some(sheet))
        && gold
            .row
            .is_none_or(|r| l.kind == LocatorKind::SheetRows && within(r, l.row_start, l.row_end));
    let contains_ok = gold.contains.as_deref().is_none_or(|needle| {
        let hay = nfkc(&format!("{}\n{}", hit.heading_path, hit.text));
        hay.contains(&nfkc(needle))
    });
    position_ok && contains_ok
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TypeStats {
    pub questions: usize,
    pub recall_at_1: f64,
    pub recall_at_5: f64,
    pub recall_at_10: f64,
    pub mrr_at_10: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModeReport {
    pub mode: String,
    pub answerable: TypeStats,
    pub by_type: BTreeMap<String, TypeStats>,
    /// Unanswerable questions: precision / recall of "no result".
    pub unanswerable_questions: usize,
    pub no_result_precision: Option<f64>,
    pub no_result_recall: Option<f64>,
    pub query_p50_ms: f64,
    pub query_p95_ms: f64,
    /// Ids of answerable questions with no gold hit in the top 10.
    pub misses: Vec<String>,
}

#[derive(Default)]
struct Acc {
    n: usize,
    r1: usize,
    r5: usize,
    r10: usize,
    rr: f64,
}

impl Acc {
    fn add(&mut self, rank: Option<usize>) {
        self.n += 1;
        if let Some(r) = rank {
            self.r1 += (r <= 1) as usize;
            self.r5 += (r <= 5) as usize;
            self.r10 += (r <= 10) as usize;
            if r <= 10 {
                self.rr += 1.0 / r as f64;
            }
        }
    }

    fn stats(&self) -> TypeStats {
        let d = self.n.max(1) as f64;
        TypeStats {
            questions: self.n,
            recall_at_1: self.r1 as f64 / d,
            recall_at_5: self.r5 as f64 / d,
            recall_at_10: self.r10 as f64 / d,
            mrr_at_10: self.rr / d,
        }
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let idx = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

/// Evaluate `search` (top-10 hits and the time it took) over `questions`.
pub fn evaluate<F>(mode: &str, questions: &[Question], mut search: F) -> ModeReport
where
    F: FnMut(&str) -> (Vec<Hit>, Duration),
{
    let mut all = Acc::default();
    let mut by_type: BTreeMap<String, Acc> = BTreeMap::new();
    let mut times = Vec::new();
    let (mut unans, mut tp, mut predicted_empty) = (0usize, 0usize, 0usize);
    let mut misses = Vec::new();
    for q in questions {
        let (hits, took) = search(&q.question);
        times.push(took.as_secs_f64() * 1000.0);
        if hits.is_empty() {
            predicted_empty += 1;
        }
        if !q.answerable {
            unans += 1;
            tp += hits.is_empty() as usize;
            continue;
        }
        let rank = hits
            .iter()
            .take(10)
            .position(|h| q.gold.iter().any(|g| hit_matches(h, g)))
            .map(|p| p + 1);
        if rank.is_none() {
            misses.push(q.id.clone());
        }
        all.add(rank);
        by_type
            .entry(if q.qtype.is_empty() {
                "untyped".into()
            } else {
                q.qtype.clone()
            })
            .or_default()
            .add(rank);
    }
    times.sort_by(f64::total_cmp);
    ModeReport {
        mode: mode.to_string(),
        answerable: all.stats(),
        by_type: by_type.into_iter().map(|(k, v)| (k, v.stats())).collect(),
        unanswerable_questions: unans,
        no_result_precision: (predicted_empty > 0).then(|| tp as f64 / predicted_empty as f64),
        no_result_recall: (unans > 0).then(|| tp as f64 / unans as f64),
        query_p50_ms: percentile(&times, 0.5),
        query_p95_ms: percentile(&times, 0.95),
        misses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locator::Locator;

    fn hit(title: &str, page: u32, text: &str) -> Hit {
        let mut l = Locator::new(LocatorKind::Page);
        l.page_start = Some(page);
        Hit {
            chunk_id: page as i64,
            source_id: "s".into(),
            collection_id: "c".into(),
            title: title.into(),
            locator_text: l.display(),
            locator: l,
            heading_path: String::new(),
            text: text.into(),
            score: 1.0,
            fts_rank: Some(1),
            like_rank: None,
            vec_rank: None,
            vec_score: None,
            suspect: false,
        }
    }

    #[test]
    fn metrics() {
        let qs = parse_questions(
            r#"{"id":"q1","question":"特休","type":"factoid","gold":[{"source":"手冊.pdf","page":2,"contains":"七日"}]}
# comment
{"id":"q2","question":"宿舍","type":"unanswerable","answerable":false}
{"id":"q3","question":"加班","type":"factoid","gold":[{"source":"手冊.pdf","page":9}]}"#,
        )
        .unwrap();
        let report = evaluate("keyword", &qs, |q| {
            let hits = match q {
                "特休" => vec![hit("手冊.pdf", 1, "無關"), hit("手冊.PDF", 2, "給予七日")],
                "加班" => vec![hit("手冊.pdf", 3, "加班費")],
                _ => vec![],
            };
            (hits, Duration::from_millis(3))
        });
        assert_eq!(report.answerable.questions, 2);
        assert_eq!(report.answerable.recall_at_1, 0.0);
        assert_eq!(report.answerable.recall_at_5, 0.5);
        assert_eq!(report.answerable.mrr_at_10, 0.25);
        assert_eq!(report.misses, vec!["q3".to_string()]);
        assert_eq!(report.no_result_recall, Some(1.0));
        assert_eq!(report.no_result_precision, Some(1.0));
    }

    #[test]
    fn bad_lines_are_reported() {
        assert!(parse_questions("{\"id\":\"x\"}").is_err());
        assert!(
            parse_questions("{\"id\":\"x\",\"question\":\"q\"}").is_err(),
            "answerable without gold"
        );
    }
}
