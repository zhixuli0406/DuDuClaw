//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

/// WP-6C — regression coverage for `handle_accounts_add`'s WP-4A behaviour
/// change (encrypt succeeds ⇒ write only `_enc`; encrypt fails ⇒ degrade to
/// plaintext + warn), deferred at the time because it needed a full
/// `MethodHandler` fixture. Per WP-6C's stated priority, the write-shape
/// decision is extracted into the pure [`build_account_entry`] and covered
/// with fast no-I/O unit tests first; one lightweight `MethodHandler`-based
/// integration test (the fixture turned out cheap — `MethodHandler::new` is
/// already the standard pattern used throughout this file) covers the
/// remaining end-to-end property that a pure function can't: an existing
/// account surviving a second `accounts.add` untouched.
use super::*;

// ── build_account_entry: pure, no I/O ───────────────────────

#[test]
fn encryption_success_writes_only_the_enc_field() {
    let entry = build_account_entry(
        "acct-1",
        "api_key",
        "anthropic",
        5000,
        1,
        "sk-plain-key",
        Some("cipher-blob"),
    );
    assert!(!entry.plaintext_fallback);
    assert_eq!(entry.key_field, "anthropic_api_key");
    assert_eq!(
        entry
            .table
            .get("anthropic_api_key_enc")
            .and_then(|v| v.as_str()),
        Some("cipher-blob")
    );
    assert!(
        entry.table.get("anthropic_api_key").is_none(),
        "plaintext twin must not be written when encryption succeeded: {:?}",
        entry.table
    );
    assert_eq!(
        entry.table.get("id").and_then(|v| v.as_str()),
        Some("acct-1")
    );
    assert_eq!(
        entry.table.get("provider").and_then(|v| v.as_str()),
        Some("anthropic")
    );
    assert_eq!(
        entry
            .table
            .get("monthly_budget_cents")
            .and_then(|v| v.as_integer()),
        Some(5000)
    );
    assert_eq!(
        entry.table.get("priority").and_then(|v| v.as_integer()),
        Some(1)
    );
}

#[test]
fn encryption_failure_falls_back_to_plaintext_and_flags_it() {
    let entry = build_account_entry(
        "acct-2",
        "api_key",
        "anthropic",
        5000,
        1,
        "sk-plain-key",
        None,
    );
    assert!(entry.plaintext_fallback, "caller must be told to warn");
    assert_eq!(entry.key_field, "anthropic_api_key");
    assert_eq!(
        entry
            .table
            .get("anthropic_api_key")
            .and_then(|v| v.as_str()),
        Some("sk-plain-key")
    );
    assert!(
        entry.table.get("anthropic_api_key_enc").is_none(),
        "no _enc twin when encryption never ran: {:?}",
        entry.table
    );
}

#[test]
fn oauth_type_uses_the_oauth_token_field_name() {
    let ok = build_account_entry(
        "acct-oauth",
        "oauth",
        "anthropic",
        0,
        1,
        "oauth-secret",
        Some("cipher"),
    );
    assert_eq!(ok.key_field, "oauth_token");
    assert_eq!(
        ok.table.get("oauth_token_enc").and_then(|v| v.as_str()),
        Some("cipher")
    );
    assert!(ok.table.get("anthropic_api_key_enc").is_none());

    let fallback = build_account_entry(
        "acct-oauth-2",
        "oauth",
        "anthropic",
        0,
        1,
        "oauth-secret",
        None,
    );
    assert_eq!(
        fallback.table.get("oauth_token").and_then(|v| v.as_str()),
        Some("oauth-secret")
    );
}

