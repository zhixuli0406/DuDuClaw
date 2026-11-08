//! Mandatory masking of a task title before it leaves the building (RM-064).
//!
//! A title is written by a person or an AI employee and can name anyone or
//! anything — including people no pattern knows (「拜訪王小明確認續約」).
//! Owner decision: a title is returned **only** when the product's on-device
//! PII model (the redaction crate's NER, OpenAI Privacy Filter via ONNX
//! Runtime) is installed, opens, and actually runs on this title. Without it
//! the title is `None` and only `completed_at` is sent.
//!
//! Pipeline:
//!
//! 1. Unicode normalisation with the project's
//!    [`UnicodeNormalizer`]: invisible / format / bidi characters
//!    (zero-width, word joiner and invisible operators, BOM, bidi embeddings,
//!    overrides, isolates and marks, variation selectors, tags, fillers) are
//!    removed, then NFKC, then removed again. A token (whitespace / CJK
//!    separated) mixing confusable scripts — a Cyrillic `а` inside a Latin
//!    e-mail — omits the title;
//! 2. credentials are masked (`duduclaw_security::credential_detect`);
//! 3. personal data is masked by three independent detectors whose spans are
//!    unioned (never priority-resolved — a lower-priority partial overlap
//!    would otherwise expose the rest of a value):
//!    - the on-device NER model ([`TitleNer`]): every entity it reports
//!      (person, address, e-mail, phone, URL, date, account number, secret)
//!      is masked. An inference error, an unknown label, a span off a
//!      character boundary or a scored entity under [`MIN_NER_CONFIDENCE`]
//!      omits the title;
//!    - the project's redaction rule engine, built synchronously from the
//!      built-in `general` / `taiwan_strict` / `financial` profiles, plus the
//!      wiki identity directory (known people's display names) when present;
//!    - a focused masker for what those patterns miss in real titles: e‑mail
//!      without word boundaries next to CJK, TW national / resident IDs glued
//!      to CJK, international and TW phone numbers in any spacing, digit runs
//!      of 8 or more digits (account, customer, card, order numbers),
//!      identifiers after a label (「帳號」「客戶編號」「卡號」 `account` …),
//!      and honorific-marked names (「王先生」, `Mr. Smith`).
//!
//!    All run on a copy where the remaining full-width forms, the
//!    ideographic space and dash variants are folded to ASCII one character
//!    for one character, so offsets map straight back;
//! 4. every masked span becomes [`MASK`] (fixed width: the length of what
//!    was there is not disclosed);
//! 5. the truncated result is scanned again by all of the above, the model
//!    included; anything still detected — or any failure anywhere (model
//!    missing or failing, rule compile error, oversized input, a panic) —
//!    omits the title (`None`) instead of returning it.

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use duduclaw_redaction::ner::runtime::NerEngine;
use duduclaw_redaction::ner::{InstallDirs, NerConfig};
use duduclaw_redaction::{EngineOptions, RestoreScope, RuleEngine, RuleKind, RuleSpec, Source};
use duduclaw_security::unicode_normalizer::UnicodeNormalizer;
use regex::Regex;

/// What a masked span becomes.
pub(crate) const MASK: &str = "＊＊＊";
/// Longest raw title considered at all (bytes). Longer ⇒ omitted, so a
/// truncation can never cut a value in half before it is masked.
pub(crate) const MAX_SCAN_BYTES: usize = 4096;
/// A detector that scores its entities must reach this for every one of
/// them; anything less (or a NaN) omits the title rather than guessing
/// whether the uncertain span was personal data. The privacy-filter decoder
/// (constrained Viterbi) reports no per-span score, so its entities carry
/// none and are all masked.
pub(crate) const MIN_NER_CONFIDENCE: f32 = 0.5;
/// Built-in redaction profiles applied to every external title.
const PROFILES: [&str; 3] = ["general", "taiwan_strict", "financial"];
/// Profile rules not applied here, each replaced or not personal data:
/// - `swift_bic` names a bank, not a person, and matches any 8- or 11-letter
///   upper-case word;
/// - `email` uses Unicode `\w`, so in a CJK title it swallows the whole
///   sentence around the address; the focused e-mail rules below cover it.
const SKIPPED_RULES: [&str; 2] = ["swift_bic", "email"];
/// Distinct homes whose engine is kept (one in production).
const MAX_CACHED_ENGINES: usize = 8;

/// Mask, flatten and truncate `raw` to at most `max_chars` characters, or
/// `None` when it cannot be shown safely — always `None` while the on-device
/// PII model is not installed or cannot be opened.
pub(crate) fn external_title(home: &Path, raw: &str, max_chars: usize) -> Option<String> {
    std::panic::catch_unwind(AssertUnwindSafe(|| {
        let ner = match available_ner(home) {
            Ok(ner) => ner,
            Err(e) => {
                tracing::debug!(error = %e, "external status: on-device PII model unavailable, title omitted");
                return None;
            }
        };
        external_title_inner(home, ner.as_ref(), raw, max_chars)
    }))
    .ok()
    .flatten()
}

