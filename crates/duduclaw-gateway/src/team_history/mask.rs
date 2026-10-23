//! Read-time, non-reversible masking for external transcripts.

use crate::session::SessionMessage;

#[path = "mask_normalization.rs"]
mod normalization;
use duduclaw_redaction::{
    Source,
    config::RedactionConfig,
    engine::{EngineOptions, RuleEngine},
    manager::{ManagerPaths, resolve_data_sources, resolve_rule_specs},
    ner::{InstallDirs, runtime::NerEngine},
    rules::RuleKind,
};
use regex::Regex;
use serde::Serialize;
use std::{collections::BTreeMap, path::Path};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Part {
    Text {
        v: String,
    },
    Mask {
        cat: String,
        n: u32,
    },
    Attachment {
        kind: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct Message {
    pub role: String,
    pub sender_label: String,
    pub parts: Vec<Part>,
    pub timestamp: String,
}

#[derive(Debug, Serialize)]
pub struct Transcript {
    pub messages: Vec<Message>,
    pub masked_counts: BTreeMap<String, u32>,
    pub ner: bool,
}

pub struct ViewMasker {
    engine: RuleEngine,
    ner: Option<NerEngine>,
    max_chars: usize,
}

struct SensitiveSpan {
    start: usize,
    end: usize,
    cat: String,
    // A large lower-priority match can be split into many pieces. Never
    // clone its whole sensitive value per piece or compare long values in
    // the numbering map. This digest is salted per transcript, stays in
    // memory, and is never serialized.
    key: [u8; 32],
    priority: i32,
}

/// The permit lives in the blocking closure, even after timeout. A wedged
/// installed model cannot accumulate unlimited detached inference workers.
pub async fn external(
    home: std::path::PathBuf,
    rows: Vec<SessionMessage>,
    labels: BTreeMap<String, String>,
    agent: String,
) -> Result<Transcript, String> {
    use std::sync::{Arc, OnceLock};
    static ADMISSION: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let admission = ADMISSION
        .get_or_init(|| Arc::new(tokio::sync::Semaphore::new(8)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| "masking busy")?;
    static SLOT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    let permit = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        SLOT.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
            .acquire_owned(),
    )
    .await
    .map_err(|_| "masking admission timeout")?
    .map_err(|_| "masking busy")?;
    let chars: usize = rows.iter().map(|r| r.content.chars().count()).sum();
    let seconds = ((chars.saturating_add(999) / 1000).max(1) * 3).min(120) as u64;
    let job = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _admission = admission;
        ViewMasker::for_home(&home)?.mask_transcript(&rows, &labels, &agent)
    });
    tokio::time::timeout(std::time::Duration::from_secs(seconds), job)
        .await
        .map_err(|_| "masking timeout")?
        .map_err(|_| "masking worker failed")?
}

