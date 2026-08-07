//! WP2.4 — the **Gate** half of the Gate/Measure split
//! (`commercial/docs/DESIGN-evolution-v3-aee.md` §2.2).
//!
//! A Gate is deterministic, costs zero LLM tokens, and **keeps its veto**.
//! Everything that is actually a *quality judgement* (LLM judge, similarity
//! to a rolled-back version, mistake relevance, sycophancy style) was moved
//! out to [`super::verifier_measure`] where it becomes a score dimension with
//! no veto — that separation is the whole point of §2.1's eight-layer audit:
//! the pre-WP2.4 chain gave a one-vote veto to two heuristics (L2's 0.5
//! Jaccard threshold, L3's 0.7 judge score) that have no empirical backing,
//! which is root cause R6.
//!
//! Ordering matters as much as membership: [`run_gates`] is called **before**
//! any Measure dimension is computed, so a candidate the deterministic layers
//! were always going to reject never pays for a judge call (§2.5.2, B2).
//!
//! Gate membership (§2.2):
//!
//! | Gate | What it checks |
//! |------|----------------|
//! | `G-Safety` | killswitch / human-override / identity rewrites |
//! | `G-Contract` | agent `must_not`, [`DEFAULT_MUST_NOT`], `must_always` state invariant, sensitive patterns, size |
//! | `G-Assertiveness` | folded into `G-Contract` via [`DEFAULT_MUST_NOT`] — "stop correcting the user" is a contract violation, not a style deduction |
//! | `G-Canary-Static` | literal "always say X" instructions that would break a canary |
//! | `G-Schema` | playbook write-time validation — [`crate::playbook::delta::validate_all`] (>= 1 eval case, signal vocabulary, wildcard quota, …) |
//! | `G-Capacity` | [`capacity_headroom`] — never rejects, reports how many entries must be evicted |
//!
//! Fail-closed (CLAUDE.md security convention #4): an unrecognised subject,
//! a detection error, or missing configuration DENIES.

use duduclaw_core::word_contains_ci;

use super::text_gradient::TextGradient;
use super::verifier::CanaryTest;

/// Patterns that used to live in `verify_anti_sycophancy`'s
/// "anti-assertiveness instructions" list (§2.1 row 7). They are **not** a
/// style preference — telling an agent to stop correcting the user is a
/// contract violation, so they are enforced as an always-on `must_not`
/// extension by `G-Contract` instead of costing a deduction on a score
/// dimension that averages away.
///
/// The *sycophancy* patterns proper (`always agree`, `avoid conflict`, …)
/// deliberately stayed behind in `verify_anti_sycophancy` and became the
/// `anti_sycophancy` **Measure** dimension — see
/// [`super::verifier_measure::anti_sycophancy_score`].
pub const DEFAULT_MUST_NOT: &[&str] = &[
    "stop correcting",
    "don't correct the user",
    "do not correct the user",
    "avoid pointing out errors",
    "stop disagreeing",
    "don't point out mistakes",
    "no longer correct",
    // zh-TW
    "\u{4E0D}\u{8981}\u{7CFE}\u{6B63}\u{7528}\u{6236}", // 不要糾正用戶
    "\u{505C}\u{6B62}\u{7CFE}\u{6B63}",                 // 停止糾正
    "\u{4E0D}\u{518D}\u{6307}\u{51FA}\u{932F}\u{8AA4}", // 不再指出錯誤
];

