//! Subscribe, list, unsubscribe, rotate and refresh MCP Events
//! subscriptions (behind the admin RPCs `mcp.events_*`).
//!
//! The callback URL is `<[mcp_events] public_base_url>/webhook/mcp-events/<id>`.
//! The upstream refuses non-`https` callbacks and validates them against
//! public addresses, so `public_base_url` must be an https address the
//! upstream can reach (a reverse proxy or tunnel in front of the gateway);
//! plain `http` is accepted only for a loopback host (local testing).

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Value, json};

use super::store::{self, DeliveryKind, DeliveryStatus, EventMode, EventSubscription, SubSecrets, SubStatus, UpstreamSub};
use super::upstream::{self, Session, UpstreamError};

/// Suggested subscription lifetime (`ttlMs`): one day, the draft's upper
/// recommendation; the upstream's `refreshBefore` is authoritative.
pub const TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Refresh when `refreshBefore` is closer than this.
pub const REFRESH_MARGIN_SECS: i64 = 2 * 60 * 60;
/// How long the previous secret is still accepted after a rotation.
pub const ROTATION_GRACE_SECS: i64 = 15 * 60;

/// One event name of a subscription as the dashboard sees it (the cursor
/// itself and the de-duplication ids stay in the store).
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamView {
    pub name: String,
    pub upstream_id: Option<String>,
    pub refresh_before: Option<String>,
    pub last_error: Option<String>,
    /// A replay position is held (the event type supports replay).
    pub has_cursor: bool,
    /// The last answer or a `gap` said events were skipped.
    pub truncated: bool,
    pub gap_at: Option<String>,
    pub delivery_status: Option<DeliveryStatus>,
    pub last_polled_at: Option<String>,
    pub arguments: Value,
}

/// A subscription as the dashboard sees it (no secret).
#[derive(Debug, Clone, Serialize)]
pub struct SubscriptionView {
    pub id: String,
    pub agent_id: String,
    pub server: String,
    pub event_types: Vec<String>,
    pub mode: &'static str,
    pub delivery: &'static str,
    pub status: &'static str,
    pub callback_url: Option<String>,
    pub upstream: Vec<UpstreamView>,
    pub created_at: String,
    pub updated_at: String,
    pub rotated_at: Option<String>,
    pub last_delivery_at: Option<String>,
    pub deliveries: u64,
}

fn view(home: &Path, rec: &EventSubscription) -> SubscriptionView {
    SubscriptionView {
        id: rec.id.clone(),
        agent_id: rec.agent_id.clone(),
        server: rec.server.clone(),
        event_types: rec.event_types.clone(),
        mode: rec.mode.as_str(),
        delivery: rec.delivery.as_str(),
        status: rec.status.as_str(),
        callback_url: if rec.delivery == DeliveryKind::Webhook {
            public_base_url(home).ok().map(|b| callback_url(&b, &rec.id))
        } else {
            None
        },
        upstream: rec
            .upstream
            .iter()
            .map(|u| UpstreamView {
                name: u.name.clone(),
                upstream_id: u.upstream_id.clone(),
                refresh_before: u.refresh_before.clone(),
                last_error: u.last_error.clone(),
                has_cursor: u.cursor.is_some(),
                truncated: u.truncated,
                gap_at: u.gap_at.clone(),
                delivery_status: u.delivery_status.clone(),
                last_polled_at: u.last_polled_at.clone(),
                arguments: rec.arguments.get(&u.name).cloned().unwrap_or_else(|| json!({})),
            })
            .collect(),
        created_at: rec.created_at.clone(),
        updated_at: rec.updated_at.clone(),
        rotated_at: rec.rotated_at.clone(),
        last_delivery_at: rec.last_delivery_at.clone(),
        deliveries: rec.deliveries,
    }
}

/// `[mcp_events] max_age_ms` (default 5 minutes, 1 s – 7 days): how far back
/// a replay may reach when a cursor is sent (the draft's `maxAgeMs`).
pub fn max_age_ms(home: &Path) -> u64 {
    config_u64(home, "max_age_ms").unwrap_or(300_000).clamp(1_000, 7 * 86_400_000)
}

/// `[mcp_events] poll_floor_ms` (default 5 s, 1 s – 1 h): the shortest wait
/// between polls of one event name, whatever `nextPollMs` says.
pub fn poll_floor_ms(home: &Path) -> u64 {
    config_u64(home, "poll_floor_ms").unwrap_or(5_000).clamp(1_000, 3_600_000)
}

