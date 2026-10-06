//! F4 tests for the shared decision-routing pieces. Adapter-level entry tests
//! live in `adapter_tests/` (child modules of each channel adapter).

use super::*;
use crate::decision_notify::native_loop_fixture::{
    NativeCuFixture, assert_native_pending, pending_native_request,
};

const ID: &str = "0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c";

#[test]
fn only_verb_plus_full_request_id_is_a_decision() {
    // Conversation: no complete request id, so never intercepted (M1). The
    // F5-C tolerance for spacing and punctuation must not change this.
    for text in [
        "確認",
        "取消",
        "回答",
        "approve",
        "deny",
        "answer",
        " 確認 ",
        "確認！",
        "確認。",
        "確認  。",
        "approve the Q3 budget",
        "deny it",
        "answer me",
        "確認 一下",
        "確認 0b5c7a52",
        "確認 0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6",
        "確認 0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c7",
        "確認 。0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c",
        "我確認 0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c",
        "Approve 0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c",
        "確認：0b5c7a52-3f4e-4b8f-9a3d-6c1e2f4a5b6c",
        "",
    ] {
        assert_eq!(parse_strict_decision(text), None, "{text:?}");
        assert!(!is_strict_decision(text), "{text:?}");
    }
    let text_a = format!("確認 {ID}");
    let c = parse_strict_decision(&text_a).unwrap();
    assert_eq!((c.verb, c.id.as_str(), c.rest), ("確認", ID, None));
    assert!(c.approves() && !c.is_answer());
    let text_b = format!("  deny {ID}  ");
    let c = parse_strict_decision(&text_b).unwrap();
    assert!(!c.approves() && !c.is_answer());
    let text_d = format!("回答 {ID}  方案 B ");
    let c = parse_strict_decision(&text_d).unwrap();
    assert_eq!(c.rest, Some("方案 B"));
    assert!(c.is_answer());
    // Extra words after an approval still parse (route_bound_text refuses it).
    let text_e = format!("approve {ID} now");
    assert_eq!(parse_strict_decision(&text_e).unwrap().rest, Some("now"));
}

/// F5-C (review F4-L5): a near-miss with a complete id is still a decision —
/// several spaces, U+3000, closing punctuation after the id or at the end,
/// and other spellings of the same UUID (normalised to the stored form).
#[test]
fn near_miss_commands_with_a_full_id_are_still_decisions() {
    let upper = ID.to_uppercase();
    let simple = ID.replace('-', "");
    for text in [
        format!("確認  {ID}"),
        format!("確認\u{3000}{ID}"),
        format!("確認\u{3000}\u{3000}{ID}"),
        format!("確認 {ID}。"),
        format!("確認 {ID} 。"),
        format!("approve {ID}."),
        format!("approve {ID}!"),
        format!("確認 {ID}！"),
        format!("取消\t{ID}"),
        format!("  確認 {ID}  "),
        format!("確認 {upper}"),
        format!("確認 {simple}"),
    ] {
        let c = parse_strict_decision(&text).unwrap_or_else(|| panic!("{text:?}"));
        assert_eq!(c.id, ID, "{text:?}");
        assert_eq!(c.rest, None, "{text:?}");
    }
    let answer = format!("回答  {ID}。  方案 B");
    let c = parse_strict_decision(&answer).unwrap();
    assert_eq!((c.id.as_str(), c.rest), (ID, Some("方案 B")));
}

#[test]
fn telegram_shared_bot_and_forwarded_senders_are_never_principals() {
    assert_eq!(
        telegram_decision_principal(Some(11), false, false, false),
        "11"
    );
    for (id, bot, sender_chat, forwarded) in [
        (Some(1087968824), false, false, false),
        (Some(136817688), false, false, false),
        (Some(777000), false, false, false),
        (Some(11), true, false, false),
        (Some(11), false, true, false),
        (Some(11), false, false, true),
        (Some(-42), false, false, false),
        (Some(0), false, false, false),
        (None, false, false, false),
    ] {
        assert_eq!(
            telegram_decision_principal(id, bot, sender_chat, forwarded),
            "",
            "{id:?} bot={bot} sender_chat={sender_chat} forwarded={forwarded}"
        );
    }
}

