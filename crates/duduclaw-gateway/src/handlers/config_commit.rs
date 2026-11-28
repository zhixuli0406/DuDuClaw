//! v1.68.0 — shared plumbing for the dashboard config writers
//! (`system.update_config`, `tick.sources.*`, `config.raw.*`).
//!
//! Three things live here:
//!
//! 1. **Contract parameter lookup.** The v1.68 RPC contract names every new
//!    parameter after its TOML path (`takeover.enabled`). A caller may send it
//!    nested (`{"takeover": {"enabled": true}}`) or flat
//!    (`{"takeover.enabled": true}`); [`param_at`] accepts both, nested first.
//!    The typed getters reject a present-but-wrong-typed value instead of
//!    silently ignoring it (the pre-1.68 writers ignored it, which is how a
//!    save could "succeed" while writing nothing).
//! 2. **Locked commit.** `config.toml` is also written by the CLI
//!    (`duduclaw onboard`, `mcp_internal_key`), so a dashboard save takes the
//!    same `with_file_lock` advisory lock and refuses to write when the file
//!    changed on disk since it was read (optimistic concurrency): the caller
//!    gets "changed on disk, reload and retry" instead of silently undoing the
//!    other writer's change.
//! 3. **Backups** for the raw editor: `<file>.bak-<unix ts>`, last five kept.

#[allow(unused_imports)]
use super::*;

use sha2::{Digest, Sha256};

/// The literal the raw editor and the v1.68 secret fields show in place of a
/// stored secret. Sending it back unchanged means "keep the stored value".
pub(crate) const RAW_SECRET_MASK: &str = "«set»";

/// Placeholders older dashboard code echoes back for an untouched secret.
pub(crate) fn is_secret_placeholder(v: &str) -> bool {
    v == RAW_SECRET_MASK || v == SECRET_MASK_SET || (!v.is_empty() && v.chars().all(|c| c == '*'))
}

// ── Parameter lookup ────────────────────────────────────────────────────────

/// Find the contract parameter at dotted `path`: nested objects first, then a
/// flat key spelled exactly `path`. `None` when absent (or JSON `null`, which
/// the dashboard uses for "not sent").
pub(crate) fn param_at<'a>(params: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = params;
    let mut nested_ok = true;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => {
                nested_ok = false;
                break;
            }
        }
    }
    let found = if nested_ok { Some(cur) } else { params.get(path) };
    found.filter(|v| !v.is_null())
}

pub(crate) fn bool_param(params: &Value, path: &str) -> Result<Option<bool>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{path} must be a boolean")),
    }
}

pub(crate) fn str_param<'a>(params: &'a Value, path: &str) -> Result<Option<&'a str>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("{path} must be a string")),
    }
}

/// Unsigned integer in `min..=max`.
pub(crate) fn u64_param(params: &Value, path: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{path} must be an integer between {min} and {max}")),
        },
    }
}

/// Array of strings (each trimmed). Non-string entries are an error.
pub(crate) fn str_array_param(params: &Value, path: &str) -> Result<Option<Vec<String>>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| format!("{path} entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{path} must be an array of strings")),
    }
}

/// True when the text is non-empty and free of control characters.
pub(crate) fn is_plain_text(s: &str, max_chars: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max_chars && !s.chars().any(char::is_control)
}

// ── TOML table navigation ───────────────────────────────────────────────────

/// Get-or-create the nested table at `path` (e.g. `["container", "sandbox"]`).
pub(crate) fn table_at_mut<'a>(table: &'a mut toml::Table, path: &[&str]) -> Result<&'a mut toml::Table, String> {
    let mut cur = table;
    for (i, seg) in path.iter().enumerate() {
        cur = cur
            .entry(*seg)
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| format!("[{}] in config.toml is not a table", path[..=i].join(".")))?;
    }
    Ok(cur)
}

/// Value at dotted `path`, if every segment exists.
pub(crate) fn toml_at<'a>(table: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut segs = path.split('.');
    let mut cur = table.get(segs.next()?)?;
    for seg in segs {
        cur = cur.as_table()?.get(seg)?;
    }
    Some(cur)
}

/// The value at `path` as JSON, `null` when absent. For audit before/after.
pub(crate) fn toml_at_json(table: &toml::Table, path: &str) -> Value {
    toml_at(table, path)
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(Value::Null)
}

// ── Locked commit ───────────────────────────────────────────────────────────

/// Create `path` fresh with owner-only permissions (0600 on Unix) and write
/// `content`. Config files hold encrypted secrets and the keys of the raw
/// editor's placeholders, so no temp or backup copy is ever world-readable,
/// not even for the moment between create and chmod.
pub(crate) fn write_owner_only(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    verify_live_io()?;
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()
}

/// Hex SHA-256 of a file's bytes; the empty string for an absent file.
pub(crate) fn content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Read `path` as text: `Ok("")` when absent, `Err` on any other IO error.
pub(crate) fn read_text_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

/// Host-owned proof of the read which produced a whole Home config table.
/// No caller may replace this with a fresh hash taken at final publication.
pub(crate) struct HomeTableSnapshot {
    path: PathBuf,
    expected_hash: String,
    existed: bool,
}

