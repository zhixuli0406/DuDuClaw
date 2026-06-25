//! First-run configuration bootstrap.
//!
//! The gateway tolerates a missing `config.toml` at startup (every read of it
//! is defensive — `.ok()` / `unwrap_or_default()`), so the only thing blocking
//! a brand-new install from reaching the dashboard was the CLI's hard-stop.
//! [`write_minimal_config`] writes the smallest valid, parseable config that
//! lets the gateway boot; the rest of first-run setup happens in the browser
//! (the dashboard onboarding flow), not the terminal.

use crate::error::{DuDuClawError, Result};
use std::path::Path;

/// Write a minimal but valid `config.toml` into `home`.
///
/// Only the two sections the operator cares about on first boot are written —
/// `[general]` log level and `[gateway]` bind/port. Everything else the gateway
/// reads with `unwrap_or_default`, so this is sufficient to start. The write is
/// atomic (temp file + rename) so a crash mid-write never leaves a half-written
/// config that would fail to parse on the next boot.
///
/// This is a no-op-safe primitive: callers should check for an existing config
/// first; this function always overwrites, so don't call it when a config the
/// user authored already exists.
pub fn write_minimal_config(home: &Path, bind: &str, port: u16) -> Result<()> {
    std::fs::create_dir_all(home)?;

    let content = format!(
        "# DuDuClaw configuration (auto-created on first run).\n\
         # Finish setup in the dashboard — no need to edit this by hand.\n\
         \n\
         [general]\n\
         log_level = \"info\"\n\
         \n\
         [gateway]\n\
         bind = \"{bind}\"\n\
         port = {port}\n"
    );

    let target = home.join("config.toml");
    let tmp = home.join("config.toml.tmp");
    std::fs::write(&tmp, content.as_bytes())?;
    std::fs::rename(&tmp, &target).map_err(|e| {
        // Best-effort cleanup so a failed rename doesn't leave the temp behind.
        let _ = std::fs::remove_file(&tmp);
        DuDuClawError::Io(e)
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_parseable_config_with_gateway_section() {
        let dir = std::env::temp_dir().join(format!("ddc-cfg-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        write_minimal_config(&dir, "0.0.0.0", 12345).expect("write should succeed");

        let raw = std::fs::read_to_string(dir.join("config.toml")).expect("config exists");
        let table: toml::Table = raw.parse().expect("config parses as TOML");

        let gateway = table.get("gateway").and_then(|v| v.as_table()).expect("[gateway]");
        assert_eq!(gateway.get("bind").and_then(|v| v.as_str()), Some("0.0.0.0"));
        assert_eq!(gateway.get("port").and_then(|v| v.as_integer()), Some(12345));
        assert_eq!(
            table
                .get("general")
                .and_then(|v| v.as_table())
                .and_then(|g| g.get("log_level"))
                .and_then(|v| v.as_str()),
            Some("info")
        );

        // No leftover temp file.
        assert!(!dir.join("config.toml.tmp").exists());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
