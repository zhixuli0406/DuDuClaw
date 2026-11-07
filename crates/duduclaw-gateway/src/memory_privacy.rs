//! RM-039 — automatic memory must not keep personal sensitive information.
//!
//! An administrator chooses which **topic categories** automatic memory may
//! never store (`[memory_privacy] excluded_categories`). Four categories are
//! built in — `health`, `family`, `personal_finance`, `salary` — and all four
//! are excluded by default, on new installs and on existing ones alike: a
//! missing `[memory_privacy]` section means the defaults apply. Administrators
//! can untick categories or define their own with `custom_keywords`.
//!
//! ## What is gated
//!
//! Only the *automatic* writers: conversation distillation
//! (`wiki_ingest::persist_facts`), profile-trait distillation
//! (`profile_distill`), digital-footprint distillation, Reflexion rule
//! consolidation and the persona-induction `key_facts` bridge. When a staff
//! member explicitly asks the AI to remember something (MCP `memory_store` /
//! `user_profile_record`), it is stored regardless — those paths never call
//! this module.
//!
//! ## Classifier
//!
//! Deterministic, zh-TW + English keyword/phrase lists per category, matched
//! on each of a fact's `content` / `subject` / `predicate` / `object`
//! separately with [`duduclaw_core::word_contains_ci`] (never an unanchored
//! substring test). ASCII keywords therefore only match whole words
//! (`salary` matches `monthly_salary` and `Salary:` but not `salaryman`). CJK
//! has no word boundaries, so a CJK phrase matches wherever it appears between
//! non-ASCII-alphanumeric neighbours — which is why the CJK lists hold
//! multi-character phrases only (`生病`, never `病`: `病毒` is a computer virus,
//! `毛病` is a bug).
//!
//! **Trade-off (documented, accepted):** the lists are topic-level, not
//! intent-level. A work fact that merely *names* a sensitive topic is dropped
//! too — e.g. 「健康檢查報告模板放在共用槽」 hits `health` (健康檢查), and
//! "the bank client launched a new loan product" hits `personal_finance`
//! (loan). Losing such a fact from automatic memory costs little (it is still
//! in the conversation, and the staff member can ask the AI to remember it
//! explicitly); storing a colleague's diagnosis costs a lot. Ordinary
//! business facts (「客戶要求下週交貨」) match nothing.
//!
//! ## Fail closed
//!
//! If `config.toml` cannot be read or parsed, or the section is invalid, the
//! **defaults** are active (and a warning is logged). An unreadable config can
//! therefore only make automatic memory stricter, never looser.
//!
//! ## Forgotten facts are not re-learned
//!
//! `me.memory.forget` records a one-way key (SHA-256 of the normalised
//! subject / predicate / object) in `<home>/memory_privacy_forgotten.jsonl`.
//! The gate refuses an automatic write whose triple produces the same key, so
//! the next conversation cannot quietly put a fact the staff member deleted
//! back. The file holds digests only — never the forgotten text. (The
//! engine's source tombstones are per conversation turn and would also delete
//! unrelated facts from the same turn, so they are not used here.)
//!
//! Logging: never the fact text above `debug`, and not even there — only
//! counts and category names.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

/// `config.toml` section name.
pub const SECTION: &str = "memory_privacy";

/// Built-in categories, in display order. All are excluded by default.
pub const BUILTIN_CATEGORIES: &[&str] = &["health", "family", "personal_finance", "salary"];

/// Limits enforced by [`MemoryPrivacyConfig::validate`].
pub const MAX_CATEGORIES: usize = 32;
pub const MAX_KEYWORDS_PER_CATEGORY: usize = 200;
pub const MAX_KEYWORD_CHARS: usize = 64;
const MAX_CATEGORY_NAME_LEN: usize = 32;

// ---------------------------------------------------------------------------
// Built-in keyword lists
// ---------------------------------------------------------------------------
//
// Kept deliberately focused: every entry is a phrase that, in an employee's
// chat with an AI colleague, almost always means the personal topic. Broad
// words with a common business sense ("hospital" — a hospital client, 醫院,
// 收入 — revenue, "bonus" — bonus points, "separated" — comma-separated) are
// left out on purpose.