pub(crate) fn read_home_table_snapshot(
    path: &Path,
) -> Result<(toml::Table, HomeTableSnapshot), String> {
    if path.file_name().and_then(|n| n.to_str()) != Some("config.toml") {
        return Err("Home table snapshot requires config.toml".into());
    }
    let (raw, existed) = match std::fs::read_to_string(path) {
        Ok(raw) => (raw, true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => return Err(format!("failed to read {}: {e}", path.display())),
    };
    let table = raw
        .parse::<toml::Table>()
        .map_err(|e| format!("Invalid config.toml; refusing edit: {e}"))?;
    let snapshot = HomeTableSnapshot {
        path: path.to_path_buf(),
        expected_hash: content_hash(&raw),
        existed,
    };
    Ok((table, snapshot))
}

pub(crate) fn commit_home_table_snapshot(
    path: &Path,
    snapshot: HomeTableSnapshot,
    table: &toml::Table,
) -> Result<(), String> {
    if snapshot.path.as_path() != path {
        return Err("config.toml snapshot target mismatch".into());
    }
    commit_rendered_locked(path, &snapshot.expected_hash, |current| {
        // An absent read must not bless a concurrently created empty file.
        if path.try_exists().map_err(|e| e.to_string())? != snapshot.existed {
            return Err(changed_on_disk(path));
        }
        let current_table = current
            .parse::<toml::Table>()
            .map_err(|e| format!("Invalid config.toml; refusing edit: {e}"))?;
        // Keep the legacy whole-table helper's accounts restriction, now
        // after the original snapshot CAS and under the common locks.
        if current_table.get("accounts") != table.get("accounts") {
            return Err(
                "config.toml accounts changed while this edit was open; reload and retry".into(),
            );
        }
        render_preserving(current, table)
    })
}

/// Atomically replace `path` with `new_content`, under the cross-process
/// advisory lock, but only when the file still hashes to `expected_hash`
/// (the hash of what the caller read). Synchronous; call it through
/// `spawn_blocking`.
pub(crate) fn commit_text_locked(path: &Path, expected_hash: &str, new_content: &str) -> Result<(), String> {
    commit_rendered_locked(path, expected_hash, |_| Ok(new_content.to_string()))
}

/// Same policy/Google-source/MFA/CAS path, with a durable producer receipt
/// recorded from the prepared file before publication. Unrelated config only.
pub(crate) fn commit_text_observed(
    path:&Path,expected_hash:&str,new_content:&str,
    observer:&mut dyn FnMut(&std::fs::File)->Result<(),String>,
)->Result<(),String> {
    commit_text_observed_guarded(path,expected_hash,new_content,||Ok(()),observer)
}
pub(crate) fn commit_text_observed_guarded(
    path:&Path,expected_hash:&str,new_content:&str,
    guard:impl FnOnce()->Result<(),String>,
    observer:&mut dyn FnMut(&std::fs::File)->Result<(),String>,
)->Result<(),String> {
    let render=|_:&str|{guard()?;Ok(new_content.to_owned())};
    if path.file_name().and_then(|n|n.to_str())==Some("config.toml") {
        let home=path.parent().ok_or("config.toml has no parent")?;
        return crate::google_identity::home_source::with_source_guard(home,|g|Ok(
            commit_rendered_inner(path,expected_hash,render,true,Some(g),None,Some(observer))
        )).map_err(|e|e.to_string())?;
    }
    commit_rendered_inner(path,expected_hash,render,true,None,None,Some(observer))
}

pub(crate) fn commit_text_locked_with_guard(
    path: &Path, expected_hash: &str, new_content: &str,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    commit_rendered_locked(path, expected_hash, |_| {
        guard()?;
        Ok(new_content.to_owned())
    })
}

/// MFA-only adapter; the guard and policy transition run inside the config lock.
pub(crate) async fn commit_mfa_table_locked_with_guard(
    path: &Path,
    expected_hash: String,
    table: &toml::Table,
    actor: String,
    guard: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let path = path.to_path_buf();
    let table = table.clone();
    tokio::task::spawn_blocking(move || {
        commit_mfa_table_inner(&path, &expected_hash, &table, &actor, guard)
    })
    .await
    .map_err(|e| format!("config write task failed: {e}"))?
}

fn commit_mfa_table_inner(
    path: &Path,
    expected_hash: &str,
    table: &toml::Table,
    actor: &str,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    commit_rendered_policy(path, expected_hash, |current| {
        guard()?;
        render_preserving(current, table)
    }, true, Some(actor))
}

#[cfg(test)]
std::thread_local! {
    static MFA_BEFORE_REPLACE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = Default::default();
}

/// Commit `table` with [`commit_text_locked`]. The bytes written are the
/// file as it is on disk (verified against `expected_hash` under the lock)
/// with only the keys that differ from `table` edited in place — see
/// [`render_preserving`] — so the operator's comments, blank lines and key
/// order survive a dashboard save.
pub(crate) async fn commit_table_locked(path: &Path, expected_hash: String, table: &toml::Table) -> Result<(), String> {
    let path = path.to_path_buf();
    let table = table.clone();
    tokio::task::spawn_blocking(move || {
        commit_rendered_locked(&path, &expected_hash, |current| render_preserving(current, &table))
    })
    .await
    .map_err(|e| format!("config write task failed: {e}"))?
}

/// As above, but re-check an authorization condition inside the file lock,
/// immediately before rendering and replacing a credential-bearing config.
pub(crate) async fn commit_table_locked_with_guard(
    path: &Path,
    expected_hash: String,
    table: &toml::Table,
    guard: impl FnOnce() -> Result<(), String> + Send + 'static,
) -> Result<(), String> {
    let path = path.to_path_buf();
    let table = table.clone();
    tokio::task::spawn_blocking(move || {
        commit_rendered_locked(&path, &expected_hash, |current| {
            guard()?;
            render_preserving(current, &table)
        })
    })
    .await
    .map_err(|e| format!("config write task failed: {e}"))?
}

/// [`commit_text_locked`] with the new content computed from the current
/// file text while the lock is held (and after the hash compare), so the
/// render and the write see the same bytes.
pub(crate) fn commit_rendered_locked(
    path: &Path,
    expected_hash: &str,
    render: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    commit_rendered_policy(path,expected_hash,render,true,None)
}
/// Used by backup restore and CLI import, including final publication validation.
pub fn refuse_drive_write_text_edit(before: &str, after: &str) -> Result<(), String> {
    let parse = |text: &str| text.parse::<toml::Table>()
        .map_err(|_| "無法讀取設定，這次沒有變更寫入權限。".to_string());
    super::config_raw_rpc::refuse_drive_write_edit(&parse(before)?, &parse(after)?)
}

/// Shared by generic commits, backup restoration and CLI import.
pub fn refuse_odoo_widening_text_edit(before: &str, after: &str) -> Result<(), String> {
    let parse = |text: &str| text.parse::<toml::Table>()
        .map_err(|_| "無法讀取 Odoo 權限設定，這次沒有變更設定。".to_string());
    super::config_raw_rpc::refuse_odoo_widening_edit(&parse(before)?, &parse(after)?)
}

pub(crate) fn refuse_odoo_agent_widening_text_edit(before: &str, after: &str) -> Result<(), String> {
    let parse = |text: &str| text.parse::<toml::Table>()
        .map_err(|_| "無法讀取 AI 的 Odoo 權限設定，這次沒有變更設定。".to_string());
    super::config_raw_rpc::refuse_odoo_agent_widening_edit(&parse(before)?, &parse(after)?)
}

#[derive(Clone, Copy)]
pub(super) enum OdooWrite { Access, MemberCredentials }

/// Only dedicated, risk-confirming Odoo RPCs call this adapter. The exception
/// cannot change unrelated config, including the other Odoo RPC's section.
pub(super) fn commit_odoo_table(
    home: &Path, expected_hash: &str, table: &toml::Table, scope: OdooWrite,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let path = home.join("config.toml");
    crate::google_identity::home_source::with_source_guard(home, |source| Ok(
        commit_rendered_inner_authorized(&path, expected_hash, |current| {
            let mut old = current.parse::<toml::Table>().map_err(|e| e.to_string())?;
            let mut new = table.clone();
            for candidate in [&mut old, &mut new] {
                match scope {
                    OdooWrite::Access => { candidate.remove("odoo_access"); }
                    OdooWrite::MemberCredentials => {
                        if let Some(section) = candidate.get_mut("odoo") {
                            let section = section.as_table_mut().ok_or("Odoo 設定格式不正確。")?;
                            section.remove("member_credentials");
                            if section.is_empty() { candidate.remove("odoo"); }
                        }
                    }
                }
            }
            if old != new { return Err("只能變更這項 Odoo 權限設定。".into()); }
            guard()?;
            render_preserving(current, table)
        }, true, Some(source), None, None, false, false, true)
    )).map_err(|e| e.to_string())?
}

/// The only consent-producing adapter. Its input is a validated policy, never
/// an arbitrary caller-supplied config table; all other writers use the guard.
pub(super) async fn commit_drive_write_policy(
    home: &Path, expected_hash: String,
    policy: crate::google_workspace::drive_write_policy::DriveWritePolicy,
    actor: UserContext, db: Option<Arc<UserDb>>,
) -> Result<(), String> {
    acl::require_role(&actor, UserRole::Admin)?;
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let section = crate::google_workspace::drive_write_policy::render_section(
            &policy, &crate::google_workspace::drive_write_policy::sign(
                &policy, &actor.user_id, &actor.email, chrono::Utc::now()));
        // Verify the exact section we are about to publish, including actor fields.
        if crate::google_workspace::drive_write_policy::parse_section(Some(&section), true).ack.is_none() {
            return Err("無法確認管理員身分，這次沒有變更寫入權限。".into());
        }
        crate::google_identity::home_source::with_source_guard(&home, |g| Ok(
            commit_rendered_inner_authorized(&home.join("config.toml"), &expected_hash, |current| {
                // Folder lookups may await the network. Recheck the authenticated
                // person immediately before publication, under the config lock.
                if let Some(db) = &db {
                    let live = db.live_user_context(&actor.user_id)
                        .map_err(|_| "無法確認管理員身分。".to_string())?
                        .ok_or_else(|| "無法確認管理員身分。".to_string())?;
                    if !live.is_admin() || live.credential_epoch != actor.credential_epoch
                        || live.email != actor.email || live.must_change_password {
                        return Err("管理員身分已變更，請重新登入。".into());
                    }
                }
                let mut table = current.parse::<toml::Table>().map_err(|_| "無法讀取設定。".to_string())?;
                table_at_mut(&mut table, &["google"])?.insert("drive_write".into(), section);
                render_preserving(current, &table)
            }, true, Some(g), None, None, true, false, false)
        )).map_err(|e| e.to_string())?
    }).await.map_err(|_| "無法儲存設定，請稍後重試。".to_string())?
}

/// Existing local CLI policy remains at its caller. This narrow final-write
/// adapter changes locking/durability, not operator role/product policy.
pub(crate) fn commit_local_config_rendered(
    home:&Path,expected_hash:&str,render:impl FnOnce(&str)->Result<String,String>,
)->Result<(),String> {
    commit_rendered_policy(&home.join("config.toml"),expected_hash,render,false,None)
}
fn commit_rendered_policy(path:&Path,expected_hash:&str,render:impl FnOnce(&str)->Result<String,String>,dashboard_policy:bool,mfa_actor:Option<&str>)->Result<(),String> {
    if path.file_name().and_then(|n|n.to_str())==Some("config.toml") {
        let home=path.parent().ok_or("config.toml has no parent")?;
        return crate::google_identity::home_source::with_source_guard(home,|g|Ok(commit_rendered_inner(path,expected_hash,render,dashboard_policy,Some(g),mfa_actor,None)))
            .map_err(|e|e.to_string())?;
    }
    commit_rendered_inner(path,expected_hash,render,dashboard_policy,None,mfa_actor,None)
}
/// Dedicated writer; callers must perform Admin verification before entering.
/// All normal CAS, policy, source, authority and atomic-write fences still apply.
pub(super) fn commit_skill_trust_table(
    home: &Path, expected_hash: &str, table: &toml::Table,
    guard: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    let path = home.join("config.toml");
    crate::google_identity::home_source::with_source_guard(home, |source| Ok(
        commit_rendered_inner_authorized(&path, expected_hash, |current| {
            guard()?;
            // This exception may change exactly one section.
            let mut old = current.parse::<toml::Table>().map_err(|e| e.to_string())?;
            let mut new = table.clone();
            old.remove("skill_trust");
            new.remove("skill_trust");
            if old != new { return Err("只能變更自家技能來源。".into()); }
            render_preserving(current, table)
        }, true, Some(source), None, None, false, true, false)
    )).map_err(|e| e.to_string())?
}

