//! Who the sender of a message is, for deciding which employees they may
//! call. Every lookup failure narrows (fail closed): a broken identity store
//! never grants more than an unknown sender gets.
//!
//! A mapped account's callable set is its [`EffectiveAccess`] read from
//! `users.db` (`duduclaw_auth::org`): direct bindings plus, for a Manager,
//! the employees bound to anyone below them in the reporting tree. The
//! conversation's default employee and `[channels] public_trigger`
//! employees are open to every sender on top of that.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_auth::{EffectiveAccess, GrantSource, UserDb, UserRole};

/// The human behind a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// An active dashboard Admin: may call everyone. `user_id` is `None`
    /// only for a synthetic "everyone" sender (no real account).
    Admin { user_id: Option<String> },
    /// An active dashboard account and what it may call (read live).
    Account {
        user_id: String,
        access: Arc<EffectiveAccess>,
    },
    /// Nobody we can name: a messaging-app sender without a verified
    /// identity, a public web-widget visitor, or a failed lookup.
    Unmapped,
}

/// A sender, for `@` routing.
///
/// Every sender may call the conversation's default employee (in WebChat the
/// default/main employee stays reachable by every signed-in account, as
/// before `@` routing existed) and employees opened with `[channels]
/// public_trigger`. Calling anyone else needs a direct binding, a team grant
/// (a Manager calling a report's employee), or Admin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderAccess {
    pub sender: Sender,
}

impl SenderAccess {
    /// An unmapped sender.
    pub fn unmapped() -> Self {
        Self {
            sender: Sender::Unmapped,
        }
    }

    /// From an effective access already loaded (an Admin role becomes
    /// [`Sender::Admin`]).
    pub fn from_effective(access: Arc<EffectiveAccess>) -> Self {
        let sender = if access.role == UserRole::Admin {
            Sender::Admin {
                user_id: Some(access.user_id.clone()),
            }
        } else {
            Sender::Account {
                user_id: access.user_id.clone(),
                access,
            }
        };
        Self { sender }
    }

    /// An account that may call only what everyone may (unknown, inactive
    /// or unreadable).
    pub fn no_bindings(user_id: &str) -> Self {
        Self {
            sender: Sender::Account {
                user_id: user_id.to_string(),
                access: Arc::new(empty_access(user_id, 0)),
            },
        }
    }

    /// Why this sender may call `agent` in a conversation whose default
    /// employee is `fallback`, or `None` when they may not. A binding or a
    /// team grant is reported before the "open to everyone" sources.
    pub fn grant(&self, agent: &LoadedAgent, fallback: Option<&str>) -> Option<GrantSource> {
        let id = agent.config.agent.name.as_str();
        let open_here = || {
            if fallback == Some(id) {
                Some(GrantSource::Default)
            } else if is_public_trigger(agent) {
                Some(GrantSource::Public)
            } else {
                None
            }
        };
        match &self.sender {
            Sender::Admin { .. } => Some(GrantSource::Admin),
            Sender::Account { access, .. } => access
                .level(id)
                .map(|(_, source)| source)
                .or_else(open_here),
            Sender::Unmapped => open_here(),
        }
    }

    /// May this sender call `agent` in a conversation whose default employee
    /// is `fallback`?
    pub fn may_call(&self, agent: &LoadedAgent, fallback: Option<&str>) -> bool {
        self.grant(agent, fallback).is_some()
    }

    /// Stable, non-identifying label for logs and the audit trail. An
    /// unmapped sender is an HMAC of the channel id under a per-home key, so
    /// the platform id cannot be recovered from the log by guessing.
    pub fn audit_label(&self, home: &Path, channel: &str, channel_sender: &str) -> String {
        match &self.sender {
            Sender::Admin { user_id: Some(id) } => format!("user:{id}"),
            Sender::Admin { user_id: None } => "admin".to_string(),
            Sender::Account { user_id, .. } => format!("user:{user_id}"),
            Sender::Unmapped => format!(
                "unmapped:{channel}:{}",
                keyed_digest(home, channel, channel_sender)
            ),
        }
    }
}

