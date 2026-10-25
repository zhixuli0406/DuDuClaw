//! Source-first presence and owned fake-wire callback regressions, UNRUN.
use super::super::{HomeRevokePresence, home_presence_for_personal_revoke};
use super::*;
const NOW: u64 = 1_800_000_000;
fn home() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=false\n",
    )
    .unwrap();
    super::super::upsert_client_config(
        d.path(),
        super::super::McpOAuthClientConfig {
            provider_id: "google".into(),
            client_id: "presence-fixture".into(),
            client_secret: String::new(),
            auth_url: crate::google_personal::oauth::AUTHORIZE.into(),
            token_url: crate::google_personal::oauth::TOKEN.into(),
            redirect_uri: "http://127.0.0.1:18789/api/mcp/oauth/callback".into(),
            scopes: vec![],
        },
    )
    .unwrap();
    d
}
fn raw(home: &Path) -> Vec<u8> {
    std::fs::read(home.join(TOKEN_FILE)).unwrap()
}
fn edit(home: &Path, change: impl FnOnce(&mut Value)) {
    let mut rows: Value = serde_json::from_slice(&raw(home)).unwrap();
    change(&mut rows[0]);
    std::fs::write(home.join(TOKEN_FILE), serde_json::to_vec(&rows).unwrap()).unwrap();
}
fn pending(home: &Path) {
    let desired = super::super::McpOAuthConfig {
        provider_id: "google".into(),
        client_id: "presence-fixture".into(),
        client_secret: String::new(),
        auth_url: crate::google_personal::oauth::AUTHORIZE.into(),
        token_url: crate::google_personal::oauth::TOKEN.into(),
        redirect_uri: "http://127.0.0.1:18789/api/mcp/oauth/callback".into(),
        scopes: vec![],
    };
    super::super::home_lifecycle::begin_google_home_connect(
        home,
        &desired,
        RawDigest::of(b"presence-state"),
        NOW + 500,
    )
    .unwrap();
}
#[tokio::test(flavor = "current_thread")]
async fn actual_saved_google_client_with_personal_only_and_disabled_master_has_no_home_credential()
{
    let d = home();
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let before = raw(d.path());
    let client = std::fs::read(d.path().join(super::super::CLIENT_CONFIG_FILE)).unwrap();
    let probe = test_probe::Guard::install(d.path());
    assert_eq!(
        home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
        HomeRevokePresence::NoCredential
    );
    assert!(
        super::super::get_token(d.path(), "google").is_none(),
        "public view still hides the private control"
    );
    assert_eq!(raw(d.path()), before);
    assert_eq!(
        std::fs::read(d.path().join(super::super::CLIENT_CONFIG_FILE)).unwrap(),
        client
    );
    assert_eq!(probe.counts(), [0, 0]);
    assert!(!d.path().join(".keyfile").exists());
}
#[tokio::test(flavor = "current_thread")]
async fn empty_store_and_valid_revoked_tombstone_are_read_only_absence() {
    let d = tempfile::tempdir().unwrap();
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    assert_eq!(
        home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
        HomeRevokePresence::NoCredential
    );
    assert!(!d.path().join(TOKEN_FILE).exists());
    revoke_google(d.path()).unwrap();
    let before = raw(d.path());
    assert_eq!(
        home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
        HomeRevokePresence::NoCredential
    );
    assert_eq!(raw(d.path()), before);
    assert!(!d.path().join(".keyfile").exists());
}
#[tokio::test(flavor = "current_thread")]
async fn ready_pending_start_is_absent_only_with_original_source_generation_and_fresh_expiry() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    pending(d.path());
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let before = raw(d.path());
    assert_eq!(
        home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
        HomeRevokePresence::NoCredential
    );
    assert_eq!(raw(d.path()), before);
    for kind in [
        "generation",
        "source",
        "client",
        "home",
        "expiry",
        "exchanging",
    ] {
        std::fs::write(d.path().join(TOKEN_FILE), &before).unwrap();
        edit(d.path(), |row| {
            let ticket = &mut row["google_home"]["active_connect"];
            match kind {
                "generation" => ticket["generation"] = json!(uuid::Uuid::new_v4()),
                "source" => {
                    ticket["source_stamp"] =
                        serde_json::to_value(RawDigest::of(b"different source")).unwrap()
                }
                "client" => {
                    ticket["client_stamp"] =
                        serde_json::to_value(RawDigest::of(b"different client")).unwrap()
                }
                "home" => {
                    ticket["canonical_home"] =
                        serde_json::to_value(RawDigest::of(b"different home")).unwrap()
                }
                "expiry" => ticket["expires_at"] = json!(NOW),
                "exchanging" => ticket["phase"] = json!("exchanging"),
                _ => unreachable!(),
            }
        });
        let invalid = raw(d.path());
        assert!(
            home_presence_for_personal_revoke(d.path(), &lease).is_err(),
            "{kind}"
        );
        assert_eq!(raw(d.path()), invalid, "{kind}");
    }
    std::fs::write(d.path().join(TOKEN_FILE), before).unwrap();
    std::fs::write(
        d.path().join("config.toml"),
        "[integrations]\ngoogle_workspace=true\n",
    )
    .unwrap();
    assert!(
        home_presence_for_personal_revoke(d.path(), &lease).is_err(),
        "same ticket cannot recapture a changed source"
    );
    assert!(!d.path().join(".keyfile").exists());
}
#[tokio::test(flavor = "current_thread")]
async fn malformed_control_legacy_missing_token_and_residual_payload_never_prove_absence() {
    for kind in [
        "legacy",
        "schema",
        "credential",
        "pending",
        "refresh_token",
        "expires_at",
        "scopes",
        "malformed",
        "duplicate",
    ] {
        let d = home();
        let lease = crate::google_personal::grants::acquire(d.path())
            .await
            .unwrap();
        edit(d.path(), |row| match kind {
            "legacy" => {
                row.as_object_mut().unwrap().remove("google_home");
            }
            "schema" => row["google_home"]["schema"] = json!(99),
            "credential" => {
                row["google_home"]["credential"] = json!({"credential_id":uuid::Uuid::new_v4(),"logical_epoch":1,"token_revision":1})
            }
            "pending" => row["google_home"]["state"] = json!("pending"),
            "refresh_token" => row["refresh_token"] = json!("enc:v1:unknown-refresh"),
            "expires_at" => row["expires_at"] = Value::Null,
            "scopes" => row["scopes"] = json!([]),
            "malformed" | "duplicate" => {}
            _ => unreachable!(),
        });
        if kind == "malformed" {
            std::fs::write(d.path().join(TOKEN_FILE), b"{bad").unwrap();
        }
        if kind == "duplicate" {
            let rows: Value = serde_json::from_slice(&raw(d.path())).unwrap();
            std::fs::write(
                d.path().join(TOKEN_FILE),
                serde_json::to_vec(&json!([rows[0], rows[0]])).unwrap(),
            )
            .unwrap();
        }
        let before = raw(d.path());
        let probe = test_probe::Guard::install(d.path());
        assert!(
            home_presence_for_personal_revoke(d.path(), &lease).is_err(),
            "{kind}"
        );
        assert_eq!(raw(d.path()), before, "{kind}");
        assert_eq!(probe.counts(), [0, 0]);
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn real_legacy_or_controlled_token_presence_is_not_promoted_to_readable_or_decrypted() {
    for controlled in [false, true] {
        let d = home();
        let lease = crate::google_personal::grants::acquire(d.path())
            .await
            .unwrap();
        edit(d.path(), |row| {
            if controlled {
                row["google_home"]["credential"] = json!({"credential_id":uuid::Uuid::new_v4(),"logical_epoch":1,"token_revision":1});
            } else {
                row.as_object_mut().unwrap().remove("google_home");
            }
            row["access_token"] = json!("enc:v1:unusable-access");
            row["refresh_token"] = json!("enc:v2:unusable-refresh");
            row["expires_at"] = Value::Null;
            row["scopes"] = json!([]);
        });
        let before = raw(d.path());
        let probe = test_probe::Guard::install(d.path());
        assert_eq!(
            home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
            HomeRevokePresence::GoogleCredential
        );
        assert_eq!(raw(d.path()), before);
        assert_eq!(probe.counts(), [0, 0]);
        assert!(!d.path().join(".keyfile").exists());
    }
}
#[tokio::test(flavor = "current_thread")]
async fn unknown_non_google_rows_publication_gate_and_foreign_lease_stay_closed() {
    let d = home();
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    let before = raw(d.path());
    let mut rows: Value = serde_json::from_slice(&before).unwrap();
    rows.as_array_mut()
        .unwrap()
        .push(json!({"provider_id":"unknown-provider","future":{"opaque":true}}));
    std::fs::write(
        d.path().join(TOKEN_FILE),
        serde_json::to_vec(&rows).unwrap(),
    )
    .unwrap();
    assert_eq!(
        home_presence_for_personal_revoke(d.path(), &lease).unwrap(),
        HomeRevokePresence::OtherProvider
    );
    std::fs::write(d.path().join(TOKEN_FILE), &before).unwrap();
    std::fs::write(
        d.path().join(publication::GATE_FILE),
        b"unconfirmed publication",
    )
    .unwrap();
    assert!(home_presence_for_personal_revoke(d.path(), &lease).is_err());
    assert_eq!(raw(d.path()), before);
    let other = tempfile::tempdir().unwrap();
    assert!(home_presence_for_personal_revoke(other.path(), &lease).is_err());
    assert!(!other.path().join(TOKEN_FILE).exists());
    assert!(!d.path().join(".keyfile").exists());
}

async fn ram_callback(
    home: &Path,
    age: Option<std::time::Duration>,
) -> (
    String,
    tokio::sync::RwLock<std::collections::HashMap<String, super::super::PendingOAuth>>,
) {
    use super::super::home_lifecycle as flow;
    let plan =
        flow::capture_google_home_start(home, flow::GoogleHomeStartInput::new(None, None).unwrap())
            .unwrap();
    let started = flow::begin_google_home_authorization(home, plan)
        .await
        .unwrap();
    let map = tokio::sync::RwLock::new(std::collections::HashMap::new());
    let (_, state) = super::super::insert_google_start(home, started, &map)
        .await
        .unwrap();
    if let Some(age) = age {
        let mut entries = map.write().await;
        let entry = entries.get_mut(&state).unwrap();
        match &mut entry.flow {
            super::super::PendingOAuthFlow::Google(pending) => flow::testing::age_ram(pending, age),
            _ => unreachable!(),
        }
    }
    (state, map)
}
#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn actual_ram_callback_waits_for_inflight_revoke_then_consumes_same_ticket_once() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    let (state, map) = ram_callback(d.path(), None).await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    lease.begin_revoke().unwrap();
    let query = format!("state={state}&code=owned-code");
    let callback = super::super::complete_callback_query(d.path(), &map, &query);
    tokio::pin!(callback);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut callback)
            .await
            .is_err(),
        "inflight marker must wait, not become terminal before grant acquire"
    );
    assert!(
        map.read().await.is_empty(),
        "actual callback consumed its one original RAM state"
    );
    assert_eq!(wire.requests(), 0);
    lease.confirm_revoke().unwrap();
    drop(lease);
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(3), &mut callback)
            .await
            .unwrap()
            .unwrap(),
        "google"
    );
    assert_eq!(wire.requests(), 1);
    assert!(
        super::super::complete_callback_query(d.path(), &map, &query)
            .await
            .is_err()
    );
    assert_eq!(wire.requests(), 1);
}
#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn actual_ram_callback_refuses_persisted_marker_after_revoke_holder_is_cancelled() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    let (state, map) = ram_callback(d.path(), None).await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    lease.begin_revoke().unwrap();
    let before = raw(d.path());
    let query = format!("state={state}&code=owned-code");
    let callback = super::super::complete_callback_query(d.path(), &map, &query);
    tokio::pin!(callback);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut callback)
            .await
            .is_err()
    );
    drop(lease); // Simulate cancellation: no confirm_revoke, marker remains.
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), &mut callback)
            .await
            .unwrap()
            .is_err()
    );
    assert!(map.read().await.is_empty());
    assert_eq!(wire.requests(), 0);
    assert_eq!(raw(d.path()), before);
    assert!(
        d.path()
            .join("locks/google-grant-revoke-uncertain.json")
            .exists()
    );
}
#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn actual_ram_callback_grant_wait_timeout_never_claims_or_sends_provider() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    let (state, map) = ram_callback(d.path(), None).await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    lease.begin_revoke().unwrap();
    let before = raw(d.path());
    let query = format!("state={state}&code=owned-code");
    let callback = super::super::complete_callback_query(d.path(), &map, &query);
    tokio::pin!(callback);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut callback)
            .await
            .is_err(),
        "must reach real bounded lease wait"
    );
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(6), &mut callback)
            .await
            .unwrap()
            .is_err(),
        "original5second grant budget remains bounded"
    );
    assert_eq!(wire.requests(), 0);
    assert_eq!(raw(d.path()), before);
    assert!(map.read().await.is_empty());
    lease.confirm_revoke().unwrap();
    drop(lease);
}
#[tokio::test(flavor = "current_thread")]
#[cfg(unix)]
async fn actual_ram_callback_wait_retains_original_ram_deadline_despite_fixed_wall_clock() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    let (state, map) = ram_callback(d.path(), Some(std::time::Duration::from_secs(590))).await;
    let ram = super::super::home_lifecycle::testing::RamClockGuard::at(d.path());
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    let lease = crate::google_personal::grants::acquire(d.path())
        .await
        .unwrap();
    lease.begin_revoke().unwrap();
    let before = raw(d.path());
    let query = format!("state={state}&code=owned-code");
    let callback = super::super::complete_callback_query(d.path(), &map, &query);
    tokio::pin!(callback);
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(75), &mut callback)
            .await
            .is_err()
    );
    ram.elapsed_handle()
        .store(11, std::sync::atomic::Ordering::SeqCst);
    lease.confirm_revoke().unwrap();
    drop(lease);
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), &mut callback)
            .await
            .unwrap()
            .is_err()
    );
    assert_eq!(wire.requests(), 0);
    assert_eq!(raw(d.path()), before);
    assert!(map.read().await.is_empty());
}
#[tokio::test(flavor = "current_thread")]
async fn actual_ram_callback_bad_revocation_marker_is_consumed_without_claim_or_provider() {
    let _clock = publication::ClockGuard::fixed_if_unset(NOW);
    let d = home();
    let (state, map) = ram_callback(d.path(), None).await;
    let wire = home_source::callback_testing::Wire::json(d.path(), None);
    std::fs::create_dir_all(d.path().join("locks")).unwrap();
    let marker = d.path().join("locks/google-grant-revoke-uncertain.json");
    std::fs::write(&marker, b"malformed marker is still uncertainty").unwrap();
    let before = raw(d.path());
    let query = format!("state={state}&code=owned-code");
    assert!(
        super::super::complete_callback_query(d.path(), &map, &query)
            .await
            .is_err()
    );
    assert_eq!(wire.requests(), 0);
    assert_eq!(raw(d.path()), before);
    assert!(map.read().await.is_empty());
    assert_eq!(
        std::fs::read(marker).unwrap(),
        b"malformed marker is still uncertainty"
    );
}
