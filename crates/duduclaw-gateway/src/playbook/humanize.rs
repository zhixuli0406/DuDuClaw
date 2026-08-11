//! W3-2 — 「經驗法則」口語化改寫層(D-O3 = A:一等物件曝光)。
//!
//! A playbook entry's `content` is written **for the model**: a ≤400-char
//! condition-action rule whose trigger vocabulary (`signals_match`) is a set
//! of namespaced machine tokens (`mistake:capability`, `channel:telegram`,
//! `kw:訂單`…). Rendering that verbatim in a dashboard card or a chat reply
//! reads like machine output, which is exactly the black box this product
//! claims not to be.
//!
//! This module is the translation layer: **pure functions, zero LLM calls**,
//! deterministic zh-TW template assembly from `category` + `signals_match` +
//! `strategy` (+ `content` as the action source while `strategy` stays empty
//! in phase 1). Because it never calls a model it can run on every list/RPC
//! read without a cost or latency budget.
//!
//! Two hard rules this module exists to enforce:
//!
//! 1. **Fail-safe, never fail-silent.** When the templates cannot build a
//!    readable sentence (empty content, unparseable shape), the result keeps
//!    the raw content and sets [`HumanizedRule::fallback`] so the caller can
//!    label it — a wrong-but-fluent paraphrase would be worse than the raw
//!    text (誠實回報 > 假性完成).
//! 2. **No internal vocabulary escapes.** "playbook" / "shadow" / "probation"
//!    / "GVU" never appear in any string produced here. The status vocabulary
//!    is the UX spec's §C.9 wording (觀察中/試用中/生效中/已淘汰/…) and every
//!    label also ships a stable `*_key` so the front end can localize instead
//!    of echoing the zh-TW string.

use duduclaw_core::truncate_chars;

use super::entry::{PlaybookCategory, PlaybookMeta, PlaybookState};
use crate::prediction::rule_lifecycle::{
    RuleStats, PROBATION_RULE_TAG, RETIRED_RULE_TAG, SHADOW_RULE_TAG,
};

/// Marker appended to a fallback rendering so the reader knows they are
/// looking at the raw rule text, not a rewrite.
pub const FALLBACK_NOTE: &str = "（原始內容，尚無法自動改寫成白話）";

/// Shown when the entry has no usable content at all.
pub const EMPTY_CONTENT_TEXT: &str = "（這條經驗法則沒有內容）";

/// Max `char`s of the derived action clause before it is elided.
const ACTION_MAX_CHARS: usize = 80;
/// Max condition fragments joined into one sentence.
const CONDITION_MAX_FRAGMENTS: usize = 3;
/// Max strategy steps rendered into the action clause.
const STRATEGY_MAX_STEPS: usize = 3;

/// Deterministic, human-readable rendering of one experience rule.
///
/// Every `*_key` field is a stable machine key (never localized) so a UI can
/// translate; every non-key field is ready-to-show zh-TW (used verbatim by
/// the channel `/rules` command, which has no i18n runtime).
#[derive(Debug, Clone, PartialEq)]
pub struct HumanizedRule {
    /// One-sentence zh-TW rendering, e.g.
    /// 「當任務出現〈做不到、能力不足〉時,我會〈先確認手上有哪些工具〉。」
    pub sentence: String,
    /// The 「什麼時候會用到」 clause on its own (may be empty).
    pub condition: String,
    /// The 「我會怎麼做」 clause on its own (may be empty when `fallback`).
    pub action: String,
    /// Plain-language purpose of the rule (from `category`).
    pub purpose: String,
    /// Stable key for `purpose` (`repair` / `optimize` / …).
    pub purpose_key: &'static str,
    /// §C.9 status label (觀察中(尚未生效) / 試用中 / 生效中 / …).
    pub status: String,
    /// Stable key for `status` — see [`status_of`].
    pub status_key: &'static str,
    /// 「為什麼有這條」 — provenance sentence assembled from the evidence counts.
    pub why: String,
    /// Machine-readable evidence behind `why`.
    pub evidence: RuleEvidence,
    /// `true` ⇒ the templates could not build a sentence and `sentence`
    /// carries the raw content plus [`FALLBACK_NOTE`].
    pub fallback: bool,
}

