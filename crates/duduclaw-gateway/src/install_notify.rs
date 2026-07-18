//! Channel-side notification + decision for install-approval requests.
//!
//! Two directions, both free functions (they open the stores from `home_dir`
//! so they work from the WS handler AND from channel inbound dispatchers,
//! neither of which shares a `MethodHandler`):
//!
//! - **Outbound** [`notify_install_approvers`] — when a request is filed or
//!   advances a stage, proactively DM the humans who can act on it, on their
//!   linked channel (`channel_identities`). Telegram / Slack / Discord / LINE
//!   get inline approve/deny buttons; any other linked channel gets text plus
//!   a dashboard hint via the proven `channel_sender` path. The requester is
//!   DM'd their request's final outcome via [`notify_requester`].
//! - **Inbound** [`decide_from_channel`] — a button press / postback carrying
//!   `duduclaw:install_approve|deny:{id}` maps the clicking channel account
//!   back to a dashboard user, authorizes by that user's role + department
//!   (same rules as the dashboard), and decides the request. On final approval
//!   it applies the install ([`apply_install_request`]).
//!
//! Everything is best-effort and fail-soft: a missing token / unlinked account
//! / send error is logged, never panics, never blocks the request.

use std::path::Path;

use serde_json::json;
use tracing::{info, warn};

use duduclaw_auth::models::{User, UserRole, UserStatus};
use duduclaw_auth::UserDb;

use crate::install_requests::{DecideOutcome, InstallRequest, InstallRequestStore};

/// Channels that can render inline approve/deny buttons today. All four have
/// their inbound dispatchers wired to [`decide_from_channel`].
fn channel_supports_buttons(channel: &str) -> bool {
    matches!(channel, "telegram" | "slack" | "discord" | "line")
}

/// The approver users for a request's CURRENT stage.
///
/// - awaiting the manager gate (employee request, no manager yet): managers in
///   the requester's department (or ALL managers if the requester has no
///   department — the same graceful fallback the store enforces).
/// - awaiting the admin gate: all admins.
///
/// Suspended / offboarded users are excluded. The requester themself is never
/// returned (you don't approve your own request).
pub fn approvers_for(users: &[User], req: &InstallRequest) -> Vec<User> {
    let req_dept = req
        .requester_department
        .as_deref()
        .map(str::trim)
        .filter(|d| !d.is_empty());
    let needs_manager = req.requester_role == "employee" && req.manager_by.is_none();

    users
        .iter()
        .filter(|u| u.status == UserStatus::Active && u.id != req.requester_id)
        .filter(|u| {
            if needs_manager {
                // manager gate → a manager in the same department (or any
                // manager when the request has no department)
                u.role == UserRole::Manager
                    && match req_dept {
                        None => true,
                        Some(d) => u
                            .department
                            .as_deref()
                            .map(str::trim)
                            .filter(|md| !md.is_empty())
                            .map(|md| md.eq_ignore_ascii_case(d))
                            .unwrap_or(false),
                    }
            } else {
                // admin gate
                u.role == UserRole::Admin
            }
        })
        .cloned()
        .collect()
}

/// Render the zh-TW notification body for a request.
fn notify_body(req: &InstallRequest, needs_button_hint: bool) -> String {
    let kind = if req.kind == "skill" { "Skill" } else { "MCP" };
    let findings = req.scan.as_array().map(|a| a.len()).unwrap_or(0);
    let mut body = format!(
        "🔔 安裝簽核申請\n\
         類型：{kind}\n\
         項目：{title}\n\
         申請人：{who}（{role}）\n\
         功能：{desc}\n\
         安全審查：風險 {risk}，{findings} 項發現\n\
         編號：{id}",
        title = req.title,
        who = if req.requester_email.is_empty() { &req.requester_id } else { &req.requester_email },
        role = zh_role(&req.requester_role),
        desc = duduclaw_core::truncate_chars(&req.description, 200),
        risk = req.risk_level,
        id = req.id,
    );
    if needs_button_hint {
        // Channels without inline buttons: point the approver at the dashboard.
        body.push_str("\n\n請至儀表板「安裝簽核申請」頁核准或退回。");
    }
    body
}

