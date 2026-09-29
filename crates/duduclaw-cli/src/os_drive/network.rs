//! A7a `network` group — read-only wired/Wi-Fi status queries.
//!
//! O16: every verb is a thin adapter over [`duduclaw_gateway::os_ops`], the
//! single authority the agent-facing MCP tools and the dashboard
//! `device.*`/`network.*` RPCs also go through. No write verbs here on
//! purpose (see `commercial/docs/DESIGN-os-self-drive-2026-08.md` §2 —
//! `wifi_connect`/`wired_config` stay behind the dashboard RPC and
//! `mcp_os_ops.rs`'s more heavily-gated surface).

use std::path::Path;

use duduclaw_gateway::os_ops;

/// Same rendering split `os_drive::system` documents: the authority's
/// payload pretty-printed, and this front door's own wording for the
/// (unreachable) serialize arm.
fn finish(result: Result<serde_json::Value, os_ops::OsOpError>) -> Result<String, String> {
    match result {
        Ok(v) => serde_json::to_string_pretty(&v).map_err(|e| format!("序列化失敗：{e}")),
        Err(e) => Err(match e.serialize_detail() {
            Some(detail) => format!("序列化失敗：{detail}"),
            None => e.message(),
        }),
    }
}

pub async fn status() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    finish(os_ops::network_interfaces())
}

pub async fn wired_status(home_dir: &Path) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    finish(os_ops::wired_status(home_dir))
}

pub async fn wifi_status() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    finish(os_ops::wifi_status().await)
}

fn not_appliance_message() -> String {
    os_ops::NOT_APPLIANCE_MESSAGE.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_network_read_fails_closed_off_appliance() {
        assert!(std::env::var(duduclaw_core::APPLIANCE_ENV).is_err());
        assert!(status().await.unwrap_err().contains("appliance"));
        let home = tempfile::tempdir().unwrap();
        assert!(
            wired_status(home.path())
                .await
                .unwrap_err()
                .contains("appliance")
        );
        assert!(wifi_status().await.unwrap_err().contains("appliance"));
    }

    /// O16: a Wi-Fi backend failure must still print the structured
    /// `error_to_json` body this surface has always printed — not a bare
    /// message, and not the MCP/RPC serialize wording.
    #[test]
    fn finish_renders_a_wifi_error_as_the_structured_json_body() {
        let err = duduclaw_gateway::network::WifiError {
            code: duduclaw_gateway::network::WifiErrorCode::BackendUnavailable,
            detail: "no iwd".to_string(),
        };
        let expected = duduclaw_gateway::network::error_to_json(&err).to_string();
        let rendered = finish(Err(os_ops::OsOpError::Wifi(err))).unwrap_err();
        assert_eq!(rendered, expected);
    }
}
