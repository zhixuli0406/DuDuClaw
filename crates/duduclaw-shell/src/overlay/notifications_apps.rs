// D6 (2026-08-23) — the Notifications panel's third-party app section.
//
// Its own file, not more lines in `overlay/notifications.rs`: that file was
// already 890 lines before this round, and this crate's own "200-400 typical,
// 800 max" convention (stated in `i18n/tests.rs`'s header, and the reason
// `oobe/mod.rs` was split into `ui_state.rs`/`network_ui.rs`/`selections.rs`)
// leaves no room. The D6 brief's own instruction was "overlay/notifications
// 接點最小改" — that接點 is now exactly two things: one extra parameter on
// `notifications::render`/`content`, and one call to
// `app_notifications_section` from `content`.
//
// ## What this draws, and what it deliberately reuses
//
// The visual language is the EXISTING one. The card is `approval_card`'s box
// (same radius/padding/shadow/border tokens), the avatar is the same
// `avatar()` circle, the buttons are the same `action_button()`, the banner
// is the same `status_banner()`. There is no design board for this section —
// the brief said "UI 卡片樣式照現有 Calm Glass", and a second card style
// would defeat the entire reason the shell serves `org.freedesktop.
// Notifications` itself instead of shipping mako/dunst (see `crate::notifyd`'s
// module doc): a browser's notification and an agent's approval have to land
// in one place, looking like one system.
//
// Those five helpers are `pub(super)` on `notifications.rs` rather than
// copied here — a second `avatar()` that drifts from the first is exactly
// what this split must not cause.
//
// ## No state of its own
//
// Every click here mutates `ShellView::notify_center` (a pure state machine,
// `notifyd::center`) and nothing else. The resulting `ActionInvoked`/
// `NotificationClosed` signals are picked up by the drain task in `main.rs`
// on its next tick and handed to the notifyd thread — nothing in this file
// touches D-Bus, or knows it exists.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Rgba, Stateful};

use duduclaw_native_gui::theme;

use super::notifications::{action_button, agent_color_for, avatar, decision_badge_owned, status_banner};
use crate::i18n::{t, t1, Key, Locale};
use crate::palette::ShellPalette;
use crate::ShellView;

/// Appends the section heading, the honest daemon-status banner (when there
/// is bad news) and one card per notification. Renders nothing at all when
/// the daemon is healthy and there are no notifications — an empty section
/// heading over an empty list is noise, and the approval feed already owns
/// the panel's "nothing here" line.
pub(super) fn app_notifications_section(
    body: Div,
    center: &crate::notifyd::center::NotificationCenter,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Div {
    let banner = daemon_status_banner(center.daemon());
    if banner.is_none() && center.is_empty() {
        return body;
    }

    let mut body = body.child(app_section_header(!center.is_empty(), palette, cx));
    if let Some((text, is_error)) = banner {
        body = body.child(status_banner(&text, palette, is_error));
    }

    // One clock read for the whole section, so every card's relative
    // timestamp in a given frame is consistent with every other's.
    let now = std::time::Instant::now();
    body.children(center.items().iter().map(|card| app_notification_card(card, now, palette, cx)).collect::<Vec<_>>())
}

/// Turns the daemon's state into the operator-facing sentence, or `None` when
/// there is nothing to say.
///
/// `Running` and the two pre-answer states say nothing on purpose: a working
/// daemon needs no banner, and flashing "connecting…" for the ~10ms the bus
/// takes to answer would be noise. The three bad-news states are all
/// surfaced, because E1a's whole finding was that this failure had no
/// symptom at all.
fn daemon_status_banner(state: &crate::notifyd::center::DaemonState) -> Option<(String, bool)> {
    use crate::notifyd::center::DaemonState;
    match state {
        DaemonState::NotStarted | DaemonState::Starting | DaemonState::Running => None,
        // Not an error: those notifications ARE being shown, by the other
        // daemon — just not in this panel.
        DaemonState::NameTaken => Some((t(Locale::ZhTw, Key::NotifDaemonNameTakenBanner).to_string(), false)),
        DaemonState::Failed(why) => Some((t1(Locale::ZhTw, Key::NotifDaemonFailedBanner, why), true)),
        DaemonState::Unsupported => Some((t(Locale::ZhTw, Key::NotifDaemonUnsupportedBanner).to_string(), false)),
    }
}

fn app_section_header(has_items: bool, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let clear_all = cx.listener(|view, _ev, _window, cx| {
        // Clearing tells every sender its notification was dismissed (see
        // `NotificationCenter::dismiss_all`); the drain task picks the
        // resulting `EmitCommand`s up on its next tick and hands them to the
        // notifyd thread. Nothing here touches the bus directly.
        if view.notify_center.dismiss_all() {
            cx.notify();
        }
    });

    let mut row = div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(4.))
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .child(t(Locale::ZhTw, Key::NotifAppSectionLabel)),
        );
    if has_items {
        let link_text = if palette.is_dark() { palette.brand_bright } else { palette.brand };
        row = row.child(
            div()
                .id("notif-app-clear-all")
                .cursor_pointer()
                .text_size(px(11.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(link_text, 1.0))
                .child(t(Locale::ZhTw, Key::NotifClearAllButton))
                .on_click(clear_all),
        );
    }
    row
}

