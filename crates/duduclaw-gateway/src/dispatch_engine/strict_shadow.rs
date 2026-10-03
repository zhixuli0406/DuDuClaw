//! WP-G1 (`commercial/docs/DESIGN-llm-contract-goal-loop-2026-10.md` §2):
//! judge replies under the strict JSON contract, shadow first.
//!
//! The goal loop has three places that turn an LLM (or external command)
//! reply into a decision — the MAV panel ([`super::parse_panel_verdict_for`]),
//! the first-stage evaluator ([`super::parse_pre_evaluation`]) and the
//! external judge ([`crate::judge_mode::parse_external_verdict`]). All three
//! locate "the JSON" by slicing from the first `{` to the last `}`, so prose
//! around it, a second value, or trailing text are silently repaired.
//! [`duduclaw_core::llm_contract::strict_json::parse_strict`] refuses all of
//! that: the whole reply must be exactly one JSON value of the expected type.
//!
//! Switching the authority from one parser to the other changes which
//! replies are accepted, so it happens in two steps, controlled by
//! `config.toml [dispatch] strict_reply_parsing` (read at every decision):
//!
//! | mode | authoritative result | strict parse |
//! |---|---|---|
//! | `off` | lenient (byte-identical to before WP-G1) | not run |
//! | `shadow` (default) | lenient | run and compared |
//! | `enforce` | strict; a violation takes the parser's existing fail-closed path | run and compared |
//!
//! Every compared decision increments
//! `judge_parse_shadow_total{parser, outcome}` and, unless the two parsers
//! agree, appends one `judge_parse_shadow_mismatch` audit event. Moving the
//! default from `shadow` to `enforce` is an operator decision made on those
//! numbers (design §2 WP-G1 observation window), never an automatic flip.

use std::path::Path;

use duduclaw_core::llm_contract::strict_json::Violation;

use crate::metrics::MetricsRegistry;

/// `[dispatch] strict_reply_parsing`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StrictReplyParsing {
    /// Strict parse never runs; every parser behaves exactly as before.
    Off,
    /// Lenient parse decides; strict parse is run and compared (default).
    #[default]
    Shadow,
    /// Strict parse decides; a contract violation is a parser failure.
    Enforce,
}

impl StrictReplyParsing {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Shadow => "shadow",
            Self::Enforce => "enforce",
        }
    }

    /// Interpret the raw `[dispatch] strict_reply_parsing` value. Lenient
    /// like the sibling `[dispatch]` keys: only `"off"` and `"enforce"`
    /// (trimmed, ASCII case-insensitive) move away from the default; an
    /// absent key, a non-string value or an unknown string is
    /// [`Self::Shadow`]. Shadow never changes a verdict, so a typo can only
    /// cost observation, never acceptance.
    pub fn from_value(value: Option<&toml::Value>) -> Self {
        match value
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("off") => Self::Off,
            Some("enforce") => Self::Enforce,
            _ => Self::Shadow,
        }
    }

    /// Read the mode from `<home_dir>/config.toml`, at each decision (same
    /// hot-reload schedule as `two_stage_judge` / `judge`).
    ///
    /// `home_dir = None` ⇒ [`Self::Off`]: a judge that was never given a
    /// home has no config to read and no audit log to write, so it keeps
    /// its pre-WP-G1 behavior exactly. A home with a missing, unreadable or
    /// malformed `config.toml` ⇒ [`Self::Shadow`] (the default).
    pub fn from_home(home_dir: Option<&Path>) -> Self {
        let Some(home_dir) = home_dir else {
            return Self::Off;
        };
        let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
            return Self::default();
        };
        let Ok(table) = content.parse::<toml::Table>() else {
            return Self::default();
        };
        Self::from_value(
            table
                .get("dispatch")
                .and_then(|v| v.as_table())
                .and_then(|s| s.get("strict_reply_parsing")),
        )
    }
}

/// Which parser produced an observation (`parser` label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyParser {
    Panel,
    PreEvaluator,
    External,
}

impl ReplyParser {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Panel => "panel",
            Self::PreEvaluator => "pre_evaluator",
            Self::External => "external",
        }
    }
}