/// No bindings, no team, never Admin.
fn empty_access(user_id: &str, generation: u64) -> EffectiveAccess {
    EffectiveAccess {
        user_id: user_id.to_string(),
        role: UserRole::Employee,
        direct: BTreeMap::new(),
        team: BTreeMap::new(),
        generation,
    }
}

/// `[channels] public_trigger = true` on this employee.
pub fn is_public_trigger(agent: &LoadedAgent) -> bool {
    agent
        .config
        .channels
        .as_ref()
        .is_some_and(|c| c.public_trigger)
}

/// A dashboard account. Unknown, inactive or unreadable accounts get no
/// bindings and are never Admin.
pub fn for_dashboard_user(home: &Path, db: &UserDb, user_id: &str) -> SenderAccess {
    match load_account(home, db, user_id) {
        Ok(Some(access)) => SenderAccess::from_effective(access),
        Ok(None) | Err(_) => SenderAccess::no_bindings(user_id),
    }
}

/// How long one channel sender's identity lookup may be reused. Every
/// authorization write bumps `auth_meta.acl_generation`, and an entry is
/// only reused at the generation it was read at, so a revoked binding or a
/// changed manager takes effect on the next message; this TTL is a backstop.
const LOOKUP_TTL: Duration = Duration::from_secs(5);
const LOOKUP_MAX: usize = 4_096;

type LookupCache = HashMap<(PathBuf, String, String), (SenderAccess, Instant, u64)>;

fn lookup_cache() -> std::sync::MutexGuard<'static, LookupCache> {
    static CACHE: OnceLock<Mutex<LookupCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// `auth_meta.acl_generation` through a read-only connection (kept per
/// home and reopened after any error), or `None` when the identity store is
/// absent or unreadable (the cache is then bypassed, never trusted).
fn read_generation(home: &Path) -> Option<u64> {
    type Conns = HashMap<PathBuf, rusqlite::Connection>;
    static CONNS: OnceLock<Mutex<Conns>> = OnceLock::new();
    let mut conns = CONNS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let query = |conn: &rusqlite::Connection| {
        conn.query_row(
            "SELECT value FROM auth_meta WHERE key = 'acl_generation'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .ok()
        .and_then(|v| u64::try_from(v).ok())
    };
    if let Some(conn) = conns.get(home) {
        if let Some(g) = query(conn) {
            return Some(g);
        }
        conns.remove(home);
    }
    let conn = crate::review_evidence::audience::readonly_db(&home.join("users.db")).ok()?;
    let g = query(&conn)?;
    if conns.len() >= 64 {
        conns.clear();
    }
    conns.insert(home.to_path_buf(), conn);
    Some(g)
}

/// A messaging-app sender: mapped through a **verified** `channel_identities`
/// row to an active dashboard account, else unmapped. An absent or unreadable
/// identity store, an unverified row or an inactive account all mean
/// unmapped (the least an app sender can have). Reused while the ACL
/// generation is unchanged, for a few seconds at most.
pub fn for_channel_sender(home: &Path, channel: &str, channel_user_id: &str) -> SenderAccess {
    if channel_user_id.is_empty() {
        return SenderAccess::unmapped();
    }
    let key = (
        home.to_path_buf(),
        channel.to_string(),
        channel_user_id.to_string(),
    );
    let now = Instant::now();
    let generation = read_generation(home);
    if let Some(g) = generation {
        if let Some((access, at, cached_gen)) = lookup_cache().get(&key) {
            if *cached_gen == g && now.saturating_duration_since(*at) < LOOKUP_TTL {
                return access.clone();
            }
        }
    }
    let access = lookup_channel_sender(home, channel, channel_user_id);
    // Cache only under a generation read before the lookup: a write in
    // between bumps it, so the entry is unreachable rather than stale.
    if let Some(g) = generation {
        let mut cache = lookup_cache();
        if cache.len() >= LOOKUP_MAX {
            cache.retain(|_, (_, at, cg)| *cg == g && now.saturating_duration_since(*at) < LOOKUP_TTL);
            if cache.len() >= LOOKUP_MAX {
                cache.clear();
            }
        }
        cache.insert(key, (access.clone(), now, g));
    }
    access
}

fn lookup_channel_sender(home: &Path, channel: &str, channel_user_id: &str) -> SenderAccess {
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return SenderAccess::unmapped();
    };
    let Ok(Some(user_id)) = db.find_verified_user_id_by_channel(channel, channel_user_id) else {
        return SenderAccess::unmapped();
    };
    match load_account(home, &db, &user_id) {
        Ok(Some(access)) => SenderAccess::from_effective(access),
        Ok(None) => SenderAccess::unmapped(),
        Err(e) => {
            tracing::warn!(channel, error = %e, "sender access read failed — treating the sender as unmapped");
            SenderAccess::unmapped()
        }
    }
}

