// Lock screen rendering + gpui glue — Shell-S4-lock (2026-08-22).
//
// Visual spec: `commercial/design/duduclaw-os-desktop/LockScreen.dc.html`
// (1440×900, absolute-positioned to match 1:1 — same convention `home.rs`/
// `oobe/render.rs` already establish, see those files' own header comments).
// This board has NO light/dark counterpart (unlike Home/overlay's paired
// boards) — a lock screen is conventionally its own fixed dark-navy look
// regardless of the OS theme choice (matches every OS precedent surveyed in
// `research/native-os-2026-08/shell-s4-surfaces-2026-08.md` §3: none of
// them re-theme the lock surface off the desktop's own light/dark setting).
// This module therefore does NOT thread `crate::palette::ShellPalette`
// through at all — every color below is a literal lifted straight off the
// one board that exists, same "no board precedent, no token" honesty
// `crate::palette`'s own header comment establishes for its badge-color
// gaps, just applied to an entire surface instead of one field.
//
// ── ICON-3 (2026-08-23): the board this file now follows ────────────────
// The visual spec above (`duduclaw-os-desktop/LockScreen.dc.html`) is
// SUPERSEDED for everything below the summary card by
// `commercial/design/duduclaw-lockscreen-oobe-icons/Main.dc.html`, plus five
// operator rulings that override the board where it left a question open.
// The three that land here:
//
//   ① the 56px glass circle holds a real `changes-prevent` padlock (not the
//      "鎖" glyph, and not the identity) — the identity moves to its own
//      22px letter-avatar row underneath it (`name_row`);
//   ② the two system-action buttons go BOTTOM-CENTRE (the GNOME
//      convention), not the board's own bottom-right (the Windows one);
//   ③ the power menu offers 關機 as well as 重新啟動.
//
// Plus the research's §E dark-surface legibility numbers: the clock's weight
// goes 700 → 800 and the wake-up hint becomes bold. See each call site.
//
// ── Read-only, EXCEPT for the password field and the power control ──────
// The paragraph below was written when the ONLY interactive thing here was
// the password field. ICON-3 adds a second: the bottom-centre power button
// (`system_actions_row` → `power_menu_panel`). That is not a departure from
// the researched precedent — §A.1 finds a power control on the lock surface
// to be the cross-OS norm — but it IS the one thing on this pre-auth screen
// that can end the session, so it is gated behind an explicit two-step
// confirmation (see `crate::lockscreen::PowerMenu`). The board's OTHER
// bottom-centre button, accessibility, is deliberately not drawn at all:
// see `crate::lockscreen::LOCKSCREEN_A11Y_ACTIONS`.
//
// Every OS precedent researched (§3.3) keeps the lock surface non-
// interactive outside of its own credential entry, and that is now the
// shape here too: the clock/summary card/glow blobs still have ZERO click
// targets of their own, but `unlock_prompt_rows` below renders one real
// `OobeTextField` (the SAME reusable widget OOBE's `AccountCreate`/`Network`
// steps use — see `crate::oobe::LockPasswordField`'s own doc comment for
// why it's reached through that module rather than re-derived here) once
// the prompt is revealed. Everything else about the wake-up path is
// unchanged: `main.rs`'s `Render::render` still wires unconditional
// `on_key_down`/`on_mouse_down` listeners at the WINDOW ROOT, but they now
// call `note_input_or_reveal` (renamed from the old `note_input_or_unlock`)
// — the first key/click after locking REVEALS the field and moves keyboard
// focus onto it (see that fn's own doc comment for the exact focus-handoff
// mechanics); it no longer unlocks anything by itself. Actually unlocking
// only ever happens through `dispatch_unlock_attempt` below, gated on a
// real `gateway_client::verify_password` round trip. See
// `crate::lockscreen`'s own header comment for the full reversal writeup
// and the offline fail-safe semantics that fall out of it.
//
// ── Data source: the SAME `NotificationsFeed` the Notifications overlay
// panel reads ───────────────────────────────────────────────────────────
// Per the research doc's own §3.4 recommendation ("鎖屏摘要...與通知中心
// ...三者互為同一份資料在不同 surface 的三種切片...不要各自維護一份資料模
// 型"), this module does NOT poll its own copy of pending approvals — it
// reads `ShellView.overlay_ui.notifications` (`overlay::notifications_feed::
// NotificationsFeed`) directly, and reuses `overlay::notifications::
// trigger_refresh_if_stale` (widened to `pub(crate)` this round) for its own
// 30s-while-locked refresh cadence, rather than a second poll loop.
//
// ── Honest scope gap: no "activity" (completed-work) feed exists yet ─────
// The design board's card shows BOTH completed-task lines ("小杜 完成了 客
// 服月報 草稿") and a pending-approval line. This shell has no gateway
// client method for "activity"/completed-work (only `approvals.list`/
// `approvals.decide` exist in `gateway_client`, see that module's own header
// comment) — inventing fabricated "completed" lines from `NotificationsFeed
// ::decided()` would be dishonest (those rows are approvals the OPERATOR
// personally just decided in-session, not agent task completions). This
// round's summary card therefore shows ONLY the real pending-approval count
// (+ real summaries at the `Full` privacy tier) — a deliberate, documented
// gap, not silently dropped scope. See the parent crate's report for the
// tracked follow-up.

use std::time::{Duration, Instant};

use chrono::{Datelike, Timelike};
use gpui::{div, img, linear_color_stop, linear_gradient, prelude::*, px, rgb, Context, Div, Focusable, FontWeight, Stateful, Window};

use duduclaw_native_gui::theme;

use super::{format_away_duration, LockPrivacy, LockScreenState, UnlockFailureKind, UnlockPhase};
use crate::i18n::{t, t1, Key, Locale};
use crate::oobe::LockPasswordField;
use crate::overlay::notifications_feed::{FeedStatus, NotificationsFeed};
use crate::ShellView;

/// Poll cadence while locked — reuses `overlay::notifications_feed::
/// REFRESH_STALE_AFTER` for the refresh-if-stale threshold itself (the SAME
/// 30s the Notifications panel uses, see this file's header comment), but
/// this local constant is how often the clock's own display re-paints —
/// independent of whether a gateway refresh happened, so the clock keeps
/// advancing even fully offline (task brief: "gateway 不可達時...純時鐘照
/// 常").
const CLOCK_TICK_INTERVAL: Duration = Duration::from_secs(20);

/// Idle watchdog tick — how often `spawn_idle_watchdog`'s background loop
/// re-checks `LockScreenState::is_idle_past`. Independent of, and much
/// tighter than, `crate::lockscreen::idle_after_from_env`'s own (minutes-
/// scale) threshold — this just bounds the worst-case lag between "the
/// operator went idle" and "the screen actually locks" to ~10s, cheap since
/// each tick is a plain duration comparison, no I/O.
const IDLE_WATCHDOG_INTERVAL: Duration = Duration::from_secs(10);

