//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::*;

// No test here mutates `ANTHROPIC_API_KEY` (a process-global env var read
// by other tests too, potentially concurrently) — every assertion below
// is monotonic: it only checks a path that returns `true` regardless of
// whatever the ambient env var happens to be in this test run (env-var
// present would also yield `true`, never `false`), so there is no
// ordering dependency to get flaky.

#[tokio::test]
async fn true_when_config_toml_api_key_set() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[api]\nanthropic_api_key = \"sk-test-123\"\n",
    )
    .unwrap();
    assert!(has_api_key_configured(dir.path()).await);
}

#[tokio::test]
async fn true_when_accounts_array_non_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("config.toml"),
        "[[accounts]]\nid = \"a1\"\ntype = \"oauth\"\n",
    )
    .unwrap();
    assert!(has_api_key_configured(dir.path()).await);
}

#[tokio::test]
async fn missing_config_toml_does_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    // No config.toml at all — must degrade gracefully (never panic);
    // the boolean result itself depends on the ambient env, so it is
    // deliberately not asserted here.
    let _ = has_api_key_configured(dir.path()).await;
}
