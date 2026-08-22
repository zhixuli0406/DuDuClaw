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
// ── Read-only, EXCEPT for the password field itself (WP-lock-pw reversal) ─
// Every OS precedent researched (§3.3) keeps the lock surface non-
// interactive outside of its own credential entry, and that is now the
// shape here too: the clock/summary card/glow blobs still have ZERO click
// targets of their own, but `unlock_prompt_panel` below renders one real
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

    // WP-lock-pw: the hint ("按任意鍵或點擊以輸入密碼") only makes sense
    // BEFORE the field exists — once revealed, `unlock_prompt_panel` takes
    // over the same bottom-of-screen real estate.
    root = if state.prompt_visible() { root.child(unlock_prompt_panel(state, password_field)) } else { root.child(unlock_hint()) };

    if privacy != LockPrivacy::None {
        root = root.child(summary_card(state, notifications, privacy));
    }

    root
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
        .child(div().text_size(px(92.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(0xfafafa, 1.0)).child(time_text.to_string()))
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

fn unlock_hint() -> Div {
    div()
        .absolute()
        .bottom(px(90.))
        .left(px(0.))
        .right(px(0.))
        .flex()
        .flex_col()
        .items_center()
        .gap(px(12.))
        .child(lock_glyph_circle())
        .child(div().text_size(px(12.)).text_color(theme::alpha(0xfafafa, 0.55)).child(t(Locale::ZhTw, Key::LockUnlockHint)))
}

/// A single CJK glyph ("鎖", lock) stands in for a real lock icon. ICON-1
/// (2026-08-22) gave this crate real vector icons (`crate::icons`) but did
/// NOT change this slot: `LockScreen.dc.html` — the approved board for this
/// very surface — contains no SVG at all, so there is no board artwork to
/// draw here and inventing one would be this file deciding a design
/// question. Left as the honest placeholder, listed as a gap.
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
        .child(div().text_size(px(20.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(0xfafafa, 1.0)).child("鎖"))
}

/// WP-lock-pw (2026-08-22) — replaces `unlock_hint()` once the password
/// prompt is revealed, at the same bottom-of-screen position (`unlock_hint`
/// sits at `bottom(px(90.))`; this panel starts a little higher to leave
/// room for the input box itself, still well clear of the summary card
/// above it at `top(px(420.))`). Keeps the same lock glyph as a visual
/// anchor so the transition from "hint" to "prompt" doesn't jump the whole
/// layout around.
fn unlock_prompt_panel(state: &LockScreenState, password_field: &LockPasswordField) -> Div {
    let now = Instant::now();
    let mut col = div().absolute().bottom(px(70.)).left(px(0.)).right(px(0.)).flex().flex_col().items_center().gap(px(10.));

    col = col.child(lock_glyph_circle());
    col = col.child(div().w(px(240.)).child(password_field.field.clone()));

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
    view.lockscreen.lock();
    view.surface.close();
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
    window.focus(&handle, cx);
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
    let password = view.lockscreen_password_field.field.read(cx).content.clone();
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
