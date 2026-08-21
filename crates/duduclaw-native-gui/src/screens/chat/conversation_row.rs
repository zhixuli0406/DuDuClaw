// S4b — pure parsing/classification for `conversations.rs`'s sidebar list,
// plus the per-row card element and the scrollable grouped-list assembly.
// Split out of `conversations.rs` (which owns the sidebar's STATE + fetch +
// search box + header) purely to keep each file under this crate's own
// <800-line convention — no behavior differs from an unsplit version; see
// `conversations.rs`'s module doc comment for the RPC-shape/design-trade-off
// background this file's logic is built against (last-message-content
// absence, "未讀點" client-local approximation, admin-vs-non-admin scoping).

use chrono::{DateTime, Local, NaiveDate, Utc};
use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};
use serde_json::Value;

use crate::i18n::{self, Locale};
use crate::mds_gpui::{empty_state, skeleton};
use crate::theme;
use crate::RootView;

use super::agents_picker::{self, AgentSummary};
use super::conversations::{fetch_history, ConversationsState, LoadStatus};

#[derive(Debug, Clone, PartialEq)]
pub struct ConversationSummary {
    pub session_id: String,
    pub agent_id: String,
    pub title: String,
    /// Raw RFC3339 string as the server sent it — parsed on demand by
    /// [`parse_last_active`] rather than stored pre-parsed, so a malformed
    /// value from a future server change degrades per-row (empty time
    /// label, still sorts/groups as "Earlier") instead of dropping the
    /// whole row.
    pub last_active: String,
    pub turns: u64,
}

/// Known channel-id prefixes a conversation session id can carry — ported
/// verbatim from `web/src/lib/session-channel.ts`'s `CHANNEL_LABELS` key set
/// (the second reference this task's brief points at), so the native client
/// filters out the same internal work sessions (cron / delegation / goal
/// runs, which carry no recognized prefix) the web dashboard already does.
const CHANNEL_KEYS: [&str; 10] =
    ["webchat", "telegram", "discord", "line", "slack", "whatsapp", "feishu", "googlechat", "teams", "dingtalk"];

pub fn is_conversation_session(session_id: &str) -> bool {
    match session_id.split_once(':') {
        Some((prefix, _)) if !prefix.is_empty() => {
            let lower = prefix.to_lowercase();
            CHANNEL_KEYS.contains(&lower.as_str())
        }
        _ => false,
    }
}

/// Pure parse of the `chat.sessions.list` payload. An entry missing
/// `session_id` is dropped (never panics); everything else defaults to an
/// empty/zero value rather than failing the whole list over one odd row.
pub fn parse_sessions(payload: &Value) -> Vec<ConversationSummary> {
    payload
        .get("sessions")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|s| {
                    let session_id = s.get("session_id")?.as_str()?.to_string();
                    let agent_id = s.get("agent_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let title = s.get("title").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let last_active = s.get("last_active").and_then(|v| v.as_str()).unwrap_or("").to_string();
                    let turns = s.get("turns").and_then(|v| v.as_u64()).unwrap_or(0);
                    Some(ConversationSummary { session_id, agent_id, title, last_active, turns })
                })
                .filter(|c| is_conversation_session(&c.session_id))
                .collect()
        })
        .unwrap_or_default()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateGroup {
    Today,
    Yesterday,
    ThisWeek,
    Earlier,
}

/// Pure classification given two already-resolved local calendar dates — no
/// wall-clock dependency, so tests can exercise every branch with fixed
/// dates. `item > today` (clock skew / a slightly-future timestamp) is
/// treated as `Today` rather than falling through to `Earlier`, which would
/// read as a confusing "this hasn't happened yet, but it's in the past"
/// section.
fn classify_group(item: NaiveDate, today: NaiveDate) -> DateGroup {
    let days = (today - item).num_days();
    if days <= 0 {
        DateGroup::Today
    } else if days == 1 {
        DateGroup::Yesterday
    } else if days <= 7 {
        DateGroup::ThisWeek
    } else {
        DateGroup::Earlier
    }
}

fn parse_last_active(s: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s).ok().map(|dt| dt.with_timezone(&Utc))
}

fn group_label(group: DateGroup, locale: Locale) -> SharedString {
    let key = match group {
        DateGroup::Today => "native.chat.conversations.groupToday",
        DateGroup::Yesterday => "native.chat.conversations.groupYesterday",
        DateGroup::ThisWeek => "native.chat.conversations.groupThisWeek",
        DateGroup::Earlier => "native.chat.conversations.groupEarlier",
    };
    i18n::t(locale, key)
}

/// Right-aligned per-row time label: a clock time for anything from today,
/// a numeric `MM/DD` otherwise (see `conversations.rs`'s module doc comment
/// — localizing weekday abbreviations across zh-TW/en/ja-JP for the "week"
/// case is out of scope for this pass; the SECTION header already carries
/// that meaning).
fn row_time_label(last_active: &str) -> String {
    let Some(utc) = parse_last_active(last_active) else {
        return String::new();
    };
    let local = utc.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m/%d").to_string()
    }
}

