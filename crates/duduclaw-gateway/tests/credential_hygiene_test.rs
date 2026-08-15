//! WP-K — dashboard "credential hygiene" friendly cleanup surface.
//!
//! `security.credential_hygiene` (detect) and `security.credential_cleanup`
//! (clean up ONLY plaintext fields that already have a confirmed `_enc`
//! twin) drive the Settings → System "憑證衛生" card. These are integration
//! tests (own process, tempdir home) because they exercise real file I/O —
//! the timestamped backup, the atomic locked rewrite, and the audit log —
//! which the pure-logic unit tests in `security_posture.rs` do not cover.

use duduclaw_auth::UserContext;
use duduclaw_gateway::handlers::MethodHandler;
use duduclaw_gateway::protocol::WsFrame;
use serde_json::{json, Value};

fn payload(frame: &WsFrame) -> Value {
    match frame {
        WsFrame::Response {
            ok: true,
            payload: Some(p),
            ..
        } => p.clone(),
        other => panic!("expected an ok response, got {other:?}"),
    }
}

async fn handler(home: &std::path::Path) -> MethodHandler {
    MethodHandler::new(home.to_path_buf()).await
}

/// The real-world shape of the 2026-08-15 incident: a plaintext `oauth_token`
/// left sitting next to its already-authoritative `oauth_token_enc` twin
/// inside `[[accounts]]` (an array-of-tables — the exact structure that used
/// to slip past `system.config`'s masking).
const TWIN_RESIDUE_CONFIG: &str = r#"
[[accounts]]
id = "a1"
oauth_token = "sk-ant-oat01-PLAINTEXTLEAK123"
oauth_token_enc = "Y2lwaGVydGV4dA=="
"#;

// ── Detection: clean / twin-residue / no-twin three states ─────────────────

#[tokio::test]
async fn hygiene_detects_twin_residue() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), TWIN_RESIDUE_CONFIG).unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    let p = payload(&frame);
    assert_eq!(p["clean"], json!(false));
    assert_eq!(p["count"], json!(1));
    assert_eq!(p["findings"][0]["path"], json!("accounts[0].oauth_token"));
    assert_eq!(p["findings"][0]["has_enc_twin"], json!(true));
    assert_eq!(p["findings"][0]["severity"], json!("high"));
}

#[tokio::test]
async fn hygiene_reports_no_twin_field_without_marking_it_cleanable() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[providers]\nanthropic_api_key = \"sk-ant-api03-nolongertwin\"\n",
    )
    .unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    let p = payload(&frame);
    assert_eq!(p["clean"], json!(false));
    assert_eq!(p["findings"][0]["has_enc_twin"], json!(false));

    // Cleanup must be a true no-op — no twin, nothing this pass may touch.
    let frame2 = h
        .handle("security.credential_cleanup", json!({}), &ctx)
        .await;
    let p2 = payload(&frame2);
    assert_eq!(p2["cleaned"], json!(false));
    assert_eq!(p2["removed_paths"], json!(Vec::<String>::new()));

    let raw = std::fs::read_to_string(home.path().join("config.toml")).unwrap();
    assert!(
        raw.contains("sk-ant-api03-nolongertwin"),
        "no-twin field must survive cleanup untouched"
    );
}

#[tokio::test]
async fn hygiene_clean_config_reports_green() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(
        home.path().join("config.toml"),
        "[[accounts]]\nid = \"a1\"\noauth_token_enc = \"Y2lwaGVydGV4dA==\"\n",
    )
    .unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    let p = payload(&frame);
    assert_eq!(p["clean"], json!(true));
    assert_eq!(p["count"], json!(0));
}

/// No config.toml at all (fresh install) must read as clean, not an error —
/// "nothing configured yet" is legitimately safe, distinct from "config
/// exists but is corrupt" (see `hygiene_fails_closed_on_malformed_config`).
#[tokio::test]
async fn hygiene_missing_config_is_clean_not_an_error() {
    let home = tempfile::tempdir().unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    let p = payload(&frame);
    assert_eq!(p["clean"], json!(true));
}

// ── Cleanup: backup, removal, _enc preservation, idempotency ───────────────

