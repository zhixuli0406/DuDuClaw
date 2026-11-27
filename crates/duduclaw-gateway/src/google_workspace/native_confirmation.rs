//! Gateway-owned, single-use confirmation for irreversible native Google calls.
//! MIME remains in the credential owner's stack; approvals persist only a hash and a curated card.
use std::path::{Path, PathBuf};
use serde_json::{Value, json};
use super::native_call::NativeCallTemplate;
use crate::approval::{ApprovalBroker, ApprovalStatus, action_grant::{self, ActionGrant}};

pub(crate) const MAX_CONFIRMATION_RECIPIENTS: usize = 100;
pub(crate) const MAX_CONFIRMATION_ATTACHMENTS: usize = 50;
pub(crate) const MAX_CONFIRMATION_BYTES: usize = 32 * 1024;
const MAX_BODY_PREVIEW_BYTES: usize = 2000;
const BODY_PREVIEW_TRUNCATED: &str = "…（內文預覽已截短）";

fn size_error() -> String {
    "Google 確認內容超過 32 KiB，已拒絕操作；請減少內容後重新確認。".into()
}

// Stop serialization at the budget rather than allocate an unbounded JSON string.
fn ensure_json_budget<T: serde::Serialize + ?Sized>(value: &T, budget: usize) -> Result<(), String> {
    struct Budget(usize);
    impl std::io::Write for Budget {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.0 {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "confirmation size limit"));
            }
            self.0 -= bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    serde_json::to_writer(Budget(budget), value).map_err(|_| size_error())
}

pub(crate) fn ensure_json_size<T: serde::Serialize + ?Sized>(value: &T) -> Result<(), String> {
    ensure_json_budget(value, MAX_CONFIRMATION_BYTES)
}

pub(crate) fn ensure_approval_size(payload: &Value, summary: &str) -> Result<(), String> {
    let budget = MAX_CONFIRMATION_BYTES.checked_sub(summary.len()).ok_or_else(size_error)?;
    ensure_json_budget(payload, budget)
}

pub(crate) fn check_recipient_count(count: usize) -> Result<(), String> {
    if count > MAX_CONFIRMATION_RECIPIENTS {
        return Err(format!("Google 收件人或參加者超過 {MAX_CONFIRMATION_RECIPIENTS} 筆，已拒絕操作；請減少人數後重新確認。"));
    }
    Ok(())
}

pub(crate) fn check_attachment_count(count: usize) -> Result<(), String> {
    if count > MAX_CONFIRMATION_ATTACHMENTS {
        return Err(format!("Google 附件超過 {MAX_CONFIRMATION_ATTACHMENTS} 筆，已拒絕操作；請減少附件後重新確認。"));
    }
    Ok(())
}

pub(crate) fn validate_snapshot(snapshot: &Value, recipients: &[String]) -> Result<(), String> {
    check_recipient_count(recipients.len())?;
    for field in ["recipients", "attendees"] {
        if let Some(entries) = snapshot[field].as_array() { check_recipient_count(entries.len())?; }
    }
    if let Some(entries) = snapshot["attachments"].as_array() { check_attachment_count(entries.len())?; }
    ensure_json_size(recipients)?;
    ensure_json_size(snapshot)
}

pub(crate) fn body_preview(body: &str) -> String {
    if body.len() <= MAX_BODY_PREVIEW_BYTES { return body.to_owned(); }
    format!("{}{BODY_PREVIEW_TRUNCATED}", duduclaw_core::truncate_bytes(body, MAX_BODY_PREVIEW_BYTES - BODY_PREVIEW_TRUNCATED.len()))
}

