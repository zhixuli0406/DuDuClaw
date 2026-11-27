//! Domain-bound knowledge cards. Generic channel approval paths still refuse.
use std::path::Path;
use duduclaw_auth::{UserRole, UserStatus};
use duduclaw_knowledge::{acl::{self, PrincipalKind, Role}, requester::ResolvedUser, schema};
use serde_json::{Value, json};
use crate::approval::{ApprovalRecord, DecisionContext};
use super::add_source_approval as approval;

/// A verified identity is still not a group address. Refuse unknown transport
/// shapes rather than showing a preview at an arbitrary configured destination.
pub(crate) fn private_destination(channel: &str, id: &str) -> bool {
    match channel {
        "telegram" | "discord" => id.parse::<u64>().is_ok_and(|id| id > 0),
        "line" => id.strip_prefix('U').is_some_and(|rest|
            rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_hexdigit())),
        "slack" => id.strip_prefix('U').or_else(|| id.strip_prefix('W')).is_some_and(|rest|
            !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric())),
        // These addresses only come from the authenticated HTTP DM table,
        // never from a manually entered users/... channel identity.
        "googlechat" => crate::google_workspace::chat_read::valid_space(id),
        _ => false,
    }
}

/// Capture direct human Owners and verified DM addresses, never the origin.
/// Multiple Owners still require approval, and each may receive a bound card.
pub(crate) fn bind_owner(home: &Path, conn: &rusqlite::Connection, payload: &mut Value) {
    let Some(cid) = payload["collection_id"].as_str() else { return; };
    let Ok((acl, _)) = acl::get_acl(conn, cid) else { return; };
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else { return; };
    let mut bindings = vec![];
    for entry in acl.iter().filter(|a| a.kind == PrincipalKind::User && a.role == Role::Owner) {
        let uid = &entry.id;
        if !db.get_user(uid).ok().flatten().is_some_and(|u| u.status == UserStatus::Active) { continue; }
        let Ok(destinations) = db.verified_dm_destinations_for_user(uid) else { continue; };
        bindings.push(json!({"user_id": uid, "targets": destinations.iter()
            .filter(|d| private_destination(&d.channel, &d.channel_user_id))
            .map(|d| json!([d.channel, d.channel_user_id])).collect::<Vec<_>>()}));
    }
    payload["owner_chat_bindings"] = json!(bindings);
}

fn owner_binding<'a>(rec: &'a ApprovalRecord, uid: &str) -> Option<&'a Value> {
    rec.payload["owner_chat_bindings"].as_array()?.iter()
        .find(|b| b["user_id"].as_str() == Some(uid))
}

fn owner_entries(home: &Path, rec: &ApprovalRecord) -> Vec<(String, String, String)> {
    let Some(bindings) = rec.payload["owner_chat_bindings"].as_array() else { return vec![]; };
    let Some(cid) = rec.payload["collection_id"].as_str() else { return vec![]; };
    let Ok(conn) = schema::open_home(home) else { return vec![]; };
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else { return vec![]; };
    let mut out = vec![];
    for binding in bindings {
        let Some(uid) = binding["user_id"].as_str() else { continue; };
        if !super::owner_self_add::is_direct_owner(&conn, cid, uid).unwrap_or(false) { continue; }
        if !db.get_user(uid).ok().flatten().is_some_and(|u| u.status == UserStatus::Active) { continue; }
        let Ok(current) = db.verified_dm_destinations_for_user(uid) else { continue; };
        for d in current.into_iter().filter(|d| private_destination(&d.channel, &d.channel_user_id)) {
            if binding["targets"].as_array().is_some_and(|targets|
                targets.iter().any(|t| t == &json!([d.channel, d.channel_user_id]))) {
                out.push((uid.to_owned(), d.channel, d.channel_user_id));
            }
        }
    }
    out
}

pub(crate) fn owner_targets(home: &Path, rec: &ApprovalRecord) -> Vec<(String, String)> {
    owner_entries(home, rec).into_iter().map(|(_, ch, dest)| (ch, dest)).collect()
}

pub(crate) fn owner_user(home: &Path, rec: &ApprovalRecord, context: &DecisionContext) -> Option<ResolvedUser> {
    context.validate().ok()?;
    let db = crate::decision_notify::open_user_db(home).ok()??;
    let uid = crate::googlechat_identity::verified_user_id(home, &db, &context.channel, &context.principal_id).ok()??;
    owner_binding(rec, &uid)?;
    let user = db.get_user(&uid).ok()??;
    if user.status != UserStatus::Active { return None; }
    let targets: Vec<_> = owner_entries(home, rec).into_iter()
        .filter(|(owner, _, _)| owner == &uid).map(|(_, ch, dest)| (ch, dest)).collect();
    let exact_dm = !matches!(context.channel.as_str(), "slack" | "discord")
        && targets.iter().any(|(ch, dest)| ch == &context.channel && dest == &context.conversation_id);
    let delivered_dm = if matches!(context.channel.as_str(), "slack" | "discord") {
        let conn = schema::open_home(home).ok()?;
        targets.iter().any(|(ch, dest)| ch == &context.channel && dest == &context.principal_id
            && conn.query_row("SELECT EXISTS(SELECT 1 FROM knowledge_owner_card_deliveries
                WHERE approval_id=?1 AND owner_id=?2 AND channel=?3 AND destination=?4 AND conversation=?5)",
                rusqlite::params![rec.id.as_str(), uid, ch, dest, context.conversation_id], |r| r.get::<_, bool>(0)).unwrap_or(false))
    } else { false };
    if !exact_dm && !delivered_dm { return None; }
    if context.channel == "googlechat" {
        if !crate::ccr_runtime::ATTESTED_ONE_TO_ONE.try_with(|a| a.as_ref().is_some_and(|a|
            a.matches("googlechat", &context.conversation_id, &context.principal_id))).unwrap_or(false)
            || !db.matches_googlechat_dm(&uid, &context.account_id, &context.conversation_id, &context.principal_id).ok()? {
            return None;
        }
    }
    let resolved = ResolvedUser { user_id: uid, department: user.department, is_admin: user.role == UserRole::Admin };
    approval::may_decide(home, rec, &resolved).ok()?;
    Some(resolved)
}