#[test]
fn every_non_credential_field_is_present_regardless_of_encryption_outcome() {
    for encrypted in [Some("cipher"), None] {
        let entry =
            build_account_entry("acct-x", "api_key", "anthropic", 12345, 7, "key", encrypted);
        assert_eq!(
            entry.table.get("id").and_then(|v| v.as_str()),
            Some("acct-x")
        );
        assert_eq!(
            entry.table.get("type").and_then(|v| v.as_str()),
            Some("api_key")
        );
        assert_eq!(
            entry.table.get("provider").and_then(|v| v.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            entry
                .table
                .get("monthly_budget_cents")
                .and_then(|v| v.as_integer()),
            Some(12345)
        );
        assert_eq!(
            entry.table.get("priority").and_then(|v| v.as_integer()),
            Some(7)
        );
    }
}

// ── WP-A: provider-aware key field name ─────────────────────

/// A non-Anthropic provider writes the provider-agnostic `api_key(_enc)`
/// field name — never `anthropic_api_key*`, which stays reserved for
/// provider `"anthropic"` — and always carries its own `provider` value,
/// which `duduclaw-agent::account_rotator::load_from_config` reads to
/// route the account into that provider's rotation pool.
#[test]
fn openai_provider_writes_the_api_key_field_and_provider_value() {
    let entry = build_account_entry(
        "acct-openai",
        "api_key",
        "openai",
        5000,
        1,
        "sk-openai-key",
        Some("cipher-blob"),
    );
    assert_eq!(entry.key_field, "api_key");
    assert_eq!(
        entry.table.get("api_key_enc").and_then(|v| v.as_str()),
        Some("cipher-blob")
    );
    assert!(entry.table.get("anthropic_api_key_enc").is_none());
    assert!(entry.table.get("anthropic_api_key").is_none());
    assert_eq!(
        entry.table.get("provider").and_then(|v| v.as_str()),
        Some("openai")
    );
}

/// Same shape, plaintext fallback path (no writable keyfile).
#[test]
fn deepseek_provider_plaintext_fallback_uses_api_key_field() {
    let entry =
        build_account_entry("acct-ds", "api_key", "deepseek", 5000, 1, "sk-ds-key", None);
    assert!(entry.plaintext_fallback);
    assert_eq!(entry.key_field, "api_key");
    assert_eq!(
        entry.table.get("api_key").and_then(|v| v.as_str()),
        Some("sk-ds-key")
    );
    assert_eq!(
        entry.table.get("provider").and_then(|v| v.as_str()),
        Some("deepseek")
    );
}

/// An `oauth`-type account keeps writing `oauth_token(_enc)` regardless of
/// provider — that field name is shared across providers (see
/// `resolve_oauth_token`), only the provider-key API path branches on
/// provider.
#[test]
fn oauth_type_key_field_is_provider_independent() {
    let entry = build_account_entry(
        "acct-oauth-openai",
        "oauth",
        "openai",
        0,
        1,
        "oauth-secret",
        Some("cipher"),
    );
    assert_eq!(entry.key_field, "oauth_token");
    assert_eq!(
        entry.table.get("provider").and_then(|v| v.as_str()),
        Some("openai")
    );
}

// ── handle_accounts_add: end-to-end, MethodHandler fixture ──

/// Minimal fixed-response HTTP server for the D6 write-time probe.
///
/// Mirrors `duduclaw_agent::credential_probe`'s own test harness: one
/// connection, one canned response, no HTTP-server dependency. Returns the
/// base URL to hand `handle_accounts_add_with_probe_base`.
async fn probe_server(response: &'static str) -> (String, tokio::task::JoinHandle<()>) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let handle = tokio::spawn(async move {
        if let Ok((mut sock, _)) = listener.accept().await {
            let mut buf = vec![0u8; 4096];
            let _ = sock.read(&mut buf).await;
            let _ = sock.write_all(response.as_bytes()).await;
            let _ = sock.flush().await;
        }
    });
    (format!("http://{addr}"), handle)
}

const PROBE_200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
const PROBE_401: &str =
    "HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const PROBE_403: &str =
    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Add an account through the RPC with the D6 probe pointed at a local
/// listener that answers 200 — i.e. "the credential authenticates", which
/// is what every pre-D6 caller of this helper implicitly assumed.
async fn added_account(home: &std::path::Path, id: &str) -> WsFrame {
    let handler = MethodHandler::new(home.to_path_buf()).await;
    let (base, server) = probe_server(PROBE_200).await;
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": id,
                "type": "api_key",
                "key": format!("sk-{id}-secret"),
                "priority": 1,
                "monthly_budget_cents": 5000,
            }),
            &base,
        )
        .await;
    let _ = server.await;
    res
}

