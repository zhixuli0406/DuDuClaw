//! RM-131 P3: Admin consent for Drive writes. `identity_routes` is deliberately
//! empty until the readiness package supplies its per-identity projection.
use super::*;
use crate::google_workspace::drive_write_policy::{self as policy, ApprovalMode, DriveWriteAction,
    DriveWritePolicy, FolderEntry, FolderKind};
use std::collections::HashSet;

const SAVE_ERROR: &str = "無法儲存設定，設定可能已變更，請重新載入後再試。";
const ACTIONS: [DriveWriteAction; 4] = [DriveWriteAction::Create, DriveWriteAction::Modify,
    DriveWriteAction::Move, DriveWriteAction::Trash];

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalInput { create: String, modify: String, #[serde(rename = "move")] move_: String, trash: String }
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct SetInput {
    create: bool, modify: bool, #[serde(rename = "move")] move_: bool, trash: bool,
    approval: ApprovalInput,
    // Keep individual values until validation so errors name the position.
    folders: Vec<Value>,
    #[serde(default)] acknowledge_risk: bool,
}

fn parse_input(params: Value) -> Result<(DriveWritePolicy, bool), String> {
    let input: SetInput = serde_json::from_value(params)
        .map_err(|_| "設定內容不完整或格式不正確，請檢查後再試。".to_string())?;
    if input.folders.len() > 50 { return Err("最多只能加入 50 個資料夾。".into()); }
    let mode = |v: &str| match v {
        "none" => Ok(ApprovalMode::None), "member" => Ok(ApprovalMode::Member),
        "member_and_admin" => Ok(ApprovalMode::MemberAndAdmin),
        _ => Err("請選擇有效的確認方式。".to_string()),
    };
    let mut new = DriveWritePolicy::all_off();
    new.create = input.create; new.modify = input.modify;
    new.move_ = input.move_; new.trash = input.trash;
    new.approval.create = mode(&input.approval.create)?;
    new.approval.modify = mode(&input.approval.modify)?;
    new.approval.move_ = mode(&input.approval.move_)?;
    new.approval.trash = mode(&input.approval.trash)?;
    let mut seen = HashSet::new();
    for (index, value) in input.folders.iter().enumerate() {
        let id = value.as_str().and_then(policy::extract_folder_id)
            .ok_or_else(|| format!("第 {} 個資料夾連結無法辨識，請重新貼上。", index + 1))?;
        if seen.insert(id.clone()) {
            new.folders.push(FolderEntry { id, name: String::new(), kind: FolderKind::Unverified });
        }
    }
    Ok((new, input.acknowledge_risk))
}

/// One lookup per account, no retries and no redirects. None means unavailable;
/// a readable non-folder is an error and must never fall back to another account.
#[async_trait::async_trait]
pub(super) trait FolderLookup: Send + Sync {
    async fn lookup(&self, id: &str) -> Option<Value>;
}
struct GatewayFolderLookup<'a> { home: &'a Path, db: Option<Arc<UserDb>>, user: &'a str }
#[async_trait::async_trait]
impl FolderLookup for GatewayFolderLookup<'_> {
    async fn lookup(&self, id: &str) -> Option<Value> {
        let url = reqwest::Url::parse(&format!(
            "https://www.googleapis.com/drive/v3/files/{id}?fields=id,name,mimeType,driveId,shortcutDetails&supportsAllDrives=true"
        )).ok()?;
        if let Some(db) = &self.db {
            if let Ok(access) = crate::google_personal::meeting_access::resolve_drive_read(self.home, db, self.user).await {
                if let Ok(response) = access.get(self.home, db, url.clone()).await {
                    if let Some(value) = folder_response_value(response).await { return Some(value); }
                }
            }
        }
        let bearer = zeroize::Zeroizing::new(
            crate::google_workspace::get_valid_google_token_for_tool(self.home, "drive_read").await.ok()?);
        if !crate::google_workspace::native_confirmation::drive_read_path_allowed(&url) { return None; }
        let client = reqwest::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).retry(reqwest::retry::never())
            .timeout(std::time::Duration::from_secs(15)).build().ok()?;
        let response = client.get(url).bearer_auth(bearer.as_str()).send().await.ok()?;
        folder_response_value(response).await
    }
}

/// HTTP error bodies are not file metadata; callers may try the next account.
pub(super) async fn folder_response_value(response: reqwest::Response) -> Option<Value> {
    if !response.status().is_success() { return None; }
    response.json().await.ok()
}

async fn resolve_folders(new: &mut DriveWritePolicy, stored: &DriveWritePolicy, lookup: &dyn FolderLookup) -> Result<(), String> {
    for folder in &mut new.folders {
        if let Some(previous) = stored.folders.iter().find(|entry| entry.id == folder.id) {
            *folder = previous.clone();
            continue;
        }
        if let Some(value) = lookup.lookup(&folder.id).await {
            if value.get("mimeType").and_then(Value::as_str) != Some("application/vnd.google-apps.folder")
                || value.get("shortcutDetails").is_some_and(|v| !v.is_null()) {
                return Err("這個連結不是資料夾".into());
            }
            if value.get("id").and_then(Value::as_str) != Some(folder.id.as_str()) {
                return Err("無法確認這個資料夾，請重新貼上連結。".into());
            }
            let name: String = value.get("name").and_then(Value::as_str).unwrap_or("")
                .chars().filter(|c| !c.is_control()).collect();
            folder.name = duduclaw_core::truncate_chars(&name, 200).to_string();
            folder.kind = if value.get("driveId").and_then(Value::as_str) == Some(folder.id.as_str()) {
                FolderKind::SharedDrive
            } else { FolderKind::Folder };
        }
    }
    Ok(())
}

