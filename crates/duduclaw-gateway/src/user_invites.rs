//! Secret-free configuration and response helpers for account invitations.
use std::path::Path;
use serde_json::{Value, json};

/// Only operator configuration or an explicit Admin argument can supply the
/// destination. This API deliberately has no request-header input.
pub(crate) fn public_base_url(configured: Option<&str>, supplied: Option<&str>) -> Result<String, &'static str> {
    let raw = configured.filter(|s| !s.trim().is_empty())
        .or(supplied.filter(|s| !s.trim().is_empty()))
        .ok_or("public_url_required")?;
    // Reserve space for /invite# and the 43-character bearer token.
    if raw.len() > 1997 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("invalid_public_url");
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid_public_url")?;
    if url.scheme() != "https" || url.host_str().is_none()
        || !url.username().is_empty() || url.password().is_some()
        || url.query().is_some() || url.fragment().is_some() {
        return Err("invalid_public_url");
    }
    let base = url.as_str().trim_end_matches('/');
    if base.len() > 1997 { return Err("invalid_public_url"); }
    Ok(base.to_owned())
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

/// Do not persist this message in the employee mailbox: it contains a bearer
/// capability. Transport errors are intentionally discarded, not logged.
#[cfg(test)]
pub(crate) async fn send(home: &Path, to: &str, link: &str) -> Value {
    let content = duduclaw_auth::invitation_content::InvitationContent::defaults("zh-TW");
    let rendered = match render_invitation(home, &content, "zh-TW", "", link, Some(link)) {
        Ok(value) => value,
        Err(_) => return delivery_payload(false, "mail_content_invalid", link),
    };
    send_rendered(home, to, link, &rendered).await
}

pub(crate) async fn send_rendered(home: &Path, to: &str, link: &str, rendered: &crate::invitation_email::RenderedInvitation) -> Value {
    let Some(mut cfg) = crate::mail_worker::resolve_smtp_config(home) else {
        return delivery_payload(false, "mail_unconfigured", link);
    };
    cfg.from_addr = crate::invitation_email::header_value(&cfg.from_addr);
    let to = crate::invitation_email::header_value(to);
    let policy = crate::mail::MailConfig::from_home(home);
    if !crate::mail::is_plausible_address(&to) || !crate::mail::address_allowed(&to, &policy.allowed_recipients) {
        return delivery_payload(false, "mail_recipient_refused", link);
    }
    let sent = matches!(tokio::time::timeout(std::time::Duration::from_secs(30),
        crate::email::send_email_alternative(&cfg, &to, &rendered.subject, &rendered.text, &rendered.html)).await, Ok(Ok(())));
    delivery_payload(sent, "mail_send_failed", link)
}

/// Combine the shared preview/delivery renderer with operator-owned branding.
pub(crate) fn render_invitation(home: &Path, content: &duduclaw_auth::invitation_content::InvitationContent, locale: &str, display_name: &str, link: &str, logo_base: Option<&str>) -> Result<crate::invitation_email::RenderedInvitation, &'static str> {
    let company = crate::company_info::read(home).unwrap_or_default();
    let brand = if company.name.is_empty() { crate::distributor_brand::product_name() } else { company.name };
    let mut rendered = crate::invitation_email::render(content, locale, &brand, display_name, link)?;
    let logo = company.logo_url.and_then(|path| {
        // Only a server-built content-addressed path is embedded. Preview
        // uses a same-origin path; delivery uses the operator-approved origin.
        let logo_url = match logo_base {
            Some(link) => {
                let base = reqwest::Url::parse(link).ok()?;
                let relative = path.strip_prefix('/').unwrap_or(&path);
                let url = base.join(relative).ok()?;
                if url.origin() != base.origin() { return None; }
                url.to_string()
            }
            None => path,
        };
        Some(format!(
            "<p><img src=\"{}\" alt=\"{}\" width=\"64\" height=\"64\"></p>",
            escape_html(&logo_url), escape_html(&brand),
        ))
    }).unwrap_or_default();
    let heading = format!("{logo}<p>{}</p>", escape_html(&brand));
    rendered.html = rendered.html.replacen("<body>", &format!("<body>{heading}"), 1);
    rendered.text = format!("{brand}\n\n{}", rendered.text);
    Ok(rendered)
}

