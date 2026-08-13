//! B5 (ecosystem): `duduclaw tunnel` — one-command remote access for the
//! dashboard via a Cloudflare *quick tunnel* (no account, no domain, no
//! config). For production use (stable URL, SLA) the deployment guide's
//! Tailscale Funnel / named-tunnel paths remain the recommendation; this is
//! the zero-friction trial path.
//!
//! Deliberately read-only on configuration: the wizard PRINTS the
//! `allowed_origins` line the operator must add — a tool that silently
//! widens an origin allowlist would be a security smell.

use std::process::Stdio;

use duduclaw_core::error::{DuDuClawError, Result};
use tokio::io::{AsyncBufReadExt, BufReader};

/// Extract the assigned quick-tunnel URL from a cloudflared log line.
/// Whole-token match on the known host shape — no loose substring games.
pub fn extract_tunnel_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let token: String = line[start..]
        .chars()
        .take_while(|c| !c.is_whitespace())
        .collect();
    let host = token.strip_prefix("https://")?;
    let host = host.trim_end_matches('/');
    let mut parts = host.split('.');
    let (sub, rest): (&str, Vec<&str>) = (parts.next()?, parts.collect());
    let sub_ok = !sub.is_empty()
        && sub
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if sub_ok && rest == ["trycloudflare", "com"] {
        return Some(format!("https://{host}"));
    }
    None
}

pub async fn cmd_tunnel(home_dir: &std::path::Path) -> Result<()> {
    let (port, _src) = duduclaw_core::config::gateway_port_for_home(home_dir);

    // cloudflared present?
    let probe = tokio::process::Command::new("cloudflared")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    if probe.map(|s| s.success()).unwrap_or(false) == false {
        println!("找不到 cloudflared —— 安裝後再跑一次：");
        println!("  macOS : brew install cloudflared");
        println!("  Windows: winget install Cloudflare.cloudflared");
        println!("  Linux : https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/");
        println!("\n替代方案（要帳號但 URL 固定）：Tailscale Funnel —— 見 docs/guides/deployment-guide.md §2");
        return Ok(());
    }

    println!("🐾 啟動 Cloudflare 快速通道（免帳號；URL 每次啟動都不同，Ctrl-C 結束）…\n");
    let mut child = tokio::process::Command::new("cloudflared")
        .args(["tunnel", "--url", &format!("http://127.0.0.1:{port}")])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| DuDuClawError::Config(format!("cloudflared 啟動失敗：{e}")))?;

    // The assigned URL is announced on stderr; scan both streams to be safe.
    let stderr = child.stderr.take();
    let stdout = child.stdout.take();
    let announce = |url: String| {
        let host = url.trim_start_matches("https://").to_string();
        println!("✅ 儀表板網址：{url}");
        println!("\n⚠ 一次性設定（否則儀表板 WebSocket 會被來源檢查擋下）：");
        println!("   config.toml → [gateway] allowed_origins 加入 \"{host}\" 後重載設定");
        println!("\n⚠ 快速通道特性：URL 每次啟動都會變、無 SLA——日常遠端建議 Tailscale（URL 固定），");
        println!("   正式對外（LINE webhook 等）見 docs/guides/deployment-guide.md。");
    };
    if let Some(err) = stderr {
        let mut lines = BufReader::new(err).lines();
        let mut announced = false;
        tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                if !announced {
                    if let Some(url) = extract_tunnel_url(&line) {
                        announce(url);
                        announced = true;
                    }
                }
            }
        });
    }
    if let Some(out) = stdout {
        let mut lines = BufReader::new(out).lines();
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    }

    // Foreground until the operator stops it; the tunnel dies with us
    // (kill_on_drop) so no orphan process outlives Ctrl-C.
    let status = child
        .wait()
        .await
        .map_err(|e| DuDuClawError::Config(format!("cloudflared 執行錯誤：{e}")))?;
    if !status.success() {
        return Err(DuDuClawError::Config(format!(
            "cloudflared 結束（{status}）——網路受限環境可能無法建立快速通道"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn url_extraction_is_anchored() {
        assert_eq!(
            extract_tunnel_url("2026-08-13 INF |  https://neat-fox-abc123.trycloudflare.com  |").as_deref(),
            Some("https://neat-fox-abc123.trycloudflare.com")
        );
        // Suffix attacks / other hosts never match.
        assert!(extract_tunnel_url("https://evil.com/x.trycloudflare.com").is_none());
        assert!(extract_tunnel_url("https://foo.trycloudflare.com.evil.com").is_none());
        assert!(extract_tunnel_url("no url here").is_none());
        // Uppercase subdomain is not the service's shape → reject.
        assert!(extract_tunnel_url("https://ABC.trycloudflare.com").is_none());
    }
}