/// [`external_title`] with an explicit detector (tests).
#[cfg(test)]
pub(crate) fn external_title_with(
    home: &Path,
    ner: &dyn TitleNer,
    raw: &str,
    max_chars: usize,
) -> Option<String> {
    std::panic::catch_unwind(AssertUnwindSafe(|| external_title_inner(home, ner, raw, max_chars)))
        .ok()
        .flatten()
}

fn external_title_inner(
    home: &Path,
    ner: &dyn TitleNer,
    raw: &str,
    max_chars: usize,
) -> Option<String> {
    if raw.len() > MAX_SCAN_BYTES {
        return None;
    }
    let normalized = normalize(raw)?;
    let engine = engine_for(home).ok()?;
    let credentials_masked =
        duduclaw_security::credential_detect::mask_document(&normalized).ok()?;
    let masked = mask_pii(&engine, ner, &credentials_masked).ok()?;
    let flat: String = masked
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let out = duduclaw_core::truncate_chars(flat.trim(), max_chars);
    // Second look at exactly what would be sent, the model included.
    match pii_spans(&engine, ner, &out) {
        Ok(spans) if spans.is_empty() => {}
        _ => return None,
    }
    match duduclaw_security::credential_detect::detect(&out) {
        Ok(findings) if findings.is_empty() => Some(out),
        _ => None,
    }
}

// ── On-device NER ───────────────────────────────────────────────

/// One entity reported by the on-device model, as byte offsets into the
/// text it was given.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NerEntity {
    pub start: usize,
    pub end: usize,
    /// Model label (`private_person`, …), see
    /// `duduclaw_redaction::ner::labels`.
    pub label: String,
    /// `None` when the detector does not score spans.
    pub confidence: Option<f32>,
}

/// The on-device PII / NER detector a title must pass. Production: the
/// redaction crate's privacy-filter model ([`ModelNer`]); tests: a double.
pub(crate) trait TitleNer: Send + Sync {
    /// Every entity in `text`. An `Err` means the text was not scanned.
    fn entities(&self, text: &str) -> Result<Vec<NerEntity>, String>;
}

/// The privacy-filter model, called directly: `NerRule` swallows inference
/// errors (and skips short text), which this boundary must not.
struct ModelNer {
    engine: NerEngine,
    max_chars: usize,
}

impl TitleNer for ModelNer {
    fn entities(&self, text: &str) -> Result<Vec<NerEntity>, String> {
        let spans = self.engine.spans(text, self.max_chars).map_err(|e| e.to_string())?;
        Ok(spans
            .iter()
            .map(|s| NerEntity {
                start: s.start,
                end: s.end,
                label: s.label.clone(),
                confidence: None,
            })
            .collect())
    }
}

/// One strong reference so the engine (the registry holds `Weak`s) is not
/// rebuilt per request; its own idle watchdog still unloads the session.
static RETAINED_MODEL: LazyLock<Mutex<Option<NerEngine>>> = LazyLock::new(|| Mutex::new(None));

/// `config.toml [redaction.ner]`; absent ⇒ defaults, unreadable ⇒ error.
fn ner_config(home: &Path) -> Result<NerConfig, String> {
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(NerConfig::default()),
        Err(e) => return Err(e.to_string()),
    };
    let table: toml::Table = raw.parse().map_err(|e: toml::de::Error| e.to_string())?;
    match table.get("redaction").and_then(|r| r.get("ner")) {
        None => Ok(NerConfig::default()),
        Some(v) => v.clone().try_into().map_err(|e: toml::de::Error| e.to_string()),
    }
}

/// Open the installed model. `NerEngine::open` re-validates the install
/// (marker, revision, file sizes, ONNX Runtime) on every call and refuses
/// under an isolated root key; the session itself loads on first use, so a
/// model that cannot load fails the first [`TitleNer::entities`] call.
fn model_ner(home: &Path) -> Result<ModelNer, String> {
    let config = ner_config(home)?.sanitized();
    let mut dirs = InstallDirs::under_home(home);
    dirs.model_dir = config.resolve_model_dir(home);
    let engine = NerEngine::open(&dirs, &config).map_err(|e| e.to_string())?;
    if let Ok(mut retained) = RETAINED_MODEL.lock() {
        *retained = Some(engine.clone());
    }
    Ok(ModelNer { engine, max_chars: config.max_chars })
}

fn available_ner(home: &Path) -> Result<Arc<dyn TitleNer>, String> {
    #[cfg(test)]
    let registered = test_ner::registered(home);
    #[cfg(not(test))]
    let registered: Option<Arc<dyn TitleNer>> = None;
    if let Some(ner) = registered {
        return Ok(ner);
    }
    Ok(Arc::new(model_ner(home)?))
}