/// How the lenient and strict parses of one reply compare (`outcome` label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShadowOutcome {
    /// Both parsed and yield the same verdict / decision.
    Agree,
    /// Lenient parsed, strict found a contract violation.
    StrictRejects,
    /// Strict parsed, lenient failed.
    LenientRejects,
    /// Neither parsed.
    BothReject,
    /// Both parsed but yield different verdicts / decisions.
    Disagree,
}

impl ShadowOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agree => "agree",
            Self::StrictRejects => "strict_rejects",
            Self::LenientRejects => "lenient_rejects",
            Self::BothReject => "both_reject",
            Self::Disagree => "disagree",
        }
    }

    /// Classify one decision. `same` is only consulted when both parsed.
    pub fn classify(lenient_ok: bool, strict_ok: bool, same: bool) -> Self {
        match (lenient_ok, strict_ok) {
            (true, true) if same => Self::Agree,
            (true, true) => Self::Disagree,
            (true, false) => Self::StrictRejects,
            (false, true) => Self::LenientRejects,
            (false, false) => Self::BothReject,
        }
    }
}

/// Audit event type for a non-`agree` observation.
pub const SHADOW_MISMATCH_EVENT: &str = "judge_parse_shadow_mismatch";
/// Attribution id on the audit event (agent-less judge calls; same id the
/// judge seam uses for its utility calls).
pub(crate) const SHADOW_AUDIT_AGENT_ID: &str = "goal-acceptance-judge";
/// How many characters of the (masked) reply the audit event keeps.
pub const REPLY_HEAD_CHARS: usize = 200;
/// Cap on the masked violation / lenient-error text in the audit event.
/// Both can quote model-chosen field names or values.
const DETAIL_MAX_CHARS: usize = 300;

/// One shadow observation, before it is recorded.
pub(crate) struct ShadowObservation<'a> {
    pub parser: ReplyParser,
    pub mode: StrictReplyParsing,
    pub outcome: ShadowOutcome,
    /// The strict contract violation, when the strict parse failed.
    pub violation: Option<&'a Violation>,
    /// Why the lenient parse failed, when it did.
    pub lenient_error: Option<&'a str>,
    /// The raw reply exactly as the parsers saw it.
    pub raw: &'a str,
}

/// Mask, then truncate by characters (never by raw byte index). Masking
/// first means a secret that straddles the cut is still masked.
fn masked_head(text: &str, max_chars: usize) -> String {
    let masked = duduclaw_security::audit::mask_sensitive_text(text);
    duduclaw_core::truncate_chars(&masked, max_chars)
}

/// Build the `judge_parse_shadow_mismatch` audit event, or `None` for an
/// `agree` observation (agreement is counted, never audited).
pub(crate) fn mismatch_event(
    obs: &ShadowObservation<'_>,
) -> Option<duduclaw_security::audit::AuditEvent> {
    if obs.outcome == ShadowOutcome::Agree {
        return None;
    }
    let violation = obs
        .violation
        .map(|v| masked_head(&v.to_string(), DETAIL_MAX_CHARS));
    let lenient_error = obs
        .lenient_error
        .map(|e| masked_head(e, DETAIL_MAX_CHARS));
    Some(duduclaw_security::audit::AuditEvent::new(
        SHADOW_MISMATCH_EVENT,
        SHADOW_AUDIT_AGENT_ID,
        duduclaw_security::audit::Severity::Info,
        serde_json::json!({
            "parser": obs.parser.as_str(),
            "outcome": obs.outcome.as_str(),
            "mode": obs.mode.as_str(),
            "violation": violation,
            "lenient_error": lenient_error,
            "reply_head": masked_head(obs.raw, REPLY_HEAD_CHARS),
            "reply_bytes": obs.raw.len(),
        }),
    ))
}

/// Record one observation: increment the counter and, for every non-`agree`
/// outcome with a known home, append the audit event. Never fails — an
/// observation must not be able to change or block a decision.
pub(crate) fn record_shadow(
    metrics: &MetricsRegistry,
    home_dir: Option<&Path>,
    obs: &ShadowObservation<'_>,
) {
    metrics.judge_parse_shadow(obs.parser.as_str(), obs.outcome.as_str());
    if obs.outcome != ShadowOutcome::Agree {
        tracing::info!(
            parser = obs.parser.as_str(),
            outcome = obs.outcome.as_str(),
            mode = obs.mode.as_str(),
            "judge reply strict-contract shadow mismatch"
        );
    }
    if let (Some(home_dir), Some(event)) = (home_dir, mismatch_event(obs)) {
        crate::security_autopilot::audit_and_emit(home_dir, &event);
    }
}

