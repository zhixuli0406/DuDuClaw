// S4b (second wave) — row-level rendering for `inbox.rs`. Split out purely
// to keep `inbox.rs` (state + fetch orchestration + top composition) under
// this crate's own <800-line file convention — same reason
// `dashboard.rs`/`dashboard_cards.rs` are split. The pure feed model
// (`FeedItem` and friends) lives in the sibling `inbox_data.rs` module
// (WP-NG-debt, 2026-08-21 — same "types + pure parsing" split
// `goals_data.rs` established for `goals.rs`), imported directly from here
// rather than re-exported through `inbox.rs`, mirroring
// `goals_inspector.rs`'s own import of `goals_data`. No behavior differs
// from an unsplit version.

use chrono::{DateTime, Local, Utc};
use gpui::{div, prelude::*, px, Context, Div, SharedString, Stateful};

use crate::i18n::{self, Locale};
use crate::mds_gpui::{badge, button, skeleton, BadgeVariant, ButtonVariant};
use crate::screens::dashboard::error_row;
use crate::screens::inbox::{self, InboxState};
use crate::screens::inbox_data::{ApprovalExpand, FeedBadge, FeedCategory, FeedItem, FilterKind};
use crate::theme;
use crate::RootView;

// ── Chips ──────────────────────────────────────────────────────────────

pub(super) fn filter_chip(
    kind: FilterKind,
    label_key: &'static str,
    count: Option<usize>,
    selected: bool,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let label = i18n::t(locale, label_key);
    let text: SharedString = match count {
        Some(n) if n > 0 => format!("{label} {n}").into(),
        _ => label,
    };
    let id: &'static str = match kind {
        FilterKind::All => "inbox-chip-all",
        FilterKind::Approval => "inbox-chip-approval",
        FilterKind::Notification => "inbox-chip-notification",
        FilterKind::Mention => "inbox-chip-mention",
        FilterKind::System => "inbox-chip-system",
    };
    div()
        .id(id)
        .h(px(28.))
        .px_3()
        .flex()
        .items_center()
        .rounded(px(theme::RADIUS_4XL))
        .cursor_pointer()
        .when(selected, |el| {
            el.bg(theme::alpha(theme::BRAND, 1.0)).text_color(theme::alpha(theme::BRAND_FOREGROUND, 1.0))
        })
        .when(!selected, |el| {
            el.bg(theme::alpha(theme::SURFACE, 1.0))
                .border_1()
                .border_color(theme::surface_border())
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .hover(|style| style.bg(theme::alpha(theme::SURFACE_HOVER, 1.0)))
        })
        .text_size(px(theme::TEXT_XS))
        .child(text)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            cx.global_mut::<InboxState>().filter = kind;
            cx.notify();
        }))
}

// ── Loading / error scaffolding ───────────────────────────────────────

pub(super) fn skeleton_row() -> Div {
    div()
        .flex()
        .items_center()
        .gap_2p5()
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, 1.0))
        .border_1()
        .border_color(theme::surface_border())
        .child(skeleton(px(30.), px(30.)))
        .child(div().flex_1().flex().flex_col().gap_1p5().child(skeleton(px(260.), px(14.))).child(skeleton(px(160.), px(12.))))
}

pub(super) fn error_banner(message: SharedString) -> Div {
    div()
        .p_3()
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(theme::DESTRUCTIVE, 0.08))
        .border_1()
        .border_color(theme::alpha(theme::DESTRUCTIVE, 0.3))
        .child(error_row_text(message))
}

fn error_row_text(message: SharedString) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(theme::DESTRUCTIVE, 1.0)).child(message)
}

pub(super) fn retry_button(locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    button(
        "inbox-retry",
        i18n::t(locale, "native.inbox.retry"),
        ButtonVariant::Secondary,
        false,
        None,
        cx.listener(|_this, _ev, _window, cx| {
            cx.global_mut::<InboxState>().request_refresh();
            cx.notify();
        }),
    )
}

// ── Feed rows ──────────────────────────────────────────────────────────