fn commit_rendered_inner(path:&Path,expected_hash:&str,render:impl FnOnce(&str)->Result<String,String>,dashboard_policy:bool,source:Option<&crate::google_identity::home_source::SourceGuard>,mfa_actor:Option<&str>,observer:Option<&mut dyn FnMut(&std::fs::File)->Result<(),String>>)->Result<(),String> {
    commit_rendered_inner_authorized(path, expected_hash, render, dashboard_policy, source, mfa_actor, observer, false, false, false)
}
fn commit_rendered_inner_authorized(path:&Path,expected_hash:&str,render:impl FnOnce(&str)->Result<String,String>,dashboard_policy:bool,source:Option<&crate::google_identity::home_source::SourceGuard>,mfa_actor:Option<&str>,observer:Option<&mut dyn FnMut(&std::fs::File)->Result<(),String>>,drive_write_consent:bool,skill_trust_write:bool,odoo_write:bool)->Result<(),String> {
    let authority_home = match path.file_name().and_then(|n| n.to_str()) {
        Some("config.toml") => path.parent(),
        Some("agent.toml") => path.parent().and_then(Path::parent).and_then(Path::parent),
        _ => None,
    };
    let _authority = authority_home.map(|home|
        duduclaw_core::authority_coordinator::AuthorityCoordinator::new(home).write()
    ).transpose().map_err(|e| e.to_string())?;
    let outcome = duduclaw_core::with_file_lock(path, || {
        verify_live_io()?;
        let current = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Ok(Err(format!("failed to re-read {}: {e}", path.display()))),
        };
        if content_hash(&current) != expected_hash {
            return Ok(Err(changed_on_disk(path)));
        }
        let new_content = match render(&current) {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        if !drive_write_consent && path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            if let Err(error) = refuse_drive_write_text_edit(&current, &new_content) {
                return Ok(Err(error));
            }
        }
        if !odoo_write && path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            if let Err(error) = refuse_odoo_widening_text_edit(&current, &new_content) {
                return Ok(Err(error));
            }
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("agent.toml") {
            if let Err(error) = refuse_odoo_agent_widening_text_edit(&current, &new_content) {
                return Ok(Err(error));
            }
        }
        // Only the dedicated Admin RPC may change the auto-approval policy.
        // Check final TOML values under the common lock, including generic
        // whole-table, raw, observed and local-config adapters.
        if !skill_trust_write && path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            let unchanged = current.parse::<toml::Table>().and_then(|old|
                new_content.parse::<toml::Table>().map(|new| old.get("skill_trust") == new.get("skill_trust")));
            if !matches!(unchanged, Ok(true)) {
                return Ok(Err("請到自家技能來源設定頁變更自動放行清單。".into()));
            }
        }
        if observer.is_some() && source.is_some() {
            if let Err(error)=crate::google_identity::home_source::require_unrelated_config(&current,&new_content) {
                return Ok(Err(error.to_string()));
            }
        }
        if dashboard_policy && matches!(path.file_name().and_then(|n| n.to_str()), Some("config.toml" | "agent.toml" | "inference.toml")) {
            let home = if path.file_name().and_then(|n| n.to_str()) == Some("agent.toml") {
                path.parent().and_then(Path::parent).and_then(Path::parent)
            } else { path.parent() };
            if let Some(home) = home {
                if let Err(e) = crate::product_policy::validate_write(home, path, &current, &new_content) {
                    return Ok(Err(e));
                }
            }
        }
        if mfa_actor.is_none() && path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            if let Err(error) = crate::mfa::refuse_policy_change_at(path.parent().unwrap_or(Path::new(".")), &current, &new_content) {
                return Ok(Err(error));
            }
        }
        if path.file_name().and_then(|n| n.to_str()) == Some("config.toml") {
            let Some(home) = path.parent() else { return Ok(Err("config.toml has no parent".into())); };
            let table = match new_content.parse::<toml::Table>() {
                Ok(table) => table,
                Err(_) => return Ok(Err("invalid config.toml; connection ownership cannot be verified".into())),
            };
            if let Err(e) = crate::disconnect_forget::connections::mark_config_connecting(home, &table, None) {
                return Ok(Err(e));
            }
        }
        let write = || {
            fence_isolation_policy_path(path).map_err(|e| e.to_string())?;
            #[cfg(test)]
            if mfa_actor.is_some() {
                MFA_BEFORE_REPLACE.with(|hook| {
                    if let Some(hook) = hook.borrow_mut().take() { hook(); }
                });
            }
            if let Some(g) = source {
                if let Some(observer)=observer {
                    crate::google_identity::home_source::commit_unrelated_config_observed(g,&current,&new_content,observer).map_err(|e|e.to_string())
                } else {
                    crate::google_identity::home_source::commit_google_config_under_guard(g, &current, &new_content).map_err(|e|e.to_string())
                }
            } else {
                if let Some(observer)=observer {
                    use std::io::Write;
                    let parent=path.parent().ok_or("config path missing parent")?;
                    let mut temporary=tempfile::NamedTempFile::new_in(parent).map_err(|e|e.to_string())?;
                    temporary.write_all(new_content.as_bytes()).and_then(|_|temporary.as_file().sync_all()).map_err(|e|e.to_string())?;
                    observer(temporary.as_file())?;
                    verify_live_io().map_err(|e| e.to_string())?;
                    temporary.persist(path).map_err(|e|e.to_string())?;
                    std::fs::File::open(parent).and_then(|f|f.sync_all()).map_err(|e|e.to_string())
                } else {replace_owner_only(path, &new_content).map_err(|e| e.to_string())}
            }
        };
        if let Some(actor) = mfa_actor {
            let Some(home) = path.parent().filter(|_| path.file_name().and_then(|n| n.to_str()) == Some("config.toml")) else {
                return Ok(Err("MFA policy requires config.toml".into()));
            };
            Ok(crate::mfa::commit_policy(home, &new_content, actor, write))
        } else {
            Ok(write())
        }
    });
    match outcome {
        Ok(inner) => {
            if mfa_actor.is_none() && inner.is_ok() && path.file_name().and_then(|n|n.to_str())==Some("config.toml") {
                if let Some(home)=path.parent() { crate::mfa::reload_policy(home); }
            }
            inner
        },
        Err(e) => Err(format!("failed to write {}: {e}", path.display())),
    }
}

/// Read only a regular config, refusing links even when the target is missing.
/// O_NOFOLLOW closes the metadata-to-open symlink window on Unix.
pub(crate) fn read_regular_config(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("config.toml must be a regular file"));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options.open(path)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    Ok(text)
}

/// Early boot adapter: source lock then config lock, before dashboard services
/// exist. The caller may only provision gateway metadata and restore MFA state.
/// Ordinary commit adapters reject the absent-auth -> enforced repair.
pub(crate) fn commit_boot_config<T>(
    home: &Path,
    render: impl FnOnce(&str, bool) -> Result<(T, Option<String>), String>,
    after_write: impl FnOnce(&T) -> Result<(), String>,
) -> Result<T, String> {
    duduclaw_core::config::create_home_dir(home).map_err(|e| e.to_string())?;
    crate::google_identity::home_source::with_source_guard(home, |_| {
        Ok(duduclaw_core::with_file_lock(&home.join("config.toml"), || {
            let path = home.join("config.toml");
            let check = || -> std::io::Result<()> {
                match std::fs::symlink_metadata(&path) {
                    Ok(m) if !m.is_file() || m.file_type().is_symlink() =>
                        Err(std::io::Error::other("config.toml must be a regular file")),
                    Ok(_) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e),
                }
            };
            check()?;
            let (current, missing) = match read_regular_config(&path) {
                Ok(text) => (text, false),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), true),
                Err(e) => return Err(e),
            };
            let (result, rendered) = render(&current, missing).map_err(std::io::Error::other)?;
            if let Some(rendered) = rendered {
                refuse_drive_write_text_edit(&current, &rendered).map_err(std::io::Error::other)?;
                refuse_odoo_widening_text_edit(&current, &rendered).map_err(std::io::Error::other)?;
                use std::io::Write;
                let tmp = home.join(format!(".config-{}.tmp", uuid::Uuid::new_v4()));
                let mut opts = std::fs::OpenOptions::new();
                opts.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    opts.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                }
                let mut file = opts.open(&tmp)?;
                let write = (|| {
                    file.write_all(rendered.as_bytes())?;
                    file.sync_all()?;
                    check()?;
                    std::fs::rename(&tmp, &path)?;
                    #[cfg(unix)]
                    std::fs::File::open(home)?.sync_all()?;
                    Ok::<_, std::io::Error>(())
                })();
                if write.is_err() { let _ = std::fs::remove_file(&tmp); }
                write?;
            }
            // The fresh policy seed shares the Admin toggle's config lock.
            // Config lands first, but no other writer may disable it between
            // that write and the durable identity-store initialization.
            after_write(&result).map_err(std::io::Error::other)?;
            Ok(result)
        }).map_err(|e| format!("boot config commit failed: {e}")))
    }).map_err(|e| e.to_string())?
}

fn changed_on_disk(path: &Path) -> String {
    format!(
        "{} changed on disk while this edit was open (another writer saved first) — reload and try again",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("config file")
    )
}

/// Write `content` to `<file name>.tmp` next to `path` (owner-only) and
/// rename it over `path`. The caller holds the lock. The temp name keeps the
/// whole file name (`SOUL.md.tmp`, `agent.toml.tmp`); it used to swap the
/// extension for `toml.tmp`, which named a Markdown file's temp copy
/// `SOUL.toml.tmp`.
fn verify_live_io() -> std::io::Result<()> {
    duduclaw_mcp_boundary::io_authority::verify_before_io()
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::PermissionDenied, error.to_string()))
}