#[tokio::test]
async fn cleanup_backs_up_removes_plaintext_keeps_enc_and_is_idempotent() {
    let home = tempfile::tempdir().unwrap();
    let config_path = home.path().join("config.toml");
    std::fs::write(&config_path, TWIN_RESIDUE_CONFIG).unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_cleanup", json!({}), &ctx)
        .await;
    let p = payload(&frame);
    assert_eq!(p["cleaned"], json!(true));
    assert_eq!(p["removed_paths"], json!(["accounts[0].oauth_token"]));

    // A timestamped backup exists and holds the ORIGINAL plaintext — proof
    // the backup was taken before mutation, not after.
    let backup_path_str = p["backup_path"].as_str().unwrap().to_string();
    let backup_content = std::fs::read_to_string(&backup_path_str).unwrap();
    assert!(backup_content.contains("sk-ant-oat01-PLAINTEXTLEAK123"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&backup_path_str).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "backup must be 0600, got {mode:o}");
    }

    // The live config no longer has the plaintext, but keeps the _enc twin.
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert!(
        !after.contains("sk-ant-oat01-PLAINTEXTLEAK123"),
        "plaintext must be gone: {after}"
    );
    assert!(
        after.contains("oauth_token_enc"),
        "enc twin must survive: {after}"
    );

    // Idempotent: nothing left to clean on a second call, and no second
    // backup is created for a true no-op.
    let backups_before = count_backups(home.path());
    let frame2 = h
        .handle("security.credential_cleanup", json!({}), &ctx)
        .await;
    let p2 = payload(&frame2);
    assert_eq!(p2["cleaned"], json!(false));
    let backups_after = count_backups(home.path());
    assert_eq!(
        backups_before, backups_after,
        "idempotent cleanup must not create a second backup"
    );
}

fn count_backups(home: &std::path::Path) -> usize {
    std::fs::read_dir(home)
        .unwrap()
        .filter(|e| {
            e.as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("config.toml.bak.")
        })
        .count()
}

// ── The RPC contract itself must never leak the secret value ───────────────

#[tokio::test]
async fn rpc_never_leaks_the_plaintext_value() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), TWIN_RESIDUE_CONFIG).unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    let serialized = serde_json::to_string(&frame).unwrap();
    assert!(
        !serialized.contains("PLAINTEXTLEAK123"),
        "detection RPC must never echo the secret value: {serialized}"
    );

    let cleanup_frame = h
        .handle("security.credential_cleanup", json!({}), &ctx)
        .await;
    let cleanup_serialized = serde_json::to_string(&cleanup_frame).unwrap();
    assert!(
        !cleanup_serialized.contains("PLAINTEXTLEAK123"),
        "cleanup RPC must never echo the secret value: {cleanup_serialized}"
    );
}

// ── Fail-closed: a malformed config.toml is an error, never a false green ──

#[tokio::test]
async fn hygiene_fails_closed_on_malformed_config() {
    let home = tempfile::tempdir().unwrap();
    // Unterminated string literal — deliberately invalid TOML.
    std::fs::write(
        home.path().join("config.toml"),
        "oauth_token = \"unterminated",
    )
    .unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_hygiene", json!({}), &ctx)
        .await;
    match frame {
        WsFrame::Response { ok: false, .. } => {}
        other => panic!("malformed config.toml must fail closed, got {other:?}"),
    }
}

#[tokio::test]
async fn cleanup_fails_closed_on_malformed_config_and_never_overwrites_it() {
    let home = tempfile::tempdir().unwrap();
    let config_path = home.path().join("config.toml");
    let malformed = "oauth_token = \"unterminated";
    std::fs::write(&config_path, malformed).unwrap();
    let h = handler(home.path()).await;
    let ctx = UserContext::admin_fallback();

    let frame = h
        .handle("security.credential_cleanup", json!({}), &ctx)
        .await;
    match frame {
        WsFrame::Response { ok: false, .. } => {}
        other => panic!("malformed config.toml must fail closed, got {other:?}"),
    }
    // The corrupt file must be left exactly as-is — never silently replaced
    // with an empty table.
    let after = std::fs::read_to_string(&config_path).unwrap();
    assert_eq!(after, malformed);
}
