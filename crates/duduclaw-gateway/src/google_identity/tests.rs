//! B2a: resolution order, fail-closed cases and the domain-wide-delegation
//! JWT exchange against a local mock Google (token endpoint + Gmail profile).
//! The protected key lives in the per-home in-memory store
//! (`credential_protection_rpc::test_store`); no keychain is touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use duduclaw_knowledge::requester::RequesterIdentity;
use serde_json::{Value, json};

use super::requester_gate::{NoPeople, People, Person};
use super::resolve::{Resolved, resolve_person};
use super::*;

// ── mock Google ─────────────────────────────────────────────────

/// One token request the mock saw (claims of the signed assertion).
#[derive(Debug, Clone)]
struct Seen {
    sub: String,
    scope: String,
    iss: String,
    grant_type: String,
}

fn seen() -> &'static Mutex<Vec<Seen>> {
    static S: OnceLock<Mutex<Vec<Seen>>> = OnceLock::new();
    S.get_or_init(|| Mutex::new(Vec::new()))
}

fn seen_for(sub: &str) -> Vec<Seen> {
    seen()
        .lock()
        .unwrap()
        .iter()
        .filter(|s| s.sub == sub)
        .cloned()
        .collect()
}

fn claims_of(assertion: &str) -> Value {
    use base64::Engine as _;
    let payload = assertion.split('.').nth(1).unwrap_or("");
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .unwrap_or_default();
    serde_json::from_slice(&bytes).unwrap_or(Value::Null)
}

async fn token_endpoint(
    axum::Form(form): axum::Form<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let c = claims_of(form.get("assertion").map(String::as_str).unwrap_or(""));
    let sub = c["sub"].as_str().unwrap_or("").to_string();
    seen().lock().unwrap().push(Seen {
        sub: sub.clone(),
        scope: c["scope"].as_str().unwrap_or("").to_string(),
        iss: c["iss"].as_str().unwrap_or("").to_string(),
        grant_type: form.get("grant_type").cloned().unwrap_or_default(),
    });
    let local = sub.split('@').next().unwrap_or("");
    if local.starts_with("unauth") {
        return (
            axum::http::StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "unauthorized_client", "error_description": "Client is unauthorized"})),
        )
            .into_response();
    }
    if local.starts_with("gone") {
        return (
            axum::http::StatusCode::BAD_REQUEST,
            axum::Json(
                json!({"error": "invalid_grant", "error_description": "Invalid email or User ID"}),
            ),
        )
            .into_response();
    }
    axum::Json(
        json!({"access_token": format!("tok:{sub}"), "expires_in": 3600, "token_type": "Bearer"}),
    )
    .into_response()
}

async fn gmail_profile(headers: axum::http::HeaderMap) -> axum::Json<Value> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer tok:"))
        .unwrap_or("")
        .to_string();
    let reported = if bearer.starts_with("mismatch") {
        "someone-else@c.test".to_string()
    } else {
        bearer
    };
    axum::Json(json!({"emailAddress": reported}))
}

async fn ok_json() -> axum::Json<Value> {
    axum::Json(json!({"items": []}))
}

/// Start the mock once per test process; returns its base URL.
fn mock() -> &'static str {
    static BASE: OnceLock<String> = OnceLock::new();
    BASE.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            rt.block_on(async move {
                let app = axum::Router::new()
                    .route("/token", axum::routing::post(token_endpoint))
                    .route(
                        "/gmail.googleapis.com/gmail/v1/users/me/profile",
                        axum::routing::get(gmail_profile),
                    )
                    .fallback(ok_json);
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                tx.send(format!("http://{}", listener.local_addr().unwrap()))
                    .unwrap();
                axum::serve(listener, app).await.unwrap();
            });
        });
        let base = rx.recv().unwrap();
        crate::google_workspace::test_api_base::set(base.clone());
        base
    })
}

/// A throwaway RSA key (generated once; never a real credential).
fn key_json(token_uri: &str) -> String {
    static PEM: OnceLock<String> = OnceLock::new();
    let pem = PEM.get_or_init(|| {
        use rsa::pkcs8::{EncodePrivateKey, LineEnding};
        let k = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        k.to_pkcs8_pem(LineEnding::LF).unwrap().to_string()
    });
    json!({
        "type": "service_account",
        "client_email": "duduclaw@proj.iam.gserviceaccount.test",
        "client_id": "1234567890",
        "private_key": pem,
        "token_uri": token_uri,
    })
    .to_string()
}

