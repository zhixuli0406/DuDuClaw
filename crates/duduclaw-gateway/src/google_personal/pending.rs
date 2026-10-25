//! One-time authorization state, bound to a home, person and verified
//! dashboard login. Callback receipt cannot exchange or persist a token.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use zeroize::Zeroize;

pub const TTL: Duration = Duration::from_secs(600);
const MAX_PENDING: usize = 128;
const REFUSED: &str =
    "這次 Google 連結已失效，或不是由目前登入的工作階段發起。請重新按「連結我的 Google」。";

pub struct Pending {
    pub home: PathBuf,
    pub user_id: String,
    pub session_ref: String,
    pub redirect_uri: String,
    pub dashboard_origin: String,
    pub paste: bool,
    pub code_verifier: String,
    pub client_id: String,
    pub client_secret: String,
    pub epoch: u64,
    pub google_data_consent: crate::google_data_consent::Evidence,
    pub created: Instant,
    pub code: Option<String>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.code_verifier.zeroize();
        self.client_secret.zeroize();
        if let Some(code) = &mut self.code {
            code.zeroize();
        }
    }
}

fn waiting() -> &'static Mutex<HashMap<String, Pending>> {
    static PENDING: OnceLock<Mutex<HashMap<String, Pending>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn insert(value: Pending) -> String {
    let state = hex::encode(rand::random::<[u8; 32]>());
    let mut map = waiting().lock().unwrap_or_else(|p| p.into_inner());
    map.retain(|_, p| p.created.elapsed() < TTL);
    if map.len() >= MAX_PENDING {
        if let Some(oldest) = map
            .iter()
            .min_by_key(|(_, p)| p.created)
            .map(|(state, _)| state.clone())
        {
            map.remove(&oldest);
        }
    }
    map.insert(state.clone(), value);
    state
}

fn valid_state(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Returns only the previously validated dashboard destination. Never a
/// credential or verifier; a repeated callback cannot swap in another code.
pub fn receive_code(home: &Path, state: &str, code: &str) -> Result<(String, bool), String> {
    if !valid_state(state)
        || code.is_empty()
        || code.len() > 4096
        || code.chars().any(char::is_control)
    {
        return Err(REFUSED.into());
    }
    let mut map = waiting().lock().unwrap_or_else(|p| p.into_inner());
    let pending = map.get_mut(state).ok_or(REFUSED)?;
    if pending.home != home || pending.created.elapsed() >= TTL || pending.code.is_some() {
        return Err(REFUSED.into());
    }
    pending.code = Some(code.into());
    Ok((pending.dashboard_origin.clone(), pending.paste))
}

/// Identity mismatches do not consume somebody else's valid state. Once
/// the bound person/session claims it, it is consumed before any exchange.
pub fn take(home: &Path, state: &str, user: &str, session: &str) -> Result<Pending, String> {
    if !valid_state(state) {
        return Err(REFUSED.into());
    }
    let mut map = waiting().lock().unwrap_or_else(|p| p.into_inner());
    let pending = map.get(state).ok_or(REFUSED)?;
    if pending.home != home
        || pending.user_id != user
        || pending.session_ref != session
        || pending.created.elapsed() >= TTL
        || pending.code.is_none()
    {
        return Err(REFUSED.into());
    }
    map.remove(state).ok_or_else(|| REFUSED.into())
}

pub fn cancel_user(home: &Path, user: &str) {
    waiting()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|_, p| p.home != home || p.user_id != user);
}

pub fn callback_values(query: &str) -> Result<(String, String), String> {
    if query.len() > 8192 {
        return Err(REFUSED.into());
    }
    let pairs: Vec<_> = url::form_urlencoded::parse(query.as_bytes()).collect();
    let one = |name: &str| -> Result<String, String> {
        let mut values = pairs.iter().filter(|(key, _)| key == name);
        let value = values
            .next()
            .map(|(_, value)| value.to_string())
            .ok_or(REFUSED)?;
        if values.next().is_some() {
            return Err(REFUSED.into());
        }
        Ok(value)
    };
    if pairs.iter().any(|(key, _)| key == "error") {
        return Err("這次 Google 授權沒有完成，請回到「我的連線」重新開始。".into());
    }
    let state = one("state")?;
    let code = one("code")?;
    if !valid_state(&state)
        || code.is_empty()
        || code.len() > 4096
        || code.chars().any(char::is_control)
    {
        return Err(REFUSED.into());
    }
    Ok((state, code))
}

pub fn receive_pasted(home: &Path, user: &str, session: &str, raw: &str) -> Result<String, String> {
    if raw.len() > 12 * 1024 {
        return Err(REFUSED.into());
    }
    let mut url = url::Url::parse(raw.trim()).map_err(|_| REFUSED)?;
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(REFUSED.into());
    }
    let (state, code) = callback_values(url.query().ok_or(REFUSED)?)?;
    url.set_query(None);
    let mut map = waiting().lock().unwrap_or_else(|p| p.into_inner());
    let pending = map.get_mut(&state).ok_or(REFUSED)?;
    if pending.home != home
        || pending.user_id != user
        || pending.session_ref != session
        || !pending.paste
        || pending.created.elapsed() >= TTL
        || pending.code.is_some()
        || url.as_str() != pending.redirect_uri
    {
        return Err(REFUSED.into());
    }
    pending.code = Some(code);
    Ok(state)
}

#[cfg(test)]
#[path = "pending_tests.rs"]
mod tests;
