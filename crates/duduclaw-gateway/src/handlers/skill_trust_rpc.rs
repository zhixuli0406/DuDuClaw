//! Admin-only management of publisher + exact public-key auto-approval pins.
use super::*;
use std::collections::BTreeSet;
use std::io::Write;
use duduclaw_core::publishers::{Publisher, TrustStore, TRUST_STORE_FILE, OFFICIAL_PUBLISHER_KEY,
    validate_publisher_id, validate_pubkey};
use duduclaw_security::audit::{append_audit_event, AuditEvent, Severity};
use crate::approval::{ApprovalBroker, ApprovalRecord};
use crate::skill_trust::{conflicting_pin_ids, minisign_key_id, PinnedKey, TrustConfig};
use super::config_commit;

const READ_ERROR: &str = "無法讀取自家技能來源，請稍後再試。";
const WRITE_ERROR: &str = "無法儲存自家技能來源，請重新整理後再試。";
const ADMIN: &str = "只有管理員可以管理自家技能來源。";
const BROWSER: &str = "請使用瀏覽器登入帳號後管理自家技能來源。";
const REAUTH: &str = "請重新輸入目前密碼或新的二次驗證碼。";
const KEY_CONFLICT: &str = "同一發行者的金鑰代號對應到不同公鑰，請先請管理員排除衝突。";
const PIN_INCOMPLETE: &str = "來源已儲存，但尚未啟用，請重新整理後再試。";
const POLICY_PREFIX: &str = "policy:skill_trust:";

fn frame(result: Result<Value, String>) -> WsFrame {
    match result {
        Ok(value) => WsFrame::ok_response("", value),
        Err(message) => WsFrame::error_response("", &message),
    }
}
fn publisher_param(params: &Value) -> Result<&str, String> {
    let publisher = params.get("publisher").and_then(Value::as_str).ok_or("請填寫發行者代號。")?;
    validate_publisher_id(publisher).map_err(|_| "發行者代號須為 1 到 64 個小寫英文字母、數字或連字號，並以字母或數字開頭。")?;
    if publisher == "duduclaw" { return Err("官方發行者不能設為自家技能來源。".into()); }
    Ok(publisher)
}
fn valid_key_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_digit() || (b'A'..=b'F').contains(&b))
}
fn config(home: &Path) -> Result<(toml::Table, String, Vec<PinnedKey>), String> {
    // A missing or broken config is not a valid empty policy.
    let text = config_commit::read_regular_config(&home.join("config.toml")).map_err(|_| READ_ERROR)?;
    let table = text.parse::<toml::Table>().map_err(|_| READ_ERROR)?;
    let trust: TrustConfig = match table.get("skill_trust") {
        Some(value) => value.clone().try_into().map_err(|_| READ_ERROR)?,
        None => TrustConfig::default(),
    };
    // Keep invalid keys and mismatched ids visible for conflict reporting and
    // refusal. Do not discard them before computing poisoned identities.
    if trust.auto_approve.iter().any(|entry| entry.publisher == "duduclaw"
        || validate_publisher_id(&entry.publisher).is_err()
        || entry.added_by.is_empty() || entry.added_at.is_empty()) { return Err(READ_ERROR.into()); }
    Ok((table, config_commit::content_hash(&text), trust.auto_approve))
}
fn set_entries(table: &mut toml::Table, entries: &[PinnedKey]) -> Result<(), String> {
    let section = config_commit::table_at_mut(table, &["skill_trust"]).map_err(|_| WRITE_ERROR)?;
    section.insert("auto_approve".into(), toml::Value::try_from(entries).map_err(|_| WRITE_ERROR)?);
    Ok(())
}
pub(super) fn live_browser_admin(db: &UserDb, ctx: &UserContext, session: &str) -> Result<UserContext, String> {
    let live = db.live_user_context(&ctx.user_id).map_err(|_| REAUTH)?
        .filter(|live| live.is_admin() && live.credential_epoch == ctx.credential_epoch && !live.must_change_password)
        .ok_or(REAUTH)?;
    if !db.remember_session_valid(Some(session), &ctx.user_id, Utc::now().timestamp()).map_err(|_| REAUTH)? {
        return Err(REAUTH.into());
    }
    Ok(live)
}