/// The full-screen lockscreen surface — `main.rs`'s `Render::render` makes
/// this the root's ENTIRE child while `ShellView.lockscreen.is_locked()`
/// (same "OOBE takes over the whole screen" precedent `oobe::render`
/// establishes, see `main.rs`'s own header comment for why that shape was
/// chosen over layering as another overlay).
pub(crate) fn render(
    state: &LockScreenState,
    notifications: &NotificationsFeed,
    password_field: &LockPasswordField,
    // ICON-3 (2026-08-23): the identity row's display name, read ONCE at
    // boot from the persisted OOBE state (`main.rs`'s `operator_name`
    // field — see `oobe::boot_operator_name`'s own doc comment). `None`
    // means no name is on file; the row then draws `avatar-default` and no
    // text, which is honest rather than a blank pill.
    operator_name: Option<&str>,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    schedule_clock_tick(cx);
    // The gateway-data poll (task brief's own "鎖定期間資料輪詢維持（30s 刷
    // 件數）") is the SAME single checker the Notifications panel uses, not
    // a second timer of this surface's own — see this file's header comment
    // on why this surface never maintains an independent poll of the same
    // data, and `overlay::notifications::schedule_stale_check`'s own doc
    // comment for the WP-A4-4 pile-up that a near-duplicate copy here (as
    // this file carried until 2026-08-22) helped produce. Both calls are
    // idempotent claims, so calling them from a render body is safe.
    crate::overlay::notifications::schedule_stale_check(cx);

    let privacy = super::privacy_from_env();
    let (time_text, date_text) = now_local_strings();

    let mut root = div()
        .id("lockscreen-root")
        .relative()
        .size_full()
        // LockScreen.dc.html: `linear-gradient(150deg, #1a2436 0%, #22304a
        // 45%, #2a3a58 100%)` — gpui's `linear_gradient` is 2-stop only (same
        // limitation `home.rs`'s own header comment documents), the middle
        // stop is dropped.
        .bg(linear_gradient(150.0, linear_color_stop(rgb(0x1a2436), 0.0), linear_color_stop(rgb(0x2a3a58), 1.0)))
        .child(glow_top_right())
        .child(glow_bottom_left())
        .child(cat_watermark())
        .child(clock_block(&time_text, &date_text));

    if privacy != LockPrivacy::None {
        root = root.child(summary_card(state, notifications, privacy));
    }

    // ICON-3 (2026-08-23): the whole bottom band is ONE column now.
    //
    // It used to be two independently-anchored blocks: the unlock hint /
    // password prompt at `bottom: 90` (or 70), and nothing else. This round
    // adds the board's system-action buttons at `bottom: 30`, and a popover
    // menu above them — three stacked things whose heights all change with
    // state (the prompt grows an input and up to two status lines; the menu
    // grows a confirmation). Keeping them on separate absolute anchors would
    // mean hand-tuning offsets that only happen to clear each other in the
    // states someone thought to check. One bottom-anchored flex column makes
    // non-overlap structural instead, and reproduces the board's own
    // measurements exactly in the resting state: buttons 40px tall at
    // `bottom: 30` (→ 830..870), gap 20, so the identity block's own bottom
    // edge lands at 810 — the board's `bottom: 90`.
    root = root.child(bottom_stack(state, password_field, operator_name, cx));

    root
}

