use super::*;

fn targets() -> Vec<MentionTarget> {
    vec![
        MentionTarget::new("dudu", "嘟嘟總機", "@嘟嘟總機"),
        MentionTarget::new("finance", "財務小幫手", "@財務小幫手"),
        MentionTarget::new("sales-rep", "業務專員", "@sales-rep"),
    ]
}

fn all(_: &str) -> bool {
    true
}

fn hit_id(scan: &MentionScan) -> Option<&str> {
    match scan {
        MentionScan::Hit(h) => Some(h.id.as_str()),
        _ => None,
    }
}

#[test]
fn target_names_strip_at_and_dedupe() {
    let t = MentionTarget::new("sales-rep", "業務專員", "@sales-rep");
    assert_eq!(
        t.names,
        vec!["sales-rep".to_string(), "業務專員".to_string()]
    );
    let t = MentionTarget::new("x", "", "  ");
    assert_eq!(t.names, vec!["x".to_string()]);
}

#[test]
fn trigger_display_name_and_id_all_match() {
    let t = targets();
    assert_eq!(
        hit_id(&scan_mentions("@財務小幫手 幫我算帳", &t, &all)),
        Some("finance")
    );
    assert_eq!(
        hit_id(&scan_mentions("@業務專員 報價", &t, &all)),
        Some("sales-rep")
    );
    assert_eq!(
        hit_id(&scan_mentions("@sales-rep 報價", &t, &all)),
        Some("sales-rep")
    );
    assert_eq!(
        hit_id(&scan_mentions("@finance 幫我", &t, &all)),
        Some("finance")
    );
}

#[test]
fn case_insensitive_ascii_names() {
    let t = targets();
    assert_eq!(
        hit_id(&scan_mentions("@Sales-Rep hi", &t, &all)),
        Some("sales-rep")
    );
    assert_eq!(
        hit_id(&scan_mentions("@SALES-REP", &t, &all)),
        Some("sales-rep")
    );
}

#[test]
fn full_width_at_sign_is_a_mention() {
    let t = targets();
    assert_eq!(
        hit_id(&scan_mentions("＠財務小幫手 幫我", &t, &all)),
        Some("finance")
    );
}

#[test]
fn cjk_name_may_run_straight_into_text() {
    let t = targets();
    assert_eq!(
        hit_id(&scan_mentions("@財務小幫手幫我算帳", &t, &all)),
        Some("finance")
    );
    assert_eq!(
        hit_id(&scan_mentions("請@財務小幫手算帳", &t, &all)),
        Some("finance")
    );
    // ASCII name followed by CJK text is still a boundary.
    assert_eq!(
        hit_id(&scan_mentions("@sales-rep報價", &t, &all)),
        Some("sales-rep")
    );
}

#[test]
fn ascii_name_must_not_run_into_more_ascii() {
    let t = vec![MentionTarget::new("ann", "Ann", "@ann")];
    assert_eq!(scan_mentions("@anna hello", &t, &all), MentionScan::None);
    assert_eq!(scan_mentions("@ann-2 hello", &t, &all), MentionScan::None);
    assert_eq!(hit_id(&scan_mentions("@ann, hello", &t, &all)), Some("ann"));
    assert_eq!(hit_id(&scan_mentions("@ann.", &t, &all)), Some("ann"));
}

#[test]
fn email_addresses_are_not_mentions() {
    let t = vec![MentionTarget::new("example", "Example", "@example")];
    assert_eq!(
        scan_mentions("mail ann@example.com", &t, &all),
        MentionScan::None
    );
    assert_eq!(scan_mentions("a.b@example", &t, &all), MentionScan::None);
}

#[test]
fn no_at_sign_never_switches() {
    let t = targets();
    assert_eq!(
        scan_mentions("財務小幫手，幫我算帳", &t, &all),
        MentionScan::None
    );
    assert_eq!(scan_mentions("sales-rep 報價", &t, &all), MentionScan::None);
}

#[test]
fn longest_name_wins_over_a_prefix_name() {
    let t = vec![
        MentionTarget::new("fin", "財務", "@財務"),
        MentionTarget::new("finance", "財務小幫手", "@財務小幫手"),
    ];
    assert_eq!(
        hit_id(&scan_mentions("@財務小幫手幫我", &t, &all)),
        Some("finance")
    );
    assert_eq!(hit_id(&scan_mentions("@財務 幫我", &t, &all)), Some("fin"));
}