pub(crate) async fn decide(home: &Path, rec: &ApprovalRecord, context: &DecisionContext, approve: bool) -> Result<String, String> {
    let user = owner_user(home, rec, context).ok_or(crate::channel_decision_route::DECISION_REFUSED)?;
    let broker = crate::approval::ApprovalBroker::open(home)?;
    approval::decide_owner(home, &broker, &rec.id, approve, &user, context).await?;
    Ok(if approve { "已核准並加入知識庫。" } else { "已拒絕加入知識庫。" }.into())
}

/// Shared content fields and limits for owner and chat-admin cards.
/// Keep missing fields absent; callers supply their own localized labels.
pub(crate) fn card_fields(rec: &ApprovalRecord) -> [(&'static str, usize); 3] {
    [("title", 120),
        if rec.payload["url"].is_string() { ("url", 2048) } else { ("original_name", 200) },
        ("preview", 300)]
}

/// Each untrusted field is a bounded, inert single line; notices never call this.
pub(crate) fn card_body(rec: &ApprovalRecord, channel: &str) -> String {
    let field = |key: &str, max: usize| crate::markdown_render::untrusted_preview(
        channel, rec.payload[key].as_str().unwrap_or(""), max);
    let fields = card_fields(rec);
    format!("加入知識庫的請求\n標題：{}\n檔案／網址：{}\n預覽：{}\n僅限集合目前的擁有者在此私訊決定。\n確認 {}\n取消 {}",
        field(fields[0].0, fields[0].1), field(fields[1].0, fields[1].1),
        field(fields[2].0, fields[2].1), rec.id.as_str(), rec.id.as_str())
}

/// Each collection decider's verified DMs first. Departments expand to active
/// members through the normal ACL evaluator; an admin override is not used.
pub(crate) fn decider_targets(home: &Path, rec: &ApprovalRecord) -> Vec<(String, String, String)> {
    let Some(cid) = rec.payload["collection_id"].as_str() else { return vec![]; };
    let Ok(conn) = schema::open_home(home) else { return vec![]; };
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else { return vec![]; };
    let Ok(users) = db.list_users() else { return vec![]; };
    let mut targets = vec![];
    for u in users {
        if u.status != UserStatus::Active { continue; }
        let role = acl::user_role(&conn, cid, &u.id, u.department.as_deref(), false).ok().flatten();
        if role.is_none_or(|r| r < Role::Editor) { continue; }
        if let Ok(dms) = db.verified_dm_destinations_for_user(&u.id) {
            for d in dms.into_iter().filter(|d| private_destination(&d.channel, &d.channel_user_id)) {
                targets.push((u.id.clone(), d.channel, d.channel_user_id));
            }
        }
    }
    targets.sort_by_key(|(uid, _, _)| owner_binding(rec, uid).is_none());
    targets
}

pub(crate) fn origin_exception(rec: &ApprovalRecord, target: &(String, String), owners: &[(String, String)]) -> bool {
    rec.payload["verified_requester"].as_str().and_then(|uid| owner_binding(rec, uid))
        .and_then(|b| b["targets"].as_array()).is_some_and(|targets|
            targets.iter().any(|t| t == &json!([target.0, target.1]))) && owners.contains(target)
}

/// Only an owner destination receives content, even when a fallback channel
/// happens to be the source group. Other deciders receive metadata-free notices.
pub(crate) fn body_for_target(home: &Path, rec: &ApprovalRecord, target: &(String, String), notice: &str) -> String {
    if owner_targets(home, rec).contains(target) { card_body(rec, &target.0) } else { notice.to_owned() }
}

/// The provider resolves Slack/Discord user IDs into DM conversation IDs. Keep
/// those receipts in the domain store: cosmetic card bookkeeping is not an
/// authority source. Missing receipts fail closed to dashboard decisions.
pub(crate) async fn push_owner_card(
    home: &Path, http: &reqwest::Client, rec: &ApprovalRecord,
    channel: &str, token: &str, destination: &str,
) -> bool {
    if !owner_targets(home, rec).contains(&(channel.to_owned(), destination.to_owned())) { return false; }
    let body = card_body(rec, channel);
    let markup = crate::channel_format::decision_markup(channel,
        crate::decision_action::DecisionSource::Approval, rec.id.as_str());
    match crate::channel_sender::send_untrusted_card(home, http, channel, token, destination, &body, markup).await {
        Ok(receipt) => {
            if let Some(receipt) = receipt {
                let Some((uid, _, _)) = owner_entries(home, rec).into_iter()
                    .find(|(_, ch, dest)| ch == channel && dest == destination) else { return false; };
                let Ok(conn) = schema::open_home(home) else { return false; };
                if conn.execute("INSERT OR IGNORE INTO knowledge_owner_card_deliveries
                    (approval_id, owner_id, channel, destination, conversation) VALUES(?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![rec.id.as_str(), uid, channel, destination, receipt.edit_chat_id]).is_err() { return false; }
            }
            true
        }
        Err(_) => false,
    }
}