pub(crate) fn replace_owner_only(path: &Path, content: &str) -> std::io::Result<()> {
    fence_isolation_policy_path(path)?;
    let tmp = tmp_path_for(path);
    write_owner_only(&tmp, content)?;
    if let Err(e) = verify_live_io().and_then(|_| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

/// Increment the existing users.db authority epoch before publishing policy.
/// Even a failed write may revoke a run; a later A -> B -> A edit must never
/// restore an earlier capability merely because its content hash matches.
fn fence_isolation_policy_path(path: &Path) -> std::io::Result<()> {
    let name = path.file_name().and_then(|name| name.to_str());
    let home = match name {
        Some("config.toml" | "org.toml" | "KILLSWITCH.toml" | "preset_bindings.toml") => path.parent(),
        Some("agent.toml" | "CONTRACT.toml") => path.parent().and_then(Path::parent)
            .filter(|dir| dir.file_name().and_then(|name| name.to_str()) == Some("agents"))
            .and_then(Path::parent),
        _ => None,
    };
    if let Some(home) = home { duduclaw_core::isolation_policy::fence_change(home)?; }
    Ok(())
}

/// `<dir>/<file name>.tmp` for `path`.
pub(crate) fn tmp_path_for(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".tmp");
    path.with_file_name(name)
}

// ── Format-preserving render ────────────────────────────────────────────────

pub const ODOO_RESTORE_NOTICE: &str =
    "Odoo 的權限模式與同仁 Odoo 帳號開關未還原，請到 Odoo 設定頁重新確認。";

/// Restore/import must not carry consent or remove the installation's refusals.
/// Interactive editors still use the refusing guard, never this adapter.
pub fn strip_odoo_widening_preserving(before: &str, after: &str) -> Result<(String, bool), String> {
    let old_table = parse_odoo_restore_table(before)?;
    let original = parse_odoo_restore_table(after)?;
    let mut narrowed = original.clone();
    for table in [&old_table, &original] {
        for section in ["odoo_access", "odoo"] {
            if table.get(section).is_some_and(|value| !value.is_table()) {
                return Err("Odoo 設定格式不正確，沒有匯入或還原。".into());
            }
        }
    }
    fn refusals(table: &toml::Table) -> Result<Vec<String>, String> {
        match toml_at(table, "odoo_access.always_refuse") {
            None => Ok(duduclaw_odoo::access_policy::DEFAULT_ALWAYS_REFUSE.iter()
                .map(|entry| (*entry).to_owned()).collect()),
            Some(toml::Value::Array(values)) => values.iter().map(|value| value.as_str()
                .map(str::to_owned).ok_or_else(|| "Odoo 拒絕清單格式不正確，沒有匯入或還原。".into())).collect(),
            Some(_) => Err("Odoo 拒絕清單格式不正確，沒有匯入或還原。".into()),
        }
    }
    let old = refusals(&old_table)?;
    let mut union = refusals(&original)?;
    let mut added_refusals = false;
    for entry in old {
        if !union.contains(&entry) {
            union.push(entry);
            added_refusals = true;
        }
    }
    if let Some(access) = narrowed.get_mut("odoo_access").and_then(toml::Value::as_table_mut) {
        if access.get("permission_mode").and_then(toml::Value::as_str) == Some("follow_odoo_account") {
            access.insert("permission_mode".into(), toml::Value::String("engine".into()));
        }
        access.remove("permission_mode_ack");
    }
    // An implicit parent containing only the removed acknowledgement has no
    // remaining TOML spelling. Keep the expected table aligned with the edit.
    if narrowed.get("odoo_access").and_then(toml::Value::as_table).is_some_and(|table| table.is_empty()) {
        let doc: toml_edit::DocumentMut = after.parse().map_err(|e| format!("invalid Odoo restore TOML: {e}"))?;
        if doc.get("odoo_access").and_then(toml_edit::Item::as_table).is_some_and(|table| table.is_implicit()) {
            narrowed.remove("odoo_access");
        }
    }
    if added_refusals {
        table_at_mut(&mut narrowed, &["odoo_access"])?.insert("always_refuse".into(),
            toml::Value::Array(union.into_iter().map(toml::Value::String).collect()));
    }
    if toml_at(&original, "odoo.member_credentials").and_then(toml::Value::as_bool) == Some(true)
        && toml_at(&old_table, "odoo.member_credentials").and_then(toml::Value::as_bool) != Some(true)
    {
        table_at_mut(&mut narrowed, &["odoo"])?.insert("member_credentials".into(), toml::Value::Boolean(false));
    }
    let result = render_odoo_restore_narrowing(after, &original, &narrowed)?;
    refuse_odoo_widening_text_edit(before, &result.0)?;
    Ok(result)
}

/// Restore/import bypass the interactive agent writer. Keep its guard after
/// narrowing, including when removing an override would fall back to inherit.
pub fn strip_odoo_agent_widening_preserving(before: &str, after: &str) -> Result<(String, bool), String> {
    use duduclaw_odoo::access_policy::PermissionOverride;
    let old = parse_odoo_restore_table(before)?;
    let original = parse_odoo_restore_table(after)?;
    for table in [&old, &original] {
        if table.get("odoo").is_some_and(|value| !value.is_table())
            || toml_at(table, "odoo.permission_override").is_some_and(|value| !value.is_str())
        {
            return Err("AI 的 Odoo 設定格式不正確，沒有匯入或還原。".into());
        }
    }
    let mut narrowed = original.clone();
    if toml_at(&original, "odoo.act_as_requester").and_then(toml::Value::as_bool) == Some(true) {
        table_at_mut(&mut narrowed, &["odoo"])?.insert("act_as_requester".into(), toml::Value::Boolean(false));
    }
    let previous = PermissionOverride::from_config(toml_at(&old, "odoo.permission_override").and_then(toml::Value::as_str));
    let override_ = toml_at(&original, "odoo.permission_override").and_then(toml::Value::as_str);
    let safe_override = match override_ {
        Some("engine" | "read_only") => None,
        // Unknown overrides already mean read_only; preserve that restriction.
        Some(value) if value != "inherit" => Some("read_only"),
        _ if previous == PermissionOverride::ReadOnly => Some("read_only"),
        Some(_) => Some("engine"),
        None if previous == PermissionOverride::Engine => Some("engine"),
        None => None,
    };
    if let Some(value) = safe_override {
        table_at_mut(&mut narrowed, &["odoo"])?.insert("permission_override".into(), toml::Value::String(value.into()));
    }
    let result = render_odoo_restore_narrowing(after, &original, &narrowed)?;
    refuse_odoo_agent_widening_text_edit(before, &result.0)?;
    Ok(result)
}

fn parse_odoo_restore_table(text: &str) -> Result<toml::Table, String> {
    text.parse().map_err(|_| "無法讀取 Odoo 權限設定，沒有匯入或還原。".into())
}

/// Unlike the generic renderer, never fall back to rewriting unrelated text.
fn render_odoo_restore_narrowing(text: &str, old: &toml::Table, new: &toml::Table) -> Result<(String, bool), String> {
    if old == new { return Ok((text.to_owned(), false)); }
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("invalid Odoo restore TOML: {e}"))?;
    sync_table(doc.as_table_mut(), old, new, false);
    let mut rendered = doc.to_string();
    let header = leading_comment_header(text);
    if !header.is_empty() && !rendered.starts_with(header) { rendered.insert_str(0, header); }
    if text.ends_with('\n') && !rendered.ends_with('\n') { rendered.push('\n'); }
    if &parse_odoo_restore_table(&rendered)? != new {
        return Err("Odoo 權限收窄時改動了其他設定，沒有匯入或還原。".into());
    }
    Ok((rendered, true))
}

/// Remove portable Drive write consent without re-serializing unrelated settings.
/// Empty implicit parents disappear with the section; explicit `[google]` stays.
pub fn strip_drive_write_preserving(text: &str) -> Result<(String, bool), String> {
    let mut doc: toml_edit::DocumentMut = text.parse()
        .map_err(|e| format!("config file is not valid TOML: {e}"))?;
    let Some(google) = doc.get_mut("google") else {
        return Ok((text.to_owned(), false));
    };
    let implicit = google.as_table().is_some_and(|table| table.is_implicit());
    let Some(table) = google.as_table_like_mut() else {
        return Err("設定中的 google 區段格式不正確，沒有匯入或還原。".into());
    };
    if table.remove("drive_write").is_none() {
        return Ok((text.to_owned(), false));
    }
    let remove_google = implicit && table.is_empty();
    if remove_google {
        doc.as_table_mut().remove("google");
    }
    let mut rendered = doc.to_string();
    // Opening comments may belong to the removed first item.
    let header = leading_comment_header(text);
    if !header.is_empty() && !rendered.starts_with(header) {
        rendered.insert_str(0, header);
    }
    if text.ends_with('\n') && !rendered.ends_with('\n') {
        rendered.push('\n');
    }
    // Refuse a divergent edit rather than falling back to a lossy serializer.
    let mut expected: toml::Table = text.parse()
        .map_err(|e| format!("config file is not valid TOML: {e}"))?;
    expected.get_mut("google").and_then(toml::Value::as_table_mut)
        .ok_or("google is not a table")?.remove("drive_write");
    if remove_google {
        expected.remove("google");
    }
    let actual: toml::Table = rendered.parse()
        .map_err(|e| format!("stripped config is not valid TOML: {e}"))?;
    if actual != expected {
        return Err("Drive write consent removal changed unrelated settings".into());
    }
    Ok((rendered, true))
}

/// Render `new_table` as an edit of `original_text`: every key whose value is
/// unchanged keeps its exact bytes (comments above it, trailing comment,
/// blank lines, position, inline-table / array formatting); a changed scalar
/// keeps its line's decoration (the trailing comment survives); a removed key
/// goes with its own line; a new key is appended at the end of its section;
/// a new section is created after the last table of its parent (for a
/// top-level section, the end of the file).
///
/// `Err` when `original_text` is not valid TOML: callers must never replace
/// a file they could not parse. An absent / blank original is rendered with
/// `toml::to_string_pretty`, as before. If the edited document would not
/// read back as exactly `new_table` (a bug here), the full re-serialisation
/// is used instead, which is always correct, only not format-preserving.
pub fn render_preserving(original_text: &str, new_table: &toml::Table) -> Result<String, String> {
    let pretty = || toml::to_string_pretty(new_table).map_err(|e| format!("failed to serialize config: {e}"));
    if original_text.trim().is_empty() {
        return pretty();
    }
    let unparsable = |e: &dyn std::fmt::Display| {
        format!("config file is not valid TOML ({e}) — refusing to overwrite it (fix it in the raw editor or a terminal first)")
    };
    let old: toml::Table = original_text.parse().map_err(|e: toml::de::Error| unparsable(&e))?;
    let mut doc: toml_edit::DocumentMut =
        original_text.parse().map_err(|e: toml_edit::TomlError| unparsable(&e))?;
    sync_table(doc.as_table_mut(), &old, new_table, false);
    let mut rendered = doc.to_string();
    // toml_edit attaches a document's opening comments to its first item.
    // Removing that item (for example, the first [[accounts]] entry) would
    // otherwise discard the document header along with it.
    let header = leading_comment_header(original_text);
    if !header.is_empty() && !rendered.starts_with(header) {
        rendered.insert_str(0, header);
    }
    match rendered.parse::<toml::Table>() {
        Ok(back) if &back == new_table => Ok(rendered),
        _ => {
            tracing::warn!("format-preserving config render diverged; writing a full re-serialisation instead");
            pretty()
        }
    }
}

fn leading_comment_header(text: &str) -> &str {
    let mut end = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.trim().is_empty() || trimmed.starts_with('#') {
            end += line.len();
        } else {
            break;
        }
    }
    &text[..end]
}