impl ViewMasker {
    pub fn for_home(home: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(home.join("config.toml")).map_err(|e| e.to_string())?;
        let table = raw.parse::<toml::Table>().map_err(|e| e.to_string())?;
        let config: RedactionConfig = table
            .get("redaction")
            .cloned()
            .unwrap_or_else(|| toml::Value::Table(Default::default()))
            .try_into()
            .map_err(|e: toml::de::Error| e.to_string())?;
        config.validate().map_err(|e| e.to_string())?;
        let paths = ManagerPaths::under_home(home);
        let mut specs = resolve_rule_specs(&config, &paths).map_err(|e| e.to_string())?;
        // Operator rules may supplement, but cannot override mandatory rules.
        for profile in ["taiwan_strict", "financial", "general"] {
            let bundle = duduclaw_redaction::profiles::load_builtin(profile)
                .map_err(|e| e.to_string())?
                .ok_or("mandatory masking profile missing")?;
            for mut spec in bundle.into_specs() {
                spec.id = format!("team_history_mandatory_{}_{}", profile, spec.id);
                spec.enabled = true;
                // ASCII identifiers adjacent to CJK still have boundaries.
                // Only this view's mandatory copies change; model-side
                // profiles retain their existing matching semantics.
                if let RuleKind::Regex { pattern } = &mut spec.kind {
                    *pattern = pattern.replace(r"\b", r"(?-u:\b)");
                }
                specs.push(spec);
            }
        }
        // NerRule deliberately swallows inference errors. Never use it on
        // this disclosure boundary: call NerEngine::spans explicitly below.
        specs.retain(|s| !matches!(s.kind, RuleKind::Ner { .. }));
        let engine = RuleEngine::from_specs_with(
            specs,
            &EngineOptions {
                identity_people_dir: paths.identity_people_dir,
                data_sources: resolve_data_sources(&config).map_err(|e| e.to_string())?,
                ..Default::default()
            },
        )
        .map_err(|e| e.to_string())?;
        let mut dirs = InstallDirs::under_home(home);
        dirs.model_dir = config.ner.resolve_model_dir(home);
        // NerEngine's shared registry holds Weak references. Keep one view
        // model alive between requests instead of loading 1.7 GiB for every
        // list title. Revalidate the installation on every open; no cached
        // engine can hide a partial/uninstalled model. Config changes drop
        // this strong reference before opening the replacement.
        use sha2::Digest;
        type RetainedModel = Option<(std::path::PathBuf, String, NerEngine)>;
        static MODEL: std::sync::OnceLock<std::sync::Mutex<RetainedModel>> =
            std::sync::OnceLock::new();
        let mut retained = MODEL
            .get_or_init(Default::default)
            .lock()
            .map_err(|_| "mask model lock poisoned")?;
        let hash = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
        if retained
            .as_ref()
            .is_some_and(|(path, key, _)| path != home || *key != hash)
        {
            *retained = None;
        }
        // An absent model is explicitly reported as regex-only. A partial
        // install, stale marker, corrupt model or unreadable directory fails.
        let ner = match std::fs::metadata(&dirs.model_dir) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.to_string()),
            Ok(_) => Some(NerEngine::open(&dirs, &config.ner).map_err(|e| e.to_string())?),
        };
        *retained = ner
            .as_ref()
            .map(|engine| (home.to_path_buf(), hash, engine.clone()));
        Ok(Self {
            engine,
            ner,
            max_chars: config.ner.sanitized().max_chars,
        })
    }

    pub fn mask_transcript(
        &self,
        rows: &[SessionMessage],
        labels: &BTreeMap<String, String>,
        agent: &str,
    ) -> Result<Transcript, String> {
        render(rows, labels, agent, Some(self))
    }

    fn matches(&self, text: &str) -> Vec<duduclaw_redaction::engine::MatchedSpan> {
        let mut hits = self.engine.all_text_matches(
            text,
            &Source::ToolResult {
                tool_name: "team_history".into(),
            },
        );
        // Config maps and identity dictionaries need not iterate in a stable
        // order. Equal-priority labels must still be identical on refresh;
        // never use the freshly salted value digest as a tie-breaker.
        hits.sort_by(|a, b| {
            a.span
                .start
                .cmp(&b.span.start)
                .then_with(|| a.rule.id().cmp(b.rule.id()))
                .then_with(|| b.span.end.cmp(&a.span.end))
        });
        hits
    }

    fn spans(&self, text: &str, salt: &[u8; 32]) -> Result<Vec<SensitiveSpan>, String> {
        let mut spans: Vec<_> = self
            .matches(text)
            .into_iter()
            .map(|m| {
                let cat = category(m.rule.category()).to_string();
                SensitiveSpan {
                    start: m.span.start,
                    end: m.span.end,
                    key: identity_key(&cat, &text[m.span.start..m.span.end], salt),
                    cat,
                    priority: m.rule.priority(),
                }
            })
            .collect();
        for (pattern, cat) in [
            (r"<REDACT:[^>\r\n]*(?:>|$)", "REDACTED"),
            (
                r"(?-u:\b)(?:[UCR][0-9a-fA-F]{32}|[UWBCGT][A-Z0-9]{7,63}|[0-9]{7,64}|(?:ou|oc|on)_[A-Za-z0-9]{16,64}|[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12})(?-u:\b)",
                "PLATFORM_ID",
            ),
        ] {
            for m in Regex::new(pattern)
                .map_err(|e| e.to_string())?
                .find_iter(text)
            {
                spans.push(SensitiveSpan {
                    start: m.start(),
                    end: m.end(),
                    cat: cat.into(),
                    key: identity_key(cat, m.as_str(), salt),
                    priority: if cat == "REDACTED" { 1000 } else { 10 },
                });
            }
        }
        if let Some(ner) = &self.ner {
            for span in ner
                .spans(text, self.max_chars)
                .map_err(|e| e.to_string())?
                .iter()
            {
                if let Some(cat) = duduclaw_redaction::ner::labels::category_for(&span.label) {
                    if span.start >= span.end
                        || !text.is_char_boundary(span.start)
                        || !text.is_char_boundary(span.end)
                    {
                        return Err("invalid NER span".into());
                    }
                    let cat = category(cat).to_string();
                    spans.push(SensitiveSpan {
                        start: span.start,
                        end: span.end,
                        key: identity_key(&cat, &text[span.start..span.end], salt),
                        cat,
                        priority: 90,
                    });
                }
            }
        }
        Ok(cover_union(spans))
    }
}