impl MethodHandler {
    pub(crate) async fn handle_skill_trust_rpc(
        &self, method: &str, params: Value, ctx: &UserContext, conn: crate::power_local::RpcConnInfo,
    ) -> WsFrame {
        if !ctx.is_admin() || ctx.member_key_id.is_some() { return frame(Err(ADMIN.into())); }
        match method {
            "skill_trust.pin" => {
                if ctx.user_id == "system" { return frame(Err(BROWSER.into())); }
                let session = match crate::google_personal::session_for(&ctx.user_id) {
                    Ok(session) => session,
                    Err(_) => return frame(Err(BROWSER.into())),
                };
                let Some(db) = self.user_db.read().await.clone() else { return frame(Err(REAUTH.into())); };
                let home = self.home_dir.clone();
                let ctx = ctx.clone();
                let result = tokio::task::spawn_blocking(move || {
                    let live = live_browser_admin(&db, &ctx, &session)?;
                    if params.get("acknowledge_risk").and_then(Value::as_bool) != Some(true) {
                        return Err("請先閱讀並勾選自動放行技能的風險說明。".into());
                    }
                    let peer = conn.peer.ok_or(BROWSER)?;
                    if !crate::server::check_login_rate_limit(peer.ip(), &live.email) {
                        return Err("嘗試次數過多，請於 15 分鐘後再試。".into());
                    }
                    let password = params.get("password").and_then(Value::as_str).unwrap_or("");
                    let code = params.get("code").and_then(Value::as_str).unwrap_or("");
                    let proof = db.skill_trust_pin_reauthenticate(&ctx.user_id, &session, ctx.credential_epoch,
                        password, code, &crate::mfa::Cipher(&home), Utc::now().timestamp())?;
                    // No proof is accepted from or exposed to the client.
                    db.skill_trust_pin_consume(&ctx.user_id, &session, ctx.credential_epoch, &proof, Utc::now().timestamp())?;
                    pin(&home, &params, &ctx.user_id, || live_browser_admin(&db, &ctx, &session).map(|_| ()))
                }).await;
                frame(result.unwrap_or_else(|_| Err(WRITE_ERROR.into())))
            }
            "skill_trust.get" | "skill_trust.unpin" => {
                // The broker is the sole source of approval history; errors
                // never turn into a plausible empty history.
                let broker = match ApprovalBroker::open(&self.home_dir) {
                    Ok(broker) => broker,
                    Err(_) => return frame(Err(READ_ERROR.into())),
                };
                let records = match broker.list_by_kind(crate::expert_skills::SKILLS_ACTION_KIND).await {
                    Ok(records) => records,
                    Err(_) => return frame(Err(READ_ERROR.into())),
                };
                let home = self.home_dir.clone();
                let ctx = ctx.clone();
                let get = method == "skill_trust.get";
                let result = tokio::task::spawn_blocking(move || {
                    super::system_update_config::require_live_admin_at_commit(&home, &ctx).map_err(|_| ADMIN)?;
                    if get { get_sources(&home, records) }
                    else { unpin(&home, &params, &ctx, &records) }
                }).await;
                frame(result.unwrap_or_else(|_| Err((if get { READ_ERROR } else { WRITE_ERROR }).into())))
            }
            _ => frame(Err("不支援此操作。".into())),
        }
    }
}

struct TrustStoreWrite { changed: bool, durable: bool }

#[cfg(test)]
pub(super) fn trust_store_sync_failures() -> &'static std::sync::Mutex<BTreeSet<std::path::PathBuf>> {
    static FAILURES: std::sync::OnceLock<std::sync::Mutex<BTreeSet<std::path::PathBuf>>> = std::sync::OnceLock::new();
    FAILURES.get_or_init(Default::default)
}

