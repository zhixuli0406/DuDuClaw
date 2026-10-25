//! SOURCE-ONLY this phase: real in-memory Router/auth/metadata tests. No
//! listener, provider, protected key, token unseal, refresh or browser.
use super::*;
use axum::{Router, body::Body, extract::ConnectInfo, http::Request};
use duduclaw_auth::{AccessLevel, UserDb, UserRole};
use duduclaw_core::google_identity::{CredentialKind, GoogleSelectionRequest};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tower::ServiceExt;

const INTERNAL: &str = "ddc_dev_11111111111111111111111111111111";
const DEDICATED: &str = "ddc_dev_22222222222222222222222222222222";
const AGENT: &str = "unit-personal-selection";
struct Fixture {
    _dir: tempfile::TempDir,
    state: Arc<ExecuteState>,
    token_state: Arc<crate::google_identity::route::GoogleTokenRouteState>,
    db: Arc<UserDb>,
    user: String,
}
impl Fixture {
    fn new(verified_edition: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(home.join("agents").join(AGENT)).unwrap();
        std::fs::write(
            home.join("agents").join(AGENT).join("agent.toml"),
            format!("[agent]\nname='{AGENT}'\n[google]\nidentity='owner'\nrequesters='anyone'\n"),
        )
        .unwrap();
        std::fs::write(home.join("config.toml"),format!("[integrations]\ngoogle_workspace=true\n[mcp_keys.{INTERNAL}]\nclient_id='gateway-internal'\nscopes=['admin']\nis_external=false\ncreated_at='{}'\n[mcp_keys.{DEDICATED}]\nclient_id='{AGENT}'\nscopes=['google:read','google:write']\nis_external=true\ncreated_at='{}'\n",chrono::Utc::now().to_rfc3339(),chrono::Utc::now().to_rfc3339())).unwrap();
        let db = Arc::new(UserDb::new(&home.join("users.db")).unwrap());
        let user = db
            .create_user(
                "login@unit.test",
                "Alice",
                "fixture-password-only",
                UserRole::Employee,
            )
            .unwrap();
        db.bind_agent(&user.id, AGENT, AccessLevel::Owner).unwrap();
        db.set_google_consent(&user.id, AGENT, true, &user.id)
            .unwrap();
        std::fs::create_dir(home.join("person_credentials")).unwrap();
        std::fs::write(home.join("person_credentials/store.json"),serde_json::to_vec(&json!({"version":1,"epochs":{(user.id.clone()):1},"records":[{
            "owner_kind":"user","owner_id":user.id,"provider":"google","credential_id":uuid::Uuid::new_v4().to_string(),"revision":1,
            "account_email":"alice-google@unit.test","google_sub":"unit-alice-sub","hosted_domain":null,
            "granted_scopes":["https://www.googleapis.com/auth/gmail.readonly"],"status":"connected","connected_at":chrono::Utc::now().to_rfc3339(),
            "last_refresh_at":null,"oauth_client_id":"unit-client","secret":"pk:v1:unusable-no-key-exists"}]})).unwrap()).unwrap();
        std::fs::write(home.join("mcp-oauth-configs.json"),serde_json::to_vec(&json!([{"provider_id":"google","client_id":"unit-client","client_secret":"unit-not-for-output"}])).unwrap()).unwrap();
        // Only the gateway's verified edition context authorizes this. There
        // is deliberately no protection marker/key or gateway instance lease.
        if verified_edition {
            crate::team_edition::record(&home, duduclaw_core::EditionProfile::Enterprise);
        }
        duduclaw_core::ensure_identity_key(&home).unwrap();
        let token_state = Arc::new(crate::google_identity::route::GoogleTokenRouteState::new(
            home.clone(),
            Some(db.clone()),
        ));
        let state = Arc::new(ExecuteState::new(home, Some(db.clone())));
        Self {
            _dir: dir,
            state,
            token_state,
            db,
            user: user.id,
        }
    }
    fn reference(&self) -> CredentialRef {
        CredentialRef {
            kind: CredentialKind::Legacy,
            sha256: format!("{:x}", Sha256::digest(DEDICATED.as_bytes())),
        }
    }
    fn select(&self) -> GoogleSelectionRequest {
        GoogleSelectionRequest::SelectGoogleAccount {
            tool: "gmail_read".into(),
            arguments: json!({"message_id":"unit"}),
            credential_ref: self.reference(),
        }
    }
    fn agent(&self, google: &str) {
        std::fs::write(
            self.state
                .home
                .join("agents")
                .join(AGENT)
                .join("agent.toml"),
            format!("[agent]\nname='{AGENT}'\n[google]\n{google}\n"),
        )
        .unwrap();
    }
    fn router(&self) -> Router {
        super::super::router(self.state.clone()).merge(crate::google_identity::route::router(
            self.token_state.clone(),
        ))
    }
}
fn signed(f: &Fixture, body: &[u8]) -> HeaderMap {
    use crate::computer_use_sessions::auth;
    static N: AtomicUsize = AtomicUsize::new(1);
    let nonce = format!("{:032x}", N.fetch_add(1, Ordering::SeqCst));
    let timestamp = crate::google_personal::authority::now().to_string();
    let token = duduclaw_core::mint_identity_token(
        &duduclaw_core::load_identity_key(&f.state.home).unwrap(),
        AGENT,
    );
    let signature = duduclaw_core::internal_request_signature(
        INTERNAL.as_bytes(),
        AGENT,
        &token,
        &timestamp,
        &nonce,
        body,
    );
    let mut headers = HeaderMap::new();
    for (k, v) in [
        (auth::AGENT_ID_HEADER, AGENT),
        (auth::TIMESTAMP_HEADER, &timestamp),
        (auth::NONCE_HEADER, &nonce),
        (auth::SIGNATURE_HEADER, &signature),
    ] {
        headers.insert(k, v.parse().unwrap());
    }
    headers
}
async fn raw(f: &Fixture, path: &str, body: Vec<u8>, headers: HeaderMap) -> (u16, Value) {
    raw_peer(f, path, body, headers, "127.0.0.1:12345".parse().unwrap()).await
}
async fn raw_peer(
    f: &Fixture,
    path: &str,
    body: Vec<u8>,
    headers: HeaderMap,
    peer: SocketAddr,
) -> (u16, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    req.headers_mut().extend(headers);
    req.extensions_mut().insert(ConnectInfo::<SocketAddr>(peer));
    let resp = f.router().oneshot(req).await.unwrap();
    let status = resp.status().as_u16();
    let body = axum::body::to_bytes(resp.into_body(), super::super::MAX_RESPONSE)
        .await
        .unwrap();
    (status, serde_json::from_slice(&body).unwrap())
}
async fn post(f: &Fixture, value: Value) -> (u16, Value) {
    let bytes = serde_json::to_vec(&value).unwrap();
    let headers = signed(f, &bytes);
    raw(f, GOOGLE_SELECTION_ROUTE, bytes, headers).await
}
async fn get_stamp(f: &Fixture) -> GoogleSelectionStamp {
    let (status, value) = post(f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(status, 200);
    serde_json::from_value(value["selection"].clone()).unwrap()
}
fn bind_request(f: &Fixture, stamp: &GoogleSelectionStamp, args: Value) -> Value {
    serde_json::to_value(GoogleSelectionRequest::BindGoogleArguments {
        opaque_id: stamp.opaque_id.clone(),
        tool: "gmail_read".into(),
        credential_ref: f.reference(),
        original_arguments_digest: stamp.original_arguments_digest.clone(),
        account_binding_digest: stamp.approval.account_binding_digest.clone(),
        arguments: args,
    })
    .unwrap()
}

#[tokio::test]
async fn real_gateway_metadata_requires_verified_edition_and_never_opens_protected_key() {
    let f = Fixture::new(false);
    let (status, value) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(status, 403);
    assert_eq!(value["code"], "not_available");
    crate::team_edition::record(&f.state.home, duduclaw_core::EditionProfile::Enterprise);
    let stamp = get_stamp(&f).await;
    assert!(stamp.is_well_formed());
    assert_eq!(
        stamp.approval.selected_google_account,
        "alice-google@unit.test"
    );
    assert_eq!(stamp.approval.selected_person_login, "login@unit.test");
    assert_eq!(stamp.approval.requester_kind, "unknown");
    assert!(!stamp.approval.home_fallback_authorized);
    let output = serde_json::to_string(&stamp).unwrap();
    for forbidden in [
        "unit-not-for-output",
        "pk:v1:",
        "access_token",
        "refresh_token",
        "google_sub",
    ] {
        assert!(!output.contains(forbidden));
    }
}
#[tokio::test]
async fn metadata_owner_consent_requester_key_tool_and_master_gates_are_live() {
    for policy in ["owner_only", "bound_people"] {
        let f = Fixture::new(true);
        f.agent(&format!(
            "identity='owner'\nrequesters='{policy}'\nfallback='home'"
        ));
        let (_, v) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
        assert_eq!(v["code"], "requester_refused");
    }
    let f = Fixture::new(true);
    f.db.set_google_consent(&f.user, AGENT, false, &f.user)
        .unwrap();
    let (_, v) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(v["code"], "consent_declined");
    let f = Fixture::new(true);
    let mut req = serde_json::to_value(f.select()).unwrap();
    req["tool"] = json!("gmail_create_draft");
    req["arguments"] = json!({"to":"recipient@unit.test","subject":"owned","body":"owned"});
    let (_, v) = post(&f, req).await;
    assert_eq!(v["code"], "scope_missing");
    let f = Fixture::new(true);
    let path = f.state.home.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        &path,
        raw.replace("google_workspace=true", "google_workspace=false"),
    )
    .unwrap();
    let (_, v) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(v["code"], "disabled");
    let f = Fixture::new(true);
    let stamp = get_stamp(&f).await;
    let path = f.state.home.join("config.toml");
    let raw = std::fs::read_to_string(&path).unwrap();
    std::fs::write(
        path,
        raw.replace("scopes=['google:read','google:write']", "scopes=[]"),
    )
    .unwrap();
    let (s, _) = post(&f, bind_request(&f, &stamp, json!({"message_id":"unit"}))).await;
    assert_eq!(s, 403);
}
#[tokio::test]
async fn three_routes_refuse_cross_purpose_bodies_before_token_or_effect_work_with_nonce_fences() {
    let f = Fixture::new(true);
    let probe = crate::google_identity::route_test_probe::Guard::install(&f.state.home);
    let before = std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap();
    let stamp = get_stamp(&f).await;
    let select = serde_json::to_value(f.select()).unwrap();
    let bind = bind_request(&f, &stamp, json!({"message_id":"unit"}));
    let effect = json!({"tool":"gmail_read","arguments":{"message_id":"unit"},"credential_ref":f.reference()});
    let token = json!({"tool":"gmail_read","turn_id":"fixture-turn"});
    let token_route = crate::google_identity::route::ROUTE;
    let effect_route = super::super::ROUTE;
    for (value, destinations) in [
        (&select, [token_route, effect_route]),
        (&bind, [token_route, effect_route]),
        (&effect, [token_route, GOOGLE_SELECTION_ROUTE]),
        (&token, [effect_route, GOOGLE_SELECTION_ROUTE]),
    ] {
        for path in destinations {
            // A fresh valid host proof reaches each actual registered route.
            // Neither 404 nor a replay rejection can satisfy the first check.
            let body = serde_json::to_vec(value).unwrap();
            let headers = signed(&f, &body);
            let (status, reply) = raw(&f, path, body.clone(), headers.clone()).await;
            assert_eq!(status, 400, "cross-purpose body accepted at {path}");
            assert_eq!(reply["code"], "bad_request");
            assert!(reply.get("access_token").is_none());
            assert!(reply.get("result").is_none());
            assert_eq!(
                raw(&f, path, body, headers).await.0,
                401,
                "even an authenticated malformed body consumes its route nonce"
            );
        }
    }
    // Token has its own existing replay state. Execute and selection share
    // theirs. The same host proof is not a different agent proof by route.
    let body = serde_json::to_vec(&select).unwrap();
    let headers = signed(&f, &body);
    assert_eq!(
        raw(&f, effect_route, body.clone(), headers.clone()).await.0,
        400
    );
    assert_eq!(
        raw(&f, GOOGLE_SELECTION_ROUTE, body.clone(), headers.clone())
            .await
            .0,
        401
    );
    assert_eq!(
        raw(&f, token_route, body.clone(), headers.clone()).await.0,
        400
    );
    assert_eq!(raw(&f, token_route, body, headers).await.0, 401);
    let fresh = get_stamp(&f).await;
    assert_eq!(
        post(&f, bind_request(&f, &fresh, json!({"message_id":"unit"})))
            .await
            .0,
        200
    );
    let mut unknown = select;
    unknown["purpose"] = json!("claim_google_account");
    assert_eq!(post(&f, unknown).await.0, 400);
    assert_eq!(
        probe.counts(),
        [0, 0],
        "no token/key/provider or effect work boundary entered"
    );
    assert_eq!(
        std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap(),
        before
    );
}
#[tokio::test]
async fn same_selection_bind_is_one_final_digest_and_consent_aba_never_recaptures() {
    let f = Fixture::new(true);
    let stamp = get_stamp(&f).await;
    let args = json!({"message_id":"restored-unit"});
    let (s, v) = post(&f, bind_request(&f, &stamp, args.clone())).await;
    assert_eq!(s, 200);
    let bound: GoogleSelectionStamp = serde_json::from_value(v["selection"].clone()).unwrap();
    assert_eq!(
        bound.final_arguments_digest,
        Some(google_arguments_digest(&args))
    );
    assert_eq!(bound.approval, stamp.approval);
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    assert_eq!(
        post(
            &f,
            bind_request(&f, &stamp, json!({"message_id":"changed"}))
        )
        .await
        .0,
        403
    );
    f.db.set_google_consent(&f.user, AGENT, false, &f.user)
        .unwrap();
    f.db.set_google_consent(&f.user, AGENT, true, &f.user)
        .unwrap();
    assert_eq!(post(&f, bind_request(&f, &stamp, args)).await.0, 403);
}
#[tokio::test]
async fn personal_approval_never_authorizes_home_after_refresh_failure_or_without_a_home_receipt() {
    let f = Fixture::new(true);
    f.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    let stamp = get_stamp(&f).await;
    assert!(stamp.approval.configured_home_fallback);
    assert!(!stamp.approval.home_fallback_authorized);
    let args = json!({"message_id":"unit"});
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    let claim = claim_selection(
        &f.state,
        AGENT,
        &f.reference(),
        "gmail_read",
        &stamp.opaque_id,
        &args,
        Clock::now(),
    )
    .unwrap();
    let fallback = crate::google_identity::resolve::Resolved::UseHome {
        fallback: true,
        notice: Some("已改用公司共用帳號：refresh failed".into()),
    };
    let error = claim.check_resolved(&fallback).unwrap_err();
    assert_eq!(error.code, "approved_account_mismatch");
    assert!(!error.fallback_eligible);
    // A captured approval remains stale after disconnect; it cannot become
    // a new Home selection merely because fallback was configured.
    let stale = Fixture::new(true);
    stale.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    let stale_stamp = get_stamp(&stale).await;
    crate::person_credentials::disconnect(&stale.state.home, &stale.user).unwrap();
    let (status, value) = post(&stale, bind_request(&stale, &stale_stamp, args.clone())).await;
    assert_eq!(status, 403);
    assert_eq!(value["code"], "identity_changed");
    let f = Fixture::new(true);
    f.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    crate::person_credentials::disconnect(&f.state.home, &f.user).unwrap();
    let (s, v) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(s, 403);
    // This is a fresh selection after disconnect, not a stale accepted claim.
    assert_eq!(v["code"], "account_unavailable");
    assert_eq!(f.state.selections.len_for_test(), 0);
}
#[tokio::test]
async fn opaque_claim_is_atomic_and_cannot_be_replayed_with_a_fresh_host_nonce() {
    let f = Fixture::new(true);
    let stamp = get_stamp(&f).await;
    let args = json!({"message_id":"unit"});
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    let wrong = CredentialRef {
        kind: CredentialKind::Legacy,
        sha256: "a".repeat(64),
    };
    assert!(
        claim_selection(
            &f.state,
            AGENT,
            &wrong,
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now()
        )
        .is_err()
    );
    assert!(
        claim_selection(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &stamp.opaque_id,
            &json!({"message_id":"changed"}),
            Clock::now()
        )
        .is_err()
    );
    let mut tasks = Vec::new();
    for _ in 0..2 {
        let state = f.state.clone();
        let reference = f.reference();
        let id = stamp.opaque_id.clone();
        let args = args.clone();
        tasks.push(tokio::task::spawn_blocking(move || {
            claim_selection(
                &state,
                AGENT,
                &reference,
                "gmail_read",
                &id,
                &args,
                Clock::now(),
            )
            .is_ok()
        }));
    }
    let mut claims = 0;
    for t in tasks {
        claims += usize::from(t.await.unwrap());
    }
    assert_eq!(claims, 1);
    // Bind uses a fresh valid MAC/nonce, but a consumed opaque id is gone.
    assert_eq!(post(&f, bind_request(&f, &stamp, args)).await.0, 403);
}
#[test]
fn ttl_and_caps_are_bounded_without_sleep_or_external_io() {
    let f = Fixture::new(true);
    let now = Clock::now();
    let stamp = select_account(
        &f.state,
        AGENT,
        &f.reference(),
        "gmail_read",
        &json!({"message_id":"unit"}),
        now,
    )
    .unwrap()
    .unwrap();
    let later = Clock {
        unix: now.unix + GOOGLE_SELECTION_TTL_SECS,
        mono: now.mono + Duration::from_secs(GOOGLE_SELECTION_TTL_SECS),
    };
    assert!(
        bind_account(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &stamp.opaque_id,
            &stamp.original_arguments_digest,
            &stamp.approval.account_binding_digest,
            &json!({"message_id":"unit"}),
            later
        )
        .is_err()
    );
    for _ in 0..MAX_SELECTIONS_PER_AGENT {
        select_account(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &json!({"message_id":"unit"}),
            later,
        )
        .unwrap();
    }
    let error = select_account(
        &f.state,
        AGENT,
        &f.reference(),
        "gmail_read",
        &json!({"message_id":"unit"}),
        later,
    )
    .unwrap_err();
    assert_eq!(error.code, "selection_capacity");
}
#[tokio::test]
async fn metadata_route_uses_existing_shared_rate_limit() {
    let f = Fixture::new(true);
    let mut invalid = serde_json::to_value(f.select()).unwrap();
    invalid["purpose"] = json!("not_a_purpose");
    for _ in 0..crate::computer_use_sessions::auth::RATE_LIMIT_PER_MINUTE {
        assert_eq!(post(&f, invalid.clone()).await.0, 400);
    }
    assert_eq!(post(&f, invalid).await.0, 429);
}

