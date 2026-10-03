//! Always-mounted webhook endpoints for the six webhook channels
//! (whatsapp / feishu / googlechat / teams / wecom / dingtalk).
//!
//! Before v1.68.0 each channel's router was built once at boot and merged
//! into the app only when the channel was configured then, so adding a
//! webhook channel from the dashboard reported success while the platform's
//! callbacks got 404 until a restart. Now every path is mounted at boot and
//! forwards to a per-channel slot. The slot holds the channel's router when
//! the channel is configured (boot or `channels.add` hot start) and is empty
//! otherwise; an empty slot answers 404, exactly like an unmounted route.
//! Removing the channel empties the slot.
//!
//! The channel routers keep their own verification (signatures, JWTs,
//! tokens re-read per request); this module only decides whether the route
//! exists.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use tower::ServiceExt;

use crate::channel_reply::ReplyContext;

/// Channel key (as used by `channels.add` / `channels.remove`) → path.
pub const WEBHOOK_CHANNELS: &[(&str, &str)] = &[
    ("whatsapp", "/webhook/whatsapp"),
    ("feishu", "/webhook/feishu"),
    ("googlechat", "/webhook/googlechat"),
    ("teams", "/webhook/teams"),
    ("wecom", "/webhook/wecom"),
    ("dingtalk", "/webhook/dingtalk"),
];

/// True for the channel keys served through a webhook slot.
pub fn is_webhook_channel(channel: &str) -> bool {
    WEBHOOK_CHANNELS.iter().any(|(c, _)| *c == channel)
}

/// Per-channel router slots.
#[derive(Default)]
pub struct WebhookSlots {
    slots: RwLock<HashMap<String, Router>>,
}

impl WebhookSlots {
    pub fn new() -> Self {
        Self::default()
    }

    /// Install (`Some`) or clear (`None`) a channel's router. Unknown channel
    /// keys are ignored.
    pub fn set(&self, channel: &str, router: Option<Router>) {
        if !is_webhook_channel(channel) {
            return;
        }
        let mut slots = self.slots.write().unwrap_or_else(|e| e.into_inner());
        match router {
            Some(r) => {
                slots.insert(channel.to_string(), r);
            }
            None => {
                slots.remove(channel);
            }
        }
    }

    pub fn get(&self, channel: &str) -> Option<Router> {
        self.slots
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(channel)
            .cloned()
    }

    pub fn is_mounted(&self, channel: &str) -> bool {
        self.get(channel).is_some()
    }
}

static GLOBAL: OnceLock<Arc<WebhookSlots>> = OnceLock::new();

/// The process's slots (one gateway per process).
pub fn global() -> Arc<WebhookSlots> {
    GLOBAL.get_or_init(|| Arc::new(WebhookSlots::new())).clone()
}

/// Router with every webhook path mounted, forwarding to `slots`.
pub fn router(slots: Arc<WebhookSlots>) -> Router {
    let mut app = Router::new();
    for (channel, path) in WEBHOOK_CHANNELS {
        let slots = slots.clone();
        let channel: &'static str = channel;
        app = app.route(
            path,
            any(move |req: Request<Body>| {
                let slots = slots.clone();
                async move { forward(&slots, channel, req).await }
            }),
        );
    }
    app
}

async fn forward(slots: &WebhookSlots, channel: &str, req: Request<Body>) -> Response {
    let Some(inner) = slots.get(channel) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match inner.oneshot(req).await {
        Ok(resp) => resp,
        Err(never) => match never {},
    }
}

/// Build a webhook channel's router from the current config. `None` when the
/// channel is not (completely) configured; the channel's own start function
/// logs why.
pub async fn start_webhook(channel: &str, home: &Path, ctx: Arc<ReplyContext>) -> Option<Router> {
    match channel {
        "whatsapp" => crate::whatsapp::start_whatsapp_webhook(home, ctx).await,
        "feishu" => crate::feishu::start_feishu_webhook(home, ctx).await,
        "googlechat" => crate::googlechat::start_googlechat_webhook(home, ctx).await,
        "teams" => crate::msteams::start_teams_webhook(home, ctx).await,
        "wecom" => crate::wecom::start_wecom_webhook(home, ctx).await,
        "dingtalk" => crate::dingtalk::start_dingtalk_webhook(home, ctx).await,
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;

    fn req(method: &str, path: &str) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(path)
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn empty_slot_is_404_and_filled_slot_serves_without_remount() {
        let slots = Arc::new(WebhookSlots::new());
        let app = router(slots.clone());

        let resp = app.clone().oneshot(req("POST", "/webhook/feishu")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Hot start after boot: the already-built app now serves the route.
        slots.set(
            "feishu",
            Some(Router::new().route("/webhook/feishu", post(|| async { "ok" }))),
        );
        let resp = app.clone().oneshot(req("POST", "/webhook/feishu")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        // Method the channel router does not define stays refused.
        let resp = app.clone().oneshot(req("GET", "/webhook/feishu")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
        // Other channels are unaffected.
        let resp = app.clone().oneshot(req("POST", "/webhook/wecom")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // Removal clears the slot.
        slots.set("feishu", None);
        let resp = app.oneshot(req("POST", "/webhook/feishu")).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn unknown_channel_keys_are_ignored() {
        let slots = WebhookSlots::new();
        slots.set("telegram", Some(Router::new()));
        assert!(!slots.is_mounted("telegram"));
        assert!(!is_webhook_channel("msteams"));
        assert!(is_webhook_channel("teams"));
    }
}
