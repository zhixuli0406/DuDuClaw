//! Is this Google Chat conversation a 1:1 DM between one person and the app?
//!
//! Chat commands that clear a conversation's history are limited in shared
//! conversations (`chat_commands::conversation_guard`), so the channel has to
//! know the space type. Resolution, in order:
//!
//! 1. The **trusted** space objects the turn already carries — on the HTTP
//!    path the JWT-verified interaction event (`message.space` and the
//!    event's `space`); on the Pub/Sub path the `spaces.messages.get`
//!    read-back (`googlechat_pubsub::authoritative_event` rebuilds the event
//!    from it; the unverified Pub/Sub payload's `space` is discarded by
//!    `notification_reference` and never consulted). See [`carried_kind`]:
//!    each object is classified **on its own** — fields are never combined
//!    across objects — a name that differs from the turn's space (or from
//!    the other object's name) makes the turn shared, any object that says
//!    "shared" makes it shared, and only an object that is a complete 1:1
//!    proof by itself makes it a 1:1 DM.
//! 2. When no carried object says (the Pub/Sub read-back usually carries
//!    only the space name), `spaces.get` with the app's own credentials,
//!    after validating the name with `google_workspace::chat_read::valid_space`.
//!    Definite answers are cached in memory per app + space (bounded, TTL —
//!    a space's type never changes).
//! 3. Still unknown (lookup failed, malformed, or the type is not stated) →
//!    shared. Fail-closed.
//!
//! "1:1 DM" means `spaceType == "DIRECT_MESSAGE"` (legacy `type == "DM"`)
//! **and** `singleUserBotDm == true`, both in the same object. A group DM
//! (`GROUP_CHAT`, several humans) and a named space (`SPACE`, legacy `ROOM`)
//! are shared.

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
/// `space` of an event / message), using only that object's own fields.
///
/// - `Shared` when any field says shared: `spaceType` `SPACE` /
///   `GROUP_CHAT`, legacy `type` `ROOM` / `SPACE` / `GROUP_CHAT`, or
///   `singleUserBotDm == false` — whichever field says it.
/// - `OneToOneDm` only when the object itself states a direct message
///   (`spaceType == "DIRECT_MESSAGE"`, or, without `spaceType`, legacy
///   `type == "DM"`) **and** `singleUserBotDm == true`.
/// - `Unknown` otherwise (not a proof either way).
pub(crate) fn classify_space(space: &Value) -> SpaceKind {
    let space_type = space.get("spaceType").filter(|v| !v.is_null());
    let legacy = space.get("type").and_then(Value::as_str);
    let single = space.get("singleUserBotDm").and_then(Value::as_bool);
    if matches!(space_type.and_then(Value::as_str), Some("SPACE" | "GROUP_CHAT"))
        || matches!(legacy, Some("ROOM" | "SPACE" | "GROUP_CHAT"))
        || single == Some(false)
    {
        return SpaceKind::Shared;
    }
    let direct = match space_type {
        Some(v) => v.as_str() == Some("DIRECT_MESSAGE"),
        // `SPACE_TYPE_UNSPECIFIED`, a value this build does not know, or a
        // non-string `spaceType` is not a proof (and the legacy field is
        // not consulted behind it).
        None => legacy == Some("DM"),
    };
    if direct && single == Some(true) {
        SpaceKind::OneToOneDm
    } else {
        SpaceKind::Unknown
    }
}

/// The per-object kinds of the space objects a turn carries
/// (`message.space`, then the event's `space`; absent or `null` ones are
/// skipped), or `None` when the objects cannot describe the turn's `space`:
/// an object that is not a JSON object, a non-string name, or a name that
/// is not exactly `space` (which also means two present names that differ).
fn carried_kinds(space: &str, message: &Value, event: &Value) -> Option<Vec<SpaceKind>> {
    let objects: Vec<&Value> = [message.get("space"), event.get("space")]
        .into_iter()
        .flatten()
        .filter(|v| !v.is_null())
        .collect();
    let mut names: Vec<&str> = Vec::new();
    for object in &objects {
        if !object.is_object() {
            return None;
        }
        if let Some(name) = object.get("name") {
            names.push(name.as_str()?);
        }
    }
    if names.windows(2).any(|pair| pair[0] != pair[1])
        || names.iter().any(|name| *name != space)
    {
        return None;
    }
    Some(objects.into_iter().map(classify_space).collect())
}