// ── fixtures ────────────────────────────────────────────────────

struct Home {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().canonicalize().unwrap();
        duduclaw_core::gateway_instance::acquire(&path).unwrap();
        Self { _dir: dir, path }
    }
    fn p(&self) -> &Path {
        &self.path
    }
    fn agent(&self, id: &str, google: &str) {
        let dir = self.path.join("agents").join(id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            format!("[agent]\nname='{id}'\n{google}"),
        )
        .unwrap();
    }
    fn domains(&self, list: &[&str]) {
        let items: Vec<String> = list.iter().map(|d| format!("'{d}'")).collect();
        std::fs::write(
            self.path.join("config.toml"),
            format!("[integrations]\ngoogle_workspace=true\n[integrations.google_identity]\nallowed_subject_domains=[{}]\n", items.join(",")),
        )
        .unwrap();
    }
    fn protect(&self) {
        crate::handlers::credential_protection_rpc::protection_for(&self.path)
            .enable(false)
            .unwrap();
    }
    fn import(&self) {
        let key = protection_gate(&self.path).unwrap();
        delegation_key::import(&self.path, &key, &key_json(&format!("{}/token", mock()))).unwrap();
    }
    /// Everything a working delegate needs.
    fn ready(&self, agent: &str, email: &str, extra: &str) {
        self.domains(&["c.test"]);
        self.protect();
        self.import();
        self.agent(
            agent,
            &format!("[google]\nidentity='delegate:{email}'\n{extra}"),
        );
    }
}

fn system() -> RequesterIdentity {
    RequesterIdentity::System {
        kind: "cron".into(),
        on_behalf_of: None,
    }
}

#[derive(Default)]
struct FakePeople {
    people: Vec<Person>,
    bindings: Vec<(String, String)>,
}

impl FakePeople {
    fn with(mut self, id: &str, email: &str, active: bool, admin: bool) -> Self {
        self.people.push(Person {
            user_id: id.into(),
            email: email.into(),
            active,
            admin,
        });
        self
    }
    fn bind(mut self, id: &str, agent: &str) -> Self {
        self.bindings.push((id.into(), agent.into()));
        self
    }
}

impl People for FakePeople {
    fn by_id(&self, id: &str) -> Option<Person> {
        self.people.iter().find(|p| p.user_id == id).cloned()
    }
    fn by_email(&self, email: &str) -> Result<Option<Person>, Refusal> {
        Ok(self
            .people
            .iter()
            .find(|p| p.email.eq_ignore_ascii_case(email))
            .cloned())
    }
    fn bound(&self, id: &str, agent: &str) -> bool {
        self.bindings.iter().any(|(u, a)| u == id && a == agent)
    }
}

async fn resolve(
    home: &Home,
    agent: &str,
    req: &RequesterIdentity,
    people: &dyn People,
) -> Result<Resolved, Refusal> {
    resolve_person(home.p(), agent, req, people).await
}

fn code(r: Result<Resolved, Refusal>) -> &'static str {
    match r {
        Err(r) => {
            assert!(
                REFUSAL_CODES.contains(&r.code),
                "{} not in the closed set",
                r.code
            );
            r.code
        }
        Ok(other) => panic!("expected a refusal, got {other:?}"),
    }
}

// ── tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn home_and_absent_sections_stay_on_the_home_path() {
    let home = Home::new();
    home.agent("plain", "");
    home.agent("explicit", "[google]\nidentity='home'\n");
    for a in ["plain", "explicit"] {
        assert!(matches!(
            resolve(&home, a, &RequesterIdentity::Unknown, &NoPeople).await,
            Ok(Resolved::UseHome {
                fallback: false,
                notice: None
            })
        ));
    }
}

