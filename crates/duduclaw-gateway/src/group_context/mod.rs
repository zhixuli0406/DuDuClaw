//! Opt-in, bounded group conversation context. The gateway is the only writer.
//! Settings writes and buffer writes take the sessions database writer lock in
//! the same order, so a concurrent disable cannot leave a late captured row.
use std::path::Path;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use crate::channel_reply::ReplyContext;

mod prompt;
pub use prompt::{format_context, prompt_context, prompt_context_before};
#[cfg(test)]
mod tests;

pub const SETTING_KEY: &str = "group_context";
pub const UNAVAILABLE: &str = "not available";
pub const CHANNELS: &[&str] = &["telegram", "discord", "slack", "line", "feishu", "dingtalk", "teams", "googlechat"];
pub const TEXT_LIMIT: usize = 2_000;

fn default_messages() -> usize { 50 }
fn default_hours() -> u32 { 24 }

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupConfig {
    pub channel: String,
    pub group_id: String,
    pub scope_id: String,
    #[serde(default = "default_messages")]
    pub max_messages: usize,
    #[serde(default = "default_hours")]
    pub max_age_hours: u32,
}

impl GroupConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !CHANNELS.contains(&self.channel.as_str()) || !valid_id(&self.group_id)
            || !valid_id(&self.scope_id) || self.group_id == "global"
            || (self.channel != "discord" && self.scope_id != "global")
            || (self.channel == "discord" && self.scope_id == "global")
            || !(1..=200).contains(&self.max_messages) || !(1..=168).contains(&self.max_age_hours)
        { return Err("Invalid group or retention limits (messages: 1–200; hours: 1–168)".into()); }
        Ok(())
    }
}

fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BufferedMessage {
    pub id: i64,
    pub channel: String,
    pub group_id: String,
    pub thread_id: Option<String>,
    pub message_id: String,
    pub sender_id: String,
    pub sender_name: String,
    pub text: String,
    pub timestamp: i64,
    pub direction: String,
}

fn settings_db(home: &Path, write: bool) -> Result<Connection, String> {
    let flags = if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let db = Connection::open_with_flags(home.join("sessions.db"), flags).map_err(|_| UNAVAILABLE)?;
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| UNAVAILABLE)?;
    Ok(db)
}

fn buffer_present(home: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(home.join("group_context.db")) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(_) => Err(UNAVAILABLE.into()),
    }
}

/// Validate through an O_NOFOLLOW handle and chmod that handle, not a path
/// that could have been exchanged after inspection. SQLite opens separately,
/// so keep this handle alive and verify its inode again after SQLite's open.
fn private_file(path: &Path, create: bool) -> Result<std::fs::File, String> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => return Err(UNAVAILABLE.into()),
        Ok(_) => (),
        Err(e) if create && e.kind() == std::io::ErrorKind::NotFound => (),
        Err(_) => return Err(UNAVAILABLE.into()),
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(create).create(create);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    let file = opts.open(path).map_err(|_| UNAVAILABLE)?;
    let meta = file.metadata().map_err(|_| UNAVAILABLE)?;
    if !meta.is_file() { return Err(UNAVAILABLE.into()); }
    #[cfg(unix)] {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        // SAFETY: geteuid has no preconditions.
        if meta.uid() != unsafe { libc::geteuid() } { return Err(UNAVAILABLE.into()); }
        if meta.mode() & 0o7777 != 0o600 {
            file.set_permissions(std::fs::Permissions::from_mode(0o600)).map_err(|_| UNAVAILABLE)?;
        }
    }
    Ok(file)
}

