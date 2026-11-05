//! One renderer for preview and delivery. Admin-authored text is never HTML.
use duduclaw_auth::invitation_content::InvitationContent;
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct RenderedInvitation {
    pub subject: String,
    pub text: String,
    pub html: String,
    pub examples: [String; 3],
}

pub(crate) fn validate(content: &InvitationContent) -> Result<(), &'static str> {
    content.validate().map_err(|_| "invalid_invitation_content")?;
    if !content.course_url.is_empty() {
        validate_https_url(&content.course_url).map_err(|_| "invalid_course_url")?;
    }
    Ok(())
}

fn validate_https_url(raw: &str) -> Result<(), &'static str> {
    if raw.len() > 2048 || raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("invalid_invitation_url");
    }
    let url = reqwest::Url::parse(raw).map_err(|_| "invalid_invitation_url")?;
    if url.scheme() != "https" || url.host_str().is_none() || !url.username().is_empty() || url.password().is_some() {
        return Err("invalid_invitation_url");
    }
    Ok(())
}

pub(crate) fn header_value(raw: &str) -> String {
    raw.chars().filter(|c| !c.is_control()).collect()
}

fn escape(value: &str) -> String {
    value.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Only paired **bold** and line breaks are interpreted. Markdown links,
/// images and raw HTML stay literal; the course URL has its own validated field.
fn limited_markdown(raw: &str) -> String {
    let mut output = String::new();
    let mut rest = raw;
    while let Some((before, after)) = rest.split_once("**") {
        output.push_str(&escape(before));
        if let Some((bold, tail)) = after.split_once("**") {
            output.push_str("<strong>");
            output.push_str(&escape(bold));
            output.push_str("</strong>");
            rest = tail;
        } else {
            output.push_str("**");
            rest = after;
            break;
        }
    }
    output.push_str(&escape(rest));
    output.replace('\n', "<br>\n")
}

pub(crate) fn render(content: &InvitationContent, locale: &str, brand: &str, display_name: &str, link: &str) -> Result<RenderedInvitation, &'static str> {
    validate(content)?;
    validate_https_url(link)?;
    let (subject, action, expiry, ignore, examples_title, course_title) = match locale {
        "en" => (format!("Invitation to {brand}"), "Set your password", "This link expires in 7 days and can be used once.", "If you did not expect this invitation, ignore this message.", "Try these examples", "Getting-started course"),
        "ja-JP" => (format!("{brand} へのご招待"), "パスワードを設定", "リンクは7日間有効で、1回のみ使用できます。", "お心当たりがない場合は、このメールを無視してください。", "試してみましょう", "入門コース"),
        _ => (format!("邀請您加入 {brand}"), "設定密碼", "連結有效期限為 7 天，僅能使用一次。", "如果您未預期收到這封信，請忽略。", "試試這些上手範例", "入門課程"),
    };
    // Brand/name are data, never header syntax.
    let subject = header_value(&subject);
    let mut text = format!("{display_name}\n{}\n\n{}\n\n{action}: {link}\n{expiry}\n\n{examples_title}\n", content.opening, content.steps);
    let mut html = format!("<!doctype html><html><body><p>{}</p><p>{}</p><p>{}</p><p><a href=\"{}\">{}</a></p><p>{}</p><h2>{}</h2><ul>",
        escape(display_name), limited_markdown(&content.opening), limited_markdown(&content.steps), escape(link), action, expiry, examples_title);
    for example in &content.examples {
        text.push_str(&format!("- {example}\n"));
        html.push_str(&format!("<li>{}</li>", limited_markdown(example)));
    }
    html.push_str("</ul>");
    if !content.course_url.is_empty() {
        text.push_str(&format!("\n{course_title}: {}\n", content.course_url));
        html.push_str(&format!("<p><a href=\"{}\">{course_title}</a></p>", escape(&content.course_url)));
    }
    text.push_str(&format!("\n{ignore}\n"));
    html.push_str(&format!("<p>{ignore}</p></body></html>"));
    Ok(RenderedInvitation { subject, text, html, examples: content.examples.clone() })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invitation_email_escapes_all_admin_text_and_only_renders_bold() {
        let mut content = InvitationContent::defaults("zh-TW");
        content.opening = "<script>alert(1)</script> **您好** [click](javascript:bad)".into();
        content.course_url = "https://course.example.test/?a=1&b=2".into();
        let email = render(&content, "zh-TW", "Brand\r\nInjected", "<img src=x>", "https://example.test/invite#preview").unwrap();
        assert!(!email.html.contains("<script>"));
        assert!(!email.html.contains("<img"));
        assert!(email.html.contains("&lt;script&gt;"));
        assert!(email.html.contains("<strong>您好</strong>"));
        assert!(email.html.contains("[click](javascript:bad)"));
        assert!(email.html.contains("a=1&amp;b=2"));
        assert!(!email.subject.contains('\n'));
        for bad in ["javascript:alert(1)", "http://course.test", "https://user:pass@course.test", "https://course.test/\n"] {
            content.course_url = bad.into();
            assert!(render(&content, "en", "Brand", "User", "https://example.test/invite#preview").is_err());
        }
    }
    #[test]
    fn invitation_links_and_header_fields_are_fail_closed() {
        let content = InvitationContent::defaults("en");
        for link in ["http://localhost/invite", "javascript:alert(1)", "https://user:pass@host.test/invite", "https://host.test/\r\n"] {
            assert!(render(&content, "en", "Brand", "User", link).is_err());
        }
        assert_eq!(header_value("User\r\n\t\0 <user@test.invalid>\u{7f}"), "User <user@test.invalid>");
    }
    #[test]
    fn invitation_email_locales_keep_the_same_required_safety_information() {
        for locale in ["zh-TW", "en", "ja-JP"] {
            let c = InvitationContent::defaults(locale);
            let email = render(&c, locale, "Brand", "User", "https://example.test/invite#preview").unwrap();
            assert!(email.text.contains("7"));
            assert!(email.text.contains("https://example.test/invite#preview"));
            assert_eq!(email.examples.len(), 3);
            for example in c.examples { assert!(email.text.contains(&example)); }
        }
    }
}