#[test]
fn a_partial_name_offers_the_list_instead_of_routing() {
    let t = targets();
    match scan_mentions("@財務 幫我算帳", &t, &all) {
        MentionScan::Partial { token, candidates } => {
            assert_eq!(token, "財務");
            assert_eq!(candidates, vec!["finance".to_string()]);
        }
        other => panic!("expected partial, got {other:?}"),
    }
    match scan_mentions("@sal", &t, &all) {
        MentionScan::Partial { candidates, .. } => {
            assert_eq!(candidates, vec!["sales-rep".to_string()])
        }
        other => panic!("expected partial, got {other:?}"),
    }
}

#[test]
fn lone_at_sign_lists_every_callable_employee() {
    let t = targets();
    let only_finance = |id: &str| id == "finance" || id == "dudu";
    match scan_mentions("@", &t, &only_finance) {
        MentionScan::Partial { token, candidates } => {
            assert_eq!(token, "");
            assert_eq!(candidates, vec!["dudu".to_string(), "finance".to_string()]);
        }
        other => panic!("expected list, got {other:?}"),
    }
    assert!(matches!(
        scan_mentions("找誰呢 @  ", &t, &all),
        MentionScan::Partial { .. }
    ));
    // `@` in the middle of prose is not a list request.
    assert_eq!(scan_mentions("meet @ 5pm", &t, &all), MentionScan::None);
}

#[test]
fn a_partial_matching_nobody_is_ignored() {
    let t = targets();
    assert_eq!(scan_mentions("@john hi", &t, &all), MentionScan::None);
    // Discord/Slack native mention markup is not an employee mention.
    assert_eq!(scan_mentions("<@123456> hi", &t, &all), MentionScan::None);
}

#[test]
fn earliest_callable_mention_wins() {
    let t = targets();
    let s = scan_mentions("@業務專員 跟 @財務小幫手 一起看", &t, &all);
    assert_eq!(hit_id(&s), Some("sales-rep"));
    let s = scan_mentions("@財務小幫手 跟 @業務專員 一起看", &t, &all);
    assert_eq!(hit_id(&s), Some("finance"));
}

#[test]
fn a_mention_the_sender_may_not_call_is_skipped_for_a_later_callable_one() {
    let t = targets();
    let only_finance = |id: &str| id == "finance";
    let s = scan_mentions("@sales-rep 和 @財務小幫手", &t, &only_finance);
    assert_eq!(hit_id(&s), Some("finance"));
}

#[test]
fn only_forbidden_mentions_report_forbidden() {
    let t = targets();
    let only_finance = |id: &str| id == "finance";
    match scan_mentions("@sales-rep 報價", &t, &only_finance) {
        MentionScan::Forbidden(h) => assert_eq!(h.id, "sales-rep"),
        other => panic!("expected forbidden, got {other:?}"),
    }
}

#[test]
fn forbidden_longest_match_is_not_downgraded_to_a_callable_prefix() {
    // `@財務小幫手` names the forbidden employee; it must not route to the
    // callable `財務` with "小幫手" left over.
    let t = vec![
        MentionTarget::new("fin", "財務", "@財務"),
        MentionTarget::new("finance", "財務小幫手", "@財務小幫手"),
    ];
    let only_fin = |id: &str| id == "fin";
    assert!(matches!(
        scan_mentions("@財務小幫手 幫我", &t, &only_fin),
        MentionScan::Forbidden(_)
    ));
}

#[test]
fn duplicate_names_offer_a_choice() {
    let t = vec![
        MentionTarget::new("a1", "助理", ""),
        MentionTarget::new("a2", "助理", ""),
    ];
    match scan_mentions("@助理 幫我", &t, &all) {
        MentionScan::Partial { candidates, .. } => assert_eq!(candidates.len(), 2),
        other => panic!("expected a choice, got {other:?}"),
    }
    let only_a2 = |id: &str| id == "a2";
    assert_eq!(
        hit_id(&scan_mentions("@助理 幫我", &t, &only_a2)),
        Some("a2")
    );
}

#[test]
fn task_text_strips_the_mention_and_separators() {
    let t = targets();
    let MentionScan::Hit(h) = scan_mentions("@財務小幫手", &t, &all) else {
        panic!()
    };
    assert_eq!(task_text_without("@財務小幫手", &h), "");
    let MentionScan::Hit(h) = scan_mentions("@財務小幫手，", &t, &all) else {
        panic!()
    };
    assert_eq!(task_text_without("@財務小幫手，", &h), "");
    let text = "@財務小幫手 幫我算帳";
    let MentionScan::Hit(h) = scan_mentions(text, &t, &all) else {
        panic!()
    };
    assert_eq!(task_text_without(text, &h), "幫我算帳");
}

#[test]
fn multibyte_text_never_panics() {
    let t = targets();
    for s in [
        "@",
        "＠",
        "@🐾",
        "🐾@財",
        "@財務小幫手🐾",
        "@\u{301}",
        "ab@",
    ] {
        let _ = scan_mentions(s, &t, &all);
    }
}