#[tokio::test]
async fn delegate_mints_with_the_configured_subject_and_all_scopes() {
    let home = Home::new();
    home.ready("bot", "alice.l2@c.test", "requesters='anyone'\n");
    let r = resolve(&home, "bot", &RequesterIdentity::Unknown, &NoPeople)
        .await
        .unwrap();
    let Resolved::Delegated { email, token } = r else {
        panic!("{r:?}")
    };
    assert_eq!(email, "alice.l2@c.test");
    assert_eq!(token.access_token, "tok:alice.l2@c.test");
    let seen = seen_for("alice.l2@c.test");
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0].grant_type,
        "urn:ietf:params:oauth:grant-type:jwt-bearer"
    );
    assert_eq!(seen[0].iss, "duduclaw@proj.iam.gserviceaccount.test");
    assert_eq!(
        seen[0].scope,
        crate::google_workspace::REQUIRED_SCOPES.join(" ")
    );
}

#[tokio::test]
async fn two_employees_get_their_own_subjects() {
    let home = Home::new();
    home.ready("a1", "l3-a@c.test", "requesters='anyone'\n");
    home.agent(
        "a2",
        "[google]\nidentity='delegate:l3-b@c.test'\nrequesters='anyone'\n",
    );
    let one = resolve(&home, "a1", &system(), &NoPeople).await.unwrap();
    let two = resolve(&home, "a2", &system(), &NoPeople).await.unwrap();
    let tok = |r: Resolved| match r {
        Resolved::Delegated { token, .. } => token.access_token,
        other => panic!("{other:?}"),
    };
    assert_eq!(tok(one), "tok:l3-a@c.test");
    assert_eq!(tok(two), "tok:l3-b@c.test");
}

#[tokio::test]
async fn domain_outside_the_list_is_a_config_error_without_fallback_or_network() {
    let home = Home::new();
    home.ready(
        "bot",
        "l4@other.test",
        "fallback='home'\nrequesters='anyone'\n",
    );
    assert_eq!(
        code(resolve(&home, "bot", &system(), &NoPeople).await),
        "config_invalid"
    );
    assert!(
        seen_for("l4@other.test").is_empty(),
        "no token request for a refused domain"
    );
}

#[tokio::test]
async fn google_rejections_are_explained_and_fall_back_only_when_allowed() {
    let home = Home::new();
    home.ready("bot", "unauth-l5@c.test", "requesters='anyone'\n");
    let r = resolve(&home, "bot", &system(), &NoPeople)
        .await
        .unwrap_err();
    assert_eq!(r.code, "delegate_rejected");
    assert!(
        r.message_zh.contains("1234567890"),
        "names the client id: {}",
        r.message_zh
    );
    home.agent(
        "gone",
        "[google]\nidentity='delegate:gone-l5@c.test'\nfallback='home'\nrequesters='anyone'\n",
    );
    match resolve(&home, "gone", &system(), &NoPeople).await.unwrap() {
        Resolved::UseHome {
            fallback: true,
            notice: Some(n),
        } => assert!(n.contains("已改用公司共用帳號"), "{n}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn inactive_colleague_is_refused_and_may_fall_back() {
    let home = Home::new();
    home.ready("bot", "l6@c.test", "requesters='anyone'\n");
    let people = FakePeople::default().with("u6", "L6@c.test", false, false);
    assert_eq!(
        code(resolve(&home, "bot", &system(), &people).await),
        "person_inactive"
    );
    home.agent(
        "bot2",
        "[google]\nidentity='delegate:l6@c.test'\nfallback='home'\nrequesters='anyone'\n",
    );
    assert!(matches!(
        resolve(&home, "bot2", &system(), &people).await,
        Ok(Resolved::UseHome { fallback: true, .. })
    ));
    assert!(seen_for("l6@c.test").is_empty());
}

#[tokio::test]
async fn unreadable_people_directory_is_refused_without_fallback() {
    let home = Home::new();
    home.ready(
        "bot",
        "unreadable-directory@c.test",
        "fallback='home'\nrequesters='anyone'\n",
    );
    std::fs::write(home.p().join("users.db"), "not a sqlite database").unwrap();
    let people = requester_gate::people_for(home.p(), None);
    let refusal = resolve(&home, "bot", &system(), people.as_ref())
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "config_invalid");
    assert!(!refusal.fallback_eligible);
    assert!(seen_for("unreadable-directory@c.test").is_empty());
}

#[tokio::test]
async fn token_route_honors_master_switch_before_minting() {
    let home = Home::new();
    home.ready("bot", "master-switch@c.test", "requesters='anyone'\n");
    crate::google_workspace::set_integration_enabled(home.p(), false).unwrap();
    let state = route::GoogleTokenRouteState::new(home.p().to_path_buf(), None);
    let refusal = route::serve(&state, "bot", "gmail_search", &system())
        .await
        .unwrap_err();
    assert_eq!(refusal.code, "disabled");
    assert!(seen_for("master-switch@c.test").is_empty());
}

#[tokio::test]
async fn requester_refusal_never_falls_back() {
    let home = Home::new();
    home.ready("bot", "l14@c.test", "fallback='home'\n");
    let unbound = RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "g1".into(),
        sender: "x".into(),
        user_id: None,
    };
    assert_eq!(
        code(resolve(&home, "bot", &unbound, &NoPeople).await),
        "requester_refused"
    );
    assert_eq!(
        code(resolve(&home, "bot", &RequesterIdentity::Unknown, &NoPeople).await),
        "requester_refused"
    );
    // A system turn on behalf of nobody (cron) is allowed by default.
    assert!(matches!(
        resolve(&home, "bot", &system(), &NoPeople).await,
        Ok(Resolved::Delegated { .. })
    ));
}