/// The bottom band: identity/unlock block, the power menu when it is open,
/// and the system-action button row. See `render`'s own comment above for
/// why these three share one anchor.
fn bottom_stack(
    state: &LockScreenState,
    password_field: &LockPasswordField,
    operator_name: Option<&str>,
    cx: &mut Context<ShellView>,
) -> Div {
    let mut col = div()
        .absolute()
        .bottom(px(30.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(20.))
        .child(identity_block(state, password_field, operator_name));
    if let Some(menu) = power_menu_panel(state, cx) {
        col = col.child(menu);
    }
    col.child(system_actions_row(cx))
}

fn glow_top_right() -> Div {
    // LockScreen.dc.html: `rgba(70,110,180,0.22)`, 780×780, top:-160/right:-140.
    div().absolute().top(px(-160.)).right(px(-140.)).w(px(780.)).h(px(780.)).rounded(px(780.)).bg(theme::alpha(0x466eb4, 0.22))
}

fn glow_bottom_left() -> Div {
    // LockScreen.dc.html: `rgba(40,70,120,0.28)`, 720×720, bottom:-220/left:-120.
    div().absolute().bottom(px(-220.)).left(px(-120.)).w(px(720.)).h(px(720.)).rounded(px(720.)).bg(theme::alpha(0x284678, 0.28))
}

/// The mascot watermark — reuses `crate::home::{png, CAT_512}` (widened to
/// `pub(crate)` this round, see that file's own doc comment) rather than a
/// second `include_bytes!` of the same PNG. Same aspect-ratio derivation
/// `home::cat_hero` uses (264:512 actual PNG dimensions): board specifies
/// `height: 460px` at `top: 100px`, centered — width = 264/512 × 460 ≈
/// 237.2, left = (1440 − 237.2) / 2 ≈ 601.4 for this crate's fixed 1440px
/// dev-mode window (same "no resize handling this round" scope boundary
/// `home::cat_hero`'s own doc comment states).
fn cat_watermark() -> gpui::Img {
    img(crate::home::png(crate::home::CAT_512)).absolute().top(px(100.)).left(px(601.4)).w(px(237.2)).h(px(460.)).opacity(0.10)
}

fn clock_block(time_text: &str, date_text: &str) -> Div {
    div()
        .absolute()
        .top(px(150.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(6.))
        // ICON-3 (2026-08-23): 700 -> 800. The research's §E takes GNOME's
        // own lock-screen stylesheet as the only first-hand set of numbers
        // for a dark lock surface, and its clock is `font-weight: 800`; the
        // revised board follows it. gpui has no letter-spacing primitive at
        // the pinned rev, so the board's `-0.02em` tracking is not applied —
        // an honest gap, not a silent substitution.
        .child(div().text_size(px(92.)).font_weight(FontWeight::EXTRA_BOLD).text_color(theme::alpha(0xfafafa, 1.0)).child(time_text.to_string()))
        .child(div().text_size(px(17.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(0xfafafa, 0.75)).child(date_text.to_string()))
}

/// `chrono::Local::now()` — already in this crate's resolved dependency
/// graph (transitively via `duduclaw-native-gui` AND `gpui` itself, verified
/// with `cargo tree -p duduclaw-shell -i chrono` before adding it as a
/// direct dependency — see `Cargo.toml`'s own comment on that block, same
/// "promote an already-compiled transitive dep, zero new crates" reasoning
/// the `tokio`/`tokio-tungstenite` block already gives). Weekday names are a
/// small hardcoded zh-TW table, not routed through `chrono`'s own locale
/// support (which needs an extra crate feature this codebase doesn't carry)
/// — same "plain zh-TW literal, no i18n crate for one array" convention this
/// whole crate already follows for its design-board content.
fn now_local_strings() -> (String, String) {
    const ZH_WEEKDAYS: [&str; 7] = ["星期日", "星期一", "星期二", "星期三", "星期四", "星期五", "星期六"];
    let now = chrono::Local::now();
    let time = format!("{:02}:{:02}", now.hour(), now.minute());
    let weekday = ZH_WEEKDAYS[now.weekday().num_days_from_sunday() as usize];
    let date = format!("{}月{}日 {}", now.month(), now.day(), weekday);
    (time, date)
}

fn summary_card(state: &LockScreenState, feed: &NotificationsFeed, privacy: LockPrivacy) -> Div {
    let duration_text = format_away_duration(state.locked_at().map(|t| t.elapsed()).unwrap_or_default());
    let title = t1(Locale::ZhTw, Key::LockAwaySummaryTitle, &duration_text);
    let lines = summary_lines(feed, privacy);

    div().absolute().top(px(420.)).left(px(0.)).right(px(0.)).flex().justify_center().child(
        div()
            .w(px(460.))
            // LockScreen.dc.html also applies `backdrop-filter: blur(24px)`
            // — gpui has no backdrop-filter primitive (same gap `overlay.rs`'s
            // own header comment notes for Launcher's dimmed backdrop),
            // skipped rather than faked.
            .bg(theme::alpha(0xffffff, 0.10))
            .border_1()
            .border_color(theme::alpha(0xffffff, 0.16))
            .rounded(px(16.))
            .px(px(22.))
            .py(px(18.))
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(card_header(&title))
            .child(div().flex().flex_col().gap(px(9.)).children(lines.into_iter().map(|(dot_hex, text)| summary_line(dot_hex, text)))),
    )
}

fn card_header(title: &str) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .child(img(crate::home::png(crate::home::MARK_32)).w(px(16.)).h(px(16.)).rounded(px(8.)))
        .child(div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(0xfafafa, 0.9)).child(title.to_string()))
}

/// Neutral gray dot for the connectivity states (offline/loading) — no board
/// precedent (the board only ever shows the happy-path content), same
/// "approximate, don't invent a fourth semantic color without a source"
/// honesty `overlay/notifications.rs`'s own header comment establishes for
/// its own rejected-badge gap.
const DOT_NEUTRAL: u32 = 0x9ca3af;
const DOT_GREEN: u32 = 0x4ade80;
const DOT_AMBER: u32 = 0xfbbf24;

/// Builds the card's `(dot color, line text)` pairs from the real
/// `NotificationsFeed` — see this file's header comment for the honest scope
/// boundary (pending-approval data only, no fabricated "completed work"
/// lines). Connectivity states (Offline/Loading) take priority over any
/// count, mirroring `overlay/notifications.rs::content()`'s own three-way
/// status split — same underlying `FeedStatus`, a different UI reading it.
fn summary_lines(feed: &NotificationsFeed, privacy: LockPrivacy) -> Vec<(u32, String)> {
    match feed.status {
        FeedStatus::Offline => vec![(DOT_NEUTRAL, t(Locale::ZhTw, Key::NotifOfflineBanner).to_string())],
        FeedStatus::Idle => vec![(DOT_NEUTRAL, t(Locale::ZhTw, Key::NotifLoadingLabel).to_string())],
        FeedStatus::Loading if feed.rows().is_empty() => vec![(DOT_NEUTRAL, t(Locale::ZhTw, Key::NotifLoadingLabel).to_string())],
        FeedStatus::Loading | FeedStatus::Ready => {
            let count = feed.pending_count();
            if count == 0 {
                return vec![(DOT_GREEN, t(Locale::ZhTw, Key::NotifEmptyLabel).to_string())];
            }
            let mut lines = vec![(DOT_AMBER, t1(Locale::ZhTw, Key::LockPendingCountLabel, &count.to_string()))];
            if privacy == LockPrivacy::Full {
                // Real gateway content (`row.summary`), same "server-generated
                // prose stays a plain literal" treatment `overlay/
                // notifications.rs`'s own header comment gives this exact
                // field — capped at 2 so the card doesn't grow unbounded with
                // a large pending queue (same `MAX_DECIDED_HISTORY`-style cap
                // spirit `notifications_feed.rs` applies to its own list).
                for row in feed.rows().iter().take(2) {
                    lines.push((DOT_AMBER, row.summary.clone()));
                }
            }
            lines
        }
    }
}

fn summary_line(dot_hex: u32, text: String) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(9.))
        .child(div().w(px(8.)).h(px(8.)).rounded(px(8.)).bg(theme::alpha(dot_hex, 1.0)))
        .child(div().text_size(px(13.5)).text_color(theme::alpha(0xfafafa, 0.92)).child(text))
}

/// The lock glyph, the identity row, and then EITHER the wake-up hint or the
/// real password prompt — one block, because the board draws them as one
/// centred stack and because the transition between hint and prompt must not
/// move the glyph or the name.
fn identity_block(state: &LockScreenState, password_field: &LockPasswordField, operator_name: Option<&str>) -> Div {
    let col = div().flex().flex_col().items_center().gap(px(12.)).child(lock_glyph_circle()).child(name_row(operator_name));
    if state.prompt_visible() {
        unlock_prompt_rows(col, state, password_field)
    } else {
        // The board raised this from `rgba(250,250,250,0.55)` regular to
        // `0.62` BOLD — per the research's §E, weight is the primary
        // legibility lever for hint text on a dark surface (GNOME's own
        // `.unlock-dialog-clock-hint { font-weight: bold }`), not alpha.
        col.child(
            div()
                .text_size(px(12.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(0xfafafa, 0.62))
                .child(t(Locale::ZhTw, Key::LockUnlockHint)),
        )
    }
}

/// The 56px glass circle. ICON-3 (2026-08-23) replaces the single CJK glyph
/// ("鎖") that stood here since Shell-S4-lock: the revised board draws a real
/// 24px `changes-prevent` padlock inside it, so there IS board artwork now —
/// the earlier note that there was none was accurate against
/// `duduclaw-os-desktop/LockScreen.dc.html`, which is a different (older)
/// board. The glyph survives as the icon's `icon_or_glyph` fallback.
///
/// The operator's ruling ① settled the board's own open question here:
/// padlock in the circle, and the identity moves to its own row below (see
/// `name_row`) rather than competing for this slot.
fn lock_glyph_circle() -> Div {
    div()
        .w(px(56.))
        .h(px(56.))
        .rounded(px(28.))
        .bg(theme::alpha(0xffffff, 0.14))
        .border_1()
        .border_color(theme::alpha(0xffffff, 0.2))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(20.))
        .font_weight(FontWeight::BOLD)
        .text_color(theme::alpha(0xfafafa, 1.0))
        .child(crate::icons::icon_or_glyph(&[(crate::icons::LOCK_CLOSED, 0xfafafa)], 24., "鎖"))
}

