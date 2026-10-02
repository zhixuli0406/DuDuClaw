use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

const DISABLED: &str = "[account_loading]\ninherit_host_credentials = false\n";
const CHILD_CASE: &str = "DUDUCLAW_HOST_INHERITANCE_TEST_CASE";

fn detected_account() -> Account {
    let mut account: Account = serde_json::from_value(serde_json::json!({
        "id": "detected-test-session", "auth_method": AuthMethod::OAuth,
        "priority": 1, "monthly_budget_cents": 0,
    })).unwrap();
    account.is_healthy = true;
    account.oauth_token = Some("test-session-token".into());
    account
}

#[tokio::test]
async fn disabled_inheritance_does_not_invoke_host_detector() {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("config.toml"), DISABLED).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    let detector_calls = calls.clone();
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    let count = rotator.load_from_config_using(home.path(), move || {
        detector_calls.fetch_add(1, Ordering::SeqCst);
        Some(detected_account())
    }).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "host detector must not run");
    assert_eq!(count, 0);
}

#[tokio::test]
async fn absent_and_enabled_policy_preserve_host_detection() {
    for config in ["", "[account_loading]\ninherit_host_credentials = true\n"] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), config).unwrap();
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        assert_eq!(rotator.load_from_config_using(home.path(), || Some(detected_account())).await.unwrap(), 1);
        assert_eq!(rotator.select().await.unwrap().id, "detected-test-session");
    }
}

#[tokio::test]
async fn disabled_inheritance_keeps_explicit_accounts_and_api_config() {
    for (entry, id, provider) in [
        ("[[accounts]]\nid = 'explicit'\ntype = 'api_key'\nprovider = 'openai'\napi_key = 'test-explicit-key'\n", "explicit", "openai"),
        ("[api]\nanthropic_api_key = 'test-explicit-key'\n", "main", "anthropic"),
    ] {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("config.toml"), format!("{DISABLED}\n{entry}")).unwrap();
        let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
        rotator.load_from_config_using(home.path(), || None).await.unwrap();
        let selected = rotator.select_for_provider(provider).await.unwrap();
        assert_eq!(selected.id, id);
        assert_eq!(selected.raw_key.as_deref(), Some("test-explicit-key"));
    }
}

#[tokio::test]
async fn disabled_inheritance_keeps_encrypted_explicit_account() {
    use duduclaw_security::crypto::CryptoEngine;
    let home = tempfile::tempdir().unwrap();
    let key = CryptoEngine::generate_key().unwrap();
    std::fs::write(home.path().join(".keyfile"), key).unwrap();
    let encrypted = CryptoEngine::new(&key).unwrap().encrypt_string("test-explicit-key").unwrap();
    std::fs::write(home.path().join("config.toml"), format!("{DISABLED}\n[[accounts]]\nid = 'explicit'\ntype = 'api_key'\nprovider = 'openai'\napi_key_enc = '{encrypted}'\n")).unwrap();
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    rotator.load_from_config_using(home.path(), || None).await.unwrap();
    assert_eq!(rotator.select_for_provider("openai").await.unwrap().raw_key.as_deref(), Some("test-explicit-key"));
}

#[tokio::test]
async fn invalid_policy_toml_and_read_errors_fail_closed() {
    let invalid = [
        "[account_loading]\ninherit_host_credentials = 'false'\n",
        "account_loading = false\n",
        "[account_loading]\ninherit_host_credentials = false\n[api]\nanthropic_api_key = 'test-secret-in-source'\n[bad",
    ];
    let home = tempfile::tempdir().unwrap();
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    for content in invalid {
        // A failed reload must remove previously inherited credentials too.
        std::fs::write(home.path().join("config.toml"), "").unwrap();
        rotator.load_from_config_using(home.path(), || Some(detected_account())).await.unwrap();
        std::fs::write(home.path().join("config.toml"), content).unwrap();
        let error = rotator.load_from_config_using(home.path(), || None).await.expect_err("invalid config must reject the reload");
        assert!(!error.contains("test-secret-in-source"), "parse diagnostics must not quote secret-bearing source");
        assert_eq!(rotator.count().await, 0);
        assert!(rotator.select().await.is_none());
    }
    std::fs::remove_file(home.path().join("config.toml")).unwrap();
    std::fs::create_dir(home.path().join("config.toml")).unwrap();
    assert!(rotator.load_from_config_using(home.path(), || None).await.is_err());
}