fn switches(p: &DriveWritePolicy) -> Value {
    json!({"create":p.create,"modify":p.modify,"move":p.move_,"trash":p.trash})
}
fn newly_enabled(old: &DriveWritePolicy, new: &DriveWritePolicy) -> bool {
    ACTIONS.iter().any(|&a| !old.action_enabled(a) && new.action_enabled(a))
}
fn audit_changes(old: &DriveWritePolicy, new: &DriveWritePolicy) -> Vec<String> {
    let mut changes = Vec::new();
    let before = switches(old); let after = switches(new);
    for action in ACTIONS {
        let key = action.as_str();
        changes.push(format!("google.drive_write.{key}: {} -> {}", before[key], after[key]));
        changes.push(format!("google.drive_write.approval.{key}: {} -> {}",
            serde_json::to_value(old.approval_for(action)).unwrap_or(Value::Null),
            serde_json::to_value(new.approval_for(action)).unwrap_or(Value::Null)));
    }
    let added: Vec<_> = new.folders.iter().filter(|f| !old.folders.iter().any(|o| o.id == f.id)).map(|f| &f.id).collect();
    let removed: Vec<_> = old.folders.iter().filter(|f| !new.folders.iter().any(|n| n.id == f.id)).map(|f| &f.id).collect();
    changes.push(format!("google.drive_write.folders added: {added:?}; removed: {removed:?}"));
    changes
}

impl MethodHandler {
    pub(crate) async fn handle_google_drive_write_rpc(&self, method: &str, params: Value, ctx: &UserContext) -> WsFrame {
        if !ctx.is_admin() { return WsFrame::error_response("", "只有管理員可以調整雲端硬碟寫入權限。"); }
        if method == "google.drive_write.get" { return self.drive_write_get(); }
        if matches!(method, "google.drive_write.consent_home_start" | "google.drive_write.consent_home_disconnect"
            | "google.drive_write.consent_home_finish" | "google.drive_write.consent_home_paste") {
            let result: Result<Value, String> = async {
                use crate::google_personal::{self, flow, home_write, pending};
                let session = google_personal::session_for(&ctx.user_id)?;
                let db = self.user_db.read().await.clone().ok_or("人員資料尚未就緒。")?;
                home_write::require_admin(&db, &ctx.user_id)?;
                match method {
                    "google.drive_write.consent_home_start" => {
                        let origin = params.get("redirect_origin").and_then(Value::as_str)
                            .ok_or("請提供目前控制台的網址。")?;
                        home_write::start(&self.home_dir, &db, &ctx.user_id, &session, origin,
                            &crate::server::allowed_origins_snapshot(),
                            params.get("acknowledge_google_data").and_then(Value::as_bool) == Some(true),
                            params.get("acknowledge_write").and_then(Value::as_bool) == Some(true)).await
                    }
                    "google.drive_write.consent_home_disconnect" =>
                        home_write::disconnect(&self.home_dir, &db, &ctx.user_id),
                    _ => {
                        let state = if method == "google.drive_write.consent_home_paste" {
                            pending::receive_pasted(&self.home_dir, &ctx.user_id, &session,
                                params.get("url").and_then(Value::as_str).unwrap_or(""))?
                        } else { params.get("state").and_then(Value::as_str).unwrap_or("").to_string() };
                        flow::finish_home(&self.home_dir, &db, &ctx.user_id, &session, &state).await
                    }
                }
            }.await;
            return match result { Ok(value) => WsFrame::ok_response("", value), Err(e) => WsFrame::error_response("", &e) };
        }
        if method != "google.drive_write.set" { return WsFrame::error_response("", "未知的雲端硬碟寫入方法。"); }
        let lookup = GatewayFolderLookup { home: &self.home_dir, db: self.user_db.read().await.clone(), user: &ctx.user_id };
        self.drive_write_set(params, ctx, &lookup).await
    }

