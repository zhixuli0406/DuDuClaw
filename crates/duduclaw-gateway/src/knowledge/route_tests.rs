//! The internal route's permission decisions (authentication itself is the
//! shared `computer_use_sessions::auth`, tested there).

use super::*;

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
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
    entries.extend(
        grants
            .iter()
            .map(|(k, i)| (*k, i.to_string(), Role::Viewer)),
    );
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
    let prepared = duduclaw_knowledge::chunk::prepare(
        Format::Txt,
        vec![RawSegment::new(SegmentKind::Text, text.into())],
    );
    duduclaw_knowledge::index::write_index(&mut conn, &s.id, &prepared, &[], "t", &|_| false, &cfg)
        .unwrap();
    conn.query_row("SELECT id FROM chunks WHERE source_id = ?1", [&s.id], |r| {
        r.get(0)
    })
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
        run_id: None,
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
    agent(
        h,
        "off-bot",
        "[agent]\nname='off-bot'\n[knowledge]\ncollections = []\n",
    );
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
            run_id: None,
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
            run_id: None,
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
    let list = serve(
        &st,
        "sales-bot",
        system(),
        Request::ListCollections { turn_id: None, run_id: None },
    )
    .unwrap();
    assert_eq!(list["collections"][0]["slug"], "faq");
    // Narrowing by tool argument only shrinks.
    let narrowed = Request::Search {
        query: "退貨".into(),
        collections: Some(vec!["hr".into()]),
        limit: None,
        turn_id: None,
        run_id: None,
    };
    assert_eq!(
        hits(&serve(&st, "sales-bot", system(), narrowed).unwrap()),
        0
    );

    let conn = schema::open_home(h).unwrap();
    let logged: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_log WHERE agent_id = 'hr-bot'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(logged >= 2);
    let denied_rows: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM access_log WHERE outcome = 'denied'",
            [],
            |r| r.get(0),
        )
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
        &[
            (PrincipalKind::Agent, "bot"),
            (PrincipalKind::Channel, "line"),
        ],
        "營業時間為早上九點。",
    );
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    let stranger = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "Cgroup".into(),
        sender: String::new(),
        user_id: None,
    };
    assert_eq!(
        hits(&serve(&st, "bot", stranger.clone(), search("底價")).unwrap()),
        0
    );
    assert_eq!(
        hits(&serve(&st, "bot", stranger, search("營業時間")).unwrap()),
        1
    );
    // A system turn (no person) sees internal collections granted to the employee.
    assert_eq!(
        hits(&serve(&st, "bot", system(), search("底價")).unwrap()),
        1
    );
    // An unknown dashboard user serves nobody.
    let ghost = RequesterIdentity::DashboardUser {
        user_id: "ghost".into(),
    };
    assert_eq!(
        hits(&serve(&st, "bot", ghost, search("營業時間")).unwrap()),
        0
    );
}

#[test]
fn missing_turn_id_in_a_channel_group_cannot_read_internal() {
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
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    // The MCP process of a LINE group turn sent no (or an unknown) turn id.
    for turn in [None, Some("forged-turn"), Some("bad\nid")] {
        let who = super::super::turn_requesters::resolve("bot", turn);
        assert_eq!(who, RequesterIdentity::Unknown);
        assert_eq!(
            hits(&serve(&st, "bot", who, search("內部報價底價")).unwrap()),
            0,
            "{turn:?}"
        );
    }
    // A live LINE group turn the gateway registered: channel grants only.
    let _g =
        crate::computer_use_sessions::turns::register("bot", "turn-g", "line:Cgroup9").unwrap();
    let who = super::super::turn_requesters::resolve("bot", Some("turn-g"));
    assert_eq!(
        hits(&serve(&st, "bot", who, search("內部報價底價")).unwrap()),
        0
    );
}

