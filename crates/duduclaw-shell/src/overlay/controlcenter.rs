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
// `toggle_mute` — both through `audio::kick_off_audio_call`, the same
// background-thread + `std::sync::mpsc` + `cx.spawn` poll bridge
// `steps::network`'s click handlers established. (That helper lived in this
// file until D5 moved it to `audio::bridge`, where a second caller — the
// settings 聲音 page — could reach it; see that module's own header comment
// for the exact shape and why it is not `settings::spawn_rpc`.)
//
// ── D5 (2026-08-24): the volume row shows only what was actually read ────
// Two changes, both in service of the same rule the settings app already
// holds itself to (`settings/mod.rs`'s honesty contract):
//   1. NO SEEDED VALUE. The row used to open at 62% — `fake_data::
//      SLIDER_ROWS[0]`'s static snapshot, copied into `AudioUiState`'s
//      default — and only started telling the truth after the operator's
//      first drag round-tripped. It now dispatches an eager read on first
//      render (`audio::ensure_volume_probed`) and renders a plainly
//      un-read state until that lands, so no number is ever displayed that
//      a backend did not report.
//   2. AN HONEST DISABLED STATE. PipeWire ships in the appliance image as of
//      this round, so a Linux box that cannot reach it has a real fault, and
//      `audio::select_backend` now returns `Unavailable` there instead of
//      silently substituting the demo backend (see `crate::audio`'s own
//      header comment). This row renders that as a dimmed, non-interactive
//      track with the reason under it — never as a slider that moves and
//      changes nothing.
// The 示範模式 notice is unchanged and still fires on `Fake`, which is now
// reachable only on a non-Linux host or via `DUDUCLAW_SHELL_FAKE_AUDIO=1`.
// Brightness (`SLIDER_ROWS[1]`) is untouched by all of this and remains the
// static snapshot it has always been.
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
//
// ── D4a-6 (2026-08-24): the Wi-Fi tile is real ────────────────────────────
// `quick_tiles_row`/`quick_tile` used to render all three quick-settings
// tiles straight off `fake_data::QUICK_TILES`'s static snapshot (see this
// file's own earlier header comment, "still renders as a static snapshot of
// the board's own state"). D4b built a real `network.status`-backed Wi-Fi
// page and deliberately left this row alone (its own header comment says
// so); this round closes that one gap. `quick_tile` is now generic over
// primitives (id/glyph/title/subtitle/active) rather than `&fake_data::
// QuickTile`, so ONE render function serves both the two still-fake tiles
// (Bluetooth/勿擾, unchanged) and the Wi-Fi tile's real
// `overlay::wifi_tile::WifiTileState` — see that module's own header
// comment for the backend/state side. Matched by id
// (`wifi_tile::WIFI_TILE_ID`), not by array position, so a future reorder
// of `fake_data::QUICK_TILES` cannot silently wire the wrong tile.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    div, prelude::*, px, relative, rgb, App, Bounds, BoxShadow, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent,
    Pixels, Point, Stateful, Window,
};

use duduclaw_native_gui::theme;

use super::OverlayUiState;
use crate::audio::{self, AudioBackendKind};
use crate::icons;
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

/// Shown in place of the slider's percentage when this run's audio backend
/// cannot do anything — the `AudioBackendKind::Unavailable` state. Wording
/// deliberately names an ACTION the operator can take rather than a
/// component they have never heard of ("PipeWire"): internal implementation
/// names do not belong on an operator-facing surface (opus-playbook §7,
/// 使用者視角).
const AUDIO_UNAVAILABLE_NOTICE: &str = "音訊服務未啟動，目前無法調整音量。可到「系統設定 › 聲音」查看詳情。";

/// Shown while the eager first read is still in flight or has not been
/// dispatched yet. Says nothing about a level, because nothing is known.
const AUDIO_NOT_READ_YET_NOTICE: &str = "正在讀取音量…";

