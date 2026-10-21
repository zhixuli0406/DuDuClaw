use super::*;
use std::path::Path;

const KEY: &str = "sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abcdefABCDEF-QQ";

fn ev(block: &str, phase: ThinkingPhase, kind: ThinkingKind, text: Option<&str>) -> ThinkingEvent {
    ThinkingEvent {
        text: text.map(str::to_string),
        ..ThinkingEvent::new(block, phase, kind, "anthropic_api")
    }
}

fn ctx(home: &Path) -> SanitizeCtx<'_> {
    SanitizeCtx {
        home,
        agent_id: "agnes",
        engine: None,
    }
}

fn run(gate: &mut Gate, home: &Path, steps: Vec<(u64, ThinkingEvent)>) -> Vec<ThinkingEvent> {
    let mut out = Vec::new();
    for (t, e) in steps {
        out.extend(gate.process(e, t, &ctx(home)).events);
    }
    out
}

#[test]
fn off_forwards_nothing() {
    let home = tempfile::tempdir().unwrap();
    let mut gate = Gate::new(SinkLevel::Off, 4_000, true);
    let got = run(
        &mut gate,
        home.path(),
        vec![
            (
                0,
                ev("t0", ThinkingPhase::Start, ThinkingKind::Summary, None),
            ),
            (
                10,
                ev("t0", ThinkingPhase::End, ThinkingKind::Summary, Some("x")),
            ),
        ],
    );
    assert!(got.is_empty());
}

#[test]
fn signal_and_status_carry_timing_and_tokens_but_never_text() {
    let home = tempfile::tempdir().unwrap();
    for level in [SinkLevel::Signal, SinkLevel::Status] {
        let mut gate = Gate::new(level, 4_000, true);
        let mut upd = ev(
            "t0",
            ThinkingPhase::Update,
            ThinkingKind::Summary,
            Some("secret plan"),
        );
        upd.tokens = Some(30);
        let got = run(
            &mut gate,
            home.path(),
            vec![
                (
                    1_000,
                    ev("t0", ThinkingPhase::Start, ThinkingKind::Summary, None),
                ),
                (1_100, upd.clone()),
                (1_200, upd.clone()), // throttled
                (1_600, upd),
                (
                    3_000,
                    ev(
                        "t0",
                        ThinkingPhase::End,
                        ThinkingKind::Summary,
                        Some("full plan"),
                    ),
                ),
            ],
        );
        assert_eq!(got.len(), 4, "{level:?}: {got:?}");
        assert!(
            got.iter().all(|e| e.text.is_none() && e.line.is_none()),
            "{level:?}"
        );
        assert_eq!(got[1].tokens, Some(30));
        assert_eq!(got[0].started_at, Some(1_000));
        assert_eq!(got[3].duration_ms, Some(2_000));
    }
}

#[test]
fn summary_holds_back_a_key_that_is_still_streaming() {
    let home = tempfile::tempdir().unwrap();
    let mut gate = Gate::new(SinkLevel::Summary, 4_000, true);
    let (head, rest) = KEY.split_at(20);
    let got = run(
        &mut gate,
        home.path(),
        vec![
            (
                0,
                ev("t0", ThinkingPhase::Start, ThinkingKind::Summary, None),
            ),
            (
                500,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some("I will call the API with "),
                ),
            ),
            (
                1_000,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some(head),
                ),
            ),
            (
                1_500,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some(rest),
                ),
            ),
            (
                2_000,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some(" and then check"),
                ),
            ),
            (
                2_500,
                ev("t0", ThinkingPhase::End, ThinkingKind::Summary, None),
            ),
        ],
    );
    for e in &got {
        let shown = e.text.as_deref().unwrap_or("");
        // No snapshot ever shows any part of the key body.
        assert!(!shown.contains("AbCd"), "{shown}");
        assert!(!shown.contains("sk-ant-api03-A"), "{shown}");
        assert!(!shown.contains("-QQ"), "{shown}");
    }
    let end = got.last().unwrap();
    assert_eq!(end.phase, ThinkingPhase::End);
    let final_text = end.text.as_deref().unwrap();
    assert!(
        final_text.contains(super::super::sanitize::SECRET_PLACEHOLDER),
        "{final_text}"
    );
    assert!(final_text.ends_with("and then check"), "{final_text}");
    assert_eq!(end.line.as_deref(), Some(final_text));
}

#[test]
fn summary_updates_are_throttled_per_block_but_accumulate() {
    let home = tempfile::tempdir().unwrap();
    let mut gate = Gate::new(SinkLevel::Summary, 4_000, true);
    let got = run(
        &mut gate,
        home.path(),
        vec![
            (
                0,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some("one "),
                ),
            ),
            (
                100,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some("two "),
                ),
            ),
            (
                100,
                ev(
                    "t1",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some("other "),
                ),
            ),
            (
                450,
                ev(
                    "t0",
                    ThinkingPhase::Update,
                    ThinkingKind::Summary,
                    Some("three "),
                ),
            ),
        ],
    );
    let t0: Vec<&str> = got
        .iter()
        .filter(|e| e.block_id == "t0")
        .map(|e| e.text.as_deref().unwrap_or(""))
        .collect();
    assert_eq!(t0, ["one", "one two three"], "{got:?}");
    assert_eq!(got.iter().filter(|e| e.block_id == "t1").count(), 1);
}