#[tokio::test]
async fn first_dedicated_select_explicitly_normalizes_active_owner_metadata_before_pinning() {
    let f = Fixture::new(true);
    f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended)
        .unwrap();
    crate::person_credentials::sync_user_status(&f.state.home, &f.user, false).unwrap();
    f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Active)
        .unwrap();
    let before: Value = serde_json::from_slice(
        &std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap(),
    )
    .unwrap();
    assert_eq!(before["records"][0]["status"], "suspended");
    let stamp = get_stamp(&f).await;
    let after: Value = serde_json::from_slice(
        &std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap(),
    )
    .unwrap();
    assert_eq!(after["records"][0]["status"], "connected");
    assert_eq!(
        after["records"][0]["revision"].as_u64(),
        before["records"][0]["revision"].as_u64().map(|n| n + 1)
    );
    assert_eq!(
        after["epochs"][f.user.as_str()].as_u64(),
        before["epochs"][f.user.as_str()].as_u64().map(|n| n + 1)
    );
    assert!(
        before["records"][0]["secret"] == after["records"][0]["secret"],
        "normalization never touches the envelope"
    );
    assert_eq!(
        post(&f, bind_request(&f, &stamp, json!({"message_id":"unit"})))
            .await
            .0,
        200
    );
}
#[tokio::test]
async fn suspended_active_owner_invalid_client_metadata_refuses_without_any_vault_write() {
    for failure in [
        "missing_file",
        "invalid_json",
        "missing_google",
        "client_mismatch",
    ] {
        let f = Fixture::new(true);
        f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended)
            .unwrap();
        crate::person_credentials::sync_user_status(&f.state.home, &f.user, false).unwrap();
        f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Active)
            .unwrap();
        let client_path = f.state.home.join("mcp-oauth-configs.json");
        match failure {
            "missing_file" => std::fs::remove_file(client_path).unwrap(),
            "invalid_json" => std::fs::write(client_path, "{invalid-json").unwrap(),
            "missing_google" => std::fs::write(client_path, "[]").unwrap(),
            "client_mismatch" => std::fs::write(
                client_path,
                r#"[{"provider_id":"google","client_id":"different-client"}]"#,
            )
            .unwrap(),
            _ => unreachable!(),
        }
        let before = std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap();
        let metadata: Value = serde_json::from_slice(&before).unwrap();
        assert_eq!(metadata["records"][0]["status"], "suspended");
        let (status, reply) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
        assert_eq!(status, 403, "{failure}");
        assert_eq!(reply["code"], "identity_changed", "{failure}");
        assert_eq!(f.state.selections.len_for_test(), 0, "{failure}");
        assert_eq!(
            std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap(),
            before,
            "client metadata refusal must precede status/revision/epoch CAS: {failure}"
        );
    }
}