#[tokio::test]
async fn missing_config_preserves_default_detection() {
    let home = tempfile::tempdir().unwrap();
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    assert_eq!(rotator.load_from_config_using(home.path(), || Some(detected_account())).await.unwrap(), 1);
}

fn run_ambient_child(case: &str) {
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "account_rotator::host_inheritance_tests::ambient_env_case_child", "--nocapture"])
        .env(CHILD_CASE, case)
        // Only child processes receive controlled test credentials. The
        // parallel test runner's environment and real secrets stay untouched.
        .env("ANTHROPIC_API_KEY", "test-ambient-anthropic")
        .env("OPENAI_API_KEY", "test-ambient-openai")
        .env("GEMINI_API_KEY", "test-ambient-gemini")
        .env("GOOGLE_API_KEY", "test-ambient-google-alias")
        .env("DUDUCLAW_EXPLICIT_SECRET_TEST", "test-explicit-secret-ref")
        .output().unwrap();
    assert!(output.status.success(), "child case {case} failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
}

#[test]
fn disabled_inheritance_blocks_all_selection_env_fallbacks() { run_ambient_child("disabled"); }

#[test]
fn explicit_env_secret_reference_remains_authorized() { run_ambient_child("explicit-reference"); }

#[test]
fn reload_from_enabled_to_disabled_removes_inherited_accounts() { run_ambient_child("reload"); }

#[test]
fn enabled_inheritance_preserves_ambient_fallbacks() { run_ambient_child("enabled"); }

#[tokio::test]
async fn ambient_env_case_child() {
    let Ok(case) = std::env::var(CHILD_CASE) else { return; };
    let home = tempfile::tempdir().unwrap();
    let rotator = AccountRotator::new(RotationStrategy::Priority, 120);
    match case.as_str() {
        "enabled" => {
            std::fs::write(home.path().join("config.toml"), "").unwrap();
            rotator.load_from_config_using(home.path(), || None).await.unwrap();
            assert_eq!(rotator.select().await.unwrap().id, "env");
            assert_eq!(rotator.select_for_provider("openai").await.unwrap().id, "openai-env");
            assert_eq!(rotator.select_for_provider("gemini").await.unwrap().id, "gemini-env");
        }
        "explicit-reference" => {
            std::fs::write(home.path().join("config.toml"), format!("{DISABLED}\n[[accounts]]\nid = 'reference'\ntype = 'api_key'\nprovider = 'openai'\napi_key = 'secret://env/DUDUCLAW_EXPLICIT_SECRET_TEST'\n")).unwrap();
            rotator.load_from_config_using(home.path(), || None).await.unwrap();
            assert_eq!(rotator.select_for_provider("openai").await.unwrap().raw_key.as_deref(), Some("test-explicit-secret-ref"));
        }
        "disabled" | "reload" => {
            if case == "reload" {
                std::fs::write(home.path().join("config.toml"), "").unwrap();
                rotator.load_from_config_using(home.path(), || Some(detected_account())).await.unwrap();
                assert!(rotator.select().await.is_some());
            }
            std::fs::write(home.path().join("config.toml"), DISABLED).unwrap();
            assert_eq!(rotator.load_from_config_using(home.path(), || None).await.unwrap(), 0);
            assert!(rotator.select().await.is_none());
            assert!(rotator.select_with_pool(&["stale".into()]).await.is_none());
            for provider in ["anthropic", "openai", "gemini"] {
                assert!(rotator.select_for_provider(provider).await.is_none(), "ambient fallback for {provider}");
                assert!(rotator.select_for_provider_with_pool(provider, &["stale".into()]).await.is_none());
            }
            // Also cover callers constructing the rotator directly from a
            // parsed config before a load/reload takes place.
            let configured = create_from_config(&DISABLED.parse().unwrap());
            assert!(configured.select_for_provider("openai").await.is_none());
        }
        _ => panic!("unknown child case"),
    }
}