fn zh_role(role: &str) -> &str {
    match role {
        "employee" => "員工",
        "manager" => "主管",
        "admin" => "管理員",
        other => other,
    }
}

/// Proactively notify the approvers of `req` on their linked channels.
/// Best-effort; logs and swallows every delivery error.
pub async fn notify_install_approvers(home_dir: &Path, db: &UserDb, req: &InstallRequest) {
    let users = match db.list_users() {
        Ok(u) => u,
        Err(e) => {
            warn!(error = %e, "install-notify: cannot list users; skipping notification");
            return;
        }
    };
    let approvers = approvers_for(&users, req);
    if approvers.is_empty() {
        info!(request = %req.id, "install-notify: no eligible approver with a linked channel");
        return;
    }

    let http = reqwest::Client::new();
    for approver in &approvers {
        let channels = match db.verified_channels_for_user(&approver.id) {
            Ok(c) => c,
            Err(e) => {
                warn!(user = %approver.id, error = %e, "install-notify: channel lookup failed");
                continue;
            }
        };
        for ident in channels {
            let Some(token) = global_channel_token(home_dir, &ident.channel).await else {
                info!(channel = %ident.channel, "install-notify: no bot token configured; skipping");
                continue;
            };
            let has_buttons = channel_supports_buttons(&ident.channel);
            if has_buttons {
                let body = notify_body(req, false);
                if let Err(e) = send_with_buttons(
                    &http,
                    &ident.channel,
                    &token,
                    &ident.channel_user_id,
                    &body,
                    &req.id,
                )
                .await
                {
                    warn!(user = %approver.id, channel = %ident.channel, error = %e,
                          "install-notify: button send failed");
                }
            } else {
                // No inline buttons on this channel → text + dashboard hint.
                let body = notify_body(req, true);
                send_plain_text(home_dir, &http, &ident.channel, &ident.channel_user_id, &body)
                    .await;
            }
        }
    }
}

/// Notify the requester of their request's FINAL outcome (approved+executed /
/// approved-but-failed / denied) on their linked channels. Best-effort.
pub async fn notify_requester(home_dir: &Path, db: &UserDb, req: &InstallRequest, text: &str) {
    let channels = match db.verified_channels_for_user(&req.requester_id) {
        Ok(c) => c,
        Err(e) => {
            warn!(user = %req.requester_id, error = %e, "install-notify: requester channel lookup failed");
            return;
        }
    };
    let http = reqwest::Client::new();
    for ident in channels {
        send_plain_text(home_dir, &http, &ident.channel, &ident.channel_user_id, text).await;
    }
}

/// Send plain text to one linked channel identity via the shared sender
/// factory. Logs and swallows errors (best-effort notification path).
async fn send_plain_text(
    home_dir: &Path,
    http: &reqwest::Client,
    channel: &str,
    chat_id: &str,
    text: &str,
) {
    let Some(token) = global_channel_token(home_dir, channel).await else {
        info!(channel = %channel, "install-notify: no bot token configured; skipping");
        return;
    };
    let target = crate::channel_sender::ChannelTarget {
        channel_type: channel.to_string(),
        chat_id: chat_id.to_string(),
        token,
        extra_id: None,
    };
    let sender = crate::channel_sender::create_sender(&target, http.clone());
    if let Err(e) = sender.send_text(text).await {
        warn!(channel = %channel, error = %e, "install-notify: send failed");
    }
}

/// Read the global bot token for a channel, using the same channel → config
/// field mapping as OTP delivery (`telegram_bot_token`, `line_channel_token`,
/// `discord_bot_token`, `slack_bot_token`). A naive `{channel}_bot_token`
/// would silently miss LINE (whose field is `line_channel_token`).
async fn global_channel_token(home_dir: &Path, channel: &str) -> Option<String> {
    let field = crate::otp_delivery::token_field(channel)?;
    crate::config_crypto::read_encrypted_config_field(home_dir, "channels", field)
        .await
        .filter(|t| !t.is_empty())
}

