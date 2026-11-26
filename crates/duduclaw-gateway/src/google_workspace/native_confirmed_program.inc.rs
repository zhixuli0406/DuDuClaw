// Included in the private native program of each credential owner.
use crate::google_workspace::native_confirmation as confirmation_limits;

fn confirmation_error(message: String) -> GoogleApiError {
    GoogleApiError::Api { status: 403, message }
}
fn changed_snapshot() -> GoogleApiError {
    confirmation_error("Google 內容或帳號已改變，這次未執行；請重新呼叫並確認新卡片。".into())
}
fn unconfirmed_effect(_: GoogleApiError) -> GoogleApiError {
    GoogleApiError::Api { status: 502, message: "result_unconfirmed：Google 操作結果無法確認，請先到 Google 檢查，勿自動重送。".into() }
}
fn provider_string(value: &Value, key: &str) -> Result<String, GoogleApiError> {
    value.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
        .map(str::to_owned).ok_or_else(|| confirmation_error(format!("Google 未回傳 {key}，已拒絕操作。")))
}
fn inspect_snapshot<I: LocalIo>(io: &mut I, snapshot: &Value) -> Result<(), GoogleApiError> {
    match snapshot {
        Value::String(s) => io.reject_derived(s)?,
        Value::Array(a) => for v in a { inspect_snapshot(io, v)?; },
        Value::Object(o) => for v in o.values() { inspect_snapshot(io, v)?; },
        _ => {},
    }
    io.inspect_curated(snapshot)
}
fn mail_targets(headers: &Value) -> Result<Vec<String>, GoogleApiError> {
    confirmation_limits::ensure_json_size(headers).map_err(confirmation_error)?;
    let mut targets = Vec::new();
    // Refuse ambiguous/group/obsolete recipient syntax rather than omit a recipient.
    // Standard simple addresses and display-name <address> are supported.
    for key in ["To", "Cc", "Bcc"] {
        let rows = headers.as_array().ok_or_else(changed_snapshot)?;
        for row in rows.iter().filter(|h| h["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(key))) {
            let line = row["value"].as_str().ok_or_else(changed_snapshot)?;
            if line.chars().any(char::is_control) { return Err(changed_snapshot()); }
            for entry in line.split(',').filter(|s| !s.trim().is_empty()) {
                confirmation_limits::check_recipient_count(targets.len() + 1).map_err(confirmation_error)?;
                let entry = entry.trim();
                let email = if let Some((_, tail)) = entry.rsplit_once('<') {
                    tail.strip_suffix('>').filter(|s| !s.contains(['<', '>'])).ok_or_else(changed_snapshot)?
                } else { entry };
                duduclaw_auth::db::validate_account_email(email).map_err(|_| changed_snapshot())?;
                targets.push(duduclaw_core::recipient_policy::channel_recipient("mail", email).ok_or_else(changed_snapshot)?);
            }
        }
    }
    if targets.is_empty() { return Err(confirmation_error("草稿沒有有效收件人。".into())); }
    Ok(targets)
}
fn confirmation_attachments(payload: &Value) -> Result<Vec<GmailAttachment>, GoogleApiError> {
    // Count and bound borrowed filenames before the existing collector copies
    // metadata. Nested MIME parts share the same budget.
    fn visit<'a>(node: &'a Value, names: &mut Vec<&'a str>) -> Result<(), GoogleApiError> {
        if let Some(name) = node["filename"].as_str().filter(|s| !s.is_empty()) {
            confirmation_limits::check_attachment_count(names.len() + 1).map_err(confirmation_error)?;
            names.push(name);
            confirmation_limits::ensure_json_size(names).map_err(confirmation_error)?;
        }
        if let Some(parts) = node["parts"].as_array() {
            for part in parts { visit(part, names)?; }
        }
        Ok(())
    }
    visit(payload, &mut Vec::new())?;
    Ok(collect_attachments(payload))
}
async fn mailbox_account<I: LocalIo>(io: &mut I, template: &NativeCallTemplate) -> Result<String, GoogleApiError> {
    let profile = google_request(io, template, reqwest::Method::GET, &format!("{GMAIL_BASE}/profile"), &[], None).await?;
    let account = provider_string(&profile, "emailAddress")?;
    duduclaw_auth::db::validate_account_email(&account).map_err(|_| changed_snapshot())?;
    io.reject_derived(&account)?;
    Ok(account)
}
async fn draft_snapshot<I: LocalIo>(io: &mut I, template: &NativeCallTemplate, draft: &str) -> Result<(Value, String), GoogleApiError> {
    let endpoint = format!("{GMAIL_BASE}/drafts/{}", encode_path_component(draft));
    let full = google_request(io, template, reqwest::Method::GET, &endpoint, &[("format", "full".into())], None).await?;
    let raw = google_request(io, template, reqwest::Method::GET, &endpoint, &[("format", "raw".into())], None).await?;
    // Gmail replaces the immutable message ID when a draft is edited. The
    // display and raw MIME must therefore belong to the same message ID.
    if full["id"].as_str() != Some(draft) || raw["id"].as_str() != Some(draft)
        || full.pointer("/message/id").and_then(Value::as_str).is_none()
        || full.pointer("/message/id") != raw.pointer("/message/id") { return Err(changed_snapshot()); }
    let mime = provider_string(&raw["message"], "raw")?;
    let bytes = decode_b64url(&mime).filter(|b| !b.is_empty()).ok_or_else(changed_snapshot)?;
    io.reject_derived(&mime)?;
    io.reject_derived(&String::from_utf8_lossy(&bytes))?;
    // Only metadata, preview and the content digest leave the private owner.
    Ok((full, mime))
}
async fn confirmed_effect<I: LocalIo>(io: &mut I, template: &NativeCallTemplate) -> Result<Value, GoogleApiError> {
    use ParsedNativeArguments::*;
    match template.parsed() {
        GmailSend { draft } => {
            let account = mailbox_account(io, template).await?;
            if let Ok(context) = template.confirmation() {
                if !context.account.is_empty() && !context.account.eq_ignore_ascii_case(&account) { return Err(changed_snapshot()); }
            }
            let (full, raw) = draft_snapshot(io, template, draft).await?;
            let headers = &full["message"]["payload"]["headers"];
            let recipients = mail_targets(headers)?;
            let hash = duduclaw_core::google_identity::google_arguments_digest(&json!({"draft":draft,"raw":raw}));
            let snapshot = json!({"operation":"gmail_send_draft", "account":account, "draft_id":draft,
                "content_hash":hash, "from":header_value(headers,"From").unwrap_or_default(), "to":header_value(headers,"To").unwrap_or_default(),
                "cc":header_value(headers,"Cc").unwrap_or_default(), "bcc":header_value(headers,"Bcc").unwrap_or_default(),
                "subject":header_value(headers,"Subject").unwrap_or_default(),
                "body_preview":confirmation_limits::body_preview(&extract_message_body(&full["message"]["payload"])),
                "attachments":confirmation_attachments(&full["message"]["payload"])?, "recipients":recipients});
            confirmation_limits::validate_snapshot(&snapshot, &recipients).map_err(confirmation_error)?;
            inspect_snapshot(io, &snapshot)?;
            io.confirm(template, &snapshot, &recipients).await?;
            io.checkpoint()?;
            let current_account = mailbox_account(io, template).await?;
            let (current_full, current_raw) = draft_snapshot(io, template, draft).await?;
            if current_account != account || current_raw != raw || current_full["message"]["id"] != full["message"]["id"] {
                return Err(changed_snapshot());
            }
            let result = google_request(io, template, reqwest::Method::POST, &format!("{GMAIL_BASE}/drafts/send"), &[],
                Some(&json!({"id":draft,"message":{"raw":raw}}))).await.map_err(unconfirmed_effect)?;
            let id = response_id(io, &result, "id").map_err(unconfirmed_effect)?;
            Ok(json!({"status":"sent","id":id,"account":account}))
        }
        CalendarDelete { calendar, event, notify } => {
            let endpoint = format!("{CALENDAR_BASE}/calendars/{}/events/{}", encode_path_component(calendar), encode_path_component(event));
            let before = google_request(io, template, reqwest::Method::GET, &endpoint, &[], None).await?;
            confirmation_limits::check_recipient_count(before["attendees"].as_array().map_or(0, Vec::len)).map_err(confirmation_error)?;
            confirmation_limits::ensure_json_size(&[&before["summary"], &before["start"], &before["end"], &before["attendees"]]).map_err(confirmation_error)?;
            let etag = provider_string(&before, "etag")?;
            if before["status"] == "cancelled" || event_time(&before, "start").is_empty() || event_time(&before, "end").is_empty() {
                return Err(changed_snapshot());
            }
            if etag.chars().any(char::is_control) || etag.len() > 1024 || before["id"].as_str() != Some(event) {
                return Err(changed_snapshot());
            }
            let (account, metadata) = calendar_details(io, template, calendar).await?;
            confirmation_limits::ensure_json_size(&[&metadata["id"], &metadata["summary"], &metadata["timeZone"]]).map_err(confirmation_error)?;
            if let Ok(context) = template.confirmation() {
                if !context.account.is_empty() && !context.account.eq_ignore_ascii_case(&account) { return Err(changed_snapshot()); }
            }
            let recipients: Vec<_> = before["attendees"].as_array().into_iter().flatten().map(|a|
                duduclaw_core::recipient_policy::channel_recipient("mail", a["email"].as_str().unwrap_or_default()).unwrap_or_default()).collect();
            let snapshot = json!({"operation":"calendar_delete_event","account":account,"calendar":calendar,"event_id":event,
                "etag":etag,"title":before["summary"],"start":before["start"],"end":before["end"],
                "calendar_info":{"id":metadata["id"],"title":metadata["summary"],"timeZone":metadata["timeZone"]},
                "attendees":before.get("attendees").cloned().unwrap_or_else(|| json!([])),"whole_recurring_series":before["recurrence"].as_array().is_some_and(|a| !a.is_empty()),
                "notify_attendees":notify,"sendUpdates":if *notify {"all"} else {"none"},
                "content_hash":duduclaw_core::google_identity::google_arguments_digest(&before)});
            confirmation_limits::validate_snapshot(&snapshot, &recipients).map_err(confirmation_error)?;
            inspect_snapshot(io, &snapshot)?;
            io.confirm(template, &snapshot, &recipients).await?;
            io.checkpoint()?;
            let current = google_request(io, template, reqwest::Method::GET, &endpoint, &[], None).await?;
            let (current_account, current_metadata) = calendar_details(io, template, calendar).await?;
            if current != before || current_account != account || current_metadata != metadata { return Err(changed_snapshot()); }
            if let Ok(context) = template.confirmation() { context.current().map_err(confirmation_error)?; }
            let step = NativeStep { template, method:reqwest::Method::DELETE, url:endpoint,
                query:vec![("sendUpdates".into(), if *notify {"all"} else {"none"}.into())], body:None, if_match:Some(etag) };
            // 204 is a valid JSON-less success. The version precondition closes
            // the final read/delete race; 412 means a new confirmation is needed.
            let result = io.json(step).await;
            if matches!(&result, Err(GoogleApiError::Api { status:412, .. })) { return Err(changed_snapshot()); }
            result.map_err(unconfirmed_effect)?;
            io.checkpoint()?;
            Ok(json!({"status":"deleted","event_id":event,"calendar_id":calendar,"sendUpdates":if *notify {"all"} else {"none"}}))
        }
        _ => Err(io.refuse()),
    }
}

async fn calendar_details<I: LocalIo>(io: &mut I, template: &NativeCallTemplate, calendar: &str) -> Result<(String, Value), GoogleApiError> {
    let primary = google_request(io, template, reqwest::Method::GET,
        &format!("{CALENDAR_BASE}/users/me/calendarList/primary"), &[], None).await?;
    let account = provider_string(&primary, "id")?;
    duduclaw_auth::db::validate_account_email(&account).map_err(|_| changed_snapshot())?;
    let selected = google_request(io, template, reqwest::Method::GET,
        &format!("{CALENDAR_BASE}/users/me/calendarList/{}", encode_path_component(calendar)), &[], None).await?;
    let id = provider_string(&selected, "id")?;
    provider_string(&selected, "timeZone")?;
    if id != (if calendar == "primary" { account.as_str() } else { calendar }) { return Err(changed_snapshot()); }
    Ok((account, selected))
}
