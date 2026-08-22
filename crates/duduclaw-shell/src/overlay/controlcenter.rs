// ControlCenter overlay content — Shell-S0 round 2, dark theme in Shell-S1.
//
// Visual spec: `commercial/design/duduclaw-os-desktop/ControlCenter.dc.html`
// (light) / `commercial/design/duduclaw-os-home-dark/ControlCenter.dc.html`
// (dark) — a right-docked floating panel (`top:40px; right:12px;
// width:372px`, no `bottom`/explicit height: the board lets it size to its
// own content, reproduced the same way by simply not calling `.h(...)`).
//
// Interaction scope this round (task brief: "開關做視覺 toggle 狀態
// （點擊可切換本地 bool 即可，不接後端）"): the phrase "開關" is read
// literally as the three AI-team TOGGLE-SWITCH widgets in the board's own
// "AI 團隊" card (自動化/主動行為/全部暫停 — pill-shaped switches with a
// sliding circular handle), which is exactly what got wired to
// `overlay::OverlayUiState` below. The top 3-tile quick-settings row
// (Wi-Fi/藍牙/勿擾) still renders as a static snapshot of the board's own
// state — consistent with the task brief's narrower "快速設定...等照畫板"
// wording for that part. Note the task brief's own switch-naming
// paraphrase ("自動化暫停/勿擾/接管模式") doesn't literally match this
// board's actual three switch labels (自動化/主動行為/全部暫停) — the board
// itself is the authoritative spec per this round's own instructions, so
// its literal labels/copy are what's implemented here.
//
// ── Shell-S4 (2026-08-22): the volume slider is real ─────────────────────
// The two sliders (volume/brightness) used to BOTH render as a static
// snapshot of `fake_data::SLIDER_ROWS` — this round replaces that for
// VOLUME ONLY, wiring it to `crate::audio::AudioBackend` (mirroring
// `oobe::network::NetworkBackend`'s real-backend/fake-fallback shape, see
// that module's own header comment and `crate::audio`'s own for the audio
// side). Brightness (`SLIDER_ROWS[1]`) stays exactly as it was — a
// backlight control is a DIFFERENT backend (no `wpctl` equivalent; Linux
// backlight lives under `/sys/class/backlight/*/brightness`, a kernel
// sysfs interface with no research pass done against it yet), explicitly
// out of scope for this round, left for a future one. Click/drag on the
// volume track calls `set_volume`; clicking the volume glyph calls
// `toggle_mute` — both through `kick_off_audio_call` below, the same
// background-thread + `std::sync::mpsc` + `cx.spawn` poll bridge
// `steps::network`'s click handlers established (`kick_off_audio_call`'s
// own doc comment has the exact shape).
//
// Layout: gpui does have a real `Display::Grid` (`Styled::grid()`, backed
// by Taffy) that could reproduce the board's `grid-template-columns:
// repeat(3, 1fr)` more literally, but nothing in this codebase has ever
// exercised that API — for a fixed 3-equal-column row, `flex()` + a
// `flex_1()` sizing constraint on each tile produces the identical visual
// result using the same primitive every other screen in this crate already
// relies on, so that's what's used here instead of introducing gpui's
// least-battle-tested layout mode for one call site.
//
// ── Dark theme (Shell-S1) ─────────────────────────────────────────
// Every color below now resolves through `palette: ShellPalette` — see
// `crate::palette`'s own header comment. Same dark-only panel-root
// `.text_color(...)` fallback the sibling overlay files document applies
// here too. One deliberate NON-follow of the general text ladder: the
// quick-tile subtitle ("DuDu-Office"/"關閉") and the AI-team switch-row
// description text both use the LITERAL hex `#9f9fa9` in BOTH themes on the
// approved canvas — unlike every other `#9f9fa9`/`#71717b` pair in this
// crate's other overlay boards, this ONE specific role does not invert
// (checked against both `ControlCenter.dc.html` files directly, not
// assumed). Routing it through `text_faint` would silently produce the
// WRONG dark value (`0x71717b` instead of the board's actual `0x9f9fa9`),
// so it's kept as an unthemed literal at both call sites instead, each
// commented — see `quick_tile`/`switch_row` below.
//
// `toggle_pill`'s "off" track color (`#e4e4e7` light / `#3f3f46` dark) is
// ALSO reused by the two horizontal sliders' own track background — both
// are the same bespoke gray this file's `track_off_hex` helper centralizes,
// rather than being duplicated as a literal pair at three call sites.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, relative, rgb, App, Bounds, BoxShadow, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Point, Stateful, Window,
};