// ── Unicode normalisation ───────────────────────────────────────

/// Invisible characters [`UnicodeNormalizer::strip_invisible_chars`] does not
/// already cover: every remaining `Cf` format character (invisible operators
/// U+2061–U+2064, Arabic letter mark, Mongolian vowel separator, …),
/// Hangul / Khmer fillers and the variation-selector / tag planes.
fn is_hidden(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x0600..=0x0605
            | 0x061C
            | 0x06DD
            | 0x070F
            | 0x0890..=0x0891
            | 0x08E2
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFFB
            | 0x110BD
            | 0x110CD
            | 0x13430..=0x1343F
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

fn strip_hidden(text: &str) -> String {
    let (stripped, _) = UnicodeNormalizer::strip_invisible_chars(text);
    stripped.chars().filter(|c| !is_hidden(*c)).collect()
}

/// Hidden characters removed, NFKC, removed again; `None` when the result
/// is too long or a token mixes confusable scripts.
fn normalize(raw: &str) -> Option<String> {
    let nfkc = UnicodeNormalizer::normalize_nfkc(&strip_hidden(raw));
    let clean = strip_hidden(&nfkc);
    if clean.len() > MAX_SCAN_BYTES * 2 || has_mixed_script_token(&clean).unwrap_or(true) {
        return None;
    }
    Some(clean)
}

/// Does any token mix confusable scripts (Latin with Cyrillic / Greek /
/// Armenian / Georgian)? Tokens are split on whitespace and on CJK, so a
/// Latin word next to Han text is not itself a mix.
fn has_mixed_script_token(text: &str) -> Result<bool, String> {
    static SEPARATORS: LazyLock<Result<Regex, String>> = LazyLock::new(|| {
        Regex::new(r"[\s\p{Han}\p{Hiragana}\p{Katakana}\p{Hangul}\p{Bopomofo}]+")
            .map_err(|e| format!("title masker: {e}"))
    });
    let separators = SEPARATORS.as_ref().map_err(Clone::clone)?;
    Ok(separators
        .split(text)
        .any(|token| !UnicodeNormalizer::detect_mixed_script(token).is_empty()))
}

// ── Folding ─────────────────────────────────────────────────────

/// One character for one character: full-width ASCII, ideographic space and
/// dash variants become their ASCII form; everything else is unchanged.
fn fold_char(c: char) -> char {
    match c {
        '\u{FF01}'..='\u{FF5E}' => char::from_u32(c as u32 - 0xFEE0).unwrap_or(c),
        '\u{3000}' => ' ',
        '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{FE58}' | '\u{FE63}' => '-',
        _ => c,
    }
}

/// The folded text plus, per character, `(folded byte offset, original byte
/// offset)`, with a sentinel for the end of both strings.
fn fold(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut folded = String::with_capacity(text.len());
    let mut offsets = Vec::with_capacity(text.len() + 1);
    for (orig, c) in text.char_indices() {
        offsets.push((folded.len(), orig));
        folded.push(fold_char(c));
    }
    offsets.push((folded.len(), text.len()));
    (folded, offsets)
}

fn to_original(offsets: &[(usize, usize)], folded_byte: usize) -> Result<usize, String> {
    offsets
        .binary_search_by_key(&folded_byte, |(f, _)| *f)
        .map(|i| offsets[i].1)
        .map_err(|_| "masked span off a character boundary".to_string())
}

// ── Detection ───────────────────────────────────────────────────

struct Detectors {
    /// Spans masked as a whole.
    whole: Vec<Regex>,
    /// Spans whose capture group 1 is masked (labels / honorifics stay).
    group: Vec<Regex>,
    /// Digit-run candidates, judged by [`digit_run_is_sensitive`].
    digits: Regex,
}

fn detectors() -> Result<&'static Detectors, String> {
    static DETECTORS: LazyLock<Result<Detectors, String>> = LazyLock::new(|| {
        let compile = |p: &str| Regex::new(p).map_err(|e| format!("title masker: {e}"));
        Ok(Detectors {
            whole: vec![
                // E-mail, ASCII local part (CJK around it stays readable).
                compile(r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+")?,
                // ...and a non-ASCII local part (「王小明@example.com」): up
                // to four characters before the `@`, enough for a name.
                compile(r"[\p{L}\p{N}._%+\-]{1,4}@[A-Za-z0-9\-]+(?:\.[A-Za-z0-9\-]+)+")?,
                // TW national ID / resident certificate (old and new), no
                // word boundary: CJK glued on either side is the common case.
                compile(r"(?i)[A-Z][12ABCD89]\d{8}")?,
            ],
            group: vec![
                // Labelled identifiers (CJK labels need no boundary).
                compile(
                    r"(?i)(?:帳號|帳戶|戶號|客戶編號|客編|客戶代號|會員編號|會員號碼|會員號|訂單編號|訂單號碼|保單號碼|保單編號|合約編號|卡號|信用卡|身分證(?:字號|號碼)?|身份證(?:字號|號碼)?|居留證(?:號碼)?|護照(?:號碼)?|統一編號|統編|電話|手機|分機|傳真|信箱|郵件|e-?mail)\s*(?:是|為|:|=|#|\.)?\s*([A-Za-z0-9][A-Za-z0-9\-_.@]{3,})",
                )?,
                compile(
                    r"(?i)\b(?:account(?:\s*(?:no|number|id))?|acct|a/c|customer\s*(?:no|number|id|code)|member\s*(?:no|number|id)|client\s*(?:no|number|id)|order\s*(?:no|number|id)|policy\s*(?:no|number)|passport(?:\s*(?:no|number))?|phone|tel|mobile|fax|id\s*no)\b\.?\s*(?:is|:|=|#)?\s*([A-Za-z0-9][A-Za-z0-9\-_.@]{3,})",
                )?,
                // Honorific-marked names. Up to three Han characters before
                // the honorific are masked: over-masking a verb is the price
                // of never leaving half a name.
                compile(r"(\p{Han}{1,3})(?:先生|小姐|女士|太太)")?,
                compile(r"\b(?:Mr|Mrs|Ms|Miss|Dr|Mx)\.?\s+([A-Z][A-Za-z'\-]+(?:\s+[A-Z][A-Za-z'\-]+)?)")?,
            ],
            digits: compile(r"\+?\d+(?:[ \-.()]{1,3}\d+)*")?,
        })
    });
    DETECTORS.as_ref().map_err(Clone::clone)
}

/// A digit-run candidate is sensitive when it carries 8+ digits (phone,
/// account, customer, card, order number), or 7+ with a `+` country prefix
/// or a phone-shaped `ddd-dddd` ending. A plain date (`2026-11-08`,
/// `2026.11.8`) or year range (`2025-2026`) is kept.
fn digit_run_is_sensitive(candidate: &str) -> bool {
    let groups: Vec<&str> = candidate
        .split(|c: char| !c.is_numeric())
        .filter(|g| !g.is_empty())
        .collect();
    let digits: usize = groups.iter().map(|g| g.chars().count()).sum();
    let international = candidate.starts_with('+');
    if digits >= 8 {
        return !(date_like(&groups) || year_range(&groups)) || international;
    }
    digits >= 7
        && (international
            || (groups.len() >= 2 && groups.last().is_some_and(|g| g.chars().count() == 4)))
}

fn number(g: &str) -> Option<u32> {
    g.parse().ok().filter(|_| g.chars().all(|c| c.is_ascii_digit()))
}

fn date_like(groups: &[&str]) -> bool {
    let [y, m, d] = groups else { return false };
    y.len() == 4
        && (1..=2).contains(&m.len())
        && (1..=2).contains(&d.len())
        && number(y).is_some_and(|y| (1900..=2199).contains(&y))
        && number(m).is_some_and(|m| (1..=12).contains(&m))
        && number(d).is_some_and(|d| (1..=31).contains(&d))
}

fn year_range(groups: &[&str]) -> bool {
    let [a, b] = groups else { return false };
    a.len() == 4
        && b.len() == 4
        && number(a).is_some_and(|y| (1900..=2199).contains(&y))
        && number(b).is_some_and(|y| (1900..=2199).contains(&y))
}

/// Nothing but an already-applied mask (folded or not) and spacing.
fn only_masks(text: &str) -> bool {
    text.chars().all(|c| c == '*' || c == '＊' || c.is_whitespace())
}

/// The model's entities in `folded`, or an error when any of them cannot be
/// trusted (unknown label, off-boundary span, a score that is low or NaN).
fn ner_spans(ner: &dyn TitleNer, folded: &str) -> Result<Vec<(usize, usize)>, String> {
    let mut spans = Vec::new();
    for entity in ner.entities(folded)? {
        if duduclaw_redaction::ner::labels::category_for(&entity.label).is_none() {
            return Err("NER label unknown".into());
        }
        if entity.confidence.is_some_and(|c| c.is_nan() || c < MIN_NER_CONFIDENCE) {
            return Err("NER confidence too low".into());
        }
        let (start, end) = (entity.start, entity.end);
        if start >= end
            || end > folded.len()
            || !folded.is_char_boundary(start)
            || !folded.is_char_boundary(end)
        {
            return Err("invalid NER span".into());
        }
        // A mask the model reads as an entity has nothing left to hide.
        if !only_masks(&folded[start..end]) {
            spans.push((start, end));
        }
    }
    Ok(spans)
}

/// Every sensitive span of `text`, as original byte ranges, merged.
fn pii_spans(
    engine: &RuleEngine,
    ner: &dyn TitleNer,
    text: &str,
) -> Result<Vec<(usize, usize)>, String> {
    let detectors = detectors()?;
    let (folded, offsets) = fold(text);
    let mut spans: Vec<(usize, usize)> = ner_spans(ner, &folded)?;
    for hit in engine.all_text_matches(&folded, &Source::CronContext) {
        spans.push((hit.span.start, hit.span.end));
    }
    for re in &detectors.whole {
        spans.extend(re.find_iter(&folded).map(|m| (m.start(), m.end())));
    }
    for re in &detectors.group {
        for caps in re.captures_iter(&folded) {
            if let Some(m) = caps.get(1) {
                spans.push((m.start(), m.end()));
            }
        }
    }
    for m in detectors.digits.find_iter(&folded) {
        if digit_run_is_sensitive(m.as_str()) {
            spans.push((m.start(), m.end()));
        }
    }
    let mut original = Vec::with_capacity(spans.len());
    for (start, end) in spans {
        if start >= end || end > folded.len() {
            return Err("masked span out of range".into());
        }
        original.push((to_original(&offsets, start)?, to_original(&offsets, end)?));
    }
    original.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::with_capacity(original.len());
    for (start, end) in original {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    Ok(merged)
}

/// `text` with every sensitive span replaced by [`MASK`].
fn mask_pii(engine: &RuleEngine, ner: &dyn TitleNer, text: &str) -> Result<String, String> {
    if text.len() > MAX_SCAN_BYTES * 2 {
        return Err("title too long to mask".into());
    }
    let spans = pii_spans(engine, ner, text)?;
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    for (start, end) in spans {
        out.push_str(text.get(at..start).ok_or("masked span off a character boundary")?);
        out.push_str(MASK);
        at = end;
    }
    out.push_str(text.get(at..).ok_or("masked span off a character boundary")?);
    Ok(out)
}

// ── Redaction engine ────────────────────────────────────────────

struct CachedEngine {
    people_dir: Option<PathBuf>,
    engine: Arc<RuleEngine>,
}

static ENGINES: LazyLock<Mutex<HashMap<PathBuf, CachedEngine>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The rule engine for `home`, rebuilt when the identity directory appears
/// or disappears (the identity rule refreshes its own name list).
fn engine_for(home: &Path) -> Result<Arc<RuleEngine>, String> {
    let people = home.join("shared/wiki/identity/people");
    let people = people.is_dir().then_some(people);
    // A panic while the lock was held leaves nothing half-built worth
    // keeping: start the cache over rather than disabling titles for good.
    let mut cache = ENGINES.lock().unwrap_or_else(|poisoned| {
        let mut cache = poisoned.into_inner();
        cache.clear();
        cache
    });
    if let Some(cached) = cache.get(home).filter(|c| c.people_dir == people) {
        return Ok(cached.engine.clone());
    }
    let engine = Arc::new(build_engine(people.clone())?);
    if cache.len() >= MAX_CACHED_ENGINES && !cache.contains_key(home) {
        cache.clear();
    }
    cache.insert(home.to_path_buf(), CachedEngine { people_dir: people, engine: engine.clone() });
    Ok(engine)
}

fn build_engine(people_dir: Option<PathBuf>) -> Result<RuleEngine, String> {
    let mut specs: Vec<RuleSpec> = Vec::new();
    for name in PROFILES {
        let profile = duduclaw_redaction::profiles::load_builtin(name)
            .map_err(|e| format!("redaction profile {name}: {e}"))?
            .ok_or_else(|| format!("redaction profile {name} missing"))?;
        let mut rules: Vec<(String, RuleSpec)> = profile.rules.into_iter().collect();
        rules.sort_by(|a, b| a.0.cmp(&b.0));
        for (id, mut spec) in rules {
            if SKIPPED_RULES.contains(&id.as_str()) {
                continue;
            }
            spec.id = id;
            spec.enabled = true;
            specs.push(spec);
        }
    }
    if people_dir.is_some() {
        specs.push(RuleSpec {
            id: "external_status_people".into(),
            category: "PERSON".into(),
            restore_scope: RestoreScope::Owner,
            priority: 70,
            cross_session_stable: false,
            apply_to_system_prompt: false,
            enabled: true,
            kind: RuleKind::Identity { source: "wiki".into() },
        });
    }
    let options = EngineOptions { identity_people_dir: people_dir, ..EngineOptions::default() };
    let engine = RuleEngine::from_specs_with(specs, &options).map_err(|e| format!("redaction engine: {e}"))?;
    if engine.rule_count() == 0 {
        return Err("redaction engine has no rules".into());
    }
    Ok(engine)
}

/// Test doubles for the on-device model, and a per-home registry that the
/// production lookup consults first (test builds only).
#[cfg(test)]
pub(crate) mod test_ner {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static REGISTERED: LazyLock<Mutex<HashMap<PathBuf, Arc<dyn TitleNer>>>> =
        LazyLock::new(|| Mutex::new(HashMap::new()));

    /// Serve `ner` as the "installed model" for `home`.
    pub(crate) fn register(home: &Path, ner: Arc<dyn TitleNer>) {
        REGISTERED.lock().unwrap_or_else(|p| p.into_inner()).insert(home.to_path_buf(), ner);
    }

    /// Back to the real lookup (no model installed in a temp home).
    pub(crate) fn unregister(home: &Path) {
        REGISTERED.lock().unwrap_or_else(|p| p.into_inner()).remove(home);
    }

    pub(super) fn registered(home: &Path) -> Option<Arc<dyn TitleNer>> {
        REGISTERED.lock().unwrap_or_else(|p| p.into_inner()).get(home).cloned()
    }

    /// Reports every occurrence of the given names as `private_person`.
    pub(crate) struct People {
        names: Vec<String>,
        confidence: Option<f32>,
    }

    impl People {
        pub(crate) fn none() -> Self {
            Self::of(&[])
        }
        /// A scoring detector, sure of every name.
        pub(crate) fn of(names: &[&str]) -> Self {
            Self { names: names.iter().map(|n| n.to_string()).collect(), confidence: Some(0.97) }
        }
        /// Like the production decoder: no per-span score.
        pub(crate) fn unscored(names: &[&str]) -> Self {
            Self { confidence: None, ..Self::of(names) }
        }
    }

    impl TitleNer for People {
        fn entities(&self, text: &str) -> Result<Vec<NerEntity>, String> {
            Ok(self
                .names
                .iter()
                .flat_map(|name| {
                    text.match_indices(name.as_str()).map(|(at, found)| NerEntity {
                        start: at,
                        end: at + found.len(),
                        label: "private_person".into(),
                        confidence: self.confidence,
                    })
                })
                .collect())
        }
    }

    /// Inference fails.
    pub(crate) struct Failing;

    impl TitleNer for Failing {
        fn entities(&self, _: &str) -> Result<Vec<NerEntity>, String> {
            Err("inference failed".into())
        }
    }

    /// Always the same entities.
    pub(crate) struct Fixed(pub Vec<NerEntity>);

    impl TitleNer for Fixed {
        fn entities(&self, _: &str) -> Result<Vec<NerEntity>, String> {
            Ok(self.0.clone())
        }
    }

    /// No entities; counts the texts it was asked about.
    #[derive(Default)]
    pub(crate) struct Counting(pub AtomicUsize);

    impl TitleNer for Counting {
        fn entities(&self, _: &str) -> Result<Vec<NerEntity>, String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_ner::{Counting, Failing, Fixed, People};
    use super::*;

    const MAX: usize = 60;

    fn title_with(ner: &dyn TitleNer, raw: &str) -> Option<String> {
        let home = tempfile::tempdir().unwrap();
        external_title_with(home.path(), ner, raw, MAX)
    }

    fn title(raw: &str) -> Option<String> {
        title_with(&People::none(), raw)
    }

    fn masked(raw: &str) -> String {
        title(raw).unwrap_or_else(|| panic!("title omitted for {raw:?}"))
    }

    const VISIT: &str = "拜訪王小明確認續約";

    #[test]
    fn an_unknown_chinese_name_is_masked_by_the_on_device_model() {
        let out = title_with(&People::of(&["王小明"]), VISIT);
        assert_eq!(out.as_deref(), Some("拜訪＊＊＊確認續約"));
        // No rule knows this name: without the model it would go out as-is,
        // which is why a title requires the model at all.
        assert_eq!(title(VISIT).as_deref(), Some(VISIT));
    }

    #[test]
    fn without_a_working_model_the_title_is_omitted() {
        // A temp home has no model installed and no double registered.
        let home = tempfile::tempdir().unwrap();
        assert_eq!(external_title(home.path(), VISIT, MAX), None, "model not installed");
        assert_eq!(external_title(home.path(), "整理第四季報表", MAX), None, "even a harmless title");
        assert_eq!(title_with(&Failing, VISIT), None, "inference error");
        let entity = |start, end, label: &str, confidence| NerEntity {
            start,
            end,
            label: label.into(),
            confidence,
        };
        // 王小明 is bytes 6..15 of VISIT.
        assert_eq!(title_with(&Fixed(vec![entity(6, 15, "private_person", Some(0.2))]), VISIT), None, "low confidence");
        assert_eq!(title_with(&Fixed(vec![entity(6, 15, "private_person", Some(f32::NAN))]), VISIT), None, "NaN score");
        assert_eq!(title_with(&Fixed(vec![entity(6, 15, "organisation", None)]), VISIT), None, "unknown label");
        assert_eq!(title_with(&Fixed(vec![entity(7, 15, "private_person", None)]), VISIT), None, "off a char boundary");
        assert_eq!(title_with(&Fixed(vec![entity(6, 999, "private_person", None)]), VISIT), None, "out of range");
        // An unscored entity (the production decoder) is simply masked.
        assert_eq!(title_with(&People::unscored(&["王小明"]), VISIT).as_deref(), Some("拜訪＊＊＊確認續約"));
    }

    #[test]
    fn the_model_also_rescans_the_truncated_output() {
        let counting = Counting::default();
        assert!(title_with(&counting, "整理第四季報表").is_some());
        assert_eq!(counting.0.load(std::sync::atomic::Ordering::SeqCst), 2, "mask pass + rescan");
    }

    #[test]
    fn zero_width_characters_cannot_split_a_value() {
        let out = masked("回電0912\u{200B}345\u{200C}678");
        assert!(!out.contains("345") && out.contains(MASK), "{out:?}");
        let out = masked("回電0912\u{00AD}345\u{2063}678");
        assert!(!out.contains("345") && out.contains(MASK), "{out:?}");
        let out = masked("寄給ali\u{200D}ce@exa\u{2060}mple\u{FEFF}.com確認");
        assert!(!out.contains("ali") && !out.contains("mple"), "{out:?}");
        assert!(out.starts_with("寄給") && out.ends_with("確認"), "{out:?}");
        let out = title_with(&People::of(&["王小明"]), "拜訪王\u{200B}小\u{2062}明\u{FE0F}確認續約");
        assert_eq!(out.as_deref(), Some("拜訪＊＊＊確認續約"));
        for out in [masked("寄出\u{200B}報價\u{2061}單"), masked("整理\u{180E}報表")] {
            assert!(!out.chars().any(is_hidden), "{out:?}");
        }
    }

    #[test]
    fn bidi_controls_are_removed_before_detection() {
        let out = masked("回電\u{202E}876-543-2190\u{202C}給客戶");
        assert!(!out.contains("543") && out.contains(MASK), "{out:?}");
        let out = masked("核對身分證\u{2067}A123456789\u{2069}");
        assert!(!out.contains("123456789"), "{out:?}");
        let out = masked("回覆\u{200F}ops@example.com\u{061C}\u{200E}");
        assert!(!out.contains("ops@") && !out.contains("example"), "{out:?}");
        for out in [
            masked("寄出\u{202A}報價單\u{202C}"),
            masked("\u{2066}週報\u{2069}\u{2068}整理\u{2069}"),
        ] {
            assert!(!out.chars().any(is_hidden), "{out:?}");
        }
        assert_eq!(masked("寄出\u{202D}報價單\u{202C}"), "寄出報價單");
    }

    #[test]
    fn mixed_script_tokens_omit_the_title() {
        // Cyrillic а / е inside a Latin address.
        assert_eq!(title("寄合約給\u{0430}lice@example.com確認"), None);
        assert_eq!(title("寄給 alic\u{0435}@example.com"), None);
        // Full-width Latin folds to Latin by NFKC, then mixes with Cyrillic.
        assert_eq!(title("寄給ａｌｉｃ\u{0435}@example.com"), None);
        // Greek omicron in a Latin word.
        assert_eq!(title("登入 g\u{03BF}\u{03BF}gle 帳號"), None);
        // One script per token is fine, as is Latin right next to Han.
        assert!(title("回覆 Иван 的報價").is_some());
        assert!(title("整理Q4報表給sales團隊").is_some());
    }

    #[test]
    fn plain_titles_pass_unchanged_including_dates_and_amounts() {
        for raw in [
            "寄出報價單",
            "整理第四季報表",
            "2026-11-08 週會紀錄",
            "2025-2026 年度預算",
            "報價 NT$1,250,000 確認",
            "v1.80.1 發版檢查",
            "回覆 3 位客戶",
        ] {
            assert_eq!(masked(raw), raw);
        }
    }

    #[test]
    fn emails_are_masked_next_to_cjk_and_full_width() {
        let out = masked("寄合約給alice.chen@example.com確認");
        assert!(!out.contains("alice") && !out.contains("example"), "{out}");
        assert!(out.starts_with("寄合約給") && out.contains(MASK), "{out}");
        // NFKC folds the full-width address first; neither spelling survives.
        let out = masked("回信ｂｏｂ＠ｅｘａｍｐｌｅ．ｃｏｍ");
        assert!(!out.contains("ｂｏｂ") && !out.contains("ｅｘａｍｐｌｅ"), "{out}");
        assert!(!out.contains("bob") && !out.contains("example"), "{out}");
        let out = masked("轉寄 王小明@example.com 的附件");
        assert!(!out.contains("王小明") && !out.contains("example"), "{out}");
        assert!(out.ends_with("的附件"), "{out}");
    }

    #[test]
    fn phones_in_every_spelling_are_masked() {
        for (raw, secret) in [
            ("回電王經理0912-345-678", "345"),
            ("回電 0912 345 678 討論", "678"),
            ("致電０９１２３４５６７８", "345678"),
            ("致電（０２）２３４５－６７８９", "6789"),
            ("聯絡 +886 912 345 678 確認", "345"),
            ("call +1 (415) 555-0123 today", "555"),
            ("撥打 02-2345-6789", "2345"),
            ("分機 2345-6789", "6789"),
        ] {
            let out = masked(raw);
            assert!(!out.contains(secret), "{raw} -> {out}");
            assert!(out.contains(MASK), "{raw} -> {out}");
        }
    }

    #[test]
    fn national_ids_and_long_numbers_are_masked() {
        for (raw, secret) in [
            ("核對身分證A123456789資料", "123456789"),
            ("核對Ａ１２３４５６７８９資料", "123456789"),
            ("居留證 ac12345678", "12345678"),
            ("客戶帳號 0123456789012 對帳", "6789012"),
            ("訂單20261108001出貨", "1108001"),
            ("刷卡 4111 1111 1111 1111 失敗", "1111"),
            ("統編12345678開立發票", "12345678"),
        ] {
            let out = masked(raw);
            assert!(!out.contains(secret), "{raw} -> {out}");
        }
    }

    #[test]
    fn labelled_identifiers_and_honorific_names_are_masked() {
        for (raw, secret) in [
            ("更新客戶編號：C-20391 資料", "20391"),
            ("確認會員編號ＶＩＰ７７８８", "7788"),
            ("Update account no. AB1234 owner", "AB1234"),
            ("Customer ID: CUST-0042 follow-up", "0042"),
            ("回覆陳先生的詢價", "陳"),
            ("Email Mr. Smith about renewal", "Smith"),
        ] {
            let out = masked(raw);
            assert!(!out.contains(secret), "{raw} -> {out}");
            assert!(out.contains(MASK), "{raw} -> {out}");
        }
    }

    #[test]
    fn credentials_and_pii_together() {
        let out = masked("設定主機，密碼是測試值，通知 ops@example.com 與 0912345678");
        for secret in ["測試值", "ops@", "0912345678"] {
            assert!(!out.contains(secret), "{out}");
        }
        let out = masked("api_key=sk-abcdefghijklmnopqrstuvwxyz 給 A123456789");
        assert!(!out.contains("sk-abc") && !out.contains("A123456789"), "{out}");
    }

    #[test]
    fn known_people_from_the_identity_directory_are_masked() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("shared/wiki/identity/people");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("p1.md"), "---\nperson_id: p1\ndisplay_name: 林美華\n---\n").unwrap();
        let out = external_title_with(home.path(), &People::none(), "幫林美華準備簡報", MAX).unwrap();
        assert!(!out.contains("林美華"), "{out}");
        assert!(out.starts_with("幫") && out.ends_with("準備簡報"), "{out}");
    }

    #[test]
    fn masking_happens_before_truncation_and_width_is_cjk_safe() {
        // The phone straddles the cut: masked first, never half-shown.
        let raw = format!("{}0912345678", "請".repeat(MAX - 4));
        let out = masked(&raw);
        assert!(out.chars().count() <= MAX);
        assert!(!out.contains("0912"), "{out}");
        let out = masked(&"季".repeat(80));
        assert_eq!(out.chars().count(), MAX);
        assert!(!masked("第一行\n第二行").contains('\n'));
    }

    #[test]
    fn failures_omit_the_title() {
        assert_eq!(title(&"a".repeat(MAX_SCAN_BYTES + 1)), None, "oversized ⇒ omitted");
        // Under the raw limit, but NFKC expands each character ~11×.
        let expanding = "\u{FDFA}".repeat(MAX_SCAN_BYTES / 3);
        assert!(expanding.len() <= MAX_SCAN_BYTES);
        assert_eq!(title(&expanding), None, "oversized after NFKC ⇒ omitted");
        let engine = build_engine(None).unwrap();
        assert!(mask_pii(&engine, &People::none(), &"x".repeat(MAX_SCAN_BYTES * 2 + 1)).is_err());
        assert!(to_original(&[(0, 0), (3, 3)], 1).is_err(), "off-boundary span refuses");
    }

    #[test]
    fn folding_keeps_offsets_one_to_one() {
        let (folded, offsets) = fold("Ａ１－　x");
        assert_eq!(folded, "A1- x");
        assert_eq!(offsets.len(), "Ａ１－　x".chars().count() + 1);
        assert_eq!(offsets.last(), Some(&(folded.len(), "Ａ１－　x".len())));
    }
}