#[tokio::test]
async fn public_visitors_are_refused_even_for_anyone() {
    let home = Home::new();
    home.ready("bot", "l15@c.test", "requesters='anyone'\n");
    for channel in ["widget", "odoolivechat"] {
        let v = RequesterIdentity::ChannelUser {
            channel: channel.into(),
            chat: "v".into(),
            sender: String::new(),
            user_id: None,
        };
        assert_eq!(
            code(resolve(&home, "bot", &v, &NoPeople).await),
            "requester_refused"
        );
    }
}

#[tokio::test]
async fn protection_below_the_minimum_refuses() {
    let home = Home::new();
    home.domains(&["c.test"]);
    home.agent(
        "bot",
        "[google]\nidentity='delegate:p@c.test'\nfallback='home'\nrequesters='anyone'\n",
    );
    let r = resolve(&home, "bot", &system(), &NoPeople)
        .await
        .unwrap_err();
    assert_eq!(r.code, "credential_protection");
    assert!(!r.fallback_eligible, "protection failures never fall back");
    assert!(r.message_zh.contains("安全"), "{}", r.message_zh);
}

#[test]
fn platform_without_a_keychain_refuses_person_identities() {
    let home = Home::new();
    let cp = duduclaw_security::credential_protection::CredentialProtection::with_store(
        home.p(),
        Box::new(duduclaw_security::credential_protection::UnsupportedStore),
    );
    assert_eq!(gate_with(&cp).unwrap_err().code, "credential_protection");
    let _ = cp.enable(false).unwrap_err();
    assert_eq!(gate_with(&cp).unwrap_err().code, "credential_protection");
}

#[tokio::test]
async fn missing_sealed_key_is_unavailable_and_may_fall_back() {
    let home = Home::new();
    home.domains(&["c.test"]);
    home.protect();
    home.agent(
        "bot",
        "[google]\nidentity='delegate:k@c.test'\nrequesters='anyone'\n",
    );
    assert_eq!(
        code(resolve(&home, "bot", &system(), &NoPeople).await),
        "delegate_unavailable"
    );
}

#[tokio::test]
async fn sealed_key_from_a_replaced_protection_key_fails_closed_and_is_audited() {
    let home = Home::new();
    home.ready("bot", "kc@c.test", "fallback='home'\nrequesters='anyone'\n");
    // Replace the protected key (an admin reset, or a same-user attacker).
    crate::handlers::credential_protection_rpc::protection_for(home.p())
        .enable(true)
        .unwrap();
    let r = resolve(&home, "bot", &system(), &NoPeople)
        .await
        .unwrap_err();
    assert_eq!(r.code, "credential_protection");
    assert!(!r.fallback_eligible);
    assert!(r.message_zh.contains("重新匯入"), "{}", r.message_zh);
    let audit = std::fs::read_to_string(home.p().join("security_audit.jsonl")).unwrap_or_default();
    assert!(
        audit.contains("google_delegation_key_unreadable"),
        "{audit}"
    );
    assert!(
        delegation_key::path(home.p()).exists(),
        "the old record is not deleted silently"
    );
}