/// Shown next to a working control whose LAST call failed — the value on
/// screen is real but stale, and pretending the adjustment took would be the
/// dishonest option.
const AUDIO_LAST_CALL_FAILED_NOTICE: &str = "最後一次音量調整沒有成功，畫面上是上一次讀到的數值。";

/// The audio service answered, but this machine has no volume to read — in
/// practice a box with no sound output attached, which is a different fault
/// from the service being down and gets its own sentence.
const AUDIO_NO_VALUE_NOTICE: &str = "讀不到音量，這台機器可能沒有接上輸出裝置。";

pub(super) fn render(ui: &OverlayUiState, audio_ui: &audio::AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // D4a-6 (2026-08-24): the Wi-Fi tile's real read — the same render-time,
    // idempotent `cx.spawn` then `weak.update` dispatch `codrive_row::
    // render`/`audio::ensure_volume_probed` already use for a panel that has
    // to show state nobody clicked for yet. `ensure_loaded` itself is a
    // no-op once loaded (`Load::needs_load()` only arms on `NotLoaded`), so
    // this is safe to call on every repaint.
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, super::wifi_tile::ensure_loaded);
    })
    .detach();

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
    panel
        .child(quick_tiles_row(&ui.wifi_tile, palette))
        .child(sliders_card(audio_ui, palette, cx))
        .child(ai_team_card(ui, palette, cx))
        .child(system_settings_card(palette, cx))
        .child(accessibility_card(palette, cx))
        .child(footer_row(palette, cx))
}

/// D4b (2026-08-23): the entry point for 系統設定 (`crate::settings`).
///
/// It sits HERE, not on the dock's gear, for the reason macOS puts
/// 「系統設定…」 at the bottom of Control Centre: the gear opens the quick
/// panel (that is what `home/home_dock.rs::dock_settings` has always done,
/// and changing it would take the quick toggles away from a one-click
/// reach), and the full application is one row deeper. Same shape and same
/// geometry as `accessibility_card` below — that row is the precedent this
/// one copies, so the two entries in this panel look like siblings.
///
/// Its strings are literals rather than `crate::i18n` keys, matching this
/// file's own convention for its own copy (see the `AUDIO_DEMO_MODE_NOTICE`
/// comment above) and `crate::settings`' own header comment for why that
/// whole directory is hardcoded zh-TW.
fn system_settings_card(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let label_divider = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };

    let open_click = cx.listener(|view, _ev, _window, cx| {
        if crate::diag_enabled() {
            eprintln!("[hit] control centre -> open Settings");
        }
        view.surface.open(crate::surface::Overlay::Settings);
        cx.notify();
    });

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
                .child("這台機器"),
        )
        .child(
            div()
                .id("cc-settings-entry")
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(11.))
                .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
                .child(icons::icon_or_none(&[(icons::SETTINGS, palette.icon_control())], 18.).unwrap_or_else(|| div().into_any_element()))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::alpha(palette.foreground, 1.0))
                                .child("系統設定"),
                        )
                        // `#9f9fa9` in BOTH themes, the same non-inverting
                        // literal every sibling row here uses.
                        .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child("網路、螢幕、時間、帳號與更新")),
                )
                .child(icons::icon_or_none(&[(icons::CHEVRON_RIGHT, palette.text_faint)], 14.).unwrap_or_else(|| div().into_any_element()))
                .on_click(open_click),
        )
}