/// F5-C (review F4-L4): only a leading mention of this bot is removed,
/// case-insensitively; another bot's mention, a mention later in the text and
/// a name that merely starts with this bot's name are left alone.
#[test]
fn only_a_leading_mention_of_this_bot_is_removed() {
    let cmd = format!("確認 {ID}");
    for (text, expected) in [
        (format!("@dudu {cmd}"), cmd.clone()),
        (format!("@DuDu {cmd}"), cmd.clone()),
        (format!("  @DUDU   {cmd}"), cmd.clone()),
        (format!("@dudubot {cmd}"), format!("@dudubot {cmd}")),
        (format!("@other {cmd}"), format!("@other {cmd}")),
        (format!("{cmd} @dudu"), format!("{cmd} @dudu")),
        ("@dudu".to_string(), String::new()),
    ] {
        assert_eq!(strip_leading_mention(&text, "dudu"), expected, "{text:?}");
    }
    assert_eq!(
        strip_leading_mention(&format!("@dudu {cmd}"), ""),
        format!("@dudu {cmd}")
    );
}

#[test]
fn discord_lanes_follow_the_strict_decision_definition() {
    assert_eq!(
        DiscordLane::for_message(&format!("確認 {ID}")),
        DiscordLane::Decision
    );
    for text in ["確認", "approve the Q3 budget", "hello", ""] {
        assert_eq!(DiscordLane::for_message(text), DiscordLane::General);
    }
    let button = |kind: u64, custom_id: &str| serde_json::json!({"type": kind, "data": {"custom_id": custom_id}});
    let decision_id = crate::decision_action::encode(
        crate::decision_action::DecisionSource::Approval,
        crate::decision_action::DecisionAct::Approve,
        ID,
    );
    assert_eq!(
        DiscordLane::for_interaction(&button(3, &decision_id)),
        DiscordLane::Decision
    );
    assert_eq!(
        DiscordLane::for_interaction(&button(2, &decision_id)),
        DiscordLane::General,
        "a slash command is not a decision button"
    );
    assert_eq!(
        DiscordLane::for_interaction(&button(3, "duduclaw:other:x")),
        DiscordLane::General
    );
}

/// Review M3: with every ordinary permit held by long replies, a decision
/// still gets a permit at once; ordinary work keeps waiting.
#[tokio::test]
async fn saturated_ordinary_permits_never_delay_a_decision() {
    let permits = DiscordPermits::new(10, 4);
    let mut held = Vec::new();
    for _ in 0..10 {
        held.push(permits.acquire(DiscordLane::General).await.unwrap());
    }
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            permits.acquire(DiscordLane::General)
        )
        .await
        .is_err(),
        "ordinary pool should be exhausted"
    );
    let decision = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        permits.acquire(DiscordLane::Decision),
    )
    .await
    .expect("decision waited behind ordinary work")
    .unwrap();
    drop(decision);
    drop(held);
    assert_eq!(
        super::discord_lanes::DISCORD_GENERAL_PERMITS,
        10,
        "ordinary concurrency is unchanged"
    );
}

#[test]
fn threat_level_absent_is_green_and_unreadable_or_unknown_is_red() {
    use crate::computer_use_orchestrator::{
        ThreatLevel, read_threat_level_sync, threat_level_from_read,
    };
    let not_found = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    assert_eq!(threat_level_from_read(not_found()), ThreatLevel::Green);
    for denied in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::InvalidData,
        std::io::ErrorKind::Other,
    ] {
        assert_eq!(
            threat_level_from_read(Err(std::io::Error::from(denied))),
            ThreatLevel::Red,
            "{denied:?}"
        );
    }
    for (raw, level) in [
        ("GREEN\n", ThreatLevel::Green),
        (" green ", ThreatLevel::Green),
        ("YELLOW\n", ThreatLevel::Yellow),
        ("RED", ThreatLevel::Red),
        ("", ThreatLevel::Red),
        ("GRREN", ThreatLevel::Red),
        ("ok", ThreatLevel::Red),
    ] {
        assert_eq!(threat_level_from_read(Ok(raw.into())), level, "{raw:?}");
    }
    let home = tempfile::tempdir().unwrap();
    assert_eq!(read_threat_level_sync(home.path()), ThreatLevel::Green);
    // Exists but cannot be read as a file.
    std::fs::create_dir(home.path().join("threat_level")).unwrap();
    assert_eq!(read_threat_level_sync(home.path()), ThreatLevel::Red);
    std::fs::remove_dir(home.path().join("threat_level")).unwrap();
    std::fs::write(home.path().join("threat_level"), [0xff, 0xfe]).unwrap();
    assert_eq!(read_threat_level_sync(home.path()), ThreatLevel::Red);
}