/// Dispatches to the collapsed approval / expanded approval / plain
/// notification-or-system row shape — the three visual forms the canvas
/// shows (amber expanded card, plain "待決定"-badged collapsed card, muted
/// system/notification row).
pub(super) fn feed_row(state: &RootView, item: &FeedItem, cx: &mut Context<RootView>) -> Stateful<Div> {
    let locale = state.locale;
    let unread = !cx.global::<InboxState>().read_ids.contains(&item.id);
    match (&item.category, &item.approval) {
        (FeedCategory::Approval, Some(exp)) => {
            let expanded = cx.global::<InboxState>().expanded_id.as_deref() == Some(item.id.as_str());
            if expanded {
                expanded_approval_row(state, item, exp, unread, locale, cx)
            } else {
                collapsed_approval_row(item, unread, locale, cx)
            }
        }
        _ => plain_row(item, unread, locale),
    }
}

fn unread_dot(unread: bool) -> Div {
    div().mt(px(4.)).size(px(8.)).flex_shrink_0().rounded_full().when(unread, |el| el.bg(theme::alpha(theme::BRAND, 1.0)))
}

/// First ASCII/CJK letter of `agent_id`, uppercased where applicable — this
/// crate has no per-agent display-name/color mapping reachable from either
/// `approvals.list` or `activity.list` (both only ever carry the raw
/// `agent_id` slug, confirmed by reading both handlers), so the avatar is
/// an honest same-shape approximation of `nav.rs`'s own "badge_letter"
/// placeholder-icon strategy, not a real per-agent identity, glyph, or
/// color.
fn avatar_glyph(agent_id: &str) -> String {
    agent_id.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".to_string())
}

fn avatar(agent_id: &str, bg: u32) -> Div {
    div()
        .size(px(30.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme::alpha(bg, 1.0))
        .text_size(px(12.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(theme::alpha(theme::SURFACE_FOREGROUND, 1.0))
        .child(avatar_glyph(agent_id))
}

fn status_badge(badge_kind: Option<FeedBadge>, locale: Locale) -> Option<Div> {
    badge_kind.map(|b| match b {
        FeedBadge::Pending => badge(i18n::t(locale, "native.inbox.badge.pending"), BadgeVariant::Warning),
        FeedBadge::Done => badge(i18n::t(locale, "native.inbox.badge.done"), BadgeVariant::Success),
    })
}

/// `"2026-08-21T13:45:00Z"` → local `HH:MM` if today, local `MM/DD`
/// otherwise. Same convention `chat/conversation_row.rs::row_time_label`
/// already established for this crate's other feed-of-timestamped-rows
/// surface (localizing weekday abbreviations across zh-TW/en/ja-JP is out
/// of scope here too, for the same reason). Duplicated rather than
/// exported from that module — it's `screens::chat`-private, and the
/// canvas's own richer relative-time copy ("10 分鐘前", "週一") isn't
/// reproduced: this crate has no i18n plural/relative-time helper, and
/// hand-rolling one across three locales for a single page wasn't judged
/// worth it over the simpler, already-precedented HH:MM/MM:DD split.
fn format_feed_time(ts: &str) -> String {
    let Some(parsed) = DateTime::parse_from_rfc3339(ts).ok().map(|dt| dt.with_timezone(&Utc)) else {
        return String::new();
    };
    let local = parsed.with_timezone(&Local);
    if local.date_naive() == Local::now().date_naive() {
        local.format("%H:%M").to_string()
    } else {
        local.format("%m/%d").to_string()
    }
}

fn row_frame(id: SharedString, border_color: gpui::Hsla, bg_muted: bool) -> Stateful<Div> {
    let mut el = div()
        .id(id)
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .border_1()
        .border_color(border_color)
        .cursor_pointer();
    el = if bg_muted {
        el.bg(theme::alpha(theme::SURFACE, 0.7))
    } else {
        el.bg(theme::alpha(theme::SURFACE, 1.0)).shadow(theme::surface_shadow())
    };
    el
}

fn collapsed_approval_row(item: &FeedItem, unread: bool, locale: Locale, cx: &mut Context<RootView>) -> Stateful<Div> {
    let item_id = item.id.clone();
    row_frame(format!("inbox-row-{}", item.id).into(), theme::surface_border(), false)
        .child(
            div()
                .flex()
                .gap_2p5()
                .items_center()
                .child(unread_dot(unread))
                .child(avatar(&item.agent_id, theme::BRAND))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_SM))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                                        .child(format!("{} · {}", item.agent_id, item.title)),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                        .child(format_feed_time(&item.timestamp)),
                                ),
                        ),
                )
                .children(status_badge(item.badge, locale)),
        )
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            let note_field = {
                let g = cx.global_mut::<InboxState>();
                g.read_ids.insert(item_id.clone());
                g.expanded_id = Some(item_id.clone());
                g.note_field.clone()
            };
            note_field.update(cx, |field, cx| {
                field.content.clear();
                cx.notify();
            });
            cx.notify();
        }))
}