#[cfg(test)]
mod tests {
    use super::super::judge::parse_panel_verdict_contract_with;
    use super::super::pre_evaluator::parse_pre_evaluation_contract_with;
    use super::super::{Difficulty, PreDecision, panel_aspects};
    use super::*;
    use crate::metrics::{JUDGE_PARSE_SHADOW_OUTCOMES, JUDGE_PARSE_SHADOW_PARSERS};

    // ── helpers ──────────────────────────────────────────────────────

    fn audit_lines(home: &Path) -> Vec<serde_json::Value> {
        let Ok(s) = std::fs::read_to_string(home.join("security_audit.jsonl")) else {
            return Vec::new();
        };
        s.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .filter(|v: &serde_json::Value| v["event_type"] == SHADOW_MISMATCH_EVENT)
            .collect()
    }

    /// Assert exactly one counter series moved by one, and the audit log
    /// gained an event only for a non-`agree` outcome.
    fn assert_observed(
        m: &MetricsRegistry,
        home: &Path,
        parser: &str,
        outcome: ShadowOutcome,
        audits_before: usize,
    ) {
        for p in JUDGE_PARSE_SHADOW_PARSERS {
            for o in JUDGE_PARSE_SHADOW_OUTCOMES {
                let want = u64::from(p == parser && o == outcome.as_str());
                assert_eq!(
                    m.judge_parse_shadow_count(p, o),
                    want,
                    "series {p}/{o} (expected {parser}/{})",
                    outcome.as_str()
                );
            }
        }
        let audits = audit_lines(home);
        if outcome == ShadowOutcome::Agree {
            assert_eq!(audits.len(), audits_before, "agree must not audit");
        } else {
            assert_eq!(audits.len(), audits_before + 1, "mismatch must audit once");
            let ev = audits.last().unwrap();
            assert_eq!(ev["details"]["parser"], parser);
            assert_eq!(ev["details"]["outcome"], outcome.as_str());
            assert!(ev["details"]["reply_head"].is_string());
        }
    }

    fn assert_not_observed(m: &MetricsRegistry, home: &Path) {
        for p in JUDGE_PARSE_SHADOW_PARSERS {
            for o in JUDGE_PARSE_SHADOW_OUTCOMES {
                assert_eq!(m.judge_parse_shadow_count(p, o), 0, "off must not count");
            }
        }
        assert!(audit_lines(home).is_empty(), "off must not audit");
    }

    const MODES: [StrictReplyParsing; 3] = [
        StrictReplyParsing::Off,
        StrictReplyParsing::Shadow,
        StrictReplyParsing::Enforce,
    ];

    // ── mode reader ──────────────────────────────────────────────────

    #[test]
    fn mode_reader_is_lenient_with_shadow_default() {
        let v = |s: &str| toml::Value::String(s.to_string());
        assert_eq!(StrictReplyParsing::from_value(None), StrictReplyParsing::Shadow);
        assert_eq!(StrictReplyParsing::from_value(Some(&v("off"))), StrictReplyParsing::Off);
        assert_eq!(
            StrictReplyParsing::from_value(Some(&v("  Enforce "))),
            StrictReplyParsing::Enforce
        );
        assert_eq!(
            StrictReplyParsing::from_value(Some(&v("shadow"))),
            StrictReplyParsing::Shadow
        );
        assert_eq!(
            StrictReplyParsing::from_value(Some(&v("enforced"))),
            StrictReplyParsing::Shadow,
            "unknown value ⇒ shadow"
        );
        assert_eq!(
            StrictReplyParsing::from_value(Some(&toml::Value::Boolean(true))),
            StrictReplyParsing::Shadow,
            "non-string ⇒ shadow"
        );
    }