fn sync_trust_store_directory(home: &Path) -> std::io::Result<()> {
    #[cfg(test)]
    if trust_store_sync_failures().lock().unwrap().remove(home) {
        return Err(std::io::Error::other("injected post-persist directory sync failure"));
    }
    #[cfg(unix)]
    std::fs::File::open(home)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = home;
    Ok(())
}

/// Same format and sidecar lock as the operator publisher CLI. Validate the
/// complete resulting store before atomically publishing it.
fn add_trusted_key(home: &Path, publisher: &str, key: &str, guard: impl FnOnce() -> Result<(), String>) -> Result<TrustStoreWrite, String> {
    let path = home.join(TRUST_STORE_FILE);
    duduclaw_core::with_file_lock(&path, || {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err(std::io::Error::other(WRITE_ERROR)),
            Ok(_) => {},
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(e),
        }
        let mut store = TrustStore::load(home).map_err(std::io::Error::other)?;
        let keys = &mut store.publishers.entry(publisher.into()).or_insert_with(|| Publisher { keys: Vec::new() }).keys;
        if keys.iter().any(|existing| existing == key) { return Ok(TrustStoreWrite { changed: false, durable: true }); }
        keys.push(key.into());
        let text = toml::to_string_pretty(&store).map_err(std::io::Error::other)?;
        TrustStore::parse(&text).map_err(std::io::Error::other)?;
        let mut temporary = tempfile::NamedTempFile::new_in(home)?;
        temporary.write_all(text.as_bytes())?;
        temporary.as_file().sync_all()?;
        // Recheck the live session and role under the trust-store lock, at the
        // final publication boundary (the config commit has already finished).
        guard().map_err(std::io::Error::other)?;
        temporary.persist(&path).map_err(|e| e.error)?;
        // Publication makes the source active. A later sync failure affects
        // crash durability only and must not be reported as an inactive pin.
        let durable = match sync_trust_store_directory(home) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(publisher, %error, "skill trust key published but directory sync failed");
                false
            }
        };
        Ok(TrustStoreWrite { changed: true, durable })
    }).map_err(|_| WRITE_ERROR.into())
}

pub(super) fn pin(home: &Path, params: &Value, actor: &str, guard: impl Fn() -> Result<(), String>) -> Result<Value, String> {
    let publisher = publisher_param(params)?;
    let key = params.get("public_key").and_then(Value::as_str).ok_or("請填寫公鑰。")?;
    validate_pubkey(key).map_err(|_| "公鑰格式不正確，請確認後再試。")?;
    let key_id = minisign_key_id(key).map_err(|_| "公鑰格式不正確，請確認後再試。")?;
    let (mut table, hash, mut entries) = config(home)?;
    if conflicting_pin_ids(&entries).contains(&(publisher.to_owned(), key_id.clone()))
        || entries.iter().any(|entry| entry.publisher == publisher && entry.key != key
            && minisign_key_id(&entry.key).is_ok_and(|id| id == key_id)) {
        return Err(KEY_CONFLICT.into());
    }
    let entry = entries.iter().find(|entry| entry.publisher == publisher && entry.key == key).cloned()
        .unwrap_or_else(|| PinnedKey { publisher: publisher.into(), key: key.into(), key_id,
            added_by: actor.into(), added_at: Utc::now().to_rfc3339() });
    if !entries.iter().any(|existing| existing.publisher == publisher && existing.key == key) { entries.push(entry.clone()); }
    set_entries(&mut table, &entries)?;
    guard()?;
    // A pin without its exact key in the trust store is inert. Commit this
    // first so a failed config CAS cannot broaden global publisher trust.
    config_commit::commit_skill_trust_table(home, &hash, &table, &guard).map_err(|_| WRITE_ERROR)?;
    let write = add_trusted_key(home, publisher, key, &guard).map_err(|_| {
        append_audit_event(home, &AuditEvent::new("skill_trust_pin_incomplete", actor, Severity::Warning,
            json!({"publisher": publisher, "key_id": entry.key_id})));
        PIN_INCOMPLETE
    })?;
    append_audit_event(home, &AuditEvent::new("skill_trust_pinned", actor, Severity::Info,
        json!({"publisher": publisher, "key_id": entry.key_id, "trust_store_changed": write.changed,
            "durability_complete": write.durable})));
    serde_json::to_value(entry).map_err(|_| WRITE_ERROR.into())
}

