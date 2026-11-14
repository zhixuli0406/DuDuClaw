//! Is this Google Chat conversation a 1:1 DM between one person and the app?
//!
//! Chat commands that clear a conversation's history are limited in shared
//! conversations (`chat_commands::conversation_guard`), so the channel has to
//! know the space type. Resolution, in order:
//!
//! 1. The **trusted** space object the turn already carries — on the HTTP
//!    path the JWT-verified interaction event (`message.space`, then the
//!    event's `space`, field by field); on the Pub/Sub path the
//!    `spaces.messages.get` read-back (`googlechat_pubsub::authoritative_event`
//!    rebuilds the event from it; the unverified Pub/Sub payload's `space`
//!    is discarded by `notification_reference` and never consulted).
//! 2. When that object does not say (the Pub/Sub read-back usually carries
//!    only the space name), `spaces.get` with the app's own credentials,
//!    after validating the name with `google_workspace::chat_read::valid_space`.
//!    Definite answers are cached in memory per app + space (bounded, TTL —
//!    a space's type never changes).
//! 3. Still unknown (lookup failed, malformed, or the type is not stated) →
//!    shared. Fail-closed.
//!
//! "1:1 DM" means `spaceType == "DIRECT_MESSAGE"` (legacy `type == "DM"`)
//! **and** `singleUserBotDm == true`. A group DM (`GROUP_CHAT`, several
//! humans) and a named space (`SPACE`, legacy `ROOM`) are shared.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::googlechat::GoogleChatCreds;
use crate::isolation_http::LiveRequest;

/// What a space object says about who shares the conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpaceKind {
    /// A DM between one human and this app.
    OneToOneDm,
    /// A named space, a group DM, or a DM explicitly not with a single user.
    Shared,
    /// The object does not say.
    Unknown,
}

/// Classify one Chat `Space` object (as returned by `spaces.get`, or the
/// `space` of an event / message).
pub(crate) fn classify_space(space: &Value) -> SpaceKind {
    let direct = match space.get("spaceType").and_then(Value::as_str) {
        Some("DIRECT_MESSAGE") => true,
        Some("SPACE" | "GROUP_CHAT") => false,
        // `SPACE_TYPE_UNSPECIFIED` or a value this build does not know.
        Some(_) => return SpaceKind::Unknown,
        None => match space.get("type").and_then(Value::as_str) {
            Some("DM") => true,
            Some("ROOM" | "SPACE" | "GROUP_CHAT") => false,
            _ => return SpaceKind::Unknown,
        },
    };
    if !direct {
        return SpaceKind::Shared;
    }
    match space.get("singleUserBotDm").and_then(Value::as_bool) {
        Some(true) => SpaceKind::OneToOneDm,
        Some(false) => SpaceKind::Shared,
        None => SpaceKind::Unknown,
    }
}