/// Bring `dst` (whose current content is `old`) to `new`, touching only the
/// keys that differ. `inline` is true inside an inline table, where every
/// child must be a plain value.
fn sync_table(dst: &mut dyn toml_edit::TableLike, old: &toml::Table, new: &toml::Table, inline: bool) {
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    for key in removed {
        dst.remove(key);
    }
    for (key, nv) in new {
        match (old.get(key), dst.get_mut(key)) {
            (Some(ov), Some(_)) if ov == nv => {}
            (Some(ov), Some(item)) => sync_item(item, ov, nv, inline),
            _ if inline => {
                let value = append_inline_value(dst, to_value(nv));
                dst.insert(key, toml_edit::Item::Value(value));
            }
            _ => {
                dst.insert(key, fresh_item(nv, inline));
            }
        }
    }
}

/// An appended inline-table entry takes over the closing whitespace of what
/// was the last entry, so `{ a = 1 }` grows to `{ a = 1, b = 2 }` and not
/// `{ a = 1 , b = 2 }`.
fn append_inline_value(dst: &mut dyn toml_edit::TableLike, mut value: toml_edit::Value) -> toml_edit::Value {
    let closing = dst.iter_mut().last().and_then(|(_, item)| {
        let last = item.as_value_mut()?;
        let suffix = last.decor().suffix().cloned();
        last.decor_mut().set_suffix("");
        suffix
    });
    value.decor_mut().set_prefix(" ");
    if let Some(suffix) = closing {
        value.decor_mut().set_suffix(suffix);
    }
    value
}

fn sync_item(item: &mut toml_edit::Item, old: &toml::Value, new: &toml::Value, inline: bool) {
    use toml_edit::{Item, Value as EV};
    match (item, old, new) {
        (Item::Table(t), toml::Value::Table(o), toml::Value::Table(n)) => sync_table(t, o, n, false),
        (Item::Value(EV::InlineTable(t)), toml::Value::Table(o), toml::Value::Table(n)) => sync_table(t, o, n, true),
        (Item::ArrayOfTables(a), toml::Value::Array(o), toml::Value::Array(n)) if all_tables(n) && all_tables(o) => {
            sync_array_of_tables(a, o, n)
        }
        (Item::Value(v), _, _) if inline || !needs_block(new) => {
            let mut replacement = to_value(new);
            *replacement.decor_mut() = v.decor().clone();
            *v = replacement;
        }
        (item, _, _) => *item = fresh_item(new, inline),
    }
}

/// Same length: edit element by element. Otherwise keep every old element
/// that survives unchanged (in order, so a removal keeps its neighbours'
/// bytes) and create the rest fresh.
fn sync_array_of_tables(dst: &mut toml_edit::ArrayOfTables, old: &[toml::Value], new: &[toml::Value]) {
    if old.len() == new.len() && dst.len() == old.len() {
        for (i, (o, n)) in old.iter().zip(new).enumerate() {
            if let (Some(t), Some(ot), Some(nt)) = (dst.get_mut(i), o.as_table(), n.as_table())
                && ot != nt
            {
                sync_table(t, ot, nt, false);
            }
        }
        return;
    }
    let kept: Vec<toml_edit::Table> = dst.iter().cloned().collect();
    let mut rebuilt = toml_edit::ArrayOfTables::new();
    let mut next = 0;
    for n in new {
        let found = (next..old.len().min(kept.len())).find(|&k| &old[k] == n);
        match (found, n.as_table()) {
            (Some(k), _) => {
                rebuilt.push(kept[k].clone());
                next = k + 1;
            }
            (None, Some(nt)) => rebuilt.push(fresh_table(nt)),
            (None, None) => {}
        }
    }
    *dst = rebuilt;
}

fn all_tables(items: &[toml::Value]) -> bool {
    !items.is_empty() && items.iter().all(toml::Value::is_table)
}

/// Whether `v` must be written as a `[section]` / `[[array]]` block rather
/// than a plain value (what `toml::to_string_pretty` would do).
fn needs_block(v: &toml::Value) -> bool {
    match v {
        toml::Value::Table(_) => true,
        toml::Value::Array(a) => all_tables(a),
        _ => false,
    }
}

fn fresh_item(v: &toml::Value, inline: bool) -> toml_edit::Item {
    match v {
        toml::Value::Table(t) if !inline => toml_edit::Item::Table(fresh_table(t)),
        toml::Value::Array(a) if !inline && all_tables(a) => {
            let mut aot = toml_edit::ArrayOfTables::new();
            for t in a.iter().filter_map(toml::Value::as_table) {
                aot.push(fresh_table(t));
            }
            toml_edit::Item::ArrayOfTables(aot)
        }
        _ => toml_edit::Item::Value(to_value(v)),
    }
}

/// A new `[section]`: implicit, so a section holding only sub-sections gets
/// no empty header of its own (as `toml::to_string_pretty` writes it).
fn fresh_table(t: &toml::Table) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    out.set_implicit(true);
    for (k, v) in t {
        out.insert(k, fresh_item(v, false));
    }
    out
}

fn to_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(d) => match d.to_string().parse::<toml_edit::Datetime>() {
            Ok(dt) => dt.into(),
            // Unreachable for a datetime toml itself parsed; the read-back
            // check in `render_preserving` catches it either way.
            Err(_) => d.to_string().into(),
        },
        toml::Value::Array(a) => toml_edit::Value::Array(a.iter().map(to_value).collect()),
        toml::Value::Table(t) => {
            let mut out = toml_edit::InlineTable::new();
            for (k, v) in t {
                out.insert(k, to_value(v));
            }
            toml_edit::Value::InlineTable(out)
        }
    }
}

// ── Backups ─────────────────────────────────────────────────────────────────

/// How many `<file>.bak-<ts>` copies the raw editor keeps per file.
pub(crate) const RAW_BACKUPS_KEPT: usize = 5;

/// Copy `content` to `<path>.bak-<unix ts>` and delete all but the newest
/// [`RAW_BACKUPS_KEPT`] backups of that file. Returns the backup path.
/// A same-second collision gets a `-<n>` suffix rather than overwriting.
pub(crate) fn write_backup(path: &Path, content: &str, now_unix: i64) -> Result<PathBuf, String> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("config path has no file name".into());
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut backup = dir.join(format!("{name}.bak-{now_unix}"));
    let mut n = 1;
    while backup.exists() {
        backup = dir.join(format!("{name}.bak-{now_unix}-{n}"));
        n += 1;
    }
    // Backups hold the same secrets as the file itself.
    write_owner_only(&backup, content).map_err(|e| format!("failed to write backup: {e}"))?;
    prune_backups(dir, name, RAW_BACKUPS_KEPT);
    Ok(backup)
}