/// The identity row — ICON-3 (2026-08-23), the operator's ruling ①: a 22px
/// letter avatar followed by the name, in the same visual vocabulary the
/// dock's agent avatars already use (a circular tile, the first character,
/// bold, in the tile's own foreground).
///
/// With no name on file the row degrades to the board's own `avatar-default`
/// (L7: 「取不到名字首字母時才用」) and NO name text — this shell must not
/// invent a name for a machine that was never given one.
fn name_row(operator_name: Option<&str>) -> Div {
    let initial = operator_name.and_then(first_initial);
    let mut row = div().flex().items_center().gap(px(8.));

    row = row.child(match initial {
        Some(letter) => div()
            .w(px(22.))
            .h(px(22.))
            .rounded(px(11.))
            .bg(theme::alpha(0xffffff, 0.18))
            .flex()
            .items_center()
            .justify_center()
            .text_size(px(11.))
            .font_weight(FontWeight::BOLD)
            .text_color(theme::alpha(0xfafafa, 1.0))
            .child(letter)
            .into_any_element(),
        None => crate::icons::icon_or_none(&[(crate::icons::AVATAR_DEFAULT, 0xfafafa)], 22.).unwrap_or_else(|| div().into_any_element()),
    });

    if let Some(name) = operator_name {
        row = row.child(
            div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(0xfafafa, 0.92)).child(name.to_string()),
        );
    }
    row
}

/// The avatar's letter: the name's first CHARACTER, upper-cased.
///
/// Deliberately `chars().next()`, never `&name[..1]` — this crate's coding
/// convention 1 (a byte slice panics mid-codepoint, and an operator name is
/// very often CJK here). `to_uppercase()` is the Unicode-correct form and
/// can yield more than one char for a few Latin letters (ß → SS); those are
/// kept whole rather than truncated, since a 22px tile showing "SS" is
/// merely wide, while a half-uppercased letter would be wrong. CJK is
/// unaffected — uppercasing a Han character is the identity.
///
/// `None` when the name has no characters at all, which is the same signal
/// `oobe::boot_operator_name` already filters for; belt-and-braces, because
/// the fallback path has to exist anyway for `operator_name == None`.
fn first_initial(name: &str) -> Option<String> {
    name.chars().next().map(|c| c.to_uppercase().to_string())
}

/// The password prompt's own rows, appended to the identity block once the
/// prompt is revealed (WP-lock-pw, 2026-08-22; re-parented into
/// `identity_block` by ICON-3 — see that fn's own doc comment).
fn unlock_prompt_rows(col: Div, state: &LockScreenState, password_field: &LockPasswordField) -> Div {
    let now = Instant::now();
    let mut col = col.child(div().w(px(240.)).child(password_field.field.clone()));

    match state.unlock_phase() {
        UnlockPhase::Idle => {}
        UnlockPhase::InFlight => col = col.child(prompt_hint_line(t(Locale::ZhTw, Key::LockVerifyingLabel))),
        UnlockPhase::Failed(UnlockFailureKind::WrongPassword) => col = col.child(prompt_error_line(t(Locale::ZhTw, Key::LockPasswordWrongError))),
        // See `crate::lockscreen`'s own header comment ("Offline fail-safe
        // semantics") for why this is the SAME honest message regardless of
        // which underlying `gateway_client::LoginError` variant it was —
        // offline, timeout, unexpected status, or the gateway's own rate
        // limit all read as "本機服務未回應" to the operator, who has no
        // actionable way to tell those apart anyway.
        UnlockPhase::Failed(UnlockFailureKind::Unreachable) => col = col.child(prompt_error_line(t(Locale::ZhTw, Key::LockOfflineError))),
    }
    if state.is_throttled(now) {
        col = col.child(prompt_hint_line(t(Locale::ZhTw, Key::LockThrottledLabel)));
    }
    col
}

/// Small neutral-gray status line (verifying/throttled) — distinct from
/// `prompt_error_line` below by color only, same "same shape, different
/// color token" split `summary_line`'s own dot-color parameter establishes
/// elsewhere in this file.
fn prompt_hint_line(text: &str) -> Div {
    div().text_size(px(12.)).text_color(theme::alpha(0xfafafa, 0.6)).child(text.to_string())
}

/// `theme::dark::DESTRUCTIVE` — this surface is always the fixed dark-navy
/// look (see this file's header comment on why `ShellPalette` is never
/// threaded through here), so the DARK variant is the only one that's ever
/// correct, not a light/dark choice that got hardcoded by accident.
fn prompt_error_line(text: &str) -> Div {
    div().text_size(px(12.)).text_color(theme::alpha(theme::dark::DESTRUCTIVE, 1.0)).child(text.to_string())
}

// ── System actions: the bottom-centre glass buttons ─────────────────────
// ICON-3 (2026-08-23). The board drew these bottom-RIGHT (the Windows
// login-screen convention); the operator's ruling ② moved them to a
// bottom-CENTRE group (the GNOME convention), which is what this renders.

/// The 40px glass button group. Today it has exactly ONE button — power.
/// The board's second button (accessibility) is deliberately not drawn:
/// see `crate::lockscreen::LOCKSCREEN_A11Y_ACTIONS`' own doc comment for
/// the "which switch could it flip?" audit that came back empty, and the
/// test that will fail the moment that stops being true.
fn system_actions_row(cx: &mut Context<ShellView>) -> Div {
    let mut row = div().flex().items_center().justify_center().gap(px(10.));
    if !super::LOCKSCREEN_A11Y_ACTIONS.is_empty() {
        // Intentionally unreachable today. Left as a real branch, not a
        // `todo!()`, so adding the first action is a one-line change here
        // rather than a rewrite — and so this row's geometry (a two-button
        // group, gap 10, exactly as the board draws it) is already correct
        // when it happens.
        row = row.child(glass_button("lock-a11y-button", crate::icons::ACCESSIBILITY, "無", |_view, _cx| {}, cx));
    }
    row.child(glass_button(
        "lock-power-button",
        crate::icons::POWER,
        "電",
        |view, cx| {
            view.lockscreen.toggle_power_menu();
            cx.notify();
        },
        cx,
    ))
}