/// The **active** dashboard account a messaging-app sender is bound to
/// through a **verified** `channel_identities` row; `None` for an unbound
/// sender, an unverified row, an inactive account or any lookup failure
/// (the least a sender can have). Used by the knowledge base to decide whose
/// collections a channel turn may read.
pub fn verified_user_for_channel_sender(
    home: &Path,
    channel: &str,
    channel_user_id: &str,
) -> Option<String> {
    if channel_user_id.is_empty() {
        return None;
    }
    let db = crate::decision_notify::open_user_db(home).ok().flatten()?;
    let user_id = db
        .find_verified_user_id_by_channel(channel, channel_user_id)
        .ok()
        .flatten()?;
    load_account(home, &db, &user_id).ok().flatten().map(|_| user_id)
}

/// What an **active** account may call, read live (role, direct bindings,
/// team grants, generation); `None` when missing or inactive. The single
/// computation point for channel senders, dashboard users and WebChat.
/// Team grants are dropped unless `home` runs the Enterprise edition
/// (`team_edition.rs`).
pub(crate) fn load_account(
    home: &Path,
    db: &UserDb,
    user_id: &str,
) -> Result<Option<Arc<EffectiveAccess>>, String> {
    Ok(db
        .effective_access(user_id)?
        .map(|a| Arc::new(crate::team_edition::gate_access(home, a))))
}

/// An inactive account's access at `generation`: nothing but what everyone
/// may call.
pub(crate) fn inactive_access(user_id: &str, generation: u64) -> Arc<EffectiveAccess> {
    Arc::new(empty_access(user_id, generation))
}

/// File holding the per-home key for audit identifiers (32 random bytes,
/// owner-only, created once).
const AUDIT_KEY_FILE: &str = "trigger_audit.key";

fn audit_key(home: &Path) -> Option<Vec<u8>> {
    static KEYS: OnceLock<Mutex<HashMap<PathBuf, Vec<u8>>>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut keys = keys.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = keys.get(home) {
        return Some(k.clone());
    }
    let path = home.join(AUDIT_KEY_FILE);
    let key = match std::fs::read(&path) {
        Ok(k) if k.len() == 32 => k,
        Ok(_) => return None,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => create_audit_key(&path)?,
        Err(_) => return None,
    };
    keys.insert(home.to_path_buf(), key.clone());
    Some(key)
}

fn create_audit_key(path: &Path) -> Option<Vec<u8>> {
    use rand::RngCore;
    use rand::rngs::OsRng;
    use std::io::Write;
    let mut key = vec![0u8; 32];
    OsRng.fill_bytes(&mut key);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    match opts.open(path) {
        Ok(mut f) => f.write_all(&key).ok().map(|_| key),
        // Another task created it first: use theirs.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::read(path).ok().filter(|k| k.len() == 32)
        }
        Err(_) => None,
    }
}

/// HMAC-SHA256(key, channel ‖ 0 ‖ id), 16 hex characters. Without a usable
/// key the identifier is withheld (`"unavailable"`), never logged raw.
fn keyed_digest(home: &Path, channel: &str, id: &str) -> String {
    use hmac::{Hmac, Mac};
    let Some(key) = audit_key(home) else {
        return "unavailable".to_string();
    };
    let Ok(mut mac) = Hmac::<sha2::Sha256>::new_from_slice(&key) else {
        return "unavailable".to_string();
    };
    mac.update(channel.as_bytes());
    mac.update(&[0]);
    mac.update(id.as_bytes());
    let hex = hex::encode(mac.finalize().into_bytes());
    duduclaw_core::truncate_bytes(&hex, 16).to_string()
}