fn expanded_approval_row(
    state: &RootView,
    item: &FeedItem,
    exp: &ApprovalExpand,
    unread: bool,
    locale: Locale,
    cx: &mut Context<RootView>,
) -> Stateful<Div> {
    let item_id = item.id.clone();
    let approval_id = exp.approval_id.clone();
    let deciding = cx.global::<InboxState>().deciding_id.as_deref() == Some(approval_id.as_str());

    let mut card = div()
        .flex()
        .gap_2p5()
        .child(unread_dot(unread))
        .child(avatar(&item.agent_id, theme::BRAND))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1p5()
                .child(
                    div()
                        .text_size(px(theme::TEXT_SM))
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .text_color(theme::alpha(theme::FOREGROUND, 1.0))
                        .child(format!("{} · {}", item.agent_id, item.title)),
                )
                .child(
                    div()
                        .text_size(px(theme::TEXT_XS))
                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                        .child(kind_label(&exp.kind, locale)),
                ),
        );

    if let Some(wsc) = &exp.world_state_change {
        let mut mono = wsc.clone();
        if !exp.risk_points.is_empty() {
            mono.push('（');
            mono.push_str(&i18n::t(locale, "native.inbox.riskPrefix"));
            mono.push_str(&exp.risk_points.join("；"));
            mono.push('）');
        }
        card = card.child(
            div()
                .mt_1()
                .p_2()
                .rounded(px(theme::RADIUS_MD))
                .bg(theme::alpha(theme::SURFACE_HOVER, 0.6))
                .border_1()
                .border_color(theme::surface_border())
                .text_size(px(11.))
                .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                .child(mono),
        );
    }

    if let Some(err) = &cx.global::<InboxState>().decide_error {
        card = card.child(error_row(locale, err));
    }

    let note = cx.global::<InboxState>().note_field.clone();
    let session_tx_approve = state.session_tx.clone();
    let session_tx_reject = state.session_tx.clone();
    let approval_id_approve = approval_id.clone();
    let approval_id_reject = approval_id.clone();

    card = card.child(
        div()
            .mt_1()
            .flex()
            .items_center()
            .gap_2()
            .child(div().flex_1().child(note))
            .child(button(
                "inbox-approve",
                i18n::t(locale, if deciding { "native.inbox.deciding" } else { "native.inbox.approve" }),
                ButtonVariant::Primary,
                deciding,
                None,
                cx.listener(move |_this, _ev, _window, cx| {
                    let note_field = cx.global::<InboxState>().note_field.clone();
                    let note_text = note_field.read(cx).content.clone();
                    let g = cx.global_mut::<InboxState>();
                    g.deciding_id = Some(approval_id_approve.clone());
                    g.decide_error = None;
                    inbox::dispatch_decide(cx, session_tx_approve.clone(), approval_id_approve.clone(), true, note_text);
                    cx.notify();
                }),
            ))
            .child(button(
                "inbox-reject",
                i18n::t(locale, "native.inbox.reject"),
                ButtonVariant::Secondary,
                deciding,
                None,
                cx.listener(move |_this, _ev, _window, cx| {
                    let note_field = cx.global::<InboxState>().note_field.clone();
                    let note_text = note_field.read(cx).content.clone();
                    let g = cx.global_mut::<InboxState>();
                    g.deciding_id = Some(approval_id_reject.clone());
                    g.decide_error = None;
                    inbox::dispatch_decide(cx, session_tx_reject.clone(), approval_id_reject.clone(), false, note_text);
                    cx.notify();
                }),
            )),
    );

    row_frame(format!("inbox-row-{}", item.id).into(), theme::alpha(theme::WARNING, 0.45).into(), false)
        .child(card)
        .on_click(cx.listener(move |_this, _ev, _window, cx| {
            // Collapse on a second click of the card itself — but not when
            // the click landed on the note field or the approve/reject
            // buttons, which already have their own `on_click`/focus
            // handlers; gpui's event bubbling would otherwise also fire
            // this outer handler on every inner click. Scoped narrowly: only
            // toggle when the row is still the currently-expanded one AND
            // no decide is in flight (avoids collapsing out from under a
            // request that's still running).
            let g = cx.global_mut::<InboxState>();
            if g.expanded_id.as_deref() == Some(item_id.as_str()) && g.deciding_id.is_none() {
                g.expanded_id = None;
            }
            cx.notify();
        }))
}