#[tokio::test]
async fn accounts_add_round_trip_writes_enc_only() {
    let home = tempfile::tempdir().unwrap();
    let res = added_account(home.path(), "acct-a").await;
    assert!(matches!(res, WsFrame::Response { ok: true, .. }), "{res:?}");

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table: toml::Table = raw.parse().unwrap();
    let accounts = table["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1);
    let acct = accounts[0].as_table().unwrap();
    assert_eq!(acct.get("id").and_then(|v| v.as_str()), Some("acct-a"));
    assert!(
        acct.get("anthropic_api_key_enc")
            .and_then(|v| v.as_str())
            .is_some()
    );
    assert!(
        acct.get("anthropic_api_key").is_none(),
        "plaintext credential must not be persisted alongside a successful \
             encryption: {acct:?}"
    );
}

/// The property a pure-function test can't reach: adding a second account
/// must not perturb the first one's entry at all.
#[tokio::test]
async fn existing_accounts_are_unaffected_by_a_new_add() {
    let home = tempfile::tempdir().unwrap();
    assert!(matches!(
        added_account(home.path(), "acct-a").await,
        WsFrame::Response { ok: true, .. }
    ));
    let raw_after_first = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table_after_first: toml::Table = raw_after_first.parse().unwrap();
    let first_entry = table_after_first["accounts"].as_array().unwrap()[0].clone();

    assert!(matches!(
        added_account(home.path(), "acct-b").await,
        WsFrame::Response { ok: true, .. }
    ));
    let raw_after_second = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table_after_second: toml::Table = raw_after_second.parse().unwrap();
    let accounts = table_after_second["accounts"].as_array().unwrap();
    assert_eq!(
        accounts.len(),
        2,
        "both accounts must be present: {accounts:?}"
    );
    assert_eq!(
        accounts[0], first_entry,
        "the first account's entry must be byte-for-byte unchanged after a \
             second accounts.add: before={first_entry:?} after={:?}",
        accounts[0]
    );
    assert_eq!(
        accounts[1]
            .as_table()
            .unwrap()
            .get("id")
            .and_then(|v| v.as_str()),
        Some("acct-b")
    );
}

#[tokio::test]
async fn duplicate_id_is_rejected_without_touching_the_existing_entry() {
    let home = tempfile::tempdir().unwrap();
    assert!(matches!(
        added_account(home.path(), "dup-id").await,
        WsFrame::Response { ok: true, .. }
    ));
    let raw_before = std::fs::read_to_string(home.path().join("config.toml")).unwrap();

    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle_accounts_add(json!({
            "id": "dup-id",
            "type": "api_key",
            "key": "sk-second-attempt",
            "priority": 1,
            "monthly_budget_cents": 5000,
        }))
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );

    let raw_after = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert_eq!(
        raw_before, raw_after,
        "a rejected duplicate must not touch config.toml"
    );
}

// ── WP-A: `accounts.add` provider param, end-to-end via the RPC ─────

fn frame_payload(f: &WsFrame) -> Value {
    match f {
        WsFrame::Response {
            payload: Some(p), ..
        } => p.clone(),
        other => panic!("expected a payload: {other:?}"),
    }
}

/// No `provider` param ⇒ defaults to `"anthropic"` and keeps writing the
/// legacy `anthropic_api_key*` field name — every pre-WP-A dashboard/OOBE
/// caller must keep working byte-for-byte.
#[tokio::test]
async fn accounts_add_without_provider_defaults_to_anthropic() {
    let home = tempfile::tempdir().unwrap();
    let res = added_account(home.path(), "acct-default").await;
    assert_eq!(frame_payload(&res)["provider"], json!("anthropic"));

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table: toml::Table = raw.parse().unwrap();
    let acct = table["accounts"].as_array().unwrap()[0].as_table().unwrap();
    assert_eq!(
        acct.get("provider").and_then(|v| v.as_str()),
        Some("anthropic")
    );
    assert!(acct.get("anthropic_api_key_enc").is_some());
}

/// An explicit non-Anthropic `provider` is validated, written into the
/// entry, and the credential lands under the provider-agnostic `api_key*`
/// field name (not `anthropic_api_key*`) — the exact shape
/// `duduclaw-agent::account_rotator::load_from_config` /
/// `select_for_provider` reads back for Direct-API routing.
#[tokio::test]
async fn accounts_add_with_openai_provider_writes_api_key_field() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle_accounts_add(json!({
            "id": "acct-openai",
            "type": "api_key",
            "provider": "openai",
            "key": "sk-openai-secret",
            "priority": 1,
            "monthly_budget_cents": 5000,
        }))
        .await;
    assert!(matches!(res, WsFrame::Response { ok: true, .. }), "{res:?}");
    assert_eq!(frame_payload(&res)["provider"], json!("openai"));

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table: toml::Table = raw.parse().unwrap();
    let acct = table["accounts"].as_array().unwrap()[0].as_table().unwrap();
    assert_eq!(
        acct.get("provider").and_then(|v| v.as_str()),
        Some("openai")
    );
    assert!(
        acct.get("api_key_enc").and_then(|v| v.as_str()).is_some(),
        "openai key must land under `api_key_enc`, not `anthropic_api_key_enc`: {acct:?}"
    );
    assert!(acct.get("anthropic_api_key_enc").is_none());
    assert!(acct.get("anthropic_api_key").is_none());
}

