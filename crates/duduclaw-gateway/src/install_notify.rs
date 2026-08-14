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

use crate::decision_action::DecisionSource;
use crate::decision_notify::DecisionCard;
use crate::install_requests::{DecideOutcome, InstallRequest, InstallRequestStore};

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

/// Resolve the "open in channel" target for an install request — the E8
/// reverse-handoff button on `InstallDetailPanel`, extending the pattern
/// already wired for tasks/approvals to the one object type it was missing
/// from (04-orca doc §4.1: E8 covers task/approval, not install). Not one of
/// `07-unified-decision-design.md` §6's lettered hand-off items (H1-H5) —
/// those are a distinct list; naming it "H3" would collide with that
/// document's own H3 (install ack wording). `InstallRequest` itself carries no
/// `notify_channel`/`notify_chat_id` columns (unlike `ApprovalRecord`/
/// `TaskRow`), so this checks, in order:
///
/// 1. **`decision_message_store`'s `install` namespace** — the exact
///    conversation a decision card was actually pushed to and (if any) the
///    message id, letting the deep link jump to that specific message.
/// 2. **The current stage's first approver with a verified channel
///    identity** (`approvers_for`) — falls back to *a* reachable approver
///    conversation when no card was ever recorded (e.g. filed before any
///    approver had linked a channel, or the record predates this feature).
///
/// Returns `(channel, chat_id, message_id)`, or `None` when neither resolves
/// — the caller must render no button rather than guess (fail closed / fail
/// quiet, same posture as `channel_link.rs`).
pub async fn resolve_channel_target(
    home_dir: &Path,
    db: &UserDb,
    req: &InstallRequest,
) -> Option<(String, String, Option<String>)> {
    let cards = crate::decision_message_store::list_card_messages(
        home_dir,
        DecisionSource::Install.namespace(),
        &req.id,
    );
    if let Some(card) = cards.into_iter().next() {
        return Some((card.channel, card.chat_id, Some(card.pushed.message_id)));
    }

    let users = db.list_users().ok()?;
    for approver in approvers_for(&users, req) {
        if let Ok(channels) = db.verified_channels_for_user(&approver.id) {
            if let Some(ident) = channels.into_iter().next() {
                return Some((ident.channel, ident.channel_user_id, None));
            }
        }
    }
    None
}

/// Render the zh-TW notification body for a request. The dashboard deep link
/// and the "this channel has no buttons" hint are appended by the shared
/// delivery path, so this is purely the description of what is being asked.
fn notify_body(req: &InstallRequest) -> String {
    let kind = if req.kind == "skill" { "Skill" } else { "MCP" };
    let findings = req.scan.as_array().map(|a| a.len()).unwrap_or(0);
    format!(
        "{prefix}\n🔔 安裝簽核申請\n\
         類型：{kind}\n\
         項目：{title}\n\
         申請人：{who}（{role}）\n\
         功能：{desc}\n\
         安全審查：風險 {risk}，{findings} 項發現\n\
         編號：{id}",
        prefix = crate::decision_notify::reason_prefix(DecisionSource::Install),
        title = req.title,
        who = if req.requester_email.is_empty() { &req.requester_id } else { &req.requester_email },
        role = zh_role(&req.requester_role),
        desc = duduclaw_core::truncate_chars(&req.description, 200),
        risk = req.risk_level,
        id = req.id,
    )
}