/// What the trusted space objects a turn carries say about `space`. Each
/// object is classified on its own ([`classify_space`]); fields are never
/// combined across objects.
///
/// - `Shared` when the objects cannot describe `space` (mismatched names,
///   malformed object — see `carried_kinds`) or any object says shared,
///   even if the other object is a complete 1:1 proof.
/// - `OneToOneDm` when at least one object is a complete 1:1 proof and no
///   object says shared.
/// - `Unknown` when every present object is silent: the `spaces.get` lookup
///   decides.
pub(crate) fn carried_kind(space: &str, message: &Value, event: &Value) -> SpaceKind {
    let Some(kinds) = carried_kinds(space, message, event) else {
        return SpaceKind::Shared;
    };
    if kinds.contains(&SpaceKind::Shared) {
        SpaceKind::Shared
    } else if kinds.contains(&SpaceKind::OneToOneDm) {
        SpaceKind::OneToOneDm
    } else {
        SpaceKind::Unknown
    }
}

/// Whether the space objects a turn carries contradict each other or the
/// turn's `space`: mismatched names, a malformed object, or one object that
/// is a complete 1:1 proof while another says shared. A decision press
/// (`CARD_CLICKED`) with such an event is refused outright.
pub(crate) fn carried_spaces_conflict(space: &str, message: &Value, event: &Value) -> bool {
    match carried_kinds(space, message, event) {
        None => true,
        Some(kinds) => {
            kinds.contains(&SpaceKind::Shared) && kinds.contains(&SpaceKind::OneToOneDm)
        }
    }
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

/// Steps 1–3 with the `spaces.get` call injected. `carried` is
/// [`carried_kind`] of the turn; `cache_key` must identify the app and the
/// space.
pub(crate) async fn resolve_one_to_one_with<F, Fut>(
    carried: SpaceKind,
    space: &str,
    cache_key: &str,
    fetch: F,
) -> bool
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Value, String>>,
{
    match carried {
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
/// 1:1 DM between one person and the app answering as `creds`. The single
/// source of the space kind for every Google Chat path (MESSAGE over HTTP
/// for the shared and employee-owned apps, the Pub/Sub re-read, CARD_CLICKED).
pub(crate) async fn is_one_to_one(
    creds: &GoogleChatCreds,
    space: &str,
    message: &Value,
    event: &Value,
) -> bool {
    let key = serde_json::json!([creds.client_email(), space]).to_string();
    resolve_one_to_one_with(carried_kind(space, message, event), space, &key, || {
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
        // Any field that says shared wins, whichever field it is.
        assert_eq!(
            classify_space(&json!({"spaceType":"DIRECT_MESSAGE","type":"ROOM","singleUserBotDm":true})),
            SpaceKind::Shared
        );
        assert_eq!(
            classify_space(&json!({"spaceType":"SPACE_TYPE_UNSPECIFIED","singleUserBotDm":false})),
            SpaceKind::Shared
        );
        // A non-string spaceType is not a proof, and the legacy field is not
        // consulted behind it.
        assert_eq!(
            classify_space(&json!({"spaceType":5,"type":"DM","singleUserBotDm":true})),
            SpaceKind::Unknown
        );
    }

    fn with_space(space: Value) -> Value {
        json!({ "space": space })
    }

    #[test]
    fn carried_objects_are_classified_separately_never_merged() {
        // The former field-by-field merge turned these two objects into a
        // 1:1 proof. Each is classified on its own now.
        let message = with_space(json!({"spaceType":"DIRECT_MESSAGE"}));
        let event = with_space(json!({"spaceType":"SPACE","singleUserBotDm":true}));
        assert_eq!(carried_kind("spaces/A", &message, &event), SpaceKind::Shared);
        assert_eq!(carried_kind("spaces/A", &event, &message), SpaceKind::Shared);
        assert_eq!(carried_kind("spaces/A", &json!({}), &json!({})), SpaceKind::Unknown);
        // One object complete, the other silent: the complete one decides.
        let silent = with_space(json!({"name":"spaces/DM1"}));
        assert_eq!(carried_kind("spaces/DM1", &silent, &with_space(dm())), SpaceKind::OneToOneDm);
        assert_eq!(carried_kind("spaces/DM1", &with_space(dm()), &silent), SpaceKind::OneToOneDm);
        assert_eq!(carried_kind("spaces/DM1", &with_space(dm()), &json!({})), SpaceKind::OneToOneDm);
        // Malformed objects are shared.
        assert_eq!(carried_kind("spaces/DM1", &with_space(json!("spaces/DM1")), &with_space(dm())), SpaceKind::Shared);
        assert_eq!(
            carried_kind("spaces/DM1", &with_space(json!({"name":7})), &with_space(dm())),
            SpaceKind::Shared
        );
    }

    #[test]
    fn type_from_one_object_and_single_user_from_the_other_is_not_a_proof() {
        let type_only = with_space(json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE"}));
        let flag_only = with_space(json!({"name":"spaces/DM1","singleUserBotDm":true}));
        assert_eq!(carried_kind("spaces/DM1", &type_only, &flag_only), SpaceKind::Unknown);
        assert_eq!(carried_kind("spaces/DM1", &flag_only, &type_only), SpaceKind::Unknown);
        let legacy_only = with_space(json!({"name":"spaces/DM1","type":"DM"}));
        assert_eq!(carried_kind("spaces/DM1", &legacy_only, &flag_only), SpaceKind::Unknown);
        assert!(!carried_spaces_conflict("spaces/DM1", &type_only, &flag_only));
    }

    #[tokio::test]
    async fn split_proof_falls_to_the_lookup_or_shared() {
        let type_only = with_space(json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE"}));
        let flag_only = with_space(json!({"name":"spaces/DM1","singleUserBotDm":true}));
        let carried = carried_kind("spaces/DM1", &type_only, &flag_only);
        let group = json!({"name":"spaces/DM1","spaceType":"GROUP_CHAT"});
        assert_eq!(resolve_kind(carried, "spaces/DM1", Ok(group)).await, (false, 1));
        assert_eq!(resolve_kind(carried, "spaces/DM1", Err("403".into())).await, (false, 1));
        // Only the authoritative lookup can make it a 1:1 DM.
        assert_eq!(resolve_kind(carried, "spaces/DM1", Ok(dm())).await, (true, 1));
    }

    #[tokio::test]
    async fn explicit_shared_in_either_object_beats_a_complete_proof() {
        for shared in [
            json!({"name":"spaces/DM1","spaceType":"SPACE"}),
            json!({"name":"spaces/DM1","spaceType":"GROUP_CHAT"}),
            json!({"name":"spaces/DM1","type":"ROOM"}),
            json!({"name":"spaces/DM1","singleUserBotDm":false}),
            json!({"name":"spaces/DM1","spaceType":"DIRECT_MESSAGE","singleUserBotDm":false}),
        ] {
            let shared = with_space(shared);
            let proof = with_space(dm());
            for (message, event) in [(&shared, &proof), (&proof, &shared)] {
                let kind = carried_kind("spaces/DM1", message, event);
                assert_eq!(kind, SpaceKind::Shared, "{message} {event}");
                assert!(carried_spaces_conflict("spaces/DM1", message, event), "{message} {event}");
                // Decided without a lookup, even one that would say DM.
                assert_eq!(resolve_kind(kind, "spaces/DM1", Ok(dm())).await, (false, 0));
            }
        }
    }

    #[tokio::test]
    async fn a_message_with_mismatched_space_names_gets_no_attestation() {
        // Both objects are complete 1:1 proofs, but they name different
        // spaces: shared, no lookup, so `handle_message` never builds a
        // `OneToOneAttestation` (it does so only when `is_one_to_one`).
        let message = with_space(dm());
        let event = with_space(json!({"name":"spaces/OTHER","spaceType":"DIRECT_MESSAGE","singleUserBotDm":true}));
        for space in ["spaces/DM1", "spaces/OTHER"] {
            for (m, e) in [(&message, &event), (&event, &message)] {
                let kind = carried_kind(space, m, e);
                assert_eq!(kind, SpaceKind::Shared);
                assert!(carried_spaces_conflict(space, m, e));
                assert_eq!(resolve_kind(kind, space, Ok(dm())).await, (false, 0));
            }
        }
        // A carried name other than the turn's space is shared too.
        assert_eq!(carried_kind("spaces/DM2", &message, &json!({})), SpaceKind::Shared);
        // Equal names agree.
        assert_eq!(carried_kind("spaces/DM1", &message, &message), SpaceKind::OneToOneDm);
        assert!(!carried_spaces_conflict("spaces/DM1", &message, &message));
    }

    async fn resolve_kind(carried: SpaceKind, space: &str, lookup: Result<Value, String>) -> (bool, usize) {
        let calls = AtomicUsize::new(0);
        let k = key();
        let got = resolve_one_to_one_with(carried, space, &k, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { lookup }
        })
        .await;
        (got, calls.load(Ordering::SeqCst))
    }

    /// `carried` as the turn's `message.space`, no event-level space.
    async fn resolve(carried: Value, space: &str, lookup: Result<Value, String>) -> (bool, usize) {
        let kind = carried_kind(space, &with_space(carried), &json!({}));
        resolve_kind(kind, space, lookup).await
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
        let carried = json!({"name":"spaces/DM1"});
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
        let unknown = SpaceKind::Unknown;
        assert!(!resolve_one_to_one_with(unknown, "spaces/DM1", &k, || fetch(Err("timeout".into()))).await);
        assert!(resolve_one_to_one_with(unknown, "spaces/DM1", &k, || fetch(Ok(dm()))).await);
        // Cached: the next resolution makes no call even if it would fail.
        assert!(resolve_one_to_one_with(unknown, "spaces/DM1", &k, || fetch(Err("x".into()))).await);
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
