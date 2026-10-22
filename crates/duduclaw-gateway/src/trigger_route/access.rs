//! Who the sender of a message is, for deciding which employees they may
//! call. Every lookup failure narrows (fail closed): a broken identity store
//! never grants more than an unknown sender gets.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use duduclaw_agent::registry::LoadedAgent;
use duduclaw_auth::{UserDb, UserRole, UserStatus};

/// The human behind a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sender {
    /// An active dashboard Admin: may call everyone.
    Admin,
    /// An active dashboard account with these employee bindings.
    Account {
        user_id: String,
        bound: BTreeSet<String>,
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
/// public_trigger`. Calling anyone else needs a binding, or Admin.
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

    /// From a role and binding set already loaded (e.g. WebChat's ACL cache).
    pub fn from_role(user_id: &str, role: UserRole, bound: BTreeSet<String>) -> Self {
        let sender = if role == UserRole::Admin {
            Sender::Admin
        } else {
            Sender::Account {
                user_id: user_id.to_string(),
                bound,
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
                bound: BTreeSet::new(),
            },
        }
    }

    /// May this sender call `agent` in a conversation whose default employee
    /// is `fallback`?
    pub fn may_call(&self, agent: &LoadedAgent, fallback: Option<&str>) -> bool {
        let id = agent.config.agent.name.as_str();
        let open_here = fallback == Some(id) || is_public_trigger(agent);
        match &self.sender {
            Sender::Admin => true,
            Sender::Account { bound, .. } => open_here || bound.contains(id),
            Sender::Unmapped => open_here,
        }
    }

    /// Stable, non-identifying label for logs and the audit trail. An
    /// unmapped sender is an HMAC of the channel id under a per-home key, so
    /// the platform id cannot be recovered from the log by guessing.
    pub fn audit_label(&self, home: &Path, channel: &str, channel_sender: &str) -> String {
        match &self.sender {
            Sender::Admin => "admin".to_string(),
            Sender::Account { user_id, .. } => format!("user:{user_id}"),
            Sender::Unmapped => format!(
                "unmapped:{channel}:{}",
                keyed_digest(home, channel, channel_sender)
            ),
        }
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
pub fn for_dashboard_user(db: &UserDb, user_id: &str) -> SenderAccess {
    match load_account(db, user_id) {
        Ok(Some((role, bound))) => SenderAccess::from_role(user_id, role, bound),
        Ok(None) | Err(_) => SenderAccess::no_bindings(user_id),
    }
}

/// How long one channel sender's identity lookup is reused (one event's
/// route is computed several times; a revoked binding still takes effect
/// within this window).
const LOOKUP_TTL: Duration = Duration::from_secs(5);
const LOOKUP_MAX: usize = 4_096;

type LookupCache = HashMap<(PathBuf, String, String), (SenderAccess, Instant)>;

fn lookup_cache() -> std::sync::MutexGuard<'static, LookupCache> {
    static CACHE: OnceLock<Mutex<LookupCache>> = OnceLock::new();
    CACHE
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|e| e.into_inner())
}

/// A messaging-app sender: mapped through a **verified** `channel_identities`
/// row to an active dashboard account, else unmapped. An absent or unreadable
/// identity store, an unverified row or an inactive account all mean
/// unmapped (the least an app sender can have). Reused for a few seconds.
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
    if let Some((access, at)) = lookup_cache().get(&key) {
        if now.saturating_duration_since(*at) < LOOKUP_TTL {
            return access.clone();
        }
    }
    let access = lookup_channel_sender(home, channel, channel_user_id);
    let mut cache = lookup_cache();
    if cache.len() >= LOOKUP_MAX {
        cache.retain(|_, (_, at)| now.saturating_duration_since(*at) < LOOKUP_TTL);
        if cache.len() >= LOOKUP_MAX {
            cache.clear();
        }
    }
    cache.insert(key, (access.clone(), now));
    access
}

pub(crate) fn lookup_channel_sender(home: &Path, channel: &str, channel_user_id: &str) -> SenderAccess {
    let Ok(Some(db)) = crate::decision_notify::open_user_db(home) else {
        return SenderAccess::unmapped();
    };
    let Ok(Some(user_id)) = db.find_verified_user_id_by_channel(channel, channel_user_id) else {
        return SenderAccess::unmapped();
    };
    match load_account(&db, &user_id) {
        Ok(Some((role, bound))) => SenderAccess::from_role(&user_id, role, bound),
        _ => SenderAccess::unmapped(),
    }
}

/// Role and bindings of an **active** account; `None` when missing or
/// inactive.
pub(crate) fn load_account(
    db: &UserDb,
    user_id: &str,
) -> Result<Option<(UserRole, BTreeSet<String>)>, String> {
    let Some(user) = db.get_user(user_id)? else {
        return Ok(None);
    };
    if user.status != UserStatus::Active {
        return Ok(None);
    }
    let bound = db
        .get_user_agents(user_id)?
        .into_iter()
        .map(|b| b.agent_name)
        .collect();
    Ok(Some((user.role, bound)))
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