/// Delete all but the newest `keep` backups of `name` in `dir`. Order is by
/// the numeric timestamp in the name (then the collision suffix), so a
/// touched mtime can never make an old backup look new.
pub(crate) fn prune_backups(dir: &Path, name: &str, keep: usize) {
    let prefix = format!("{name}.bak-");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut found: Vec<((i64, u32), PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name().to_str()?.to_string();
            let rest = file.strip_prefix(&prefix)?;
            let mut parts = rest.splitn(2, '-');
            let ts = parts.next()?.parse::<i64>().ok()?;
            let seq = match parts.next() {
                Some(s) => s.parse::<u32>().ok()?,
                None => 0,
            };
            Some(((ts, seq), e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in found.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn isolation_policy_replacement_fences_all_authority_targets_before_publish() {
        let home = tempfile::tempdir().unwrap();
        let users = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let epoch = || users.isolation_authority_snapshot(None, "alice").unwrap().policy_epoch;
        for relative in ["config.toml", "org.toml", "KILLSWITCH.toml", "preset_bindings.toml",
            "agents/alice/agent.toml", "agents/alice/CONTRACT.toml"] {
            let path = home.path().join(relative);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let before = epoch();
            super::replace_owner_only(&path, "value='a'\n").unwrap();
            super::replace_owner_only(&path, "value='b'\n").unwrap();
            super::replace_owner_only(&path, "value='a'\n").unwrap();
            assert_eq!(epoch(), before + 3, "{relative}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "value='a'\n");
        }
        let before = epoch();
        super::replace_owner_only(&home.path().join("agents/alice/SOUL.md"), "text").unwrap();
        assert_eq!(epoch(), before, "non-policy text must not use this fence");
        let db = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        db.execute("DELETE FROM auth_meta WHERE key='isolation_policy_epoch'", []).unwrap();
        let path = home.path().join("config.toml");
        assert!(super::replace_owner_only(&path, "value='new'\n").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "value='a'\n");
    }
    use super::*;

    // Added regression tests; not run in this change.
    #[tokio::test]
    async fn connecting_config_clears_disconnect_evidence_before_publication() {
        let home = tempfile::tempdir().unwrap();
        let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.path().join("memory.db")).unwrap();
        engine.record_connector_disconnect("db:live").await.unwrap();
        let path = home.path().join("config.toml");
        let next = "[db_sources.live]\ndriver='sqlite'\nurl='/tmp/live.sqlite'\nallowed_tables=['*']\n";
        commit_text_locked(&path, &content_hash(""), next).unwrap();
        assert!(engine.disconnected_connector_refs().await.unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(path).unwrap(), next);
    }

    #[test]
    fn mfa_missing_singleton_refuses_config_commit_without_repair() {
        for before in ["", "[auth]\nmfa_enforced=false\n", "[auth]\nmfa_enforced=true\n"] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.toml");
            std::fs::write(&path, before).unwrap();
            drop(duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap());
            let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            conn.execute("DELETE FROM mfa_policy_state", []).unwrap();
            let after = format!("{before}\n[general]\nlog_level='debug'\n");
            assert!(commit_text_locked(&path, &content_hash(before), &after).is_err());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
            assert_eq!(conn.query_row("SELECT COUNT(*) FROM mfa_policy_state", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
        }
    }

    #[test]
    fn connecting_config_refuses_publication_when_memory_fence_is_unreadable() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join("memory.db"), "not a database").unwrap();
        let path = home.path().join("config.toml");
        std::fs::write(&path, "").unwrap();
        let next = "[db_sources.live]\ndriver='sqlite'\nurl='/tmp/live.sqlite'\nallowed_tables=['*']\n";
        assert!(commit_text_locked(&path, &content_hash(""), next).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "");
    }

    #[test]
    fn temp_copy_keeps_the_whole_file_name() {
        assert_eq!(tmp_path_for(Path::new("/h/agents/a/SOUL.md")), Path::new("/h/agents/a/SOUL.md.tmp"));
        assert_eq!(tmp_path_for(Path::new("/h/agents/a/agent.toml")), Path::new("/h/agents/a/agent.toml.tmp"));
        let dir = tempfile::tempdir().unwrap();
        let md = dir.path().join("SOUL.md");
        std::fs::write(&md, "old").unwrap();
        replace_owner_only(&md, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&md).unwrap(), "new");
        assert!(!dir.path().join("SOUL.toml.tmp").exists() && !dir.path().join("SOUL.md.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&md).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn param_at_accepts_nested_and_flat() {
        let nested = json!({ "takeover": { "enabled": true } });
        let flat = json!({ "takeover.enabled": false });
        assert_eq!(bool_param(&nested, "takeover.enabled").unwrap(), Some(true));
        assert_eq!(bool_param(&flat, "takeover.enabled").unwrap(), Some(false));
        assert_eq!(bool_param(&json!({}), "takeover.enabled").unwrap(), None);
        assert_eq!(bool_param(&json!({"takeover": {"enabled": null}}), "takeover.enabled").unwrap(), None);
    }

    #[test]
    fn typed_getters_reject_wrong_types() {
        assert!(bool_param(&json!({"a": {"b": "true"}}), "a.b").is_err());
        assert!(u64_param(&json!({"a": -1}), "a", 0, 5).is_err());
        assert!(u64_param(&json!({"a": 9}), "a", 0, 5).is_err());
        assert_eq!(u64_param(&json!({"a": 5}), "a", 0, 5).unwrap(), Some(5));
        assert!(str_array_param(&json!({"a": [1]}), "a").is_err());
    }

    #[test]
    fn commit_refuses_when_file_changed_underneath() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "a = 1\n").unwrap();
        let stale = content_hash("a = 0\n");
        let err = commit_text_locked(&path, &stale, "a = 2\n").unwrap_err();
        assert!(err.contains("changed on disk"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
        let fresh = content_hash("a = 1\n");
        commit_text_locked(&path, &fresh, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
    }

    #[test]
    fn commit_creates_an_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        commit_text_locked(&path, &content_hash(""), "x = true\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = true\n");
    }

    #[cfg(unix)]
    #[test]
    fn commits_and_backups_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        commit_text_locked(&path, &content_hash(""), "x = 1\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let b = write_backup(&path, "x = 1\n", 1).unwrap();
        assert_eq!(std::fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o600);
    }

    const FIXTURE: &str = r#"# DuDuClaw config, hand edited

# Gateway section comment
[gateway]
port = 18889 # trailing on port
bind = "127.0.0.1"
# auth_token = "commented-out"

[general]
default_agent = "dudu"   # who answers
log_level = "info"

# the mailbox
[mail]
# the switch
enabled = false # off for now
# who reads mail
default_agent = "dudu"
# drop folder
dropfolder_enabled = true

[[accounts]]
id = "a"
api_key_enc = "ZW5jcnlwdGVk/base64=="

[[accounts]]
id = "b"
priority = 2

[tick]
enabled = false

[[tick.sources]]
id = "s1"
kind = "http_poll"
url = "https://example.com"
headers = { "X-Key" = "v" }

[channels.line]
channel_token_enc = "abc=="

[[channels.line.accounts]]
name = "oa1"

# trailing comment at end
"#;

    fn edited(f: impl FnOnce(&mut toml::Table)) -> String {
        let mut t: toml::Table = FIXTURE.parse().unwrap();
        f(&mut t);
        let out = render_preserving(FIXTURE, &t).unwrap();
        assert_eq!(out.parse::<toml::Table>().unwrap(), t, "{out}");
        out
    }

    fn mail(t: &mut toml::Table) -> &mut toml::Table {
        t.get_mut("mail").unwrap().as_table_mut().unwrap()
    }

    #[test]
    fn unchanged_table_renders_byte_identical() {
        assert_eq!(edited(|_| {}), FIXTURE);
    }

    #[test]
    fn changing_one_key_changes_only_its_value() {
        let out = edited(|t| {
            mail(t).insert("enabled".into(), toml::Value::Boolean(true));
        });
        assert_eq!(out, FIXTURE.replace("enabled = false # off for now", "enabled = true # off for now"));
    }

    #[test]
    fn new_key_lands_at_the_end_of_its_section() {
        let out = edited(|t| {
            mail(t).insert("auto_trigger".into(), toml::Value::Boolean(false));
        });
        assert_eq!(
            out,
            FIXTURE.replace("dropfolder_enabled = true\n", "dropfolder_enabled = true\nauto_trigger = false\n")
        );
    }

    #[test]
    fn new_section_goes_to_the_end_without_losing_comments() {
        let out = edited(|t| {
            let mut night = toml::Table::new();
            night.insert("llm_enabled".into(), toml::Value::Boolean(true));
            t.insert("night".into(), toml::Value::Table(night));
        });
        let body = FIXTURE.trim_end_matches("\n# trailing comment at end\n");
        assert!(out.starts_with(body), "{out}");
        assert!(out.contains("[night]\nllm_enabled = true\n"), "{out}");
        assert!(out.ends_with("# trailing comment at end\n"), "{out}");
    }

    #[test]
    fn removing_a_key_keeps_the_neighbours_comments() {
        let out = edited(|t| {
            mail(t).remove("default_agent");
        });
        assert_eq!(out, FIXTURE.replace("# who reads mail\ndefault_agent = \"dudu\"\n", ""));
    }

    #[test]
    fn array_of_tables_append_and_remove_keep_other_elements() {
        let appended = edited(|t| {
            let accounts = t.get_mut("accounts").unwrap().as_array_mut().unwrap();
            let mut c = toml::Table::new();
            c.insert("id".into(), toml::Value::String("c".into()));
            accounts.push(toml::Value::Table(c));
        });
        assert_eq!(
            appended,
            FIXTURE.replace("priority = 2\n", "priority = 2\n\n[[accounts]]\nid = \"c\"\n")
        );
        let removed = edited(|t| {
            t.get_mut("accounts").unwrap().as_array_mut().unwrap().remove(0);
        });
        assert_eq!(
            removed,
            FIXTURE.replace("[[accounts]]\nid = \"a\"\napi_key_enc = \"ZW5jcnlwdGVk/base64==\"\n\n", "")
        );
        // Editing inside one element of an array of tables.
        let tweaked = edited(|t| {
            let src = t["tick"]["sources"][0].clone();
            let mut src = src.as_table().unwrap().clone();
            src.insert("url".into(), toml::Value::String("https://example.org".into()));
            t.get_mut("tick").unwrap().as_table_mut().unwrap().insert(
                "sources".into(),
                toml::Value::Array(vec![toml::Value::Table(src)]),
            );
        });
        assert_eq!(tweaked, FIXTURE.replace("https://example.com", "https://example.org"));
    }

    #[test]
    fn strip_odoo_restore_narrows_consent_and_preserves_unrelated_format() {
        let before = "[odoo_access]\nalways_refuse = ['unlink:*', 'write:res.partner']\n";
        let after = "# backup header\n[general]\nlog_level = 'debug' # untouched\n\n[odoo]\nmember_credentials = true # keep comment\nurl = 'https://odoo.example'\n\n[odoo_access]\npermission_mode = 'follow_odoo_account'\nalways_refuse = ['unlink:*', 'create:res.users'] # refusals\n[odoo_access.permission_mode_ack]\nby = 'admin'\nat = '2026-11-28T00:00:00Z'\n";
        let (out, removed) = strip_odoo_widening_preserving(before, after).unwrap();
        assert!(removed);
        assert!(out.starts_with("# backup header\n"));
        assert!(out.contains("log_level = 'debug' # untouched"));
        assert!(out.contains("url = 'https://odoo.example'"));
        assert!(out.contains("# keep comment"));
        assert!(out.contains("# refusals"));
        assert!(out.ends_with('\n'));
        let table: toml::Table = out.parse().unwrap();
        assert_eq!(toml_at(&table, "odoo_access.permission_mode").and_then(toml::Value::as_str), Some("engine"));
        assert!(toml_at(&table, "odoo_access.permission_mode_ack").is_none());
        assert_eq!(toml_at(&table, "odoo.member_credentials").and_then(toml::Value::as_bool), Some(false));
        let access = duduclaw_odoo::access_policy::OdooAccessConfig::parse_config_toml(&out).unwrap();
        assert_eq!(access.always_refuse, vec!["unlink:*", "create:res.users", "write:res.partner"]);
        refuse_odoo_widening_text_edit(before, &out).unwrap();
        // Even unchanged consent belongs to the source installation.
        let (out, removed) = strip_odoo_widening_preserving(after, after).unwrap();
        assert!(removed);
        let table: toml::Table = out.parse().unwrap();
        assert!(toml_at(&table, "odoo_access.permission_mode_ack").is_none());
    }

    #[test]
    fn strip_odoo_restore_unions_effective_missing_lists_and_handles_toml_spellings() {
        for after in [
            "odoo_access = { permission_mode = 'follow_odoo_account', permission_mode_ack = { by = 'a' }, always_refuse = [] }\nodoo = { member_credentials = true, url = 'keep' }\n",
            "odoo_access.permission_mode = 'follow_odoo_account'\nodoo_access.permission_mode_ack.by = 'a'\nodoo_access.always_refuse = []\nodoo.member_credentials = true\n",
            "[odoo_access]\nalways_refuse = []\n",
        ] {
            let (out, removed) = strip_odoo_widening_preserving("", after).unwrap();
            assert!(removed);
            let access = duduclaw_odoo::access_policy::OdooAccessConfig::parse_config_toml(&out).unwrap();
            assert_eq!(access.always_refuse, duduclaw_odoo::access_policy::DEFAULT_ALWAYS_REFUSE);
            assert!(access.permission_mode_ack.is_none());
            refuse_odoo_widening_text_edit("", &out).unwrap();
        }
        let before = "[odoo_access]\nalways_refuse = ['write:res.partner']\n";
        let (out, removed) = strip_odoo_widening_preserving(before, "# keep\n[general]\nlog_level = 'debug'\n").unwrap();
        assert!(removed);
        let access = duduclaw_odoo::access_policy::OdooAccessConfig::parse_config_toml(&out).unwrap();
        assert!(access.always_refuse.contains(&"write:res.partner".to_owned()));
        for entry in duduclaw_odoo::access_policy::DEFAULT_ALWAYS_REFUSE {
            assert!(access.always_refuse.iter().any(|value| value == entry));
        }
        let (out, removed) = strip_odoo_widening_preserving("", "# keep\n[odoo_access.permission_mode_ack]\nby = 'a'\n").unwrap();
        assert!(removed);
        assert!(out.starts_with("# keep\n"));
        assert!(toml_at(&out.parse().unwrap(), "odoo_access.permission_mode_ack").is_none());
    }

    #[test]
    fn strip_odoo_restore_clean_text_is_unchanged_and_invalid_policy_fails() {
        for text in ["", "# keep\n[odoo_access]\npermission_mode = 'engine'\n[odoo]\nmember_credentials = false # keep\n",
            "[odoo]\nmember_credentials = true\n"] {
            assert_eq!(strip_odoo_widening_preserving(text, text).unwrap(), (text.to_owned(), false));
        }
        for invalid in ["[broken", "odoo = []", "odoo_access = true", "[odoo_access]\nalways_refuse = [1]",
            "[odoo_access]\nalways_refuse = 'bad'"] {
            assert!(strip_odoo_widening_preserving("", invalid).is_err());
            assert!(strip_odoo_widening_preserving(invalid, "").is_err());
        }
    }

    #[test]
    fn strip_odoo_agent_restore_preserves_restrictions_and_unrelated_text() {
        for (before, expected) in [("", "engine"),
            ("[odoo]\npermission_override = 'engine'\n", "engine"),
            ("[odoo]\npermission_override = 'read_only'\n", "read_only")] {
            let backup = "# agent\n[runtime]\nprovider = 'claude' # keep\n[odoo]\nact_as_requester = true # requester\npermission_override = 'inherit'\n";
            let (out, removed) = strip_odoo_agent_widening_preserving(before, backup).unwrap();
            assert!(removed);
            assert!(out.contains("provider = 'claude' # keep"));
            assert!(out.contains("# requester"));
            let table: toml::Table = out.parse().unwrap();
            assert_eq!(toml_at(&table, "odoo.act_as_requester").and_then(toml::Value::as_bool), Some(false));
            assert_eq!(toml_at(&table, "odoo.permission_override").and_then(toml::Value::as_str), Some(expected));
            refuse_odoo_agent_widening_text_edit(before, &out).unwrap();
        }
        let before = "[odoo]\npermission_override = 'read_only'\n";
        let (out, removed) = strip_odoo_agent_widening_preserving(before, "# no override\n").unwrap();
        assert!(removed);
        assert_eq!(toml_at(&out.parse().unwrap(), "odoo.permission_override").and_then(toml::Value::as_str), Some("read_only"));
        for override_ in ["engine", "read_only"] {
            let clean = format!("# clean\n[odoo]\nact_as_requester = false\npermission_override = '{override_}' # keep\n");
            assert_eq!(strip_odoo_agent_widening_preserving("", &clean).unwrap(), (clean, false));
        }
    }

    #[test]
    fn strip_drive_write_preserves_header_comments_order_and_trailing_newline() {
        let kept = "# preserve this header\n[integrations]\ngoogle_workspace = true # keep this comment\n\n";
        let original = format!("{kept}[google.drive_write]\ncreate = true\n[google.drive_write.approval]\ncreate = 'none'\n[google.drive_write.ack]\nsignature = 'signed'\n[[google.drive_write.folders]]\nid = 'folder'\n");
        let (out, removed) = strip_drive_write_preserving(&original).unwrap();
        assert!(removed);
        assert_eq!(out.trim_end(), kept.trim_end());
        assert!(out.ends_with('\n'));
        let table: toml::Table = out.parse().unwrap();
        assert!(!table.contains_key("google"));
    }

    #[test]
    fn strip_drive_write_keeps_other_google_keys() {
        let kept = "# header\n[google]\nclient_id = 'keep' # untouched\nclient_secret = 'also keep'\n";
        let original = format!("{kept}[google.drive_write]\ncreate = true\n");
        let (out, removed) = strip_drive_write_preserving(&original).unwrap();
        assert!(removed);
        assert_eq!(out, kept);
    }

    #[test]
    fn strip_drive_write_removes_only_implicit_empty_google() {
        for section in ["[google.drive_write]\ncreate = true\n",
            "[google.drive_write.approval]\ncreate = 'none'\n"] {
            let (out, removed) = strip_drive_write_preserving(section).unwrap();
            assert!(removed);
            assert!(!out.parse::<toml::Table>().unwrap().contains_key("google"));
            let explicit = format!("# header\n[google]\n{section}");
            let (out, removed) = strip_drive_write_preserving(&explicit).unwrap();
            assert!(removed);
            assert_eq!(out, "# header\n[google]\n");
            assert!(out.parse::<toml::Table>().unwrap()["google"].as_table().unwrap().is_empty());
        }
    }

    #[test]
    fn strip_drive_write_handles_dotted_keys() {
        for original in [
            "# header\ngoogle.drive_write.create = true\n",
            "# header\ngoogle.drive_write.approval.create = 'none'\n",
        ] {
            let (out, removed) = strip_drive_write_preserving(original).unwrap();
            assert!(removed);
            assert_eq!(out, "# header\n");
            assert!(out.parse::<toml::Table>().unwrap().is_empty());
        }
        let original = "# header\ngoogle.client_id = 'keep' # untouched\ngoogle.drive_write.create = true\n";
        let (out, removed) = strip_drive_write_preserving(original).unwrap();
        assert!(removed);
        assert_eq!(out, "# header\ngoogle.client_id = 'keep' # untouched\n");
    }

    #[test]
    fn strip_drive_write_handles_inline_google() {
        let original = "# header\ngoogle = { client_id = 'keep', drive_write = { create = true } }\n";
        let (out, removed) = strip_drive_write_preserving(original).unwrap();
        assert!(removed);
        assert!(out.starts_with("# header\n"));
        assert!(out.ends_with('\n'));
        let table: toml::Table = out.parse().unwrap();
        assert_eq!(table["google"]["client_id"].as_str(), Some("keep"));
        assert!(table["google"].get("drive_write").is_none());
    }

    #[test]
    fn strip_drive_write_without_section_is_byte_identical() {
        for original in ["", "# header\n", "# header\n[google]\nclient_id = 'keep' # comment\n\n",
            "# header\n[integrations]\ngoogle_workspace = true"] {
            assert_eq!(strip_drive_write_preserving(original).unwrap(), (original.to_owned(), false));
        }
        assert!(strip_drive_write_preserving("[broken").is_err());
    }

    #[test]
    fn strip_drive_write_refuses_non_table_google() {
        for original in [
            "[[google]]\ndrive_write = { create = true }\n",
            "google = [{ drive_write = { create = true } }]\n",
            "google = 'unrelated'\n",
            "google = []\n",
        ] {
            let error = strip_drive_write_preserving(original).unwrap_err();
            assert!(error.contains("google 區段格式不正確"), "{original}: {error}");
        }
    }

    #[test]
    fn drive_write_text_edit_refuses_changed_non_table_google() {
        for after in [
            "[[google]]\ndrive_write = { create = true }\n",
            "google = [{ drive_write = { create = true } }]\n",
            "google = 'invalid'\n",
            "google = []\n",
        ] {
            for before in ["", "[google]\nclient_id = 'keep'\n", "google = false\n"] {
                assert!(refuse_drive_write_text_edit(before, after).is_err(), "{before} -> {after}");
            }
            assert!(refuse_drive_write_text_edit(after, after).is_ok());
            assert!(refuse_drive_write_text_edit(after, "").is_ok());
        }
    }

    #[test]
    fn drive_write_text_edit_keeps_table_rules() {
        let consent = "[google.drive_write]\ncreate = true\n";
        assert!(refuse_drive_write_text_edit("", consent).is_err());
        assert!(refuse_drive_write_text_edit(consent, consent).is_ok());
        assert!(refuse_drive_write_text_edit(consent, "[google]\nclient_id = 'keep'\n").is_ok());
        assert!(refuse_drive_write_text_edit("", "google = { client_id = 'keep' }\n").is_ok());
        assert!(refuse_drive_write_text_edit("", "").is_ok());
    }

    #[test]
    fn removing_first_array_table_keeps_document_header_once() {
        let original = "# Config header\n\n[[accounts]]\nid = 'first'\n\n[[accounts]]\nid = 'second'\n";
        let mut table: toml::Table = original.parse().unwrap();
        table.get_mut("accounts").unwrap().as_array_mut().unwrap().remove(0);
        let out = render_preserving(original, &table).unwrap();
        assert!(out.starts_with("# Config header\n\n"), "{out}");
        assert_eq!(out.matches("# Config header").count(), 1);
        assert!(!out.contains("id = 'first'"));
        assert_eq!(out.parse::<toml::Table>().unwrap(), table);
    }

    #[test]
    fn inline_table_edit_keeps_its_inline_form() {
        let out = edited(|t| {
            let src = t.get_mut("tick").unwrap().get_mut("sources").unwrap().as_array_mut().unwrap()[0]
                .as_table_mut()
                .unwrap();
            src.get_mut("headers").unwrap().as_table_mut().unwrap().insert("X-Other".into(), toml::Value::String("w".into()));
        });
        assert!(out.contains(r#"headers = { "X-Key" = "v", X-Other = "w" }"#), "{out}");
    }

    #[test]
    fn unparsable_original_is_refused_and_blank_is_pretty() {
        let t: toml::Table = "a = 1".parse().unwrap();
        assert!(render_preserving("[broken", &t).unwrap_err().contains("not valid TOML"));
        assert_eq!(render_preserving("", &t).unwrap(), "a = 1\n");
    }

    #[test]
    fn commit_table_locked_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, FIXTURE).unwrap();
        let mut t: toml::Table = FIXTURE.parse().unwrap();
        mail(&mut t).insert("enabled".into(), toml::Value::Boolean(true));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(commit_table_locked(&path, content_hash(FIXTURE), &t)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            FIXTURE.replace("enabled = false # off for now", "enabled = true # off for now")
        );
    }

    #[test]
    fn backups_keep_the_newest_five() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for ts in 100..108 {
            write_backup(&path, &format!("v = {ts}\n"), ts).unwrap();
        }
        // Same-second collision gets a suffix and counts as newer.
        write_backup(&path, "v = dup\n", 107).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "config.toml.bak-104",
                "config.toml.bak-105",
                "config.toml.bak-106",
                "config.toml.bak-107",
                "config.toml.bak-107-1",
            ]
        );
    }
}

