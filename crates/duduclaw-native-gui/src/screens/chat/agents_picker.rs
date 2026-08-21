// S4b — agent list + picker for the chat page (task item #3): who a NEW
// conversation talks to. Rendered as a chip row (avatar-letter + name +
// status dot, per the task brief's own wording) at the top of the
// conversations sidebar — see `conversations.rs::render_sidebar`.
//
// Fetched once via `sidebar_rpc::call(.., "agents.list", ..)` — see
// `sidebar_rpc.rs`'s module doc comment for why this doesn't go through the
// main `/ws`'s dormant `ws_status::call` infrastructure.
//
// Response shape read directly from `duduclaw-gateway/src/handlers.rs`'s
// `handle_agents_list_filtered` (not guessed — see that function, roughly
// line 25683): `{ "agents": [ { "name", "display_name", "role", "status",
// "archived", "icon", ... (many more fields this client doesn't read) } ] }`.
// `status` is `AgentStatus`'s `{:?}` lowercased — "active" / "paused" /
// "terminated" in practice (archived/deleted agents are already excluded
// server-side by `AgentStatus::is_listable`).

use std::cell::Cell;

use gpui::{div, prelude::*, px, Context, Div, FontWeight, SharedString, Stateful};
use serde_json::Value;
use tokio::sync::mpsc as tokio_mpsc;

use crate::i18n::{self, Locale};
use crate::mds_gpui::skeleton;
use crate::theme;
use crate::RootView;