/// One 40px circular glass button — `rgba(255,255,255,0.12)` on a
/// `rgba(255,255,255,0.2)` hairline, a 20px `#fafafa` stroke icon inside.
/// Board measurements, unchanged from the bottom-right group it replaces.
fn glass_button(
    id: &'static str,
    icon: &'static str,
    fallback_glyph: &'static str,
    on_click: impl Fn(&mut ShellView, &mut Context<ShellView>) + 'static,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let listener = cx.listener(move |view, _ev, _window, cx| on_click(view, cx));
    div()
        .id(id)
        .cursor_pointer()
        .w(px(40.))
        .h(px(40.))
        .rounded(px(20.))
        .bg(theme::alpha(0xffffff, 0.12))
        .border_1()
        .border_color(theme::alpha(0xffffff, 0.2))
        .hover(|style| style.bg(theme::alpha(0xffffff, 0.20)))
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(15.))
        .font_weight(FontWeight::BOLD)
        .text_color(theme::alpha(0xfafafa, 1.0))
        .child(crate::icons::icon_or_glyph(&[(icon, 0xfafafa)], 20., fallback_glyph))
        .on_click(listener)
}

/// The power popover — `None` while the menu is closed, so `bottom_stack`
/// simply doesn't add a child and the button row sits directly under the
/// identity block.
///
/// Four visual states, one per `PowerMenu` variant (see that enum's own doc
/// comment for the transitions). The confirmation is a real second step, not
/// a styling flourish: a single click anywhere in here never reaches the
/// gateway.
fn power_menu_panel(state: &LockScreenState, cx: &mut Context<ShellView>) -> Option<Div> {
    use crate::gateway_client::PowerAction;
    use super::{PowerFailure, PowerMenu};

    let panel = |body: Div| {
        div()
            .w(px(240.))
            .bg(theme::alpha(0xffffff, 0.10))
            .border_1()
            .border_color(theme::alpha(0xffffff, 0.16))
            .rounded(px(14.))
            .p(px(8.))
            .flex()
            .flex_col()
            .gap(px(4.))
            .child(body)
    };

    match state.power_menu() {
        PowerMenu::Closed => None,
        PowerMenu::Open => Some(panel(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(power_action_row(PowerAction::Reboot, cx))
                .child(power_action_row(PowerAction::Shutdown, cx)),
        )),
        PowerMenu::Confirming(action) => Some(panel(confirm_body(action, cx))),
        PowerMenu::Sending(_) => Some(panel(div().px(px(8.)).py(px(8.)).child(prompt_hint_line(t(Locale::ZhTw, Key::LockPowerSending))))),
        PowerMenu::Failed(failure) => {
            let key = match failure {
                PowerFailure::NoAnswer => Key::LockPowerFailed,
                PowerFailure::Unsupported => Key::LockPowerUnsupported,
            };
            Some(panel(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .px(px(8.))
                    .py(px(8.))
                    .child(prompt_error_line(t(Locale::ZhTw, key)))
                    .child(power_text_button("lock-power-dismiss", t(Locale::ZhTw, Key::NetworkCancelButton), cx)),
            ))
        }
    }
}

/// One row of the two-item menu. Clicking it only ARMS the confirmation —
/// `LockScreenState::arm_power_confirm`, never `begin_power`.
fn power_action_row(action: crate::gateway_client::PowerAction, cx: &mut Context<ShellView>) -> Stateful<Div> {
    use crate::gateway_client::PowerAction;
    let (id, key) = match action {
        PowerAction::Reboot => ("lock-power-reboot", Key::LockPowerRestart),
        PowerAction::Shutdown => ("lock-power-shutdown", Key::LockPowerShutdown),
    };
    let listener = cx.listener(move |view, _ev, _window, cx| {
        view.lockscreen.arm_power_confirm(action);
        cx.notify();
    });
    div()
        .id(id)
        .cursor_pointer()
        .px(px(10.))
        .py(px(8.))
        .rounded(px(9.))
        .hover(|style| style.bg(theme::alpha(0xffffff, 0.12)))
        .text_size(px(13.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(0xfafafa, 0.92))
        .child(t(Locale::ZhTw, key))
        .on_click(listener)
}

/// The second step: the question, then 取消 / 確定. `確定` is the ONLY thing
/// in this file that reaches the gateway.
fn confirm_body(action: crate::gateway_client::PowerAction, cx: &mut Context<ShellView>) -> Div {
    use crate::gateway_client::PowerAction;
    let question = match action {
        PowerAction::Reboot => Key::LockPowerConfirmRestart,
        PowerAction::Shutdown => Key::LockPowerConfirmShutdown,
    };
    let confirm = cx.listener(move |view, _ev, _window, cx| dispatch_power_action(view, action, cx));
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .px(px(8.))
        .py(px(8.))
        .child(div().text_size(px(12.5)).text_color(theme::alpha(0xfafafa, 0.92)).child(t(Locale::ZhTw, question)))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(power_text_button("lock-power-cancel", t(Locale::ZhTw, Key::NetworkCancelButton), cx))
                .child(
                    div()
                        .id("lock-power-confirm")
                        .cursor_pointer()
                        .px(px(12.))
                        .py(px(6.))
                        .rounded(px(8.))
                        .bg(theme::alpha(0xffffff, 0.22))
                        .hover(|style| style.bg(theme::alpha(0xffffff, 0.30)))
                        .text_size(px(12.5))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::alpha(0xfafafa, 1.0))
                        .child(t(Locale::ZhTw, Key::NotifConfirmButton))
                        .on_click(confirm),
                ),
        )
}

/// A low-key "back out" text button inside the popover — 取消 next to a
/// confirmation, and the dismiss under a failure message.
///
/// Both go to `cancel_power_confirm`, i.e. back to the two-item MENU rather
/// than closing the popover outright: the operator opened the power menu on
/// purpose, and changing their mind about WHICH action is not the same as
/// changing their mind about the menu. Closing it is what the power button
/// itself does.
fn power_text_button(id: &'static str, label: &'static str, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let listener = cx.listener(move |view, _ev, _window, cx| {
        view.lockscreen.cancel_power_confirm();
        cx.notify();
    });
    div()
        .id(id)
        .cursor_pointer()
        .px(px(12.))
        .py(px(6.))
        .rounded(px(8.))
        .hover(|style| style.bg(theme::alpha(0xffffff, 0.12)))
        .text_size(px(12.5))
        .text_color(theme::alpha(0xfafafa, 0.7))
        .child(label)
        .on_click(listener)
}