/// Killswitch verbs × nouns (P0). Matched with [`word_contains_ci`] so
/// `taskillswitch` / `risk switch` style substrings cannot trip the gate and,
/// more importantly, so `disable  killswitch` style spacing cannot slip past
/// a naive `contains` of the literal two-word phrase.
const KILLSWITCH_NOUNS: &[&str] = &["killswitch", "emergency_stop", "kill_switch", "kill switch"];
const KILLSWITCH_VERBS: &[&str] = &["remove", "disable", "ignore", "bypass"];
/// zh-TW killswitch phrasings (CJK has no word boundaries, so these are
/// matched as whole phrases — `word_contains_ci`'s ASCII boundary rule is a
/// no-op between CJK codepoints, which is the intended behaviour).
const KILLSWITCH_ZH: &[&str] = &[
    "\u{79FB}\u{9664}\u{7DCA}\u{6025}\u{505C}\u{6B62}", // 移除緊急停止
    "\u{95DC}\u{9589}\u{7DCA}\u{6025}\u{505C}\u{6B62}", // 關閉緊急停止
    "\u{505C}\u{7528}\u{7DCA}\u{6025}\u{505C}\u{6B62}", // 停用緊急停止
];

/// Human-override reduction (P1).
const OVERRIDE_PATTERNS: &[&str] = &[
    "ignore human",
    "override human",
    "autonomous decision",
    "no human approval",
    "skip approval",
    // zh-TW (§2.1 row 1: the pre-WP2.4 list was English-only, so a zh-TW
    // proposal — i.e. most of them — could never trip P1 at all).
    "\u{7121}\u{9700}\u{4EBA}\u{5DE5}\u{6838}\u{51C6}", // 無需人工核准
    "\u{4E0D}\u{9700}\u{8981}\u{4EBA}\u{5DE5}\u{6838}\u{51C6}", // 不需要人工核准
    "\u{8DF3}\u{904E}\u{4EBA}\u{5DE5}\u{5BE9}\u{6838}", // 跳過人工審核
    "\u{7565}\u{904E}\u{4EBA}\u{5DE5}\u{5BE9}\u{6838}", // 略過人工審核
    "\u{81EA}\u{4E3B}\u{6C7A}\u{5B9A}\u{4E0D}\u{7528}\u{554F}", // 自主決定不用問
];

/// Identity rewrite (P3) — only meaningful when SOUL.md actually declares an
/// identity section.
const IDENTITY_PATTERNS: &[&str] = &[
    "replace identity",
    "rewrite identity",
    "change core personality",
    "new identity",
    "override personality",
    // zh-TW
    "\u{6539}\u{5BEB}\u{8EAB}\u{4EFD}", // 改寫身份
    "\u{66FF}\u{63DB}\u{4EBA}\u{683C}", // 替換人格
    "\u{8986}\u{5BEB}\u{4EBA}\u{683C}", // 覆寫人格
];

/// Sensitive credential shapes. Shared with the legacy L1 so both paths agree.
pub(crate) const SENSITIVE_PATTERNS: &[&str] = &[
    "sk-ant-",
    "sk-proj-",
    "api_key=",
    "password=",
    "secret=",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "DISCORD_TOKEN",
    "LINE_CHANNEL_SECRET",
    "TELEGRAM_BOT_TOKEN",
    "token=",
];

/// Hard byte ceiling for a single gated text (mirrors the legacy L1 10KB rule).
pub const CONTENT_MAX_BYTES: usize = 10_000;

/// What [`run_gates`] is asked to clear.
///
/// The same gate battery serves the legacy SOUL path (one proposal body plus
/// a simulated post-apply SOUL.md for the `must_always` state invariant) and
/// the AEE playbook path (N delta contents, no state invariant — a playbook
/// delta cannot remove anything from SOUL.md because it never writes it).
#[derive(Debug, Clone)]
pub struct GateInput<'a> {
    pub agent_id: &'a str,
    /// Every text that will actually take effect. For the SOUL path this is
    /// the patch body (never the human-readable narrative); for the playbook
    /// path it is one entry per `Add`/`Revise` content.
    pub contents: Vec<String>,
    /// Post-apply projection used for the `must_always` STATE invariant.
    /// `None` disables that check — correct for playbook deltas, which never
    /// touch SOUL.md (§2.2: "`must_always` 對 SOUL.md 不適用").
    pub simulated_final: Option<String>,
    /// Text already in force (current SOUL.md). Used to distinguish
    /// *newly introduced* patterns from ones the operator wrote by hand.
    pub current_reference: &'a str,
    /// Agent `CONTRACT.toml` `must_not`. [`DEFAULT_MUST_NOT`] is appended
    /// automatically — callers must not pre-merge it.
    pub must_not: &'a [String],
    pub must_always: &'a [String],
    pub canary_tests: &'a [CanaryTest],
}

