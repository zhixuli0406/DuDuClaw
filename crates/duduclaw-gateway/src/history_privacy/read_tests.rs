use super::*;
fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,agent_id TEXT,origin_kind TEXT,owner_user_id TEXT,title TEXT,last_active TEXT,archived_at TEXT);CREATE TABLE session_messages(id INTEGER PRIMARY KEY,session_id TEXT,role TEXT,content TEXT,tokens INTEGER,timestamp TEXT,hidden INTEGER DEFAULT 0,undone_at TEXT);").unwrap();
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