    fn drive_write_get(&self) -> WsFrame {
        let table = std::fs::read_to_string(self.home_dir.join("config.toml")).ok()
            .and_then(|s| s.parse::<toml::Table>().ok()).unwrap_or_default();
        let section = config_commit::toml_at(&table, "google.drive_write");
        let stored = policy::parse_section(section, true);
        let effective = policy::load(&self.home_dir);
        // Display configured booleans even if consent is invalid; never use
        // these unchecked values for authorization or widening comparisons.
        let mut configured = switches(&DriveWritePolicy::all_off());
        for action in ACTIONS {
            let key = action.as_str();
            configured[key] = json!(section.and_then(|s| s.get(key)).and_then(toml::Value::as_bool).unwrap_or(false));
        }
        let mut active = switches(&effective);
        for action in ACTIONS { active[action.as_str()] = json!(effective.action_enabled(action)); }
        let metadata = crate::person_credentials::write_store::all_metadata(&self.home_dir);
        let (connections, unreadable) = match metadata {
            Ok(rows) => (json!({"connected":rows.iter().filter(|r| r.owner_kind == "user"
                && r.status == crate::person_credentials::Status::Connected).count()}), false),
            Err(_) => (Value::Null, true),
        };
        let route_oauth = crate::google_workspace::selected_path(&self.home_dir).ok().as_deref() == Some("oauth");
        let home_write = match crate::person_credentials::write_store::get(&self.home_dir, "home", "home") {
            Ok(Some(record)) => json!({"route_oauth": route_oauth,
                "connected": record.metadata.status == crate::person_credentials::Status::Connected,
                "status": record.metadata.status, "account_email": record.metadata.account_email}),
            Ok(None) => json!({"route_oauth": route_oauth, "connected": false,
                "status": "disconnected", "account_email": null}),
            Err(_) => json!({"route_oauth": route_oauth, "connected": false,
                "status": "unreadable", "account_email": null}),
        };
        WsFrame::ok_response("", json!({
            "home_write":home_write,
            "configured":configured, "effective":active, "approval":stored.approval,
            "folders":stored.folders.iter().map(|f| json!({"id":f.id,"name":f.name,"kind":f.kind})).collect::<Vec<_>>(),
            "ack":stored.ack.as_ref().map(|a| json!({"by_email":a.by_email,"by_role":a.by_role,"at":a.at})),
            "integration_enabled":crate::google_workspace::integration_enabled(&self.home_dir),
            "folder_list_empty_note":stored.folders.is_empty(),
            "write_connections":connections,"write_store_unreadable":unreadable,
            "identity_routes":[]
        }))
    }

    pub(super) async fn drive_write_set(&self, params: Value, ctx: &UserContext, lookup: &dyn FolderLookup) -> WsFrame {
        if !ctx.is_admin() { return WsFrame::error_response("", "只有管理員可以調整雲端硬碟寫入權限。"); }
        let result = async {
            let (mut new, acknowledge) = parse_input(params)?;
            let raw = config_commit::read_text_or_empty(&self.home_dir.join("config.toml")).map_err(|_| SAVE_ERROR)?;
            let table = raw.parse::<toml::Table>().map_err(|_| SAVE_ERROR)?;
            let hash = config_commit::content_hash(&raw);
            let old = policy::load(&self.home_dir);
            // Tie the effective read to the original CAS snapshot, before any network IO.
            let reread = config_commit::read_text_or_empty(&self.home_dir.join("config.toml")).map_err(|_| SAVE_ERROR)?;
            if config_commit::content_hash(&reread) != hash { return Err(SAVE_ERROR.to_string()); }
            if policy::widening(&old, &new) && !acknowledge {
                return Err("請先閱讀並確認風險說明".into());
            }
            let snapshot_enabled = config_commit::toml_at(&table, "integrations.google_workspace")
                .and_then(toml::Value::as_bool).unwrap_or(false);
            if old != policy::parse_section(config_commit::toml_at(&table, "google.drive_write"), snapshot_enabled) {
                return Err(SAVE_ERROR.to_string());
            }
            let stored = policy::parse_section(config_commit::toml_at(&table, "google.drive_write"), true);
            resolve_folders(&mut new, &stored, lookup).await?;
            let changes = audit_changes(&old, &new);
            let db = self.user_db.read().await.clone();
            config_commit::commit_drive_write_policy(&self.home_dir, hash, new.clone(), ctx.clone(), db).await
                .map_err(|_| SAVE_ERROR.to_string())?;
            duduclaw_security::audit::log_config_changed(&self.home_dir, &ctx.email, "admin", &changes);
            crate::security_autopilot::emit_config_changed();
            // Publication feeds MCP's existing visible_tool_snapshot watcher:
            // it emits tools/list_changed for visible-set changes, without a
            // gateway generation counter. Drive tools enter that set in P9b;
            // until then the CLI intentionally hides them all.
            if newly_enabled(&old, &new) {
                match crate::person_credentials::all_metadata(&self.home_dir) {
                    Ok(rows) => {
                        let mut seen = HashSet::new();
                        for row in rows {
                            if row.owner_kind == "user" && row.provider == "google"
                                && row.status == crate::person_credentials::Status::Connected
                                && seen.insert(row.owner_id.clone()) {
                                crate::notify_push::push_person_notice(&self.home_dir, &row.owner_id,
                                    "管理員已允許 AI 寫入雲端硬碟。如果你的 AI 員工使用你本人的 Google 連線，請到「我的連線」連結雲端硬碟寫入。").await;
                            }
                        }
                    }
                    Err(_) => tracing::warn!("Drive write settings saved; personal connection notices unavailable"),
                }
            }
            Ok::<_, String>(())
        }.await;
        match result { Ok(()) => self.drive_write_get(), Err(e) => WsFrame::error_response("", &e) }
    }
}