    #[test]
    fn mode_reader_reads_config_per_call() {
        assert_eq!(StrictReplyParsing::from_home(None), StrictReplyParsing::Off);
        let dir = tempfile::tempdir().unwrap();
        // No config.toml ⇒ default.
        assert_eq!(
            StrictReplyParsing::from_home(Some(dir.path())),
            StrictReplyParsing::Shadow
        );
        let cfg = dir.path().join("config.toml");
        std::fs::write(&cfg, "[dispatch]\nstrict_reply_parsing = \"enforce\"\n").unwrap();
        assert_eq!(
            StrictReplyParsing::from_home(Some(dir.path())),
            StrictReplyParsing::Enforce
        );
        std::fs::write(&cfg, "[dispatch]\nstrict_reply_parsing = \"off\"\n").unwrap();
        assert_eq!(StrictReplyParsing::from_home(Some(dir.path())), StrictReplyParsing::Off);
        std::fs::write(&cfg, "[dispatch]\ntwo_stage_judge = false\n").unwrap();
        assert_eq!(
            StrictReplyParsing::from_home(Some(dir.path())),
            StrictReplyParsing::Shadow
        );
        std::fs::write(&cfg, "this is [not toml").unwrap();
        assert_eq!(
            StrictReplyParsing::from_home(Some(dir.path())),
            StrictReplyParsing::Shadow,
            "malformed config ⇒ default"
        );
    }

    #[test]
    fn labels_are_pinned_to_the_metric_label_sets() {
        for p in [ReplyParser::Panel, ReplyParser::PreEvaluator, ReplyParser::External] {
            assert!(JUDGE_PARSE_SHADOW_PARSERS.contains(&p.as_str()));
        }
        for o in [
            ShadowOutcome::Agree,
            ShadowOutcome::StrictRejects,
            ShadowOutcome::LenientRejects,
            ShadowOutcome::BothReject,
            ShadowOutcome::Disagree,
        ] {
            assert!(JUDGE_PARSE_SHADOW_OUTCOMES.contains(&o.as_str()));
        }
    }

    #[test]
    fn classify_covers_the_five_outcomes() {
        assert_eq!(ShadowOutcome::classify(true, true, true), ShadowOutcome::Agree);
        assert_eq!(ShadowOutcome::classify(true, true, false), ShadowOutcome::Disagree);
        assert_eq!(ShadowOutcome::classify(true, false, true), ShadowOutcome::StrictRejects);
        assert_eq!(ShadowOutcome::classify(false, true, true), ShadowOutcome::LenientRejects);
        assert_eq!(ShadowOutcome::classify(false, false, false), ShadowOutcome::BothReject);
    }