/// Review L7: a Slack bot whose account cannot be verified is reported in
/// `duduclaw doctor` and once in the Activity Feed; a later success clears it.
#[tokio::test]
async fn slack_identity_failure_reaches_doctor_and_activity_feed_once() {
    let fixture = NativeCuFixture::new("slack-identity-fixture").await;
    let home = fixture.home.path();
    let (warn, message) = slack_identity_doctor(home);
    assert!(!warn);
    assert!(message.contains("users:read"), "{message}");
    let failure = || SlackIdentityFailure {
        step: "bots.info".into(),
        error: "missing_scope".into(),
        needed: Some("users:read".into()),
        at: "2026-10-06T00:00:00Z".into(),
    };
    note_slack_identity_failure(&fixture.ctx, "slack:alice", failure()).await;
    note_slack_identity_failure(&fixture.ctx, "slack:alice", failure()).await;
    let (warn, message) = slack_identity_doctor(home);
    assert!(warn);
    assert!(
        message.contains("slack:alice")
            && message.contains("bots.info")
            && message.contains("missing_scope")
            && message.contains("users:read"),
        "{message}"
    );
    let store = crate::task_store::TaskStore::open(home).unwrap();
    let (rows, total) = store
        .list_activity(None, Some("slack_decision_identity_unavailable"), 10, 0)
        .await
        .unwrap();
    assert_eq!(total, 1, "one Activity Feed row per bot and process");
    assert!(rows[0].summary.contains("users:read"));
    note_slack_identity_ok(home, "slack:alice");
    assert!(!slack_identity_doctor(home).0);
}

fn private_context(principal: &str) -> crate::approval::DecisionContext {
    crate::approval::DecisionContext {
        channel: "telegram".into(),
        account_id: "telegram|7".into(),
        conversation_id: "11".into(),
        principal_id: principal.into(),
    }
}

const PRIVATE: crate::decision_notify::DecisionAccessScope<'static> =
    crate::decision_notify::DecisionAccessScope {
        channel_id: None,
        guild_id: None,
        session_id: None,
    };