fn plain_row(item: &FeedItem, unread: bool, locale: Locale) -> Stateful<Div> {
    let is_system = item.category == FeedCategory::System;
    let title_color = if is_system { theme::MUTED_FOREGROUND } else { theme::FOREGROUND };
    let border = if is_system {
        theme::alpha(theme::SURFACE_BORDER, theme::SURFACE_BORDER_ALPHA * 0.6).into()
    } else {
        theme::surface_border()
    };

    let avatar_el: Div = if is_system {
        div()
            .size(px(30.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_center()
            .rounded_full()
            .bg(theme::alpha(theme::MUTED, 1.0))
            .text_size(px(13.))
            .child("⚙")
    } else {
        avatar(&item.agent_id, theme::BRAND)
    };

    div()
        .id(format!("inbox-row-{}", item.id))
        .p_3()
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(theme::SURFACE, if is_system { 0.7 } else { 1.0 }))
        .border_1()
        .border_color(border)
        .child(
            div()
                .flex()
                .items_center()
                .gap_2p5()
                .child(unread_dot(unread))
                .child(avatar_el)
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .justify_between()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(theme::TEXT_SM))
                                        .font_weight(if is_system { gpui::FontWeight::MEDIUM } else { gpui::FontWeight::SEMIBOLD })
                                        .text_color(theme::alpha(title_color, 1.0))
                                        .child(if item.agent_id.is_empty() {
                                            item.title.clone()
                                        } else {
                                            format!("{} {}", item.agent_id, item.title)
                                        }),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(theme::alpha(theme::MUTED_FOREGROUND, 1.0))
                                        .child(format_feed_time(&item.timestamp)),
                                ),
                        ),
                )
                .children(status_badge(item.badge, locale)),
        )
}

/// `ApprovalKind::as_str()` values → i18n label — mirrors `dashboard_cards.
/// rs::status_label`'s "known set + honest fallback to the raw token"
/// shape. Not exhaustive by design (`ApprovalKind::Other(s)` covers every
/// legacy/unrecognized kind server-side already).
fn kind_label(kind: &str, locale: Locale) -> SharedString {
    let key = match kind {
        "tool_call" => "native.inbox.kind.toolCall",
        "browser_action" => "native.inbox.kind.browserAction",
        "skill_activation" => "native.inbox.kind.skillActivation",
        "strategic_plan" => "native.inbox.kind.strategicPlan",
        "agent_hire" => "native.inbox.kind.agentHire",
        "wiki_ingest" => "native.inbox.kind.wikiIngest",
        _ => return kind.to_string().into(),
    };
    i18n::t(locale, key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_feed_time_falls_back_to_empty_on_unparseable_input() {
        assert_eq!(format_feed_time("not-a-timestamp"), "");
        assert_eq!(format_feed_time(""), "");
    }

    #[test]
    fn avatar_glyph_uppercases_first_char_and_handles_empty() {
        assert_eq!(avatar_glyph("duke"), "D");
        assert_eq!(avatar_glyph("小杜"), "小");
        assert_eq!(avatar_glyph(""), "?");
    }

    #[test]
    fn kind_label_falls_back_to_the_raw_token_for_unknown_kinds() {
        assert_eq!(kind_label("some_future_kind", Locale::En).to_string(), "some_future_kind");
    }
}