use duduclaw_native_gui::theme;

use super::OverlayUiState;
use crate::audio::{self, AudioBackendKind};
use crate::palette::ShellPalette;
use crate::{fake_data, ShellView};

/// Demo-mode disclosure for the volume slider — same "在 UI 誠實標示（不可
/// 假裝連線成功）" discipline `oobe::network`'s own `NetworkDemoModeNotice`
/// established (`i18n.rs`), kept as a plain literal here rather than an
/// `i18n::Key` for consistency with its own file, not with that one:
/// `overlay.rs`'s header comment states Home/overlay's hardcoded zh-TW
/// stays untouched this round (no `Locale` is threaded into ControlCenter
/// at all), and every OTHER string in this file (`fake_data::CC_*`) is
/// already the same kind of unthemed literal — routing ONE new string
/// through `i18n::t()` while everything around it stays hardcoded would be
/// a worse inconsistency than staying literal alongside its neighbors.
const AUDIO_DEMO_MODE_NOTICE: &str = "示範模式：目前的音量調整為模擬效果，尚未連接真實音訊裝置";

pub(super) fn render(ui: &OverlayUiState, audio_ui: &audio::AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // ControlCenter.dc.html: bg `rgba(255,255,255,0.96)` light / `rgba(30,
    // 30,33,0.96)` dark — `surface_raised` in both. Border: opaque
    // `border()` light / `rgba(255,255,255,0.12)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };

    let mut panel = div()
        .id("overlay-controlcenter-panel")
        .absolute()
        .top(px(40.))
        .right(px(12.))
        .w(px(372.))
        .flex()
        .flex_col()
        .gap(px(14.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.96))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow())
        .p(px(16.));
    if palette.is_dark() {
        // See this file's header comment on why this is dark-only.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }
    panel.child(quick_tiles_row(palette)).child(sliders_card(audio_ui, palette, cx)).child(ai_team_card(ui, palette, cx)).child(footer_row(palette, cx))
}

/// The bespoke "inactive control" gray this file's header comment
/// describes — reused by `toggle_pill`'s off-track and both slider tracks.
fn track_off_hex(palette: ShellPalette) -> u32 {
    if palette.is_dark() { 0x3f3f46 } else { 0xe4e4e7 }
}

// ── Quick settings (static) ──────────────────────────────────────────────

fn quick_tiles_row(palette: ShellPalette) -> Div {
    let mut row = div().flex().gap(px(10.));
    for tile in fake_data::QUICK_TILES {
        row = row.child(quick_tile(tile, palette));
    }
    row
}

fn quick_tile(tile: &fake_data::QuickTile, palette: ShellPalette) -> Stateful<Div> {
    // Active (Wi-Fi) tile: bg `brand` in both themes; title/subtitle stay
    // the SAME literal (`#fafafa` opaque / `rgba(250,250,250,.75)`) in both
    // themes too — a colored tile's own white-on-brand text doesn't need to
    // change per theme. Inactive tiles: bg is `SECONDARY`/`MUTED`/
    // `SURFACE_HOVER` (all three tokens share one hex per theme, see
    // `theme.rs`'s own table), so `surface_hover` covers it in both; title
    // falls through to `palette.foreground` (no explicit color, matching
    // the original code's own `theme::light::FOREGROUND` — now resolved
    // per theme); icon glyph stroke is a bespoke pair (`#52525c`/`#b0b0b8`,
    // no clean token); subtitle is the ONE literal that does NOT invert —
    // see this file's header comment.
    let (bg_hex, title_hex) = if tile.active { (palette.brand, 0xfafafa) } else { (palette.surface_hover, palette.foreground) };
    let glyph_hex = if tile.active {
        0xfafafa
    } else if palette.is_dark() {
        0xb0b0b8
    } else {
        0x52525c
    };
    // Main.dc.html/ControlCenter.dc.html: `#9f9fa9`, unchanged across
    // themes — see this file's header comment.
    let (sub_hex, sub_alpha) = if tile.active { (0xfafafa, 0.75) } else { (0x9f9fa9, 1.0) };

    div()
        .id(tile.id)
        .flex_1()
        .bg(theme::alpha(bg_hex, 1.0))
        .rounded(px(13.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(glyph_hex, 1.0)).child(tile.glyph))
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(title_hex, 1.0)).child(tile.title))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(sub_hex, sub_alpha)).child(tile.subtitle)),
        )
}

