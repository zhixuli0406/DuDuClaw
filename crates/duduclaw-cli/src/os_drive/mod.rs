//! A7a — `duduclaw os <group> <verb>`: a self-documenting CLI surface an
//! agent (or an operator) can call to read/drive the three OS layers
//! (comp/display, gateway system, gateway network) DuDuClaw already exposes
//! through the dashboard WS RPC and the `mcp_os_ops.rs` MCP tool bridge.
//! Full design + rejected alternatives:
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md`.
//!
//! This module owns every real implementation; `lib.rs`'s `OsCommands`/
//! `OsDisplayCommands`/`OsSystemCommands`/`OsNetworkCommands` enums and the
//! `cmd_os` match arms that call into here stay a thin routing layer, kept
//! deliberately small because `lib.rs`'s command-registration point is a
//! shared hotspot other in-flight work also touches this round.

pub mod approval;
mod display;
mod network;
pub mod spec;
mod system;

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};

/// Convert a `Result<String, String>` from one of the submodules into the
/// CLI's own `Result<()>` — success prints the text and returns `Ok(())`,
/// failure becomes `Err(DuDuClawError::Gateway(msg))` so `entry_point`'s
/// existing `Error: {e}` + exit-1 handling applies uniformly, the same as
/// every other `cmd_*` function in this crate.
fn finish(result: std::result::Result<String, String>) -> Result<()> {
    match result {
        Ok(text) => {
            println!("{text}");
            Ok(())
        }
        Err(msg) => Err(DuDuClawError::Gateway(msg)),
    }
}

// ── display ──────────────────────────────────────────────────────────────

pub async fn cursor_size_get() -> Result<()> {
    finish(display::cursor_size_get().await)
}

pub async fn cursor_size_set(size: i64) -> Result<()> {
    finish(display::cursor_size_set(size).await)
}

pub async fn cursor_source_get() -> Result<()> {
    finish(display::cursor_source_get().await)
}

pub async fn cursor_source_set(source: &str) -> Result<()> {
    finish(display::cursor_source_set(source).await)
}

pub async fn theme_set(theme: &str) -> Result<()> {
    finish(display::theme_set(theme).await)
}

// ── system ───────────────────────────────────────────────────────────────

pub async fn system_about() -> Result<()> {
    finish(system::about().await)
}

pub async fn system_timezone_get() -> Result<()> {
    finish(system::timezone_get().await)
}

/// Runs the `requires_approval` gate (design doc §5) before the effect.
pub async fn system_timezone_set(home_dir: &Path, timezone: &str) -> Result<()> {
    if let Err(msg) = approval::gate(
        home_dir,
        &format!("agent 要求變更系統時區為 {timezone}"),
        "os_system_timezone_set",
    )
    .await
    {
        return Err(DuDuClawError::Gateway(msg));
    }
    finish(system::timezone_set(home_dir, timezone).await)
}

pub async fn system_ntp_get() -> Result<()> {
    finish(system::ntp_get().await)
}

pub async fn system_ntp_set(home_dir: &Path, enabled: bool) -> Result<()> {
    if let Err(msg) = approval::gate(
        home_dir,
        &format!("agent 要求{} NTP 時間同步", if enabled { "啟用" } else { "停用" }),
        "os_system_ntp_set",
    )
    .await
    {
        return Err(DuDuClawError::Gateway(msg));
    }
    finish(system::ntp_set(home_dir, enabled).await)
}

pub async fn system_update_check() -> Result<()> {
    finish(system::update_check().await)
}

// ── network ──────────────────────────────────────────────────────────────

pub async fn network_status() -> Result<()> {
    finish(network::status().await)
}

pub async fn network_wired_status(home_dir: &Path) -> Result<()> {
    finish(network::wired_status(home_dir).await)
}

pub async fn network_wifi_status() -> Result<()> {
    finish(network::wifi_status().await)
}

// ── introspection ────────────────────────────────────────────────────────

pub fn commands(json: bool) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&spec::commands_json())
                .map_err(|e| DuDuClawError::Gateway(format!("序列化失敗：{e}")))?
        );
    } else {
        println!("{}", spec::render_commands_table());
    }
    Ok(())
}
