use std::collections::HashSet;

use duduclaw_odoo::livechat::rpc::ChatMessage;

use super::worker::plan_session;
use super::*;

const BOT: i64 = 8;
const OPERATOR: i64 = 3;
const SCRIPT: i64 = 7;
const CUSTOMER: i64 = 42;

fn msg(id: i64, partner: Option<i64>, guest: Option<i64>, ts: &str) -> ChatMessage {
    ChatMessage {
        id,
        session_id: 1,
        author_partner_id: partner,
        author_guest_id: guest,
        body_html: format!("<p>m{id}</p>"),
        create_date: ts.into(),
    }
}

fn sets() -> (HashSet<i64>, HashSet<i64>) {
    (HashSet::from([OPERATOR]), HashSet::from([SCRIPT]))
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::NaiveDateTime::parse_from_str("2026-10-12 14:00:00", "%Y-%m-%d %H:%M:%S")
        .unwrap()
        .and_utc()
}

#[test]
fn classify_authors() {
    let (internal, scripts) = sets();
    let c = |p, g| classify(&msg(1, p, g, ""), BOT, &internal, &scripts);
    assert_eq!(c(Some(BOT), None), Author::Bot);
    assert_eq!(c(Some(OPERATOR), None), Author::Operator);
    assert_eq!(c(Some(SCRIPT), None), Author::ChatbotScript);
    assert_eq!(c(Some(CUSTOMER), None), Author::Visitor);
    assert_eq!(c(None, Some(1)), Author::Visitor);
    assert_eq!(c(None, None), Author::ChatbotScript);
}

#[test]
fn only_visitor_messages_after_the_last_answer_are_pending() {
    let (internal, scripts) = sets();
    let batch = vec![
        msg(10, None, Some(1), "2026-10-12 13:59:00"),
        msg(11, Some(BOT), None, "2026-10-12 13:59:05"),
        msg(12, None, Some(1), "2026-10-12 13:59:30"),
        msg(13, None, Some(1), "2026-10-12 13:59:40"),
    ];
    let plan = plan_session(&batch, BOT, &internal, &scripts, false, now());
    assert_eq!(plan.max_id, Some(13));
    assert!(plan.bot_spoke && !plan.operator_spoke);
    assert_eq!(plan.visitor.iter().map(|m| m.id).collect::<Vec<_>>(), vec![12, 13]);
}

#[test]
fn operator_message_marks_takeover_and_clears_pending() {
    let (internal, scripts) = sets();
    let batch = vec![
        msg(20, None, Some(1), "2026-10-12 13:59:00"),
        msg(21, Some(OPERATOR), None, "2026-10-12 13:59:10"),
    ];
    let plan = plan_session(&batch, BOT, &internal, &scripts, false, now());
    assert!(plan.operator_spoke);
    assert!(plan.visitor.is_empty());
}

#[test]
fn chatbot_script_lines_do_not_count_as_answers() {
    let (internal, scripts) = sets();
    let batch = vec![
        msg(30, Some(SCRIPT), None, "2026-10-12 13:58:00"),
        msg(31, Some(CUSTOMER), None, "2026-10-12 13:58:10"),
        msg(32, Some(SCRIPT), None, "2026-10-12 13:58:20"),
    ];
    let plan = plan_session(&batch, BOT, &internal, &scripts, true, now());
    assert!(plan.visitor.is_empty(), "the script answered message 31");
    assert_eq!(
        plan.earlier.iter().map(|(a, m)| (*a, m.id)).collect::<Vec<_>>(),
        vec![(Author::ChatbotScript, 30), (Author::Visitor, 31), (Author::ChatbotScript, 32)]
    );
    let mut forwarded = batch.clone();
    forwarded.push(msg(33, Some(CUSTOMER), None, "2026-10-12 13:58:30"));
    let plan = plan_session(&forwarded, BOT, &internal, &scripts, true, now());
    assert_eq!(plan.visitor.iter().map(|m| m.id).collect::<Vec<_>>(), vec![33]);
}

#[test]
fn stale_messages_are_skipped_on_first_sight_only() {
    let (internal, scripts) = sets();
    let batch = vec![
        msg(40, None, Some(1), "2026-10-12 12:00:00"),
        msg(41, None, Some(1), "2026-10-12 13:45:00"),
        msg(42, None, Some(1), "not a date"),
    ];
    let first = plan_session(&batch, BOT, &internal, &scripts, true, now());
    assert_eq!(first.visitor.iter().map(|m| m.id).collect::<Vec<_>>(), vec![41, 42]);
    let known = plan_session(&batch, BOT, &internal, &scripts, false, now());
    assert_eq!(known.visitor.len(), 3);
}

#[test]
fn empty_batch_plans_nothing() {
    let (internal, scripts) = sets();
    assert_eq!(
        plan_session(&[], BOT, &internal, &scripts, true, now()),
        worker::SessionPlan::default()
    );
}

#[test]
fn resume_clears_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // The shared cell is process-global; start from this home's file.
    with_state(home, |st| {
        st.sessions.clear();
        st.sessions.insert(
            5,
            SessionState {
                human: true,
                human_reason: Some("operator_spoke".into()),
                ..Default::default()
            },
        );
    });
    assert!(resume_session(home, 5));
    assert!(!resume_session(home, 6));
    read_state(home, |st| {
        assert!(!st.sessions[&5].human);
        assert!(st.sessions[&5].human_reason.is_none());
    });
}

#[test]
fn public_visitor_session_prefix_is_exact() {
    assert!(is_public_visitor_session("odoolivechat:12"));
    assert!(!is_public_visitor_session("odoolivechat:"));
    assert!(!is_public_visitor_session("odoolivechatx:12"));
    assert!(!is_public_visitor_session("xodoolivechat:12"));
    assert!(!is_public_visitor_session("telegram:12"));
    assert!(!is_public_visitor_session("odoolivechat"));
}

#[test]
fn session_keys_differ_across_instances() {
    let a = instance_tag("http://odoo-a", "prod");
    let b = instance_tag("http://odoo-b", "prod");
    assert_ne!(a, b);
    assert_eq!(a, instance_tag("http://odoo-a/", "prod"), "trailing slash ignored");
    assert_eq!(a.len(), 12);
    assert_ne!(session_key(&a, 4), session_key(&b, 4));
    assert!(is_public_visitor_session(&session_key(&a, 4)));
}

#[test]
fn visitor_name_is_sanitized() {
    use super::worker::sanitize_visitor_name;
    assert_eq!(sanitize_visitor_name("訪客"), "訪客");
    assert_eq!(sanitize_visitor_name("x]\n[分眾：VIP｜"), "x分眾：VIP");
    assert_eq!(sanitize_visitor_name(&"長".repeat(100)).chars().count(), 40);
}