/// What to tell an approver whose channel cannot render inline buttons.
const NO_BUTTON_HINT: &str = "此通道無法顯示按鈕，請至儀表板的待辦決定頁同意或婉拒。";

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
    // A clickable deep link to the unified inbox — `None` when no
    // dashboard base URL is configured/derivable.
    let link = crate::deep_link::deep_link(home_dir, crate::deep_link::DeepLinkKind::Approval, &req.id);
    let body = notify_body(req);
    let card = DecisionCard {
        source: DecisionSource::Install,
        decision_id: &req.id,
        body: &body,
        link: link.as_deref(),
        no_button_hint: NO_BUTTON_HINT,
    };
    for approver in &approvers {
        let channels = match db.verified_channels_for_user(&approver.id) {
            Ok(c) => c,
            Err(e) => {
                warn!(user = %approver.id, error = %e, "install-notify: channel lookup failed");
                continue;
            }
        };
        for ident in channels {
            let candidates =
                crate::config_crypto::channel_dm_token_candidates(home_dir, &ident.channel).await;
            if candidates.is_empty() {
                info!(channel = %ident.channel, "install-notify: no bot token configured; skipping");
                continue;
            }
            for token in &candidates {
                if crate::decision_notify::deliver(
                    home_dir,
                    &http,
                    &ident.channel,
                    token,
                    &ident.channel_user_id,
                    &card,
                )
                .await
                {
                    break;
                }
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

/// Send plain text to one linked channel identity. Logs and swallows errors
/// (best-effort notification path).
///
/// Delegates to the shared sender so Google Chat and Teams keep working: the
/// generic `channel_sender` factory has no branch for those two (their
/// credentials live in home-dir config, not on a `ChannelTarget`) and falls
/// through to `NullSender`, whose `send_text` always returns `Ok(())` — a
/// message that was never sent would look identical to one that was.
async fn send_plain_text(
    home_dir: &Path,
    http: &reqwest::Client,
    channel: &str,
    chat_id: &str,
    text: &str,
) {
    let candidates = crate::config_crypto::channel_dm_token_candidates(home_dir, channel).await;
    if candidates.is_empty() {
        info!(channel = %channel, "install-notify: no bot token configured; skipping");
        return;
    }
    let mut sent = false;
    for token in &candidates {
        if crate::goal_notify::send_plain_text(home_dir, http, channel, token, chat_id, text).await {
            sent = true;
            break;
        }
    }
    if !sent {
        warn!(channel = %channel, "install-notify: send failed");
    }
}

/// First candidate DM token for a channel — global `[channels]` first, then
/// per-agent (`config_crypto::channel_dm_token_candidates`). Used by the
/// card-collapse resolvers, which need one deterministic token to edit the
/// already-delivered cards with; the delivery paths above instead try every
/// candidate until a send succeeds.
async fn global_channel_token(home_dir: &Path, channel: &str) -> Option<String> {
    crate::config_crypto::channel_dm_token_candidates(home_dir, channel)
        .await
        .into_iter()
        .next()
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
    let action = crate::decision_action::parse(action_data)?;
    if action.source != DecisionSource::Install {
        return None;
    }
    Some(apply_decision(home_dir, channel, channel_user_id, &action.id, action.approve()).await)
}

/// Apply an already-decoded sign-off to `install_requests`. Called by the
/// unified inbound router as well as this module's own thin wrapper.
///
/// Authorization here is strictly identity-based: an install sign-off must map
/// to a verified dashboard user with a Manager/Admin role, and the store
/// re-checks role + department for the current stage. There is deliberately no
/// "solo operator decides from the delivered destination" fallback — a
/// two-stage sign-off whose stages nobody is identified for has no meaning.
pub(crate) async fn apply_decision(
    home_dir: &Path,
    channel: &str,
    channel_user_id: &str,
    request_id: &str,
    approve: bool,
) -> Result<String, String> {
    // Open the user DB from the home dir (channel dispatchers don't carry
    // one). Existing file only — filing a decision must never conjure an auth
    // database as a side effect; an absent one simply means nobody is
    // identified, which is the same refusal an unmapped account gets.
    let Some(db) = crate::decision_notify::open_user_db(home_dir) else {
        return Err(crate::decision_notify::refusal_text(
            crate::decision_notify::PressAuth::DenyUnknown,
            "核准",
        ));
    };

    // Map the clicking channel account → a dashboard user. VERIFIED links only:
    // the doc comment always claimed this, but the lookup it used
    // (`find_user_id_by_channel`) deliberately also returns *pending* rows so
    // the OTP flow can find the binding it is about to confirm. An unverified
    // row means "someone typed this channel id into the binding form" — with
    // the old call, filing an unverified binding against a manager/admin
    // account was enough to inherit their approval rights from that chat.
    let user_id = match db.find_verified_user_id_by_channel(channel, channel_user_id) {
        Ok(Some(uid)) => uid,
        Ok(None) => {
            return Err(crate::decision_notify::refusal_text(
                crate::decision_notify::PressAuth::DenyUnknown,
                "核准",
            ))
        }
        Err(e) => return Err(format!("查詢身分失敗：{e}")),
    };
    let user = match db.get_user(&user_id) {
        Ok(Some(u)) if u.status == UserStatus::Active => u,
        _ => return Err("找不到有效的使用者身分".into()),
    };
    if !matches!(user.role, UserRole::Manager | UserRole::Admin) {
        return Err(crate::decision_notify::refusal_text(
            crate::decision_notify::PressAuth::DenyNotApprover,
            "核准",
        ));
    }

    let store = match InstallRequestStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return Err(format!("開啟申請資料庫失敗：{e}")),
    };
    let decider = format!("{}:{}", user.role, user.id);
    let dept = user.department.as_deref();
    let outcome = match store
        .decide(request_id, &decider, &user.role.to_string(), dept, approve, "")
        .await
    {
        Ok(o) => o,
        Err(e) => return Err(e),
    };

    match outcome {
        DecideOutcome::Denied => {
            if let Ok(Some(req)) = store.get(request_id).await {
                notify_requester(
                    home_dir,
                    &db,
                    &req,
                    &format!("🙅 您的安裝申請「{}」已婉拒。", req.title),
                )
                .await;
            }
            spawn_install_collapse(
                home_dir.to_path_buf(),
                request_id.to_string(),
                channel.to_string(),
                channel_user_id.to_string(),
                user.display_name.clone(),
                crate::decision_card::DecisionVerb::DeclinedInstall,
            );
            Ok("已婉拒此安裝申請。".into())
        }
        DecideOutcome::AdvancedToAdmin => {
            // Notify the next stage's approvers (admins).
            if let Ok(Some(req)) = store.get(request_id).await {
                notify_install_approvers(home_dir, &db, &req).await;
            }
            spawn_install_collapse(
                home_dir.to_path_buf(),
                request_id.to_string(),
                channel.to_string(),
                channel_user_id.to_string(),
                user.display_name.clone(),
                crate::decision_card::DecisionVerb::Approved,
            );
            Ok("已同意（主管關卡），現在等管理員同意。".into())
        }
        DecideOutcome::ReadyToExecute => {
            let req = match store.get(request_id).await {
                Ok(Some(r)) => r,
                _ => return Err("已同意，但讀取申請失敗，安裝未執行".into()),
            };
            spawn_install_collapse(
                home_dir.to_path_buf(),
                request_id.to_string(),
                channel.to_string(),
                channel_user_id.to_string(),
                user.display_name.clone(),
                crate::decision_card::DecisionVerb::Approved,
            );
            match apply_install_request(home_dir, &req).await {
                Ok(_) => {
                    let _ = store.mark_executed(request_id, true, None).await;
                    notify_requester(
                        home_dir,
                        &db,
                        &req,
                        &format!("✅ 您的安裝申請「{}」已同意並完成安裝。", req.title),
                    )
                    .await;
                    Ok(format!("已同意並完成安裝：{}", req.title))
                }
                Err(e) => {
                    let _ = store.mark_executed(request_id, false, Some(&e)).await;
                    notify_requester(
                        home_dir,
                        &db,
                        &req,
                        &format!("⚠️ 您的安裝申請「{}」已同意，但安裝執行失敗：{e}", req.title),
                    )
                    .await;
                    Ok(format!("已完成簽核，但安裝執行失敗：{e}"))
                }
            }
        }
    }
}

/// The one-line identifying summary regenerated fresh from the request
/// record — shared by every collapse path (channel press and dashboard
/// decision alike) rather than persisting/parsing the original push text.
async fn install_collapse_summary(home_dir: &Path, request_id: &str) -> String {
    match InstallRequestStore::open(home_dir).ok() {
        Some(s) => match s.get(request_id).await.ok().flatten() {
            Some(req) => format!(
                "🔔 安裝簽核：{}",
                duduclaw_core::truncate_chars(&req.title, 60)
            ),
            None => "🔔 安裝簽核申請".to_string(),
        },
        None => "🔔 安裝簽核申請".to_string(),
    }
}

/// Spawn a best-effort, fire-and-forget attempt to retire this request's
/// channel cards. Detached so a slow or unreachable channel API can never
/// delay or fail a decision that already landed.
///
/// An install sign-off is pushed to every eligible approver's every linked
/// channel, so a settled request routinely leaves several cards behind — all
/// of them are retired, not just the presser's copy.
fn spawn_install_collapse(
    home_dir: std::path::PathBuf,
    request_id: String,
    channel: String,
    channel_user_id: String,
    decider_name: String,
    verb: crate::decision_card::DecisionVerb,
) {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let decider = (!decider_name.trim().is_empty()).then_some(decider_name.as_str());
        let summary = install_collapse_summary(&home_dir, &request_id).await;
        // Install pushes go out on the global bot for each channel (they
        // address an approver's own linked account, not an agent's control
        // channel), so the token lookup is the global one.
        let home = home_dir.clone();
        crate::decision_card::collapse_all(
            &home_dir,
            &http,
            DecisionSource::Install.namespace(),
            &request_id,
            &summary,
            verb,
            decider,
            move |ch: String| {
                let home = home.clone();
                async move { global_channel_token(&home, &ch).await }
            },
            Some((channel.as_str(), channel_user_id.as_str())),
        )
        .await;
    });
}

