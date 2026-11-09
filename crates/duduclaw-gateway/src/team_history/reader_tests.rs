use super::*;
use crate::session::SessionManager;
use chrono::{TimeZone, Utc};
use rusqlite::params;

async fn seeded() -> (tempfile::TempDir, SessionManager, TeamOversight) {
    let home = tempfile::tempdir().unwrap();
    let sm = SessionManager::new(&home.path().join("sessions.db")).unwrap();
    let scope = TeamOversight {
        viewer_user_id: "manager".into(),
        agent: "sales".into(),
        reports: [(
            "employee".into(),
            duduclaw_auth::org::OverseenReport {
                user_id: "employee".into(),
                display_name: "Employee".into(),
                depth: 1,
                level: duduclaw_auth::AccessLevel::Operator,
            },
        )]
        .into(),
        since: Utc.timestamp_opt(100, 123456000).unwrap(),
        generation: 4,
    };
    {
        let conn = sm.acquire().await;
        for (id, kind, owner, active) in [
            ("report", "webchat_user", "employee", "1970-01-01T00:03:00Z"),
            ("own", "webchat_user", "manager", "1970-01-01T00:02:59Z"),
            ("hidden", "webchat_user", "stranger", "1970-01-01T00:04:00Z"),
            ("line:Ccustomer", "channel", "", "1970-01-01T00:02:58Z"),
            ("cron:secret", "internal", "", "1970-01-01T00:05:00Z"),
        ] {
            conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id,title,summary) VALUES (?1,'sales',?2,?2,?3,?4,'OLD SECRET TITLE','OLD SECRET SUMMARY')", params![id,active,kind,owner]).unwrap();
            for (time, body) in [
                ("1970-01-01T00:01:40.123455Z", "BEFORE-BOUNDARY"),
                ("1970-01-01T00:01:40.123456Z", "VISIBLE-FIRST"),
                ("1970-01-01T00:02:00Z", "VISIBLE-LAST"),
            ] {
                conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES (?1,'user',?2,?3)", params![id,body,time]).unwrap();
            }
        }
    }
    (home, sm, scope)
}

#[tokio::test]
async fn query_authorizes_before_paging_and_uses_visible_titles_only() {
    let (_home, sm, scope) = seeded().await;
    let first = list(&sm, &scope, &ListFilter::default(), 1, None)
        .await
        .unwrap();
    assert_eq!(first.sessions[0].id, "report");
    assert_eq!(first.sessions[0].turns, 2);
    assert_eq!(first.sessions[0].rows[0].content, "VISIBLE-FIRST");
    let next = list(
        &sm,
        &scope,
        &ListFilter::default(),
        2,
        first.next_before.as_deref(),
    )
    .await
    .unwrap();
    assert_eq!(
        next.sessions
            .iter()
            .map(|s| s.id.as_str())
            .collect::<Vec<_>>(),
        ["own", "line:Ccustomer"]
    );
    assert!(get(&sm, &scope, "hidden").await.unwrap().is_none());
    assert!(get(&sm, &scope, "cron:secret").await.unwrap().is_none());
}