fn buffer_db(home: &Path, write: bool) -> Result<Connection, String> {
    // Canonicalize only the trusted home, never a database or sidecar symlink.
    let path = std::fs::canonicalize(home).map_err(|_| UNAVAILABLE)?.join("group_context.db");
    let file = private_file(&path, write)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = path.with_file_name(format!("group_context.db{suffix}"));
        match std::fs::symlink_metadata(&sidecar) {
            Ok(_) => { private_file(&sidecar, false)?; },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(_) => return Err(UNAVAILABLE.into()),
        }
    }
    let flags = if write { OpenFlags::SQLITE_OPEN_READ_WRITE } else { OpenFlags::SQLITE_OPEN_READ_ONLY };
    let db = Connection::open_with_flags(&path, flags | OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(|_| UNAVAILABLE)?;
    let after = std::fs::symlink_metadata(&path).map_err(|_| UNAVAILABLE)?;
    if !after.is_file() || after.file_type().is_symlink() { return Err(UNAVAILABLE.into()); }
    #[cfg(unix)] {
        use std::os::unix::fs::MetadataExt;
        let before = file.metadata().map_err(|_| UNAVAILABLE)?;
        if (before.dev(), before.ino()) != (after.dev(), after.ino()) { return Err(UNAVAILABLE.into()); }
    }
    db.busy_timeout(std::time::Duration::from_secs(5)).map_err(|_| UNAVAILABLE)?;
    if write {
        db.execute_batch("PRAGMA secure_delete=ON;
            PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS group_context_messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                channel TEXT NOT NULL, group_id TEXT NOT NULL, thread_id TEXT,
                message_id TEXT NOT NULL, sender_id TEXT NOT NULL, sender_name TEXT NOT NULL,
                text TEXT NOT NULL, timestamp INTEGER NOT NULL,
                direction TEXT NOT NULL CHECK(direction IN ('inbound','bot_reply')));
            CREATE INDEX IF NOT EXISTS group_context_group_time ON group_context_messages(channel,group_id,timestamp,id);
            CREATE UNIQUE INDEX IF NOT EXISTS group_context_message ON group_context_messages(channel,group_id,COALESCE(thread_id,''),message_id,direction);")
            .map_err(|_| UNAVAILABLE)?;
    }
    Ok(db)
}

fn config_from(db: &Connection, channel: &str, group: &str) -> Result<Option<GroupConfig>, String> {
    let raw: Option<String> = db.query_row(
        "SELECT value FROM channel_settings WHERE channel_type=?1 AND scope_id=?2 AND key=?3",
        params![channel,group,SETTING_KEY], |r| r.get(0),
    ).optional().map_err(|_| UNAVAILABLE)?;
    let Some(raw) = raw else { return Ok(None) };
    let config: GroupConfig = serde_json::from_str(&raw).map_err(|_| UNAVAILABLE)?;
    config.validate().map_err(|_| UNAVAILABLE)?;
    if config.channel != channel || config.group_id != group { return Err(UNAVAILABLE.into()); }
    Ok(Some(config))
}

fn list_from(db: &Connection) -> Result<Vec<GroupConfig>, String> {
    let mut stmt = db.prepare("SELECT channel_type,scope_id FROM channel_settings WHERE key=?1 ORDER BY channel_type,scope_id")
        .map_err(|_| UNAVAILABLE)?;
    let ids = stmt.query_map([SETTING_KEY], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?)))
        .map_err(|_| UNAVAILABLE)?.collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE)?;
    ids.into_iter().map(|(ch,id)| config_from(db,&ch,&id)?.ok_or_else(|| UNAVAILABLE.into())).collect()
}

/// Read-only configuration enumeration for the independently authenticated MCP
/// server. Invalid configuration denies the whole listing, never falls back.
pub fn configured_groups(home: &Path) -> Result<Vec<GroupConfig>, String> {
    list_from(&settings_db(home, false)?)
}

pub fn configs(home: &Path) -> Result<Vec<GroupConfig>, String> {
    let db = settings_db(home,false)?;
    let mut groups = Vec::new();
    for config in list_from(&db)? {
        if allowed(&db,&config.channel,&config.scope_id,&config.group_id)? { groups.push(config); }
    }
    Ok(groups)
}