/// An unknown provider id is rejected fail-closed, and the rejected call
/// must not touch config.toml at all.
#[tokio::test]
async fn accounts_add_rejects_unknown_provider() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle_accounts_add(json!({
            "id": "acct-bogus",
            "type": "api_key",
            "provider": "totally-bogus-vendor",
            "key": "sk-whatever",
            "priority": 1,
            "monthly_budget_cents": 5000,
        }))
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );
    assert!(!home.path().join("config.toml").exists());
}

/// An unknown `type` is rejected the same way — only `api_key`/`oauth`
/// are meaningful to `resolve_api_key`/`resolve_oauth_token`.
#[tokio::test]
async fn accounts_add_rejects_unknown_type() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let res = handler
        .handle_accounts_add(json!({
            "id": "acct-bad-type",
            "type": "bearer_token",
            "key": "sk-whatever",
            "priority": 1,
            "monthly_budget_cents": 5000,
        }))
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );
}

/// `accounts.list` surfaces `provider` for every account, matching what
/// was written by `accounts.add` (WP-A requirement: dashboard needs this
/// to render/filter the provider column). No I/O / no rotator cache —
/// exercises the pure `account_status_to_json` mapping directly so this
/// stays deterministic under parallel `cargo test` (the rotator cache
/// `handle_accounts_list` reads through is a single process-global slot
/// keyed by nothing, so driving this via the RPC + a real config.toml
/// would race against every other test that ever touches it).
#[test]
fn accounts_list_row_surfaces_provider() {
    let status = duduclaw_agent::account_rotator::AccountStatus {
        id: "acct-gemini".to_string(),
        auth_method: "api_key".to_string(),
        provider: "gemini".to_string(),
        priority: 1,
        is_healthy: true,
        spent_this_month: 0,
        monthly_budget_cents: 5000,
        total_requests: 0,
        is_available: true,
        email: String::new(),
        subscription: String::new(),
        label: String::new(),
        expires_at: None,
        days_until_expiry: None,
        credential_state: Default::default(),
        credential_detail: None,
        auth_dead_strikes: 0,
        next_probe_at: None,
        probe_failures: 0,
    };
    let row = account_status_to_json(&status);
    assert_eq!(row["provider"], json!("gemini"));
    assert_eq!(row["id"], json!("acct-gemini"));

    // D5: the credential badge fields must reach the dashboard. The
    // default state is the honest "we have it, we've never seen it work".
    assert_eq!(row["credential_state"], json!("unverified"));
    assert_eq!(row["credential_detail"], json!(null));

    // Probe schedule: a never-probed account is due on the next tick.
    assert_eq!(row["next_probe_at"], json!(null));
    assert_eq!(row["probe_failures"], json!(0));
}

/// D5 — an auth-dead row must carry the *kind* (`auth_dead:org_disabled`),
/// not `CredentialState`'s flat serde token (`auth_dead`). The dashboard
/// badge and its zh-TW hint both key off this: "re-issue your token" and
/// "ask your org admin" are different actions.
#[test]
fn accounts_list_row_surfaces_auth_dead_kind_and_detail() {
    use duduclaw_agent::account_rotator::{AuthFailureKind, CredentialState};

    let status = duduclaw_agent::account_rotator::AccountStatus {
        id: "acct-org".to_string(),
        credential_state: CredentialState::AuthDead(AuthFailureKind::OrgDisabled),
        credential_detail: CredentialState::AuthDead(AuthFailureKind::OrgDisabled)
            .credential_detail(),
        auth_dead_strikes: 2,
        ..Default::default()
    };
    let row = account_status_to_json(&status);
    assert_eq!(
        row["credential_state"],
        json!("auth_dead:org_disabled"),
        "the Display form (with the kind) must win over the serde form: {row}"
    );
    let detail = row["credential_detail"].as_str().expect("detail present");
    assert!(
        detail.contains("403"),
        "operator-facing detail should name the failure: {detail}"
    );

    // …and the invalid-token variant must be distinguishable from it.
    let status = duduclaw_agent::account_rotator::AccountStatus {
        id: "acct-tok".to_string(),
        credential_state: CredentialState::AuthDead(AuthFailureKind::InvalidToken),
        credential_detail: CredentialState::AuthDead(AuthFailureKind::InvalidToken)
            .credential_detail(),
        ..Default::default()
    };
    assert_eq!(
        account_status_to_json(&status)["credential_state"],
        json!("auth_dead:invalid_token")
    );
}

// ── D6: write-time credential verification ─────────────────────────

/// The user-facing error string of a rejected frame (empty when the frame
/// carries no error, which every caller below asserts against).
fn frame_error_message(f: &WsFrame) -> String {
    match f {
        WsFrame::Response { error: Some(e), .. } => e
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| e.to_string()),
        _ => String::new(),
    }
}