fn sliders_card(audio_ui: &audio::AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // ControlCenter.dc.html: bg `#ffffff` light / `#1e1e21` dark —
    // `surface_raised`. Border: opaque `border()` light / `rgba(255,255,
    // 255,0.10)` dark (bespoke, not `border()`'s own dark 0.06).
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.10).into() } else { palette.border() };
    let mut card = div()
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(13.))
        .px(px(14.))
        .py(px(12.))
        .flex()
        .flex_col()
        .gap(px(12.))
        .child(volume_slider_row(audio_ui, palette, cx))
        // Brightness (`SLIDER_ROWS[1]`) stays the pre-existing static
        // snapshot — see this file's header comment on why.
        .child(slider_row(&fake_data::SLIDER_ROWS[1], palette));
    if audio_ui.backend_kind == Some(AudioBackendKind::Fake) {
        card = card.child(demo_mode_notice(palette));
    }
    card
}

fn demo_mode_notice(palette: ShellPalette) -> Div {
    div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(palette.warning, 0.14))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(palette.warning, 1.0))
        .child(AUDIO_DEMO_MODE_NOTICE)
}

/// The brightness row — UNCHANGED from before this round (still reads
/// `fake_data::SLIDER_ROWS` directly, still no click/drag handling). Kept
/// as its own function, not folded into `volume_slider_row`, precisely
/// because it stays static — see this file's header comment.
fn slider_row(row: &fake_data::SliderRow, palette: ShellPalette) -> Div {
    let pct = row.pct.clamp(0.0, 1.0);
    // Icon glyph stroke: same bespoke `#52525c`/`#b0b0b8` pair
    // `quick_tile`'s inactive icon uses. Track bg: `track_off_hex` (this
    // file's shared helper) — same bespoke gray `toggle_pill`'s off-state
    // uses. NOTE: the approved canvas also draws a circular drag handle on
    // this slider; the pre-existing (light-only) implementation never drew
    // one (track + fill only) — that gap is unchanged by this round (dark
    // theming only, not new elements), so no handle is added here either.
    let glyph_hex = if palette.is_dark() { 0xb0b0b8 } else { 0x52525c };

    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(div().text_size(px(13.)).text_color(theme::alpha(glyph_hex, 1.0)).child(row.glyph))
        .child(
            div().flex_1().relative().h(px(5.)).rounded(px(5.)).bg(theme::alpha(track_off_hex(palette), 1.0)).child(
                div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(pct)).rounded(px(5.)).bg(theme::alpha(palette.brand, 1.0)),
            ),
        )
}