const HEALTH: &[&str] = &[
    // zh-TW
    "生病", "病情", "病歷", "病假", "看醫生", "看病", "就醫", "回診", "診斷", "確診",
    "住院", "出院", "開刀", "手術", "吃藥", "服藥", "用藥", "藥物", "處方", "懷孕",
    "孕期", "產檢", "流產", "癌症", "腫瘤", "化療", "憂鬱症", "焦慮症", "精神科",
    "身心科", "心理諮商", "慢性病", "糖尿病", "高血壓", "身心障礙", "健康檢查", "體檢",
    // English
    "diagnosis", "diagnosed", "illness", "sick leave", "medication", "medications",
    "prescription", "hospitalized", "hospitalised", "surgery", "pregnant", "pregnancy",
    "miscarriage", "cancer", "chemotherapy", "depression", "anxiety disorder",
    "mental health", "medical condition", "medical history", "medical record",
    "medical records", "chronic illness", "diabetes", "disability", "therapist",
    "see a doctor", "saw a doctor", "doctor's appointment",
];

const FAMILY: &[&str] = &[
    // zh-TW
    "離婚", "分居", "婚姻", "外遇", "結婚", "小孩", "孩子", "兒子", "女兒", "老公",
    "老婆", "丈夫", "妻子", "家人生病", "家暴", "爸媽", "父母", "監護權", "贍養費",
    "喪禮", "過世",
    // English
    "divorce", "divorced", "marriage", "married", "wife", "husband", "spouse",
    "my kids", "my children", "my son", "my daughter", "my parents", "custody",
    "domestic violence", "family member", "funeral", "passed away",
];

const PERSONAL_FINANCE: &[&str] = &[
    // zh-TW
    "負債", "欠債", "債務", "貸款", "房貸", "車貸", "信貸", "卡債", "存款", "積蓄",
    "投資虧損", "賠錢", "破產", "信用評分", "個人財務", "理財",
    // English
    "debt", "debts", "loan", "loans", "mortgage", "savings", "bankrupt", "bankruptcy",
    "credit score", "personal finance", "personal finances", "net worth",
    "investment loss", "investment losses", "credit card debt",
];

const SALARY: &[&str] = &[
    // zh-TW
    "薪水", "薪資", "月薪", "年薪", "時薪", "底薪", "加薪", "減薪", "調薪", "年終獎金",
    "薪酬", "獎金",
    // English
    "salary", "salaries", "wage", "wages", "paycheck", "payslip", "pay slip",
    "pay raise", "pay rise", "pay cut", "annual bonus", "year-end bonus",
    "compensation package",
];

/// The built-in keyword list of `category` (empty for a custom category).
pub fn builtin_keywords(category: &str) -> &'static [&'static str] {
    match category {
        "health" => HEALTH,
        "family" => FAMILY,
        "personal_finance" => PERSONAL_FINANCE,
        "salary" => SALARY,
        _ => &[],
    }
}