#[test]
fn no_collection_on_the_employee_side_means_no_tools() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "lonely", "[agent]\nname='lonely'\n");
    agent(
        h,
        "narrowed",
        "[agent]\nname='narrowed'\n[knowledge]\ncollections = ['other']\n",
    );
    collection(
        h,
        "kb",
        "public",
        &[
            (PrincipalKind::Agent, "narrowed"),
            (PrincipalKind::Channel, "line"),
        ],
        "公開資料。",
    );
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    for who in ["lonely", "narrowed"] {
        assert!(
            matches!(
                serve(
                    &st,
                    who,
                    system(),
                    Request::ListCollections { turn_id: None, run_id: None }
                ),
                Err(Refusal(_, "knowledge_off", _))
            ),
            "{who}"
        );
    }
}

// ── HTTP: authentication, replay, rate limit, switch, turn id ────────────

const INTERNAL_KEY: &str = "ddc_prod_0123456789abcdef0123456789abcdef";

fn http_home(extra: &str) -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    let now = chrono::Utc::now().to_rfc3339();
    std::fs::write(
        tmp.path().join("config.toml"),
        format!(
            "{extra}\n[mcp_keys.\"{INTERNAL_KEY}\"]\nclient_id = \"gateway-internal\"\nis_external = false\n\
             created_at = \"{now}\"\nscopes = [\"admin\"]\n"
        ),
    )
    .unwrap();
    duduclaw_core::ensure_identity_key(tmp.path()).unwrap();
    agent(tmp.path(), "bot", "[agent]\nname='bot'\n");
    collection(
        tmp.path(),
        "faq",
        "public",
        &[
            (PrincipalKind::Agent, "bot"),
            (PrincipalKind::Channel, "line"),
        ],
        "營業時間為早上九點。",
    );
    tmp
}

fn signed(home: &std::path::Path, agent: &str, body: &[u8], nonce: &str, key: &str) -> HeaderMap {
    let tok =
        duduclaw_core::mint_identity_token(&duduclaw_core::load_identity_key(home).unwrap(), agent);
    let ts = unix_now().to_string();
    let sig =
        duduclaw_core::internal_request_signature(key.as_bytes(), agent, &tok, &ts, nonce, body);
    let mut h = HeaderMap::new();
    use crate::computer_use_sessions::auth as a;
    h.insert(a::AGENT_ID_HEADER, agent.parse().unwrap());
    h.insert(a::TIMESTAMP_HEADER, ts.parse().unwrap());
    h.insert(a::NONCE_HEADER, nonce.parse().unwrap());
    h.insert(a::SIGNATURE_HEADER, sig.parse().unwrap());
    h
}