pub(crate) struct HostContext {
    home: PathBuf,
    agent: String,
    pub(crate) account: String,
    policy: String,
    incarnation: String,
    recipient_policy: String,
    owner_snapshot: Option<crate::google_identity::owner_snapshot::OwnerSnapshot>,
}
impl HostContext {
    pub(crate) fn new(home: &Path, agent: &str, account: String) -> Result<Self, String> {
        if !duduclaw_core::is_valid_agent_id(agent) { return Err("Invalid employee".into()); }
        let policy = crate::approval::strict_policy_revision(home, agent)?;
        let incarnation = Self::incarnation(home, agent)?;
        let recipient_policy = Self::recipient_policy(home, agent)?;
        let owner_snapshot = crate::google_identity::owner_snapshot::OwnerSnapshot::optional(home, agent)
            .map_err(|_| "Google 負責人授權無法確認，已拒絕。".to_owned())?;
        if owner_snapshot.as_ref().is_some_and(|owner| owner.email != account) {
            return Err("Google 負責人帳號已變更，請重新確認。".into());
        }
        Ok(Self { home: home.to_owned(), agent: agent.into(), account, policy, incarnation, recipient_policy, owner_snapshot })
    }
    fn incarnation(home: &Path, agent: &str) -> Result<String, String> {
        crate::decision_notify::open_user_db_read_only(home)?
            .ok_or("User database unavailable")?.google_employee_nonce(agent)?.ok_or_else(|| "無法確認員工生命週期，已拒絕。".into())
    }
    fn recipient_policy(home: &Path, agent: &str) -> Result<String, String> {
        let dir = crate::ephemeral::resolve_agent_dir(home, agent).unwrap_or_else(|| home.join("agents").join(agent));
        let policy = duduclaw_core::recipient_policy::load(home, Some(&dir));
        if !policy.valid { return Err("無法讀取收件人政策，已拒絕。".into()); }
        Ok(duduclaw_core::google_identity::google_arguments_digest(&json!([policy.mode, policy.recipients])))
    }
    pub(crate) fn current(&self) -> Result<(), String> {
        if let Some(owner) = &self.owner_snapshot {
            owner.validate(&self.home, &self.agent)
                .map_err(|_| "Google 負責人授權已變更，請重新確認。".to_owned())?;
        } else if duduclaw_core::google_identity::effective_for(&self.home, Some(&self.agent)).effective
            == duduclaw_core::google_identity::Effective::DelegateOwner {
            return Err("Google 負責人授權已變更，請重新確認。".into());
        }
        if Self::recipient_policy(&self.home, &self.agent)? != self.recipient_policy
            || Self::incarnation(&self.home, &self.agent)? != self.incarnation
            || crate::approval::strict_policy_revision(&self.home, &self.agent)? != self.policy {
            return Err("政策已改變，請重新確認。".into());
        }
        Ok(())
    }
    pub(crate) async fn confirm(&self, tool: &str, snapshot: &Value, recipients: &[String]) -> Result<(), String> {
        validate_snapshot(snapshot, recipients)?;
        self.current()?;
        // A block still wins. No allow, Admin scope or workflow standing grant
        // can bypass the fresh request below.
        let dir = crate::ephemeral::resolve_agent_dir(&self.home, &self.agent)
            .unwrap_or_else(|| self.home.join("agents").join(&self.agent));
        let rules = duduclaw_core::agent_toml::load_action_rules(&dir);
        if rules.resolve(tool, duduclaw_core::effect_of(tool))
            == Some(duduclaw_core::ActionVerdict::Block) {
            return Err("此操作已被動作規則封鎖。".into());
        }
        let recipient_policy = duduclaw_core::recipient_policy::load(&self.home, Some(&dir));
        if !recipient_policy.valid { return Err("無法讀取收件人政策，已拒絕。".into()); }
        // These are provider-derived addresses, never tool arguments. An
        // unlisted address is included in this same mandatory Admin card.
        let recipient_decision = recipient_policy.evaluate(recipients, None, &[]);
        let mut payload = json!({"name":tool,"arguments":snapshot,"native_google_confirmation":true});
        if recipient_decision == duduclaw_core::recipient_policy::Decision::Ask {
            payload["recipient_guard"] = json!({"reason":"outside_allowed_recipients","targets":recipients});
            payload["egress_guard"] = json!({"reason":"recipient_guard","sources":[],"recipient_guard":payload["recipient_guard"]});
        }
        payload["arguments"]["employee_incarnation"] = json!(self.incarnation);
        payload["arguments"]["policy_revision"] = json!(self.policy);
        if let Some(owner) = &self.owner_snapshot {
            // The single-use grant includes the exact owner/epoch that the
            // gateway selected; an approval cannot migrate to a new owner.
            payload["arguments"]["owner_snapshot"] = json!(owner);
            payload["google_owner_binding"] = json!(owner.digest());
        }
        let grant = ActionGrant::build(&self.agent, tool, &payload);
        payload[action_grant::ACTION_GRANT_KEY] = grant.to_json();
        let summary = card(tool, snapshot)?;
        // Include policy/grant metadata and any repeated recipient guard facts.
        ensure_approval_size(&payload, &summary)?;
        let broker = ApprovalBroker::open(&self.home)?;
        let id = broker.request(&self.agent, "mcp_call", &summary, payload, 90).await?;
        if broker.await_decision(&id, std::time::Duration::from_millis(500)).await? != ApprovalStatus::Approved {
            return Err("這次 Google 操作未獲核准，未執行變更。".into());
        }
        self.current()?;
        action_grant::verify_and_consume(&broker, &id, &grant).await
            .map_err(|_| "核准已失效或已使用，請重新確認。".to_owned())
    }
}