/// The volume row — real this round. Same visual shape `slider_row` above
/// draws (icon + track + fill), plus: a `gpui::canvas`-backed bounds probe
/// on the track (`bounds_tracker` below) so click/drag handlers can turn a
/// window-relative mouse position into a 0..=100 target percentage;
/// `on_mouse_down` for click-to-set; `on_mouse_move` (gated on
/// `MouseMoveEvent::dragging()`, gpui's own "is the primary button still
/// held" helper) for drag-to-scrub; and the icon glyph itself as a
/// mute-toggle click target, dimmed/tinted red while muted for feedback
/// beyond the (separate) track fill.
fn volume_slider_row(audio_ui: &audio::AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let pct = audio_ui.pct.min(100);
    let fraction = f32::from(pct) / 100.0;
    let busy = audio_ui.in_flight;

    let glyph_hex = if palette.is_dark() { 0xb0b0b8 } else { 0x52525c };
    let icon_hex = if audio_ui.muted { palette.destructive } else { glyph_hex };
    let mute_click = cx.listener(|view, _ev: &ClickEvent, _window, cx| {
        kick_off_audio_call(view, cx, None, |backend| backend.toggle_mute());
    });
    let icon = div()
        .id("cc-volume-icon")
        .cursor_pointer()
        .text_size(px(13.))
        .text_color(theme::alpha(icon_hex, 1.0))
        .child(fake_data::SLIDER_ROWS[0].glyph)
        .on_click(mute_click);

    // Captures the track's laid-out bounds every paint pass — `on_mouse_
    // down`/`on_mouse_move` closures only ever receive a WINDOW-relative
    // `position: Point<Pixels>` (see gpui's `Interactivity::on_mouse_down`
    // signature), never the hovered element's own bounds, so this is the
    // one piece of extra plumbing volume click/drag needs beyond what
    // `steps::network`'s existing click-only precedent required. Same
    // `gpui::canvas` low-level paint hook `main.rs`'s own `bounds_probe`
    // diagnostic uses, repurposed to make the LATEST bounds available to
    // input handlers instead of only logging them.
    let track_bounds: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
    let bounds_for_down = track_bounds.clone();
    let bounds_for_move = track_bounds.clone();

    let down_handler = cx.listener(move |view, ev: &MouseDownEvent, _window, cx| {
        let target_pct = pct_from_event_position(bounds_for_down.get(), ev.position);
        kick_off_audio_call(view, cx, Some(target_pct), move |backend| backend.set_volume(target_pct));
    });
    let move_handler = cx.listener(move |view, ev: &MouseMoveEvent, _window, cx| {
        if !ev.dragging() {
            return;
        }
        let target_pct = pct_from_event_position(bounds_for_move.get(), ev.position);
        kick_off_audio_call(view, cx, Some(target_pct), move |backend| backend.set_volume(target_pct));
    });

    let mut track = div()
        .id("cc-volume-track")
        .flex_1()
        .relative()
        .h(px(5.))
        .rounded(px(5.))
        .bg(theme::alpha(track_off_hex(palette), 1.0))
        .child(div().absolute().left(px(0.)).top(px(0.)).bottom(px(0.)).w(relative(fraction)).rounded(px(5.)).bg(theme::alpha(palette.brand, 1.0)))
        .child(bounds_tracker(track_bounds));
    if !busy {
        // Same "visually inert while an operation is in flight, cosmetic
        // only" pattern `steps::network::wifi_row` establishes — the
        // AUTHORITATIVE guard lives in `kick_off_audio_call` itself (checked
        // first, before anything here even runs), matching that file's own
        // `kick_off_connect` doc comment on why the visual gate is a
        // secondary defense, not the real one.
        track = track.cursor_pointer().on_mouse_down(MouseButton::Left, down_handler).on_mouse_move(move_handler);
    }

    div().flex().items_center().gap(px(10.)).child(icon).child(track)
}

/// A `gpui::canvas` element sized to fill its parent, whose only job is to
/// stash its own laid-out bounds into `cell` every paint pass — see
/// `volume_slider_row`'s own doc comment for why this is needed at all.
fn bounds_tracker(cell: Rc<Cell<Bounds<Pixels>>>) -> impl IntoElement {
    gpui::canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {}).absolute().size_full()
}

/// Converts a window-relative mouse `position` into a `0..=100` volume
/// target, given the track's own last-known `bounds`. A zero-width track
/// (not yet painted — the very first frame, before `bounds_tracker`'s
/// canvas has run once) returns `0` rather than dividing by zero; in
/// practice this can only fire on a click that lands before the track's
/// first paint, which cannot happen (paint always precedes input dispatch
/// for the same frame).
fn pct_from_event_position(bounds: Bounds<Pixels>, position: Point<Pixels>) -> u8 {
    let width = bounds.size.width.as_f32();
    if width <= 0.0 {
        return 0;
    }
    let local_x = (position.x.as_f32() - bounds.origin.x.as_f32()).clamp(0.0, width);
    ((local_x / width) * 100.0).round() as u8
}