/// Review M1 + L2 at the shared router: verb-first conversation is not
/// consumed; unknown ids, foreign requests and refused senders all get the
/// same sentence, and nothing is decided.
#[tokio::test]
async fn fastlane_ignores_conversation_and_refuses_uniformly() {
    let fixture = NativeCuFixture::new("uniform-refusal-fixture").await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    let route = |context: crate::approval::DecisionContext, text: String| {
        let ctx = fixture.ctx.clone();
        async move {
            crate::decision_notify::route_trusted_decision_fastlane_with_scope(
                &ctx, &context, &text, PRIVATE,
            )
            .await
        }
    };
    for text in [
        "確認",
        "取消",
        "approve the Q3 budget",
        "deny it",
        "answer me",
    ] {
        assert!(
            route(private_context("11"), text.into()).await.is_none(),
            "{text} must reach the model"
        );
    }
    let unknown = route(
        private_context("11"),
        format!("確認 {}", uuid::Uuid::new_v4()),
    )
    .await;
    let foreign = route(private_context("22"), format!("確認 {}", id.as_str())).await;
    let mut other_account = private_context("11");
    other_account.account_id = "telegram|8".into();
    let wrong_account = route(other_account, format!("確認 {}", id.as_str())).await;
    let anonymous = route(private_context(""), format!("確認 {}", id.as_str())).await;
    fixture
        .ctx
        .channel_settings
        .set("telegram", "global", "blocked_users", r#"["11"]"#)
        .await
        .unwrap();
    let blocked = route(private_context("11"), format!("確認 {}", id.as_str())).await;
    let blocked_unknown = route(
        private_context("11"),
        format!("確認 {}", uuid::Uuid::new_v4()),
    )
    .await;
    for (name, outcome) in [
        ("unknown", unknown),
        ("foreign", foreign),
        ("wrong_account", wrong_account),
        ("anonymous", anonymous),
        ("blocked", blocked),
        ("blocked_unknown", blocked_unknown),
    ] {
        assert_eq!(
            outcome,
            Some(Err(DECISION_REFUSED.to_string())),
            "{name} must get the one uniform refusal"
        );
    }
    assert_native_pending(&fixture, &id).await;
}

/// F5-C (review F4-L6): an approval button checks channel access before the
/// request row is read. A refused presser gets the uniform refusal for an
/// unknown id and for a bound request alike; a legacy (unbound) card keeps its
/// shipped authorization and is not newly subject to the channel settings.
#[tokio::test]
async fn button_presses_check_access_first_and_keep_legacy_cards_working() {
    use crate::decision_action::{DecisionAct, DecisionSource, encode};
    let fixture = NativeCuFixture::new("press-order-fixture").await;
    let id = pending_native_request(&fixture, private_context("11")).await;
    fixture
        .ctx
        .channel_settings
        .set("telegram", "global", "blocked_users", r#"["11"]"#)
        .await
        .unwrap();
    let press = |request: String| {
        let ctx = fixture.ctx.clone();
        async move {
            crate::decision_notify::route_verified_bound_press(
                &ctx,
                &private_context("11"),
                &encode(DecisionSource::Approval, DecisionAct::Approve, &request),
                PRIVATE,
            )
            .await
        }
    };
    let bound = press(id.as_str().to_owned()).await;
    let unknown = press(uuid::Uuid::new_v4().to_string()).await;
    assert_eq!(bound, Some(Err(DECISION_REFUSED.to_string())));
    assert_eq!(unknown, bound);
    assert_native_pending(&fixture, &id).await;

    // Legacy card: reaches its own authorization (here: no dashboard user
    // linked, so it is refused with the legacy wording, not the channel one).
    let legacy = crate::channel_decision_route::test_support::legacy_card(
        fixture.home.path(),
        "telegram",
        "11",
        "9001",
    )
    .await;
    let legacy_outcome = press(legacy.as_str().to_owned()).await;
    assert!(
        matches!(&legacy_outcome, Some(Err(e)) if e != DECISION_REFUSED),
        "{legacy_outcome:?}"
    );
}

/// F5-C (review F4-L1): a UTF-8 BOM and surrounding whitespace are tolerated;
/// a file that becomes readable between re-reads is honoured; one that stays
/// empty is RED.
#[tokio::test]
async fn threat_level_tolerates_bom_and_rereads_an_unclear_file() {
    use crate::computer_use_orchestrator::{
        ThreatLevel, read_threat_level, read_threat_level_sync, threat_level_from_read,
    };
    assert_eq!(
        threat_level_from_read(Ok("\u{FEFF}GREEN\r\n".into())),
        ThreatLevel::Green
    );
    assert_eq!(
        threat_level_from_read(Ok("\u{FEFF} yellow ".into())),
        ThreatLevel::Yellow
    );
    let home = tempfile::tempdir().unwrap();
    let path = home.path().join("threat_level");
    std::fs::write(&path, "").unwrap();
    assert_eq!(read_threat_level(home.path()).await, ThreatLevel::Red);
    assert_eq!(read_threat_level_sync(home.path()), ThreatLevel::Red);
    // Truncated then rewritten while the reader waits: the re-read wins.
    let writer = {
        let path = path.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            std::fs::write(&path, "GREEN\n").unwrap();
        })
    };
    assert_eq!(read_threat_level(home.path()).await, ThreatLevel::Green);
    writer.await.unwrap();
    std::fs::write(&path, "\u{FEFF}RED").unwrap();
    assert_eq!(read_threat_level_sync(home.path()), ThreatLevel::Red);
}