fn config_u64(home: &Path, key: &str) -> Option<u64> {
    let text = std::fs::read_to_string(home.join("config.toml")).ok()?;
    let doc: toml::Value = toml::from_str(&text).ok()?;
    doc.get("mcp_events")?.get(key)?.as_integer().and_then(|n| u64::try_from(n).ok())
}

/// `config.toml [mcp_events] public_base_url`, validated.
pub fn public_base_url(home: &Path) -> Result<String, String> {
    let missing = || {
        "set config.toml [mcp_events] public_base_url to the https address the MCP server can \
         reach this gateway at (for example a reverse proxy in front of it)"
            .to_string()
    };
    let text = std::fs::read_to_string(home.join("config.toml")).map_err(|_| missing())?;
    let doc: toml::Value = toml::from_str(&text).map_err(|_| "config.toml cannot be parsed".to_string())?;
    let raw = doc
        .get("mcp_events")
        .and_then(|t| t.get("public_base_url"))
        .and_then(|v| v.as_str())
        .ok_or_else(missing)?;
    validate_base_url(raw)
}

/// https only (http only for a loopback host), no userinfo, query or
/// fragment; returned without a trailing `/`.
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let url = url::Url::parse(raw.trim()).map_err(|_| "public_base_url is not a URL".to_string())?;
    let loopback = url
        .host()
        .is_some_and(|h| crate::remote_mcp::url_policy::is_loopback_host(&h));
    match url.scheme() {
        "https" => {}
        "http" if loopback => {}
        _ => return Err("public_base_url must be https (http only for a loopback address)".into()),
    }
    if !url.username().is_empty() || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
        return Err("public_base_url must not carry a user name, query or fragment".into());
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub fn callback_url(base: &str, id: &str) -> String {
    format!("{base}/webhook/mcp-events/{id}")
}

/// How the caller wants events delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryChoice {
    Webhook,
    Poll,
    /// The draft's client preference: webhook when `public_base_url` is set
    /// and every event type lists it, else poll when every type lists it.
    Auto,
}

/// Options of [`subscribe_with`].
#[derive(Debug, Clone)]
pub struct SubscribeOptions {
    pub delivery: DeliveryChoice,
    /// Subscription arguments per event name (`{}` when absent).
    pub arguments: BTreeMap<String, Value>,
}

impl Default for SubscribeOptions {
    fn default() -> Self {
        Self { delivery: DeliveryChoice::Webhook, arguments: BTreeMap::new() }
    }
}

/// Most poll-mode subscriptions per gateway.
pub const MAX_POLL_SUBSCRIPTIONS: usize = 20;

/// Create a webhook subscription (no arguments) and subscribe upstream.
pub async fn subscribe(
    home: &Path,
    agent_id: &str,
    server: &str,
    event_types: &[String],
    mode: EventMode,
) -> Result<SubscriptionView, String> {
    subscribe_with(home, agent_id, server, event_types, mode, SubscribeOptions::default()).await
}

/// `events/list` of a connected remote server (for the subscribe UI).
pub async fn discover(home: &Path, agent_id: &str, server: &str) -> Result<Vec<upstream::EventDescriptor>, String> {
    crate::remote_mcp::store::validate_ids(agent_id, server)?;
    match crate::remote_mcp::store::get(home, agent_id, server)? {
        Some(r) if r.status == crate::remote_mcp::store::ConnStatus::Connected => {}
        _ => return Err(format!("remote server '{server}' is not connected")),
    }
    let mut session = Session::open(home, agent_id, server).await.map_err(|e| e.to_string())?;
    let res = upstream::list_events(&mut session).await;
    session.close().await;
    res.map_err(|e| e.to_string())
}