/// Sweep all raw hits in O(n log n). Priority chooses only the placeholder
/// label for a covered byte; it can never drop the remaining coverage of a
/// lower-priority hit. Keys use the whole original hit, so a split phone
/// value still has the same number on every piece and every message.
fn cover_union(spans: Vec<SensitiveSpan>) -> Vec<SensitiveSpan> {
    use std::collections::BTreeSet;
    let mut edges = BTreeMap::<usize, (Vec<usize>, Vec<usize>)>::new();
    for (i, s) in spans.iter().enumerate() {
        if s.start < s.end {
            edges.entry(s.start).or_default().0.push(i);
            edges.entry(s.end).or_default().1.push(i);
        }
    }
    let positions: Vec<_> = edges.keys().copied().collect();
    let mut active = BTreeSet::new();
    let mut out: Vec<SensitiveSpan> = Vec::new();
    for pair in positions.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        let (add, remove) = &edges[&start];
        for i in remove {
            active.remove(&(spans[*i].priority, std::cmp::Reverse(*i)));
        }
        for i in add {
            active.insert((spans[*i].priority, std::cmp::Reverse(*i)));
        }
        let Some((_, std::cmp::Reverse(i))) = active.last() else {
            continue;
        };
        let winner = &spans[*i];
        if let Some(last) = out
            .last_mut()
            .filter(|s| s.end == start && s.cat == winner.cat && s.key == winner.key)
        {
            last.end = end;
        } else {
            out.push(SensitiveSpan {
                start,
                end,
                cat: winner.cat.clone(),
                key: winner.key,
                priority: winner.priority,
            });
        }
    }
    out
}

fn category(cat: &str) -> &str {
    match cat {
        "TW_MOBILE" | "TW_LANDLINE" | "PHONE_NUMBER" | "PHONE" => "PHONE",
        "PERSON" | "PERSON_NAME" => "NAME",
        _ => cat,
    }
}

fn identity_key(cat: &str, value: &str, salt: &[u8; 32]) -> [u8; 32] {
    use sha2::Digest;
    let canonical: String = value
        .nfkc()
        .filter(|c| !c.is_whitespace() && (cat != "PHONE" || c.is_ascii_digit()))
        .collect();
    let mut hash = sha2::Sha256::new();
    hash.update(salt);
    hash.update(canonical.as_bytes());
    hash.finalize().into()
}

/// Full-text classes use the same host-marker and attachment removal, but
/// never expose paths, links, hidden rows or executable confirmation tags.
pub fn full_transcript(
    rows: &[SessionMessage],
    user_label: &str,
    agent: &str,
) -> Result<Transcript, String> {
    let mut out = render(rows, &BTreeMap::new(), agent, None)?;
    for m in &mut out.messages {
        if m.role == "user" {
            m.sender_label = user_label.to_owned();
        }
    }
    Ok(out)
}

fn render(
    rows: &[SessionMessage],
    labels: &BTreeMap<String, String>,
    agent: &str,
    masker: Option<&ViewMasker>,
) -> Result<Transcript, String> {
    let mut out = Transcript {
        messages: Vec::new(),
        masked_counts: BTreeMap::new(),
        ner: masker.is_some_and(|m| m.ner.is_some()),
    };
    let mut senders = BTreeMap::<String, u32>::new();
    let mut numbers = BTreeMap::<(String, [u8; 32]), u32>::new();
    let mut salt = [0u8; 32];
    if masker.is_some() {
        use rand::RngCore;
        rand::rngs::OsRng
            .try_fill_bytes(&mut salt)
            .map_err(|_| "mask identity randomness unavailable")?;
    }
    let mut next = BTreeMap::<String, u32>::new();
    // A destination may contain nested/escaped parentheses. Conservatively
    // replace the entire attachment-bearing line rather than expose a path
    // suffix after a naive first-')' parse. Ordinary text on that line is
    // deliberately withheld too.
    let attachments = Regex::new(r"(?mi)^[^\r\n]*(?:📎(?:DELIVER:|\s*已生成檔案[：:])|!?\[[^\]\r\n]*\]\(|file://|attachment://|<img\b)[^\r\n]*$").map_err(|e| e.to_string())?;
    for row in rows
        .iter()
        .filter(|r| r.role == "user" || r.role == "assistant")
    {
        let sender = row
            .content
            .strip_prefix("[sender_id: ")
            .and_then(|s| s.split_once("]\n"))
            .map(|(id, _)| id)
            .unwrap_or("");
        let sender_label = if row.role == "assistant" {
            agent.to_owned()
        } else if let Some(label) = labels.get(sender) {
            label.clone()
        } else {
            let count = senders.len() as u32 + 1;
            format!("客戶 {}", senders.entry(sender.to_owned()).or_insert(count))
        };
        let stripped = crate::channel_reply::strip_sender_prefix(&row.content);
        let (clean, _) = crate::os_operator::strip_system_operator_pending_tag(stripped);
        let clean = clean
            .split_once("<system_operator_pending>")
            .map_or(clean.as_str(), |(before, _)| before);
        let mut parts = Vec::new();
        let mut at = 0;
        for attachment in attachments.find_iter(clean) {
            append_text(
                &clean[at..attachment.start()],
                masker,
                &mut parts,
                &mut numbers,
                &mut next,
                &mut out.masked_counts,
                &salt,
            )?;
            let raw = attachment.as_str();
            let kind = if raw.contains("![") || raw.to_ascii_lowercase().contains("<img") {
                "image"
            } else {
                "file"
            };
            let name = if masker.is_none() {
                attachment_name(raw)
            } else {
                None
            };
            parts.push(Part::Attachment {
                kind: kind.into(),
                name,
            });
            at = attachment.end();
        }
        append_text(
            &clean[at..],
            masker,
            &mut parts,
            &mut numbers,
            &mut next,
            &mut out.masked_counts,
            &salt,
        )?;
        // Truncate only complete parts after all sensitive spans are removed.
        let mut chars = 0;
        parts.retain_mut(|part| {
            if chars >= 20_000 {
                return false;
            }
            match part {
                Part::Text { v } => {
                    *v = v.chars().take(20_000 - chars).collect();
                    chars += v.chars().count();
                }
                _ => chars += 1,
            }
            true
        });
        out.messages.push(Message {
            role: row.role.clone(),
            sender_label,
            parts,
            timestamp: row.timestamp.clone(),
        });
    }
    recount(&mut out);
    Ok(out)
}