#[tokio::test]
async fn normalization_refused_for_consent_scope_or_requester_cannot_write_metadata() {
    for reason in ["consent", "scope", "requester"] {
        let f = Fixture::new(true);
        crate::person_credentials::sync_user_status(&f.state.home, &f.user, false).unwrap();
        if reason == "consent" {
            f.db.set_google_consent(&f.user, AGENT, false, &f.user)
                .unwrap();
        }
        if reason == "requester" {
            f.agent("identity='owner'\nrequesters='owner_only'");
        }
        let before = std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap();
        let mut request = serde_json::to_value(f.select()).unwrap();
        if reason == "scope" {
            request["tool"] = json!("gmail_create_draft");
        }
        assert_eq!(post(&f, request).await.0, 403);
        assert!(
            before == std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap(),
            "refused preparation must preserve metadata bytes"
        );
    }
}

#[tokio::test]
async fn uncertain_revoke_metadata_barrier_is_hard_and_never_home_fallback() {
    let f = Fixture::new(true);
    f.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    std::fs::create_dir_all(f.state.home.join("locks")).unwrap();
    std::fs::write(
        f.state
            .home
            .join("locks/google-grant-revoke-uncertain.json"),
        "{}",
    )
    .unwrap();
    let (status, value) = post(&f, serde_json::to_value(f.select()).unwrap()).await;
    assert_eq!(status, 403);
    assert_eq!(value["code"], "grant_uncertain");
    assert_eq!(f.state.selections.len_for_test(), 0);
}
#[test]
fn global_count_and_byte_caps_are_separate_from_per_employee_cap() {
    for byte_cap in [false, true] {
        let mut f = Fixture::new(true);
        let state = Arc::get_mut(&mut f.state).unwrap();
        state
            .selections
            .set_limits_for_test(1, 2, if byte_cap { 1 } else { MAX_TOTAL_BYTES });
        let first = select_account(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &json!({"message_id":"unit"}),
            Clock::now(),
        );
        if byte_cap {
            assert_eq!(first.unwrap_err().code, "selection_capacity");
            assert_eq!(f.state.selections.len_for_test(), 0);
        } else {
            first.unwrap();
            assert_eq!(
                select_account(
                    &f.state,
                    AGENT,
                    &f.reference(),
                    "gmail_read",
                    &json!({"message_id":"unit"}),
                    Clock::now()
                )
                .unwrap_err()
                .code,
                "selection_capacity"
            );
        }
    }
}