#[test]
fn raw_reasoning_needs_redaction_to_show_text() {
    let home = tempfile::tempdir().unwrap();
    let mut off = Gate::new(SinkLevel::Summary, 4_000, false);
    let mut end = ev(
        "t0",
        ThinkingPhase::End,
        ThinkingKind::Raw,
        Some("raw chain of thought"),
    );
    end.tokens = Some(77);
    let got = run(&mut off, home.path(), vec![(0, end.clone())]);
    assert_eq!(got.len(), 1);
    assert!(got[0].text.is_none());
    assert!(got[0].raw_needs_redaction);
    assert_eq!(got[0].tokens, Some(77));
    // Summaries are vendor-written and still shown without redaction.
    let got = run(
        &mut off,
        home.path(),
        vec![(
            0,
            ev(
                "t1",
                ThinkingPhase::End,
                ThinkingKind::Summary,
                Some("a summary"),
            ),
        )],
    );
    assert_eq!(got[0].text.as_deref(), Some("a summary"));

    let mut on = Gate::new(SinkLevel::Summary, 4_000, true);
    let long = "字".repeat(2_000);
    let got = run(
        &mut on,
        home.path(),
        vec![(
            0,
            ev("t0", ThinkingPhase::End, ThinkingKind::Raw, Some(&long)),
        )],
    );
    assert_eq!(
        got[0].text.as_ref().unwrap().chars().count(),
        RAW_BLOCK_CAP_CHARS
    );
    assert!(got[0].truncated && !got[0].raw_needs_redaction);
}

#[test]
fn turn_budget_spans_blocks() {
    let home = tempfile::tempdir().unwrap();
    let mut gate = Gate::new(SinkLevel::Summary, 100, true);
    let eighty = "字".repeat(80);
    let got = run(
        &mut gate,
        home.path(),
        vec![
            (
                0,
                ev(
                    "t0",
                    ThinkingPhase::End,
                    ThinkingKind::Summary,
                    Some(&eighty),
                ),
            ),
            (
                1,
                ev(
                    "t1",
                    ThinkingPhase::End,
                    ThinkingKind::Summary,
                    Some(&eighty),
                ),
            ),
            (
                2,
                ev(
                    "t2",
                    ThinkingPhase::End,
                    ThinkingKind::Summary,
                    Some(&eighty),
                ),
            ),
        ],
    );
    assert_eq!(got[0].text.as_ref().unwrap().chars().count(), 80);
    assert!(!got[0].truncated);
    assert_eq!(got[1].text.as_ref().unwrap().chars().count(), 20);
    assert!(got[1].truncated);
    assert!(got[2].text.is_none() && got[2].truncated);
}

#[test]
fn contract_refusal_is_sticky_for_the_rest_of_the_turn() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join("agents").join("agnes");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("CONTRACT.toml"),
        "[boundaries]\nmust_not = [\"reveal api keys\"]\n",
    )
    .unwrap();
    let mut gate = Gate::new(SinkLevel::Summary, 4_000, true);
    let out = gate.process(
        ev(
            "t0",
            ThinkingPhase::End,
            ThinkingKind::Summary,
            Some("maybe reveal api keys"),
        ),
        0,
        &ctx(home.path()),
    );
    assert!(matches!(
        out.newly_blocked,
        Some(OutboundBlock::Contract(_))
    ));
    assert_eq!(out.events[0].kind, ThinkingKind::Redacted);
    assert!(out.events[0].text.is_none());
    // A later, harmless block stays withheld, and is not reported again.
    let out = gate.process(
        ev(
            "t1",
            ThinkingPhase::End,
            ThinkingKind::Summary,
            Some("harmless"),
        ),
        10,
        &ctx(home.path()),
    );
    assert!(out.newly_blocked.is_none());
    assert_eq!(out.events[0].kind, ThinkingKind::Redacted);
    assert!(out.events[0].text.is_none());
}

#[test]
fn turn_usage_passes_without_text() {
    let home = tempfile::tempdir().unwrap();
    let mut gate = Gate::new(SinkLevel::Summary, 4_000, true);
    let mut usage = ev(
        crate::thinking::event::TURN_BLOCK_ID,
        ThinkingPhase::End,
        ThinkingKind::Omitted,
        Some("ignored"),
    );
    usage.tokens = Some(1_240);
    let got = run(&mut gate, home.path(), vec![(0, usage)]);
    assert_eq!(got.len(), 1);
    assert!(got[0].text.is_none());
    assert_eq!(got[0].tokens, Some(1_240));
}