#[tokio::test]
async fn disabled_invalid_and_b2b_selectors_are_refused() {
    let home = Home::new();
    home.agent("off", "[google]\nidentity='none'\nfallback='home'\n");
    home.agent(
        "bad",
        "[google]\nidentity='delegate:nope'\nfallback='home'\n",
    );
    home.agent("own", "[google]\nidentity='owner'\nfallback='home'\n");
    assert_eq!(
        code(resolve(&home, "off", &system(), &NoPeople).await),
        "disabled"
    );
    assert_eq!(
        code(resolve(&home, "bad", &system(), &NoPeople).await),
        "config_invalid"
    );
    assert_eq!(
        code(resolve(&home, "own", &system(), &NoPeople).await),
        "not_available"
    );
}

#[test]
fn requester_gate_matrix() {
    use requester_gate::check;
    let people = FakePeople::default()
        .with("bound", "b@c.test", true, false)
        .with("admin", "a@c.test", true, true)
        .with("stranger", "s@c.test", true, false)
        .with("left", "l@c.test", false, false)
        .with("alice", "alice@c.test", true, false)
        .bind("bound", "bot")
        .bind("left", "bot")
        .bind("alice", "bot");
    let dash = |u: &str| RequesterIdentity::DashboardUser { user_id: u.into() };
    let chan = |u: Option<&str>| RequesterIdentity::ChannelUser {
        channel: "line".into(),
        chat: "c".into(),
        sender: "s".into(),
        user_id: u.map(str::to_string),
    };
    let on_behalf = |u: &str| RequesterIdentity::System {
        kind: "dispatch".into(),
        on_behalf_of: Some(u.into()),
    };
    let ok = |r: Requesters, req: &RequesterIdentity| {
        check(r, req, "bot", "alice@c.test", &people).is_ok()
    };
    let bp = Requesters::BoundPeople;
    assert!(ok(bp, &dash("bound")));
    assert!(ok(bp, &dash("admin")));
    assert!(ok(bp, &chan(Some("bound"))));
    assert!(ok(bp, &on_behalf("bound")));
    assert!(ok(bp, &system()));
    assert!(!ok(bp, &dash("stranger")));
    assert!(!ok(bp, &dash("left")), "a suspended person stops counting");
    assert!(!ok(bp, &on_behalf("stranger")), "delegation cannot widen");
    assert!(!ok(bp, &chan(None)));
    assert!(!ok(bp, &RequesterIdentity::Unknown));
    let oo = Requesters::OwnerOnly;
    assert!(ok(oo, &dash("alice")));
    assert!(ok(oo, &system()));
    assert!(!ok(oo, &dash("bound")));
    assert!(!ok(oo, &dash("admin")));
    let any = Requesters::Anyone;
    assert!(ok(any, &RequesterIdentity::Unknown));
    assert!(ok(any, &chan(None)));
}

#[tokio::test]
async fn route_answers_carry_the_token_but_audit_rows_never_do() {
    let home = Home::new();
    home.ready("bot", "route@c.test", "requesters='anyone'\n");
    let state = route::GoogleTokenRouteState::new(home.p().to_path_buf(), None);
    let v = route::serve(&state, "bot", "gmail_search", &RequesterIdentity::Unknown)
        .await
        .unwrap();
    assert_eq!(v["mode"], "delegate");
    assert_eq!(v["access_token"], "tok:route@c.test");
    assert_eq!(v["account_email"], "route@c.test");
    home.agent("lock", "[google]\nidentity='delegate:route@c.test'\n");
    let refused = route::serve(&state, "lock", "gmail_read", &RequesterIdentity::Unknown)
        .await
        .unwrap_err();
    assert_eq!(refused.code, "requester_refused");
    let audit = std::fs::read_to_string(home.p().join("tool_calls.jsonl")).unwrap();
    assert!(!audit.contains("tok:"), "no token in the audit: {audit}");
    assert!(!audit.contains("PRIVATE KEY"));
    let rows: Vec<Value> = audit
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();
    let issued = rows.iter().find(|r| r["agent_id"] == "bot").unwrap();
    assert_eq!(issued["tool_name"], "google_token");
    assert_eq!(issued["google_identity"], "delegate");
    assert_eq!(issued["google_account"], "route@c.test");
    assert_eq!(issued["google_tool"], "gmail_search");
    assert_eq!(issued["requester_kind"], "unknown");
    let denied = rows.iter().find(|r| r["agent_id"] == "lock").unwrap();
    assert_eq!(denied["success"], false);
    assert_eq!(denied["reason"], "requester_refused");
}