fn setting(db: &Connection, channel: &str, scope: &str, key: &str) -> Result<Option<String>, String> {
    db.query_row("SELECT value FROM channel_settings WHERE channel_type=?1 AND key=?2 AND scope_id IN (?3,'global') ORDER BY CASE WHEN scope_id=?3 THEN 0 ELSE 1 END LIMIT 1",
        params![channel,key,scope], |r| r.get(0)).optional().map_err(|_| UNAVAILABLE.into())
}

fn allowed(db: &Connection, channel: &str, scope: &str, group: &str) -> Result<bool, String> {
    for (key, wanted, lookup) in [("allowed_channels",group,scope),("allowed_guilds",scope,"global")] {
        if key == "allowed_guilds" && channel != "discord" { continue; }
        if let Some(raw) = setting(db,channel,lookup,key)? {
            let ids: Vec<String> = serde_json::from_str(&raw).map_err(|_| UNAVAILABLE)?;
            if !ids.is_empty() && !ids.iter().any(|id| id == wanted) { return Ok(false); }
        }
    }
    Ok(true)
}

/// Admin RPC only. The sessions writer lock also fences capture and removal.
pub fn set(home: &Path, config: &GroupConfig) -> Result<(), String> {
    config.validate()?;
    let mut db = settings_db(home, true)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let raw = serde_json::to_string(config).map_err(|_| UNAVAILABLE)?;
    tx.execute("INSERT INTO channel_settings(channel_type,scope_id,key,value,updated_at) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(channel_type,scope_id,key) DO UPDATE SET value=excluded.value,updated_at=excluded.updated_at",
        params![config.channel,config.group_id,SETTING_KEY,raw,chrono::Utc::now().to_rfc3339()]).map_err(|_| UNAVAILABLE)?;
    // Shorter retention takes effect immediately, including already stored rows.
    if buffer_present(home)? {
        let buffer = buffer_db(home,true)?;
        prune_group(&buffer,config,chrono::Utc::now().timestamp())?;
        checkpoint(&buffer);
    }
    tx.commit().map_err(|_| UNAVAILABLE.into())
}

/// Delete rows before disabling the configuration, while holding the same
/// writer lock capture takes. Failure leaves an enabled, empty group (safe).
pub fn remove(home: &Path, channel: &str, group: &str) -> Result<(), String> {
    if !CHANNELS.contains(&channel) || !valid_id(group) { return Err(UNAVAILABLE.into()); }
    let mut db = settings_db(home, true)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    if buffer_present(home)? {
        let buffer = buffer_db(home,true)?;
        buffer.execute("DELETE FROM group_context_messages WHERE channel=?1 AND group_id=?2", params![channel,group]).map_err(|_| UNAVAILABLE)?;
        checkpoint(&buffer);
    }
    tx.execute("DELETE FROM channel_settings WHERE channel_type=?1 AND scope_id=?2 AND key=?3",params![channel,group,SETTING_KEY]).map_err(|_| UNAVAILABLE)?;
    tx.commit().map_err(|_| UNAVAILABLE.into())
}

/// A busy reader can prevent truncation even when SQLite returns Ok. Report
/// that result explicitly; the next retention batch retries the checkpoint.
fn checkpoint(db: &Connection) {
    let result = db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row|
        Ok((row.get::<_,i64>(0)?, row.get::<_,i64>(1)?, row.get::<_,i64>(2)?)));
    match result {
        Ok((0, 0, 0)) => (),
        Ok((busy, log_frames, checkpointed)) => tracing::warn!(busy, log_frames, checkpointed,
            "group context WAL truncation incomplete; retained bytes may remain until retry"),
        Err(error) => tracing::warn!(%error, "group context WAL truncation failed"),
    }
}

fn prune_group(db: &Connection, config: &GroupConfig, now: i64) -> Result<(), String> {
    db.execute("DELETE FROM group_context_messages WHERE channel=?1 AND group_id=?2 AND (timestamp<=?3 OR id NOT IN (SELECT id FROM group_context_messages WHERE channel=?1 AND group_id=?2 ORDER BY timestamp DESC,id DESC LIMIT ?4))",
        params![config.channel,config.group_id,now - i64::from(config.max_age_hours)*3600,config.max_messages as i64]).map_err(|_| UNAVAILABLE)?;
    Ok(())
}

