//! The navigation allowlist of tool-driven sessions (design §7): resolving
//! the employee's `allowed_domains` on the gateway at session start, the
//! wording the agent sees, and the `navigate` URL check. The pure parts are
//! unit-tested; the resolver is injectable.

use std::net::{IpAddr, Ipv4Addr};
use std::time::Duration;

use async_trait::async_trait;

use crate::computer_use_orchestrator::PinnedHost;

use super::{ErrorCode, OpError};

/// Longest URL `navigate` accepts, in bytes (before and after normalization).
pub const MAX_URL_BYTES: usize = 2_000;
/// Upper bound on resolving one allowlist host at session start.
pub const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// Longest path kept in a browser-audit `url`, in characters.
const AUDIT_PATH_MAX_CHARS: usize = 300;

/// Turns an allowlist host into the address the container is pinned to.
/// Production: [`DnsResolver`]; tests inject a fake.
#[async_trait]
pub(crate) trait HostResolver: Send + Sync {
    /// The first public IPv4 address of `host` (port 443), or `None` when it
    /// does not resolve, any answer is not public, or no IPv4 is offered.
    async fn resolve(&self, host: &str) -> Option<Ipv4Addr>;
}

/// [`crate::web_fetch::resolve_public_addrs`] (every answer must be public or
/// the whole answer is refused) plus [`strictly_public`] over the same set,
/// bounded by [`RESOLVE_TIMEOUT`].
pub(crate) struct DnsResolver;

#[async_trait]
impl HostResolver for DnsResolver {
    async fn resolve(&self, host: &str) -> Option<Ipv4Addr> {
        let addrs = match tokio::time::timeout(RESOLVE_TIMEOUT, crate::web_fetch::resolve_public_addrs(host, 443))
            .await
        {
            Ok(Ok(addrs)) => addrs,
            _ => return None,
        };
        first_pinnable_ipv4(addrs.iter().map(|a| a.ip()))
    }
}

/// The first IPv4 of an answer, `None` unless every address in it is
/// [`strictly_public`] (all or nothing, like `vet_resolved_addrs`).
pub(crate) fn first_pinnable_ipv4(addrs: impl Iterator<Item = IpAddr>) -> Option<Ipv4Addr> {
    let mut first = None;
    for ip in addrs {
        if !strictly_public(&ip) {
            return None;
        }
        if let (None, IpAddr::V4(v4)) = (first, ip) {
            first = Some(v4);
        }
    }
    first
}

/// The workspace-wide public-address check
/// ([`duduclaw_core::net_addr::is_public_ip`], the same one
/// `web_fetch::is_internal_ip` negates).
pub(crate) fn strictly_public(ip: &IpAddr) -> bool {
    duduclaw_core::net_addr::is_public_ip(ip)
}

/// Resolve every allowlist host (concurrently). Returns the pinned hosts in
/// allowlist order and the hosts that were skipped.
pub(crate) async fn resolve_hosts(
    resolver: &dyn HostResolver,
    hosts: &[String],
) -> (Vec<PinnedHost>, Vec<String>) {
    let answers = futures_util::future::join_all(hosts.iter().map(|h| resolver.resolve(h))).await;
    let mut pinned = Vec::new();
    let mut skipped = Vec::new();
    for (host, answer) in hosts.iter().zip(answers) {
        match answer {
            Some(ip) => pinned.push(PinnedHost { host: host.clone(), ip }),
            None => skipped.push(host.clone()),
        }
    }
    (pinned, skipped)
}

/// The start result's network sentence (no addresses in it).
pub fn start_message(configured: usize, reachable: &[String], skipped: &[String], ignored: usize) -> String {
    let mut out = if configured == 0 {
        "這個 session 沒有網路連線。要開網頁，請管理者在此員工 agent.toml 的 [capabilities.computer_use_config] allowed_domains 加入網域。".to_string()
    } else if reachable.is_empty() {
        format!(
            "白名單裡的網站這次都連不上（無法解析，或解析到內部網路）：{}。這個 session 沒有網路連線。",
            skipped.join("、")
        )
    } else {
        let mut s = format!("可用 computer_navigate 開啟的網站：{}。", reachable.join("、"));
        if !skipped.is_empty() {
            s.push_str(&format!(
                "以下網站這次連不上（無法解析，或解析到內部網路）：{}。",
                skipped.join("、")
            ));
        }
        s.push_str("其他網站在容器裡無法連線。");
        s
    };
    if ignored > 0 {
        out.push_str(&format!(
            "設定中有 {ignored} 個項目不符合完整主機名的格式（例如萬用字元、IP 位址、帶埠號或路徑），或超過 20 個上限，已略過。"
        ));
    }
    out
}