#[test]
fn normalization_checks_original_consent_generation_under_vault_cas_and_does_not_reselect() {
    let f = Fixture::new(true);
    crate::person_credentials::sync_user_status(&f.state.home, &f.user, false).unwrap();
    let before = std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap();
    let people = crate::google_identity::requester_gate::UserDbPeople(f.db.clone());
    let fired = std::sync::atomic::AtomicBool::new(false);
    let result = PersonalSelection::prepare_dedicated(
        &f.state.home,
        AGENT,
        &RequesterIdentity::Unknown,
        &people,
        "gmail_read",
        || {
            live_key(&f.state, AGENT, &f.reference(), "gmail_read")?;
            if !fired.swap(true, Ordering::SeqCst) {
                f.db.set_google_consent(&f.user, AGENT, false, &f.user)
                    .unwrap();
                f.db.set_google_consent(&f.user, AGENT, true, &f.user)
                    .unwrap();
            }
            Ok(())
        },
    );
    assert!(result.is_err());
    assert!(before == std::fs::read(crate::person_credentials::path(&f.state.home)).unwrap());
    assert_eq!(f.state.selections.len_for_test(), 0);
}
#[tokio::test]
async fn opaque_selection_disappears_on_gateway_state_restart_and_unbound_id_cannot_claim() {
    let f = Fixture::new(true);
    let stamp = get_stamp(&f).await;
    let args = json!({"message_id":"unit"});
    assert!(
        claim_selection(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now()
        )
        .is_err()
    );
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    let restarted = ExecuteState::new(f.state.home.clone(), Some(f.db.clone()));
    assert!(
        claim_selection(
            &restarted,
            AGENT,
            &f.reference(),
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now()
        )
        .is_err()
    );
    assert!(
        claim_selection(
            &f.state,
            AGENT,
            &f.reference(),
            "gmail_read",
            &stamp.opaque_id,
            &args,
            Clock::now()
        )
        .is_ok()
    );
}