/// Send the notification with inline approve/deny buttons on one of the four
/// button-capable channels.
async fn send_with_buttons(
    http: &reqwest::Client,
    channel: &str,
    token: &str,
    chat_id: &str,
    text: &str,
    request_id: &str,
) -> Result<(), String> {
    match channel {
        "telegram" => {
            let url = format!("https://api.telegram.org/bot{token}/sendMessage");
            let body = json!({
                "chat_id": chat_id,
                "text": text,
                "reply_markup": crate::channel_format::telegram_approval_buttons(request_id),
            });
            let resp = http.post(&url).json(&body).send().await.map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("telegram HTTP {}", resp.status()));
            }
            Ok(())
        }
        "slack" => {
            // chat.postMessage to the linked user id opens/reuses the bot DM.
            let body = json!({
                "channel": chat_id,
                "text": text,
                "blocks": [
                    { "type": "section", "text": { "type": "mrkdwn", "text": text } },
                    crate::channel_format::slack_approval_buttons(request_id),
                ],
            });
            let resp = http
                .post("https://slack.com/api/chat.postMessage")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            let data: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
            if data.get("ok").and_then(|v| v.as_bool()) != Some(true) {
                return Err(format!(
                    "slack chat.postMessage: {}",
                    data.get("error").and_then(|v| v.as_str()).unwrap_or("unknown")
                ));
            }
            Ok(())
        }
        "discord" => {
            // The linked id is the USER id — open (or reuse) the bot↔user DM
            // channel first. If the call fails, fall back to treating the id
            // as a channel id directly (older bindings may store one).
            let dm_channel = match http
                .post("https://discord.com/api/v10/users/@me/channels")
                .header("Authorization", format!("Bot {token}"))
                .json(&json!({ "recipient_id": chat_id }))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => resp
                    .json::<serde_json::Value>()
                    .await
                    .ok()
                    .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
                    .unwrap_or_else(|| chat_id.to_string()),
                _ => chat_id.to_string(),
            };
            let url = format!("https://discord.com/api/v10/channels/{dm_channel}/messages");
            let body = json!({
                "content": text,
                "components": [crate::channel_format::discord_approval_buttons(request_id)],
            });
            let resp = http
                .post(&url)
                .header("Authorization", format!("Bot {token}"))
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("discord HTTP {}", resp.status()));
            }
            Ok(())
        }
        "line" => {
            let body = json!({
                "to": chat_id,
                "messages": [{
                    "type": "text",
                    "text": text,
                    "quickReply": crate::channel_format::line_approval_quick_reply(request_id),
                }],
            });
            let resp = http
                .post("https://api.line.me/v2/bot/message/push")
                .bearer_auth(token)
                .json(&body)
                .send()
                .await
                .map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("line HTTP {}", resp.status()));
            }
            Ok(())
        }
        other => Err(format!("channel {other} has no button sender")),
    }
}