/// Sends one confirmed power request. Same "background thread + `std::sync::
/// mpsc` + a `cx.spawn` poll loop" bridge `dispatch_unlock_attempt` above
/// already uses (and `oobe/steps/account.rs` established for this crate) —
/// `gateway_client::power_local` is a plain blocking call and gpui's main
/// thread must never block on I/O.
///
/// The `begin_power` guard is the authoritative one: it refuses unless the
/// state is exactly `Confirming(action)`, so a click that somehow arrived
/// out of order sends nothing.
///
/// SUCCESS IS NEVER RENDERED, deliberately. A gateway that accepts a reboot
/// is about to take this process down with it; there is no "done" for the
/// operator to read, and drawing one would be a claim this surface cannot
/// stand behind. Only failures produce UI.
fn dispatch_power_action(view: &mut ShellView, action: crate::gateway_client::PowerAction, cx: &mut Context<ShellView>) {
    if !view.lockscreen.begin_power(action) {
        return;
    }
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(crate::gateway_client::power_local(action));
    });

    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    if let Err(e) = result {
                        // Detail to stderr only; the operator-facing text is
                        // one of two honest messages (see `PowerFailure`).
                        eprintln!("[lockscreen] power_local({action:?}) failed: {e:?}");
                        let failure = match e {
                            crate::gateway_client::PowerError::Unsupported(_) => super::PowerFailure::Unsupported,
                            crate::gateway_client::PowerError::Failed(_) => super::PowerFailure::NoAnswer,
                        };
                        view.lockscreen.settle_power_failure(failure);
                        cx.notify();
                    }
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(Duration::from_millis(50)).await;
    })
    .detach();
}

// ── Trigger / unlock entry points (called from `main.rs`) ─────────────────

/// Locks the screen, dismisses whatever Home overlay was open (a lock should
/// not leave a stale panel state that reappears the instant the operator
/// unlocks), and kicks off the same stale-feed refresh
/// `overlay::notifications::open_and_refresh` uses for its own panel —
/// shared entry point for BOTH the manual lock action (`main.rs`'s
/// `on_lock_now`, ControlCenter's own lock button — see
/// `overlay/controlcenter.rs`'s own doc comment) and the idle watchdog
/// (`maybe_auto_lock` below), so both paths behave identically.
pub(crate) fn lock_and_refresh(view: &mut ShellView, cx: &mut Context<ShellView>) {
    let was_locked = view.lockscreen.is_locked();
    view.lockscreen.lock();
    // D9-bug4 (2026-08-24): tell the compositor, or the lock screen paints
    // UNDER every application window (it lives on the `Background` layer) and
    // the operator sees Chromium on a machine they just locked. Announced on
    // the false -> true edge only: this is also the auto-lock watchdog's
    // entry point, and it must not spawn a socket thread every tick once the
    // screen is already locked. See `crate::notify_comp_session_locked`.
    if !was_locked {
        crate::notify_comp_session_locked(true);
    }
    view.surface.close();
    // D3-b (2026-08-23): closing the Launcher this way must also drop
    // whatever was typed into its search box, exactly as every other close
    // path does (`ShellView::settle_launcher_query`). Only the CLEAR half is
    // possible here — `lock_and_refresh` has no `&mut Window`, and the focus
    // half is handled anyway: `reveal_and_focus` moves focus onto the
    // password field on the first keystroke after locking.
    view.launcher_query_field.field.update(cx, |field, cx| field.clear(cx));
    // D9-bug9 (2026-08-24), M1 round: the password field gets the SAME
    // unconditional clear the Launcher's search box gets just above — it
    // used to get NONE. `LockPasswordField` is created ONCE at window-open
    // time and reused for the shell's entire process lifetime (`main.rs`'s
    // own field doc comment); before this round the only path that ever
    // cleared it was a SUCCESSFUL submit (`dispatch_unlock_attempt` below),
    // so anything left behind by a lock cycle that never reached a clean
    // submit — a throttled/in-flight attempt the operator gave up on, or
    // (the M1 evidence) a flood of synthetic repeat characters from a
    // `cmd-l` chord's release racing the lock transition (see
    // `duduclaw-comp::session_lock::DuduclawComp::force_keyboard_leave_
    // enter_cycle`'s own doc comment for that mechanism) — sat there
    // silently and reappeared the INSTANT the next lock's prompt was
    // revealed, before the operator had typed a single character. Clearing
    // here makes every lock start from a guaranteed-empty field regardless
    // of how the PREVIOUS lock cycle ended, the same guarantee `lock()`
    // already gives `unlock_prompt`/`power` (see `LockScreenState::lock`'s
    // own doc comment) — this is the missing third piece of that same
    // "guaranteed-clean slate" contract, just one layer up (gpui state, not
    // `LockScreenState`'s own plain data). This does NOT by itself stop a
    // repeat flood that is still actively happening at THIS exact instant
    // (only `force_keyboard_leave_enter_cycle` on the compositor side does
    // that) — it closes the SEPARATE "leftover from last time" hole a live
    // flood-suppression fix cannot.
    view.lockscreen_password_field.field.update(cx, |field, cx| field.clear(cx));
    crate::overlay::notifications::trigger_refresh_if_stale(view, cx);
    cx.notify();
}

/// Settles a SUCCESSFUL verify — the only path that actually clears
/// `LockScreenState.locked` (see `dispatch_unlock_attempt`/
/// `apply_unlock_result` below for the credential-gated caller; the
/// `DUDUCLAW_SHELL_LOCK_NO_PASSWORD=1` escape hatch also lands here
/// directly). A no-op (not a panic, not a redundant `cx.notify()`) when
/// already unlocked.
pub(crate) fn unlock(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.lockscreen.is_locked() {
        return;
    }
    view.lockscreen.unlock();
    // D9-bug4: the mirror of `lock_and_refresh`'s announcement. The early
    // return above already makes this the true -> false edge, so the
    // compositor is told exactly once per real unlock.
    crate::notify_comp_session_locked(false);
    cx.notify();
}

/// Shared body for `main.rs`'s root-level `on_key_down`/`on_mouse_down`
/// catch-all listeners (wired unconditionally, not gated behind `DUDUCLAW_
/// SHELL_DIAG` — see that file's own header comment on why those two raw
/// listeners are SEPARATE from its pre-existing diagnostic-only pair, gpui's
/// `key_down_listeners`/`mouse_down_listeners` being `Vec`s that accumulate
/// rather than overwrite, confirmed against the pinned gpui rev's own
/// `elements/div.rs` before relying on it).
///
/// WP-lock-pw (2026-08-22): renamed from `note_input_or_unlock` — while
/// locked, this NO LONGER unlocks anything. It only reveals the password
/// prompt (task brief: "任意鍵/點擊 → 出現密碼輸入框"), and — exactly on
/// the Hidden -> visible edge (`LockScreenState::reveal_prompt`'s own `bool`
/// return) — moves keyboard focus onto the freshly-rendered field via
/// `Focusable::focus_handle`, so the very NEXT keystroke lands as typed
/// content in the field instead of hitting this same catch-all again. Once
/// already visible, this is a pure no-op: clicking/typing on the
/// BACKGROUND (not on the field itself) neither re-focuses nor clears
/// anything — the field's own `OobeTextField::on_mouse_down` handles
/// click-to-focus if the operator clicks directly on it.
///
/// This is NOT called for `cmd-k`/`escape`/`enter` specifically, since a
/// keystroke that matches a bound `KeyBinding` is fully consumed by gpui's
/// action-dispatch path and never reaches a raw `on_key_down` listener on
/// the SAME element at all (verified against `Window::dispatch_key_event`'s
/// own control flow in the pinned gpui rev: a matched binding's action
/// handler running with `cx.propagate_event` left `false` returns BEFORE
/// `dispatch_key_down_up_event` — the raw key-listener pass — ever runs).
/// `main.rs`'s `on_toggle_launcher`/`on_close_overlay` each carry their OWN
/// identical lock-check (calling THIS fn, same reveal-only behavior — cmd-k/
/// escape have no other meaning while locked); `on_oobe_next` (`enter`)
/// instead calls `submit_or_reveal` below, since Enter is this surface's
/// SUBMIT trigger once the field is already visible and focused — a plain
/// letter keystroke typed while the field has focus still ALSO reaches this
/// catch-all after bubbling past the field's own (non-propagation-stopping)
/// `OobeTextField::on_key_down`, but by then `reveal_prompt()` is a no-op
/// (already visible), so it changes nothing.
pub(crate) fn note_input_or_reveal(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if view.lockscreen.is_locked() {
        reveal_and_focus(view, window, cx);
        return;
    }
    view.lockscreen.note_input();
}