/// Run every zero-cost gate, in lexicographic safety order.
///
/// `Ok(advisories)` — nothing vetoed; the advisories are non-blocking hints
/// worth surfacing to the Generator. `Err(gradient)` — vetoed; the gradient
/// names the violated pattern (§2.7: a gate gradient is strictly *more*
/// actionable than the judge feedback it replaces, because it points at the
/// exact rule).
pub fn run_gates(input: &GateInput) -> Result<Vec<TextGradient>, TextGradient> {
    // Fail-closed: an empty candidate is not "trivially safe", it is a
    // malformed candidate.
    if input.contents.is_empty() || input.contents.iter().all(|c| c.trim().is_empty()) {
        return Err(TextGradient::blocking(
            "G-Schema",
            "candidate.content",
            "Candidate carries no non-empty content",
            "Emit at least one concrete change",
        ));
    }

    let mut advisories = Vec::new();

    for content in &input.contents {
        gate_safety(content, input.current_reference)?;
    }
    for content in &input.contents {
        gate_contract(content, input.must_not)?;
    }
    if let Some(final_state) = &input.simulated_final {
        gate_must_always(final_state, input.must_always)?;
    }
    for content in &input.contents {
        advisories.extend(gate_canary_static(content, input.canary_tests)?);
    }

    Ok(advisories)
}

/// G-Safety (P0/P1/P3). Anchored matching + zh-TW coverage (§2.1 row 1 fix).
///
/// `pub(crate)` because the legacy [`super::verifier::verify_lexicographic_safety`]
/// delegates here — one implementation, two entry points, no drift.
pub(crate) fn gate_safety(content: &str, current_reference: &str) -> Result<(), TextGradient> {
    for noun in KILLSWITCH_NOUNS {
        for verb in KILLSWITCH_VERBS {
            let phrase = format!("{verb} {noun}");
            if word_contains_ci(content, &phrase) {
                return Err(TextGradient::blocking(
                    "G-Safety",
                    "killswitch",
                    &format!("Candidate attempts to disable or remove killswitch capability ('{phrase}')"),
                    "Killswitch is a non-negotiable safety mechanism and cannot be modified",
                ));
            }
        }
    }
    for zh in KILLSWITCH_ZH {
        if content.contains(zh) {
            return Err(TextGradient::blocking(
                "G-Safety",
                "killswitch",
                &format!("Candidate attempts to disable the killswitch ('{zh}')"),
                "Killswitch is a non-negotiable safety mechanism and cannot be modified",
            ));
        }
    }

    for pat in OVERRIDE_PATTERNS {
        if word_contains_ci(content, pat) {
            return Err(TextGradient::blocking(
                "G-Safety",
                "human_override",
                &format!("Candidate contains a pattern that reduces human authority: '{pat}'"),
                "Preserve human override capability in every evolution candidate",
            ));
        }
    }

    let declares_identity = current_reference.contains("## [identity]")
        || current_reference.contains("## Identity")
        || current_reference.contains("## \u{8EAB}\u{4EFD}"); // ## 身份
    if declares_identity {
        for pat in IDENTITY_PATTERNS {
            if word_contains_ci(content, pat) {
                return Err(TextGradient::blocking(
                    "G-Safety",
                    "identity_integrity",
                    "Candidate attempts to modify the immutable identity section",
                    "Identity is operator-owned; propose a behavioural rule instead",
                ));
            }
        }
    }

    Ok(())
}

