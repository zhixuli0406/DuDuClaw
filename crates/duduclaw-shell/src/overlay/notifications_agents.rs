//! Thread → gpui bridge that keeps `OverlayUiState.agents` fresh — the same
//! shape as `notifications_tasks`, driven from the same 30 s stale check.
use std::sync::mpsc;
use std::time::Duration;

use gpui::Context;

use super::agents_feed::AgentsFeed;
use crate::gateway_client::{self, AgentRef};
use crate::ShellView;

const POLL_INTERVAL: Duration = Duration::from_millis(50);

pub(crate) fn trigger_agents_refresh_if_stale(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.overlay_ui.agents.is_stale() || !view.overlay_ui.agents.begin_refresh() {
        return;
    }
    let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(fetch_once(existing_jwt));
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    if apply_fetch_outcome(&mut view.overlay_ui.agents, outcome) {
                        cx.notify();
                    }
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    })
    .detach();
}

fn fetch_once(existing_jwt: Option<String>) -> Result<Vec<AgentRef>, gateway_client::GatewayError> {
    let jwt = match existing_jwt {
        Some(jwt) => jwt,
        None => gateway_client::bootstrap_local_session()?,
    };
    Ok(gateway_client::list_agents(&jwt)?)
}

fn apply_fetch_outcome(feed: &mut AgentsFeed, outcome: Result<Vec<AgentRef>, gateway_client::GatewayError>) -> bool {
    match outcome {
        Ok(items) => feed.apply_list_ok(items),
        Err(e) => {
            if crate::diag_enabled() {
                eprintln!("[agents] roster fetch failed: {e:?}");
            }
            feed.apply_list_err();
            false
        }
    }
}