#[derive(serde::Deserialize, serde::Serialize)]
struct SkillEntry { agent: String, name: String, digest: String }
fn skill_entries(record: &ApprovalRecord) -> Result<Vec<SkillEntry>, String> {
    let entries: Vec<SkillEntry> = serde_json::from_value(record.payload.get("entries").cloned().ok_or(READ_ERROR)?)
        .map_err(|_| READ_ERROR)?;
    if entries.iter().any(|entry| !duduclaw_core::is_valid_agent_id(&entry.agent)
        || !duduclaw_agent::skill_loader::is_safe_skill_name(&entry.name)
        || entry.digest.len() != 64 || !entry.digest.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())) {
        return Err(READ_ERROR.into());
    }
    Ok(entries)
}
fn policy_source(record: &ApprovalRecord) -> Result<Option<(&str, &str)>, String> {
    let Some(source) = record.decided_by.as_deref().and_then(|by| by.strip_prefix(POLICY_PREFIX)) else { return Ok(None); };
    let (publisher, key_id) = source.split_once(':').ok_or(READ_ERROR)?;
    if publisher == "duduclaw" || validate_publisher_id(publisher).is_err() || !valid_key_id(key_id) {
        return Err(READ_ERROR.into());
    }
    Ok(Some((publisher, key_id)))
}
fn get_sources(home: &Path, records: Vec<ApprovalRecord>) -> Result<Value, String> {
    let (table, _, entries) = config(home)?;
    let store = TrustStore::load(home).map_err(|_| READ_ERROR)?;
    let mut publishers = Vec::new();
    for (publisher, keys) in std::iter::once(("duduclaw", vec![OFFICIAL_PUBLISHER_KEY]))
        .chain(store.publishers.iter().map(|(id, p)| (id.as_str(), p.keys.iter().map(String::as_str).collect()))) {
        let keys = keys.into_iter().map(|key| Ok(json!({"public_key": key,
            "key_id": minisign_key_id(key).map_err(|_| READ_ERROR)?}))).collect::<Result<Vec<_>, String>>()?;
        publishers.push(json!({"publisher": publisher, "built_in": publisher == "duduclaw", "keys": keys}));
    }
    let conflicts = conflicting_pin_ids(&entries);
    let entries: Vec<Value> = entries.iter().map(|entry| json!({"publisher": entry.publisher,
        "key_id": entry.key_id, "added_by": entry.added_by, "added_at": entry.added_at,
        "conflicting": conflicts.contains(&(entry.publisher.clone(), entry.key_id.clone())),
        "in_trust_store": store.keys(&entry.publisher).iter().any(|key| *key == entry.key.as_str())})).collect();
    let days = config_commit::toml_at(&table, "approval.skill_ttl_days").and_then(toml::Value::as_integer)
        .filter(|days| (1..=30).contains(days)).unwrap_or(crate::skill_ttl::DEFAULT_SKILL_APPROVAL_TTL_SECS / 86_400);
    let mut recent = Vec::new();
    for record in &records {
        if record.action_kind != crate::expert_skills::SKILLS_ACTION_KIND { continue; }
        let Some((publisher, key_id)) = policy_source(record)? else { continue; };
        let decided_at = record.decided_at.as_deref().ok_or(READ_ERROR)?;
        let time = chrono::DateTime::parse_from_rfc3339(decided_at).map_err(|_| READ_ERROR)?;
        let slug = record.payload.get("slug").and_then(Value::as_str).ok_or(READ_ERROR)?;
        recent.push((time, record.id.as_str(), json!({"id": record.id, "slug": slug,
            "decided_at": decided_at, "publisher": publisher, "key_id": key_id, "skills": skill_entries(record)?})));
    }
    recent.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(a.1)));
    Ok(json!({"publishers": publishers, "auto_approve": entries, "skill_ttl_days": days,
        "recent_auto_approvals": recent.into_iter().take(20).map(|(_, _, value)| value).collect::<Vec<_>>()}))
}