/// G-Contract + G-Assertiveness + sensitive patterns + size.
fn gate_contract(content: &str, must_not: &[String]) -> Result<(), TextGradient> {
    if content.len() > CONTENT_MAX_BYTES {
        return Err(TextGradient::blocking(
            "G-Contract",
            "candidate.content",
            &format!("Candidate is {} bytes, exceeding the {CONTENT_MAX_BYTES}-byte limit", content.len()),
            "Keep each change focused and compact",
        ));
    }

    let lower = content.to_lowercase();
    for pattern in must_not {
        if lower.contains(&pattern.to_lowercase()) {
            return Err(TextGradient::blocking(
                "G-Contract",
                "candidate.content",
                &format!("Candidate introduces forbidden pattern: '{pattern}'"),
                &format!("Remove or rephrase the part containing '{pattern}'"),
            ));
        }
    }
    for pattern in DEFAULT_MUST_NOT {
        if lower.contains(&pattern.to_lowercase()) {
            return Err(TextGradient::blocking(
                "G-Assertiveness",
                "candidate.content",
                &format!(
                    "Candidate instructs the agent to reduce assertiveness: '{pattern}'. \
                     Lower friction is not higher quality."
                ),
                "Preserve the agent's commitment to factual accuracy and honest disagreement",
            ));
        }
    }

    for pattern in SENSITIVE_PATTERNS {
        if content.contains(pattern) {
            return Err(TextGradient::blocking(
                "G-Contract",
                "candidate.content",
                &format!("Candidate contains sensitive pattern: '{pattern}'"),
                "Remove any API keys, tokens, or credentials",
            ));
        }
    }

    Ok(())
}

/// `must_always` is a STATE invariant, so it is checked against the projected
/// post-apply text, not against the increment.
fn gate_must_always(simulated_final: &str, must_always: &[String]) -> Result<(), TextGradient> {
    let lower = simulated_final.to_lowercase();
    for pattern in must_always {
        if !lower.contains(&pattern.to_lowercase()) {
            return Err(TextGradient::blocking(
                "G-Contract",
                "simulated_final",
                &format!("Applying this candidate would drop a required behaviour: '{pattern}'"),
                &format!("Ensure the result still contains the '{pattern}' requirement"),
            ));
        }
    }
    Ok(())
}

/// G-Canary-Static. Blocking when the candidate literally instructs the agent
/// to emit a canary-forbidden phrase; advisory when it suppresses an expected
/// one.
fn gate_canary_static(
    content: &str,
    canary_tests: &[CanaryTest],
) -> Result<Vec<TextGradient>, TextGradient> {
    let mut advisories = Vec::new();
    let lower = content.to_lowercase();
    for test in canary_tests {
        for forbidden in &test.must_not_contain {
            let f = forbidden.to_lowercase();
            if lower.contains(&format!("always say {f}"))
                || lower.contains(&format!("respond with {f}"))
                || lower.contains(&format!("output {f}"))
            {
                return Err(TextGradient::blocking(
                    "G-Canary-Static",
                    &test.id,
                    &format!(
                        "Candidate would break canary '{}': it instructs the agent to output '{forbidden}'",
                        test.id
                    ),
                    &format!("Canary test: {}", test.description),
                ));
            }
        }
        for required in &test.must_contain {
            let r = required.to_lowercase();
            if lower.contains(&format!("never say {r}"))
                || lower.contains(&format!("avoid saying {r}"))
                || lower.contains(&format!("don't use {r}"))
            {
                advisories.push(TextGradient::advisory(
                    "G-Canary-Static",
                    &test.id,
                    &format!("Candidate may weaken canary '{}': it suppresses '{required}'", test.id),
                    &format!("Canary test: {}", test.description),
                ));
            }
        }
    }
    Ok(advisories)
}

