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
mod audio;
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

/// A7c: when the direct `$XDG_RUNTIME_DIR`-based socket call `display.rs`
/// makes fails, retry via the gateway's fixed-path bridge
/// (`duduclaw_gateway::display_bridge` — reaches comp's socket at its
/// deployment-known path, `/run/duduclaw-kiosk/duduclaw-shell.sock`,
/// regardless of the CALLING process's own environment) before giving up.
/// This is what makes `duduclaw os display ...` work from an agent-identity
/// CLI subprocess, which A7a's design doc (§7) documented as a structural
/// dead end for the direct path alone — comp's own uid-boundary fix
/// (`duduclaw-comp/src/shell_control/listener.rs::classify_peer`'s new
/// `PeerAuthority::Agent` tier) is what the bridge's connection actually
/// relies on; this function only decides WHEN to try it.
///
/// A human operator at the kiosk terminal with a correctly-set
/// `$XDG_RUNTIME_DIR` never reaches the fallback at all — `primary` already
/// succeeded. When BOTH fail, `primary`'s error is what surfaces (not the
/// fallback's): it already carries A7a's full uid-boundary explanation
/// (design doc §7/§8), which is the more useful diagnostic for a human
/// reading a real failure; the fallback's own error is logged at debug
/// level so it is not silently lost, just not the one a user sees.
async fn with_gateway_display_fallback<F>(
    primary: std::result::Result<String, String>,
    fallback: F,
) -> std::result::Result<String, String>
where
    F: std::future::Future<Output = std::result::Result<serde_json::Value, String>>,
{
    if primary.is_ok() {
        return primary;
    }
    match fallback.await {
        Ok(v) => Ok(format!("{v:#}")),
        Err(fallback_err) => {
            tracing::debug!(
                fallback_error = %fallback_err,
                "os_drive::display: gateway bridge fallback also failed — surfacing the \
                 primary (XDG_RUNTIME_DIR-based) error instead, it is the more actionable one"
            );
            primary
        }
    }
}

// ── display ──────────────────────────────────────────────────────────────
//
// A7c: every command below now tries the direct `$XDG_RUNTIME_DIR`-based
// path first (unchanged — still the fast, zero-extra-hop path for a human
// operator at the kiosk terminal), then falls back to the gateway's
// fixed-path bridge. "同能力雙前門一份實作" — the bridge owns the ONE real
// implementation of "reach comp from a process that isn't the kiosk
// session"; this module is a thin router, same shape `system`/`network`
// already established for gateway's pure functions.

pub async fn cursor_size_get() -> Result<()> {
    finish(
        with_gateway_display_fallback(
            display::cursor_size_get().await,
            duduclaw_gateway::display_bridge::cursor_source_get(),
        )
        .await,
    )
}

pub async fn cursor_size_set(size: i64) -> Result<()> {
    finish(
        with_gateway_display_fallback(
            display::cursor_size_set(size).await,
            duduclaw_gateway::display_bridge::cursor_size_set(size),
        )
        .await,
    )
}

pub async fn cursor_source_get() -> Result<()> {
    finish(
        with_gateway_display_fallback(
            display::cursor_source_get().await,
            duduclaw_gateway::display_bridge::cursor_source_get(),
        )
        .await,
    )
}

pub async fn cursor_source_set(source: &str) -> Result<()> {
    finish(
        with_gateway_display_fallback(
            display::cursor_source_set(source).await,
            duduclaw_gateway::display_bridge::cursor_source_set(source),
        )
        .await,
    )
}

pub async fn theme_set(theme: &str) -> Result<()> {
    finish(
        with_gateway_display_fallback(
            display::theme_set(theme).await,
            duduclaw_gateway::display_bridge::theme_set(theme),
        )
        .await,
    )
}

// ── audio ────────────────────────────────────────────────────────────────
//
// Y10-1: thin wrappers over `duduclaw_gateway::audio_bridge` — no
// `with_gateway_display_fallback`-style split here, because
// `audio_bridge`'s own `run_wpctl` already does the ambient-then-fixed-path
// retry internally (see `os_drive::audio`'s module doc for why one function
// covers both attempts for audio, unlike display's two hand-rolled copies).

pub async fn audio_get() -> Result<()> {
    finish(audio::get().await)
}

pub async fn audio_volume_set(pct: u8) -> Result<()> {
    finish(audio::volume_set(pct).await)
}

pub async fn audio_mute_toggle() -> Result<()> {
    finish(audio::mute_toggle().await)
}

pub async fn audio_output_set(id: u32) -> Result<()> {
    finish(audio::output_set(id).await)
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

pub async fn system_update_check(home_dir: &Path) -> Result<()> {
    finish(system::update_check(home_dir).await)
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

#[cfg(test)]
mod fallback_tests {
    use super::with_gateway_display_fallback;

    #[tokio::test]
    async fn a_successful_primary_never_touches_the_fallback() {
        let result = with_gateway_display_fallback(Ok("primary-ok".to_string()), async {
            panic!("fallback must not be polled when primary already succeeded")
        })
        .await;
        assert_eq!(result, Ok("primary-ok".to_string()));
    }

    #[tokio::test]
    async fn a_failed_primary_falls_back_and_succeeds() {
        let result = with_gateway_display_fallback(
            Err("primary failed (e.g. XDG_RUNTIME_DIR mismatch)".to_string()),
            async { Ok(serde_json::json!({ "size": 48 })) },
        )
        .await;
        assert!(result.unwrap().contains("48"));
    }

    #[tokio::test]
    async fn both_failing_surfaces_the_primary_error_not_the_fallbacks() {
        let result = with_gateway_display_fallback(
            Err("primary: the A7a uid-boundary explanation".to_string()),
            async { Err("fallback: some other error".to_string()) },
        )
        .await;
        let err = result.unwrap_err();
        assert!(err.contains("uid-boundary"), "unexpected: {err}");
        assert!(!err.contains("some other error"), "unexpected: {err}");
    }
}