fn insert(home: &Path, scope: &str, message: &BufferedMessage) -> Result<bool, String> {
    let mut settings = settings_db(home,true)?;
    let lock = settings.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let Some(config) = config_from(&lock,&message.channel,&message.group_id)? else { return Ok(false) };
    if config.scope_id != scope || !allowed(&lock,&message.channel,scope,&message.group_id)? { return Ok(false); }
    if message.channel == "discord" {
        if let Some(thread) = message.thread_id.as_deref() {
            if !allowed(&lock,&message.channel,scope,thread)? { return Ok(false); }
        }
    }
    if !valid_id(&message.message_id) || !valid_id(&message.sender_id)
        || message.thread_id.as_deref().is_some_and(|id| !valid_id(id))
        || !["inbound","bot_reply"].contains(&message.direction.as_str()) { return Err(UNAVAILABLE.into()); }
    let mut db = buffer_db(home,true)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    tx.execute("INSERT OR IGNORE INTO group_context_messages(channel,group_id,thread_id,message_id,sender_id,sender_name,text,timestamp,direction) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
        params![message.channel,message.group_id,message.thread_id,message.message_id,message.sender_id,message.sender_name,message.text,message.timestamp,message.direction]).map_err(|_| UNAVAILABLE)?;
    prune_group(&tx,&config,chrono::Utc::now().timestamp())?;
    tx.commit().map_err(|_| UNAVAILABLE)?;
    checkpoint(&db);
    lock.commit().map_err(|_| UNAVAILABLE)?;
    Ok(true)
}

/// Keep SQLite busy waits, credential detection and filesystem work off the
/// Tokio worker threads. Join failures are closed like other store failures.
pub(crate) async fn blocking<T: Send + 'static>(job: impl FnOnce() -> Result<T,String> + Send + 'static) -> Result<T,String> {
    tokio::task::spawn_blocking(job).await.map_err(|_| UNAVAILABLE.to_string())?
}

/// Single adapter hook. Capture never affects whether the adapter answers.
/// Credentials are masked in every derived store even in audit-only mode.
pub async fn capture(ctx: &ReplyContext, channel: &str, scope: &str, group_id: &str,
    thread_id: Option<&str>, message_id: &str, sender_id: &str, sender_name: &str, text: &str, direction: &str)
{
    // Bound input before cloning it into the blocking task. Truncation happens
    // after masking, so cutting a secret cannot defeat detection.
    if text.len() > 128 * 1024 || sender_name.len() > 4096 { return; }
    let home = ctx.home_dir.clone();
    let fallback_manager = ctx.redaction_manager.clone();
    let scope = scope.to_owned();
    let message = BufferedMessage {
        id:0,channel:channel.into(),group_id:group_id.into(),thread_id:thread_id.map(str::to_owned),
        message_id:message_id.into(),sender_id:sender_id.into(),sender_name:sender_name.into(),text:text.into(),
        timestamp:chrono::Utc::now().timestamp(),direction:direction.into(),
    };
    if let Err(error) = blocking(move || capture_message(&home,fallback_manager,&scope,message)).await {
        tracing::warn!(channel,group_id,error=%error,"group context capture skipped");
    }
}