/// Create a subscription and subscribe (webhook) or bootstrap (poll)
/// upstream.
pub async fn subscribe_with(
    home: &Path,
    agent_id: &str,
    server: &str,
    event_types: &[String],
    mode: EventMode,
    opts: SubscribeOptions,
) -> Result<SubscriptionView, String> {
    crate::remote_mcp::store::validate_ids(agent_id, server)?;
    match crate::remote_mcp::store::get(home, agent_id, server)? {
        Some(r) if r.status == crate::remote_mcp::store::ConnStatus::Connected => {}
        Some(_) => return Err(format!("remote server '{server}' is not connected; connect it first")),
        None => {
            return Err(format!(
                "'{server}' is not a remote MCP server of '{agent_id}' (only remote servers can deliver events)"
            ));
        }
    }
    let mut names: Vec<String> = Vec::new();
    for t in event_types {
        let t = t.trim();
        if !store::is_valid_event_name(t) {
            return Err(format!("invalid event type '{}'", duduclaw_core::truncate_chars(t, 64)));
        }
        if !names.iter().any(|n| n == t) {
            names.push(t.to_string());
        }
    }
    if names.is_empty() || names.len() > store::MAX_EVENT_TYPES {
        return Err(format!("name 1–{} event types", store::MAX_EVENT_TYPES));
    }
    for (k, v) in &opts.arguments {
        if !names.contains(k) {
            return Err(format!("arguments given for '{}', which is not a subscribed event type", duduclaw_core::truncate_chars(k, 64)));
        }
        store::validate_arguments(v)?;
    }
    // Choose the delivery mode. The server's own `events/list` decides when
    // it answers; when it does not (or the call fails) a webhook/poll choice
    // is tried as asked and the server's refusal is shown.
    let base = public_base_url(home);
    let advertised = match discover(home, agent_id, server).await {
        Ok(d) => Some(d),
        Err(_) => None,
    };
    // `strict`: every name must be listed with the mode (auto choice);
    // lenient: only a listed name that lacks the mode is a refusal (a server
    // whose list is empty or incomplete is asked as the operator chose).
    let lists = |mode: &str, strict: bool| -> bool {
        match &advertised {
            Some(d) => names.iter().all(|n| match d.iter().find(|e| &e.name == n) {
                Some(e) => e.delivery.iter().any(|m| m == mode),
                None => !strict,
            }),
            None => !strict,
        }
    };
    let delivery = match opts.delivery {
        DeliveryChoice::Webhook => DeliveryKind::Webhook,
        DeliveryChoice::Poll => DeliveryKind::Poll,
        DeliveryChoice::Auto => {
            if advertised.is_none() {
                return Err("could not read the server's event list; choose webhook or poll".into());
            }
            if base.is_ok() && lists("webhook", true) {
                DeliveryKind::Webhook
            } else if lists("poll", true) {
                DeliveryKind::Poll
            } else {
                return Err("no delivery mode this gateway can use is offered for these event types".into());
            }
        }
    };
    if !lists(delivery.as_str(), false) {
        return Err(format!(
            "the server does not offer {} delivery for every one of these event types",
            delivery.as_str()
        ));
    }
    if delivery == DeliveryKind::Poll
        && store::load_all(home)?.iter().filter(|r| r.delivery == DeliveryKind::Poll).count() >= MAX_POLL_SUBSCRIPTIONS
    {
        return Err(format!("at most {MAX_POLL_SUBSCRIPTIONS} poll-mode event subscriptions"));
    }
    let base = if delivery == DeliveryKind::Webhook { Some(base?) } else { None };
    let secret = super::signature::generate_secret();
    let now = store::now_rfc3339();
    let rec = EventSubscription {
        id: store::new_id(),
        agent_id: agent_id.to_string(),
        server: server.to_string(),
        event_types: names.clone(),
        mode,
        delivery,
        arguments: opts.arguments.clone(),
        status: SubStatus::Pending,
        upstream: names.iter().map(|n| UpstreamSub { name: n.clone(), ..Default::default() }).collect(),
        created_at: now.clone(),
        updated_at: now,
        rotated_at: None,
        last_delivery_at: None,
        deliveries: 0,
        secret_enc: store::seal(home, &SubSecrets { current: secret.clone(), previous: None, previous_until: None })?,
    };
    // Stored before the upstream is asked, so its verification challenge
    // (sent while it handles `events/subscribe`) finds the subscription.
    store::insert(home, rec.clone())?;
    super::audit(
        home,
        super::AUDIT_CREATED,
        agent_id,
        json!({ "subscription": rec.id, "server": server, "event_types": names, "mode": mode.as_str(), "delivery": delivery.as_str() }),
    );
    let result = match &base {
        Some(base) => subscribe_all(home, &rec, &callback_url(base, &rec.id), &secret).await,
        None => super::poll::bootstrap(home, &rec).await,
    };
    match result {
        Err(UpstreamError::NoEventsCapability) => {
            let _ = store::remove(home, &rec.id);
            super::audit(home, super::AUDIT_REVOKED, agent_id, json!({ "subscription": rec.id, "server": server, "reason": "no_events_capability" }));
            Err(format!("remote server '{server}' does not offer MCP Events"))
        }
        _ => get_view(home, &rec.id),
    }
}

fn get_view(home: &Path, id: &str) -> Result<SubscriptionView, String> {
    store::get(home, id)?
        .map(|r| view(home, &r))
        .ok_or_else(|| "subscription not found".to_string())
}