/// Refusal when the session has no reachable host.
pub fn no_hosts_error(configured: bool) -> OpError {
    let message = if configured {
        "這個 session 沒有可開啟的網站：白名單裡的網站在 session 開始時都連不上。可以稍後先呼叫 computer_session_stop，再重新開始 session。"
    } else {
        "這個 session 沒有可開啟的網站。請管理者在此員工 agent.toml 的 [capabilities.computer_use_config] allowed_domains 加入網域，再重新開始 session。"
    };
    OpError::new(ErrorCode::InvalidAction, message)
}

fn listing(hosts: &[String]) -> String {
    format!("可開啟的網站：{}。", hosts.join("、"))
}

/// A URL `navigate` may open, normalized.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckedUrl {
    /// The serialized, normalized URL handed to the container (what was
    /// validated is what is opened).
    pub url: String,
    /// The (lowercased) host, one of the session's pinned hosts.
    pub host: String,
    /// `https://<host><path>` without query or fragment, for the audit row.
    pub audit_url: String,
    /// Byte length of the path (for `tool_calls.jsonl`-style summaries).
    pub path_len: usize,
}

/// Design §7.4: `https` only, no userinfo, port absent or 443, host exactly
/// one of `hosts` (the hosts pinned for THIS session), at most
/// [`MAX_URL_BYTES`], no control or whitespace characters. `configured` says
/// whether the employee has an allowlist at all (for the wording when
/// `hosts` is empty).
pub fn validate_url(raw: &str, hosts: &[String], configured: bool) -> Result<CheckedUrl, OpError> {
    if hosts.is_empty() {
        return Err(no_hosts_error(configured));
    }
    let bad_format = || {
        OpError::new(
            ErrorCode::InvalidAction,
            format!(
                "網址格式不正確：只接受 https:// 開頭的完整網址，不能帶帳號密碼，連接埠只能省略或用 443，長度上限 {MAX_URL_BYTES} 字元。{}",
                listing(hosts)
            ),
        )
    };
    if raw.is_empty() || raw.len() > MAX_URL_BYTES || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(bad_format());
    }
    let parsed = reqwest::Url::parse(raw).map_err(|_| bad_format())?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.as_str().len() > MAX_URL_BYTES
    {
        return Err(bad_format());
    }
    let host = match (parsed.domain(), parsed.host_str()) {
        (Some(d), _) => d.to_ascii_lowercase(),
        (None, Some(_)) => {
            return Err(OpError::new(
                ErrorCode::InvalidAction,
                format!("不能用 IP 位址開啟網頁，請用網站名稱。{}", listing(hosts)),
            ));
        }
        (None, None) => return Err(bad_format()),
    };
    if !hosts.iter().any(|h| *h == host) {
        return Err(OpError::new(
            ErrorCode::InvalidAction,
            format!(
                "「{}」不在這個 session 可開啟的網站中。{}",
                duduclaw_core::truncate_chars(&host, 80),
                listing(hosts)
            ),
        ));
    }
    let path = parsed.path();
    Ok(CheckedUrl {
        url: parsed.as_str().to_string(),
        audit_url: format!("https://{host}{}", duduclaw_core::truncate_chars(path, AUDIT_PATH_MAX_CHARS)),
        path_len: path.len(),
        host,
    })
}