pub(crate) fn escaped(value: &Value) -> String {
    // Escape controls in strings before JSON quoting, including nested
    // attendee/attachment data. Reuse the shared core diagnostic boundary.
    let safe = duduclaw_security::credential_protection::sanitize_output_value(value.clone());
    let text = safe.to_string().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let mut out = String::new();
    for ch in text.chars() {
        if matches!(ch, '*' | '_' | '`' | '[' | ']' | '#' | '!') { out.push('\\'); }
        out.push(ch);
    }
    out
}

/// Escape all untrusted card data, including Markdown and line controls.
/// Each value is quoted JSON, so provider text cannot impersonate a card label.
pub(crate) fn card(tool: &str, snapshot: &Value) -> Result<String, String> {
    validate_snapshot(snapshot, &[])?;
    let mut text = format!("確認這一次 Google 操作：{tool}\n（核准用過即失效；內容改變須重新確認）\n");
    if let Some(fields) = snapshot.as_object() {
        let ordered = ["account", "from", "to", "cc", "bcc", "subject", "calendar", "calendar_info", "title", "start", "end", "attendees",
            "whole_recurring_series", "notify_attendees", "sendUpdates", "body_preview", "attachments", "draft_id", "event_id", "etag", "operation", "content_hash", "recipients"];
        for key in ordered.iter().filter(|key| fields.contains_key(**key)) {
            let value = &fields[*key];
            let label = match *key {
                "account" => "Google 帳號", "from" => "寄件標頭 From", "to" => "收件人 To", "cc" => "副本 Cc", "bcc" => "密件副本 Bcc",
                "subject" => "主旨", "body_preview" => "內文預覽", "attachments" => "附件（檔名／位元組）",
                "title" => "活動標題", "start" => "開始（含時區）", "end" => "結束（含時區）",
                "attendees" => "參加者", "calendar" => "日曆", "calendar_info" => "日曆名稱／時區", "whole_recurring_series" => "刪除整個重複系列",
                "notify_attendees" => "通知參加者", "content_hash" => "內容指紋", _ => *key,
            };
            text.push_str(&format!("{label}: {}\n", escaped(value)));
            if text.len() > MAX_CONFIRMATION_BYTES { return Err(size_error()); }
        }
    }
    if text.len() > MAX_CONFIRMATION_BYTES { return Err(size_error()); }
    Ok(text)
}

pub(crate) fn calendar_endpoint(template: &NativeCallTemplate, url: &reqwest::Url) -> bool {
    let super::native_call::ParsedNativeArguments::CalendarDelete { calendar, event, .. } = template.parsed() else { return false; };
    url.host_str() == Some("www.googleapis.com") && [
        format!("/calendar/v3/calendars/{}/events/{}", super::encode_path_component(calendar), super::encode_path_component(event)),
        "/calendar/v3/users/me/calendarList/primary".into(),
        format!("/calendar/v3/users/me/calendarList/{}", super::encode_path_component(calendar)),
    ].iter().any(|path| path == url.path())
}

// Legacy helpers accept arbitrary URLs, so cover the two Google API aliases too.
fn drive_host(url: &reqwest::Url) -> bool {
    matches!(url.host_str(), Some("www.googleapis.com" | "content.googleapis.com" | "googleapis.com"))
}

// Decode exactly once, rejecting malformed escapes and ambiguous path syntax.
// A remaining '%' could become a separator after a second decoding upstream.
fn drive_segments(path: &str) -> Option<Vec<String>> {
    if path == "/" { return Some(Vec::new()); }
    path.strip_prefix('/')?.split('/').map(|segment| {
        if segment.is_empty() { return None; }
        let mut decoded = Vec::with_capacity(segment.len());
        let mut bytes = segment.bytes();
        while let Some(byte) = bytes.next() {
            decoded.push(if byte == b'%' {
                let high = char::from(bytes.next()?).to_digit(16)?;
                let low = char::from(bytes.next()?).to_digit(16)?;
                (high * 16 + low) as u8
            } else { byte });
        }
        let value = String::from_utf8(decoded).ok()?;
        if matches!(value.as_str(), "." | "..")
            || value.chars().any(|ch| matches!(ch, '/' | '\\' | '%') || ch.is_control())
        { return None; }
        Some(value)
    }).collect()
}