#[tokio::test]
async fn metadata_host_auth_refuses_missing_signature_external_key_mac_and_nonloopback_peer() {
    use crate::computer_use_sessions::auth;
    let f = Fixture::new(true);
    let body = serde_json::to_vec(&f.select()).unwrap();
    assert_eq!(
        raw(&f, GOOGLE_SELECTION_ROUTE, body.clone(), HeaderMap::new())
            .await
            .0,
        401
    );
    let mut headers = signed(&f, &body);
    let timestamp = headers[auth::TIMESTAMP_HEADER].to_str().unwrap();
    let nonce = headers[auth::NONCE_HEADER].to_str().unwrap();
    let token = duduclaw_core::mint_identity_token(
        &duduclaw_core::load_identity_key(&f.state.home).unwrap(),
        AGENT,
    );
    let signature = duduclaw_core::internal_request_signature(
        DEDICATED.as_bytes(),
        AGENT,
        &token,
        timestamp,
        nonce,
        &body,
    );
    headers.insert(auth::SIGNATURE_HEADER, signature.parse().unwrap());
    assert_eq!(
        raw(&f, GOOGLE_SELECTION_ROUTE, body.clone(), headers)
            .await
            .0,
        401
    );
    let mut headers = signed(&f, &body);
    headers.insert("x-forwarded-for", "127.0.0.1".parse().unwrap());
    assert_eq!(
        raw_peer(
            &f,
            GOOGLE_SELECTION_ROUTE,
            body,
            headers,
            "198.51.100.7:12345".parse().unwrap()
        )
        .await
        .0,
        401
    );
}

