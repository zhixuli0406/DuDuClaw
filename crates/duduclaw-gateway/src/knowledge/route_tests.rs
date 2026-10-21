//! The internal route's permission decisions (authentication itself is the
//! shared `computer_use_sessions::auth`, tested there).

use super::*;
use duduclaw_knowledge::acl::{PrincipalKind, Role, get_acl, set_acl};
use duduclaw_knowledge::extract::{Format, RawSegment, SegmentKind};
use duduclaw_knowledge::limits::KnowledgeConfig;
use duduclaw_knowledge::sources::{Added, NewSource, SourceKind, add_source};
use duduclaw_knowledge::store::{NewCollection, create_collection};

fn agent(home: &std::path::Path, id: &str, toml: &str) {
    let dir = home.join("agents").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("agent.toml"), toml).unwrap();
}

fn collection(
    home: &std::path::Path,
    slug: &str,
    sensitivity: &str,
    grants: &[(PrincipalKind, &str)],
    text: &str,
) -> i64 {
    let mut conn = schema::open_home(home).unwrap();
    let cfg = KnowledgeConfig::default();
    let c = create_collection(
        &mut conn,
        &NewCollection {
            name: slug.into(),
            slug: Some(slug.into()),
            sensitivity: Some(sensitivity.into()),
            ..Default::default()
        },
        "owner",
        &cfg,
    )
    .unwrap();
    let (_, rev) = get_acl(&conn, &c.id).unwrap();
    let mut entries = vec![(PrincipalKind::User, "owner".to_string(), Role::Owner)];
    entries.extend(grants.iter().map(|(k, i)| (*k, i.to_string(), Role::Viewer)));
    set_acl(&mut conn, &c.id, &entries, rev, "owner").unwrap();
    let new = NewSource {
        collection_id: c.id.clone(),
        kind: SourceKind::Paste,
        title: format!("{slug}.txt"),
        original_name: None,
        url: None,
        format: Format::Txt,
        blob_sha256: duduclaw_knowledge::sha256_hex(slug.as_bytes()),
        bytes: 1,
        origin: json!({}),
        created_by: "owner".into(),
    };
    let Added::New(s) = add_source(&mut conn, &new, &cfg).unwrap() else {
        panic!()
    };
    let prepared =
        duduclaw_knowledge::chunk::prepare(Format::Txt, vec![RawSegment::new(SegmentKind::Text, text.into())]);
    duduclaw_knowledge::index::write_index(&mut conn, &s.id, &prepared, &[], "t", &|_| false, &cfg).unwrap();
    conn.query_row("SELECT id FROM chunks WHERE source_id = ?1", [&s.id], |r| r.get(0))
        .unwrap()
}

fn system() -> RequesterIdentity {
    RequesterIdentity::System {
        kind: "test".into(),
        on_behalf_of: None,
    }
}

fn search(q: &str) -> Request {
    Request::Search {
        query: q.into(),
        collections: None,
        limit: None,
        turn_id: None,
    }
}

fn hits(v: &Value) -> usize {
    v["hits"].as_array().map(Vec::len).unwrap_or(0)
}

#[test]
fn employee_side_grants_and_narrowing() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "hr-bot", "[agent]\nname='hr-bot'\ndepartment='人資'\n");
    agent(h, "sales-bot", "[agent]\nname='sales-bot'\n");
    agent(h, "off-bot", "[agent]\nname='off-bot'\n[knowledge]\ncollections = []\n");
    agent(
        h,
        "broken-bot",
        "[agent]\nname='broken-bot'\n[knowledge]\ncollections = 'hr'\n",
    );
    let hr_chunk = collection(
        h,
        "hr",
        "internal",
        &[(PrincipalKind::Department, "人資")],
        "特休假依年資計算，滿一年給七日。",
    );
    collection(
        h,
        "faq",
        "public",
        &[(PrincipalKind::Agent, "sales-bot")],
        "退貨請在七日內辦理。",
    );
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);

    let v = serve(&st, "hr-bot", system(), search("特休假")).unwrap();
    assert_eq!(hits(&v), 1);
    let v = serve(&st, "sales-bot", system(), search("特休假")).unwrap();
    assert_eq!(hits(&v), 0, "sales-bot has no hr grant");
    let denied = serve(
        &st,
        "sales-bot",
        system(),
        Request::GetPassage {
            chunk_id: hr_chunk,
            context: None,
            turn_id: None,
        },
    );
    assert!(matches!(
        denied,
        Err(Refusal(StatusCode::FORBIDDEN, "permission_denied", _))
    ));
    let p = serve(
        &st,
        "hr-bot",
        system(),
        Request::GetPassage {
            chunk_id: hr_chunk,
            context: Some(2),
            turn_id: None,
        },
    )
    .unwrap();
    assert!(p["match"].as_str().unwrap().contains("七日"));

    for off in ["off-bot", "broken-bot", "missing-bot"] {
        assert!(
            matches!(
                serve(&st, off, system(), search("特休")),
                Err(Refusal(_, "knowledge_off", _))
            ),
            "{off}"
        );
    }
    let list = serve(&st, "sales-bot", system(), Request::ListCollections { turn_id: None }).unwrap();
    assert_eq!(list["collections"][0]["slug"], "faq");
    // Narrowing by tool argument only shrinks.
    let narrowed = Request::Search {
        query: "退貨".into(),
        collections: Some(vec!["hr".into()]),
        limit: None,
        turn_id: None,
    };
    assert_eq!(hits(&serve(&st, "sales-bot", system(), narrowed).unwrap()), 0);

    let conn = schema::open_home(h).unwrap();
    let logged: i64 = conn
        .query_row("SELECT COUNT(*) FROM access_log WHERE agent_id = 'hr-bot'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(logged >= 2);
    let denied_rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM access_log WHERE outcome = 'denied'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(denied_rows, 1);
}

#[test]
fn unidentified_channel_sender_sees_only_channel_grants() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "bot", "[agent]\nname='bot'\n");
    collection(
        h,
        "internal-kb",
        "internal",
        &[(PrincipalKind::Agent, "bot")],
        "內部報價底價為八折。",
    );
    collection(
        h,
        "public-faq",
        "public",
        &[(PrincipalKind::Agent, "bot"), (PrincipalKind::Channel, "line")],
        "營業時間為早上九點。",
    );
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    let stranger = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Cgroup".into(),
        sender: String::new(),
        user_id: None,
    };
    assert_eq!(hits(&serve(&st, "bot", stranger.clone(), search("底價")).unwrap()), 0);
    assert_eq!(hits(&serve(&st, "bot", stranger, search("營業時間")).unwrap()), 1);
    // A system turn (no person) sees internal collections granted to the employee.
    assert_eq!(hits(&serve(&st, "bot", system(), search("底價")).unwrap()), 1);
    // An unknown dashboard user serves nobody.
    let ghost = RequesterIdentity::DashboardUser {
        user_id: "ghost".into(),
    };
    assert_eq!(hits(&serve(&st, "bot", ghost, search("營業時間")).unwrap()), 0);
}