/// The space fields a turn carries: each of `spaceType`, `type`,
/// `singleUserBotDm` from `message.space`, else from the event's `space`.
pub(crate) fn carried_space(message: &Value, event: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for key in ["spaceType", "type", "singleUserBotDm"] {
        let path = format!("/space/{key}");
        if let Some(v) = message.pointer(&path).or_else(|| event.pointer(&path)) {
            out.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(out)
}

/// The kind a successful `spaces.get` body states for `space`. A body that
/// names another space is not evidence (`Unknown`).
pub(crate) fn kind_from_lookup(space: &str, body: &Value) -> SpaceKind {
    if body.get("name").and_then(Value::as_str) != Some(space) {
        return SpaceKind::Unknown;
    }
    classify_space(body)
}

// ── Cache ───────────────────────────────────────────────────────

const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
const CACHE_CAP: usize = 4096;

fn cache() -> &'static Mutex<HashMap<String, (SpaceKind, Instant)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (SpaceKind, Instant)>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

fn cache_get(key: &str) -> Option<SpaceKind> {
    let map = cache().lock().unwrap_or_else(|e| e.into_inner());
    map.get(key).filter(|(_, at)| at.elapsed() < CACHE_TTL).map(|(kind, _)| *kind)
}

/// Only definite answers are cached; a failed or inconclusive lookup is
/// retried next time (and refuses this time).
fn cache_put(key: &str, kind: SpaceKind) {
    if kind == SpaceKind::Unknown {
        return;
    }
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    bounded_insert(&mut map, key, kind, CACHE_CAP);
}

/// Insert keeping at most `cap` entries: expired entries go first, then the
/// oldest.
fn bounded_insert(
    map: &mut HashMap<String, (SpaceKind, Instant)>,
    key: &str,
    kind: SpaceKind,
    cap: usize,
) {
    if map.len() >= cap && !map.contains_key(key) {
        map.retain(|_, (_, at)| at.elapsed() < CACHE_TTL);
        if map.len() >= cap {
            if let Some(oldest) = map.iter().min_by_key(|(_, (_, at))| *at).map(|(k, _)| k.clone()) {
                map.remove(&oldest);
            }
        }
    }
    map.insert(key.to_string(), (kind, Instant::now()));
}

// ── Resolution ──────────────────────────────────────────────────

/// Steps 1–3 with the `spaces.get` call injected. `cache_key` must identify
/// the app and the space.
pub(crate) async fn resolve_one_to_one_with<F, Fut>(
    carried: &Value,
    space: &str,
    cache_key: &str,
    fetch: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    match classify_space(carried) {
        SpaceKind::OneToOneDm => return true,
        SpaceKind::Shared => return false,
        SpaceKind::Unknown => {}
    }
    if !crate::google_workspace::chat_read::valid_space(space) {
        return false;
    }
    if let Some(kind) = cache_get(cache_key) {
        return kind == SpaceKind::OneToOneDm;
    }
    let kind = match fetch().await {
        Ok(body) => kind_from_lookup(space, &body),
        Err(reason) => {
            tracing::warn!(reason, "Google Chat spaces.get failed; treating the space as shared");
            SpaceKind::Unknown
        }
    };
    cache_put(cache_key, kind);
    kind == SpaceKind::OneToOneDm
}

/// `spaces.get` as the app. The caller validated `space`.
async fn fetch_space(creds: &GoogleChatCreds, space: &str) -> Result<Value, String> {
    if !crate::google_workspace::chat_read::valid_space(space) {
        return Err("invalid space name".into());
    }
    let token = creds.token().await.map_err(|_| "token unavailable".to_string())?;
    let mut response = creds
        .http
        .get(format!("{}/{space}", creds.api_base()))
        .bearer_auth(token)
        .send_authorized()
        .await?;
    if !response.status().is_success() {
        return Err(format!("status {}", response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| "read failed".to_string())? {
        if bytes.len() + chunk.len() > 256 * 1024 {
            return Err("response too large".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| "malformed response".to_string())
}

/// Whether the turn in `space` (with its trusted `message` / `event`) is a
/// 1:1 DM between one person and the app answering as `creds`.
pub(crate) async fn is_one_to_one(
    creds: &GoogleChatCreds,
    space: &str,
    message: &Value,
    event: &Value,
) -> bool {
    let key = serde_json::json!([creds.client_email(), space]).to_string();
    resolve_one_to_one_with(&carried_space(message, event), space, &key, || {
        fetch_space(creds, space)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Interaction-event fixtures as Google Chat sends them.
    fn dm() -> Value {
        json!({"name":"spaces/DM1","type":"DM","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true})
    }
    fn group_dm() -> Value {
        json!({"name":"spaces/GRP1","type":"ROOM","spaceType":"GROUP_CHAT"})
    }
    fn named_space() -> Value {
        json!({"name":"spaces/SP1","type":"ROOM","spaceType":"SPACE","displayName":"Sales"})
    }
    fn missing_type() -> Value {
        json!({"name":"spaces/X1"})
    }

    fn key() -> String {
        format!("test:{}", uuid::Uuid::new_v4())
    }

    #[test]
    fn classifies_fixture_spaces() {
        assert_eq!(classify_space(&dm()), SpaceKind::OneToOneDm);
        assert_eq!(classify_space(&group_dm()), SpaceKind::Shared);
        assert_eq!(classify_space(&named_space()), SpaceKind::Shared);
        assert_eq!(classify_space(&missing_type()), SpaceKind::Unknown);
        // Legacy field only.
        assert_eq!(classify_space(&json!({"type":"DM","singleUserBotDm":true})), SpaceKind::OneToOneDm);
        assert_eq!(classify_space(&json!({"type":"ROOM"})), SpaceKind::Shared);
        // A DM that is not stated to be with a single user is not proof.
        assert_eq!(classify_space(&json!({"spaceType":"DIRECT_MESSAGE"})), SpaceKind::Unknown);
        assert_eq!(classify_space(&json!({"type":"DM"})), SpaceKind::Unknown);
        assert_eq!(
            classify_space(&json!({"spaceType":"DIRECT_MESSAGE","singleUserBotDm":false})),
            SpaceKind::Shared
        );
        // spaceType wins over the legacy field.
        assert_eq!(
            classify_space(&json!({"spaceType":"SPACE","type":"DM","singleUserBotDm":true})),
            SpaceKind::Shared
        );
        assert_eq!(
            classify_space(&json!({"spaceType":"SPACE_TYPE_UNSPECIFIED"})),
            SpaceKind::Unknown
        );
        assert_eq!(classify_space(&json!({"singleUserBotDm":true})), SpaceKind::Unknown);
    }

    #[test]
    fn carried_space_merges_message_then_event() {
        let message = json!({"space":{"spaceType":"DIRECT_MESSAGE"}});
        let event = json!({"space":{"spaceType":"SPACE","singleUserBotDm":true}});
        let merged = carried_space(&message, &event);
        assert_eq!(merged["spaceType"], "DIRECT_MESSAGE");
        assert_eq!(classify_space(&merged), SpaceKind::OneToOneDm);
        assert_eq!(carried_space(&json!({}), &json!({})), json!({}));
    }

    async fn resolve(carried: Value, space: &str, lookup: Result<Value, String>) -> (bool, usize) {
        let calls = AtomicUsize::new(0);
        let k = key();
        let got = resolve_one_to_one_with(&carried, space, &k, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { lookup }
        })
        .await;
        (got, calls.load(Ordering::SeqCst))
    }

    #[tokio::test]
    async fn http_event_with_type_needs_no_lookup() {
        // HTTP interaction events carry the type: decided without a call.
        assert_eq!(resolve(dm(), "spaces/DM1", Err("unused".into())).await, (true, 0));
        assert_eq!(resolve(group_dm(), "spaces/GRP1", Ok(dm())).await, (false, 0));
        assert_eq!(resolve(named_space(), "spaces/SP1", Ok(dm())).await, (false, 0));
    }

    #[tokio::test]
    async fn pubsub_readback_without_type_uses_spaces_get() {
        // The spaces.messages.get read-back carries only the space name.
        let carried = carried_space(&json!({"space":{"name":"spaces/DM1"}}), &json!({}));
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Ok(dm())).await, (true, 1));
        let group = json!({"name":"spaces/DM1","spaceType":"GROUP_CHAT"});
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Ok(group)).await, (false, 1));
        let space = json!({"name":"spaces/DM1","spaceType":"SPACE"});
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Ok(space)).await, (false, 1));
        // Missing type after the lookup → shared.
        let bare = json!({"name":"spaces/DM1"});
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Ok(bare)).await, (false, 1));
        // A DM without singleUserBotDm → shared.
        let dm_unsure = json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE"});
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Ok(dm_unsure)).await, (false, 1));
        // Lookup error → shared.
        assert_eq!(resolve(carried.clone(), "spaces/DM1", Err("403".into())).await, (false, 1));
        // A body for another space is not evidence.
        let other = json!({"name":"spaces/OTHER","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true});
        assert_eq!(resolve(carried, "spaces/DM1", Ok(other)).await, (false, 1));
    }

    #[tokio::test]
    async fn invalid_space_name_is_never_looked_up() {
        for space in ["spaces/../x", "spaces/a/messages/b", "spaces/a?x=1", "", "users/a"] {
            assert_eq!(resolve(json!({}), space, Ok(dm())).await, (false, 0), "{space}");
        }
    }

    #[tokio::test]
    async fn definite_answers_are_cached_failures_are_not() {
        let k = key();
        let calls = AtomicUsize::new(0);
        let fetch = |body: Result<Value, String>| {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { body }
        };
        assert!(!resolve_one_to_one_with(&json!({}), "spaces/DM1", &k, || fetch(Err("timeout".into()))).await);
        assert!(resolve_one_to_one_with(&json!({}), "spaces/DM1", &k, || fetch(Ok(dm()))).await);
        // Cached: the next resolution makes no call even if it would fail.
        assert!(resolve_one_to_one_with(&json!({}), "spaces/DM1", &k, || fetch(Err("x".into()))).await);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cache_is_bounded() {
        // A local map: filling the process-wide cache would evict entries
        // other tests running in parallel rely on.
        let mut map = HashMap::new();
        for i in 0..20 {
            bounded_insert(&mut map, &format!("k{i}"), SpaceKind::Shared, 8);
        }
        assert_eq!(map.len(), 8);
        assert!(map.contains_key("k19"));
        // Unknown is never cached.
        let k = key();
        cache_put(&k, SpaceKind::Unknown);
        assert_eq!(cache_get(&k), None);
    }
}
