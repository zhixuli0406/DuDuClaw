//! Upload and download permission tests (through axum's extractors).

use std::collections::HashMap;

use axum::Router;
use axum::routing::{get, post};
use duduclaw_auth::UserRole;

use super::*;

fn ctx(id: &str, role: UserRole) -> UserContext {
    UserContext {
        credential_epoch: 0,
        user_id: id.to_string(),
        email: format!("{id}@example.test"),
        role,
        agent_access: HashMap::new(),
        must_change_password: false,
        team_access: Default::default(),
        acl_generation: 0,
    }
}

fn app(home: std::path::PathBuf, who: UserContext) -> Router {
    let (h1, w1, h2, w2) = (home.clone(), who.clone(), home, who);
    Router::new()
        .route(
            "/up",
            post(move |mp: Multipart| {
                let (h, w) = (h1.clone(), w1.clone());
                async move { handle_upload(&h, &w, None, mp).await }
            }),
        )
        .route(
            "/dl/{id}",
            get(
                move |axum::extract::Path(id): axum::extract::Path<String>| {
                    let (h, w) = (h2.clone(), w2.clone());
                    async move { handle_original(&h, &w, None, &id).await }
                },
            ),
        )
}

fn multipart(collection: Option<&str>, files: &[(&str, &[u8])]) -> (String, Vec<u8>) {
    let b = "XKBOUNDARY";
    let mut body = Vec::new();
    if let Some(c) = collection {
        body.extend_from_slice(
            format!(
                "--{b}\r\nContent-Disposition: form-data; name=\"collection_id\"\r\n\r\n{c}\r\n"
            )
            .as_bytes(),
        );
    }
    for (name, bytes) in files {
        body.extend_from_slice(
            format!("--{b}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{b}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={b}"), body)
}

async fn send(
    router: Router,
    req: axum::http::Request<axum::body::Body>,
) -> (u16, Vec<u8>, axum::http::HeaderMap) {
    use tower::ServiceExt;
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let headers = resp.headers().clone();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap()
        .to_vec();
    (status, bytes, headers)
}

async fn upload(
    home: &std::path::Path,
    who: UserContext,
    c: Option<&str>,
    files: &[(&str, &[u8])],
) -> (u16, serde_json::Value) {
    let (ct, body) = multipart(c, files);
    let req = axum::http::Request::builder()
        .method("POST")
        .uri("/up")
        .header("content-type", ct)
        .body(axum::body::Body::from(body))
        .unwrap();
    let (s, b, _) = send(app(home.to_path_buf(), who), req).await;
    (
        s,
        serde_json::from_slice(&b).unwrap_or(serde_json::Value::Null),
    )
}

fn collection_with_editor(home: &std::path::Path) -> String {
    let mut conn = duduclaw_knowledge::schema::open_home(home).unwrap();
    let c = duduclaw_knowledge::store::create_collection(
        &mut conn,
        &duduclaw_knowledge::store::NewCollection {
            name: "HR".into(),
            ..Default::default()
        },
        "owner",
        &KnowledgeConfig::default(),
    )
    .unwrap();
    use duduclaw_knowledge::acl::PrincipalKind;
    let entries = vec![
        (PrincipalKind::User, "owner".to_string(), Role::Owner),
        (PrincipalKind::User, "editor".to_string(), Role::Editor),
        (PrincipalKind::User, "viewer".to_string(), Role::Viewer),
    ];
    duduclaw_knowledge::acl::set_acl(&mut conn, &c.id, &entries, 0, "owner").unwrap();
    c.id
}

#[tokio::test]
async fn upload_needs_editor_before_any_byte_is_stored() {
    let home = tempfile::tempdir().unwrap();
    let cid = collection_with_editor(home.path());
    let file: &[u8] = "員工請假需提前三日申請。".as_bytes();
    for who in ["viewer", "stranger"] {
        let (s, v) = upload(
            home.path(),
            ctx(who, UserRole::Employee),
            Some(&cid),
            &[("a.txt", file)],
        )
        .await;
        assert_eq!(
            (s, v["error"].as_str()),
            (403, Some("permission denied")),
            "{who}"
        );
    }
    assert!(
        !home.path().join("knowledge/blobs").exists(),
        "nothing stored for a refused caller"
    );
    let (s, _) = upload(
        home.path(),
        ctx("editor", UserRole::Employee),
        None,
        &[("a.txt", file)],
    )
    .await;
    assert_eq!(s, 400, "collection_id must come first");
    let (s, v) = upload(
        home.path(),
        ctx("editor", UserRole::Employee),
        Some(&cid),
        &[
            ("a.txt", file),
            ("a.txt", file),
            ("old.doc", b"x"),
            ("empty.txt", b""),
        ],
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let files = v["files"].as_array().unwrap();
    assert_eq!(files[0]["ok"], true);
    assert_eq!(files[1]["duplicate"], true);
    assert_eq!(files[2]["error_code"], "legacy_office");
    assert_eq!(files[3]["error_code"], "empty_text");
    // Admin is owner of everything.
    let (s, _) = upload(
        home.path(),
        ctx("root", UserRole::Admin),
        Some(&cid),
        &[("b.txt", "加班規定".as_bytes())],
    )
    .await;
    assert_eq!(s, 200);
}

#[tokio::test]
async fn download_follows_the_allowed_set() {
    let home = tempfile::tempdir().unwrap();
    let cid = collection_with_editor(home.path());
    let (_, v) = upload(
        home.path(),
        ctx("editor", UserRole::Employee),
        Some(&cid),
        &[("手冊.txt", "特休七日".as_bytes())],
    )
    .await;
    let sid = v["files"][0]["source_id"].as_str().unwrap().to_string();
    let get = |who: UserContext| {
        let req = axum::http::Request::builder()
            .uri(format!("/dl/{sid}"))
            .body(axum::body::Body::empty())
            .unwrap();
        send(app(home.path().to_path_buf(), who), req)
    };
    let (s, body, headers) = get(ctx("viewer", UserRole::Employee)).await;
    assert_eq!(s, 200);
    assert_eq!(body, "特休七日".as_bytes());
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert!(
        headers["content-disposition"]
            .to_str()
            .unwrap()
            .starts_with("attachment")
    );
    let (s, _, _) = get(ctx("stranger", UserRole::Employee)).await;
    assert_eq!(s, 403);
    let conn = duduclaw_knowledge::schema::open_home(home.path()).unwrap();
    let outcomes: Vec<String> = conn
        .prepare("SELECT outcome FROM access_log WHERE op = 'blob_download' ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(outcomes, vec!["ok".to_string(), "denied".to_string()]);
}
