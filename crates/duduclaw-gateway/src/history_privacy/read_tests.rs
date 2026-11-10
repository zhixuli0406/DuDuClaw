use super::*;
fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,agent_id TEXT,origin_kind TEXT,owner_user_id TEXT,title TEXT,last_active TEXT,archived_at TEXT,archive_reason TEXT,user_cleared_at TEXT);CREATE TABLE session_messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,tokens INTEGER,timestamp TEXT,hidden INTEGER DEFAULT 0,undone_at TEXT);").unwrap();
    c
}
fn session(c: &Connection, id: &str, kind: &str, owner: &str, time: &str) {
    c.execute("INSERT INTO sessions(id,agent_id,origin_kind,owner_user_id,title,last_active) VALUES (?1,'sales',?2,?3,'TITLE SECRET',?4)",params![id,kind,owner,time]).unwrap();
}
#[test]
fn bound_history_sql_scopes_before_limit_and_never_reads_external_stored_titles() {
    let c = db();
    for i in 0..100 {
        session(
            &c,
            &format!("webchat:other:{i}"),
            "webchat_user",
            "other",
            "2026-10-24T10:00:00Z",
        );
    }
    session(
        &c,
        "webchat:mine",
        "webchat_user",
        "me",
        "2026-10-23T10:00:00Z",
    );
    let page = list(&c, "me", "sales", 1).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].id, "webchat:mine");
    assert_eq!(page[0].title.as_deref(), Some("TITLE SECRET"));
    session(
        &c,
        "whatsapp:+886912345678",
        "channel",
        "",
        "2026-10-24T11:00:00Z",
    );
    let page = list(&c, "me", "sales", 1).unwrap();
    assert_eq!(page.len(), 1);
    assert!(matches!(page[0].class, Class::External { .. }));
    assert!(page[0].title.is_none());
    assert_eq!(
        metadata(&c, "me", "webchat:other:0")
            .unwrap()
            .unwrap()
            .class,
        Class::Hidden
    );
}
#[test]
fn bound_history_sql_hides_internal_unknown_archived_and_inconsistent_external_rows() {
    let c = db();
    for (id, kind) in [
        ("cron:task", "internal"),
        ("legacy:any", "unknown"),
        ("invalid:external", "channel"),
    ] {
        session(&c, id, kind, "", "2026-10-24T12:00:00Z");
    }
    session(
        &c,
        "webchat:archived",
        "webchat_user",
        "me",
        "2026-10-24T12:00:00Z",
    );
    c.execute(
        "UPDATE sessions SET archived_at='2026-10-24' WHERE id='webchat:archived'",
        [],
    )
    .unwrap();
    assert!(list(&c, "me", "sales", 50).unwrap().is_empty());
    assert!(metadata(&c, "me", "webchat:archived").unwrap().is_none());
}
#[test]
fn rm173_archived_reader_shows_own_clears_but_never_someone_elses() {
    let c = db();
    session(&c, "webchat:mine#cleared:1", "webchat_user", "me", "2026-10-24T12:00:00Z");
    session(&c, "line:Ucustomer#cleared:2", "channel", "", "2026-10-24T12:00:01Z");
    session(&c, "line:Uidle", "channel", "", "2026-10-24T12:00:02Z");
    c.execute(
        "UPDATE sessions SET archived_at='2026-10-25', archive_reason=CASE WHEN id LIKE '%#cleared:%' THEN 'user' ELSE 'retention' END",
        [],
    )
    .unwrap();
    let ids: Vec<_> = list_in(&c, "me", "sales", 50, true)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect();
    assert_eq!(ids, ["line:Uidle", "webchat:mine#cleared:1"]);
    assert!(metadata_in(&c, "me", "line:Ucustomer#cleared:2", true).unwrap().is_none());
    assert!(metadata_in(&c, "me", "webchat:mine#cleared:1", true).unwrap().is_some());
    // Another account never sees my cleared conversation.
    assert!(list_in(&c, "you", "sales", 50, true).unwrap().iter().all(|m| m.id == "line:Uidle"));
}
#[test]
fn rm173_restored_clears_stay_out_of_the_restricted_reader_except_for_their_owner() {
    let c = db();
    session(&c, "webchat:mine#cleared:1", "webchat_user", "me", "2026-10-24T12:00:00Z");
    session(&c, "line:Ucustomer#cleared:2", "channel", "", "2026-10-24T12:00:01Z");
    session(&c, "line:Ulive", "channel", "", "2026-10-24T12:00:02Z");
    // Both clears were restored (live again) — the sticky mark stays.
    c.execute(
        "UPDATE sessions SET user_cleared_at='2026-10-25' WHERE id LIKE '%#cleared:%'",
        [],
    )
    .unwrap();
    let ids: Vec<_> = list(&c, "me", "sales", 50).unwrap().into_iter().map(|m| m.id).collect();
    assert_eq!(ids, ["line:Ulive", "webchat:mine#cleared:1"]);
    assert!(metadata(&c, "me", "line:Ucustomer#cleared:2").unwrap().is_none());
    assert!(metadata(&c, "me", "webchat:mine#cleared:1").unwrap().is_some());
}
#[test]
fn bound_history_rows_are_bounded_visible_text_and_keep_message_ids_for_masked_runs() {
    let c = db();
    session(&c, "line:x", "channel", "", "2026-10-24T12:00:00Z");
    for (i, role, hidden, undone) in [
        (1, "user", 0, None),
        (2, "assistant", 0, None),
        (3, "tool", 0, None),
        (4, "user", 1, None),
        (5, "user", 0, Some("2026-10-24")),
    ] {
        c.execute("INSERT INTO session_messages(id,session_id,role,content,tokens,timestamp,hidden,undone_at) VALUES (?1,'line:x',?2,'text',0,'2026-10-24T12:00:00Z',?3,?4)",params![i,role,hidden,undone]).unwrap();
    }
    let r = rows(&c, "line:x").unwrap();
    assert_eq!(r.iter().map(|r| r.id).collect::<Vec<_>>(), vec![1, 2]);
    c.execute(
        "UPDATE session_messages SET content=?1 WHERE id=1",
        ["x".repeat(MAX_BYTES + 1)],
    )
    .unwrap();
    assert!(rows(&c, "line:x").is_err());
}