    #[test]
    fn audit_reply_head_is_masked_and_char_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let m = MetricsRegistry::new_isolated();
        let raw = format!("api_key=sk-ant-SECRETSECRETSECRET {}", "判".repeat(400));
        let v = Violation::TrailingContent;
        record_shadow(
            &m,
            Some(dir.path()),
            &ShadowObservation {
                parser: ReplyParser::Panel,
                mode: StrictReplyParsing::Shadow,
                outcome: ShadowOutcome::StrictRejects,
                violation: Some(&v),
                lenient_error: None,
                raw: &raw,
            },
        );
        let ev = audit_lines(dir.path()).pop().unwrap();
        let head = ev["details"]["reply_head"].as_str().unwrap();
        assert!(!head.contains("SECRETSECRET"), "secret must be masked: {head}");
        assert!(head.chars().count() <= REPLY_HEAD_CHARS);
        assert_eq!(ev["details"]["mode"], "shadow");
        assert_eq!(ev["details"]["violation"], v.to_string());
        assert_eq!(ev["details"]["reply_bytes"], raw.len());
        assert_eq!(ev["severity"], "info");
    }

    #[test]
    fn no_home_counts_but_does_not_audit() {
        let m = MetricsRegistry::new_isolated();
        record_shadow(
            &m,
            None,
            &ShadowObservation {
                parser: ReplyParser::External,
                mode: StrictReplyParsing::Shadow,
                outcome: ShadowOutcome::BothReject,
                violation: None,
                lenient_error: Some("x"),
                raw: "x",
            },
        );
        assert_eq!(m.judge_parse_shadow_count("external", "both_reject"), 1);
    }

    // ── panel × mode × reply shape ───────────────────────────────────

    const PANEL_PASS: &str = r#"{"correctness":{"pass":true,"reason":"ok"},"completeness":{"pass":true,"reason":"ok"},"safety":{"pass":true,"reason":"ok"}}"#;
    const PANEL_FAIL: &str = r#"{"correctness":{"pass":false,"reason":"wrong"},"completeness":{"pass":true,"reason":"ok"},"safety":{"pass":true,"reason":"ok"}}"#;

    struct PanelCase {
        name: &'static str,
        raw: String,
        /// Shadow classification.
        outcome: ShadowOutcome,
        /// Authoritative `passed` under off/shadow (lenient).
        lenient_passed: bool,
        /// Authoritative `passed` under enforce (strict).
        strict_passed: bool,
        /// Under enforce, the verdict is the fail-closed "broken" FAIL.
        enforce_broken: bool,
    }

    fn panel_cases() -> Vec<PanelCase> {
        vec![
            PanelCase {
                name: "clean",
                raw: PANEL_PASS.to_string(),
                outcome: ShadowOutcome::Agree,
                lenient_passed: true,
                strict_passed: true,
                enforce_broken: false,
            },
            PanelCase {
                name: "clean fenced",
                raw: format!("```json\n{PANEL_FAIL}\n```"),
                outcome: ShadowOutcome::Agree,
                lenient_passed: false,
                strict_passed: false,
                enforce_broken: false,
            },
            PanelCase {
                name: "prose-wrapped",
                raw: format!("Here is my verdict:\n{PANEL_PASS}\nHope this helps."),
                outcome: ShadowOutcome::StrictRejects,
                lenient_passed: true,
                strict_passed: false,
                enforce_broken: true,
            },
            PanelCase {
                name: "two values",
                raw: format!("{PANEL_FAIL}\n{PANEL_PASS}"),
                outcome: ShadowOutcome::BothReject,
                lenient_passed: false,
                strict_passed: false,
                enforce_broken: true,
            },
            PanelCase {
                name: "unknown extra field",
                raw: PANEL_PASS.replacen('{', r#"{"notes":"extra","#, 1),
                outcome: ShadowOutcome::StrictRejects,
                lenient_passed: true,
                strict_passed: false,
                enforce_broken: true,
            },
            PanelCase {
                name: "unknown field inside an aspect",
                raw: PANEL_PASS.replacen(r#""reason":"ok""#, r#""reason":"ok","confidence":0.9"#, 1),
                outcome: ShadowOutcome::StrictRejects,
                lenient_passed: true,
                strict_passed: false,
                enforce_broken: true,
            },
            PanelCase {
                name: "garbage",
                raw: "I could not decide, sorry.".to_string(),
                outcome: ShadowOutcome::BothReject,
                lenient_passed: false,
                strict_passed: false,
                enforce_broken: true,
            },
            PanelCase {
                name: "legacy PASS line",
                raw: "PASS\nall good".to_string(),
                outcome: ShadowOutcome::StrictRejects,
                lenient_passed: true,
                strict_passed: false,
                enforce_broken: true,
            },
        ]
    }

    #[test]
    fn panel_parser_every_mode_every_shape() {
        let aspects = panel_aspects(Difficulty::Complex);
        for case in panel_cases() {
            for mode in MODES {
                let dir = tempfile::tempdir().unwrap();
                let m = MetricsRegistry::new_isolated();
                let v = parse_panel_verdict_contract_with(
                    &m,
                    &case.raw,
                    aspects,
                    mode,
                    Some(dir.path()),
                );
                let lenient = super::super::parse_panel_verdict_for(&case.raw, aspects);
                match mode {
                    StrictReplyParsing::Off => {
                        assert_eq!(v, lenient, "{}: off is byte-identical", case.name);
                        assert_not_observed(&m, dir.path());
                    }
                    StrictReplyParsing::Shadow => {
                        assert_eq!(v, lenient, "{}: shadow keeps lenient", case.name);
                        assert_eq!(v.passed, case.lenient_passed, "{}", case.name);
                        assert_observed(&m, dir.path(), "panel", case.outcome, 0);
                    }
                    StrictReplyParsing::Enforce => {
                        assert_eq!(v.passed, case.strict_passed, "{}: enforce", case.name);
                        assert_eq!(
                            v.feedback.contains("fail-closed"),
                            case.enforce_broken,
                            "{}: enforce broken path ({})",
                            case.name,
                            v.feedback
                        );
                        assert_observed(&m, dir.path(), "panel", case.outcome, 0);
                    }
                }
            }
        }
    }

    #[test]
    fn panel_strict_refuses_aspects_outside_the_active_panel() {
        // A simple panel has no `completeness`: the three-aspect reply carries
        // an aspect the schema did not ask for.
        let aspects = panel_aspects(Difficulty::Simple);
        let dir = tempfile::tempdir().unwrap();
        let m = MetricsRegistry::new_isolated();
        let v = parse_panel_verdict_contract_with(
            &m,
            PANEL_PASS,
            aspects,
            StrictReplyParsing::Enforce,
            Some(dir.path()),
        );
        assert!(!v.passed);
        assert_observed(&m, dir.path(), "panel", ShadowOutcome::StrictRejects, 0);

        // A missing active aspect is a strict violation too. Lenient still
        // extracts a panel (and fails that aspect closed), so this counts as
        // `strict_rejects` with a FAIL verdict either way.
        let missing = r#"{"correctness":{"pass":true,"reason":"ok"}}"#;
        let m = MetricsRegistry::new_isolated();
        let dir = tempfile::tempdir().unwrap();
        let v = parse_panel_verdict_contract_with(
            &m,
            missing,
            aspects,
            StrictReplyParsing::Shadow,
            Some(dir.path()),
        );
        assert!(!v.passed);
        assert_observed(&m, dir.path(), "panel", ShadowOutcome::StrictRejects, 0);
    }

    // ── pre-evaluator × mode × reply shape ───────────────────────────

    const PRE_CONTINUE: &str =
        r#"{"decision":"continue","evidence":"only a plan","next_step":"write the file","blocker_key":null}"#;

    struct PreCase {
        name: &'static str,
        raw: String,
        outcome: ShadowOutcome,
        lenient: Option<PreDecision>,
        strict: Option<PreDecision>,
    }

    fn pre_cases() -> Vec<PreCase> {
        vec![
            PreCase {
                name: "clean",
                raw: PRE_CONTINUE.to_string(),
                outcome: ShadowOutcome::Agree,
                lenient: Some(PreDecision::Continue),
                strict: Some(PreDecision::Continue),
            },
            PreCase {
                name: "clean, blocker_key omitted",
                raw: r#"{"decision":"candidate_complete","evidence":"file written","next_step":"check the header"}"#
                    .to_string(),
                outcome: ShadowOutcome::Agree,
                lenient: Some(PreDecision::CandidateComplete),
                strict: Some(PreDecision::CandidateComplete),
            },
            PreCase {
                name: "prose-wrapped",
                raw: format!("Sure!\n```json\n{PRE_CONTINUE}\n```\nDone."),
                outcome: ShadowOutcome::StrictRejects,
                lenient: Some(PreDecision::Continue),
                strict: None,
            },
            PreCase {
                name: "two values",
                raw: format!("{PRE_CONTINUE}{PRE_CONTINUE}"),
                outcome: ShadowOutcome::BothReject,
                lenient: None,
                strict: None,
            },
            PreCase {
                name: "unknown extra field",
                raw: PRE_CONTINUE.replacen('{', r#"{"confidence":0.4,"#, 1),
                outcome: ShadowOutcome::StrictRejects,
                lenient: Some(PreDecision::Continue),
                strict: None,
            },
            PreCase {
                name: "garbage",
                raw: "看起來做完了。".to_string(),
                outcome: ShadowOutcome::BothReject,
                lenient: None,
                strict: None,
            },
            PreCase {
                name: "semantic violation (blocker_key on continue)",
                raw: PRE_CONTINUE.replace("null", r#""some_key""#),
                outcome: ShadowOutcome::BothReject,
                lenient: None,
                strict: None,
            },
        ]
    }

    #[test]
    fn pre_evaluator_parser_every_mode_every_shape() {
        for case in pre_cases() {
            for mode in MODES {
                let dir = tempfile::tempdir().unwrap();
                let m = MetricsRegistry::new_isolated();
                let got = parse_pre_evaluation_contract_with(&m, &case.raw, mode, Some(dir.path()));
                let lenient = super::super::parse_pre_evaluation(&case.raw);
                match mode {
                    StrictReplyParsing::Off => {
                        assert_eq!(got, lenient, "{}: off is byte-identical", case.name);
                        assert_not_observed(&m, dir.path());
                    }
                    StrictReplyParsing::Shadow => {
                        assert_eq!(got, lenient, "{}: shadow keeps lenient", case.name);
                        assert_eq!(got.ok().map(|e| e.decision), case.lenient, "{}", case.name);
                        assert_observed(&m, dir.path(), "pre_evaluator", case.outcome, 0);
                    }
                    StrictReplyParsing::Enforce => {
                        assert_eq!(
                            got.as_ref().ok().map(|e| e.decision),
                            case.strict,
                            "{}: enforce",
                            case.name
                        );
                        assert_observed(&m, dir.path(), "pre_evaluator", case.outcome, 0);
                    }
                }
            }
        }
    }

    // ── external verdict × mode × reply shape ────────────────────────

    struct ExtCase {
        name: &'static str,
        raw: String,
        outcome: ShadowOutcome,
        lenient: Option<bool>,
        strict: Option<bool>,
    }

    fn ext_cases() -> Vec<ExtCase> {
        let clean = r#"{"pass": true, "feedback": "所有驗收條件皆滿足"}"#;
        vec![
            ExtCase {
                name: "clean",
                raw: clean.to_string(),
                outcome: ShadowOutcome::Agree,
                lenient: Some(true),
                strict: Some(true),
            },
            ExtCase {
                name: "clean, string pass, no feedback",
                raw: r#"{"pass":"FAIL"}"#.to_string(),
                outcome: ShadowOutcome::Agree,
                lenient: Some(false),
                strict: Some(false),
            },
            ExtCase {
                name: "prose-wrapped",
                raw: format!("judging...\n{clean}\ndone"),
                outcome: ShadowOutcome::StrictRejects,
                lenient: Some(true),
                strict: None,
            },
            ExtCase {
                name: "two values",
                raw: format!("{clean}\n{clean}"),
                outcome: ShadowOutcome::BothReject,
                lenient: None,
                strict: None,
            },
            ExtCase {
                name: "unknown extra field",
                raw: r#"{"schema":"duduclaw.judge.v1","pass":true,"feedback":"ok"}"#.to_string(),
                outcome: ShadowOutcome::StrictRejects,
                lenient: Some(true),
                strict: None,
            },
            ExtCase {
                name: "garbage",
                raw: "Segmentation fault".to_string(),
                outcome: ShadowOutcome::BothReject,
                lenient: None,
                strict: None,
            },
        ]
    }

    #[test]
    fn external_parser_every_mode_every_shape() {
        for case in ext_cases() {
            for mode in MODES {
                let dir = tempfile::tempdir().unwrap();
                let m = MetricsRegistry::new_isolated();
                let got = crate::judge_mode::parse_external_verdict_contract_with(
                    &m,
                    &case.raw,
                    mode,
                    Some(dir.path()),
                );
                let lenient = crate::judge_mode::parse_external_verdict(&case.raw);
                match mode {
                    StrictReplyParsing::Off => {
                        assert_eq!(got, lenient, "{}: off is byte-identical", case.name);
                        assert_not_observed(&m, dir.path());
                    }
                    StrictReplyParsing::Shadow => {
                        assert_eq!(got, lenient, "{}: shadow keeps lenient", case.name);
                        assert_eq!(got.ok().map(|v| v.passed), case.lenient, "{}", case.name);
                        assert_observed(&m, dir.path(), "external", case.outcome, 0);
                    }
                    StrictReplyParsing::Enforce => {
                        assert_eq!(
                            got.as_ref().ok().map(|v| v.passed),
                            case.strict,
                            "{}: enforce",
                            case.name
                        );
                        assert_observed(&m, dir.path(), "external", case.outcome, 0);
                    }
                }
            }
        }
    }
}