fn home_fallback_fixture() -> Fixture {
    let f = Fixture::new(true);
    f.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    crate::person_credentials::disconnect(&f.state.home, &f.user).unwrap();
    std::fs::write(f.state.home.join("mcp-oauth-configs.json"), serde_json::to_vec(&json!([{
        "provider_id":"google","client_id":"unit-client","client_secret":"enc:v1:unusable-owned-client",
        "auth_url":"https://accounts.google.com/o/oauth2/v2/auth","token_url":"https://oauth2.googleapis.com/token",
        "redirect_uri":"http://127.0.0.1/callback","scopes":["client-request-not-authority"]
    }])).unwrap()).unwrap();
    std::fs::write(f.state.home.join("mcp-oauth-tokens.json"), serde_json::to_vec(&json!([{
        "provider_id":"google","access_token":"owned-home-access","refresh_token":null,"expires_at":null,
        "scopes":["https://www.googleapis.com/auth/gmail.readonly"]
    }])).unwrap()).unwrap();
    f
}
async fn execute_selected(
    f: &Fixture,
    stamp: &GoogleSelectionStamp,
    tool: &str,
    args: Value,
) -> (u16, Value) {
    let body = serde_json::to_vec(&json!({"tool":tool,"arguments":args,"credential_ref":f.reference(),"selection_stamp":stamp.opaque_id})).unwrap();
    let headers = signed(f, &body);
    raw(f, super::super::ROUTE, body, headers).await
}