use super::sidebar_rpc;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentSummary {
    /// `name` in the wire payload — the routable agent id, distinct from
    /// the human-facing `display_name`.
    pub id: String,
    pub display_name: String,
    pub icon: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadStatus {
    Idle,
    Loading,
    Ready,
    Error,
}

pub struct AgentsState {
    pub agents: Vec<AgentSummary>,
    pub status: LoadStatus,
    /// `screens/chat.rs::render` only receives `&RootView` (see that file's
    /// module doc comment), so this `Cell` is the interior-mutability escape
    /// hatch that lets the one-shot initial fetch fire exactly once from a
    /// shared reference — see `chat.rs::maybe_boot_fetch`. Honest limitation:
    /// this latches for the app's lifetime, so a relogin after a
    /// `WsAuthFailed` kick-back will not automatically re-fetch (S4 has no
    /// logout button either — out of scope for this pass, not a regression).
    boot_kicked: Cell<bool>,
}

impl AgentsState {
    pub fn new() -> Self {
        Self { agents: Vec::new(), status: LoadStatus::Idle, boot_kicked: Cell::new(false) }
    }

    pub fn should_boot_fetch(&self) -> bool {
        !self.boot_kicked.get()
    }

    pub fn mark_boot_kicked(&self) {
        self.boot_kicked.set(true);
    }

    pub fn set_loading(&mut self) {
        self.status = LoadStatus::Loading;
    }

    pub fn apply_loaded(&mut self, agents: Vec<AgentSummary>) {
        self.agents = agents;
        self.status = LoadStatus::Ready;
    }

    pub fn apply_error(&mut self) {
        self.status = LoadStatus::Error;
    }

    pub fn find(&self, id: &str) -> Option<&AgentSummary> {
        self.agents.iter().find(|a| a.id == id)
    }
}

impl Default for AgentsState {
    fn default() -> Self {
        Self::new()
    }
}

/// Pure parse of the `agents.list` payload — defensive against any missing/
/// malformed field (an entry with no `name` is dropped, never panics).
pub fn parse_agents(payload: &Value) -> Vec<AgentSummary> {
    payload
        .get("agents")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|a| {
                    let id = a.get("name")?.as_str()?.to_string();
                    let display_name = a
                        .get("display_name")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .unwrap_or_else(|| id.clone());
                    let icon = a.get("icon").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
                    let status = a.get("status").and_then(|v| v.as_str()).unwrap_or("active").to_string();
                    Some(AgentSummary { id, display_name, icon, status })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Kick off the fetch and route the result back into `RootView.chat.agents`.
/// Called from `chat.rs::maybe_boot_fetch` (initial load) — see that
/// function for the one-shot-from-a-shared-reference `Cell` guard.
pub fn fetch(rpc_tx: tokio_mpsc::UnboundedSender<sidebar_rpc::Command>, cx: &mut Context<RootView>) {
    let rx = sidebar_rpc::call(&rpc_tx, "agents.list", serde_json::json!({}));
    cx.spawn(async move |view, cx| {
        // See `conversations::fetch`'s identical pattern: the loading state
        // flips here (the spawned task's first tick), not in `chat.rs`'s
        // `maybe_boot_fetch`, which only has `&RootView`.
        let _ = view.update(cx, |view, cx| {
            view.chat.agents.set_loading();
            cx.notify();
        });
        let outcome = rx.await;
        let _ = view.update(cx, |view, cx| {
            match outcome {
                Ok(Ok(payload)) => view.chat.agents.apply_loaded(parse_agents(&payload)),
                Ok(Err(e)) => {
                    eprintln!("[chat] agents.list failed: {e:?}");
                    view.chat.agents.apply_error();
                }
                Err(_) => {
                    eprintln!("[chat] agents.list: sidebar-rpc thread gone");
                    view.chat.agents.apply_error();
                }
            }
            cx.notify();
        });
    })
    .detach();
}

/// First character of a display name (CJK-safe — codepoint, never a byte
/// slice) as the avatar glyph, matching the design canvas's single-CJK-
/// character avatar labels ("杜"/"財"). Falls back to the agent's `icon`
/// emoji when one is set server-side.
fn avatar_glyph(agent: &AgentSummary) -> String {
    if let Some(icon) = &agent.icon {
        return icon.clone();
    }
    agent.display_name.chars().next().map(|c| c.to_string()).unwrap_or_else(|| "?".to_string())
}

/// Deterministic (not random) color per agent id, so the same agent always
/// gets the same avatar color across renders/sessions — picked from a small
/// curated MDS palette rather than an arbitrary hash-to-RGB (which could
/// land on a low-contrast or off-brand color).
pub fn avatar_color(id: &str) -> u32 {
    const PALETTE: [u32; 4] = [theme::BRAND, theme::SUCCESS, theme::INFO, theme::WARNING];
    let hash = id.bytes().fold(0u32, |acc, b| acc.wrapping_mul(31).wrapping_add(b as u32));
    PALETTE[hash as usize % PALETTE.len()]
}

fn status_dot_color(status: &str) -> u32 {
    match status {
        "active" => theme::SUCCESS,
        "paused" => theme::WARNING,
        _ => theme::MUTED_FOREGROUND,
    }
}

/// Shared avatar-circle element — used by both the agent chip row here and
/// `conversations.rs`'s conversation-card avatars, so the two stay visually
/// identical for the same agent.
pub fn avatar_circle(agent: &AgentSummary, size: gpui::Pixels) -> Div {
    div()
        .flex_shrink_0()
        .w(size)
        .h(size)
        .rounded_full()
        .bg(theme::alpha(avatar_color(&agent.id), 1.0))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(11.))
        .font_weight(FontWeight::BOLD)
        .text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        .child(SharedString::from(avatar_glyph(agent)))
}

fn default_chip(locale: Locale, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    div()
        .id("agent-chip-default")
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).border_1().border_color(theme::surface_border())
        })
        .hover(|s| s.bg(theme::alpha(theme::BRAND, if selected { 0.9 } else { 0.15 })))
        .text_size(px(theme::TEXT_XS))
        .text_color(if selected {
            theme::alpha(theme::BRAND_FOREGROUND, 1.0)
        } else {
            theme::alpha(theme::FOREGROUND, 1.0)
        })
        .child(i18n::t(locale, "native.chat.agentPicker.default"))
        .on_click(cx.listener(|this, _ev, _window, cx| {
            this.chat.select_agent(None);
            cx.notify();
        }))
}