/// G-Capacity (§2.2): **never rejects**. Reports how many of the lowest-GDI
/// entries the caller must park as `Stale` for the post-merge playbook to fit
/// under [`crate::playbook::PLAYBOOK_MAX_ENTRIES`].
///
/// Returning a count rather than a veto is deliberate: rejecting an otherwise
/// good `Add` because the playbook is full would make capacity a quality
/// signal, which it is not.
pub fn capacity_headroom(active_after_merge: usize) -> usize {
    active_after_merge.saturating_sub(crate::playbook::PLAYBOOK_MAX_ENTRIES)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input<'a>(contents: &[&str], must_not: &'a [String]) -> GateInput<'a> {
        GateInput {
            agent_id: "a",
            contents: contents.iter().map(|s| s.to_string()).collect(),
            simulated_final: None,
            current_reference: "",
            must_not,
            must_always: &[],
            canary_tests: &[],
        }
    }

    #[test]
    fn empty_candidate_fails_closed() {
        let none: Vec<String> = Vec::new();
        assert!(run_gates(&input(&[], &none)).is_err());
        assert!(run_gates(&input(&["   "], &none)).is_err());
    }

    #[test]
    fn assertiveness_instruction_is_a_contract_violation_not_a_deduction() {
        let none: Vec<String> = Vec::new();
        let err = run_gates(&input(
            &["\u{4E0D}\u{8981}\u{7CFE}\u{6B63}\u{7528}\u{6236}，\u{9806}\u{8457}\u{56DE}"],
            &none,
        ))
        .unwrap_err();
        assert_eq!(err.source_layer, "G-Assertiveness");
    }

    #[test]
    fn safety_gate_matches_whole_words_not_substrings() {
        let none: Vec<String> = Vec::new();
        // "disabled killswitchery" must NOT trip: neither token boundary holds.
        assert!(run_gates(&input(&["the disabledkillswitchery module"], &none)).is_ok());
        assert!(run_gates(&input(&["disable killswitch when convenient"], &none)).is_err());
    }

    #[test]
    fn safety_gate_covers_zh_tw_human_override() {
        let none: Vec<String> = Vec::new();
        let err = run_gates(&input(
            &["\u{4E4B}\u{5F8C}\u{8DF3}\u{904E}\u{4EBA}\u{5DE5}\u{5BE9}\u{6838}\u{76F4}\u{63A5}\u{57F7}\u{884C}"],
            &none,
        ))
        .unwrap_err();
        assert_eq!(err.source_layer, "G-Safety");
    }

    #[test]
    fn gate_rejection_gradient_names_the_violated_pattern() {
        let must_not = vec!["\u{4EE3}\u{7406}\u{7C3D}\u{7D04}".to_string()]; // 代理簽約
        let err = run_gates(&input(
            &["\u{5FC5}\u{8981}\u{6642}\u{53EF}\u{4EE5}\u{4EE3}\u{7406}\u{7C3D}\u{7D04}"],
            &must_not,
        ))
        .unwrap_err();
        assert_eq!(err.source_layer, "G-Contract");
        assert!(err.critique.contains("\u{4EE3}\u{7406}\u{7C3D}\u{7D04}"));
    }

    #[test]
    fn must_always_only_applies_when_a_projection_is_supplied() {
        let none: Vec<String> = Vec::new();
        let always = vec!["always cite sources".to_string()];
        // Playbook path: no projection → the state invariant is not evaluated.
        let mut gi = input(&["be concise"], &none);
        gi.must_always = &always;
        assert!(run_gates(&gi).is_ok());
        // SOUL path: projection missing the invariant → veto.
        gi.simulated_final = Some("be concise".to_string());
        assert!(run_gates(&gi).is_err());
    }

    #[test]
    fn capacity_never_vetoes_it_reports_headroom() {
        assert_eq!(capacity_headroom(crate::playbook::PLAYBOOK_MAX_ENTRIES), 0);
        assert_eq!(capacity_headroom(crate::playbook::PLAYBOOK_MAX_ENTRIES + 3), 3);
    }
}