/// Call `events/subscribe` for every event name of `rec` (a webhook
/// subscription; also the refresh: the stored cursor is sent so the server
/// resumes from it, bounded by `maxAgeMs`) and record the outcome. Returns
/// the session-level error, if the session could not open.
async fn subscribe_all(home: &Path, rec: &EventSubscription, url: &str, secret: &str) -> Result<(), UpstreamError> {
    // Cursors and de-duplication ids live in the store; start from its copy.
    let current = store::get(home, &rec.id).ok().flatten().unwrap_or_else(|| rec.clone());
    let mut session = match Session::open(home, &rec.agent_id, &rec.server).await {
        Ok(s) => s,
        Err(e) => {
            let msg = e.to_string();
            let _ = store::update(home, &rec.id, |r| {
                r.status = SubStatus::Failed;
                for u in r.upstream.iter_mut() {
                    u.last_error = Some(msg.clone());
                }
                Ok(true)
            });
            super::audit(home, super::AUDIT_REFRESH_FAILED, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server, "error": duduclaw_core::truncate_chars(&msg, 200) }));
            return Err(e);
        }
    };
    let max_age = max_age_ms(home);
    let mut results: Vec<UpstreamSub> = Vec::new();
    for name in &rec.event_types {
        let mut u = current
            .upstream
            .iter()
            .find(|u| &u.name == name)
            .cloned()
            .unwrap_or_else(|| UpstreamSub { name: name.clone(), ..Default::default() });
        let args = rec.arguments.get(name).cloned().unwrap_or_else(|| json!({}));
        match upstream::subscribe(&mut session, name, &args, url, secret, u.cursor.as_deref(), Some(max_age), TTL_MS).await {
            Ok(r) => {
                u.upstream_id = r.id;
                u.refresh_before = r.refresh_before;
                u.last_error = None;
                if r.cursor.is_some() || u.cursor.is_none() {
                    u.cursor = r.cursor;
                }
                if r.truncated {
                    // Delivery restarted later than the cursor we sent.
                    u.truncated = true;
                    u.gap_at = Some(store::now_rfc3339());
                }
                if let Some(ds) = r.delivery_status {
                    if !ds.active || ds.last_error.is_some() {
                        super::audit(
                            home,
                            super::AUDIT_REFRESH_FAILED,
                            &rec.agent_id,
                            json!({ "subscription": rec.id, "server": rec.server, "name": name, "delivery_active": ds.active, "delivery_error": ds.last_error }),
                        );
                    }
                    u.delivery_status = Some(ds);
                }
            }
            Err(e) => u.last_error = Some(duduclaw_core::truncate_chars(&e.to_string(), 200).to_string()),
        }
        results.push(u);
    }
    session.close().await;
    let all_ok = results.iter().all(|u| u.last_error.is_none());
    let _ = store::update(home, &rec.id, |r| {
        if r.status != SubStatus::Terminated {
            r.status = if all_ok { SubStatus::Active } else { SubStatus::Failed };
        }
        // Keep the ids and the cursor an event or gap moved while we waited.
        for u in results.iter_mut() {
            if let Some(old) = r.upstream.iter().find(|o| o.name == u.name) {
                u.recent_ids = old.recent_ids.clone();
                if old.cursor != current.upstream.iter().find(|c| c.name == u.name).and_then(|c| c.cursor.clone()) {
                    u.cursor = old.cursor.clone();
                }
                if u.gap_cursor.is_none() {
                    u.gap_cursor = old.gap_cursor.clone();
                }
            }
        }
        r.upstream = results.clone();
        Ok(true)
    });
    if !all_ok {
        super::audit(home, super::AUDIT_REFRESH_FAILED, &rec.agent_id, json!({ "subscription": rec.id, "server": rec.server }));
    }
    Ok(())
}

/// Every subscription (optionally of one employee), no secrets.
pub fn list(home: &Path, agent_id: Option<&str>) -> Result<Vec<SubscriptionView>, String> {
    Ok(store::load_all(home)?
        .iter()
        .filter(|r| agent_id.is_none_or(|a| r.agent_id == a))
        .map(|r| view(home, r))
        .collect())
}