/// `(English, 繁體中文)` label of a category for the ingest prompt / UI.
pub fn category_labels(category: &str) -> (String, String) {
    match category {
        "health" => ("health".into(), "健康".into()),
        "family" => ("family".into(), "家庭".into()),
        "personal_finance" => ("personal finances".into(), "個人財務".into()),
        "salary" => ("salary".into(), "薪資".into()),
        other => {
            let readable = other.replace('_', " ");
            (readable.clone(), readable)
        }
    }
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

fn default_excluded() -> Vec<String> {
    BUILTIN_CATEGORIES.iter().map(|s| s.to_string()).collect()
}

/// `[memory_privacy]` in `config.toml`.
///
/// ```toml
/// [memory_privacy]
/// excluded_categories = ["health", "family", "personal_finance", "salary"]
///
/// [memory_privacy.custom_keywords]
/// health = ["復健"]             # extra words for a built-in category
/// legal_dispute = ["訴訟", "lawsuit"]  # a new category (tick it above to use it)
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryPrivacyConfig {
    /// Categories automatic memory never stores. A missing key means the four
    /// built-in defaults; an explicit `[]` turns the filter off.
    #[serde(default = "default_excluded")]
    pub excluded_categories: Vec<String>,
    /// Extra keywords per category. A key that is not built in defines a new
    /// category (it only filters while it is also in `excluded_categories`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub custom_keywords: BTreeMap<String, Vec<String>>,
}

impl Default for MemoryPrivacyConfig {
    fn default() -> Self {
        Self {
            excluded_categories: default_excluded(),
            custom_keywords: BTreeMap::new(),
        }
    }
}

fn valid_category_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && name.len() <= MAX_CATEGORY_NAME_LEN
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

impl MemoryPrivacyConfig {
    /// Reject anything the gate could not apply as written. Error messages
    /// are shown to the administrator (zh-TW) and carry no memory content.
    pub fn validate(&self) -> Result<(), String> {
        if self.custom_keywords.len() > MAX_CATEGORIES {
            return Err(format!("自訂類別最多 {MAX_CATEGORIES} 個"));
        }
        for (name, words) in &self.custom_keywords {
            if !valid_category_name(name) {
                return Err(format!(
                    "類別名稱「{name}」無效：只能使用小寫英文字母、數字與底線，且須以字母開頭（最多 {MAX_CATEGORY_NAME_LEN} 字元）"
                ));
            }
            if words.len() > MAX_KEYWORDS_PER_CATEGORY {
                return Err(format!(
                    "類別「{name}」的關鍵字最多 {MAX_KEYWORDS_PER_CATEGORY} 個"
                ));
            }
            for w in words {
                let w = w.trim();
                if w.is_empty() {
                    return Err(format!("類別「{name}」含有空白關鍵字"));
                }
                if w.chars().count() > MAX_KEYWORD_CHARS {
                    return Err(format!(
                        "類別「{name}」的關鍵字過長（最多 {MAX_KEYWORD_CHARS} 字元）"
                    ));
                }
                if w.chars().any(char::is_control) {
                    return Err(format!("類別「{name}」的關鍵字含有控制字元"));
                }
                // A one-letter ASCII keyword ("a", "x") matches as a word all
                // over ordinary text; a single CJK character is allowed but
                // is the administrator's call.
                if w.is_ascii() && w.chars().count() < 2 {
                    return Err(format!(
                        "類別「{name}」的英文關鍵字「{w}」太短（至少 2 個字元）"
                    ));
                }
            }
        }
        let mut seen = HashSet::new();
        for name in &self.excluded_categories {
            if !seen.insert(name.as_str()) {
                return Err(format!("排除類別「{name}」重複"));
            }
            let builtin = BUILTIN_CATEGORIES.contains(&name.as_str());
            let custom = self
                .custom_keywords
                .get(name)
                .is_some_and(|w| !w.is_empty());
            if !builtin && !custom {
                return Err(format!(
                    "未知的排除類別「{name}」：請使用內建類別（{}），或先在 custom_keywords 定義它的關鍵字",
                    BUILTIN_CATEGORIES.join("、")
                ));
            }
        }
        Ok(())
    }

    /// Parse and validate the section from a whole `config.toml` table.
    /// A missing section is the defaults (existing installs get them too).
    pub fn from_table(table: &toml::Table) -> Result<Self, String> {
        let Some(section) = table.get(SECTION) else {
            return Ok(Self::default());
        };
        let cfg: Self = section
            .clone()
            .try_into()
            .map_err(|e: toml::de::Error| format!("[{SECTION}]: {}", e.message()))?;
        cfg.validate().map_err(|e| format!("[{SECTION}]: {e}"))?;
        Ok(cfg)
    }

    /// Load from `<home>/config.toml`, failing **closed**: an unreadable,
    /// unparsable or invalid config yields the defaults, with a warning.
    pub fn load(home_dir: &Path) -> Self {
        Self::load_with_state(home_dir).0
    }

    /// [`Self::load`] plus whether the defaults were forced by an error.
    pub fn load_with_state(home_dir: &Path) -> (Self, LoadState) {
        let path = home_dir.join("config.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Self::default(), LoadState::Defaults)
            }
            Err(e) => {
                warn!(
                    "memory privacy: config.toml unreadable ({}); default exclusions active (fail closed)",
                    e.kind()
                );
                return (Self::default(), LoadState::FailedClosed);
            }
        };
        // The toml error text quotes the offending line, and config.toml can
        // hold credentials — so the parse error itself is not logged.
        let Ok(table) = text.parse::<toml::Table>() else {
            warn!("memory privacy: config.toml is not valid TOML; default exclusions active (fail closed)");
            return (Self::default(), LoadState::FailedClosed);
        };
        if !table.contains_key(SECTION) {
            return (Self::default(), LoadState::Defaults);
        }
        match Self::from_table(&table) {
            Ok(cfg) => (cfg, LoadState::Configured),
            Err(e) => {
                warn!("memory privacy: {e}; default exclusions active (fail closed)");
                (Self::default(), LoadState::FailedClosed)
            }
        }
    }
}