#[tokio::test]
async fn home_logical_approval_is_host_minted_and_same_row_final_binding_is_required() {
    let f = home_fallback_fixture();
    let stamp = get_stamp(&f).await;
    assert_eq!(
        stamp.approval.selected_identity,
        gi::ApprovedGoogleIdentity::HomeOAuthFallback
    );
    assert!(stamp.approval.home_fallback_authorized);
    assert!(stamp.approval.selected_google_account.is_empty());
    assert!(stamp.approval.selected_person_login.is_empty());
    assert!(
        stamp
            .approval
            .home_connection
            .as_ref()
            .is_some_and(|h| h.original_unavailable == "not_connected")
    );
    let another = get_stamp(&f).await;
    assert_ne!(
        stamp.approval.account_binding_digest,
        another.approval.account_binding_digest
    );
    let mut wrong = bind_request(&f, &stamp, json!({"message_id":"unit"}));
    wrong["account_binding_digest"] = json!(another.approval.account_binding_digest);
    assert_eq!(post(&f, wrong).await.0, 403);
    assert_eq!(
        post(&f, bind_request(&f, &stamp, json!({"message_id":"unit"})))
            .await
            .0,
        200
    );
}

#[tokio::test]
async fn signed_selected_http_consumes_claim_before_closed_sender_refusal_and_never_enters_legacy()
{
    let f = home_fallback_fixture();
    let guard = crate::google_identity::route_test_probe::Guard::install(&f.state.home);
    let stamp = get_stamp(&f).await;
    let args = json!({"message_id":"unit"});
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    let first = execute_selected(&f, &stamp, "gmail_read", args.clone()).await;
    assert_eq!(first.0, 403);
    assert_eq!(first.1["code"], "personal_external_unsupported");
    assert_eq!(f.state.selections.len_for_test(), 0);
    let second = execute_selected(&f, &stamp, "gmail_read", args).await;
    assert_eq!(second.0, 403);
    assert_eq!(second.1["code"], "identity_changed");
    assert_eq!(guard.counts(), [0, 0]);
    assert!(!f.state.home.join(".keyfile").exists());
    assert!(!f.state.home.join("locks/google-grants.lock").exists());
}