/// Dispatches ONE `AudioBackend` call on a background thread and bridges
/// its result back to `ShellView` — same background-thread ->
/// `std::sync::mpsc` -> `cx.spawn` poll-loop pattern `steps::network`'s
/// `kick_off_scan`/`kick_off_connect` already established (see that file's
/// own header comment for why: no `reqwest`/`tokio` in this crate, and
/// gpui's main thread must never block on real I/O — a subprocess spawn,
/// here, rather than a network call). Shared by both `set_volume` (`down_
/// handler`/`move_handler` above, `call` closes over the target pct) and
/// `toggle_mute` (`mute_click` above, no pct) rather than being duplicated
/// per call site, since the two only differ in which `AudioBackend` method
/// they invoke. The in-flight guard is checked here FIRST, before
/// `AudioUiState::begin` even runs — this is the authoritative guard `audio
/// ::AudioUiState.in_flight`'s own doc comment refers to, not the `busy`
/// cosmetic gate in `volume_slider_row`.
fn kick_off_audio_call(
    view: &mut ShellView,
    cx: &mut Context<ShellView>,
    optimistic_pct: Option<u8>,
    call: impl FnOnce(&dyn audio::AudioBackend) -> Result<audio::VolumeState, audio::AudioError> + Send + 'static,
) {
    if view.audio_ui.in_flight {
        return;
    }
    view.audio_ui.begin(optimistic_pct);
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (backend, kind) = audio::select_backend();
        let result = call(backend.as_ref());
        let _ = tx.send((kind, result));
    });

    // 20ms poll interval, shorter than `steps::network`'s 50ms — a volume
    // backend call is a local sub-10ms round-trip, not real network I/O, so
    // polling more often keeps a drag feeling responsive without adding
    // meaningful busy-work (this timer only runs while ONE call is
    // in-flight, never continuously).
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok((kind, result)) => {
                let _ = weak.update(cx, |view, cx| {
                    view.audio_ui.settle(kind, result);
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(20)).await;
    })
    .detach();
}

// ── AI 團隊 (interactive) ─────────────────────────────────────────────────

fn ai_team_card(ui: &OverlayUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let automation_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_automation();
        cx.notify();
    });
    let proactive_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_proactive();
        cx.notify();
    });
    let pause_all_click = cx.listener(|view, _ev, _window, cx| {
        view.overlay_ui.toggle_pause_all();
        cx.notify();
    });

    // Card bg/border: same `surface_raised`/`border()` pair `sliders_card`
    // uses. Section label ("AI 團隊"): text ladder rank 4 (`text_faint`) —
    // this one DOES invert (`#9f9fa9` light / `#71717b` dark), unlike the
    // switch-row description below it; verified against the board directly
    // rather than assumed, per this file's header comment. Divider under
    // the label: `#f0f0f2` light / `rgba(255,255,255,0.08)` dark.
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let label_divider = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };

    div()
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(13.))
        .overflow_hidden()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(11.))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::alpha(palette.text_faint, 1.0))
                .px(px(14.))
                .pt(px(11.))
                .pb(px(9.))
                .border_b_1()
                .border_color(label_divider)
                .child(fake_data::CC_SECTION_AI_TEAM),
        )
        .child(switch_row(fake_data::CC_SWITCH_AUTOMATION_LABEL, fake_data::CC_SWITCH_AUTOMATION_DESC, ui.automation_on(), true, palette, automation_click))
        .child(switch_row(fake_data::CC_SWITCH_PROACTIVE_LABEL, fake_data::CC_SWITCH_PROACTIVE_DESC, ui.proactive_on(), true, palette, proactive_click))
        .child(switch_row(fake_data::CC_SWITCH_PAUSE_ALL_LABEL, fake_data::CC_SWITCH_PAUSE_ALL_DESC, ui.pause_all_on(), false, palette, pause_all_click))
}

fn switch_row(
    label: &'static str,
    desc: &'static str,
    on: bool,
    border_b: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    // ControlCenter.dc.html: this description text is `#9f9fa9` in BOTH
    // themes — the same non-inverting literal `quick_tile`'s subtitle uses,
    // see this file's header comment.
    let mut row = div()
        .id(label)
        .cursor_pointer()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(14.))
        .py(px(11.))
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(palette.foreground, 1.0)).child(label))
                .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(desc)),
        )
        .child(toggle_pill(on, palette))
        .on_click(on_click);
    if border_b {
        let color = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };
        row = row.border_b_1().border_color(color);
    }
    row
}