/// Counts behind [`HumanizedRule::why`]. Deliberately counts only — the
/// underlying ids (`derived_from`, mistake ids) are internal breadcrumbs and
/// must never reach a user surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RuleEvidence {
    /// Linked eval cases (the 「驗收案例」 that keep the rule honest).
    pub eval_cases: usize,
    /// Recorded failures that motivated / challenged the rule.
    pub failure_notes: usize,
    /// Times the rule was applied and settled.
    pub applications: usize,
    pub helpful: u32,
    pub harmful: u32,
    pub success_streak: u32,
}

/// Plain-language purpose for a category. Returns `(key, label)`.
pub fn purpose_of(category: PlaybookCategory) -> (&'static str, &'static str) {
    match category {
        PlaybookCategory::Repair => ("repair", "修正過去犯過的錯"),
        PlaybookCategory::Optimize => ("optimize", "把既有做法做得更好"),
        PlaybookCategory::Innovate => ("innovate", "嘗試新的做法"),
        PlaybookCategory::Regulatory => ("regulatory", "必須遵守的規定"),
        PlaybookCategory::Explore => ("explore", "探索性的嘗試"),
    }
}

/// §C.9 status vocabulary. Returns `(key, label)`.
///
/// Derivation order matters and is deliberately the same precedence the
/// selection path uses: a shadow candidate is invisible to injection even if
/// it also carries a probation tag, and a retired entry beats everything.
/// `meta.state` is authoritative only for `Stale` (see [`PlaybookState`]);
/// Probation/Retired are read from the row's tags exactly like selection does.
pub fn status_of(state: PlaybookState, tags: &[String]) -> (&'static str, &'static str) {
    let has = |t: &str| tags.iter().any(|x| x == t);
    if has(RETIRED_RULE_TAG) || state == PlaybookState::Retired {
        return ("retired", "已淘汰");
    }
    if has(SHADOW_RULE_TAG) {
        return ("observing", "觀察中（尚未生效）");
    }
    if state == PlaybookState::Stale {
        return ("dormant", "很久沒用到，已收起來");
    }
    if has(PROBATION_RULE_TAG) || state == PlaybookState::Probation {
        return ("trial", "試用中");
    }
    ("active", "生效中")
}

/// Render one namespaced signal token as a zh-TW condition fragment.
/// Unknown namespaces degrade to a quoted echo of the value — never to an
/// empty string, so a rule is never silently shown as unconditional.
pub fn describe_signal(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }
    if token == "*" {
        return Some("任何任務".to_string());
    }
    let (ns, value) = match token.split_once(':') {
        Some((ns, v)) if !v.trim().is_empty() => (ns, v.trim()),
        // A bare (non-namespaced) token is legacy/free-form: echo it.
        _ => return Some(format!("提到「{}」", truncate_chars(token, 24))),
    };
    let value_short = truncate_chars(value, 24);
    let text = match ns {
        "mistake" => match value {
            "factual" => "講錯事實".to_string(),
            "behavioral" => "應對方式不恰當".to_string(),
            "capability" => "做不到、能力不足".to_string(),
            "safety" => "踩到安全界線".to_string(),
            "hallucination" => "說了沒真的做過的事".to_string(),
            _ => format!("出現「{value_short}」這類問題"),
        },
        "source_kind" => match value {
            "decision_gap" => "該做的決定沒做".to_string(),
            "task_failure" => "任務執行失敗".to_string(),
            "unattributed" => "來源不明的問題".to_string(),
            _ => format!("出現「{value_short}」這類狀況"),
        },
        "error" => match value {
            "significant" => "明顯出錯".to_string(),
            "critical" => "嚴重出錯".to_string(),
            _ => format!("出現「{value_short}」等級的錯誤"),
        },
        "failure" => match value {
            "rate_limited" => "用量達到上限".to_string(),
            "billing" => "帳戶額度用完".to_string(),
            "auth_failed" => "帳號登入失效".to_string(),
            "timeout" => "等太久逾時".to_string(),
            "empty_response" => "沒有產出回覆".to_string(),
            "spawn_error" => "程式啟動失敗".to_string(),
            "binary_missing" => "找不到可執行的程式".to_string(),
            "no_accounts" => "沒有可用的帳號".to_string(),
            "accounts_cooling_down_long"
            | "accounts_cooling_down_short"
            | "accounts_cooling_down_unknown" => "帳號正在冷卻".to_string(),
            "unknown" => "發生原因不明的失敗".to_string(),
            _ => format!("發生「{value_short}」的失敗"),
        },
        "channel" => format!("在{}上對話", channel_display(value)),
        "tool" => format!("用到「{value_short}」這項工具"),
        "kw" => format!("提到「{value_short}」"),
        _ => format!("符合「{value_short}」的情況"),
    };
    Some(text)
}