/// The reveal-and-focus operation shared by `note_input_or_reveal` above and
/// `submit_or_reveal` below (Enter, on a not-yet-revealed prompt, is "just
/// another key" per the task brief — see that fn's own doc comment).
fn reveal_and_focus(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !view.lockscreen.reveal_prompt() {
        return;
    }
    let handle = view.lockscreen_password_field.field.read(cx).focus_handle(cx);
    // D9-bug FIX (2026-08-23, root-caused on the appliance VM): the focus call
    // is DEFERRED to the next frame, and that deferral is the entire fix.
    //
    // `reveal_prompt()` above is what makes the password field render at all.
    // Until the frame that follows it, the field's element is NOT in this
    // window's element tree — and gpui silently drops `Window::focus` for a
    // handle no live element is tracking. So the direct call that used to be
    // here looked correct, logged nothing, and left focus on the root.
    //
    // Measured, not guessed: with `DUDUCLAW_SHELL_DIAG=1`, typing after the
    // reveal produced three `[probe] os key_down: Keystroke { key: "a",
    // key_char: Some("a") }` lines on the ROOT element's handler — i.e. the
    // keystrokes arrived intact and bubbled straight past the field, which is
    // exactly what an unfocused field looks like. Clicking the field directly
    // (its own element, by then mounted) made typing work immediately,
    // confirming the element-not-yet-mounted timing rather than anything
    // about the compositor, the keymap, or fcitx5 — all three were ruled out
    // first (comp delivered the keys; killing fcitx5 changed nothing).
    //
    // `on_next_frame` runs after the frame `cx.notify()` below schedules, so
    // the element is guaranteed to exist by then. The keystroke that caused
    // the reveal is still not inserted — that is the intended "press any key
    // to wake" behaviour, unchanged.
    //
    // CORRECTION to the mechanism above (D9-bug2, 2026-08-23) — the deferral
    // stays, the EXPLANATION was wrong. gpui does NOT drop a `Window::focus`
    // for an unmounted element: `Window::focus` only assigns `self.focus`
    // (`gpui/src/window.rs`, pinned rev), `FocusHandle::is_focused` is a plain
    // `window.focus == Some(id)` comparison, and the next frame's paint pass
    // installs the field's `EntityInputHandler` because that comparison is
    // already true by then. Measured on macOS with an in-app keystroke driver
    // (`Window::dispatch_keystroke`, which reproduces the platform's own
    // text-insert fallback): focus-then-mount types correctly on the very
    // first keystroke after the mount, with NO deferral. So whatever the
    // appliance measurement above caught, it was not this — the likeliest
    // candidate is the compositor-side keyboard-focus hole D9-bug2 fixed
    // (`duduclaw-comp::settle_layer_keyboard_focus`), which could leave the
    // whole client unfocused regardless of which element gpui believed was
    // focused. Left in place because it is harmless, cheap and verified
    // working end-to-end; do not "restore" the direct call on the strength of
    // the corrected reading alone.
    window.on_next_frame(move |window, cx| {
        window.focus(&handle, cx);
    });
    cx.notify();
}

/// `main.rs`'s `on_oobe_next` (bound to `enter`, `None` key-context — see
/// that binding's own registration in `main.rs`) calls this while locked,
/// REPLACING the old direct `unlock(view, cx)` call. Enter reaches the
/// currently-focused element (the password field, once revealed) BEFORE
/// bubbling up to this global binding — see `note_input_or_reveal`'s own
/// doc comment for the exact bubble order this relies on — so by the time
/// this fires, `view.lockscreen_password_field.field`'s `content` already
/// holds whatever the operator just typed.
pub(crate) fn submit_or_reveal(view: &mut ShellView, window: &mut Window, cx: &mut Context<ShellView>) {
    if !view.lockscreen.prompt_visible() {
        // First Enter press on a freshly-locked screen — same reveal-only
        // behavior as any other key; nothing has been typed yet for there
        // to be anything to submit.
        reveal_and_focus(view, window, cx);
        return;
    }
    dispatch_unlock_attempt(view, cx);
}

/// The credential-gated unlock attempt — reads the typed password, applies
/// the throttle/in-flight guard, and (unless `DUDUCLAW_SHELL_LOCK_NO_
/// PASSWORD=1` is set — see `lockscreen::password_required_from_env`'s own
/// doc comment) dispatches a real `gateway_client::verify_password` round
/// trip on a background thread, bridged back via `std::sync::mpsc` + a
/// `cx.spawn` poll loop — the exact "thread + channel + poll" shape
/// `oobe/steps/account.rs`'s own `create_click` handler already establishes
/// for the OOBE claim flow (see that file's own header comment); re-derived
/// here rather than shared for the same "small independent modules" reason
/// `gateway_client/mod.rs`'s own header comment gives for its sibling
/// clients.
fn dispatch_unlock_attempt(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !super::password_required_from_env() {
        // Dev/headless escape hatch — reproduces the ORIGINAL Shell-S4-lock
        // behavior verbatim: unlocks immediately, no gateway round trip.
        // Whatever (if anything) was typed is discarded unread, never sent
        // anywhere.
        view.lockscreen_password_field.field.update(cx, |f, cx| f.clear(cx));
        unlock(view, cx);
        return;
    }

    let now = Instant::now();
    if !view.lockscreen.can_submit(now) {
        // Throttled or already in flight — a no-op keypress, not an error:
        // the field keeps whatever was typed so the operator doesn't have
        // to retype during the cooldown (task brief: "不鎖死").
        return;
    }
    let password = view.lockscreen_password_field.field.read(cx).content(cx);
    if password.is_empty() {
        // Enter on an empty field is a no-op, not a wasted round trip or a
        // spurious "wrong password" flash.
        return;
    }
    view.lockscreen.begin_verify(now);
    // Clear the field's typed content IMMEDIATELY, before the round trip
    // even starts — task brief: "密碼零落 log／不留明文於狀態". The
    // password this attempt is verifying is already captured in `password`
    // above (moved into the background thread below); there is no reason
    // for the plaintext to keep sitting in the `OobeTextField`'s own
    // `content` for however long the round trip takes.
    view.lockscreen_password_field.field.update(cx, |f, cx| f.clear(cx));
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = crate::gateway_client::verify_password(&password);
        // `password` is dropped right here at thread exit — nothing holds
        // it past this call, and it is never written to a log (this thread
        // has no logging of its own; `apply_unlock_result`'s own stderr
        // diagnostic for a failure logs only the `LoginError` variant, never
        // the password).
        let _ = tx.send(result);
    });

    // Same one-shot poll shape as `oobe/steps/account.rs`'s own
    // `create_click` handler — see that file's own comment on this exact
    // `try_recv` + paced `background_executor().timer` loop.
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(result) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_unlock_result(view, result, cx);
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(Duration::from_millis(50)).await;
    })
    .detach();
}