#[cfg(test)]
mod mfa_guard_tests {
    use super::*;
    fn policy_value(home: &Path) -> bool {
        rusqlite::Connection::open(home.join("users.db")).unwrap()
            .query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get(0)).unwrap()
    }

    #[test]
    fn mfa_boot_seed_holds_config_lock_after_durable_config_write() {
        let home = tempfile::tempdir().unwrap();
        let strict = "[auth]\nmfa_enforced=true\n";
        commit_boot_config(home.path(), |_, _| Ok(((), Some(strict.into()))), |_| {
            assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), strict);
            let lock = std::fs::OpenOptions::new().read(true).write(true)
                .open(home.path().join("config.toml.lock")).unwrap();
            assert_eq!(fs2::FileExt::try_lock_exclusive(&lock).unwrap_err().raw_os_error(), fs2::lock_contended_error().raw_os_error());
            let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db"))?;
            let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123)?;
            Ok(())
        }).unwrap();
        assert!(crate::mfa::boot_policy(home.path(), false).unwrap());
    }

    #[test]
    fn mfa_boot_seed_failure_leaves_strict_config_for_retry() {
        let home = tempfile::tempdir().unwrap();
        let strict = "[auth]\nmfa_enforced=true\n";
        assert!(commit_boot_config(home.path(), |_, _| Ok(((), Some(strict.into()))),
            |_| Err("identity initialization failed".into())).is_err());
        assert_eq!(std::fs::read_to_string(home.path().join("config.toml")).unwrap(), strict);
        assert!(!home.path().join("users.db").exists());
        crate::mcp_internal_key::ensure_internal_mcp_key_for_existing_config(home.path(), false).unwrap();
        assert!(crate::mfa::boot_policy(home.path(), false).unwrap());
    }

    #[test]
    fn mfa_ordinary_config_writers_refuse_changes_under_lock() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original = "[auth]\nmfa_enforced=true\n";
        std::fs::write(&path, original).unwrap();
        for next in ["", "[auth]\nmfa_enforced=false\n"] {
            let error = commit_text_locked(&path, &content_hash(original), next).unwrap_err();
            assert!(error.contains("system.update_config"));
            let error = commit_local_config_rendered(home.path(), &content_hash(original), |_| Ok(next.into())).unwrap_err();
            assert!(error.contains("system.update_config"));
            assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        }
        commit_text_locked(&path, &content_hash(original),
            "[auth]\nmfa_enforced=true\n[general]\nlog_level='debug'\n").unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("log_level"));
    }

    #[test]
    fn admin_can_explicitly_disable_after_auth_table_is_lost() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original = "[gateway]\nport=18789\n";
        std::fs::write(&path, original).unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let mut conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
        duduclaw_auth::mfa::record_enforcement(&mut conn, true, 123).unwrap();
        crate::mfa::reload_policy(home.path());
        let next: toml::Table = "[gateway]\nport=18789\n[auth]\nmfa_enforced=false\n".parse().unwrap();
        assert!(commit_text_locked(&path, &content_hash(original), &toml::to_string(&next).unwrap()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
        commit_mfa_table_inner(&path, &content_hash(original), &next, "test-admin", || Ok(())).unwrap();
        crate::mfa::reload_policy(home.path());
        assert!(!policy_value(home.path()));
        assert!(!crate::mfa::enforced(home.path(), false).unwrap());
    }

    #[test]
    fn mfa_enable_is_strict_at_real_config_replacement() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original = "[auth]\nmfa_enforced=false\n";
        std::fs::write(&path, original).unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        crate::mfa::reload_policy(home.path());
        let observed_home = home.path().to_path_buf();
        let observed = std::rc::Rc::new(std::cell::Cell::new(false));
        let flag = observed.clone();
        MFA_BEFORE_REPLACE.with(|hook| *hook.borrow_mut() = Some(Box::new(move || {
            assert!(policy_value(&observed_home));
            assert_eq!(std::fs::read_to_string(observed_home.join("config.toml")).unwrap(), original);
            // A separate credential transaction already observes the strict state.
            let conn = rusqlite::Connection::open(observed_home.join("users.db")).unwrap();
            conn.execute_batch("BEGIN IMMEDIATE").unwrap();
            assert!(conn.query_row("SELECT enforced FROM mfa_policy_state WHERE id=1", [], |r| r.get::<_, bool>(0)).unwrap());
            flag.set(true);
        })));
        let next = "[auth]\nmfa_enforced=true\n".parse().unwrap();
        commit_mfa_table_inner(&path, &content_hash(original), &next, "test-admin", || Ok(())).unwrap();
        assert!(observed.get());
        assert!(std::fs::read_to_string(&path).unwrap().contains("mfa_enforced=true"));
        assert!(crate::mfa::enforced(home.path(), false).unwrap());
    }

    #[test]
    fn mfa_commit_db_failure_preserves_strict_order_and_reload_reconciles() {
        for initial in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let path = home.path().join("config.toml");
            let original = format!("[auth]\nmfa_enforced={initial}\n");
            std::fs::write(&path, &original).unwrap();
            let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
            crate::mfa::reload_policy(home.path());
            let conn = rusqlite::Connection::open(home.path().join("users.db")).unwrap();
            conn.execute_batch("CREATE TRIGGER fail_policy BEFORE INSERT ON mfa_policy_state BEGIN SELECT RAISE(FAIL, 'policy write failed'); END;").unwrap();
            let next: toml::Table = format!("[auth]\nmfa_enforced={}\n", !initial).parse().unwrap();
            let error = commit_mfa_table_inner(&path, &content_hash(&original), &next, "test-admin", || Ok(())).unwrap_err();
            assert!(error.contains("policy write failed"), "{error}");
            assert_eq!(policy_value(home.path()), initial);
            let disk = std::fs::read_to_string(&path).unwrap();
            if initial {
                assert_eq!(disk.parse::<toml::Table>().unwrap(), next);
                assert!(crate::mfa::enforced(home.path(), false).unwrap());
            } else {
                assert_eq!(disk, original);
            }
            assert_eq!(crate::mfa::policy_status(home.path())["degraded"], true);
            conn.execute_batch("DROP TRIGGER fail_policy").unwrap();
            crate::mfa::reload_policy(home.path());
            assert!(!policy_value(home.path()));
            assert_eq!(crate::mfa::policy_status(home.path())["degraded"], false);
        }
    }

    #[test]
    fn mfa_commit_guard_and_hash_fail_before_policy_write() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        let original = "[auth]\nmfa_enforced=false\n";
        std::fs::write(&path, original).unwrap();
        let _db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        crate::mfa::reload_policy(home.path());
        let next = "[auth]\nmfa_enforced=true\n".parse().unwrap();
        assert!(commit_mfa_table_inner(&path, &content_hash(original), &next, "test-admin", || Err("guard refused".into())).is_err());
        assert!(commit_mfa_table_inner(&path, "stale hash", &next, "test-admin", || panic!("guard ran before CAS")).is_err());
        assert!(!policy_value(home.path()));
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[tokio::test]
    async fn mfa_config_writers_recheck_admin_inside_commit_guard() {
        let home=tempfile::tempdir().unwrap();
        let path=home.path().join("config.toml");
        let original="[auth]\nmfa_enforced=false\n";
        let replacement="[auth]\nmfa_enforced=true\n";
        std::fs::write(&path,original).unwrap();
        let db=std::sync::Arc::new(duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap());
        let user=db.create_user("admin@example.com","Admin","password-test",duduclaw_auth::UserRole::Admin).unwrap();
        crate::mfa::require_admin(&db,&user.id).unwrap();
        db.update_user(&user.id,None,Some(duduclaw_auth::UserRole::Employee),None).unwrap();
        assert!(commit_text_locked_with_guard(&path,&content_hash(original),replacement,
            ||crate::mfa::require_admin(&db,&user.id)).is_err());
        let db2=db.clone(); let id=user.id.clone();
        assert!(commit_table_locked_with_guard(&path,content_hash(original),&replacement.parse().unwrap(),
            move ||crate::mfa::require_admin(&db2,&id)).await.is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(),original);
    }
}