/// Spawn a best-effort, fire-and-forget attempt to retire this request's
/// channel cards after a **dashboard** decision (`handlers.rs`'s
/// `install_requests.decide` RPC — H1 of the unified-decision hand-off, 07
/// §6). Mirrors [`spawn_install_collapse`] but the decider is a resolved
/// dashboard display name rather than a channel identity, and there is no
/// channel destination to fall back to on a total collapse miss — the
/// dashboard RPC already carries its own acknowledgement, so a miss stays
/// silent rather than pushing a new message anywhere.
///
/// `verb` is the caller's to compute: `approve == false` always denies
/// (`DeclinedInstall`); `approve == true` covers both the manager-stage and
/// final-stage outcomes, which read identically as `Approved` — same mapping
/// [`apply_decision`] uses for the channel path.
pub(crate) fn spawn_dashboard_collapse(
    home_dir: std::path::PathBuf,
    request_id: String,
    decider_name: Option<String>,
    verb: crate::decision_card::DecisionVerb,
) {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let summary = install_collapse_summary(&home_dir, &request_id).await;
        let home = home_dir.clone();
        crate::decision_card::collapse_all(
            &home_dir,
            &http,
            DecisionSource::Install.namespace(),
            &request_id,
            &summary,
            verb,
            decider_name.as_deref(),
            move |ch: String| {
                let home = home.clone();
                async move { global_channel_token(&home, &ch).await }
            },
            None,
        )
        .await;
    });
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
    fn notify_body_starts_with_the_reason_prefix() {
        // W1-6: line 1 is the canonical install-request reason.
        let body = notify_body(&req("employee", None, None));
        assert!(body.starts_with("📦 安裝申請\n"));
        assert!(body.contains("安裝簽核申請"));
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

    // ── E8: resolve_channel_target (InstallDetailPanel reverse handoff) ──

    #[tokio::test]
    async fn resolve_channel_target_prefers_a_recorded_card_over_approver_lookup() {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("auth.db")).unwrap();
        let admin = db.create_user("admin@x", "Admin", "pw", UserRole::Admin).unwrap();
        // The approver has a linked channel too — the recorded card must still win.
        db.bind_channel_identity(&admin.id, "telegram", "999", true).unwrap();

        let request = req("manager", None, None);
        crate::decision_message_store::record_card_message(
            dir.path(),
            DecisionSource::Install.namespace(),
            &request.id,
            "slack",
            "C123",
            &crate::decision_card::PushedMessage { edit_chat_id: "C123".into(), message_id: "m1".into() },
        );

        let target = resolve_channel_target(dir.path(), &db, &request).await;
        assert_eq!(target, Some(("slack".to_string(), "C123".to_string(), Some("m1".to_string()))));
    }

    #[tokio::test]
    async fn resolve_channel_target_falls_back_to_first_approver_channel_when_no_card_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("auth.db")).unwrap();
        let admin = db.create_user("admin@x", "Admin", "pw", UserRole::Admin).unwrap();
        db.bind_channel_identity(&admin.id, "telegram", "555", true).unwrap();

        let request = req("manager", None, None);
        let target = resolve_channel_target(dir.path(), &db, &request).await;
        assert_eq!(target, Some(("telegram".to_string(), "555".to_string(), None)));
    }

    #[tokio::test]
    async fn resolve_channel_target_none_when_no_card_and_no_approver_channel() {
        let dir = tempfile::tempdir().unwrap();
        let db = UserDb::new(&dir.path().join("auth.db")).unwrap();
        // Admin exists (so `approvers_for` isn't empty) but never linked a channel.
        db.create_user("admin@x", "Admin", "pw", UserRole::Admin).unwrap();

        let request = req("manager", None, None);
        assert_eq!(resolve_channel_target(dir.path(), &db, &request).await, None);
    }

    // ── H1: dashboard collapse summary (`spawn_dashboard_collapse`,
    // `handlers.rs`'s `install_requests.decide` RPC) ────────────

    #[tokio::test]
    async fn install_collapse_summary_regenerates_from_the_request_title() {
        let dir = tempfile::tempdir().unwrap();
        let store = InstallRequestStore::open(dir.path()).unwrap();
        let id = store
            .create("skill", "客戶名單整理", "d", "emp", "emp@x", "employee", None, "Low", &json!([]), &json!({}), 3600)
            .await
            .unwrap();
        let summary = install_collapse_summary(dir.path(), &id).await;
        assert!(summary.contains("客戶名單整理"));
    }

    #[tokio::test]
    async fn install_collapse_summary_degrades_to_a_generic_phrase_when_not_found() {
        let dir = tempfile::tempdir().unwrap();
        // Never filed: `open()` still succeeds (creates an empty db), `get()`
        // returns None — must degrade, not panic or return an empty string.
        let summary = install_collapse_summary(dir.path(), "does-not-exist").await;
        assert_eq!(summary, "🔔 安裝簽核申請");
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

    #[tokio::test]
    async fn only_claims_install_presses_and_accepts_the_legacy_encoding() {
        let dir = tempfile::tempdir().unwrap();
        // Other sources' buttons fall through so their handler sees them.
        for data in [
            "garbage",
            "duduclaw:approval_ok:a1",
            "duduclaw:goal_retry:t1",
            "duduclaw:autopilot_pause:r1",
            "duduclaw:decide:apv:ok:a1",
            "duduclaw:install_approve:", // id-less ⇒ fail-closed
        ] {
            assert!(
                decide_from_channel(dir.path(), "telegram", "u1", data).await.is_none(),
                "should not claim {data}"
            );
        }
        // Both encodings of an install press are claimed. With no users.db the
        // answer is a refusal, which is the point: an unidentified account
        // cannot sign off an install.
        for data in [
            "duduclaw:install_approve:r1".to_string(),
            crate::decision_action::encode(DecisionSource::Install, crate::decision_action::DecisionAct::Approve, "r1"),
        ] {
            let out = decide_from_channel(dir.path(), "telegram", "u1", &data).await;
            assert!(out.is_some(), "must claim {data}");
            assert!(out.unwrap().is_err(), "unidentified account must be refused: {data}");
        }
        // …and no auth database was conjured as a side effect.
        assert!(!dir.path().join("users.db").exists());
    }

    #[test]
    fn suspended_users_excluded() {
        let mut u = user("m1", UserRole::Manager, None);
        u.status = UserStatus::Suspended;
        let got = approvers_for(&[u], &req("employee", None, None));
        assert!(got.is_empty());
    }
}