#[tokio::test]
async fn signed_home_status_consumes_real_claim_and_returns_only_unverified_logical_metadata() {
    let f = home_fallback_fixture();
    let guard = crate::google_identity::route_test_probe::Guard::install(&f.state.home);
    let args = json!({});
    let select = GoogleSelectionRequest::SelectGoogleAccount {
        tool: "google_status".into(),
        arguments: args.clone(),
        credential_ref: f.reference(),
    };
    let (s, v) = post(&f, serde_json::to_value(select).unwrap()).await;
    assert_eq!(s, 200);
    let stamp: GoogleSelectionStamp = serde_json::from_value(v["selection"].clone()).unwrap();
    let bind = GoogleSelectionRequest::BindGoogleArguments {
        tool: "google_status".into(),
        arguments: args.clone(),
        credential_ref: f.reference(),
        opaque_id: stamp.opaque_id.clone(),
        original_arguments_digest: stamp.original_arguments_digest.clone(),
        account_binding_digest: stamp.approval.account_binding_digest.clone(),
    };
    assert_eq!(post(&f, serde_json::to_value(bind).unwrap()).await.0, 200);
    let (s, v) = execute_selected(&f, &stamp, "google_status", args).await;
    assert_eq!(s, 200);
    let text = serde_json::to_string(&v).unwrap();
    assert!(text.contains("未驗證"));
    assert!(!text.contains("owned-home-access"));
    assert!(!text.contains("alice-google@unit.test"));
    assert_eq!(guard.counts(), [0, 0]);
    assert_eq!(f.state.selections.len_for_test(), 0);
    assert!(!f.state.home.join("locks/google-grants.lock").exists());
}

#[tokio::test]
async fn original_home_receipt_or_personal_unavailable_change_consumes_or_refuses_without_reselecting()
 {
    let f = home_fallback_fixture();
    let stamp = get_stamp(&f).await;
    let args = json!({"message_id":"unit"});
    assert_eq!(
        post(&f, bind_request(&f, &stamp, args.clone())).await.0,
        200
    );
    f.db.set_user_status(&f.user, duduclaw_auth::UserStatus::Suspended)
        .unwrap();
    assert_eq!(
        execute_selected(&f, &stamp, "gmail_read", args).await.0,
        403
    );
    assert_eq!(f.state.selections.len_for_test(), 0);
    let f = home_fallback_fixture();
    let stamp = get_stamp(&f).await;
    let mut rows: Value =
        serde_json::from_slice(&std::fs::read(f.state.home.join("mcp-oauth-tokens.json")).unwrap())
            .unwrap();
    rows[0]["google_home"]["source_generation"] = json!(uuid::Uuid::new_v4().to_string());
    std::fs::write(
        f.state.home.join("mcp-oauth-tokens.json"),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
    assert_eq!(
        post(&f, bind_request(&f, &stamp, json!({"message_id":"unit"})))
            .await
            .0,
        403
    );
}

#[test]
fn stored_home_union_weight_counts_private_metadata_and_rejects_oversized_original_personal_grants()
{
    let f = Fixture::new(true);
    f.agent("identity='owner'\nrequesters='anyone'\nfallback='home'");
    let path = crate::person_credentials::path(&f.state.home);
    let mut data: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    data["records"][0]["status"] = json!("needs_reconnect");
    let mut scopes = vec!["https://www.googleapis.com/auth/gmail.readonly".to_string()];
    scopes.extend((0..64).map(|i| format!("{i:03}-{}", "x".repeat(1900))));
    data["records"][0]["granted_scopes"] = json!(scopes);
    std::fs::write(&path, serde_json::to_vec(&data).unwrap()).unwrap();
    std::fs::write(f.state.home.join("mcp-oauth-configs.json"),serde_json::to_vec(&json!([{"provider_id":"google","client_id":"unit-client","client_secret":"","auth_url":"https://accounts.google.com/o/oauth2/v2/auth","token_url":"https://oauth2.googleapis.com/token","redirect_uri":"http://127.0.0.1/callback","scopes":[]}])).unwrap()).unwrap();
    std::fs::write(f.state.home.join("mcp-oauth-tokens.json"),serde_json::to_vec(&json!([{"provider_id":"google","access_token":"owned-home","refresh_token":null,"expires_at":null,"scopes":["https://www.googleapis.com/auth/gmail.readonly"]}])).unwrap()).unwrap();
    let result = select_account(
        &f.state,
        AGENT,
        &f.reference(),
        "gmail_read",
        &json!({}),
        Clock::now(),
    );
    assert!(result.is_err_and(|e| e.code == "selection_capacity"));
    assert_eq!(f.state.selections.len_for_test(), 0);
}
