//! `GET /v1/find` — LAN discovery convenience page.
//!
//! Lists devices that are (a) currently online and (b) were last seen
//! reporting the *same public IP address* as whoever is loading this page —
//! i.e. devices plausibly on the same network as the caller. Only a device
//! name and its self-reported LAN IP are shown; nothing secret. This
//! endpoint performs no authentication of its own and grants no access —
//! clicking a listed LAN address just takes the browser to the box's own
//! login screen on the LAN.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::response::{Html, IntoResponse};

use crate::db::DeviceSummary;
use crate::error::RelayError;
use crate::state::AppState;

pub async fn find_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, RelayError> {
    let client_ip = resolve_client_ip(&headers, peer);
    let devices = state.db.devices_by_public_ip(&client_ip)?;
    Ok(Html(render_find_page(&client_ip, &devices)))
}

/// Resolve the caller's public IP address.
///
/// Cloud Run's front end appends the real connecting client IP as the
/// *last* entry of `X-Forwarded-For` (earlier entries may be
/// client-supplied and are not trustworthy). Falls back to the raw TCP peer
/// address when the header is absent (local/dev runs with no proxy in
/// front). This is a convenience-grouping heuristic, not an access-control
/// decision — worst case of a spoofed value is an incorrect (not
/// over-privileged) device listing.
fn resolve_client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    if let Some(xff) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(last) = xff.rsplit(',').next() {
            let candidate = last.trim();
            if !candidate.is_empty() && candidate.parse::<std::net::IpAddr>().is_ok() {
                return candidate.to_string();
            }
        }
    }
    peer.ip().to_string()
}

fn render_find_page(client_ip: &str, devices: &[DeviceSummary]) -> String {
    let rows = if devices.is_empty() {
        "<p class=\"empty\">目前沒有偵測到同網段的裝置。</p>".to_string()
    } else {
        let items: String = devices
            .iter()
            .map(|d| {
                let name = html_escape(if d.name.trim().is_empty() {
                    "未命名裝置"
                } else {
                    d.name.trim()
                });
                let lan = html_escape(&d.lan_ip);
                format!(
                    "<li><a href=\"https://{lan}\">{name}</a><span class=\"ip\">{lan}</span></li>"
                )
            })
            .collect();
        format!("<ul class=\"devices\">{items}</ul>")
    };

    // `client_ip` is already validated to parse as a std::net::IpAddr (see
    // resolve_client_ip), so it contains no HTML-structural characters and
    // needs no escaping here.
    format!(
        r#"<!doctype html>
<html lang="zh-Hant">
<head>
<meta charset="utf-8">
<title>DuDuClaw 裝置探索</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
 body {{ font-family: -apple-system, "Noto Sans TC", sans-serif; max-width: 32rem; margin: 3rem auto; padding: 0 1rem; color: #1c1917; background: #fafaf9; }}
 h1 {{ font-size: 1.25rem; }}
 .devices {{ list-style: none; padding: 0; }}
 .devices li {{ display: flex; justify-content: space-between; align-items: center; padding: 0.75rem 1rem; margin-bottom: 0.5rem; background: #fff; border-radius: 0.75rem; border: 1px solid #e7e5e4; }}
 .devices a {{ color: #b45309; text-decoration: none; font-weight: 600; }}
 .ip {{ color: #78716c; font-size: 0.875rem; }}
 .empty {{ color: #78716c; }}
 .meta {{ color: #a8a29e; font-size: 0.75rem; margin-top: 2rem; }}
</style>
</head>
<body>
<h1>同網段的 DuDuClaw 裝置</h1>
{rows}
<p class="meta">依來源公網位址比對（{client_ip}）</p>
</body>
</html>"#
    )
}

/// Minimal HTML text-node escaping. Device names are user-supplied at
/// registration time and rendered here, so this is a security control
/// (stored-XSS prevention), not cosmetic.
fn html_escape(s: &str) -> String {
    s.chars().fold(String::with_capacity(s.len()), |mut acc, c| {
        match c {
            '&' => acc.push_str("&amp;"),
            '<' => acc.push_str("&lt;"),
            '>' => acc.push_str("&gt;"),
            '"' => acc.push_str("&quot;"),
            '\'' => acc.push_str("&#39;"),
            _ => acc.push(c),
        }
        acc
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_client_ip_prefers_last_xff_entry() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "9.9.9.9, 203.0.113.1".parse().unwrap());
        let peer: SocketAddr = "127.0.0.1:1".parse().unwrap();
        assert_eq!(resolve_client_ip(&headers, peer), "203.0.113.1");
    }

    #[test]
    fn resolve_client_ip_falls_back_to_peer() {
        let headers = HeaderMap::new();
        let peer: SocketAddr = "203.0.113.1:1".parse().unwrap();
        assert_eq!(resolve_client_ip(&headers, peer), "203.0.113.1");
    }

    #[test]
    fn html_escape_neutralizes_script_tags() {
        let escaped = html_escape("<script>alert(1)</script>");
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
    }

    #[test]
    fn render_find_page_escapes_malicious_device_name() {
        let devices = vec![DeviceSummary {
            name: "<img src=x onerror=alert(1)>".to_string(),
            lan_ip: "192.168.1.5".to_string(),
        }];
        let html = render_find_page("203.0.113.1", &devices);
        assert!(!html.contains("<img src=x"));
        assert!(html.contains("&lt;img"));
    }

    #[test]
    fn render_find_page_shows_empty_state() {
        let html = render_find_page("203.0.113.1", &[]);
        assert!(html.contains("目前沒有偵測到同網段的裝置"));
    }

    #[test]
    fn render_find_page_falls_back_for_blank_name() {
        let devices = vec![DeviceSummary {
            name: "   ".to_string(),
            lan_ip: "192.168.1.5".to_string(),
        }];
        let html = render_find_page("203.0.113.1", &devices);
        assert!(html.contains("未命名裝置"));
    }
}
