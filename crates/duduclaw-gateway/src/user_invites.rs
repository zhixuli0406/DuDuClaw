//! Secret-free configuration and response helpers for account invitations.
use std::path::Path;
use serde_json::{Value, json};

/// Only operator configuration or an explicit Admin argument can supply the
/// destination. This API deliberately has no request-header input.
pub(crate) fn public_base_url(configured: Option<&str>, supplied: Option<&str>) -> Result<String, &'static str> {
    let raw = configured.filter(|s| !s.trim().is_empty())
        .or(supplied.filter(|s| !s.trim().is_empty()))
        .ok_or("public_url_required")?;
    if raw.len() > 2048 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("invalid_public_url");
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid_public_url")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() {
        return Err("invalid_public_url");
    }
    // Compare the parsed host exactly; lookalike domains are never local.
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if url.scheme() == "http" && !local {
        return Err("invalid_public_url");
    }
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

pub(crate) async fn base_from_home(home: &Path, supplied: Option<&str>) -> Result<String, &'static str> {
    let content = match tokio::fs::read_to_string(home.join("config.toml")).await {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err("invite_config_unavailable"),
    };
    let table = match content {
        Some(content) => Some(content.parse::<toml::Table>().map_err(|_| "invite_config_unavailable")?),
        None => None,
    };
    let gateway = match table.as_ref().and_then(|t| t.get("gateway")) {
        Some(value) => Some(value.as_table().ok_or("invite_config_unavailable")?),
        None => None,
    };
    let configured = match gateway.and_then(|g| g.get("public_url")) {
        Some(value) => Some(value.as_str().ok_or("invalid_public_url")?),
        None => None,
    };
    public_base_url(configured, supplied)
}

pub(crate) fn delivery_payload(sent: bool, reason: &str, link: &str) -> Value {
    if sent { json!({"sent":true}) }
    else { json!({"sent":false,"reason":reason,"link":link}) }
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
        .replace('"', "&quot;").replace('\'', "&#39;")
}

/// Do not persist this message in the employee mailbox: it contains a bearer
/// capability. Transport errors are intentionally discarded, not logged.
pub(crate) async fn send(home: &Path, to: &str, link: &str) -> Value {
    let Some(cfg) = crate::mail_worker::resolve_smtp_config(home) else {
        return delivery_payload(false, "mail_unconfigured", link);
    };
    let policy = crate::mail::MailConfig::from_home(home);
    if !crate::mail::is_plausible_address(to) || !crate::mail::address_allowed(to, &policy.allowed_recipients) {
        return delivery_payload(false, "mail_recipient_refused", link);
    }
    let (subject, plain, html) = invitation_content(home, link);
    let sent = matches!(tokio::time::timeout(std::time::Duration::from_secs(30),
        crate::email::send_email_alternative(&cfg, to, &subject, &plain, &html)).await, Ok(Ok(())));
    delivery_payload(sent, "mail_send_failed", link)
}

fn invitation_content(home: &Path, link: &str) -> (String, String, String) {
    let company = crate::company_info::read(home).unwrap_or_default();
    let brand = if company.name.is_empty() { crate::distributor_brand::product_name() } else { company.name };
    let logo = company.logo_url.and_then(|path| {
        // Only a server-built content-addressed path on the invitation's own
        // operator-approved origin is embedded; no uploaded URL is accepted.
        let base = reqwest::Url::parse(link).ok()?;
        let relative = path.strip_prefix('/').unwrap_or(&path);
        let url = base.join(relative).ok()?;
        (url.origin() == base.origin()).then(|| format!(
            "<p><img src=\"{}\" alt=\"{}\" width=\"64\" height=\"64\"></p>",
            escape_html(url.as_str()), escape_html(&brand),
        ))
    }).unwrap_or_default();
    let subject = format!("邀請您加入 {brand}");
    let plain = format!("您受邀加入 {brand}。\n請透過邀請連結設定密碼，再使用一般登入流程登入。\n{link}\n連結有效期限為 7 天，僅能使用一次。\n如果您未預期收到這封信，請忽略。");
    let html = format!("{logo}<p>您受邀加入 {}。</p><p>請透過邀請連結設定密碼，再使用一般登入流程登入。</p><p><a href=\"{}\">設定密碼</a></p><p>連結有效期限為 7 天，僅能使用一次。</p><p>如果您未預期收到這封信，請忽略。</p>", escape_html(&brand), escape_html(link));
    (subject, plain, html)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitation_customer_name_is_escaped_and_logo_uses_own_origin() {
        let home = tempfile::tempdir().unwrap();
        let link = "https://trusted.test/invite#secret";
        let (subject, _, html) = invitation_content(home.path(), link);
        assert_eq!(subject, format!("邀請您加入 {}", crate::distributor_brand::product_name()));
        assert!(!html.contains("<img"));
        let hash = "a".repeat(64);
        std::fs::write(home.path().join("config.toml"), format!(
            "[company]\nname = '<script>範例 & 公司</script>'\nlogo_hash = '{hash}'\n",
        )).unwrap();
        let (subject, plain, html) = invitation_content(home.path(), link);
        assert!(subject.contains("範例"));
        assert!(plain.contains(link));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;範例 &amp; 公司&lt;/script&gt;"));
        assert!(html.contains(&format!("https://trusted.test/api/company/logo/{hash}.png")));
        assert_eq!(html.matches("#secret").count(), 1);
        let (_, _, html) = invitation_content(home.path(), "https://trusted.test/agents/invite#secret");
        assert!(html.contains(&format!("https://trusted.test/agents/api/company/logo/{hash}.png")));
        assert!(!html.contains("https://trusted.test/api/company/logo/"));
    }
    #[test]
    fn explicit_url_only_never_headers() {
        assert_eq!(public_base_url(Some("https://trusted.test/"), Some("https://forged.test")), Ok("https://trusted.test".into()));
        assert_eq!(public_base_url(None, None), Err("public_url_required"));
        assert_eq!(public_base_url(None, Some("http://localhost:18889/")), Ok("http://localhost:18889".into()));
        for bad in ["http://example.com", "http://localhost.evil.com", "http://127.0.0.2", "http://[::2]", "javascript:alert(1)", "https:///", "https://user:pass@host.test", "https://host.test?next=evil", "https://host.test/#x"] {
            assert!(public_base_url(None, Some(bad)).is_err());
        }
        for local in ["http://localhost", "http://127.0.0.1:18889", "http://[::1]:18889"] {
            assert!(public_base_url(None, Some(local)).is_ok());
        }
        assert!(public_base_url(Some("invalid"), Some("https://valid.test")).is_err());
    }
    #[test]
    fn only_failed_delivery_returns_capability() {
        let link = "https://host.test/invite#secret";
        assert_eq!(delivery_payload(true, "", link), json!({"sent":true}));
        assert_eq!(delivery_payload(false, "mail_unconfigured", link)["link"], link);
        assert!(!delivery_payload(true, "", link).to_string().contains("secret"));
    }
    #[tokio::test]
    async fn no_mail_configuration_returns_copy_link() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(send(home.path(), "user@example.test", "https://host.test/invite#secret").await,
            json!({"sent":false,"reason":"mail_unconfigured","link":"https://host.test/invite#secret"}));
    }
}