fn capture_message(home: &Path, fallback_manager: Option<std::sync::Arc<duduclaw_redaction::RedactionManager>>, scope: &str, mut message: BufferedMessage) -> Result<bool,String> {
    let settings = settings_db(home,false)?;
    let Some(config) = config_from(&settings,&message.channel,&message.group_id)? else { return Ok(false) };
    if config.scope_id != scope || !allowed(&settings,&message.channel,scope,&message.group_id)? { return Ok(false); }
    let masked = duduclaw_security::credential_detect::mask_document(&message.text).map_err(|_| UNAVAILABLE)?;
    let name = duduclaw_security::credential_detect::mask_document(&message.sender_name).map_err(|_| UNAVAILABLE)?;
    // The live manager can change after ReplyContext was constructed. A
    // poisoned/missing manager while configured on is never passthrough.
    let manager = capture_manager(home, crate::redaction_sources::current().or(fallback_manager))?
        .filter(|m| m.source_policy().user_input.mode == duduclaw_redaction::config::SourceMode::On);
    let source = duduclaw_redaction::Source::UserChannelInput { channel_id: message.channel.clone() };
    let session = format!("{}:{}",message.channel,message.group_id);
    let redact = |value: &str| crate::redaction_sources::redact_text(manager.as_ref(),"group-context",Some(&session),value,&source);
    message.text = duduclaw_core::truncate_chars(&redact(&masked)?,TEXT_LIMIT).to_string();
    message.sender_name = duduclaw_core::truncate_chars(&redact(&name)?,100).to_string();
    insert(home,scope,&message)
}

fn capture_manager(home: &Path, manager: Option<std::sync::Arc<duduclaw_redaction::RedactionManager>>)
    -> Result<Option<std::sync::Arc<duduclaw_redaction::RedactionManager>>, String>
{
    use duduclaw_redaction::{CliFlag, EnvSetting};
    let env = EnvSetting::from_env();
    let cli = crate::redaction_integration::cli_flag_from_env();
    if crate::redaction_integration::force_disable_active(home) || env == EnvSetting::Off
        || (env != EnvSetting::On && cli == CliFlag::Off) { return Ok(None); }
    let raw = match std::fs::read_to_string(home.join("config.toml")) {
        Ok(raw) => Some(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return Err(UNAVAILABLE.into()),
    };
    let enabled_override = env == EnvSetting::On || cli == CliFlag::On;
    match crate::redaction_integration::classify_redaction_boot(raw.as_deref()) {
        crate::redaction_integration::BootOutcome::Poisoned(_) => Err(UNAVAILABLE.into()),
        crate::redaction_integration::BootOutcome::Enabled(_) => manager.map(Some).ok_or_else(|| UNAVAILABLE.into()),
        crate::redaction_integration::BootOutcome::Disabled if enabled_override => manager.map(Some).ok_or_else(|| UNAVAILABLE.into()),
        crate::redaction_integration::BootOutcome::Disabled => {
            // An explicit off setting supersedes a stale ReplyContext manager.
            // Missing legacy config still permits a supplied live manager.
            let explicit = raw.as_deref().and_then(|raw| raw.parse::<toml::Table>().ok())
                .is_some_and(|table| table.contains_key("redaction"));
            Ok(if explicit { None } else { manager })
        }
    }
}

/// Read-only access. Caller must separately verify the employee token and the
/// explicit binding. Expired rows are filtered even before maintenance runs.
pub fn read(home: &Path, channel: &str, group_id: &str, thread_id: Option<&str>, limit: usize, before: Option<i64>) -> Result<Vec<BufferedMessage>,String> {
    if !(1..=100).contains(&limit) { return Err(UNAVAILABLE.into()); }
    let settings = settings_db(home,false)?;
    let config = config_from(&settings,channel,group_id)?.ok_or(UNAVAILABLE)?;
    if !allowed(&settings,channel,&config.scope_id,group_id)? { return Err(UNAVAILABLE.into()); }
    if channel == "discord" {
        if let Some(thread) = thread_id {
            if !allowed(&settings,channel,&config.scope_id,thread)? { return Err(UNAVAILABLE.into()); }
        }
    }
    if !buffer_present(home)? { return Ok(Vec::new()); }
    let db = buffer_db(home,false)?;
    let mut stmt = db.prepare("SELECT id,channel,group_id,thread_id,message_id,sender_id,sender_name,text,timestamp,direction FROM group_context_messages WHERE channel=?1 AND group_id=?2 AND thread_id IS ?3 AND timestamp>?4 AND (?5 IS NULL OR timestamp<?5) AND id IN (SELECT id FROM group_context_messages WHERE channel=?1 AND group_id=?2 ORDER BY timestamp DESC,id DESC LIMIT ?6) ORDER BY timestamp DESC,id DESC LIMIT ?7").map_err(|_| UNAVAILABLE)?;
    stmt.query_map(params![channel,group_id,thread_id,chrono::Utc::now().timestamp()-i64::from(config.max_age_hours)*3600,before,config.max_messages as i64,limit as i64], |r| Ok(BufferedMessage {
        id:r.get(0)?,channel:r.get(1)?,group_id:r.get(2)?,thread_id:r.get(3)?,message_id:r.get(4)?,sender_id:r.get(5)?,sender_name:r.get(6)?,text:r.get(7)?,timestamp:r.get(8)?,direction:r.get(9)?,
    })).map_err(|_| UNAVAILABLE)?.collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE.into())
}