/// Handle an install-approval action from a channel.
///
/// Returns:
/// - `None` — `action_data` is not an install-approval action (the caller's
///   dispatcher should fall through to its other handlers).
/// - `Some(Ok(msg))` — decision handled; `msg` is the zh-TW ack to show.
/// - `Some(Err(msg))` — an error to show the user (unauthorized / not found).
pub async fn decide_from_channel(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    action_data: &str,
) -> Option<Result<String, String>> {
    let (request_id, approve) = crate::channel_format::parse_install_approval_action(action_data)?;

    // Open the user DB from the home dir (channel dispatchers don't carry one).
    let db = match UserDb::new(&home_dir.join("users.db")) {
        Ok(d) => d,
        Err(e) => return Some(Err(format!("開啟使用者資料庫失敗：{e}"))),
    };

    // Map the clicking channel account → a dashboard user (verified link only).
    let user_id = match db.find_user_id_by_channel(channel, channel_user_id) {
        Ok(Some(uid)) => uid,
        Ok(None) => {
            return Some(Err(
                "此帳號尚未連結儀表板身分，無法核准。請先於儀表板以此通道登入綁定。".into(),
            ))
        }
        Err(e) => return Some(Err(format!("查詢身分失敗：{e}"))),
    };
    let user = match db.get_user(&user_id) {
        Ok(Some(u)) if u.status == UserStatus::Active => u,
        _ => return Some(Err("找不到有效的使用者身分".into())),
    };
    if !matches!(user.role, UserRole::Manager | UserRole::Admin) {
        return Some(Err("您沒有核准權限".into()));
    }

    let store = match InstallRequestStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return Some(Err(format!("開啟申請資料庫失敗：{e}"))),
    };
    let decider = format!("{}:{}", user.role, user.id);
    let dept = user.department.as_deref();
    let outcome = match store
        .decide(&request_id, &decider, &user.role.to_string(), dept, approve, "")
        .await
    {
        Ok(o) => o,
        Err(e) => return Some(Err(e)),
    };

    match outcome {
        DecideOutcome::Denied => {
            if let Ok(Some(req)) = store.get(&request_id).await {
                notify_requester(
                    home_dir,
                    &db,
                    &req,
                    &format!("❌ 您的安裝申請「{}」已被退回。", req.title),
                )
                .await;
            }
            Some(Ok("已退回此安裝申請。".into()))
        }
        DecideOutcome::AdvancedToAdmin => {
            // Notify the next stage's approvers (admins).
            if let Ok(Some(req)) = store.get(&request_id).await {
                notify_install_approvers(home_dir, &db, &req).await;
            }
            Some(Ok("已核准（主管關卡），已轉交管理員做最終核准。".into()))
        }
        DecideOutcome::ReadyToExecute => {
            let req = match store.get(&request_id).await {
                Ok(Some(r)) => r,
                _ => return Some(Err("已核准，但讀取申請失敗，安裝未執行".into())),
            };
            match apply_install_request(home_dir, &req).await {
                Ok(_) => {
                    let _ = store.mark_executed(&request_id, true, None).await;
                    notify_requester(
                        home_dir,
                        &db,
                        &req,
                        &format!("✅ 您的安裝申請「{}」已核准並完成安裝。", req.title),
                    )
                    .await;
                    Some(Ok(format!("已核准並安裝：{}", req.title)))
                }
                Err(e) => {
                    let _ = store.mark_executed(&request_id, false, Some(&e)).await;
                    notify_requester(
                        home_dir,
                        &db,
                        &req,
                        &format!("⚠️ 您的安裝申請「{}」已核准，但安裝執行失敗：{e}", req.title),
                    )
                    .await;
                    Some(Ok(format!("已完成簽核，但安裝執行失敗：{e}")))
                }
            }
        }
    }
}

/// Reduce an attacker-influenced name to a safe temp-file stem: keep only
/// `[A-Za-z0-9._-]`, strip leading dots, cap at 64 chars, never empty.
/// Coding convention #1/#2: a frontmatter `name:` must never traverse paths.
pub(crate) fn sanitize_tmp_file_stem(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        .skip_while(|c| *c == '.')
        .take(64)
        .collect();
    if cleaned.is_empty() { "skill".to_string() } else { cleaned }
}