fn app_notification_card(
    card: &crate::notifyd::center::CenterNotification,
    now: std::time::Instant,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    use crate::notifyd::Urgency;

    // Critical borrows the pending-approval amber; Low/Normal use the same
    // neutral card border a resolved approval gets. No new color is invented
    // for this section — see this section's own header comment.
    let border_color: Rgba = match (card.urgency, palette.is_dark()) {
        (Urgency::Critical, true) => theme::alpha(palette.warning, 0.45),
        (Urgency::Critical, false) => theme::alpha(0xf3d9a4, 1.0),
        (_, true) => theme::alpha(0xffffff, 0.12),
        (_, false) => theme::alpha(0xececef, 1.0),
    };

    let initial = card.app_name.chars().next().map(String::from).unwrap_or_else(|| "?".to_string());
    let bg_hex = agent_color_for(&card.app_name);

    let id = card.id;
    let has_default = card.default_action().is_some();

    let mut root = div()
        .id(format!("notif-app-{id}"))
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(12.))
        .px(px(14.))
        .py(px(13.))
        .shadow(palette.surface_shadow())
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(avatar(initial, bg_hex, palette))
                .child(div().flex_1().text_size(px(13.)).font_weight(FontWeight::SEMIBOLD).child(card.summary.clone()))
                .when(card.urgency == Urgency::Critical, |el| {
                    el.child(div().w(px(7.)).h(px(7.)).rounded(px(7.)).bg(theme::alpha(palette.destructive, 1.0)))
                }),
        );

    if !card.body.is_empty() {
        // Plain text, never markup: `GetCapabilities` does not advertise
        // `body-markup`, so a sender that ships HTML anyway sees it verbatim
        // rather than half-parsed. Honest, and it is what the client was told.
        root = root.child(div().text_size(px(12.)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(card.body.clone()));
    }

    // Provenance line: which app, how long ago, and — when the flood guard
    // folded others onto this card — how many messages it stands for.
    let mut meta = div()
        .flex()
        .items_center()
        .gap(px(6.))
        .child(
            div()
                .flex_1()
                .text_size(px(11.))
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .child(format!("{} · {}", card.app_name, age_label(card.received_at, now))),
        );
    if card.merged > 0 {
        meta = meta.child(decision_badge_owned(
            t1(Locale::ZhTw, Key::NotifMergedCount, &card.merged.to_string()),
            theme::alpha(palette.text_secondary, 1.0),
            theme::alpha(palette.surface_hover, 1.0),
        ));
    }
    root = root.child(meta);

    // Buttons: the sender's own non-default actions, then 關閉. `button_
    // actions` already excludes `default` (that one is the card click), and
    // `notifyd` caps how many can arrive at all.
    let mut buttons = div().flex().items_center().gap(px(8.));
    for action in card.button_actions() {
        let key = action.key.clone();
        let on_click = cx.listener(move |view, _ev, _window, cx| {
            if view.notify_center.invoke(id, &key) {
                cx.notify();
            }
        });
        buttons = buttons.child(action_button(&action.label, format!("notif-app-{id}-act-{}", action.key), false, palette, on_click));
    }
    let dismiss = cx.listener(move |view, _ev, _window, cx| {
        if view.notify_center.dismiss(id) {
            cx.notify();
        }
    });
    buttons = buttons.child(action_button(t(Locale::ZhTw, Key::NotifDismissButton), format!("notif-app-{id}-dismiss"), false, palette, dismiss));
    root = root.child(buttons);

    // A click on the card body activates the sender's `default` action —
    // and ONLY when the sender declared one. A card with no default action
    // is not made clickable at all: silently swallowing a message on a click
    // that the app never asked to be told about would be worse than doing
    // nothing (see `NotificationCenter::invoke_default`).
    if has_default {
        let on_click = cx.listener(move |view, _ev, _window, cx| {
            if view.notify_center.invoke_default(id) {
                cx.notify();
            }
        });
        root = root.cursor_pointer().on_click(on_click);
    }
    root
}