fn matches_search(session: &ConversationSummary, agent_display_name: &str, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    session.title.to_lowercase().contains(&q) || agent_display_name.to_lowercase().contains(&q)
}

fn fallback_agent(agent_id: &str) -> AgentSummary {
    AgentSummary {
        id: agent_id.to_string(),
        display_name: agent_id.to_string(),
        icon: None,
        status: "active".to_string(),
    }
}

fn conversation_card(
    state: &RootView,
    session: &ConversationSummary,
    agent: &AgentSummary,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let selected = state.chat.session_id.as_deref() == Some(session.session_id.as_str());
    let unread = state.chat.conversations.is_unread(&session.session_id, state.chat.session_id.as_deref());
    let title =
        if session.title.trim().is_empty() { agent.display_name.clone() } else { session.title.clone() };
    let time_label = row_time_label(&session.last_active);
    let session_id_for_click = session.session_id.clone();

    div()
        .id(SharedString::from(format!("chat-conv-row-{}", session.session_id)))
        .mx_2()
        .my_0p5()
        .px_2()
        .py_2()
        .rounded(px(theme::RADIUS_MD))
        .cursor_pointer()
        .when(selected, |el| el.bg(theme::alpha(theme::BRAND, 0.14)))
        .when(!selected, |el| el.hover(|s| s.bg(theme::alpha(theme::SURFACE_HOVER, 1.0))))
        .flex()
        .gap_2()
        .child(agents_picker::avatar_circle(agent, px(30.)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap_0p5()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(theme::TEXT_SM))
                                .font_weight(if selected {
                                    gpui::FontWeight::SEMIBOLD
                                } else {
                                    gpui::FontWeight::MEDIUM
                                })
                                .text_color(if selected {
                                    theme::alpha(theme::BRAND, 1.0)
                                } else {
                                    theme::alpha(theme::FOREGROUND, 1.0)
                                })
                                .overflow_hidden()
                                .child(SharedString::from(title)),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .when(unread, |el| {
                                    el.child(div().size_1p5().rounded_full().bg(theme::alpha(theme::BRAND, 1.0)))
                                })
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_XS))
                                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                        .child(SharedString::from(time_label)),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .overflow_hidden()
                        .child(SharedString::from(agent.display_name.clone())),
                ),
        )
        .on_click(cx.listener(move |this, _ev, _window, cx| {
            this.chat.conversations.mark_read(&session_id_for_click);
            let rpc_tx = this.chat.rpc_tx();
            fetch_history(rpc_tx, session_id_for_click.clone(), cx);
            cx.notify();
        }))
}