/// How [`MemoryPrivacyConfig::load_with_state`] arrived at its result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    /// No config file or no `[memory_privacy]` section — defaults.
    Defaults,
    /// The section was read and is valid.
    Configured,
    /// The config could not be read / parsed / validated — defaults forced.
    FailedClosed,
}

// ---------------------------------------------------------------------------
// Gate
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct ActiveCategory {
    name: String,
    keywords: Vec<String>,
}

/// The text parts of one candidate memory write.
#[derive(Debug, Clone, Copy, Default)]
pub struct FactParts<'a> {
    pub content: &'a str,
    pub subject: Option<&'a str>,
    pub predicate: Option<&'a str>,
    pub object: Option<&'a str>,
}

/// The gate's decision for one candidate write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict<'g> {
    Allow,
    /// Matched an excluded category (its name).
    Excluded(&'g str),
    /// The same triple was deleted by its subject via `me.memory.forget`.
    Forgotten,
}

/// Compiled exclusion lists + forgotten-fact keys for one write batch.
#[derive(Debug, Clone, Default)]
pub struct PrivacyGate {
    categories: Vec<ActiveCategory>,
    forgotten: HashSet<String>,
}

static EXCLUDED_TOTAL: AtomicU64 = AtomicU64::new(0);
static FORGOTTEN_BLOCKED_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Process-wide count of automatic writes dropped by a category.
pub fn excluded_total() -> u64 {
    EXCLUDED_TOTAL.load(Ordering::Relaxed)
}

/// Process-wide count of automatic writes refused as previously forgotten.
pub fn forgotten_blocked_total() -> u64 {
    FORGOTTEN_BLOCKED_TOTAL.load(Ordering::Relaxed)
}

impl PrivacyGate {
    /// Gate for an explicit config (no forgotten keys).
    pub fn from_config(cfg: &MemoryPrivacyConfig) -> Self {
        let mut categories = Vec::new();
        for name in &cfg.excluded_categories {
            let mut keywords: Vec<String> = builtin_keywords(name)
                .iter()
                .map(|s| s.to_string())
                .collect();
            if let Some(extra) = cfg.custom_keywords.get(name) {
                for w in extra {
                    let w = w.trim();
                    if !w.is_empty() && !keywords.iter().any(|k| k == w) {
                        keywords.push(w.to_string());
                    }
                }
            }
            if !keywords.is_empty() {
                categories.push(ActiveCategory {
                    name: name.clone(),
                    keywords,
                });
            }
        }
        Self {
            categories,
            forgotten: HashSet::new(),
        }
    }

    /// The four built-in categories, nothing forgotten.
    pub fn defaults() -> Self {
        Self::from_config(&MemoryPrivacyConfig::default())
    }

    /// The gate the automatic writers use: `<home>/config.toml` (fail closed)
    /// plus the forgotten-fact keys.
    pub fn for_home(home_dir: &Path) -> Self {
        let mut gate = Self::from_config(&MemoryPrivacyConfig::load(home_dir));
        gate.forgotten = forgotten::load(home_dir);
        gate
    }

    /// Names of the active (excluded) categories.
    pub fn categories(&self) -> Vec<&str> {
        self.categories.iter().map(|c| c.name.as_str()).collect()
    }

    /// The first excluded category `text` mentions, if any.
    pub fn classify(&self, text: &str) -> Option<&str> {
        if text.trim().is_empty() {
            return None;
        }
        self.categories
            .iter()
            .find(|c| {
                c.keywords
                    .iter()
                    .any(|k| duduclaw_core::word_contains_ci(text, k))
            })
            .map(|c| c.name.as_str())
    }

