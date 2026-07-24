//! Startup provisioning of the gateway-internal MCP API key.
//!
//! Since the M6 fail-closed change (v1.31), `duduclaw mcp-server` refuses to
//! start unless `DUDUCLAW_MCP_API_KEY` carries a key registered in
//! `config.toml [mcp_keys]`. Nothing ever provisioned that key for the
//! gateway's own CLI children, so every runtime whose CLI spawns MCP servers
//! with a sanitized env (Grok, and any setup where the gateway env itself
//! lacks the key) silently lost the whole duduclaw tool surface.
//!
//! This module closes the loop: at gateway startup, ensure ONE internal key
//! (client_id = `gateway-internal`, scope `admin`, `is_external = false`)
//! exists in `[mcp_keys]` and return its cleartext so the caller can export
//! it into the gateway process env — from where `mcp_forward_env_vars()`
//! carries it into every MCP env assembly point.
//!
//! Security posture: this restores the pre-M6 *local* convenience (any
//! gateway-spawned child can reach the MCP server) but keeps M6's actual
//! goal — an EXTERNAL/stdio caller without the key still gets denied, and the
//! key is per-install (never leaves `config.toml` + local process envs).
//! Concurrency: writes hold `duduclaw_core::with_file_lock` on `config.toml`
//! (multi-instance gateways race on first boot otherwise).

use std::path::Path;
use tracing::info;

/// Reserved `client_id` marking the auto-provisioned internal key.
pub const INTERNAL_CLIENT_ID: &str = "gateway-internal";

/// Ensure the internal MCP key exists in `<home>/config.toml [mcp_keys]`,
/// creating it on first boot. Returns the cleartext key. Idempotent: an
/// existing `gateway-internal` entry is returned as-is (never rotated here —
/// rotation stays a dashboard `mcp_keys.revoke`/`create` operation).
pub fn ensure_internal_mcp_key(home_dir: &Path) -> Result<String, String> {
    let config_path = home_dir.join("config.toml");
    duduclaw_core::with_file_lock(&config_path, || {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let mut table: toml::Table = toml::from_str(&content)
            .map_err(|e| std::io::Error::other(format!("malformed config.toml: {e}")))?;

        if let Some(keys) = table.get("mcp_keys").and_then(|v| v.as_table()) {
            for (key, val) in keys {
                if val.get("client_id").and_then(|v| v.as_str()) == Some(INTERNAL_CLIENT_ID) {
                    return Ok(key.clone());
                }
            }
        }

        // First boot: mint a fresh key. `Uuid::simple()` renders 32 lowercase
        // hex chars — the exact suffix `is_valid_key_format` requires.
        let key = format!("ddc_prod_{}", uuid::Uuid::new_v4().simple());
        let mut entry = toml::map::Map::new();
        entry.insert(
            "client_id".into(),
            toml::Value::String(INTERNAL_CLIENT_ID.into()),
        );
        entry.insert("is_external".into(), toml::Value::Boolean(false));
        entry.insert(
            "created_at".into(),
            toml::Value::String(chrono::Utc::now().to_rfc3339()),
        );
        entry.insert(
            "scopes".into(),
            toml::Value::Array(vec![toml::Value::String("admin".into())]),
        );

        let mcp_keys = table
            .entry("mcp_keys")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        let mcp_keys = mcp_keys
            .as_table_mut()
            .ok_or_else(|| std::io::Error::other("config.toml [mcp_keys] is not a table"))?;
        mcp_keys.insert(key.clone(), toml::Value::Table(entry));

        // Atomic write (temp + rename) so a crash never truncates config.toml.
        let rendered = toml::to_string_pretty(&table)
            .map_err(|e| std::io::Error::other(format!("serialize config.toml: {e}")))?;
        let tmp = config_path.with_extension("toml.tmp");
        std::fs::write(&tmp, &rendered)?;
        std::fs::rename(&tmp, &config_path)?;
        duduclaw_core::platform::set_owner_only(&config_path).ok();

        info!(
            client_id = INTERNAL_CLIENT_ID,
            "Provisioned internal MCP API key in config.toml [mcp_keys]"
        );
        Ok(key)
    })
    .map_err(|e| format!("internal MCP key provisioning failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provisions_once_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let k1 = ensure_internal_mcp_key(dir.path()).unwrap();
        assert!(k1.starts_with("ddc_prod_"), "canonical format: {k1}");
        assert_eq!(k1.len(), "ddc_prod_".len() + 32);
        let k2 = ensure_internal_mcp_key(dir.path()).unwrap();
        assert_eq!(k1, k2, "second boot must reuse the same key, not mint");

        let content = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(content.contains(INTERNAL_CLIENT_ID));
        assert!(content.contains(&k1));
    }

    #[test]
    fn preserves_existing_config_and_foreign_keys() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            r#"
[general]
default_agent = "anna"

[mcp_keys."ddc_prod_00000000000000000000000000000000"]
client_id = "claude-desktop"
is_external = true
created_at = "2026-01-01T00:00:00Z"
scopes = ["memory:read"]
"#,
        )
        .unwrap();
        let key = ensure_internal_mcp_key(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(content.contains("default_agent = \"anna\""));
        assert!(content.contains("claude-desktop"));
        assert!(content.contains(&key));
    }
}