/// ICON-3 (2026-08-23): the entry point for 「協助工具 › 指向與點按」.
///
/// This panel is what the dock's gear opens (`home/home_dock.rs::
/// dock_settings`), i.e. it IS this shell's settings surface — so a settings
/// section belongs behind a row here, not behind an invented settings
/// application. The section label matches the board's own sidebar wording so
/// the two places that name this screen agree.
///
/// Its strings go through `crate::i18n` with a hardcoded `Locale::ZhTw`,
/// unlike the `fake_data::CC_*` literals around it. That is deliberate and
/// is the SAME shape `lockscreen/render.rs` already uses: this label is the
/// title of a screen that is itself fully i18n'd, so keeping the two in one
/// catalog is what stops them drifting apart. It does not change this file's
/// existing convention for its own strings — see the `AUDIO_DEMO_MODE_NOTICE`
/// comment above, which stays true of everything else here.
fn accessibility_card(palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    use crate::i18n::{t, Key, Locale};

    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let label_divider = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };

    let open_click = cx.listener(|view, _ev, _window, cx| {
        view.surface.open(crate::surface::Overlay::PointerSettings);
        cx.notify();
    });

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
                .child(t(Locale::ZhTw, Key::PointerSectionAccessibility)),
        )
        .child(
            div()
                .id("cc-pointer-entry")
                .cursor_pointer()
                .flex()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(11.))
                .hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0)))
                .child(icons::icon_or_none(&[(icons::A11Y_POINTING, palette.icon_control())], 18.).unwrap_or_else(|| div().into_any_element()))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::alpha(palette.foreground, 1.0))
                                .child(t(Locale::ZhTw, Key::PointerTitle)),
                        )
                        // `#9f9fa9` in BOTH themes, the same non-inverting
                        // literal `switch_row`/`quick_tile` use — see this
                        // file's header comment.
                        .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(t(Locale::ZhTw, Key::PointerEntryDesc))),
                )
                .child(icons::icon_or_none(&[(icons::CHEVRON_RIGHT, palette.text_faint)], 14.).unwrap_or_else(|| div().into_any_element()))
                .on_click(open_click),
        )
}

/// The bespoke "inactive control" gray this file's header comment
/// describes — reused by `toggle_pill`'s off-track and both slider tracks.
fn track_off_hex(palette: ShellPalette) -> u32 {
    if palette.is_dark() { 0x3f3f46 } else { 0xe4e4e7 }
}

// ── Quick settings (Wi-Fi real, Bluetooth/勿擾 still static) ─────────────

fn quick_tiles_row(wifi: &super::wifi_tile::WifiTileState, palette: ShellPalette) -> Div {
    let mut row = div().flex().gap(px(10.));
    for tile in fake_data::QUICK_TILES {
        if tile.id == super::wifi_tile::WIFI_TILE_ID {
            let (active, subtitle) = wifi.tile_status();
            row = row.child(quick_tile(tile.id, tile.glyph, tile.title, &subtitle, active, palette));
        } else {
            row = row.child(quick_tile(tile.id, tile.glyph, tile.title, tile.subtitle, tile.active, palette));
        }
    }
    row
}

/// Renders one quick-settings tile. Generic over primitives (not
/// `&fake_data::QuickTile`) since D4a-6: the Wi-Fi tile's title/subtitle are
/// now a real, owned `String` read from `network.status`, while Bluetooth/
/// 勿擾 still pass `fake_data::QuickTile`'s own `&'static str` fields
/// straight through — one render function, two data sources.
fn quick_tile(id: &'static str, glyph: &'static str, title: &str, subtitle: &str, active: bool, palette: ShellPalette) -> Stateful<Div> {
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
    let (bg_hex, title_hex) = if active { (palette.brand, 0xfafafa) } else { (palette.surface_hover, palette.foreground) };
    // ICON-1 (2026-08-22): the two inline literals this used to spell out
    // are now `palette.brand_foreground` (`#fafafa`, the active tile's
    // white-on-brand stroke) and `palette.icon_control()` (`#52525c` light
    // / `#b0b0b8` dark) — the SAME values, resolved through the methods
    // that also tint the real stroke icons below, so the icon and its text
    // fallback can never drift apart.
    let glyph_hex = if active { palette.brand_foreground } else { palette.icon_control() };
    // Main.dc.html/ControlCenter.dc.html: `#9f9fa9`, unchanged across
    // themes — see this file's header comment.
    let (sub_hex, sub_alpha) = if active { (0xfafafa, 0.75) } else { (0x9f9fa9, 1.0) };

    div()
        .id(id)
        .flex_1()
        .bg(theme::alpha(bg_hex, 1.0))
        .rounded(px(13.))
        .p(px(12.))
        .flex()
        .flex_col()
        .gap(px(8.))
        .child(
            div().text_size(px(15.)).font_weight(FontWeight::BOLD).text_color(theme::alpha(glyph_hex, 1.0)).child(
                // ICON-1: the board's own 17px stroke icon for this tile
                // (Wi-Fi arcs / Bluetooth rune / crescent moon), falling
                // back to the "W"/"B"/"勿" placeholder if its asset is
                // missing.
                icons::icon_or_glyph(&icons::quick_tile_layers(id, palette, active).unwrap_or_default(), 17., glyph),
            ),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(12.)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(title_hex, 1.0)).child(title.to_string()))
                .child(div().text_size(px(10.5)).text_color(theme::alpha(sub_hex, sub_alpha)).child(subtitle.to_string())),
        )
}