    /// Decide one candidate write. Each part is classified on its own so a
    /// keyword can never be assembled across a part boundary.
    pub fn check(&self, parts: &FactParts<'_>) -> Verdict<'_> {
        for text in [
            Some(parts.content),
            parts.subject,
            parts.predicate,
            parts.object,
        ]
        .into_iter()
        .flatten()
        {
            if let Some(cat) = self.classify(text) {
                return Verdict::Excluded(cat);
            }
        }
        if !self.forgotten.is_empty() {
            if let (Some(s), Some(p)) = (parts.subject, parts.predicate) {
                let third = parts.object.unwrap_or(parts.content);
                if self.forgotten.contains(&forgotten::key(s, p, third)) {
                    return Verdict::Forgotten;
                }
            }
        }
        Verdict::Allow
    }

    /// Defence-in-depth line for the LLM extraction prompts (`None` when no
    /// category is active). Category names are validated ASCII identifiers,
    /// so nothing user-typed reaches the prompt.
    pub fn prompt_instruction(&self) -> Option<String> {
        if self.categories.is_empty() {
            return None;
        }
        let (en, zh): (Vec<String>, Vec<String>) = self
            .categories
            .iter()
            .map(|c| category_labels(&c.name))
            .unzip();
        Some(format!(
            "PRIVACY: never extract facts about a person's {}. \
             請勿擷取任何關於個人{}的事實。",
            en.join(", "),
            zh.join("、")
        ))
    }
}

/// The prompt line for `home`'s current config (see
/// [`PrivacyGate::prompt_instruction`]).
pub fn prompt_line(home_dir: &Path) -> Option<String> {
    PrivacyGate::from_config(&MemoryPrivacyConfig::load(home_dir)).prompt_instruction()
}

/// Aggregate of what the gate did to one write batch. Holds counts and
/// category names only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilterReport {
    pub excluded: usize,
    pub forgotten: usize,
    pub by_category: BTreeMap<String, usize>,
}

impl FilterReport {
    /// Record one verdict; returns `true` when the write may proceed.
    pub fn record(&mut self, verdict: Verdict<'_>) -> bool {
        match verdict {
            Verdict::Allow => true,
            Verdict::Excluded(cat) => {
                self.excluded += 1;
                *self.by_category.entry(cat.to_string()).or_insert(0) += 1;
                EXCLUDED_TOTAL.fetch_add(1, Ordering::Relaxed);
                false
            }
            Verdict::Forgotten => {
                self.forgotten += 1;
                FORGOTTEN_BLOCKED_TOTAL.fetch_add(1, Ordering::Relaxed);
                false
            }
        }
    }

    pub fn dropped(&self) -> usize {
        self.excluded + self.forgotten
    }

    /// One aggregate log line (counts + category names, never content).
    pub fn log(&self, agent_id: &str, writer: &str) {
        if self.dropped() == 0 {
            return;
        }
        let categories = self
            .by_category
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(",");
        info!(
            agent = agent_id,
            writer,
            excluded = self.excluded,
            forgotten = self.forgotten,
            categories = %categories,
            "memory privacy: automatic memory writes withheld"
        );
        debug!(
            agent = agent_id,
            writer,
            excluded_total = excluded_total(),
            forgotten_total = forgotten_blocked_total(),
            "memory privacy: process totals"
        );
    }
}

// ---------------------------------------------------------------------------
// Forgotten-fact keys
// ---------------------------------------------------------------------------

/// One-way keys of facts their subject deleted via `me.memory.forget`.
pub mod forgotten {
    use super::*;
    use std::path::PathBuf;

    /// File under the DuDuClaw home.
    pub const FILE: &str = "memory_privacy_forgotten.jsonl";
    /// Each part is normalised to at most this many characters — the same cap
    /// the conversation distiller applies to stored triple parts, so a long
    /// candidate and its stored (capped) form produce the same key.
    pub const PART_CHARS: usize = 120;
    /// Oldest keys beyond this are ignored on load (bounded memory).
    pub const MAX_KEYS: usize = 20_000;

    #[derive(Serialize, Deserialize)]
    struct Line {
        key: String,
        at: String,
    }

    fn path(home: &Path) -> PathBuf {
        home.join(FILE)
    }

    fn norm(part: &str) -> String {
        let collapsed = part.split_whitespace().collect::<Vec<_>>().join(" ");
        duduclaw_core::truncate_chars(&collapsed.to_lowercase(), PART_CHARS)
    }