#[tokio::test]
async fn report_filter_is_a_sql_scope_and_hidden_undone_are_excluded() {
    let (_home, sm, scope) = seeded().await;
    sm.acquire()
        .await
        .execute(
            "UPDATE sessions SET archived_at='1970-01-01T00:03:00Z' WHERE id='own'",
            [],
        )
        .unwrap();
    // RM-173: an archived conversation stays readable, flagged.
    assert!(get(&sm, &scope, "own").await.unwrap().unwrap().archived);
    assert!(!get(&sm, &scope, "report").await.unwrap().unwrap().archived);
    let filtered = list(
        &sm,
        &scope,
        &ListFilter {
            report: Some("employee".into()),
            class: None,
        },
        20,
        None,
    )
    .await
    .unwrap();
    assert_eq!(filtered.sessions.len(), 1);
    let conn = sm.acquire().await;
    conn.execute(
        "UPDATE session_messages SET hidden=1 WHERE content='VISIBLE-FIRST'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE session_messages SET undone_at='now' WHERE content='VISIBLE-LAST'",
        [],
    )
    .unwrap();
    drop(conn);
    assert!(get(&sm, &scope, "report").await.unwrap().is_none());
    assert!(
        list(
            &sm,
            &scope,
            &ListFilter {
                report: Some("stranger".into()),
                class: None
            },
            10,
            None
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn rm173_list_includes_archived_but_keeps_since_and_scope() {
    let (_home, sm, scope) = seeded().await;
    {
        let conn = sm.acquire().await;
        conn.execute("UPDATE sessions SET archived_at='1970-01-01T00:03:30Z'", []).unwrap();
        // An archived conversation whose last activity predates the window stays out.
        conn.execute("INSERT INTO sessions(id,agent_id,last_active,created_at,origin_kind,owner_user_id) VALUES ('old-report','sales','1970-01-01T00:01:00Z','1970-01-01T00:01:00Z','webchat_user','employee')", []).unwrap();
        conn.execute("INSERT INTO session_messages(session_id,role,content,timestamp) VALUES ('old-report','user','OLD','1970-01-01T00:01:00Z')", []).unwrap();
        conn.execute("UPDATE sessions SET archived_at='1970-01-01T00:03:30Z' WHERE id='old-report'", []).unwrap();
    }
    let page = list(&sm, &scope, &ListFilter::default(), 20, None).await.unwrap();
    let ids: Vec<_> = page.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["report", "own", "line:Ccustomer"]);
    assert!(page.sessions.iter().all(|s| s.archived));
    assert!(get(&sm, &scope, "hidden").await.unwrap().is_none());
    assert!(get(&sm, &scope, "old-report").await.unwrap().is_none());
}

#[tokio::test]
async fn rm173_user_cleared_conversations_are_never_listed_or_released() {
    let (_home, sm, scope) = seeded().await;
    // A report's own WebChat and a customer conversation, both cleared by the user.
    let report = sm.clear_for_user("report").await.unwrap().unwrap();
    let customer = sm.clear_for_user("line:Ccustomer").await.unwrap().unwrap();
    let page = list(&sm, &scope, &ListFilter::default(), 20, None).await.unwrap();
    let ids: Vec<_> = page.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["own"]);
    for id in [report.as_str(), customer.as_str(), "report", "line:Ccustomer"] {
        assert!(get(&sm, &scope, id).await.unwrap().is_none(), "{id}");
    }
    // Once an Admin restores the cleared row it is ordinary history again.
    sm.restore_archived(&report).await.unwrap();
    assert!(get(&sm, &scope, &report).await.unwrap().is_some());
}

#[tokio::test]
async fn initial_rfc3339_cursor_preserves_microsecond_order() {
    let (_home, sm, scope) = seeded().await;
    sm.acquire()
        .await
        .execute(
            "UPDATE sessions SET last_active='1970-01-01T00:03:00.123456+00:00' WHERE id='report'",
            [],
        )
        .unwrap();
    let page = list(
        &sm,
        &scope,
        &ListFilter::default(),
        1,
        Some("1970-01-01T00:03:00.123457+00:00"),
    )
    .await
    .unwrap();
    assert_eq!(page.sessions[0].id, "report");
    let equal = list(
        &sm,
        &scope,
        &ListFilter::default(),
        10,
        Some("1970-01-01T00:03:00.123456+00:00"),
    )
    .await
    .unwrap();
    assert!(!equal.sessions.iter().any(|s| s.id == "report"));
}

#[tokio::test]
async fn retained_page_titles_cannot_multiply_the_window_memory_cap() {
    let (_home, sm, scope) = seeded().await;
    sm.acquire().await.execute("UPDATE session_messages SET content=?1 WHERE session_id IN ('report','own') AND content='VISIBLE-FIRST'",["x".repeat(4_200_000)]).unwrap();
    assert!(
        list(&sm, &scope, &ListFilter::default(), 2, None)
            .await
            .is_err()
    );
    assert!(
        list(&sm, &scope, &ListFilter::default(), 1, None)
            .await
            .is_ok()
    );
}