/// "剛剛 / N 分鐘前 / …" — the bucketing itself lives in
/// `notifyd::center::RelativeAge` (pure, unit-tested); this only picks the
/// catalog string.
fn age_label(received_at: std::time::Instant, now: std::time::Instant) -> String {
    use crate::notifyd::center::RelativeAge;
    match RelativeAge::of(received_at, now) {
        RelativeAge::JustNow => t(Locale::ZhTw, Key::NotifAgeJustNow).to_string(),
        RelativeAge::Minutes(n) => t1(Locale::ZhTw, Key::NotifAgeMinutes, &n.to_string()),
        RelativeAge::Hours(n) => t1(Locale::ZhTw, Key::NotifAgeHours, &n.to_string()),
        RelativeAge::Days(n) => t1(Locale::ZhTw, Key::NotifAgeDays, &n.to_string()),
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::notifyd::center::DaemonState;
    use std::time::{Duration, Instant};

    /// The honesty surface D6 exists for. E1a's finding was that a missing
    /// notification daemon had NO symptom at all — every state in which
    /// third-party notifications cannot reach this panel must therefore say
    /// so, and every state in which they can must stay quiet.
    #[test]
    fn every_state_that_costs_the_operator_notifications_says_so() {
        assert!(daemon_status_banner(&DaemonState::NameTaken).is_some());
        assert!(daemon_status_banner(&DaemonState::Failed("no session bus".into())).is_some());
        assert!(daemon_status_banner(&DaemonState::Unsupported).is_some());
    }

    #[test]
    fn a_working_daemon_and_the_states_before_it_answers_stay_quiet() {
        // A banner while the bus is still answering the very first
        // `RequestName` would flash for milliseconds and mean nothing.
        assert!(daemon_status_banner(&DaemonState::Running).is_none());
        assert!(daemon_status_banner(&DaemonState::NotStarted).is_none());
        assert!(daemon_status_banner(&DaemonState::Starting).is_none());
    }

    /// Another daemon owning the name is NOT an error: those notifications
    /// are being shown, just somewhere else. Only a genuine failure gets the
    /// destructive treatment.
    #[test]
    fn only_a_real_failure_is_rendered_as_an_error() {
        assert_eq!(daemon_status_banner(&DaemonState::NameTaken).map(|(_, err)| err), Some(false));
        assert_eq!(daemon_status_banner(&DaemonState::Unsupported).map(|(_, err)| err), Some(false));
        assert_eq!(daemon_status_banner(&DaemonState::Failed("boom".into())).map(|(_, err)| err), Some(true));
    }

    /// The failure banner must carry the actual reason — "can't receive app
    /// notifications" with no cause is the kind of message that sends an
    /// operator to a log file.
    #[test]
    fn the_failure_banner_names_the_cause() {
        let (text, _) = daemon_status_banner(&DaemonState::Failed("no session bus".into())).expect("a failure must produce a banner");
        assert!(text.contains("no session bus"), "banner did not carry the reason: {text}");
    }

    #[test]
    fn relative_ages_render_as_non_empty_zh_tw_phrases() {
        let t0 = Instant::now();
        for after in [Duration::from_secs(1), Duration::from_secs(180), Duration::from_secs(7200), Duration::from_secs(300_000)] {
            let label = age_label(t0, t0 + after);
            assert!(!label.is_empty());
            assert!(!label.contains("{}"), "an unsubstituted placeholder leaked into the UI: {label}");
        }
    }
}