#[tokio::test]
async fn sealed_record_holds_no_readable_key() {
    let home = Home::new();
    home.protect();
    home.import();
    let text = std::fs::read_to_string(delegation_key::path(home.p())).unwrap();
    assert!(!text.contains("PRIVATE KEY"), "private key must be sealed");
    assert!(text.contains("pk:v1:"));
    let info = delegation_key::info(home.p()).unwrap().unwrap();
    assert_eq!(info.client_email, "duduclaw@proj.iam.gserviceaccount.test");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(delegation_key::path(home.p()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dmode = std::fs::metadata(home.p().join("protected"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(dmode & 0o777, 0o700);
    }
}

#[tokio::test]
async fn import_refuses_non_keys_and_plain_http_token_uris() {
    let home = Home::new();
    home.protect();
    let key = protection_gate(home.p()).unwrap();
    assert!(delegation_key::import(home.p(), &key, r#"{"installed":{"client_id":"x"}}"#).is_err());
    let evil = key_json("http://evil.example/token");
    assert!(delegation_key::import(home.p(), &key, &evil).is_err());
    assert!(!delegation_key::path(home.p()).exists());
}

#[tokio::test]
async fn identity_test_reports_each_check_and_sends_nothing() {
    let home = Home::new();
    home.ready("bot", "check@c.test", "");
    let (email, steps) = check::run(home.p(), "bot", &NoPeople).await;
    assert_eq!(email.as_deref(), Some("check@c.test"));
    assert!(steps.iter().all(|s| s.ok), "{steps:?}");
    let ids: Vec<&str> = steps.iter().map(|s| s.id).collect();
    assert!(ids.contains(&"identity_match") && ids.contains(&"gmail") && ids.contains(&"tasks"));
    // Full list once, then one mint per service (8) — all fresh (cache bypassed).
    assert_eq!(seen_for("check@c.test").len(), 9);

    home.agent("mm", "[google]\nidentity='delegate:mismatch@c.test'\n");
    let (_, steps) = check::run(home.p(), "mm", &NoPeople).await;
    let last = steps.last().unwrap();
    assert_eq!(last.id, "identity_match");
    assert!(!last.ok);

    home.agent("ua", "[google]\nidentity='delegate:unauth-check@c.test'\n");
    let (_, steps) = check::run(home.p(), "ua", &NoPeople).await;
    assert_eq!(steps.last().unwrap().id, "authorize");
    assert!(!steps.last().unwrap().ok);
}

#[tokio::test]
async fn gateway_backend_for_agent_mail() {
    let home = Home::new();
    home.ready("mailbot", "mail@c.test", "");
    match gateway_backend(home.p(), Some("mailbot"), "mail_poll").await {
        Ok(crate::google_workspace::GoogleBackend::Direct(t)) => assert_eq!(t, "tok:mail@c.test"),
        other => panic!("{:?}", other.err()),
    }
    home.agent("off", "[google]\nidentity='none'\n");
    assert!(matches!(
        gateway_backend(home.p(), Some("off"), "mail_poll").await,
        Err(GatewayBackendError::Refused(Refusal {
            code: "disabled",
            ..
        }))
    ));
    assert_eq!(
        delegate_email_of(home.p(), Some("mailbot")).as_deref(),
        Some("mail@c.test")
    );
    assert_eq!(delegate_email_of(home.p(), Some("off")), None);
    assert_eq!(delegate_email_of(home.p(), None), None);
}