fn escape_html(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

#[cfg(test)]
fn invitation_content(home: &Path, link: &str) -> (String, String, String) {
    let content = duduclaw_auth::invitation_content::InvitationContent::defaults("zh-TW");
    let rendered = render_invitation(home, &content, "zh-TW", "", link, Some(link)).unwrap();
    (rendered.subject, rendered.text, rendered.html)
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
    fn edited_invitation_content_and_company_branding_are_rendered_together() {
        let home = tempfile::tempdir().unwrap();
        let hash = "b".repeat(64);
        std::fs::write(home.path().join("config.toml"), format!(
            "[company]\nname = 'Company & <team>'\nlogo_hash = '{hash}'\n",
        )).unwrap();
        let mut content = duduclaw_auth::invitation_content::InvitationContent::defaults("en");
        content.opening = "Custom **welcome** <script>bad</script>".into();
        content.steps = "Custom steps".into();
        content.examples = ["First".into(), "Second".into(), "Third".into()];
        content.course_url = "https://course.test/?a=1&b=2".into();
        let rendered = render_invitation(home.path(), &content, "en", "<Guest & name>", "https://trusted.test/agents/invite#secret", Some("https://trusted.test/agents/invite#secret")).unwrap();
        assert_eq!(rendered.subject, "Invitation to Company & <team>");
        assert!(rendered.text.contains("Custom steps"));
        assert!(rendered.html.contains("Custom <strong>welcome</strong> &lt;script&gt;bad&lt;/script&gt;"));
        assert!(rendered.html.contains("Company &amp; &lt;team&gt;"));
        assert!(rendered.html.contains("&lt;Guest &amp; name&gt;"));
        assert!(rendered.html.contains(&format!("https://trusted.test/agents/api/company/logo/{hash}.png")));
        assert!(rendered.html.contains("https://course.test/?a=1&amp;b=2"));
        assert_eq!(rendered.examples, content.examples);
        let preview = render_invitation(home.path(), &content, "en", "Guest", "https://example.invalid/invite#preview", None).unwrap();
        assert!(preview.html.contains(&format!("src=\"/api/company/logo/{hash}.png\"")));
        assert!(!preview.html.contains("src=\"https://example.invalid"));
        assert!(!rendered.html.contains("<script>"));
        assert!(!rendered.html.contains("<Guest"));
    }
    #[test]
    fn explicit_url_only_never_headers() {
        assert_eq!(public_base_url(Some("https://trusted.test/"), Some("https://forged.test")), Ok("https://trusted.test".into()));
        assert_eq!(public_base_url(None, None), Err("public_url_required"));
        assert!(public_base_url(None, Some("http://localhost:18889/")).is_err());
        for bad in ["http://example.com", "http://localhost.evil.com", "http://127.0.0.2", "http://[::2]", "javascript:alert(1)", "https:///", "https://user:pass@host.test", "https://host.test?next=evil", "https://host.test/#x"] {
            assert!(public_base_url(None, Some(bad)).is_err());
        }
        for local in ["http://localhost", "http://127.0.0.1:18889", "http://[::1]:18889"] {
            assert!(public_base_url(None, Some(local)).is_err());
        }
        assert!(public_base_url(Some("invalid"), Some("https://valid.test")).is_err());
    }
    #[test]
    fn public_base_reserves_room_for_the_rendered_invitation_token() {
        let prefix = "https://host.test/";
        let longest = format!("{prefix}{}", "a".repeat(1997 - prefix.len()));
        let base = public_base_url(None, Some(&longest)).unwrap();
        let content = duduclaw_auth::invitation_content::InvitationContent::defaults("en");
        let link = format!("{base}/invite#{}", "A".repeat(43));
        assert_eq!(link.len(), 2048);
        assert!(crate::invitation_email::render(&content, "en", "Brand", "User", &link).is_ok());
        assert!(public_base_url(None, Some(&format!("{longest}a"))).is_err());
        assert!(public_base_url(None, Some(&format!("{prefix}{}", "é".repeat(500)))).is_err());
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