/// Applies a settled `gateway_client::verify_password` result — the
/// weak-entity update body run from `dispatch_unlock_attempt`'s poll loop
/// above. Only `InvalidCredentials` maps to `UnlockFailureKind::
/// WrongPassword`; every other `LoginError` variant collapses to
/// `UnlockFailureKind::Unreachable` (see that type's own doc comment and
/// `crate::lockscreen`'s header comment "Offline fail-safe semantics" for
/// why that collapse is the honest choice, not a laziness shortcut).
fn apply_unlock_result(view: &mut ShellView, result: Result<(), crate::gateway_client::LoginError>, cx: &mut Context<ShellView>) {
    match result {
        Ok(()) => unlock(view, cx),
        Err(crate::gateway_client::LoginError::InvalidCredentials) => {
            view.lockscreen.record_failure(UnlockFailureKind::WrongPassword, Instant::now());
        }
        Err(other) => {
            // Diagnostic detail (which of Unreachable/RateLimited/Http/
            // Malformed/NonLoopback happened) goes to stderr only — the
            // operator-facing message collapses all of these to one honest
            // "本機服務未回應" (see this fn's own doc comment).
            eprintln!("[lockscreen] unlock verify failed: {other:?}");
            view.lockscreen.record_failure(UnlockFailureKind::Unreachable, Instant::now());
        }
    }
}

// ── Background timers ──────────────────────────────────────────────────

/// The ONE clock-advance timer — a pure repaint (no data fetch) so the
/// clock keeps ticking even when the gateway is unreachable (task brief:
/// "gateway 不可達時...純時鐘照常").
///
/// WP-A4-4 (2026-08-22): this used to arm a fresh one-shot timer on every
/// render pass and rely on repaints to re-arm it — while itself being a
/// pure repaint trigger. Every repaint caused by anything else (the
/// approvals poll settling, an unlock attempt, a summary-card update)
/// therefore added another permanent tick to the pile, and each of those
/// added more. On the real 值班機 that ran unattended for 5h48m and ended
/// with `cage` — whose only client is this shell — burning ~100% CPU on a
/// completely static screen. Now: claim one slot (`LockScreenState::
/// try_arm_clock_timer`), then loop internally; the loop exits and releases
/// the slot the moment the screen is no longer locked, so an unlocked Home
/// costs nothing and the next lock re-arms exactly one.
fn schedule_clock_tick(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| {
        let claimed = weak.update(cx, |view, _cx| view.lockscreen.try_arm_clock_timer()).unwrap_or(false);
        if !claimed {
            return;
        }
        loop {
            cx.background_executor().timer(CLOCK_TICK_INTERVAL).await;
            let keep_ticking = weak.update(cx, |view, cx| {
                if !view.lockscreen.is_locked() {
                    view.lockscreen.disarm_clock_timer();
                    return false;
                }
                cx.notify();
                true
            });
            match keep_ticking {
                Ok(true) => {}
                // `Ok(false)` already released the slot; `Err` means the
                // view (and its state) is gone.
                Ok(false) | Err(_) => return,
            }
        }
    })
    .detach();
}

/// The idle-auto-lock watchdog — UNLIKE `schedule_clock_tick`/`schedule_
/// stale_check` above (which only keep re-arming themselves while the
/// lockscreen surface is actually being rendered, i.e. already locked), this
/// one must run continuously from BOOT regardless of what's currently on
/// screen, since its whole job is detecting "nothing has been rendering /
/// interacting for a while" and locking in response — there is no "this
/// surface's own render pass" to piggyback re-arming on. Started exactly
/// once, from `main()`, right after the window opens (same call site style
/// as that fn's other one-time `window.update(...)` calls) — an internal
/// `loop` (this crate's one other precedent for a self-looping `cx.spawn`,
/// see `overlay/notifications.rs`'s `trigger_refresh_if_stale`'s own poll
/// bridge) that ticks forever until the view itself is dropped (`weak.
/// update` returning `Err` on a closed window, at which point the loop exits
/// rather than spinning against a dead entity).
pub(crate) fn spawn_idle_watchdog(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| loop {
        cx.background_executor().timer(IDLE_WATCHDOG_INTERVAL).await;
        if weak.update(cx, maybe_auto_lock).is_err() {
            break;
        }
    })
    .detach();
}

/// One watchdog tick's decision — no-ops during OOBE (a fresh out-of-box
/// machine locking itself mid-setup makes no sense) and while already
/// locked (idempotent either way via `lock_and_refresh` -> `LockScreenState
/// ::lock`, but short-circuiting here avoids a pointless `trigger_refresh_
/// if_stale` dispatch on every idle tick once already locked), and entirely
/// skips the check when `idle_after_from_env` reports auto-lock disabled
/// (`0` — see that fn's own doc comment).
fn maybe_auto_lock(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.oobe.is_some() || view.lockscreen.is_locked() {
        return;
    }
    let Some(idle_after) = super::idle_after_from_env() else {
        return;
    };
    if view.lockscreen.is_idle_past(idle_after) {
        lock_and_refresh(view, cx);
    }
}

// No gpui-free logic lives in this file to unit-test directly (every fn here
// either builds gpui elements or spawns a `Context<ShellView>` task) — the
// testable surface (`LockScreenState`, env parsing, duration formatting)
// lives in the parent `mod.rs` and is exercised there. Same "no `#[cfg(test)]
// mod tests` here" precedent `overlay/controlcenter.rs` already sets for the
// identical reason (its own state lives in `overlay::OverlayUiState` instead).