/// Method fence for existing transports. Raw input must also pass
/// `drive_request_url`: Url parsing has already removed dot segments.
pub(crate) fn drive_method_allowed(method: &reqwest::Method, url: &reqwest::Url) -> bool {
    if !drive_host(url) { return true; }
    let Some(parts) = drive_segments(url.path()) else { return false; };
    let parts: Vec<_> = parts.iter().map(String::as_str).collect();
    match parts.first().copied() {
        Some("upload") => false,
        Some("drive") => *method == reqwest::Method::GET
            && !parts.iter().any(|part| matches!(*part, "permissions" | "revisions" | "comments"))
            && !parts.windows(2).any(|pair| pair == ["files", "trash"]),
        _ => true,
    }
}

/// Exact read routes used by Drive tools and the account health probe.
pub(crate) fn drive_read_path_allowed(url: &reqwest::Url) -> bool {
    let Some(parts) = drive_segments(url.path()) else { return false; };
    let parts: Vec<_> = parts.iter().map(String::as_str).collect();
    match parts.as_slice() {
        ["drive", "v3", "files"] | ["drive", "v3", "about"] => true,
        ["drive", "v3", "files", id] | ["drive", "v3", "files", id, "export"] => {
            !id.is_empty() && id.len() <= 256
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                && !matches!(*id, "trash" | "permissions" | "revisions" | "comments")
        }
        _ => false,
    }
}

/// Validate the original spelling before URL normalization can erase traversal.
/// Non-Drive services retain their own transport policy.
pub(crate) fn drive_request_url(method: &reqwest::Method, original: &str) -> Option<reqwest::Url> {
    let url = reqwest::Url::parse(original).ok()?;
    if let Some(host) = url.host_str() {
        let canonical = host.strip_suffix('.').unwrap_or(host).to_ascii_lowercase();
        // Refuse aliases outright rather than treating them as another service.
        if host.ends_with('.') || (host != canonical.as_str()
            && matches!(canonical.as_str(), "www.googleapis.com" | "content.googleapis.com" | "googleapis.com"))
        { return None; }
    }
    if drive_host(&url) {
        if original.chars().any(|ch| ch == '\\' || ch.is_control()) { return None; }
        let without_query = original.split(['?', '#']).next()?;
        let (_, authority_and_path) = without_query.split_once("://")?;
        let path = authority_and_path.split_once('/').map_or("/".to_owned(), |(_, path)| format!("/{path}"));
        drive_segments(&path)?;
        let parts = drive_segments(url.path())?;
        if parts.first().is_some_and(|part| part == "drive") && !drive_read_path_allowed(&url) {
            return None;
        }
    }
    drive_method_allowed(method, &url).then_some(url)
}

#[cfg(test)]
tokio::task_local! {
    // Replace only the request recipe; all real transport gates still execute.
    pub(crate) static TEST_DRIVE_REQUEST: (reqwest::Method, String);
}

#[cfg(test)]
pub(crate) fn drive_refusal_cases() -> Vec<(reqwest::Method, String)> {
    [
        (reqwest::Method::DELETE, "/drive/v3/files/x1234567890"),
        (reqwest::Method::POST, "/upload/drive/v3/files?uploadType=multipart"),
        (reqwest::Method::PATCH, "/drive/v3/files/x1234567890"),
        (reqwest::Method::GET, "/drive/v3/files/x1234567890/permissions"),
    ].into_iter().map(|(method, path)| (method, format!("https://www.googleapis.com{path}"))).collect()
}