/// Unsubscribe upstream (best effort) and delete the local subscription;
/// its callback URL answers 404 afterwards. Returns whether the upstream
/// acknowledged every unsubscribe.
pub async fn unsubscribe(home: &Path, id: &str) -> Result<bool, String> {
    if !store::is_valid_id(id) {
        return Err("invalid subscription id".into());
    }
    let rec = store::get(home, id)?.ok_or_else(|| "subscription not found".to_string())?;
    // Local delete first: deliveries stop being accepted at once, whatever
    // the upstream does.
    store::remove(home, id)?;
    let mut upstream_ok = false;
    if rec.delivery == DeliveryKind::Poll {
        // The server holds no subscription state in poll mode.
        upstream_ok = true;
    } else if let Ok(base) = public_base_url(home)
        && let Ok(mut s) = Session::open(home, &rec.agent_id, &rec.server).await
    {
        let url = callback_url(&base, id);
        upstream_ok = true;
        for name in &rec.event_types {
            let args = rec.arguments.get(name).cloned().unwrap_or_else(|| json!({}));
            if upstream::unsubscribe(&mut s, name, &args, &url).await.is_err() {
                upstream_ok = false;
            }
        }
        s.close().await;
    }
    super::audit(home, super::AUDIT_REVOKED, &rec.agent_id, json!({ "subscription": id, "server": rec.server, "upstream_acknowledged": upstream_ok }));
    Ok(upstream_ok)
}

/// Replace the signing secret: the new one is stored (the old one stays
/// accepted for [`ROTATION_GRACE_SECS`]) and sent upstream with a refresh.
pub async fn rotate(home: &Path, id: &str) -> Result<SubscriptionView, String> {
    if !store::is_valid_id(id) {
        return Err("invalid subscription id".into());
    }
    let rec = store::get(home, id)?.ok_or_else(|| "subscription not found".to_string())?;
    if rec.delivery == DeliveryKind::Poll {
        return Err("a poll-mode subscription has no signing secret".into());
    }
    let base = public_base_url(home)?;
    let old = store::open(home, &rec)?;
    let new_secret = super::signature::generate_secret();
    let sealed = store::seal(
        home,
        &SubSecrets {
            current: new_secret.clone(),
            previous: Some(old.current),
            previous_until: Some(chrono::Utc::now().timestamp() + ROTATION_GRACE_SECS),
        },
    )?;
    store::update(home, id, |r| {
        r.secret_enc = sealed;
        r.rotated_at = Some(store::now_rfc3339());
        Ok(true)
    })?;
    super::audit(home, super::AUDIT_ROTATED, &rec.agent_id, json!({ "subscription": id, "server": rec.server }));
    let rec = store::get(home, id)?.ok_or_else(|| "subscription not found".to_string())?;
    let _ = subscribe_all(home, &rec, &callback_url(&base, id), &new_secret).await;
    get_view(home, id)
}

/// Refresh every subscription whose grant is missing or ends within
/// [`REFRESH_MARGIN_SECS`]. Run at boot and on the 10-minute sweep.
pub async fn refresh_due(home: &Path) -> usize {
    let Ok(all) = store::load_all(home) else { return 0 };
    let Ok(base) = public_base_url(home) else { return 0 };
    let now = chrono::Utc::now().timestamp();
    let mut n = 0;
    for rec in all {
        if rec.status == SubStatus::Terminated || rec.delivery == DeliveryKind::Poll {
            continue;
        }
        let due = rec.upstream.iter().any(|u| {
            u.last_error.is_some()
                || match u.refresh_before.as_deref().and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok()) {
                    Some(t) => t.timestamp() - now < REFRESH_MARGIN_SECS,
                    None => u.upstream_id.is_none(),
                }
        });
        if !due {
            continue;
        }
        let Ok(secrets) = store::open(home, &rec) else { continue };
        let _ = subscribe_all(home, &rec, &callback_url(&base, &rec.id), &secrets.current).await;
        n += 1;
    }
    n
}

/// The refresh sweep (boot, then every 10 minutes).
pub fn spawn_refresh_sweep(home: std::path::PathBuf) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(600));
        loop {
            tick.tick().await;
            if store::store_path(&home).exists() {
                super::poll::catch_up_all(&home).await;
                let n = refresh_due(&home).await;
                if n > 0 {
                    tracing::info!(refreshed = n, "mcp-events: subscriptions refreshed");
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rules() {
        assert_eq!(validate_base_url("https://hooks.example.com/").unwrap(), "https://hooks.example.com");
        assert_eq!(validate_base_url("https://x.example/gw").unwrap(), "https://x.example/gw");
        assert!(validate_base_url("http://hooks.example.com").is_err());
        assert!(validate_base_url("http://127.0.0.1:18789").is_ok());
        assert!(validate_base_url("https://u:p@x.example").is_err());
        assert!(validate_base_url("https://x.example/?a=1").is_err());
        assert!(validate_base_url("ftp://x.example").is_err());
    }
}