pub fn recount(transcript: &mut Transcript) {
    transcript.masked_counts.clear();
    for message in &transcript.messages {
        for part in &message.parts {
            if let Part::Mask { cat, .. } = part {
                *transcript.masked_counts.entry(cat.clone()).or_default() += 1;
            }
        }
    }
}

fn attachment_name(raw: &str) -> Option<String> {
    let path = raw
        .split_once("📎DELIVER:")
        .map(|(_, p)| p)
        .or_else(|| raw.split_once("已生成檔案：").map(|(_, p)| p))
        .or_else(|| raw.split_once("](").map(|(_, p)| p.trim_end_matches(')')))?;
    let base = path
        .trim()
        .split(['?', '#'])
        .next()?
        .rsplit(['/', '\\'])
        .next()?;
    (!base.is_empty()).then(|| base.chars().filter(|c| !c.is_control()).take(200).collect())
}

fn append_text(
    text: &str,
    masker: Option<&ViewMasker>,
    parts: &mut Vec<Part>,
    numbers: &mut BTreeMap<(String, [u8; 32]), u32>,
    next: &mut BTreeMap<String, u32>,
    counts: &mut BTreeMap<String, u32>,
    salt: &[u8; 32],
) -> Result<(), String> {
    if masker.is_none() {
        let safe = Regex::new(r"<REDACT:[^>\r\n]*(?:>|$)")
            .map_err(|e| e.to_string())?
            .replace_all(text, "〔已遮蔽〕");
        if !safe.is_empty() {
            parts.push(Part::Text {
                v: safe.into_owned(),
            });
        }
        return Ok(());
    }
    // Return normalized masked text, keeping all offsets in the same byte
    // string. Full-width ASCII PII cannot bypass a literal regex prefix.
    let original_text = text;
    let normalized_text: String = text.nfkc().collect();
    let text = normalized_text.as_str();
    let spans = match masker {
        Some(m) => m.spans(text, salt)?,
        None => Vec::new(),
    };
    let spans = match masker {
        Some(masker) if text != original_text => {
            normalization::preserve_original_hits(masker, original_text, text, spans, salt)
        }
        _ => spans,
    };
    let mut at = 0;
    for SensitiveSpan {
        start,
        end,
        cat,
        key,
        ..
    } in spans
    {
        if start > at {
            parts.push(Part::Text {
                v: text[at..start].into(),
            });
        }
        let key = (cat.clone(), key);
        let n = *numbers.entry(key).or_insert_with(|| {
            let n = next.entry(cat.clone()).or_default();
            *n += 1;
            *n
        });
        *counts.entry(cat.clone()).or_default() += 1;
        parts.push(Part::Mask { cat, n });
        at = end;
    }
    if at < text.len() {
        parts.push(Part::Text {
            v: text[at..].into(),
        });
    }
    Ok(())
}

#[cfg(test)]
#[path = "mask_tests.rs"]
mod tests;