fn sliders_card(audio_ui: &audio::AudioUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // Eager first read — see this file's header comment (D5) and
    // `audio::ensure_volume_probed`'s own doc comment on why a render-time
    // dispatch is the right shape here and why it cannot loop.
    audio::ensure_volume_probed(cx);

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
    for (text, tone) in audio_notices(audio_ui) {
        card = card.child(audio_notice(text, tone, palette));
    }
    card
}

/// Which honest line(s) belong under the sliders, given what the audio
/// backend has actually said so far. Pure and exhaustive over the state
/// space, so the four cases are visible in one place and testable without a
/// window — the same reason `settings::sound_page::classify` is a free
/// function.
///
/// At most two lines, and only one of them is ever a backend-identity
/// notice: a demo/unavailable/not-read line, optionally followed by the
/// "last call failed" line (which can accompany an otherwise-working
/// control).
fn audio_notices(audio_ui: &audio::AudioUiState) -> Vec<(&'static str, NoticeTone)> {
    let mut notices = Vec::new();
    match audio_ui.backend_kind {
        None => notices.push((AUDIO_NOT_READ_YET_NOTICE, NoticeTone::Muted)),
        Some(AudioBackendKind::Fake) => notices.push((AUDIO_DEMO_MODE_NOTICE, NoticeTone::Warning)),
        Some(AudioBackendKind::Unavailable) => notices.push((AUDIO_UNAVAILABLE_NOTICE, NoticeTone::Warning)),
        // A real backend needs no identity notice; only a failure gets one.
        Some(AudioBackendKind::Real) => {}
    }
    // An `Unavailable` backend fails EVERY call by construction, so adding a
    // per-call failure line under it would be a second way of saying the
    // same thing.
    if audio_ui.last_call_failed && audio_ui.backend_kind != Some(AudioBackendKind::Unavailable) {
        // Two different failures, two different sentences: never having read
        // a value at all (no sink) is not the same as an adjustment that did
        // not take (a value IS on screen, it is just stale).
        notices.push(if audio_ui.has_reading() {
            (AUDIO_LAST_CALL_FAILED_NOTICE, NoticeTone::Warning)
        } else {
            (AUDIO_NO_VALUE_NOTICE, NoticeTone::Warning)
        });
    }
    notices
}

/// The two weights an audio notice comes in. Local to this file (the panel
/// has its own colour decisions, see the header comment) rather than shared
/// with `settings::widgets::Tone`, which belongs to a different surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoticeTone {
    Warning,
    Muted,
}