/// The `net::ERR_*` token (or a short lowercase code) the helper reported,
/// or `unknown` when it does not have that shape.
pub fn error_token(error: Option<&str>) -> String {
    let Some(e) = error else {
        return "unknown".to_string();
    };
    let net = e
        .strip_prefix("net::ERR_")
        .is_some_and(|rest| !rest.is_empty() && rest.len() <= 64 && rest.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'));
    let short = !e.is_empty() && e.len() <= 32 && e.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if net || short { e.to_string() } else { "unknown".to_string() }
}

/// The CONTRACT.toml `must_not` text for a navigation.
pub fn semantic_string(checked: &CheckedUrl) -> String {
    format!("navigate open web page url {}", checked.audit_url.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosts() -> Vec<String> {
        vec!["example.com".to_string(), "docs.example.com".to_string()]
    }

    #[test]
    fn valid_urls_are_normalized() {
        let c = validate_url("https://Example.com/a/b?q=secret#x", &hosts(), true).unwrap();
        assert_eq!(c.host, "example.com");
        assert_eq!(c.url, "https://example.com/a/b?q=secret#x");
        assert_eq!(c.audit_url, "https://example.com/a/b");
        assert_eq!(c.path_len, 4);
        let c = validate_url("https://docs.example.com:443", &hosts(), true).unwrap();
        assert_eq!(c.url, "https://docs.example.com/");
    }

    #[test]
    fn every_refusal() {
        let refused = [
            "http://example.com/",
            "HTTP://example.com/",
            "ftp://example.com/",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://user@example.com/",
            "https://user:pw@example.com/",
            "https://example.com:8443/",
            "https://example.com:80/",
            "https://evil.com/",
            "https://example.com.evil.com/",
            "https://evilexample.com/",
            "https://sub.example.com/",
            "https://93.184.215.14/",
            "https://[2606:2800:220:1::1]/",
            "https://example.com/\n",
            "https://example.com/ a",
            " https://example.com/",
            "https://example.com/\u{7}",
            "example.com",
            "",
            "https://",
            "https://example.com\\@evil.com/",
        ];
        for url in refused {
            let r = validate_url(url, &hosts(), true);
            if url == "https://example.com\\@evil.com/" {
                // The WHATWG parser reads `\` as `/`: the host is example.com
                // and the normalized URL (what the browser gets) says so.
                let c = r.unwrap();
                assert_eq!(c.host, "example.com");
                assert!(c.url.starts_with("https://example.com/"));
                continue;
            }
            let e = r.expect_err(url);
            assert_eq!(e.code, ErrorCode::InvalidAction, "{url}");
            assert!(e.message.contains("example.com"), "{url}: {}", e.message);
            assert!(!e.message.contains("不是") && !e.message.contains("——"), "{}", e.message);
        }
        let long = format!("https://example.com/{}", "a".repeat(MAX_URL_BYTES));
        assert!(validate_url(&long, &hosts(), true).is_err());
        // Percent-encoding growth past the cap is refused after normalization too.
        let grows = format!("https://example.com/{}", "é".repeat(600));
        assert!(grows.len() <= MAX_URL_BYTES);
        assert!(validate_url(&grows, &hosts(), true).is_err());
    }

    #[test]
    fn empty_session_hosts_name_the_setting() {
        let e = validate_url("https://example.com/", &[], false).unwrap_err();
        assert!(e.message.contains("[capabilities.computer_use_config] allowed_domains"));
        let e = validate_url("https://example.com/", &[], true).unwrap_err();
        assert!(e.message.contains("連不上"));
    }

    #[test]
    fn public_address_screen() {
        for bad in [
            "0.1.2.3", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.1.1", "172.16.0.1", "192.168.65.1",
            "192.0.2.1", "198.18.0.1", "224.0.0.1", "255.255.255.255", "::", "::1", "fe80::1", "fd00::1",
            "ff02::1", "::ffff:10.0.0.1", "2001:db8::1",
        ] {
            assert!(!strictly_public(&bad.parse().unwrap()), "{bad}");
        }
        for good in ["93.184.215.14", "1.1.1.1", "2606:4700::1111", "::ffff:1.1.1.1"] {
            assert!(strictly_public(&good.parse().unwrap()), "{good}");
        }
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        assert_eq!(
            first_pinnable_ipv4([ip("2606:4700::1111"), ip("1.1.1.1"), ip("1.0.0.1")].into_iter()),
            Some(Ipv4Addr::new(1, 1, 1, 1))
        );
        // One internal answer refuses the whole host.
        assert_eq!(first_pinnable_ipv4([ip("1.1.1.1"), ip("10.0.0.1")].into_iter()), None);
        // IPv6 only: nothing to pin.
        assert_eq!(first_pinnable_ipv4([ip("2606:4700::1111")].into_iter()), None);
    }

    #[test]
    fn error_tokens_are_shape_checked() {
        assert_eq!(error_token(Some("net::ERR_NAME_NOT_RESOLVED")), "net::ERR_NAME_NOT_RESOLVED");
        assert_eq!(error_token(Some("timeout")), "timeout");
        assert_eq!(error_token(Some("net::ERR_<script>")), "unknown");
        assert_eq!(error_token(Some("Page says: hello")), "unknown");
        assert_eq!(error_token(None), "unknown");
    }

    #[test]
    fn start_messages_carry_no_addresses() {
        let m = start_message(0, &[], &[], 0);
        assert!(m.contains("allowed_domains"));
        let m = start_message(2, &["example.com".into()], &["down.example".into()], 1);
        assert!(m.contains("example.com") && m.contains("down.example") && m.contains("1 個項目"));
        let m = start_message(1, &[], &["down.example".into()], 0);
        assert!(m.contains("沒有網路連線"));
        for m in [m, start_message(2, &["example.com".into()], &[], 0)] {
            assert!(!m.contains("不是") && !m.contains("——"));
        }
    }
}