/// Apply an approved install to disk (skill file / `.mcp.json` entry).
///
/// Free-function twin of `MethodHandler::execute_approved_install` MINUS the
/// live `AgentRegistry` rescan (which needs the shared handler): a channel
/// path has no handler, so a skill installed this way is hot-loaded on the
/// gateway's next registry scan rather than instantly. Re-scans fail-closed so
/// a payload whose risk changed since filing is still blocked here.
pub async fn apply_install_request(
    home_dir: &Path,
    req: &InstallRequest,
) -> Result<serde_json::Value, String> {
    match req.kind.as_str() {
        "skill" => {
            let scope = req.payload.get("scope").and_then(|v| v.as_str()).unwrap_or("");
            let content = req.payload.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if scope.is_empty() || content.is_empty() {
                return Err("request payload missing scope/content".into());
            }
            let scan = crate::skill_lifecycle::security_scanner::scan_skill(content, None);
            if !scan.passed {
                return Err(format!("re-scan rejected skill: risk {:?}", scan.risk_level));
            }
            let skill_name = content
                .lines()
                .find(|l| l.starts_with("name:"))
                .and_then(|l| l.strip_prefix("name:"))
                .map(|n| n.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string());

            let tmp_dir = std::env::temp_dir().join("duduclaw-skill-install");
            std::fs::create_dir_all(&tmp_dir).map_err(|e| format!("create temp dir: {e}"))?;
            // The frontmatter `name:` is attacker-influenced — never let it
            // shape a filesystem path (a `name: ../../x` must not escape the
            // temp dir). The installed skill keeps its parsed frontmatter
            // name; only the throwaway temp filename is sanitized.
            let tmp_file = tmp_dir.join(format!("{}.md", sanitize_tmp_file_stem(&skill_name)));
            std::fs::write(&tmp_file, content).map_err(|e| format!("write temp file: {e}"))?;
            let quarantine = home_dir.join("quarantine");
            let result = if scope == "global" {
                duduclaw_agent::skill_loader::install_skill_global(&tmp_file, home_dir, &quarantine).await
            } else if let Some(dept) = scope.strip_prefix("department:") {
                if !duduclaw_core::is_valid_department(dept) {
                    let _ = std::fs::remove_file(&tmp_file);
                    return Err("invalid department in scope".into());
                }
                duduclaw_agent::skill_loader::install_skill_department(&tmp_file, home_dir, dept, &quarantine).await
            } else {
                if !crate::handlers::is_valid_agent_id(scope) {
                    let _ = std::fs::remove_file(&tmp_file);
                    return Err("invalid agent_id for scope".into());
                }
                let dir = home_dir.join("agents").join(scope).join("SKILLS");
                duduclaw_agent::skill_loader::install_skill(&tmp_file, &dir, &quarantine).await
            };
            let _ = std::fs::remove_file(&tmp_file);
            let parsed = result?;
            Ok(json!({ "skill_name": parsed.meta.name, "scope": scope }))
        }
        "mcp" => {
            use duduclaw_agent::mcp_template::{add_server_to_config, McpServerDef};
            let agent_id = req.payload.get("agent_id").and_then(|v| v.as_str()).unwrap_or("");
            let server_name = req.payload.get("server_name").and_then(|v| v.as_str()).unwrap_or("");
            let def: McpServerDef = req
                .payload
                .get("server_def")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .ok_or_else(|| "request payload missing server_def".to_string())?;
            // Same fail-closed identifier validation as the dashboard twin
            // (`execute_approved_install`) — the ids shape filesystem paths.
            if !crate::handlers::is_valid_agent_id(agent_id)
                || !crate::mcp_scan::is_valid_mcp_server_name(server_name)
            {
                return Err("invalid agent_id/server_name in payload".into());
            }
            let scan = crate::mcp_scan::scan_mcp_server_def(server_name, &def);
            if !scan.passed {
                return Err(format!("re-scan rejected MCP server: risk {:?}", scan.risk_level));
            }
            let agent_dir = home_dir.join("agents").join(agent_id);
            if !agent_dir.is_dir() {
                return Err(format!("agent '{agent_id}' not found"));
            }
            let ad = agent_dir.clone();
            let sn = server_name.to_string();
            let d = def.clone();
            tokio::task::spawn_blocking(move || add_server_to_config(&ad, &sn, &d))
                .await
                .map_err(|e| format!("join: {e}"))??;
            Ok(json!({ "server_name": server_name, "agent_id": agent_id }))
        }
        other => Err(format!("unknown request kind: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(id: &str, role: UserRole, dept: Option<&str>) -> User {
        User {
            id: id.into(),
            email: format!("{id}@x"),
            display_name: id.into(),
            role,
            status: UserStatus::Active,
            created_at: "".into(),
            updated_at: "".into(),
            last_login: None,
            must_change_password: false,
            department: dept.map(str::to_string),
        }
    }

    fn req(role: &str, dept: Option<&str>, manager_by: Option<&str>) -> InstallRequest {
        InstallRequest {
            id: "r1".into(),
            kind: "skill".into(),
            title: "t".into(),
            description: "d".into(),
            requester_id: "emp".into(),
            requester_email: "emp@x".into(),
            requester_role: role.into(),
            requester_department: dept.map(str::to_string),
            risk_level: "Low".into(),
            scan: json!([]),
            payload: json!({}),
            status: crate::install_requests::RequestStatus::Pending,
            manager_by: manager_by.map(str::to_string),
            manager_at: None,
            admin_by: None,
            admin_at: None,
            decided_reason: None,
            executed: false,
            execute_error: None,
            created_at: "".into(),
            ttl_seconds: 3600,
        }
    }

    #[test]
    fn employee_no_dept_routes_to_all_managers() {
        let users = vec![
            user("m1", UserRole::Manager, Some("sales")),
            user("m2", UserRole::Manager, None),
            user("a1", UserRole::Admin, None),
            user("emp", UserRole::Employee, None),
        ];
        let got = approvers_for(&users, &req("employee", None, None));
        let ids: Vec<_> = got.iter().map(|u| u.id.as_str()).collect();
        assert!(ids.contains(&"m1") && ids.contains(&"m2"));
        assert!(!ids.contains(&"a1")); // admins are the second gate, not the first
        assert!(!ids.contains(&"emp")); // never self
    }

    #[test]
    fn employee_with_dept_routes_to_same_dept_manager_only() {
        let users = vec![
            user("m_sales", UserRole::Manager, Some("Sales")),
            user("m_eng", UserRole::Manager, Some("eng")),
        ];
        let got = approvers_for(&users, &req("employee", Some("sales"), None));
        let ids: Vec<_> = got.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["m_sales"]); // case-insensitive dept match
    }

    #[test]
    fn manager_stage_routes_to_admins() {
        // employee request already manager-signed → awaiting admin
        let users = vec![
            user("m1", UserRole::Manager, None),
            user("a1", UserRole::Admin, None),
        ];
        let got = approvers_for(&users, &req("employee", None, Some("m1")));
        let ids: Vec<_> = got.iter().map(|u| u.id.as_str()).collect();
        assert_eq!(ids, vec!["a1"]);
    }

    #[test]
    fn manager_requester_routes_to_admins() {
        let users = vec![user("a1", UserRole::Admin, None)];
        let got = approvers_for(&users, &req("manager", None, None));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, "a1");
    }

    #[test]
    fn tmp_file_stem_never_traverses() {
        assert_eq!(sanitize_tmp_file_stem("my-skill"), "my-skill");
        assert_eq!(sanitize_tmp_file_stem("../../etc/passwd"), "etcpasswd");
        assert_eq!(sanitize_tmp_file_stem("..\\..\\x"), "x");
        assert_eq!(sanitize_tmp_file_stem("a/b/c"), "abc");
        assert_eq!(sanitize_tmp_file_stem(""), "skill");
        assert_eq!(sanitize_tmp_file_stem("危險名稱"), "skill");
        assert!(sanitize_tmp_file_stem(&"x".repeat(200)).len() <= 64);
    }

    #[test]
    fn suspended_users_excluded() {
        let mut u = user("m1", UserRole::Manager, None);
        u.status = UserStatus::Suspended;
        let got = approvers_for(&[u], &req("employee", None, None));
        assert!(got.is_empty());
    }
}