    /// The key of `(subject, predicate, object-or-content)`.
    pub fn key(subject: &str, predicate: &str, object: &str) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(norm(subject).as_bytes());
        h.update([0x1f]);
        h.update(norm(predicate).as_bytes());
        h.update([0x1f]);
        h.update(norm(object).as_bytes());
        hex::encode(h.finalize())
    }

    /// Append one key. Best-effort for the caller, but the error is returned
    /// so the RPC can report it.
    pub fn record(home: &Path, key: &str) -> std::io::Result<()> {
        use std::io::Write;
        let line = serde_json::to_string(&Line {
            key: key.to_string(),
            at: chrono::Utc::now().to_rfc3339(),
        })
        .map_err(std::io::Error::other)?;
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path(home))?;
        writeln!(f, "{line}")
    }

    /// Every recorded key (newest [`MAX_KEYS`]). Missing file ⇒ empty; an
    /// unreadable file is logged and treated as empty.
    pub fn load(home: &Path) -> HashSet<String> {
        let text = match std::fs::read_to_string(path(home)) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashSet::new(),
            Err(e) => {
                warn!("memory privacy: forgotten-fact list unreadable ({})", e.kind());
                return HashSet::new();
            }
        };
        let keys: Vec<String> = text
            .lines()
            .filter_map(|l| serde_json::from_str::<Line>(l).ok())
            .map(|l| l.key)
            .filter(|k| k.len() == 64 && k.bytes().all(|b| b.is_ascii_hexdigit()))
            .collect();
        let skip = keys.len().saturating_sub(MAX_KEYS);
        keys.into_iter().skip(skip).collect()
    }
}

// ---------------------------------------------------------------------------
// Self subjects (for `me.memory.*`)
// ---------------------------------------------------------------------------