fn toggle_pill(on: bool, palette: ShellPalette) -> Div {
    let track_hex = if on { palette.brand } else { track_off_hex(palette) };
    // Handle: `#ffffff` light / `#fafafa` dark in both on/off states — the
    // off state additionally carries a shadow (navy light / black dark),
    // matching the board's own on/off asymmetry (the "on" handle has no
    // shadow in either theme, only "off" does).
    let handle_bg = if palette.is_dark() { 0xfafafa } else { 0xffffff };
    let mut handle = div().absolute().top(px(2.)).w(px(19.)).h(px(19.)).rounded(px(19.)).bg(theme::alpha(handle_bg, 1.0));
    if on {
        handle = handle.right(px(2.));
    } else {
        let shadow_base = if palette.is_dark() { 0x000000 } else { 0x0f172a };
        let shadow_opacity = if palette.is_dark() { 0.40 } else { 0.15 };
        handle = handle.left(px(2.)).shadow(vec![BoxShadow::new(px(0.), px(1.), rgb(shadow_base).opacity(shadow_opacity).into()).blur_radius(px(2.))]);
    }
    div().relative().w(px(40.)).h(px(23.)).rounded(px(23.)).bg(theme::alpha(track_hex, 1.0)).child(handle)
}

// ── Footer ────────────────────────────────────────────────────────────────

/// Shell-S4-lock (2026-08-22): gained a "鎖定" button in the right-side
/// group, next to "打開管理面" — the ControlCenter entry point the task
/// brief asks for ("手動鎖（控制中心...加「鎖定」入口）"). No precedent on
/// EITHER `ControlCenter.dc.html` board for a lock button (macOS's own
/// Control Center puts one in its user-account tile, which this board
/// doesn't have a matching element for) — placed here rather than inventing
/// a new tile/card, following this file's own "no board precedent,
/// approximate with the nearest existing element" convention (see e.g.
/// `approval_card`'s rejected-badge color in `overlay/notifications.rs`).
fn footer_row(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // ControlCenter.dc.html: status text is `muted_foreground` in both
    // themes (`#71717b` light / `#9f9fa9` dark); the link text is `brand`
    // light / `brand_bright` dark.
    let link_text = if palette.is_dark() { palette.brand_bright } else { palette.brand };
    div()
        .flex()
        .items_center()
        .justify_between()
        .px(px(4.))
        .py(px(2.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(8.))
                .child(small_avatar("杜", palette.brand, palette))
                .child(small_avatar("財", 0x0f766e, palette))
                .child(div().text_size(px(12.)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(fake_data::CC_FOOTER_STATUS)),
        )
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(14.))
                .child(lock_button(palette, cx))
                .child(div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(link_text, 1.0)).child(fake_data::CC_FOOTER_LINK)),
        )
}

/// Locks the screen and closes ControlCenter in the same click — shares
/// `lockscreen::render::lock_and_refresh` with the `cmd-l` keyboard shortcut
/// (`main.rs::on_lock_now`) and the idle watchdog, so all three lock
/// triggers behave identically (dismiss any open overlay, refresh the
/// pending-approval feed if stale, `cx.notify()`). Styled as a plain
/// secondary-text link (same visual weight as "打開管理面" beside it, not a
/// full `action_button`-style pill like `overlay/notifications.rs`'s
/// approve/reject buttons) — a lock action is low-risk/instantly-reversible
/// (any key/click undoes it, see `crate::lockscreen`'s own header comment),
/// so it does not need that file's heavier two-step confirm treatment.
fn lock_button(palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let on_click = cx.listener(|view, _ev, _window, cx| {
        crate::lockscreen::render::lock_and_refresh(view, cx);
    });
    div()
        .id("cc-lock-button")
        .cursor_pointer()
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .text_color(theme::alpha(palette.muted_foreground, 1.0))
        .hover(|style| style.text_color(theme::alpha(palette.foreground, 1.0)))
        .child(fake_data::CC_LOCK_BUTTON)
        .on_click(on_click)
}

fn small_avatar(initial: &'static str, bg_hex: u32, palette: ShellPalette) -> Div {
    div()
        .w(px(24.))
        .h(px(24.))
        .rounded(px(12.))
        .bg(theme::alpha(bg_hex, 1.0))
        .flex()
        .items_center()
        .justify_center()
        .child(div().text_size(px(10.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.brand_foreground, 1.0)).child(initial))
}