/// User-facing platform name. Unknown platforms echo the raw value so a new
/// channel is never rendered as an empty string.
fn channel_display(value: &str) -> String {
    match value {
        "telegram" => "Telegram".to_string(),
        "line" => "LINE".to_string(),
        "discord" => "Discord".to_string(),
        "slack" => "Slack".to_string(),
        "whatsapp" => "WhatsApp".to_string(),
        "feishu" => "飛書".to_string(),
        "googlechat" | "google_chat" => "Google Chat".to_string(),
        "msteams" | "teams" => "Microsoft Teams".to_string(),
        "wecom" => "企業微信".to_string(),
        "dingtalk" => "釘釘".to_string(),
        "email" => "Email".to_string(),
        "webchat" => "網頁對話".to_string(),
        other => truncate_chars(other, 24),
    }
}

/// Join up to [`CONDITION_MAX_FRAGMENTS`] signal descriptions into one
/// clause. The `*` wildcard is dropped whenever a concrete signal exists —
/// 「任何任務或提到「訂單」」 reads as a contradiction.
pub fn describe_signals(signals: &[String]) -> String {
    let mut concrete: Vec<String> = Vec::new();
    let mut wildcard = false;
    for s in signals {
        if s.trim() == "*" {
            wildcard = true;
            continue;
        }
        if let Some(d) = describe_signal(s) {
            if !concrete.contains(&d) {
                concrete.push(d);
            }
        }
    }
    if concrete.is_empty() {
        return if wildcard { "任何任務".to_string() } else { String::new() };
    }
    let elided = concrete.len() > CONDITION_MAX_FRAGMENTS;
    concrete.truncate(CONDITION_MAX_FRAGMENTS);
    let joined = concrete.join("，或");
    if elided {
        format!("{joined}等情況")
    } else {
        joined
    }
}

/// Leading fillers stripped from a derived action so the template does not
/// produce 「我會 我會…」. Longest-first; only stripped when something is left.
const ACTION_LEAD_STRIP: &[&str] = &[
    "我必須要", "我應該要", "我必須", "我應該", "應該要", "我會要", "必須要", "我就會", "我會", "應該",
    "必須", "就要", "則要", "就會", "則會",
];

/// Markers that end an embedded condition clause inside `content`. Rules are
/// commonly authored as 「當…時,先…」 — splitting there avoids emitting a
/// doubled condition (「當 X 時,我會當 X 時先 Y」).
const CONDITION_END_MARKERS: &[&str] = &["時，", "時,", "時：", "時:", "時、", "時應", "時要", "時就"];

/// Sentence terminators used to take only the first clause of `content`.
const SENTENCE_ENDS: &[char] = &['。', '！', '!', '？', '?', '\n', '；', ';'];

/// Build the 「我會…」 clause. Prefers the explicit `strategy` steps; falls
/// back to the first sentence of `content`. `None` ⇒ nothing usable, caller
/// must degrade to the raw-content fallback.
pub fn derive_action(content: &str, strategy: &[String]) -> Option<String> {
    let steps: Vec<String> = strategy
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, ACTION_MAX_CHARS))
        .collect();
    if !steps.is_empty() {
        let elided = steps.len() > STRATEGY_MAX_STEPS;
        let mut steps: Vec<String> = steps.into_iter().take(STRATEGY_MAX_STEPS).collect();
        if steps.len() == 1 {
            let only = strip_action_lead(&steps.remove(0));
            return (!only.is_empty()).then_some(only);
        }
        let first = strip_action_lead(&steps.remove(0));
        let rest = steps.join("，接著");
        let mut out = if first.is_empty() {
            rest
        } else {
            format!("先{first}，接著{rest}")
        };
        if elided {
            out.push_str("，之後依序完成剩下的步驟");
        }
        return (!out.is_empty()).then_some(out);
    }

    let first_sentence = content
        .split(SENTENCE_ENDS)
        .map(str::trim)
        .find(|s| !s.is_empty())?;

    // Drop an embedded condition clause if the author wrote one.
    let tail = CONDITION_END_MARKERS
        .iter()
        .filter_map(|m| first_sentence.find(m).map(|i| (i, m)))
        .min_by_key(|(i, _)| *i)
        .map(|(i, m)| {
            // The whole marker is dropped: for the punctuation forms that is
            // just 「時，」; for 時應/時要/時就 it also drops the modal verb the
            // 「我會」 template is about to supply, so the action reads as an
            // action and not as 「我會應先確認」.
            first_sentence[i + m.len()..].trim()
        })
        .filter(|t| !t.is_empty())
        .unwrap_or(first_sentence);

    let stripped = strip_action_lead(tail);
    if stripped.is_empty() {
        return None;
    }
    Some(truncate_chars(&stripped, ACTION_MAX_CHARS))
}

