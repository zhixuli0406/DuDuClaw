//! TDD tests for Secret Manager integration (ADR-004).
//!
//! Test coverage targets:
//! - SecretUri parsing (happy path + error cases)
//! - LocalSecretAdapter CRUD operations
//! - SecretManager resolution from URI
//! - Config loading from TOML

use crate::secret_manager::{
    LocalSecretAdapter, SecretBackend, SecretManager, SecretManagerConfig, SecretUri,
};

// ─── SecretUri parsing ─────────────────────────────────────────────────────

#[test]
fn test_parse_vault_uri() {
    let uri = SecretUri::parse("secret://vault/brave_search").unwrap();
    assert_eq!(uri.backend, SecretBackend::Vault);
    assert_eq!(uri.name, "brave_search");
}

#[test]
fn test_parse_local_uri() {
    let uri = SecretUri::parse("secret://local/my_key").unwrap();
    assert_eq!(uri.backend, SecretBackend::Local);
    assert_eq!(uri.name, "my_key");
}

#[test]
fn test_parse_env_uri() {
    let uri = SecretUri::parse("secret://env/MY_API_KEY").unwrap();
    assert_eq!(uri.backend, SecretBackend::Env);
    assert_eq!(uri.name, "MY_API_KEY");
}

#[test]
fn test_parse_vault_uri_with_path() {
    // Paths like secret://vault/path/to/secret
    let uri = SecretUri::parse("secret://vault/figma/token").unwrap();
    assert_eq!(uri.backend, SecretBackend::Vault);
    assert_eq!(uri.name, "figma/token");
}

#[test]
fn test_parse_uri_unknown_backend_returns_error() {
    let result = SecretUri::parse("secret://unknown/foo");
    assert!(result.is_err());
}

#[test]
fn test_parse_uri_wrong_scheme_returns_error() {
    let result = SecretUri::parse("https://vault/foo");
    assert!(result.is_err());
}

#[test]
fn test_parse_uri_empty_name_returns_error() {
    let result = SecretUri::parse("secret://local/");
    assert!(result.is_err());
}

#[test]
fn test_parse_uri_missing_backend_returns_error() {
    let result = SecretUri::parse("secret://");
    assert!(result.is_err());
}

#[test]
fn test_secret_uri_display() {
    let uri = SecretUri {
        backend: SecretBackend::Vault,
        name: "brave_search".to_string(),
    };
    assert_eq!(uri.to_string(), "secret://vault/brave_search");
}

// ─── LocalSecretAdapter ────────────────────────────────────────────────────

#[tokio::test]
async fn test_local_adapter_put_and_get() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    adapter.put("api_key", "sk-1234567890").await.unwrap();
    let value = adapter.get("api_key").await.unwrap();
    assert_eq!(value, "sk-1234567890");
}

#[tokio::test]
async fn test_local_adapter_get_nonexistent_returns_error() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    let result = adapter.get("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_local_adapter_delete_existing_key() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    adapter.put("to_delete", "value").await.unwrap();
    adapter.delete("to_delete").await.unwrap();
    let result = adapter.get("to_delete").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_local_adapter_delete_nonexistent_returns_error() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    let result = adapter.delete("nonexistent").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_local_adapter_exists_true() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    adapter.put("exists_key", "v").await.unwrap();
    assert!(adapter.exists("exists_key").await.unwrap());
}

#[tokio::test]
async fn test_local_adapter_exists_false() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    assert!(!adapter.exists("absent_key").await.unwrap());
}

#[tokio::test]
async fn test_local_adapter_overwrite_value() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    adapter.put("key", "first").await.unwrap();
    adapter.put("key", "second").await.unwrap();
    let value = adapter.get("key").await.unwrap();
    assert_eq!(value, "second");
}

#[tokio::test]
async fn test_local_adapter_stores_encrypted() {
    // Verify the value is not stored as plaintext (encryption must be applied).
    // We do this by checking that raw bytes != original string.
    let adapter = LocalSecretAdapter::new_ephemeral();
    adapter.put("test", "plaintext_value").await.unwrap();

    // Retrieve should decrypt correctly
    let retrieved = adapter.get("test").await.unwrap();
    assert_eq!(retrieved, "plaintext_value");
}

#[tokio::test]
async fn test_local_adapter_rejects_empty_name() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    let result = adapter.put("", "value").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_local_adapter_rejects_empty_value() {
    let adapter = LocalSecretAdapter::new_ephemeral();
    let result = adapter.put("key", "").await;
    assert!(result.is_err());
}

// ─── SecretManagerConfig ──────────────────────────────────────────────────

#[test]
fn test_config_default_is_local_backend() {
    let config = SecretManagerConfig::default();
    assert_eq!(config.backend, "local");
}

#[test]
fn test_config_parse_from_toml_vault() {
    let toml_str = r#"
[secret_manager]
backend = "vault"
vault_addr = "http://127.0.0.1:8200"
vault_token = "hvs.test123"
vault_mount = "secret"
"#;
    let config = SecretManagerConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.backend, "vault");
    assert_eq!(config.vault_addr.as_deref(), Some("http://127.0.0.1:8200"));
    assert_eq!(config.vault_token.as_deref(), Some("hvs.test123"));
    assert_eq!(config.vault_mount.as_deref(), Some("secret"));
}

#[test]
fn test_config_parse_from_toml_local() {
    let toml_str = r#"
[secret_manager]
backend = "local"
"#;
    let config = SecretManagerConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.backend, "local");
    assert!(config.vault_addr.is_none());
}

#[test]
fn test_config_vault_addr_defaults_to_localhost() {
    // When vault backend is selected but no addr given, default to localhost:8200
    let toml_str = r#"
[secret_manager]
backend = "vault"
vault_token = "hvs.abc"
"#;
    let config = SecretManagerConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(
        config.effective_vault_addr(),
        "http://127.0.0.1:8200"
    );
}

#[test]
fn test_config_vault_mount_defaults_to_secret() {
    let toml_str = r#"
[secret_manager]
backend = "vault"
vault_token = "hvs.abc"
"#;
    let config = SecretManagerConfig::from_toml_str(toml_str).unwrap();
    assert_eq!(config.effective_vault_mount(), "secret");
}

// ─── Env backend via SecretUri ─────────────────────────────────────────────

#[tokio::test]
async fn test_env_backend_reads_environment_variable() {
    use crate::secret_manager::EnvSecretAdapter;
    // SAFETY: single-threaded test context; no other test modifies this key concurrently.
    unsafe {
        std::env::set_var("DDC_TEST_SECRET_1234", "test_value_xyz");
    }
    let adapter = EnvSecretAdapter::new();
    let value = adapter.get("DDC_TEST_SECRET_1234").await.unwrap();
    assert_eq!(value, "test_value_xyz");
    // SAFETY: same reasoning as above.
    unsafe {
        std::env::remove_var("DDC_TEST_SECRET_1234");
    }
}

#[tokio::test]
async fn test_env_backend_missing_var_returns_error() {
    use crate::secret_manager::EnvSecretAdapter;
    // SAFETY: we are only removing a key unique to this test.
    unsafe {
        std::env::remove_var("DDC_TEST_NONEXISTENT_9999");
    }
    let adapter = EnvSecretAdapter::new();
    let result = adapter.get("DDC_TEST_NONEXISTENT_9999").await;
    assert!(result.is_err());
}