async fn post(router: Router, peer: &str, hdrs: HeaderMap, body: Vec<u8>) -> (u16, Value) {
    use tower::ServiceExt;
    let mut req = axum::http::Request::builder()
        .method("POST")
        .uri(ROUTE)
        .header("content-type", "application/json")
        .body(axum::body::Body::from(body))
        .unwrap();
    req.headers_mut().extend(hdrs);
    req.extensions_mut()
        .insert(ConnectInfo::<SocketAddr>(peer.parse().unwrap()));
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn nonce() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

#[tokio::test]
async fn http_route_authenticates_and_applies_the_turn() {
    let home = http_home("");
    let state = Arc::new(KnowledgeRouteState::new(home.path().to_path_buf(), None));
    let body = serde_json::to_vec(&json!({"op": "search", "query": "營業時間"})).unwrap();
    let ok_peer = "127.0.0.1:50000";

    // Unsigned, remote peer, wrong key: one uniform refusal.
    let (s, _) = post(
        router(state.clone()),
        ok_peer,
        HeaderMap::new(),
        body.clone(),
    )
    .await;
    assert_eq!(s, 401);
    let n = nonce();
    let (s, _) = post(
        router(state.clone()),
        "10.0.0.5:5000",
        signed(home.path(), "bot", &body, &n, INTERNAL_KEY),
        body.clone(),
    )
    .await;
    assert_eq!(s, 401);
    let bad = "ddc_prod_ffffffffffffffffffffffffffffffff";
    let (s, _) = post(
        router(state.clone()),
        ok_peer,
        signed(home.path(), "bot", &body, &nonce(), bad),
        body.clone(),
    )
    .await;
    assert_eq!(s, 401);

    // Signed, no turn id: the request serves nobody, so nothing is found.
    let n = nonce();
    let hdrs = signed(home.path(), "bot", &body, &n, INTERNAL_KEY);
    let (s, v) = post(router(state.clone()), ok_peer, hdrs.clone(), body.clone()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["hits"].as_array().unwrap().len(), 0);
    // Replay of the same nonce is refused.
    let (s, _) = post(router(state.clone()), ok_peer, hdrs, body.clone()).await;
    assert_eq!(s, 401);

    // A live LINE turn of this employee: the public FAQ granted to `line` is readable.
    let _turn =
        crate::computer_use_sessions::turns::register("bot", "turn-http", "line:Uabc").unwrap();
    let body =
        serde_json::to_vec(&json!({"op": "search", "query": "營業時間", "turn_id": "turn-http"}))
            .unwrap();
    let (s, v) = post(
        router(state.clone()),
        ok_peer,
        signed(home.path(), "bot", &body, &nonce(), INTERNAL_KEY),
        body,
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["hits"].as_array().unwrap().len(), 1);
    let conn = schema::open_home(home.path()).unwrap();
    let kinds: Vec<String> = conn
        .prepare("SELECT requester_kind FROM access_log ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        kinds,
        vec!["unknown".to_string(), "channel_unbound".to_string()]
    );
}

#[tokio::test]
async fn http_route_rate_limit_and_switch() {
    let home = http_home("");
    let state = Arc::new(KnowledgeRouteState::new(home.path().to_path_buf(), None));
    let body = serde_json::to_vec(&json!({"op": "list_collections"})).unwrap();
    let mut last = 0;
    for _ in 0..=crate::computer_use_sessions::auth::RATE_LIMIT_PER_MINUTE {
        last = post(
            router(state.clone()),
            "127.0.0.1:1",
            signed(home.path(), "bot", &body, &nonce(), INTERNAL_KEY),
            body.clone(),
        )
        .await
        .0;
    }
    assert_eq!(last, 429);

    let off = http_home("[knowledge]\nenabled = false\n");
    let state = Arc::new(KnowledgeRouteState::new(off.path().to_path_buf(), None));
    let (s, v) = post(
        router(state),
        "127.0.0.1:1",
        signed(off.path(), "bot", &body, &nonce(), INTERNAL_KEY),
        body,
    )
    .await;
    assert_eq!((s, v["code"].as_str()), (403, Some("knowledge_disabled")));
}

#[test]
fn search_inside_a_live_turn_shares_the_ledger_numbering() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "ledger-bot", "[agent]\nname='ledger-bot'\n");
    collection(
        h,
        "faq",
        "internal",
        &[(PrincipalKind::Agent, "ledger-bot")],
        "退貨請在七日內辦理。",
    );
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    let _ledger = super::super::ledger::open("ledger-bot", "turn-ld").unwrap();
    let req = |q: &str| Request::Search {
        query: q.into(),
        collections: None,
        limit: None,
        turn_id: Some("turn-ld".into()),
        run_id: None,
    };
    let v = serve(&st, "ledger-bot", system(), req("退貨")).unwrap();
    assert_eq!(v["hits"][0]["n"], 1);
    let again = serve(&st, "ledger-bot", system(), req("七日內辦理")).unwrap();
    assert_eq!(again["hits"][0]["n"], 1, "same chunk keeps its number");
    assert_eq!(super::super::ledger::entries("ledger-bot", "turn-ld").len(), 1);
}

/// One fixed axis: every text maps to the same unit vector, so the vector
/// list always has a candidate. Enough to see what the tool output says.
struct OneAxis;