fn strip_action_lead(s: &str) -> String {
    let mut cur = s.trim();
    // Strip repeatedly: 「我會就要先…」 is rare but cheap to handle.
    loop {
        let mut changed = false;
        for lead in ACTION_LEAD_STRIP {
            if let Some(rest) = cur.strip_prefix(lead) {
                let rest = rest.trim_start_matches(['，', ',', '：', ':', ' ']).trim();
                if !rest.is_empty() {
                    cur = rest;
                    changed = true;
                    break;
                }
            }
        }
        if !changed {
            break;
        }
    }
    cur.to_string()
}

/// 「為什麼有這條」 — provenance assembled from counts only.
pub fn describe_why(evidence: &RuleEvidence, category: PlaybookCategory) -> String {
    let mut parts: Vec<String> = Vec::new();
    parts.push(
        match category {
            PlaybookCategory::Repair => "從過去做錯的地方歸納出來",
            PlaybookCategory::Optimize => "為了把既有做法再磨得更好",
            PlaybookCategory::Innovate => "主動嘗試的新做法",
            PlaybookCategory::Regulatory => "由管理者訂下的規定",
            PlaybookCategory::Explore => "探索性的嘗試",
        }
        .to_string(),
    );
    if evidence.failure_notes > 0 {
        parts.push(format!("記錄了 {} 次相關失敗", evidence.failure_notes));
    }
    if evidence.eval_cases > 0 {
        parts.push(format!("有 {} 個驗收案例把關", evidence.eval_cases));
    } else {
        parts.push("目前還沒有驗收案例把關".to_string());
    }
    if evidence.applications > 0 || evidence.helpful > 0 || evidence.harmful > 0 {
        parts.push(format!(
            "實際用過 {} 次，其中 {} 次有幫助、{} 次反而幫倒忙",
            evidence.applications.max((evidence.helpful + evidence.harmful) as usize),
            evidence.helpful,
            evidence.harmful
        ));
    } else {
        parts.push("還沒有實際使用紀錄".to_string());
    }
    if evidence.success_streak > 0 {
        parts.push(format!("已連續 {} 次沒出問題", evidence.success_streak));
    }
    format!("{}。", parts.join("；"))
}

