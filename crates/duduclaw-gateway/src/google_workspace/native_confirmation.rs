//! Gateway-owned, single-use confirmation for irreversible native Google calls.
//! MIME remains in the credential owner's stack; approvals persist only a hash and a curated card.
use std::path::{Path, PathBuf};
use serde_json::{Value, json};
use super::native_call::NativeCallTemplate;
use crate::approval::{ApprovalBroker, ApprovalStatus, action_grant::{self, ActionGrant}};

pub(crate) struct HostContext {
    home: PathBuf,
    agent: String,
    pub(crate) account: String,
    policy: String,
    incarnation: String,
    recipient_policy: String,
}
impl HostContext {
    pub(crate) fn new(home: &Path, agent: &str, account: String) -> Result<Self, String> {
        if !duduclaw_core::is_valid_agent_id(agent) { return Err("Invalid employee".into()); }
        let policy = crate::approval::strict_policy_revision(home, agent)?;
        let incarnation = Self::incarnation(home, agent)?;
        let recipient_policy = Self::recipient_policy(home, agent)?;
        Ok(Self { home: home.to_owned(), agent: agent.into(), account, policy, incarnation, recipient_policy })
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
        if Self::recipient_policy(&self.home, &self.agent)? != self.recipient_policy
            || Self::incarnation(&self.home, &self.agent)? != self.incarnation
            || crate::approval::strict_policy_revision(&self.home, &self.agent)? != self.policy {
            return Err("政策已改變，請重新確認。".into());
        }
        Ok(())
    }
    pub(crate) async fn confirm(&self, tool: &str, snapshot: &Value, recipients: &[String]) -> Result<(), String> {
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
        let grant = ActionGrant::build(&self.agent, tool, &payload);
        payload[action_grant::ACTION_GRANT_KEY] = grant.to_json();
        let broker = ApprovalBroker::open(&self.home)?;
        let summary = card(tool, snapshot);
        let id = broker.request(&self.agent, "mcp_call", &summary, payload, 90).await?;
        if broker.await_decision(&id, std::time::Duration::from_millis(500)).await? != ApprovalStatus::Approved {
            return Err("這次 Google 操作未獲核准，未執行變更。".into());
        }
        self.current()?;
        action_grant::verify_and_consume(&broker, &id, &grant).await
            .map_err(|_| "核准已失效或已使用，請重新確認。".to_owned())
    }
}

/// Escape all untrusted card data, including Markdown and line controls.
/// Each value is quoted JSON, so provider text cannot impersonate a card label.
pub(crate) fn card(tool: &str, snapshot: &Value) -> String {
    fn escaped(value: &Value) -> String {
        let text = value.to_string().replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
        let mut out = String::new();
        for ch in text.chars() {
            if matches!(ch, '*' | '_' | '`' | '[' | ']' | '#' | '!') { out.push('\\'); }
            out.push(ch);
        }
        out
    }
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
        }
    }
    text
}

pub(crate) fn calendar_endpoint(template: &NativeCallTemplate, url: &reqwest::Url) -> bool {
    let super::native_call::ParsedNativeArguments::CalendarDelete { calendar, event, .. } = template.parsed() else { return false; };
    url.host_str() == Some("www.googleapis.com") && [
        format!("/calendar/v3/calendars/{}/events/{}", super::encode_path_component(calendar), super::encode_path_component(event)),
        "/calendar/v3/users/me/calendarList/primary".into(),
        format!("/calendar/v3/users/me/calendarList/{}", super::encode_path_component(calendar)),
    ].iter().any(|path| path == url.path())
}

/// New tools get exact paths; existing tools retain their existing allowlists.
pub(crate) fn endpoint_allowed(template: &NativeCallTemplate, url: &reqwest::Url) -> bool {
    use super::native_call::ParsedNativeArguments::*;
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

    fn fixture() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join("agents/alice")).unwrap();
        std::fs::write(home.path().join("agents/alice/agent.toml"), "[agent]\nname='Alice'\n[capabilities]\nrecipients=['mail:alice@example.com']\n").unwrap();
        std::fs::write(home.path().join("config.toml"), "[security]\nrecipient_guard='ask'\n").unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        users.ensure_google_employee_nonce("alice").unwrap();
        home
    }
    #[test]
    fn card_escapes_provider_content_and_contains_explicit_fields() {
        let text = card("gmail_send_draft", &json!({"to":"<script>alert(1)</script>\naccount: forged",
            "bcc":"hidden@example.com", "subject":"[click](https://evil.test)", "body_preview":"**approve**", "attachments":[{"filename":"a.txt","size":42}]}));
        assert!(!text.contains("<script>"));
        assert!(!text.contains("\naccount: forged"));
        assert!(!text.contains("[click]"));
        for field in ["收件人 To", "密件副本 Bcc", "主旨", "內文預覽", "附件"] { assert!(text.contains(field)); }
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
                    broker.decide(&rec.id, true, "dashboard:admin").await.unwrap();
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