/// A short-lived `sk-ant-at01-` access token is refused on shape alone —
/// BEFORE any network call. The base URL below is unroutable on purpose:
/// if the handler probed, this test would hang for the probe timeout and
/// then (wrongly) save the account.
#[tokio::test]
async fn accounts_add_rejects_short_lived_access_token_without_a_probe() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let started = std::time::Instant::now();
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-at01",
                "type": "oauth",
                "key": "sk-ant-at01-short-lived",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            "http://192.0.2.1:9",
        )
        .await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "the at01 rejection must not pay for a network probe"
    );
    let msg = frame_error_message(&res);
    assert!(
        msg.contains("setup-token"),
        "message must name the fix: {msg}"
    );
    assert!(
        !home.path().join("config.toml").exists(),
        "a rejected credential must never be persisted"
    );
}

/// 401 ⇒ the account is refused, not saved-and-broken.
#[tokio::test]
async fn accounts_add_rejects_401_credential() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let (base, server) = probe_server(PROBE_401).await;
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-401",
                "type": "oauth",
                "key": "sk-ant-oat01-revoked",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            &base,
        )
        .await;
    let _ = server.await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );
    assert!(!home.path().join("config.toml").exists());
}

/// 403 ⇒ refused with the org-specific message (the 2026-09-08 incident
/// shape: the token is fine, the organization disabled the access path).
#[tokio::test]
async fn accounts_add_rejects_403_org_disabled() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let (base, server) = probe_server(PROBE_403).await;
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-403",
                "type": "oauth",
                "key": "sk-ant-oat01-org-disabled",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            &base,
        )
        .await;
    let _ = server.await;
    assert!(
        matches!(res, WsFrame::Response { ok: false, .. }),
        "{res:?}"
    );
    let msg = frame_error_message(&res);
    assert!(msg.contains("403"), "message must name the 403: {msg}");
    assert!(!home.path().join("config.toml").exists());
}

/// 200 ⇒ saved, and the response says so.
#[tokio::test]
async fn accounts_add_saves_verified_true_on_200() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let (base, server) = probe_server(PROBE_200).await;
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-ok",
                "type": "oauth",
                "key": "sk-ant-oat01-good",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            &base,
        )
        .await;
    let _ = server.await;
    assert!(matches!(res, WsFrame::Response { ok: true, .. }), "{res:?}");
    assert_eq!(frame_payload(&res)["verified"], json!(true));

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    let table: toml::Table = raw.parse().unwrap();
    let acct = table["accounts"].as_array().unwrap()[0].as_table().unwrap();
    assert_eq!(acct.get("id").and_then(|v| v.as_str()), Some("acct-ok"));
}

/// An unreachable probe (offline install, DNS down, a listener that
/// closes without answering) must NOT block configuration — the account is
/// saved with an honest `verified: false`.
#[tokio::test]
async fn accounts_add_saves_unverified_when_the_probe_cannot_answer() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;

    // A listener that accepts and immediately closes: a transport failure,
    // which `CredentialProbe` classifies as `Unknown` — inconclusive.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        if let Ok((sock, _)) = listener.accept().await {
            drop(sock);
        }
    });

    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-offline",
                "type": "oauth",
                "key": "sk-ant-oat01-unknown",
                "priority": 1,
                "monthly_budget_cents": 0,
            }),
            &format!("http://{addr}"),
        )
        .await;
    let _ = server.await;
    assert!(matches!(res, WsFrame::Response { ok: true, .. }), "{res:?}");
    assert_eq!(
        frame_payload(&res)["verified"],
        json!(false),
        "an unverifiable credential must be saved, but never claimed verified"
    );
    assert!(home.path().join("config.toml").exists());
}

/// A non-Anthropic provider has no probe endpoint here (D8): saved,
/// `verified: null`, and byte-identical to the pre-D6 behavior — in
/// particular it must never reach the network. The base URL is unroutable
/// on purpose.
#[tokio::test]
async fn accounts_add_does_not_probe_foreign_providers() {
    let home = tempfile::tempdir().unwrap();
    let handler = MethodHandler::new(home.path().to_path_buf()).await;
    let started = std::time::Instant::now();
    let res = handler
        .handle_accounts_add_with_probe_base(
            json!({
                "id": "acct-deepseek",
                "type": "api_key",
                "provider": "deepseek",
                "key": "sk-deepseek-secret",
                "priority": 1,
                "monthly_budget_cents": 5000,
            }),
            "http://192.0.2.1:9",
        )
        .await;
    assert!(matches!(res, WsFrame::Response { ok: true, .. }), "{res:?}");
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(frame_payload(&res)["verified"], json!(null));
}