/// The module's front door: turn one stored entry into its human rendering.
///
/// `tags` are the memory row's tags (used for the §C.9 status derivation);
/// pass an empty slice when the caller only has the metadata blob — the
/// status then falls back to `meta.state`.
pub fn humanize(
    content: &str,
    meta: &PlaybookMeta,
    stats: &RuleStats,
    tags: &[String],
) -> HumanizedRule {
    let (purpose_key, purpose) = purpose_of(meta.category);
    let (status_key, status) = status_of(meta.state, tags);
    let evidence = RuleEvidence {
        eval_cases: meta.eval_cases.len(),
        failure_notes: meta.failure_history.len(),
        applications: meta.applications.len(),
        helpful: stats.helpful,
        harmful: stats.harmful,
        success_streak: meta.success_streak,
    };
    let why = describe_why(&evidence, meta.category);
    let condition = describe_signals(&meta.signals_match);
    let trimmed_content = content.trim();

    let action = derive_action(trimmed_content, &meta.strategy);
    let (sentence, action, fallback) = match action {
        Some(a) if !condition.is_empty() => {
            (format!("當{condition}時，我會{a}。"), a, false)
        }
        Some(a) => (format!("任何情況下，我都會{a}。"), a, false),
        None => {
            let raw = if trimmed_content.is_empty() {
                EMPTY_CONTENT_TEXT.to_string()
            } else {
                format!("{trimmed_content}\n{FALLBACK_NOTE}")
            };
            (raw, String::new(), true)
        }
    };

    HumanizedRule {
        sentence,
        condition,
        action,
        purpose: purpose.to_string(),
        purpose_key,
        status: status.to_string(),
        status_key,
        why,
        evidence,
        fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::playbook::entry::{Application, EvalCaseRef, FailureNote};

    fn meta_with(
        category: PlaybookCategory,
        signals: &[&str],
        strategy: &[&str],
        state: PlaybookState,
    ) -> PlaybookMeta {
        let mut m = PlaybookMeta::legacy_default(&[], "seed");
        m.category = category;
        m.signals_match = signals.iter().map(|s| s.to_string()).collect();
        m.strategy = strategy.iter().map(|s| s.to_string()).collect();
        m.state = state;
        m
    }

    // ── condition assembly ──────────────────────────────────────────

    #[test]
    fn every_signal_namespace_renders_without_internal_vocabulary() {
        for token in [
            "mistake:factual",
            "mistake:behavioral",
            "mistake:capability",
            "mistake:safety",
            "mistake:hallucination",
            "source_kind:decision_gap",
            "source_kind:task_failure",
            "source_kind:unattributed",
            "error:significant",
            "error:critical",
            "failure:rate_limited",
            "failure:timeout",
            "channel:telegram",
            "tool:memory_search",
            "kw:訂單",
            "*",
        ] {
            let d = describe_signal(token).unwrap_or_else(|| panic!("no rendering for {token}"));
            assert!(!d.is_empty(), "{token} rendered empty");
            assert!(!d.contains(':'), "{token} leaked a namespaced token: {d}");
            for internal in ["playbook", "shadow", "probation", "GVU", "mistake:"] {
                assert!(!d.contains(internal), "{token} leaked `{internal}`: {d}");
            }
        }
    }

    #[test]
    fn unknown_signal_value_degrades_to_a_quoted_echo_never_empty() {
        let d = describe_signal("mistake:brand_new_category").unwrap();
        assert!(d.contains("brand_new_category"));
        let d = describe_signal("totally_new_namespace:whatever").unwrap();
        assert!(d.contains("whatever"));
        // A bare token is still rendered.
        assert!(describe_signal("legacy-token").is_some());
        // Only genuinely empty input yields None.
        assert!(describe_signal("   ").is_none());
    }

    #[test]
    fn wildcard_is_dropped_when_a_concrete_signal_exists() {
        let c = describe_signals(&["*".into(), "kw:訂單".into()]);
        assert!(!c.contains("任何任務"), "{c}");
        assert!(c.contains("訂單"));
        // Wildcard alone still renders.
        assert_eq!(describe_signals(&["*".into()]), "任何任務");
        assert_eq!(describe_signals(&[]), "");
    }

    #[test]
    fn condition_caps_at_three_fragments_and_marks_elision() {
        let c = describe_signals(&[
            "kw:訂單".into(),
            "kw:退款".into(),
            "kw:發票".into(),
            "kw:運費".into(),
        ]);
        assert!(c.ends_with("等情況"), "{c}");
        assert!(!c.contains("運費"), "{c}");
    }

    // ── action derivation ───────────────────────────────────────────

    #[test]
    fn strategy_steps_win_over_content() {
        let a = derive_action("忽略我", &["查資料庫".into(), "回覆使用者".into()]).unwrap();
        assert_eq!(a, "先查資料庫，接著回覆使用者");
    }

    #[test]
    fn single_strategy_step_has_no_ordering_prefix() {
        assert_eq!(derive_action("x", &["直接回報給管理員".into()]).unwrap(), "直接回報給管理員");
    }

    #[test]
    fn content_embedded_condition_clause_is_dropped_to_avoid_doubling() {
        let a = derive_action("當使用者詢問訂單狀態時，先查詢資料庫再回覆。其他情況照舊。", &[])
            .unwrap();
        assert_eq!(a, "先查詢資料庫再回覆");
        assert!(!a.contains("當使用者"));
    }

    #[test]
    fn fullwidth_punctuation_is_what_real_zh_tw_rules_actually_use() {
        // The ASCII forms were the only ones handled at first — a rule
        // written the way a human writes Chinese («當…時，先…») would have
        // fallen through and produced a doubled condition.
        let a = derive_action("當使用者詢問訂單狀態時，先查詢資料庫再回覆。", &[]).unwrap();
        assert_eq!(a, "先查詢資料庫再回覆");
        let a = derive_action("當額度用完時：改用備援帳號！其他照舊", &[]).unwrap();
        assert_eq!(a, "改用備援帳號");
    }

    #[test]
    fn a_modal_verb_marker_is_consumed_so_the_template_reads_naturally() {
        // 「時應」/「時要」/「時就」 carry the modal the 「我會」 template is
        // about to supply; keeping it would yield 「我會應先確認」.
        assert_eq!(derive_action("遇到不確定的事時應先確認再回答。", &[]).unwrap(), "先確認再回答");
        assert_eq!(derive_action("被追問細節時要如實說明。", &[]).unwrap(), "如實說明");
    }

    #[test]
    fn action_lead_fillers_are_stripped_so_the_template_never_doubles_the_verb() {
        assert_eq!(derive_action("我會主動確認需求", &[]).unwrap(), "主動確認需求");
        assert_eq!(derive_action("應該要先問清楚再動手", &[]).unwrap(), "先問清楚再動手");
    }

    #[test]
    fn long_action_is_truncated_on_a_char_boundary() {
        let long = "確認".repeat(200);
        let a = derive_action(&long, &[]).unwrap();
        assert!(a.chars().count() <= ACTION_MAX_CHARS);
        // Round-trips as valid UTF-8 (no mid-char byte slicing).
        assert_eq!(a, String::from_utf8(a.clone().into_bytes()).unwrap());
    }

    #[test]
    fn empty_or_filler_only_content_yields_no_action() {
        assert!(derive_action("", &[]).is_none());
        assert!(derive_action("   \n  ", &[]).is_none());
        assert!(derive_action("我會", &[]).is_some()); // nothing left to strip → keeps raw
    }

    // ── whole-entry assembly, per category ──────────────────────────

    #[test]
    fn each_category_assembles_a_full_sentence() {
        let stats = RuleStats { helpful: 2, harmful: 0 };
        for (cat, key) in [
            (PlaybookCategory::Repair, "repair"),
            (PlaybookCategory::Optimize, "optimize"),
            (PlaybookCategory::Innovate, "innovate"),
            (PlaybookCategory::Regulatory, "regulatory"),
            (PlaybookCategory::Explore, "explore"),
        ] {
            let meta = meta_with(cat, &["mistake:capability"], &[], PlaybookState::Active);
            let h = humanize("先確認手上有哪些工具再答應。", &meta, &stats, &[]);
            assert_eq!(h.purpose_key, key);
            assert!(!h.fallback, "{key} unexpectedly fell back");
            assert!(h.sentence.starts_with("當"), "{}", h.sentence);
            assert!(h.sentence.contains("我會"), "{}", h.sentence);
            assert!(h.sentence.ends_with("。"), "{}", h.sentence);
            assert!(h.sentence.contains("做不到、能力不足"), "{}", h.sentence);
            assert!(!h.why.is_empty());
        }
    }

    #[test]
    fn no_signals_produces_the_unconditional_template_not_a_dangling_when() {
        let meta = meta_with(PlaybookCategory::Repair, &[], &[], PlaybookState::Active);
        let h = humanize("主動回報進度。", &meta, &RuleStats::default(), &[]);
        assert!(!h.fallback);
        assert_eq!(h.condition, "");
        assert!(h.sentence.starts_with("任何情況下"), "{}", h.sentence);
    }

    #[test]
    fn fail_safe_keeps_raw_content_and_flags_it() {
        let meta = meta_with(PlaybookCategory::Repair, &["*"], &[], PlaybookState::Active);
        // Content that yields no first sentence at all.
        let h = humanize("。。。", &meta, &RuleStats::default(), &[]);
        assert!(h.fallback);
        assert!(h.sentence.contains(FALLBACK_NOTE), "{}", h.sentence);
        assert!(h.sentence.contains("。。。"), "{}", h.sentence);
        assert_eq!(h.action, "");
    }

    #[test]
    fn empty_content_fail_safe_says_so_instead_of_rendering_a_blank_card() {
        let meta = meta_with(PlaybookCategory::Repair, &["*"], &[], PlaybookState::Active);
        let h = humanize("   ", &meta, &RuleStats::default(), &[]);
        assert!(h.fallback);
        assert_eq!(h.sentence, EMPTY_CONTENT_TEXT);
    }

    // ── §C.9 status vocabulary ──────────────────────────────────────

    #[test]
    fn status_follows_the_c9_vocabulary_and_selection_precedence() {
        assert_eq!(status_of(PlaybookState::Active, &[]), ("active", "生效中"));
        assert_eq!(
            status_of(PlaybookState::Probation, &[]),
            ("trial", "試用中")
        );
        assert_eq!(
            status_of(PlaybookState::Active, &[PROBATION_RULE_TAG.to_string()]),
            ("trial", "試用中")
        );
        assert_eq!(
            status_of(PlaybookState::Active, &[SHADOW_RULE_TAG.to_string()]),
            ("observing", "觀察中（尚未生效）")
        );
        assert_eq!(
            status_of(PlaybookState::Stale, &[]),
            ("dormant", "很久沒用到，已收起來")
        );
        assert_eq!(status_of(PlaybookState::Retired, &[]), ("retired", "已淘汰"));
        // Retired beats shadow beats probation.
        assert_eq!(
            status_of(
                PlaybookState::Probation,
                &[SHADOW_RULE_TAG.to_string(), RETIRED_RULE_TAG.to_string()]
            ),
            ("retired", "已淘汰")
        );
    }

    #[test]
    fn no_produced_string_leaks_internal_vocabulary() {
        let mut meta = meta_with(
            PlaybookCategory::Innovate,
            &["mistake:safety", "channel:telegram"],
            &["先徵求同意".into()],
            PlaybookState::Probation,
        );
        meta.eval_cases = vec![EvalCaseRef("suite/case".into())];
        meta.failure_history = vec![FailureNote {
            at: "2026-08-11T00:00:00Z".into(),
            what: "x".into(),
            source: "G-Contract".into(),
        }];
        meta.applications = vec![Application {
            at: "2026-08-11T00:00:00Z".into(),
            outcome: "negligible".into(),
            score: 1.0,
            ctx: None,
        }];
        let h = humanize(
            "動手前先徵求同意。",
            &meta,
            &RuleStats { helpful: 3, harmful: 1 },
            &[SHADOW_RULE_TAG.to_string()],
        );
        let all = format!("{} {} {} {} {}", h.sentence, h.condition, h.action, h.purpose, h.why);
        for internal in [
            "playbook", "Playbook", "shadow", "Shadow", "probation", "Probation", "GVU", "gvu",
            "AEE", "held-out", "eval_case", "signals_match",
        ] {
            assert!(!all.contains(internal), "leaked `{internal}` in: {all}");
        }
        assert_eq!(h.status_key, "observing");
    }

    #[test]
    fn why_reports_missing_evidence_honestly_instead_of_staying_silent() {
        let meta = meta_with(PlaybookCategory::Repair, &["*"], &[], PlaybookState::Active);
        let h = humanize("先問再答。", &meta, &RuleStats::default(), &[]);
        assert!(h.why.contains("還沒有驗收案例把關"), "{}", h.why);
        assert!(h.why.contains("還沒有實際使用紀錄"), "{}", h.why);
        assert_eq!(h.evidence, RuleEvidence::default());
    }

    #[test]
    fn why_counts_applications_helpful_and_harmful() {
        let mut meta = meta_with(PlaybookCategory::Repair, &["*"], &[], PlaybookState::Active);
        meta.eval_cases = vec![EvalCaseRef("s/c".into())];
        meta.success_streak = 4;
        meta.applications = (0..3)
            .map(|_| Application {
                at: "2026-08-11T00:00:00Z".into(),
                outcome: "negligible".into(),
                score: 1.0,
                ctx: None,
            })
            .collect();
        let h = humanize("先問再答。", &meta, &RuleStats { helpful: 3, harmful: 1 }, &[]);
        assert!(h.why.contains("1 個驗收案例"), "{}", h.why);
        assert!(h.why.contains("4 次沒出問題"), "{}", h.why);
        assert_eq!(h.evidence.applications, 3);
        assert_eq!(h.evidence.helpful, 3);
    }

    #[test]
    fn humanize_is_deterministic_and_allocation_stable() {
        let meta = meta_with(
            PlaybookCategory::Repair,
            &["kw:訂單", "channel:line"],
            &[],
            PlaybookState::Active,
        );
        let stats = RuleStats { helpful: 1, harmful: 0 };
        let a = humanize("查完再回。", &meta, &stats, &[]);
        let b = humanize("查完再回。", &meta, &stats, &[]);
        assert_eq!(a, b);
    }
}
