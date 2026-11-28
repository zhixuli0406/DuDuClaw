//! RM-213 credential entry and per-call gates. No cached config/credential authority.
use std::collections::HashMap;
use std::path::Path;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use duduclaw_auth::{UserContext, UserDb};
use duduclaw_auth::member_keys::MemberKeyLookup;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub enabled: bool,
    pub rate_per_minute: u32,
    pub allow_conversation_content: bool,
    pub allow_mcp_scopes: Vec<String>,
}
impl Default for Config {
    fn default() -> Self { Self { enabled: false, rate_per_minute: 120, allow_conversation_content: false, allow_mcp_scopes: Vec::new() } }
}
impl Config {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let root: toml::Table = toml::from_str(text).ok()?;
        let config: Self = match root.get("member_api_keys") {
            None => Self::default(),
            Some(section) if section.is_table() => section.clone().try_into().ok()?,
            Some(_) => return None,
        };
        (config.rate_per_minute > 0 && config.rate_per_minute <= 60_000
            && config.allow_mcp_scopes.iter().all(|scope|
                duduclaw_core::mcp_scopes::PERSON_KEY_MCP_SCOPE_STRINGS.contains(&scope.as_str()))).then_some(config)
    }
}
pub(crate) fn config(home: &Path) -> Config {
    read_config(home).unwrap_or_default()
}

/// Preserve unavailable/invalid configuration separately from an explicit disable.
pub(crate) fn read_config(home: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(home.join("config.toml"))
        .map_err(|_| "member key configuration unavailable".to_owned())?;
    Config::parse(&text).ok_or_else(|| "member key configuration unavailable".to_owned())
}

pub(crate) fn authenticate(home: &Path, db: &UserDb, raw: &str) -> Result<UserContext, String> {
    if !read_config(home)?.enabled { return Err("member keys disabled".into()); }
    match db.member_key_lookup(raw)? {
        MemberKeyLookup::Valid { token_id, user_id } => refresh_id(home, db, &token_id, &user_id),
        _ => Err("member key invalid".into()),
    }
}
fn refresh_id(home: &Path, db: &UserDb, id: &str, user: &str) -> Result<UserContext, String> {
    if !read_config(home)?.enabled { return Err("member keys disabled".into()); }
    db.member_key_context(id, user)
        .map(|mut ctx| {
            ctx.origin = duduclaw_auth::acl::ContextOrigin::MemberKey;
            crate::machine_employee::without_restricted_access(home, ctx)
        })
}
pub(crate) fn refresh(home: &Path, db: &UserDb, ctx: &UserContext) -> Result<UserContext, String> {
    refresh_authority(home, db, ctx).map(|live| live.context)
}
pub(crate) fn refresh_authority(home: &Path, db: &UserDb, ctx: &UserContext)
    -> Result<duduclaw_auth::member_keys::MemberKeyAuthority, String> {
    let id = ctx.member_key_id.as_deref().ok_or("member key invalid")?;
    if !read_config(home)?.enabled { return Err("member keys disabled".into()); }
    let mut live = db.member_key_authority(id, &ctx.user_id)?;
    live.context.origin = duduclaw_auth::acl::ContextOrigin::MemberKey;
    live.context = crate::machine_employee::without_restricted_access(home, live.context);
    if live.context.credential_epoch != ctx.credential_epoch { return Err("member key invalid".into()); }
    Ok(live)
}

#[derive(Default)]
struct Limiter { entries: HashMap<(std::path::PathBuf, String), (Instant, u32)> }
impl Limiter {
    fn allow(&mut self, home: &Path, id: &str, limit: u32, now: Instant) -> bool {
        self.entries.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        let key = (home.to_path_buf(), id.to_owned());
        if self.entries.len() >= 4096 && !self.entries.contains_key(&key) { return false; }
        let (_, count) = self.entries.entry(key).or_insert((now, 0));
        if *count >= limit { return false; }
        *count += 1;
        true
    }
}
static LIMITER: LazyLock<Mutex<Limiter>> = LazyLock::new(|| Mutex::new(Limiter::default()));

/// Called once per RPC/HTTP operation/chat command, never for output frames.
pub(crate) fn record_call(home: &Path, db: &UserDb, ctx: &UserContext, method: &str, write: bool) -> Result<(), String> {
    let live = refresh(home, db, ctx)?;
    let id = live.member_key_id.as_deref().ok_or("member key invalid")?;
    let settings = read_config(home)?;
    if !settings.enabled { return Err("member keys disabled".into()); }
    let allowed = LIMITER.lock().map_err(|_| "member key limiter unavailable")?
        .allow(home, id, settings.rate_per_minute, Instant::now());
    db.member_key_record_call(id, &live.user_id, if allowed { "ok" } else { "rate_limited" }, 0, None, chrono::Utc::now())?;
    if !allowed { return Err("member_key_rate_limited".into()); }
    if write { db.member_key_record_write(id, &live.user_id, method)?; }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn config_fails_closed() {
        for text in ["", "[member_api_keys]\nenabled='true'", "[member_api_keys]\nenabled=true\nrate_per_minute=0", "bad ["] {
            assert!(!Config::parse(text).unwrap_or_default().enabled);
        }
        assert!(Config::parse("[member_api_keys]\nenabled=true").unwrap().enabled);
        assert_eq!(Config::default().rate_per_minute, 120);
    }
    #[test]
    fn config_rejects_array_sections_and_preserves_table_defaults() {
        for text in ["member_api_keys=[]", "member_api_keys=['x']"] {
            assert!(Config::parse(text).is_none(), "{text}");
        }
        for text in ["", "[member_api_keys]", "[member_api_keys]\nenabled=false"] {
            let config = Config::parse(text).unwrap();
            assert!(!config.enabled);
            assert_eq!(config.rate_per_minute, 120);
            assert!(!config.allow_conversation_content);
            assert!(config.allow_mcp_scopes.is_empty());
        }
    }
    #[test]
    fn conversation_and_mcp_config_parse_fail_closed() {
        let defaults = Config::parse("[member_api_keys]\nenabled=true").unwrap();
        assert!(!defaults.allow_conversation_content && defaults.allow_mcp_scopes.is_empty());
        let enabled = Config::parse("[member_api_keys]\nenabled=true\nallow_conversation_content=true\nallow_mcp_scopes=['tasks:read','agents:status','odoo:read','odoo:write']").unwrap();
        assert!(enabled.enabled && enabled.allow_conversation_content);
        assert_eq!(enabled.allow_mcp_scopes.len(), 4);
        for field in ["allow_conversation_content='true'", "allow_mcp_scopes=['tasks:read','admin']",
            "allow_mcp_scopes=['unknown']", "allow_mcp_scopes='tasks:read'", "unknown=true"] {
            assert!(!Config::parse(&format!("[member_api_keys]\nenabled=true\n{field}")).unwrap_or_default().enabled);
        }
    }
    #[test]
    fn per_key_rate_and_map_are_bounded() {
        let mut limiter = Limiter::default();
        let now = Instant::now();
        assert!(limiter.allow(Path::new("a"), "one", 1, now));
        assert!(!limiter.allow(Path::new("a"), "one", 1, now));
        for i in 1..4096 { assert!(limiter.allow(Path::new("a"), &i.to_string(), 1, now)); }
        assert!(!limiter.allow(Path::new("a"), "overflow", 1, now));
        assert!(limiter.allow(Path::new("a"), "one", 1, now + Duration::from_secs(60)));
    }
}