/// The scrollable body of the sidebar: idle/loading skeleton → error state →
/// empty state (no data at all, or a search with no matches) → the grouped
/// list itself. `conversations::render_sidebar` places this below the
/// header/chips/search box.
pub fn conversation_list(state: &RootView, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let conv: &ConversationsState = &state.chat.conversations;

    let filtered: Vec<&ConversationSummary> = conv
        .sessions
        .iter()
        .filter(|s| {
            let agent = state.chat.agents.find(&s.agent_id).cloned().unwrap_or_else(|| fallback_agent(&s.agent_id));
            matches_search(s, &agent.display_name, &conv.search_query.to_lowercase())
        })
        .collect();

    let mut list =
        div().id("chat-conv-list").flex_1().track_scroll(&conv.scroll_handle).overflow_y_scroll().flex().flex_col();

    match conv.status {
        LoadStatus::Idle | LoadStatus::Loading if conv.sessions.is_empty() => {
            for _ in 0..4 {
                list =
                    list.child(div().mx_2().my_1().child(skeleton(px(240.), px(48.)).rounded(px(theme::RADIUS_MD))));
            }
            return list;
        }
        LoadStatus::Error => {
            return list.child(
                div().p_4().child(empty_state(
                    "⚠️",
                    i18n::t(locale, "native.chat.conversations.loadError"),
                    None,
                    None::<Div>,
                )),
            );
        }
        _ => {}
    }

    if filtered.is_empty() {
        let (icon, text_key) = if conv.search_query.is_empty() {
            ("💬", "native.chat.conversations.empty")
        } else {
            ("🔍", "native.chat.conversations.emptyFiltered")
        };
        return list.child(div().p_4().child(empty_state(icon, i18n::t(locale, text_key), None, None::<Div>)));
    }

    let today = Local::now().date_naive();
    let mut last_group: Option<DateGroup> = None;
    for session in filtered {
        let group = parse_last_active(&session.last_active)
            .map(|utc| classify_group(utc.with_timezone(&Local).date_naive(), today))
            .unwrap_or(DateGroup::Earlier);
        if last_group != Some(group) {
            list = list.child(
                div()
                    .px_3()
                    .pt_2()
                    .pb_1()
                    .text_size(px(theme::TEXT_XS))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                    .child(group_label(group, locale)),
            );
            last_group = Some(group);
        }
        let agent =
            state.chat.agents.find(&session.agent_id).cloned().unwrap_or_else(|| fallback_agent(&session.agent_id));
        list = list.child(conversation_card(state, session, &agent, cx));
    }

    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_conversation_session_recognizes_known_channels() {
        for id in [
            "webchat:conn#agent:x#conv:y",
            "telegram:12345",
            "discord:thread:1",
            "line:u123",
            "slack:dm:x",
            "whatsapp:+886",
            "feishu:chat1",
            "googlechat:space1",
            "teams:conv1",
            "dingtalk:x",
        ] {
            assert!(is_conversation_session(id), "expected {id} to be a conversation session");
        }
    }

    #[test]
    fn is_conversation_session_rejects_internal_work_sessions() {
        for id in ["dispatch:run-1", "cron-job-42", "goal:abc", "", "nocolon"] {
            assert!(!is_conversation_session(id), "expected {id} to be rejected");
        }
    }

    #[test]
    fn is_conversation_session_is_case_insensitive_on_prefix() {
        assert!(is_conversation_session("Telegram:123"));
        assert!(is_conversation_session("WEBCHAT:x"));
    }

    #[test]
    fn parse_sessions_happy_path_filters_and_maps() {
        let payload = serde_json::json!({
            "sessions": [
                {"session_id": "webchat:a#agent:x#conv:1", "agent_id": "dudu", "title": "hi", "last_active": "2026-08-20T10:00:00Z", "turns": 3},
                {"session_id": "dispatch:internal-1", "agent_id": "dudu", "title": "internal", "last_active": "2026-08-20T10:00:00Z", "turns": 1},
            ]
        });
        let sessions = parse_sessions(&payload);
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].session_id, "webchat:a#agent:x#conv:1");
        assert_eq!(sessions[0].turns, 3);
    }

    #[test]
    fn parse_sessions_missing_session_id_is_dropped_not_panicking() {
        let payload = serde_json::json!({"sessions": [{"title": "no id"}]});
        assert!(parse_sessions(&payload).is_empty());
    }

    #[test]
    fn parse_sessions_malformed_payload_is_empty_not_panicking() {
        assert!(parse_sessions(&serde_json::json!({})).is_empty());
        assert!(parse_sessions(&serde_json::json!(null)).is_empty());
        assert!(parse_sessions(&serde_json::json!({"sessions": "nope"})).is_empty());
    }

    #[test]
    fn classify_group_today() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        assert_eq!(classify_group(today, today), DateGroup::Today);
    }

    #[test]
    fn classify_group_future_date_is_today_not_earlier() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let tomorrow = NaiveDate::from_ymd_opt(2026, 8, 21).unwrap();
        assert_eq!(classify_group(tomorrow, today), DateGroup::Today);
    }

    #[test]
    fn classify_group_yesterday() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let yesterday = NaiveDate::from_ymd_opt(2026, 8, 19).unwrap();
        assert_eq!(classify_group(yesterday, today), DateGroup::Yesterday);
    }

    #[test]
    fn classify_group_this_week_boundary() {
        let today = NaiveDate::from_ymd_opt(2026, 8, 20).unwrap();
        let six_days_ago = NaiveDate::from_ymd_opt(2026, 8, 14).unwrap();
        let seven_days_ago = NaiveDate::from_ymd_opt(2026, 8, 13).unwrap();
        let eight_days_ago = NaiveDate::from_ymd_opt(2026, 8, 12).unwrap();
        assert_eq!(classify_group(six_days_ago, today), DateGroup::ThisWeek);
        assert_eq!(classify_group(seven_days_ago, today), DateGroup::ThisWeek);
        assert_eq!(classify_group(eight_days_ago, today), DateGroup::Earlier);
    }

    #[test]
    fn parse_last_active_accepts_rfc3339_and_rejects_garbage() {
        assert!(parse_last_active("2026-08-20T10:00:00Z").is_some());
        assert!(parse_last_active("2026-08-20T10:00:00.123456+00:00").is_some());
        assert!(parse_last_active("not a date").is_none());
        assert!(parse_last_active("").is_none());
    }

    #[test]
    fn matches_search_empty_query_matches_everything() {
        let s = ConversationSummary {
            session_id: "webchat:x".into(),
            agent_id: "dudu".into(),
            title: "anything".into(),
            last_active: String::new(),
            turns: 0,
        };
        assert!(matches_search(&s, "小杜", ""));
    }

    #[test]
    fn matches_search_matches_title_case_insensitive() {
        let s = ConversationSummary {
            session_id: "webchat:x".into(),
            agent_id: "dudu".into(),
            title: "客服月報進度".into(),
            last_active: String::new(),
            turns: 0,
        };
        assert!(matches_search(&s, "小杜", "月報"));
        assert!(!matches_search(&s, "小杜", "退費"));
    }

    #[test]
    fn matches_search_matches_agent_display_name() {
        let s = ConversationSummary {
            session_id: "webchat:x".into(),
            agent_id: "finance".into(),
            title: "unrelated title".into(),
            last_active: String::new(),
            turns: 0,
        };
        assert!(matches_search(&s, "財務助理", "財務"));
    }
}