fn agent_chip(agent: &AgentSummary, selected: bool, cx: &mut Context<RootView>) -> Stateful<Div> {
    let id_for_click = agent.id.clone();
    div()
        .id(SharedString::from(format!("agent-chip-{}", agent.id)))
        .flex()
        .items_center()
        .gap_1p5()
        .px_2()
        .py_1()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 1.0)))
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE_RAISED, 1.0)).border_1().border_color(theme::surface_border())
        })
        .hover(|s| s.bg(theme::alpha(theme::BRAND, if selected { 0.9 } else { 0.15 })))
        .child(avatar_circle(agent, px(18.)))
        .child(
            div()
                .text_size(px(theme::TEXT_XS))
                .text_color(if selected {
                    theme::alpha(theme::BRAND_FOREGROUND, 1.0)
                } else {
                    theme::alpha(theme::FOREGROUND, 1.0)
                })
                .child(SharedString::from(agent.display_name.clone())),
        )
        .child(div().size_2().rounded_full().bg(theme::alpha(status_dot_color(&agent.status), 1.0)))
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.chat.select_agent(Some(id_for_click.clone()));
            cx.notify();
        }))
}

/// The full chip row: a leading "default agent" chip plus one chip per
/// fetched agent. `conversations.rs::render_sidebar` places this above the
/// search box.
pub fn render_chips(state: &RootView, cx: &mut Context<RootView>) -> Div {
    let locale = state.locale;
    let selected = state.chat.selected_agent_id.clone();

    let mut row = div().flex().flex_wrap().gap_1p5().px_2().pb_2();
    row = row.child(default_chip(locale, selected.is_none(), cx));

    match state.chat.agents.status {
        LoadStatus::Loading if state.chat.agents.agents.is_empty() => {
            row = row.child(skeleton(px(64.), px(22.)).rounded(px(theme::RADIUS_4XL)));
        }
        LoadStatus::Error => {
            row = row.child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(i18n::t(locale, "native.chat.agentPicker.loadError")),
            );
        }
        _ => {}
    }

    for agent in &state.chat.agents.agents {
        let is_selected = selected.as_deref() == Some(agent.id.as_str());
        row = row.child(agent_chip(agent, is_selected, cx));
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agents_happy_path() {
        let payload = serde_json::json!({
            "agents": [
                {"name": "dudu", "display_name": "小杜", "icon": "🐾", "status": "active"},
                {"name": "finance", "display_name": "財務助理", "status": "paused"},
            ]
        });
        let agents = parse_agents(&payload);
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0], AgentSummary {
            id: "dudu".to_string(),
            display_name: "小杜".to_string(),
            icon: Some("🐾".to_string()),
            status: "active".to_string(),
        });
        assert_eq!(agents[1].icon, None);
        assert_eq!(agents[1].status, "paused");
    }

    #[test]
    fn parse_agents_missing_name_is_dropped_not_panicking() {
        let payload = serde_json::json!({"agents": [{"display_name": "no id"}, {"name": "ok"}]});
        let agents = parse_agents(&payload);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].id, "ok");
    }

    #[test]
    fn parse_agents_missing_display_name_falls_back_to_id() {
        let payload = serde_json::json!({"agents": [{"name": "raw-id"}]});
        let agents = parse_agents(&payload);
        assert_eq!(agents[0].display_name, "raw-id");
    }

    #[test]
    fn parse_agents_empty_or_malformed_payload_is_empty_not_panicking() {
        assert!(parse_agents(&serde_json::json!({})).is_empty());
        assert!(parse_agents(&serde_json::json!({"agents": "not an array"})).is_empty());
        assert!(parse_agents(&serde_json::json!(null)).is_empty());
    }

    #[test]
    fn avatar_color_is_deterministic() {
        assert_eq!(avatar_color("dudu"), avatar_color("dudu"));
    }

    #[test]
    fn boot_kick_guard_fires_once() {
        let state = AgentsState::new();
        assert!(state.should_boot_fetch());
        state.mark_boot_kicked();
        assert!(!state.should_boot_fetch());
    }
}