fn unpin(home: &Path, params: &Value, ctx: &UserContext, records: &[ApprovalRecord]) -> Result<Value, String> {
    let publisher = publisher_param(params)?;
    let key_id = params.get("key_id").and_then(Value::as_str).filter(|id| valid_key_id(id)).ok_or("金鑰指紋格式不正確。")?;
    let disable = match params.get("disable_skills") {
        None => true,
        Some(Value::Bool(disable)) => *disable,
        _ => return Err("請選擇是否一併停用已放行的技能。".into()),
    };
    let (mut table, hash, mut entries) = config(home)?;
    if conflicting_pin_ids(&entries).contains(&(publisher.to_owned(), key_id.to_owned())) {
        return Err(KEY_CONFLICT.into());
    }
    let before = entries.len();
    entries.retain(|entry| entry.publisher != publisher || entry.key_id != key_id);
    let removed = before - entries.len();
    // Validate approval payloads before changing the policy. Match the whole
    // identity, never a publisher/key prefix (including an appended suffix).
    let decided_by = format!("{POLICY_PREFIX}{publisher}:{key_id}");
    let mut candidates = Vec::new();
    if disable {
        for record in records.iter().filter(|record| record.action_kind == crate::expert_skills::SKILLS_ACTION_KIND
            && record.decided_by.as_deref() == Some(decided_by.as_str())) {
            candidates.extend(skill_entries(record)?);
        }
    }
    set_entries(&mut table, &entries)?;
    config_commit::commit_skill_trust_table(home, &hash, &table,
        || super::system_update_config::require_live_admin_at_commit(home, ctx)).map_err(|_| WRITE_ERROR)?;
    let mut disabled = BTreeSet::new();
    let result = disable_matching(home, candidates, &mut disabled);
    append_audit_event(home, &AuditEvent::new("skill_trust_unpinned", &ctx.user_id, Severity::Info,
        json!({"publisher": publisher, "key_id": key_id, "removed": removed,
            "disabled": disabled.len(), "disable_complete": result.is_ok()})));
    result.map_err(|_| "已移除自動放行設定，但部分技能無法確認或停用，請重新整理後再試。")?;
    Ok(json!({"removed": removed, "disabled": disabled.len()}))
}
fn disable_matching(home: &Path, candidates: Vec<SkillEntry>, disabled: &mut BTreeSet<(String, String)>) -> Result<(), String> {
    for entry in candidates {
        if disabled.contains(&(entry.agent.clone(), entry.name.clone())) { continue; }
        let mut path = home.to_path_buf();
        let mut missing = false;
        // Never follow a substituted employee, SKILLS, or skill directory.
        for part in ["agents", &entry.agent, "SKILLS", &entry.name] {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {},
                Ok(_) => return Err(READ_ERROR.into()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => { missing = true; break; }
                Err(_) => return Err(READ_ERROR.into()),
            }
        }
        if missing { continue; }
        if crate::expert_skills::tree_digest(&path).map_err(|_| READ_ERROR)? != entry.digest { continue; }
        duduclaw_agent::skill_state::set_disabled(home, &entry.agent, std::slice::from_ref(&entry.name), true).map_err(|_| WRITE_ERROR)?;
        disabled.insert((entry.agent, entry.name));
    }
    Ok(())
}