/// Every memory `subject` that denotes the dashboard user `user_id`:
/// `user:<id>`, their WebChat identity (`user:webchat:<owner-tag>`, see
/// `webchat::webchat_owner_tag`), and for each **verified** linked channel
/// identity both `user:<channel_user_id>` (what the channel reply pipeline
/// records) and `user:<channel>:<channel_user_id>`.
pub fn self_subjects(user_id: &str, verified_channels: &[(String, String)]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |raw: String| {
        let s = duduclaw_memory::user_profile::user_subject(&raw);
        if !out.contains(&s) {
            out.push(s);
        }
    };
    let uid = user_id.trim();
    if uid.is_empty() {
        return Vec::new();
    }
    push(uid.to_string());
    push(format!("webchat:{}", crate::webchat::webchat_owner_tag(uid)));
    for (channel, cuid) in verified_channels {
        let (channel, cuid) = (channel.trim(), cuid.trim());
        if cuid.is_empty() || crate::profile_distill::is_anonymous(cuid) {
            continue;
        }
        push(cuid.to_string());
        if !channel.is_empty() {
            push(format!("{channel}:{cuid}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate() -> PrivacyGate {
        PrivacyGate::defaults()
    }

    fn cat(text: &str) -> Option<String> {
        gate().classify(text).map(str::to_string)
    }

    #[test]
    fn health_positives_zh_and_en() {
        for t in [
            "Alice 上週生病請了三天假",
            "他下週要住院開刀",
            "她正在吃藥控制血壓",
            "王經理懷孕了",
            "確診憂鬱症",
            "Bob was diagnosed with diabetes",
            "She is on medication for anxiety disorder",
            "He took sick leave on Monday",
            "I need to see a doctor tomorrow",
        ] {
            assert_eq!(cat(t).as_deref(), Some("health"), "{t}");
        }
    }

    #[test]
    fn family_positives_zh_and_en() {
        for t in [
            "小陳正在辦離婚",
            "她的小孩下個月出生",
            "他的爸媽住在高雄",
            "他老婆在台中工作",
            "Carol is going through a divorce",
            "His wife works at the bank",
            "my kids are on holiday next week",
        ] {
            assert_eq!(cat(t).as_deref(), Some("family"), "{t}");
        }
    }

    #[test]
    fn personal_finance_positives_zh_and_en() {
        for t in [
            "他還有三百萬房貸",
            "最近卡債很多",
            "投資虧損了一半積蓄",
            "Dave has a lot of credit card debt",
            "She is saving for a mortgage",
            "He filed for bankruptcy",
        ] {
            assert_eq!(cat(t).as_deref(), Some("personal_finance"), "{t}");
        }
    }

    #[test]
    fn salary_positives_zh_and_en() {
        for t in [
            "她的月薪是五萬",
            "年終獎金發了兩個月",
            "老闆答應下季加薪",
            "Erin's salary is 80k",
            "He asked for a pay raise",
            "monthly_salary",
        ] {
            assert_eq!(cat(t).as_deref(), Some("salary"), "{t}");
        }
    }

    #[test]
    fn ordinary_business_facts_do_not_match() {
        for t in [
            "客戶要求下週交貨",
            "會議改到週三下午三點",
            "報價單請用新版模板",
            "電腦中了病毒，請 IT 重灌",
            "這個功能有點毛病，先回報給工程",
            "API 回傳 500 錯誤時要重試三次",
            "請用繁體中文回覆客戶",
            "The customer requested delivery next week",
            "Export the report as comma-separated values",
            "Use the salaryman persona only in the demo",
            "The quarterly revenue target is 2M",
            "Alice prefers Python for scripting",
        ] {
            assert_eq!(cat(t), None, "{t}");
        }
    }

    /// The documented trade-off: a work fact that names the topic is dropped.
    #[test]
    fn topic_level_trade_off_is_explicit() {
        assert_eq!(cat("健康檢查報告模板放在共用槽").as_deref(), Some("health"));
        assert_eq!(
            cat("The bank client launched a new loan product").as_deref(),
            Some("personal_finance")
        );
    }

    #[test]
    fn check_looks_at_every_part_separately() {
        let g = gate();
        let v = g.check(&FactParts {
            content: "Alice's preference",
            subject: Some("user:alice"),
            predicate: Some("monthly_salary"),
            object: Some("50000"),
        });
        assert_eq!(v, Verdict::Excluded("salary"));
        let v = g.check(&FactParts {
            content: "Alice prefers tea",
            subject: Some("user:alice"),
            predicate: Some("prefers_drink"),
            object: Some("tea"),
        });
        assert_eq!(v, Verdict::Allow);
    }

    #[test]
    fn missing_section_and_missing_file_mean_defaults() {
        let home = tempfile::tempdir().unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home.path());
        assert_eq!((cfg.clone(), st), (MemoryPrivacyConfig::default(), LoadState::Defaults));
        assert_eq!(cfg.excluded_categories, BUILTIN_CATEGORIES);

        std::fs::write(home.path().join("config.toml"), "[gateway]\nport = 18889\n").unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home.path());
        assert_eq!((cfg, st), (MemoryPrivacyConfig::default(), LoadState::Defaults));
    }

    #[test]
    fn unreadable_or_invalid_config_fails_closed_to_defaults() {
        let home = tempfile::tempdir().unwrap();
        // Admin turned everything off …
        std::fs::write(
            home.path().join("config.toml"),
            "[memory_privacy]\nexcluded_categories = []\n",
        )
        .unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home.path());
        assert_eq!(st, LoadState::Configured);
        assert!(PrivacyGate::from_config(&cfg).categories().is_empty());

        // … but a broken file can only make it stricter.
        std::fs::write(home.path().join("config.toml"), "[memory_privacy\nexcluded = ").unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home.path());
        assert_eq!(st, LoadState::FailedClosed);
        assert_eq!(cfg, MemoryPrivacyConfig::default());

        // A directory where the file should be: read error, not NotFound.
        let home2 = tempfile::tempdir().unwrap();
        std::fs::create_dir(home2.path().join("config.toml")).unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home2.path());
        assert_eq!(st, LoadState::FailedClosed);
        assert_eq!(PrivacyGate::from_config(&cfg).categories(), BUILTIN_CATEGORIES);

        // An invalid section (unknown category) also fails closed.
        std::fs::write(
            home.path().join("config.toml"),
            "[memory_privacy]\nexcluded_categories = [\"religion\"]\n",
        )
        .unwrap();
        let (cfg, st) = MemoryPrivacyConfig::load_with_state(home.path());
        assert_eq!(st, LoadState::FailedClosed);
        assert_eq!(cfg, MemoryPrivacyConfig::default());
    }

    #[test]
    fn validation_rejects_unknown_categories_unless_custom() {
        let parse = |s: &str| MemoryPrivacyConfig::from_table(&s.parse().unwrap());
        assert!(parse("[memory_privacy]\nexcluded_categories = [\"religion\"]\n").is_err());
        let cfg = parse(
            "[memory_privacy]\nexcluded_categories = [\"health\", \"legal_dispute\"]\n\
             [memory_privacy.custom_keywords]\nlegal_dispute = [\"訴訟\", \"lawsuit\"]\nhealth = [\"復健\"]\n",
        )
        .unwrap();
        let g = PrivacyGate::from_config(&cfg);
        assert_eq!(g.categories(), vec!["health", "legal_dispute"]);
        assert_eq!(g.classify("他正在打離職訴訟"), Some("legal_dispute"));
        assert_eq!(g.classify("每週二下午去復健"), Some("health"));
        // Unticked built-ins no longer filter.
        assert_eq!(g.classify("她的月薪是五萬"), None);

        assert!(parse("[memory_privacy]\nexcluded_categories = [\"health\", \"health\"]\n").is_err());
        assert!(parse("[memory_privacy]\n[memory_privacy.custom_keywords]\nBad-Name = [\"x1\"]\n").is_err());
        assert!(parse("[memory_privacy]\n[memory_privacy.custom_keywords]\nok = [\"a\"]\n").is_err());
        assert!(parse("[memory_privacy]\n[memory_privacy.custom_keywords]\nok = [\" \"]\n").is_err());
        assert!(parse("[memory_privacy]\nexcluded_categories = \"health\"\n").is_err());
        assert!(parse("[memory_privacy]\nmystery = 1\n").is_err());
        // An empty custom list does not define a category.
        assert!(parse(
            "[memory_privacy]\nexcluded_categories = [\"x_topic\"]\n[memory_privacy.custom_keywords]\nx_topic = []\n"
        )
        .is_err());
    }

    #[test]
    fn prompt_instruction_lists_active_categories() {
        let line = gate().prompt_instruction().unwrap();
        assert!(line.contains("health") && line.contains("salary"), "{line}");
        assert!(line.contains("健康") && line.contains("薪資"), "{line}");
        let off = PrivacyGate::from_config(&MemoryPrivacyConfig {
            excluded_categories: vec![],
            custom_keywords: BTreeMap::new(),
        });
        assert_eq!(off.prompt_instruction(), None);
    }

    #[test]
    fn forgotten_keys_block_the_same_triple_only() {
        let home = tempfile::tempdir().unwrap();
        forgotten::record(home.path(), &forgotten::key("user:u1", "preferred_name", "老李")).unwrap();
        let g = PrivacyGate::for_home(home.path());
        let same = FactParts {
            content: "preferred_name: 老李",
            subject: Some("user:u1"),
            predicate: Some("preferred_name"),
            object: Some("  老李 "),
        };
        assert_eq!(g.check(&same), Verdict::Forgotten);
        let updated = FactParts { object: Some("小李"), ..same };
        assert_eq!(g.check(&updated), Verdict::Allow);
        let other_user = FactParts { subject: Some("user:u2"), ..same };
        assert_eq!(g.check(&other_user), Verdict::Allow);
        let raw = std::fs::read_to_string(home.path().join(forgotten::FILE)).unwrap();
        assert!(!raw.contains("老李"), "only digests are kept");
    }

    #[test]
    fn filter_report_counts_without_content() {
        let g = gate();
        let mut r = FilterReport::default();
        assert!(!r.record(g.check(&FactParts { content: "他下週住院", ..Default::default() })));
        assert!(r.record(g.check(&FactParts { content: "客戶要求下週交貨", ..Default::default() })));
        assert_eq!(r.excluded, 1);
        assert_eq!(r.by_category.get("health"), Some(&1));
    }

    #[test]
    fn self_subjects_cover_dashboard_webchat_and_verified_channels() {
        let subjects = self_subjects(
            "u-alice",
            &[("line".into(), "U123".into()), ("telegram".into(), "998".into())],
        );
        assert!(subjects.contains(&"user:u-alice".to_string()));
        assert!(subjects.contains(&format!("user:webchat:{}", crate::webchat::webchat_owner_tag("u-alice"))));
        assert!(subjects.contains(&"user:U123".to_string()));
        assert!(subjects.contains(&"user:line:U123".to_string()));
        assert!(subjects.contains(&"user:998".to_string()));
        assert!(self_subjects("  ", &[]).is_empty());
        let anon = self_subjects("u1", &[("line".into(), "system".into())]);
        assert!(!anon.contains(&"user:system".to_string()));
    }
}