impl duduclaw_knowledge::embed::Embedder for OneAxis {
    fn model_id(&self) -> &str {
        "test/one@2"
    }
    fn dim(&self) -> usize {
        2
    }
    fn max_input_tokens(&self) -> usize {
        8
    }
    fn embed_passages(&self, t: &[&str]) -> Result<Vec<Vec<f32>>, duduclaw_knowledge::embed::EmbedError> {
        Ok(t.iter().map(|_| vec![1.0, 0.0]).collect())
    }
    fn embed_query(&self, _: &str) -> Result<Vec<f32>, duduclaw_knowledge::embed::EmbedError> {
        Ok(vec![1.0, 0.0])
    }
    fn egress(&self) -> duduclaw_knowledge::embed::Egress {
        duduclaw_knowledge::embed::Egress::Local
    }
}

#[test]
fn tool_output_reports_hybrid_mode_and_coverage_with_vectors() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "hr-bot", "[agent]\nname='hr-bot'\n");
    let chunk = collection(h, "hr", "internal", &[(PrincipalKind::Agent, "hr-bot")], "新進員工第一年特休為七日。");
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    // Without vectors: keyword-only, with the reason in the output.
    let e: std::sync::Arc<dyn duduclaw_knowledge::embed::Embedder> = std::sync::Arc::new(OneAxis);
    let v = serve_with(&st, "hr-bot", system(), search("放假"), Some(e.clone())).unwrap();
    assert_eq!(v["mode"], "keyword_only", "{v}");
    assert!(v["vector_unavailable"].as_str().is_some_and(|r| r.contains("向量尚未建立")), "{v}");
    {
        let mut conn = schema::open_home(h).unwrap();
        duduclaw_knowledge::embed::vectors::store(&mut conn, "test/one@2", &[(chunk, vec![1.0, 0.0])]).unwrap();
    }
    // A paraphrase with no shared term comes back through the vector list.
    let v = serve_with(&st, "hr-bot", system(), search("可以放幾天"), Some(e.clone())).unwrap();
    assert_eq!(v["mode"], "hybrid", "{v}");
    assert_eq!(v["coverage"], 1.0);
    assert_eq!(v["hits"][0]["vec_rank"], 1, "{v}");
    let list = serve_with(&st, "hr-bot", system(), Request::ListCollections { turn_id: None, run_id: None }, Some(e)).unwrap();
    assert_eq!(list["mode"], "hybrid");
    assert_eq!(list["collections"][0]["vector_coverage"], 1.0);
}

#[test]
fn a_run_delegated_from_an_unbound_sender_does_not_see_internal_collections() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    agent(h, "b-bot", "[agent]\nname='b-bot'\n");
    collection(
        h,
        "b-internal",
        "internal",
        &[(PrincipalKind::Agent, "b-bot")],
        "內部價格表：成本六成。",
    );
    super::super::turn_requesters::remember_turn_origin(
        "line:Ustranger#t-route-del",
        RequesterIdentity::ChannelUser {
            channel: "line".into(),
            chat: "Ustranger".into(),
            sender: "Ustranger".into(),
            user_id: None,
        },
    );
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let _g = rt
        .block_on(duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
            Some("line:Ustranger#t-route-del".to_string()),
            async { super::super::turn_requesters::register_run("b-bot", "runroutedel1", "dispatch") },
        ))
        .unwrap();
    let who = super::super::turn_requesters::resolve_with_run("b-bot", None, Some("runroutedel1"));
    let st = KnowledgeRouteState::new(h.to_path_buf(), None);
    let v = serve(&st, "b-bot", who, search("內部價格表")).unwrap();
    assert_eq!(hits(&v), 0, "{v}");
    // The same employee in a cron run (no person) does see it.
    let v = serve(&st, "b-bot", system(), search("內部價格表")).unwrap();
    assert_eq!(hits(&v), 1);
}
