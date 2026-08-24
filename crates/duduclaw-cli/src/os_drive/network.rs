//! A7a `network` group — read-only wired/Wi-Fi status queries. Every
//! function reuses a `duduclaw-gateway` `network`/`device` function
//! directly, same "second front door, same implementation" pattern
//! `system.rs` documents. No write verbs here on purpose (see
//! `commercial/docs/DESIGN-os-self-drive-2026-08.md` §2 — `wifi_connect`/
//! `wired_config` stay behind the dashboard RPC and `mcp_os_ops.rs`'s
//! existing, more heavily-gated surface, not duplicated here).

use std::path::Path;

pub async fn status() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let interfaces = duduclaw_gateway::device::collect_network();
    serde_json::to_string_pretty(&interfaces).map_err(|e| format!("序列化失敗：{e}"))
}

pub async fn wired_status(home_dir: &Path) -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    let status = duduclaw_gateway::network::wired::collect_wired_status(home_dir);
    serde_json::to_string_pretty(&status).map_err(|e| format!("序列化失敗：{e}"))
}

pub async fn wifi_status() -> Result<String, String> {
    if !duduclaw_core::is_appliance() {
        return Err(not_appliance_message());
    }
    match duduclaw_gateway::network::status().await {
        Ok(status) => serde_json::to_string_pretty(&status).map_err(|e| format!("序列化失敗：{e}")),
        Err(e) => Err(duduclaw_gateway::network::error_to_json(&e).to_string()),
    }
}

fn not_appliance_message() -> String {
    "此功能僅限 DuDuClaw 裝置版（appliance image）使用。".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn every_network_read_fails_closed_off_appliance() {
        assert!(std::env::var(duduclaw_core::APPLIANCE_ENV).is_err());
        assert!(status().await.unwrap_err().contains("appliance"));
        let home = tempfile::tempdir().unwrap();
        assert!(wired_status(home.path()).await.unwrap_err().contains("appliance"));
        assert!(wifi_status().await.unwrap_err().contains("appliance"));
    }
}