/// Called by the gateway maintenance loop. No database is created on an unused
/// installation; non-lock-holder processes do no work.
pub fn maintenance(home: &Path) -> Result<(),String> {
    if !duduclaw_core::gateway_instance::held(home) || !buffer_present(home)? { return Ok(()); }
    let mut settings = settings_db(home,true)?;
    let lock = settings.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let mut db = buffer_db(home,true)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let groups = {
        let mut stmt = tx.prepare("SELECT DISTINCT channel,group_id FROM group_context_messages").map_err(|_| UNAVAILABLE)?;
        stmt.query_map([],|r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(|_| UNAVAILABLE)?.collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE)?
    };
    for (channel,group) in groups {
        let config = config_from(&lock,&channel,&group).ok().flatten()
            .filter(|c| allowed(&lock,&channel,&c.scope_id,&group).unwrap_or(false));
        if let Some(config) = config {
            prune_group(&tx,&config,chrono::Utc::now().timestamp())?;
        } else {
            tx.execute("DELETE FROM group_context_messages WHERE channel=?1 AND group_id=?2",params![channel,group]).map_err(|_| UNAVAILABLE)?;
        }
    }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    checkpoint(&db);
    lock.commit().map_err(|_| UNAVAILABLE.into())
}

#[derive(Clone, Debug, Serialize)]
pub struct KnownGroup { pub channel: String, pub group_id: String }

/// Recent conversation IDs are suggestions only. The adapter's actual group
/// marker remains mandatory for capture; a suggested private conversation can
/// never cause its private messages to be retained.
pub fn known_groups(home: &Path) -> Result<Vec<KnownGroup>,String> {
    let db = settings_db(home,false)?;
    let mut stmt = db.prepare("SELECT id FROM sessions ORDER BY last_active DESC LIMIT 500").map_err(|_| UNAVAILABLE)?;
    let sessions = stmt.query_map([],|r| r.get::<_,String>(0)).map_err(|_| UNAVAILABLE)?.collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE)?;
    let mut seen = std::collections::BTreeSet::new();
    for session in sessions {
        let session = duduclaw_core::sensitivity::session_conversation(&session);
        let Some((channel,rest)) = session.split_once(':') else { continue };
        if !CHANNELS.contains(&channel) { continue; }
        let group = match channel {
            "telegram" if rest.starts_with('-') => rest.split_once(':').map_or(rest,|(group,_)| group),
            "telegram" => continue,
            "slack" => match rest.strip_prefix("group:") { Some(group) => group, None => continue },
            // Thread sessions do not encode the parent group. They are never
            // offered as a parent group inferred from a thread id.
            "discord" if rest.starts_with("thread:") || rest.starts_with("dm:") => continue,
            "line" if rest.starts_with('U') => continue,
            _ => rest,
        };
        if valid_id(group) { seen.insert((channel.to_string(),group.to_string())); }
    }
    Ok(seen.into_iter().map(|(channel,group_id)| KnownGroup{channel,group_id}).collect())
}