fn audio_notice(text: &'static str, tone: NoticeTone, palette: ShellPalette) -> Div {
    let color = match tone {
        NoticeTone::Warning => palette.warning,
        NoticeTone::Muted => palette.muted_foreground,
    };
    div()
        .w_full()
        .px(px(12.))
        .py(px(8.))
        .rounded(px(theme::RADIUS_LG))
        .bg(theme::alpha(color, 0.14))
        .text_size(px(theme::TEXT_XS))
        .text_color(theme::alpha(color, 1.0))
        .child(text)
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
    let glyph_hex = palette.icon_control();

    div()
        .flex()
        .items_center()
        .gap(px(10.))
        // ICON-1: the board's 15px brightness icon (a sun with eight rays)
        // replaces the "光" placeholder. This row is the BRIGHTNESS one —
        // `volume_slider_row` below draws the volume speaker separately
        // because only that one is interactive.
        .child(
            div()
                .text_size(px(13.))
                .text_color(theme::alpha(glyph_hex, 1.0))
                .child(icons::icon_or_glyph(&[(icons::BRIGHTNESS, glyph_hex)], 15., row.glyph)),
        )
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
    // D5: the fill is drawn from a REAL reading or not at all. Before the
    // first successful read (`has_reading() == false`) the track is empty —
    // an empty track reads as "nothing known yet", while a track filled from
    // `pct`'s default would read as "this machine is at 0%", and both of
    // those are wrong in different ways from the old seeded 62%.
    let pct = audio_ui.pct.min(100);
    let fraction = if audio_ui.has_reading() { f32::from(pct) / 100.0 } else { 0.0 };
    // Two independent reasons the track stops taking input, kept separate
    // because they mean different things: `busy` is momentary (one call in
    // flight) and cosmetic, `interactive` is a property of this run's
    // backend and is the honest disabled state.
    let busy = audio_ui.in_flight;
    let interactive = audio_ui.is_interactive();

    let glyph_hex = if palette.is_dark() { 0xb0b0b8 } else { 0x52525c };
    let icon_hex = if audio_ui.muted && audio_ui.has_reading() { palette.destructive } else { glyph_hex };
    let mute_click = cx.listener(|view, _ev: &ClickEvent, _window, cx| {
        audio::kick_off_audio_call(view, cx, None, |backend| backend.toggle_mute());
    });
    let mut icon = div()
        .id("cc-volume-icon")
        .text_size(px(13.))
        .text_color(theme::alpha(icon_hex, 1.0))
        // ICON-1: the board's 15px speaker icon replaces "音". `icon_hex`
        // (not `glyph_hex`) is passed through deliberately — this icon is
        // the mute toggle and already tints red while muted, and the real
        // icon has to keep that feedback rather than lose it.
        .child(icons::icon_or_glyph(&[(icons::VOLUME, icon_hex)], 15., fake_data::SLIDER_ROWS[0].glyph));
    if interactive {
        icon = icon.cursor_pointer().on_click(mute_click);
    } else {
        // Same treatment `settings::widgets::button` gives a disabled
        // control: still visible, still where the operator expects it, just
        // plainly inert — a control that DISAPPEARS reads as a missing
        // feature rather than an unavailable one.
        icon = icon.opacity(0.55);
    }

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
        audio::kick_off_audio_call(view, cx, Some(target_pct), move |backend| backend.set_volume(target_pct));
    });
    let move_handler = cx.listener(move |view, ev: &MouseMoveEvent, _window, cx| {
        if !ev.dragging() {
            return;
        }
        let target_pct = pct_from_event_position(bounds_for_move.get(), ev.position);
        audio::kick_off_audio_call(view, cx, Some(target_pct), move |backend| backend.set_volume(target_pct));
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
    if !interactive {
        track = track.opacity(0.55);
    }
    if interactive && !busy {
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
        // A2 (2026-08-23): 共駕 — who is driving this machine right now, plus
        // the 接管/交還 button. First row in the card on purpose: it is the
        // only row here that reports something happening RIGHT NOW, and the
        // three switches below it are standing preferences. Its whole
        // implementation (state, compositor calls, copy) lives in
        // `overlay/codrive_row.rs`.
        .child(super::codrive_row::render(&ui.codrive, palette, cx))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{AudioError, AudioUiState, VolumeState};

    fn settled(kind: AudioBackendKind, result: Result<VolumeState, AudioError>) -> AudioUiState {
        let mut ui = AudioUiState::default();
        ui.settle(kind, result);
        ui
    }

    /// Before anything is read the panel says so, in muted weight — it does
    /// NOT show a percentage, and it does not warn (nothing is wrong yet).
    #[test]
    fn an_unread_state_says_it_is_reading_and_shows_no_warning() {
        let notices = audio_notices(&AudioUiState::default());
        assert_eq!(notices, vec![(AUDIO_NOT_READ_YET_NOTICE, NoticeTone::Muted)]);
    }

    /// The state this whole round exists to make reachable: a Linux box that
    /// cannot reach its audio service gets one plain explanation, not a
    /// slider that pretends.
    #[test]
    fn an_unavailable_backend_explains_itself_exactly_once() {
        let ui = settled(AudioBackendKind::Unavailable, Err(AudioError::Unavailable("no pipewire".to_string())));
        assert_eq!(notice_texts(&ui), vec![AUDIO_UNAVAILABLE_NOTICE], "the per-call failure line would just repeat this one");
    }

    #[test]
    fn a_working_backend_needs_no_notice_at_all() {
        let ui = settled(AudioBackendKind::Real, Ok(VolumeState { pct: 40, muted: false }));
        assert!(audio_notices(&ui).is_empty());
    }

    #[test]
    fn the_demo_backend_is_labelled_not_silenced() {
        let ui = settled(AudioBackendKind::Fake, Ok(VolumeState { pct: 40, muted: false }));
        assert_eq!(notice_texts(&ui), vec![AUDIO_DEMO_MODE_NOTICE]);
    }

    /// A real backend whose last call failed keeps its (stale, real) reading
    /// on screen and says the adjustment did not take.
    #[test]
    fn a_transient_failure_on_a_real_backend_is_disclosed() {
        let mut ui = settled(AudioBackendKind::Real, Ok(VolumeState { pct: 40, muted: false }));
        ui.settle(AudioBackendKind::Real, Err(AudioError::Unavailable("boom".to_string())));
        assert_eq!(notice_texts(&ui), vec![AUDIO_LAST_CALL_FAILED_NOTICE]);
    }

    /// The demo backend can also fail (its `set_default_output` does), and
    /// then BOTH facts are true and both are stated.
    #[test]
    fn a_failure_on_the_demo_backend_shows_both_lines() {
        let mut ui = settled(AudioBackendKind::Fake, Ok(VolumeState { pct: 40, muted: false }));
        ui.settle(AudioBackendKind::Fake, Err(AudioError::Unavailable("boom".to_string())));
        assert_eq!(notice_texts(&ui), vec![AUDIO_DEMO_MODE_NOTICE, AUDIO_LAST_CALL_FAILED_NOTICE]);
    }

    /// A working service on a machine with no sound output: the read fails,
    /// so there is no value — and that is a different sentence from "the
    /// adjustment didn't take", which would be nonsense here (there was no
    /// adjustment and there is no value on screen).
    #[test]
    fn a_real_backend_that_never_produced_a_value_says_so_specifically() {
        let ui = settled(AudioBackendKind::Real, Err(AudioError::Unavailable("no default sink".to_string())));
        assert_eq!(notice_texts(&ui), vec![AUDIO_NO_VALUE_NOTICE]);
        assert!(!ui.is_interactive(), "a slider with no readable level must not be draggable");
    }

    /// Interactivity needs BOTH a usable backend and a real reading — the
    /// four combinations must not collapse onto one another.
    #[test]
    fn only_a_usable_backend_with_a_real_reading_makes_the_track_interactive() {
        assert!(!AudioUiState::default().is_interactive(), "nothing read yet");
        assert!(settled(AudioBackendKind::Real, Ok(VolumeState { pct: 1, muted: false })).is_interactive());
        assert!(settled(AudioBackendKind::Fake, Ok(VolumeState { pct: 1, muted: false })).is_interactive());
        assert!(!settled(AudioBackendKind::Unavailable, Err(AudioError::Unavailable("x".to_string()))).is_interactive());
        assert!(
            !settled(AudioBackendKind::Real, Err(AudioError::Unavailable("no sink".to_string()))).is_interactive(),
            "a reachable service with no readable level is still nothing to drag"
        );
    }

    fn notice_texts(ui: &AudioUiState) -> Vec<&'static str> {
        audio_notices(ui).into_iter().map(|(text, _)| text).collect()
    }
}