/// New tools and Drive get exact paths; other existing allowlists remain in force.
pub(crate) fn endpoint_allowed(template: &NativeCallTemplate, url: &reqwest::Url) -> bool {
    use super::native_call::ParsedNativeArguments::*;
    if drive_host(url) {
        let Some(parts) = drive_segments(url.path()) else { return false; };
        if parts.first().is_some_and(|part| part == "drive" || part == "upload")
            && !drive_read_path_allowed(url) { return false; }
    }
    match template.parsed() {
        GmailSend { draft } => url.query().is_none() && url.host_str() == Some("gmail.googleapis.com")
            && ["/gmail/v1/users/me/profile".to_owned(), "/gmail/v1/users/me/drafts/send".into(),
                format!("/gmail/v1/users/me/drafts/{}", super::encode_path_component(draft))].iter().any(|p| p == url.path()),
        CalendarDelete { .. } => url.query().is_none() && calendar_endpoint(template, url),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drive_method_fence_preserves_reads_and_other_services() {
        for host in ["www.googleapis.com", "WWW.GOOGLEAPIS.COM", "content.googleapis.com", "googleapis.com"] {
            for path in ["/drive/v3/files?pageSize=1", "/drive/v3/files/a-B_1?fields=id,name,mimeType",
                "/drive/v3/files/a-B_1?alt=media", "/drive/v3/files/a-B_1/export?mimeType=text%2Fplain",
                "/drive/v3/about?fields=user"] {
                let url = format!("https://{host}{path}");
                assert!(drive_method_allowed(&reqwest::Method::GET, &url.parse().unwrap()), "{url}");
                assert!(drive_request_url(&reqwest::Method::GET, &url).is_some(), "{url}");
                for method in [reqwest::Method::POST, reqwest::Method::PATCH, reqwest::Method::DELETE,
                    reqwest::Method::HEAD, reqwest::Method::PUT, reqwest::Method::OPTIONS,
                    reqwest::Method::from_bytes(b"get").unwrap()] {
                    assert!(!drive_method_allowed(&method, &url.parse().unwrap()), "{method} {url}");
                }
            }
        }
        for url in ["https://www.googleapis.com/drivex/v3/files", "https://www.googleapis.com/drive-other/files",
            "https://www.googleapis.com/calendar/v3/calendars/primary/events/e1",
            "https://docs.googleapis.com/v1/documents/doc:batchUpdate",
            "https://sheets.googleapis.com/v4/spreadsheets/s/values/A1",
            "https://gmail.googleapis.com/drive/v3/files/x", "https://www.googleapis.com.evil.test/drive/v3/files/x"] {
            assert!(drive_method_allowed(&reqwest::Method::DELETE, &url.parse().unwrap()), "{url}");
        }
    }

    #[test]
    fn drive_method_fence_rejects_uploads_and_decoded_forbidden_segments() {
        for path in ["/upload/drive/v3/files", "/upload/anything", "/%75pload/drive/v3/files",
            "/drive/v3/files/trash", "/drive/v3/files/%74rash",
            "/drive/v3/files/x/permissions", "/drive/v3/files/x/%70ermissions",
            "/drive/v3/files/x/revisions", "/drive/v3/files/x/%63omments",
            "/drive/v3/files/x%2Fy", "/drive/v3/files/x%2fy", "/drive%2fv3/files/x",
            "/drive/v3/files/x%5Cy", "/drive/v3/files/x%252Fy", "/drive/v3/files/x%00",
            "//drive/v3/files/x", "/drive//v3/files/x", "//upload/drive/v3/files",
            "/drive/v3/files/%FF", "/drive/v3/files/%", "/drive/v3/files/%2", "/drive/v3/files/%GG"] {
            let url = format!("https://www.googleapis.com{path}");
            assert!(!drive_method_allowed(&reqwest::Method::GET, &url.parse().unwrap()), "{url}");
        }
        // Reserved names are whole segments, never substring matches.
        assert!(drive_method_allowed(&reqwest::Method::GET,
            &"https://www.googleapis.com/drive/v3/files/my_permissions_backup".parse().unwrap()));
    }

    #[test]
    fn drive_raw_url_fence_rejects_traversal_and_parse_errors() {
        for url in ["not a URL", "https://[", "https://www.googleapis.com/drive/v3/files/../files/x",
            "https://www.googleapis.com/drive/v3/files/./x", "https://www.googleapis.com/drive/v3/files/%2e/x",
            "https://www.googleapis.com/drive/v3/files/%2E%2e/files/x",
            "https://www.googleapis.com/drive/../../calendar/v3/calendars/primary/events",
            "https://www.googleapis.com/drive/v3/fi\tles/x"] {
            assert!(drive_request_url(&reqwest::Method::GET, url).is_none(), "{url}");
        }
    }

    #[test]
    fn drive_raw_url_fence_rejects_trailing_dot_hosts() {
        for host in ["www.googleapis.com.", "content.googleapis.com.", "googleapis.com.",
            "WWW.GOOGLEAPIS.COM.", "gmail.googleapis.com.", "other.example."] {
            for (method, path) in [
                (reqwest::Method::GET, "/drive/v3/files/x1234567890"),
                (reqwest::Method::DELETE, "/drive/v3/files/x1234567890"),
                (reqwest::Method::PATCH, "/drive/v3/files/x1234567890"),
                (reqwest::Method::POST, "/upload/drive/v3/files?uploadType=multipart"),
            ] {
                let url = format!("https://{host}{path}");
                assert!(drive_request_url(&method, &url).is_none(), "{method} {url}");
            }
        }
    }

    #[test]
    fn drive_endpoints_require_exact_read_routes_and_bounded_ids() {
        let template = NativeCallTemplate::prepare("drive_search", &json!({"query":"q"})).unwrap();
        for length in [1, 256] {
            let url: reqwest::Url = format!("https://www.googleapis.com/drive/v3/files/{}", "x".repeat(length)).parse().unwrap();
            assert!(drive_read_path_allowed(&url));
            assert!(endpoint_allowed(&template, &url));
        }
        for path in ["/drive/v3/files/".to_owned(), "/drive/v3/filesx".into(), "/drive/v3/files/x/copy".into(),
            "/drive/v3/files/x/export/extra".into(), "/drive/v3/files/x/export/".into(),
            "/drive/v3/files/x/permissions".into(), "/drive/v3/files/trash".into(),
            "/drive/v3/files/x.y".into(), "/drive/v3/files/中文".into(), "/drive/v3/drives".into(),
            "/upload/drive/v3/files".into(), format!("/drive/v3/files/{}", "x".repeat(257))] {
            let url: reqwest::Url = format!("https://www.googleapis.com{path}").parse().unwrap();
            assert!(!drive_read_path_allowed(&url), "{path}");
            assert!(!endpoint_allowed(&template, &url), "{path}");
            assert!(drive_request_url(&reqwest::Method::GET, url.as_str()).is_none(), "{path}");
        }
    }

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[agent]\nname='Alice'\nstatus='active'\n[capabilities]\nrecipients=['mail:alice@example.com']\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        users.ensure_google_employee_nonce("alice").unwrap();
        home
    }
    #[test]
    fn card_escapes_provider_content_and_contains_explicit_fields() {
        let text = card("gmail_send_draft", &json!({"to":"<script>alert(1)</script>\naccount: forged",
            "bcc":"hidden@example.com", "subject":"[click](https://evil.test)", "body_preview":"**approve**", "attachments":[{"filename":"a.txt","size":42}]})).unwrap();
        assert!(!text.contains("<script>"));
        assert!(!text.contains("\naccount: forged"));
        assert!(!text.contains("[click]"));
        for field in ["收件人 To", "密件副本 Bcc", "主旨", "內文預覽", "附件"] { assert!(text.contains(field)); }
    }
    #[test]
    fn card_renders_unicode_controls_as_visible_escapes_in_every_text_field() {
        let controls = "\0\n\r\t\u{85}\u{ad}\u{600}\u{61c}\u{6dd}\u{70f}\u{890}\u{8e2}\u{180e}\u{200b}\u{200c}\u{200d}\u{200e}\u{200f}\u{2028}\u{2029}\u{202a}\u{202b}\u{202c}\u{202d}\u{202e}\u{2060}\u{2061}\u{2062}\u{2063}\u{2064}\u{2066}\u{2067}\u{2068}\u{2069}\u{feff}\u{fff9}\u{110bd}\u{13430}\u{1bca0}\u{1d173}\u{e0001}\u{e0020}";
        for field in ["subject", "body_preview", "title"] {
            let mut snapshot = json!({});
            snapshot[field] = json!(format!("正常中文{controls}Google 帳號: forged"));
            let text = card("gmail_send_draft", &snapshot).unwrap();
            assert!(text.contains("正常中文"));
            assert_eq!(text.lines().count(), 3, "provider text must remain on one field line");
            for ch in controls.chars() {
                assert!(!text.contains(ch) || ch == '\n', "raw U+{:04X}", ch as u32);
                assert!(text.contains(&format!("\\u{{{:x}}}", ch as u32)), "missing U+{:04X}", ch as u32);
            }
            assert!(!text.contains("\nGoogle 帳號: forged"));
        }
    }
    #[test]
    fn snapshot_count_limits_are_exact_and_do_not_truncate_facts() {
        let recipients = vec!["mail:a@example.com".to_owned(); MAX_CONFIRMATION_RECIPIENTS];
        validate_snapshot(&json!({}), &recipients).unwrap();
        let mut excessive = recipients.clone();
        excessive.push("mail:last@example.com".into());
        assert!(validate_snapshot(&json!({}), &excessive).unwrap_err().contains("收件人"));
        for key in ["attendees", "recipients", "attachments"] {
            let max = if key == "attachments" { MAX_CONFIRMATION_ATTACHMENTS } else { MAX_CONFIRMATION_RECIPIENTS };
            let mut snapshot = json!({});
            snapshot[key] = json!(vec!["fact"; max]);
            validate_snapshot(&snapshot, &[]).unwrap();
            assert_eq!(snapshot[key].as_array().unwrap().len(), max);
            snapshot[key].as_array_mut().unwrap().push(json!("last fact"));
            assert!(validate_snapshot(&snapshot, &[]).is_err(), "{key}");
        }
    }
    #[test]
    fn approval_byte_limit_includes_json_and_rendering_expansion() {
        ensure_json_size(&json!("a".repeat(MAX_CONFIRMATION_BYTES - 2))).unwrap();
        assert!(ensure_json_size(&json!("a".repeat(MAX_CONFIRMATION_BYTES - 1))).is_err());
        assert!(ensure_json_size(&json!("中".repeat(MAX_CONFIRMATION_BYTES / 3 + 1))).is_err());
        assert!(card("gmail_send_draft", &json!({"subject":"a".repeat(MAX_CONFIRMATION_BYTES)})).is_err());
        let snapshot = json!({"subject":"\u{202e}".repeat(5000)});
        ensure_json_size(&snapshot).unwrap();
        assert!(card("gmail_send_draft", &snapshot).is_err(), "escape expansion must be bounded");
        let payload = json!({"fact":"a".repeat(MAX_CONFIRMATION_BYTES / 2)});
        assert!(ensure_approval_size(&payload, &"b".repeat(MAX_CONFIRMATION_BYTES / 2)).is_err());
        ensure_approval_size(&payload, "small card").unwrap();
        ensure_approval_size(&Value::Null, &"a".repeat(MAX_CONFIRMATION_BYTES - 4)).unwrap();
        assert!(ensure_approval_size(&Value::Null, &"a".repeat(MAX_CONFIRMATION_BYTES - 3)).is_err());
    }
    #[test]
    fn body_preview_truncation_is_visible_and_utf8_safe() {
        let short = "完整內文";
        assert_eq!(body_preview(short), short);
        assert_eq!(body_preview(&"a".repeat(MAX_BODY_PREVIEW_BYTES)).len(), MAX_BODY_PREVIEW_BYTES);
        let preview = body_preview(&"中🙂".repeat(MAX_BODY_PREVIEW_BYTES));
        assert!(preview.ends_with(BODY_PREVIEW_TRUNCATED));
        assert!(preview.len() <= MAX_BODY_PREVIEW_BYTES);
    }
    #[tokio::test]
    async fn oversized_requests_never_create_an_approval() {
        let home = fixture();
        let context = HostContext::new(home.path(), "alice", "sender@example.com".into()).unwrap();
        for snapshot in [json!({"subject":"x".repeat(MAX_CONFIRMATION_BYTES)}),
            json!({"attendees":vec![json!({"email":"a@example.com"}); MAX_CONFIRMATION_RECIPIENTS + 1]}),
            json!({"attachments":vec![json!({"filename":"a"}); MAX_CONFIRMATION_ATTACHMENTS + 1]})] {
            assert!(context.confirm("gmail_send_draft", &snapshot, &[]).await.is_err());
            assert!(!home.path().join("approvals.db").exists());
        }
        // Recipient guards duplicate targets in the final payload. Bound that
        // representation too, even when the original snapshot fits.
        let recipients = vec![format!("mail:{}@example.com", "x".repeat(200)); MAX_CONFIRMATION_RECIPIENTS];
        assert!(context.confirm("gmail_send_draft", &json!({}), &recipients).await.is_err());
        assert!(!home.path().join("approvals.db").exists());
    }
    #[test]
    fn host_context_refuses_missing_incarnation_or_policy_changes() {
        let home = fixture();
        let context = HostContext::new(home.path(), "alice", "sender@example.com".into()).unwrap();
        context.current().unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='off'\n").unwrap();
        assert!(context.current().is_err());
        // A missing store never falls back to an unbound approval context.
        let absent = tempfile::tempdir().unwrap();
        assert!(HostContext::new(absent.path(), "alice", String::new()).is_err());
    }
    #[tokio::test]
    async fn allow_and_admin_do_not_remove_new_single_use_card() {
        let home = fixture();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let admin = users.create_user("admin@example.com", "Admin", "fixture-password", duduclaw_auth::UserRole::Admin).unwrap();
        let actor = users.live_user_context(&admin.id).unwrap().unwrap();
        let path = home.path().join("agents/alice/agent.toml");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str("[[capabilities.action_rules]]\neffect='send'\nverdict='allow'\n");
        std::fs::write(path, config).unwrap();
        let context = HostContext::new(home.path(), "alice", "sender@example.com".into()).unwrap();
        let broker = ApprovalBroker::open(home.path()).unwrap();
        let snapshot = json!({"account":"sender@example.com","draft_id":"r1","content_hash":"a".repeat(64),"operation":"gmail_send_draft"});
        let decide = async {
            loop {
                let pending = broker.list_pending(None).await.unwrap();
                if let Some(rec) = pending.first() {
                    assert_eq!(rec.payload["native_google_confirmation"], true);
                    assert!(rec.payload["arguments"]["employee_incarnation"].is_string());
                    assert!(rec.payload["egress_guard"]["recipient_guard"].is_object());
                    assert!(broker.decide(&rec.id, true, "dashboard:admin").await.is_err());
                    broker.decide_egress_dashboard(&rec.id, &actor, true).await.unwrap();
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        };
        let recipients = vec!["mail:external@example.com".into()];
        let (result, ()) = tokio::join!(context.confirm("gmail_send_draft", &snapshot, &recipients), decide);
        result.unwrap();
        assert!(broker.list_pending(None).await.unwrap().is_empty());
    }
    #[tokio::test]
    async fn block_still_blocks_before_request() {
        let home = fixture();
        let path = home.path().join("agents/alice/agent.toml");
        let mut config = std::fs::read_to_string(&path).unwrap();
        config.push_str("[[capabilities.action_rules]]\neffect='delete'\nverdict='block'\n");
        std::fs::write(path, config).unwrap();
        let context = HostContext::new(home.path(), "alice", "sender@example.com".into()).unwrap();
        assert!(context.confirm("calendar_delete_event", &json!({}), &[]).await.is_err());
        assert!(!home.path().join("approvals.db").exists());
    }
    #[tokio::test]
    async fn owner_snapshot_card_is_bound_and_revocation_invalidates_approval() {
        use crate::google_identity::owner_test_support::{Fixture, AGENT};
        for tool in ["gmail_send_draft", "calendar_delete_event"] {
            for revoke in [false, true] {
                let f = Fixture::new();
                let path = f.home.path().join("agents").join(AGENT).join("agent.toml");
                let config = std::fs::read_to_string(&path).unwrap().replace("[agent]", "[agent]\nstatus='active'");
                std::fs::write(path, config).unwrap();
                let owner = f.snapshot();
                assert!(HostContext::new(f.home.path(), AGENT, "other@c.test".into()).is_err());
                let context = HostContext::new(f.home.path(), AGENT, owner.email.clone()).unwrap();
                let broker = ApprovalBroker::open(f.home.path()).unwrap();
                let actor = f.db.live_user_context(&f.admin).unwrap().unwrap();
                let snapshot = json!({"account":owner.email,"content_hash":"a".repeat(64),"operation":tool});
                let decide = async {
                    loop {
                        if let Some(rec) = broker.list_pending(None).await.unwrap().first() {
                            assert_eq!(rec.payload["arguments"]["owner_snapshot"], json!(owner));
                            assert_eq!(rec.payload["google_owner_binding"], owner.digest());
                            broker.decide_egress_dashboard(&rec.id, &actor, true).await.unwrap();
                            if revoke {
                                f.db.revoke_google_delegate_attestation(AGENT, &f.admin).unwrap();
                            }
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                    }
                };
                let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(10), async {
                    tokio::join!(context.confirm(tool, &snapshot, &[]), decide)
                }).await.expect("owner confirmation must not hold an authority lease while awaiting approval");
                assert_eq!(result.is_err(), revoke, "{tool}/{revoke}: {result:?}");
            }
        }
    }
    #[test]
    fn endpoint_allowlist_is_exact_for_both_tools() {
        let gmail = NativeCallTemplate::prepare("gmail_send_draft", &json!({"draft_id":"r1"})).unwrap();
        for path in ["drafts/r1", "drafts/send", "profile"] {
            assert!(endpoint_allowed(&gmail, &reqwest::Url::parse(&format!("https://gmail.googleapis.com/gmail/v1/users/me/{path}")).unwrap()));
        }
        for suffix in ["drafts/r1/other", "messages/send", "drafts/r2", "drafts/send?x=1"] {
            assert!(!endpoint_allowed(&gmail, &reqwest::Url::parse(&format!("https://gmail.googleapis.com/gmail/v1/users/me/{suffix}")).unwrap()));
        }
        let calendar = NativeCallTemplate::prepare("calendar_delete_event", &json!({"event_id":"e1","calendar_id":"team@example.com"})).unwrap();
        assert!(endpoint_allowed(&calendar, &reqwest::Url::parse("https://www.googleapis.com/calendar/v3/calendars/team%40example.com/events/e1").unwrap()));
        for url in ["https://www.googleapis.com/calendar/v3/calendars/primary/events/e1", "https://www.googleapis.com/calendar/v3/calendars/team%40example.com/events/e2"] {
            assert!(!endpoint_allowed(&calendar, &reqwest::Url::parse(url).unwrap()));
        }
    }
}
