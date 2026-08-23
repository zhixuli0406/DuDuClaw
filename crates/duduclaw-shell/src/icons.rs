// Vector icons for the shell — ICON-1 (2026-08-22).
//
// ── What this replaces ───────────────────────────────────────────────────
// Every icon-shaped slot in this crate used to render a single CJK
// character ("設" for the dock's settings gear, "勿" for Do-not-disturb,
// "表" for a spreadsheet, …) or plain ASCII ("W"/"B" for Wi-Fi/Bluetooth),
// and the purely decorative slots with no safe equivalent character (the
// menu bar's Wi-Fi arc, the Launcher's magnifier) were dropped entirely.
// The reasoning recorded at the time (`home.rs` / `fake_data.rs` /
// `overlay/launcher.rs` header comments) was correct about the CAUSE —
// no `gpui::svg()` usage existed anywhere in this codebase and the bundled
// font stack has no pictographic coverage — but wrong about the
// consequence: the icons were never missing. They were designed, drawn as
// real inline SVG, and signed off by the operator on 2026-08-20, sitting
// in the approved design boards this whole time
// (`commercial/design/duduclaw-os-desktop/*.dc.html` +
// `commercial/design/duduclaw-os-home-dark/*.dc.html`). This module is the
// plumbing that finally connects them.
//
// ── Where the assets come from ───────────────────────────────────────────
// `crates/duduclaw-shell/assets/icons/*.svg`, one file per icon, path data
// transcribed VERBATIM from the boards (every `d`/`cx`/`cy`/`r`/`x`/`y`/
// `width`/`height`/`rx` attribute copied character-for-character — nothing
// was redrawn "close enough"). Two container-level attributes are the only
// additions a standalone file needs and the boards' inline SVG did not
// carry: `xmlns="http://www.w3.org/2000/svg"` (usvg refuses a document
// without it) and nothing else.
//
// They deliberately live in THIS repo, not in `commercial/` — that tree is
// a separate nested git repo AND gitignored here, so an `include_bytes!`
// pointing into it would build on this machine and fail everywhere else.
// Same reasoning `home.rs`'s `MARK_32`/`CAT_512` already record for the
// branding PNGs (which resolve to `appliance/branding/png/`).
//
// Naming rule: one file per DEPICTED SHAPE, kebab-case, named for what the
// icon draws rather than where it happens to be used (`folder.svg`, not
// `dock-slot-6.svg`) — the same icon is reused at several call sites and a
// role-based name would be a lie at all but one of them. Where one board
// icon paints in more than one color, it is split into one file PER COLOR
// LAYER with a shared prefix (`document-outline` / `document-lines` /
// `document-pencil`), because of the rendering constraint below.
//
// ── The rendering constraint (read before adding a multi-color icon) ─────
// Verified against the pinned gpui rev (`7a7c3e1`,
// `crates/gpui/src/window.rs::paint_svg` + `svg_renderer.rs::
// render_alpha_mask`): `gpui::svg()` rasterizes the document to an ALPHA
// MASK and paints it as a `MonochromeSprite` tinted by the element's own
// `text_color`. Two consequences, both load-bearing here:
//
//   1. The stroke/fill colors written INSIDE an asset file are inert. They
//      are kept at the light board's own values purely so the file still
//      renders correctly in a browser/editor when someone inspects it; the
//      color that actually reaches the screen is whichever palette token
//      the caller pairs with the key. This is what makes the light/dark
//      boards collapse into ONE asset set: both boards' SVGs were compared
//      attribute-by-attribute and their path data is IDENTICAL — only the
//      stroke colors differ, and those are exactly what the palette now
//      supplies (see `crate::palette::ShellPalette`'s `icon_*` methods).
//   2. A single `svg()` element cannot paint two colors. The boards' two
//      multi-color icons (the Files document with its amber pencil, the
//      spreadsheet with white rules over a green body) are therefore
//      composed as STACKED layers — see `icon_or_none`'s `layers`
//      parameter.
//
// `gpui::img()` + `ImageFormat::Svg` WOULD render full color in one
// element (it routes to `render_single_frame`, not the alpha mask), and
// was rejected deliberately: it rasterizes at a fixed 2x the document's
// own intrinsic size rather than at the element's real bounds, and — the
// deciding reason — it would bake the light board's colors into a bitmap,
// putting every icon permanently outside the theme system. Layering costs
// two extra small files and buys per-theme correctness for free.
//
// ── Degradation ──────────────────────────────────────────────────────────
// `paint_svg` draws NOTHING (silently, via `.log_err()`) when an asset
// fails to resolve. A blank hole where an icon should be is the one
// outcome worse than the CJK-character placeholder it replaced, so every
// call site goes through `icon_or_glyph`, which resolves the bytes FIRST
// and falls back to the original placeholder character when they are
// missing, logging the miss once per key. Three independent tests make
// that path unreachable in practice rather than merely survivable: every
// key in `ICONS` resolves to non-empty bytes; every payload PARSES through
// the same `usvg` the renderer uses; and every payload RASTERIZES to a
// non-empty image. The last two matter because a file that resolves but
// fails to parse (or parses to nothing) never reaches the fallback at all
// — it reaches the silent blank hole, which the fallback cannot see.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use gpui::{div, prelude::*, px, AnyElement, Hsla, SharedString, Svg};

use duduclaw_native_gui::theme;

use crate::palette::ShellPalette;

// ── Keys ─────────────────────────────────────────────────────────────────
// The key IS the `AssetSource` path — `gpui::svg().path(k)` hands it
// straight to `ShellAssets::load` below, and `paint_svg` also uses it as
// the sprite-atlas cache key, so it must be stable across renders (it is:
// these are `&'static str` consts, not formatted strings).

pub(crate) const WIFI: &str = "icons/wifi.svg";
pub(crate) const UPLOAD: &str = "icons/upload.svg";
pub(crate) const ARROW_UP: &str = "icons/arrow-up.svg";
pub(crate) const GLOBE: &str = "icons/globe.svg";
pub(crate) const SETTINGS: &str = "icons/settings.svg";
pub(crate) const WIFI_TILE: &str = "icons/wifi-tile.svg";
pub(crate) const BLUETOOTH: &str = "icons/bluetooth.svg";
pub(crate) const MOON: &str = "icons/moon.svg";
pub(crate) const VOLUME: &str = "icons/volume.svg";
pub(crate) const BRIGHTNESS: &str = "icons/brightness.svg";
pub(crate) const SEARCH: &str = "icons/search.svg";
pub(crate) const FOLDER_FILLED: &str = "icons/folder-filled.svg";
pub(crate) const SPREADSHEET_BODY: &str = "icons/spreadsheet-body.svg";
pub(crate) const SPREADSHEET_LINES: &str = "icons/spreadsheet-lines.svg";
pub(crate) const DOWNLOAD: &str = "icons/download.svg";

/// The board's five CONCEPTUAL app icons: extracted and embedded like every
/// other icon, but with no call site today. They are `UNBOUND_APP_ICONS`
/// below, and the reason is a change that landed alongside this work
/// package rather than an oversight.
///
/// `Main.dc.html`'s dock draws six imaginary apps (mail, a document with an
/// amber pencil, a globe, a music note, a chat bubble, a folder). APP-1
/// deleted the canned `DockApp`/`DOCK_APPS` array those tiles were rendered
/// from, because on a real appliance it advertised software that was not
/// installed; the dock and the Launcher now enumerate the machine's actual
/// inventory (`crate::apps::installed`). A real installed app has no board
/// icon to lift — its icon is whatever its own `.desktop` `Icon=` names,
/// resolved against the system icon theme, which `apps::installed` already
/// parses and carries and which is explicitly a separate work package. So
/// binding, say, `MAIL` to whichever mail client happens to be installed
/// would be this module inventing an identity for someone else's app.
///
/// The globe is the one exception and IS bound: `apps::catalog`'s single
/// entry is the same Chromium the board's browser tile always stood for, so
/// `catalog_layers` uses it.
///
/// Kept rather than deleted because the `Icon=` work package needs exactly
/// this set as its unresolvable-icon fallback family, and re-extracting
/// verified board artwork later is pure waste. The test
/// `unbound_app_icons_are_shipped_and_still_unbound` pins the list so it
/// cannot quietly grow, and asserts the mapping really does not use them.
/// ICON-2 (2026-08-22) settled what these seven are NOT for. This module's
/// original note above imagined them as the "could not resolve an app's
/// icon" fallback family; the research sweep
/// (`research/native-os-2026-08/icon-and-cursor-system-2026-08.md` §2.3)
/// found that no desktop OS guesses an app's icon from its category, and
/// the operator's own ruling for this round was ONE generic application
/// icon (`APP_GENERIC`), not a family. Picking `MAIL` for whichever mail
/// client happens to be installed would be the same "inventing an identity
/// for someone else's app" this note already refuses. They stay shipped and
/// unbound — the boards' artwork, kept verbatim, for a future round that
/// legitimately draws a mail/music/chat/folder concept.
pub(crate) const MAIL: &str = "icons/mail.svg";
pub(crate) const DOCUMENT_OUTLINE: &str = "icons/document-outline.svg";
pub(crate) const DOCUMENT_LINES: &str = "icons/document-lines.svg";
pub(crate) const DOCUMENT_PENCIL: &str = "icons/document-pencil.svg";
pub(crate) const MUSIC: &str = "icons/music.svg";
pub(crate) const CHAT: &str = "icons/chat.svg";
pub(crate) const FOLDER: &str = "icons/folder.svg";

/// The generic application icon — ICON-2 (2026-08-22). Drawn for the one
/// slot that genuinely needs a stand-in: an installed app whose `Icon=`
/// this shell could not turn into a file. Semantics match freedesktop's
/// `application-x-executable`, which is what GNOME Shell itself falls back
/// to; see the asset file's own header comment for the shape's source and
/// licence. Unlike the seven above, this one IS bound — by
/// `app_icon_element`, the only call site.
pub(crate) const APP_GENERIC: &str = "icons/app-generic.svg";

// ── ICON-3 (2026-08-23): lockscreen revision + OOBE icons + pointer settings
// Boards: `commercial/design/duduclaw-lockscreen-oobe-icons/{Main,
// OOBE-ProgressAndIcons,OOBE-KeySteps,PointerSettings}.dc.html`. Same
// extraction discipline ICON-1 established (path data character-for-
// character, container attributes are the only additions) — each asset file
// names its own source board in its header comment, and the two SELF-DRAWN
// ones (`CHART_BARS`/`ALERT_TRIANGLE`, per the operator's ruling ⑤ that the
// Privacy step's four rows each get an icon, where the boards drew none)
// say so in the same place and cite their draft source + licence.

/// `changes-prevent` — the closed padlock. The lockscreen's 56px glass
/// circle (24px, stroke-width 2) uses this; `LOCK_CLOSED_SM` is the SAME
/// path at stroke-width 2.4 for the <20px slots (the boards' own rule for
/// small strokes on any background).
pub(crate) const LOCK_CLOSED: &str = "icons/lock-closed.svg";
pub(crate) const LOCK_CLOSED_SM: &str = "icons/lock-closed-sm.svg";
pub(crate) const ACCESSIBILITY: &str = "icons/accessibility.svg";
/// `system-shutdown` (IEC 5009) — the lockscreen's power button.
pub(crate) const POWER: &str = "icons/power.svg";
/// `avatar-default` — the lockscreen name row's fallback, used ONLY when the
/// operator's name yields no usable first character (see
/// `lockscreen::render::name_avatar`).
pub(crate) const AVATAR_DEFAULT: &str = "icons/avatar-default.svg";
/// `object-select` — the tick. Bound only by the pointer-settings style
/// card's own selected radio this round; the boards also assign it to the
/// input-detection / update / account steps' 頁內 ticks, which stay out of
/// this round's scope.
pub(crate) const CHECK: &str = "icons/check.svg";
pub(crate) const CHEVRON_DOWN: &str = "icons/chevron-down.svg";
pub(crate) const CHEVRON_RIGHT: &str = "icons/chevron-right.svg";

/// The five accessibility categories the OOBE language step lists.
/// `A11Y_TYPING`/`A11Y_POINTING` are the same shapes the boards also name
/// `input-keyboard`/`input-mouse` — one shape, two semantic names, exactly
/// as `OOBE-ProgressAndIcons.dc.html`'s own row-2 note records.
pub(crate) const A11Y_SEEING: &str = "icons/a11y-seeing.svg";
pub(crate) const A11Y_HEARING: &str = "icons/a11y-hearing.svg";
pub(crate) const A11Y_TYPING: &str = "icons/a11y-typing.svg";
pub(crate) const A11Y_POINTING: &str = "icons/a11y-pointing.svg";
pub(crate) const A11Y_ZOOM: &str = "icons/a11y-zoom.svg";

/// `network-wireless-signal-*` — ONE concentric-arc shape whose five levels
/// differ only in which arcs are painted in the brand colour. gpui paints one
/// colour per `svg()` element (see this module's header comment), so the
/// board's single document becomes four independently-tinted layers here.
pub(crate) const WIFI_ARC_OUTER: &str = "icons/wifi-arc-outer.svg";
pub(crate) const WIFI_ARC_MID: &str = "icons/wifi-arc-mid.svg";
pub(crate) const WIFI_ARC_INNER: &str = "icons/wifi-arc-inner.svg";
pub(crate) const WIFI_DOT: &str = "icons/wifi-dot.svg";

/// OOBE step-title icons (32px, stroke-width 2, `muted_foreground`).
pub(crate) const SHIELD: &str = "icons/shield.svg";
pub(crate) const SOFTWARE_UPDATE: &str = "icons/software-update.svg";
pub(crate) const KEY: &str = "icons/key.svg";
pub(crate) const THEME_CONTRAST: &str = "icons/theme-contrast.svg";

/// The Privacy step's four row icons. Two are lifted from
/// `PointerSettings.dc.html`'s own settings sidebar (`SLIDERS` = 一般,
/// `BELL` = 通知); two had no board precedent anywhere in this set and are
/// self-drawn — see their asset files' own headers.
pub(crate) const SLIDERS: &str = "icons/sliders.svg";
pub(crate) const BELL: &str = "icons/bell.svg";
pub(crate) const CHART_BARS: &str = "icons/chart-bars.svg";
pub(crate) const ALERT_TRIANGLE: &str = "icons/alert-triangle.svg";

/// The pointer-settings cursor previews. Each real cursor is two colours (a
/// dark outline around a light/brand interior), so each is a stacked
/// outline+fill PAIR — the same technique the shipping cursor artwork itself
/// uses (two `<use>` passes) and the same one this module already documents
/// for the boards' multi-colour icons.
pub(crate) const CURSOR_ARROW_OUTLINE: &str = "icons/cursor-arrow-outline.svg";
pub(crate) const CURSOR_ARROW_FILL: &str = "icons/cursor-arrow-fill.svg";
pub(crate) const CURSOR_TEXT_HALO: &str = "icons/cursor-text-halo.svg";
pub(crate) const CURSOR_TEXT_CORE: &str = "icons/cursor-text-core.svg";
pub(crate) const CURSOR_HAND_OUTLINE: &str = "icons/cursor-hand-outline.svg";
pub(crate) const CURSOR_HAND_FILL: &str = "icons/cursor-hand-fill.svg";
pub(crate) const CURSOR_PAW_OUTLINE: &str = "icons/cursor-paw-outline.svg";
pub(crate) const CURSOR_PAW_FILL: &str = "icons/cursor-paw-fill.svg";

/// Every embedded asset, keyed by its `AssetSource` path.
///
/// `include_bytes!` rather than a runtime file read: the appliance boots
/// with a READ-ONLY root and the shell's working directory is not
/// guaranteed, so any path-relative asset lookup would be a latent
/// production failure. Same convention `home.rs`'s branding PNGs already
/// follow.
const ICONS: &[(&str, &[u8])] = &[
    (WIFI, include_bytes!("../assets/icons/wifi.svg")),
    (UPLOAD, include_bytes!("../assets/icons/upload.svg")),
    (ARROW_UP, include_bytes!("../assets/icons/arrow-up.svg")),
    (MAIL, include_bytes!("../assets/icons/mail.svg")),
    (DOCUMENT_OUTLINE, include_bytes!("../assets/icons/document-outline.svg")),
    (DOCUMENT_LINES, include_bytes!("../assets/icons/document-lines.svg")),
    (DOCUMENT_PENCIL, include_bytes!("../assets/icons/document-pencil.svg")),
    (GLOBE, include_bytes!("../assets/icons/globe.svg")),
    (MUSIC, include_bytes!("../assets/icons/music.svg")),
    (CHAT, include_bytes!("../assets/icons/chat.svg")),
    (FOLDER, include_bytes!("../assets/icons/folder.svg")),
    (SETTINGS, include_bytes!("../assets/icons/settings.svg")),
    (WIFI_TILE, include_bytes!("../assets/icons/wifi-tile.svg")),
    (BLUETOOTH, include_bytes!("../assets/icons/bluetooth.svg")),
    (MOON, include_bytes!("../assets/icons/moon.svg")),
    (VOLUME, include_bytes!("../assets/icons/volume.svg")),
    (BRIGHTNESS, include_bytes!("../assets/icons/brightness.svg")),
    (SEARCH, include_bytes!("../assets/icons/search.svg")),
    (FOLDER_FILLED, include_bytes!("../assets/icons/folder-filled.svg")),
    (SPREADSHEET_BODY, include_bytes!("../assets/icons/spreadsheet-body.svg")),
    (SPREADSHEET_LINES, include_bytes!("../assets/icons/spreadsheet-lines.svg")),
    (DOWNLOAD, include_bytes!("../assets/icons/download.svg")),
    (APP_GENERIC, include_bytes!("../assets/icons/app-generic.svg")),
    // ── ICON-3 (2026-08-23) ──────────────────────────────────────────────
    (LOCK_CLOSED, include_bytes!("../assets/icons/lock-closed.svg")),
    (LOCK_CLOSED_SM, include_bytes!("../assets/icons/lock-closed-sm.svg")),
    (ACCESSIBILITY, include_bytes!("../assets/icons/accessibility.svg")),
    (POWER, include_bytes!("../assets/icons/power.svg")),
    (AVATAR_DEFAULT, include_bytes!("../assets/icons/avatar-default.svg")),
    (CHECK, include_bytes!("../assets/icons/check.svg")),
    (CHEVRON_DOWN, include_bytes!("../assets/icons/chevron-down.svg")),
    (CHEVRON_RIGHT, include_bytes!("../assets/icons/chevron-right.svg")),
    (A11Y_SEEING, include_bytes!("../assets/icons/a11y-seeing.svg")),
    (A11Y_HEARING, include_bytes!("../assets/icons/a11y-hearing.svg")),
    (A11Y_TYPING, include_bytes!("../assets/icons/a11y-typing.svg")),
    (A11Y_POINTING, include_bytes!("../assets/icons/a11y-pointing.svg")),
    (A11Y_ZOOM, include_bytes!("../assets/icons/a11y-zoom.svg")),
    (WIFI_ARC_OUTER, include_bytes!("../assets/icons/wifi-arc-outer.svg")),
    (WIFI_ARC_MID, include_bytes!("../assets/icons/wifi-arc-mid.svg")),
    (WIFI_ARC_INNER, include_bytes!("../assets/icons/wifi-arc-inner.svg")),
    (WIFI_DOT, include_bytes!("../assets/icons/wifi-dot.svg")),
    (SHIELD, include_bytes!("../assets/icons/shield.svg")),
    (SOFTWARE_UPDATE, include_bytes!("../assets/icons/software-update.svg")),
    (KEY, include_bytes!("../assets/icons/key.svg")),
    (THEME_CONTRAST, include_bytes!("../assets/icons/theme-contrast.svg")),
    (SLIDERS, include_bytes!("../assets/icons/sliders.svg")),
    (BELL, include_bytes!("../assets/icons/bell.svg")),
    (CHART_BARS, include_bytes!("../assets/icons/chart-bars.svg")),
    (ALERT_TRIANGLE, include_bytes!("../assets/icons/alert-triangle.svg")),
    (CURSOR_ARROW_OUTLINE, include_bytes!("../assets/icons/cursor-arrow-outline.svg")),
    (CURSOR_ARROW_FILL, include_bytes!("../assets/icons/cursor-arrow-fill.svg")),
    (CURSOR_TEXT_HALO, include_bytes!("../assets/icons/cursor-text-halo.svg")),
    (CURSOR_TEXT_CORE, include_bytes!("../assets/icons/cursor-text-core.svg")),
    (CURSOR_HAND_OUTLINE, include_bytes!("../assets/icons/cursor-hand-outline.svg")),
    (CURSOR_HAND_FILL, include_bytes!("../assets/icons/cursor-hand-fill.svg")),
    (CURSOR_PAW_OUTLINE, include_bytes!("../assets/icons/cursor-paw-outline.svg")),
    (CURSOR_PAW_FILL, include_bytes!("../assets/icons/cursor-paw-fill.svg")),
];

/// The embedded bytes for `path`, or `None` when nothing is registered
/// under that key. Pure, and the single lookup both the `AssetSource` impl
/// and the render-side `icon()` guard go through — so "the renderer can
/// find it" and "the guard says it exists" can never disagree.
pub(crate) fn bytes(path: &str) -> Option<&'static [u8]> {
    ICONS.iter().find(|(key, _)| *key == path).map(|(_, data)| *data)
}

// ── AssetSource ──────────────────────────────────────────────────────────

/// This crate's `gpui::AssetSource`, registered ONCE in `main.rs` via
/// `application().with_assets(ShellAssets)` — the only registration point
/// that exists, because `Application::with_assets` (`gpui/src/app.rs:200`)
/// consumes the pre-launch `Application` value and also rebuilds the
/// `SvgRenderer` around the new source; there is no post-`run()` API to
/// swap it. gpui's default is `impl AssetSource for ()`, which answers
/// `Ok(None)` to everything — which is exactly why `svg()` could never have
/// worked in this crate before, no matter what path it was handed.
///
/// Registered in the SHELL's own `main.rs`, not inherited from
/// `duduclaw-native-gui`: the shell is a separate binary that the appliance's
/// kiosk unit launches directly (`appliance/mkosi.extra/usr/local/sbin/
/// duduclaw-kiosk-launch.sh`), so `duduclaw-native-gui`'s `main.rs` never
/// runs on the appliance at all and nothing it configures reaches here.
/// (That same fact is why this crate now loads its own fonts too — see
/// `main.rs`'s `add_fonts` call site.)
pub(crate) struct ShellAssets;

impl gpui::AssetSource for ShellAssets {
    fn load(&self, path: &str) -> gpui::Result<Option<std::borrow::Cow<'static, [u8]>>> {
        Ok(bytes(path).map(std::borrow::Cow::Borrowed))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(ICONS.iter().filter(|(key, _)| key.starts_with(path)).map(|(key, _)| SharedString::from(*key)).collect())
    }
}

// ── Render helpers ───────────────────────────────────────────────────────

/// One paint layer: an asset key plus the color it is tinted with. A
/// single-color icon is one layer; the boards' two multi-color icons are
/// several (see this module's header comment on why gpui cannot do it in
/// one element).
pub(crate) type Layer = (&'static str, u32);

/// Renders `layers` stacked, `size`x`size` px, or `None` when ANY layer's
/// asset is missing — never a partial stack, since a document drawn without
/// its pencil would look like a deliberate design rather than a fault.
/// Callers with a text placeholder to fall back on should use
/// `icon_or_glyph`; this raw form is for the slots that never had one (the
/// menu bar's Wi-Fi indicator, the Launcher's magnifier), where rendering
/// nothing is the honest degradation and a substitute character would be an
/// invention.
///
/// A one-layer call returns the bare `svg()` element (no wrapper `div`);
/// multi-layer calls get a `relative()` box with each layer absolutely
/// pinned to the same bounds, so the layers register exactly the way they
/// do inside the board's single SVG document.
pub(crate) fn icon_or_none(layers: &[Layer], size: f32) -> Option<AnyElement> {
    for (key, _) in layers {
        if bytes(key).is_none() {
            warn_missing(key);
            return None;
        }
    }
    match layers {
        [] => None,
        [(key, hex)] => Some(layer_svg(key, *hex, size).into_any_element()),
        many => {
            let mut stack = div().relative().w(px(size)).h(px(size)).flex_none();
            for (key, hex) in many {
                stack = stack.child(layer_svg(key, *hex, size).absolute().top(px(0.)).left(px(0.)));
            }
            Some(stack.into_any_element())
        }
    }
}

fn layer_svg(key: &'static str, hex: u32, size: f32) -> Svg {
    tinted_svg(key, theme::alpha(hex, 1.0).into(), size)
}

/// The one place `gpui::svg()` is constructed. `.text_color` is not
/// cosmetic here: `paint_svg` is only reached when `style.text.color` is
/// `Some` (`gpui/src/elements/svg.rs`'s `paint`), so an untinted `svg()`
/// draws nothing at all. Always set it.
///
/// Takes an `Hsla` rather than a `u32` because ICON-2's generic fallback
/// has to reproduce the exact color its text placeholder had, and one of
/// those (the neutral tile in LIGHT) is `foreground` at 0.55 alpha — which
/// a hex cannot express.
fn tinted_svg(key: &'static str, color: Hsla, size: f32) -> Svg {
    gpui::svg().path(key).w(px(size)).h(px(size)).flex_none().text_color(color)
}

/// The one entry point every call site uses: the real icon when its assets
/// are embedded, otherwise the placeholder character that slot used before
/// ICON-1. The fallback inherits `text_size`/`font_weight`/`text_color`
/// from its parent element exactly as the original placeholder did, so a
/// degraded slot renders identically to the pre-ICON-1 build rather than
/// half-styled.
pub(crate) fn icon_or_glyph(layers: &[Layer], size: f32, glyph: &'static str) -> AnyElement {
    icon_or_none(layers, size).unwrap_or_else(|| glyph.into_any_element())
}

/// Logged once per key, not once per frame: a missing asset would
/// otherwise print on every repaint of the surface that references it.
fn warn_missing(key: &str) {
    warn_once(&format!("registry:{key}"), &format!("[icons] asset missing from the embedded registry: {key:?} — falling back to the text placeholder"));
}

/// Prints `message` the FIRST time `key` is seen in this process, and never
/// again. Shared by every icon-side diagnostic (the embedded-registry miss
/// above, and ICON-2's three third-party degradation paths) because they
/// all sit on a render path that repaints many times a second — a log line
/// per frame would bury the journal it is supposed to help.
pub(crate) fn warn_once(key: &str, message: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let seen = SEEN.get_or_init(|| Mutex::new(HashSet::new()));
    let Ok(mut guard) = seen.lock() else {
        return;
    };
    if guard.insert(key.to_string()) {
        eprintln!("{message}");
    }
}

// ── Third-party app artwork: the OTHER rendering path ───────────────────
// ICON-2 (2026-08-22). Everything above this line draws the SHELL'S OWN
// monochrome assets through `gpui::svg()`. Everything below draws SOMEBODY
// ELSE'S artwork — an installed app's real icon, resolved from its
// `.desktop` `Icon=` against the system icon theme
// (`crate::apps::icon_resolve`). The two are deliberately different code
// paths and must not be merged:
//
//   | | shell assets (`icon_or_none`) | app artwork (`app_icon_element`) |
//   |-|-------------------------------|----------------------------------|
//   | element | `gpui::svg()` | `gpui::img()` |
//   | pixels | alpha mask, tinted by `text_color` | full colour, as authored |
//   | source | `include_bytes!` via `ShellAssets` | an absolute path on this machine |
//   | theming | one asset set serves light AND dark | never recoloured |
//   | multi-colour | needs one file per layer | one file, any number of colours |
//
// `svg()` CANNOT draw a third-party icon: it throws every colour away and
// keeps only coverage, so Chromium's four-colour disc would arrive as a
// flat silhouette. `img()` CANNOT replace the shell's own icons either: it
// bakes the file's authored colours into a bitmap, which is exactly why
// ICON-1 rejected it for the board artwork (one asset set could then no
// longer serve both themes).
//
// The theming rule follows from that, and is the research's
// (§2.4) as well: a third party's full-colour icon is **never recoloured**
// — changing it breaks their brand. Only the CONTAINER around it responds
// to the theme. The one exception is the generic fallback below, which is
// this shell's OWN asset and therefore goes back down the `svg()` path.

/// Draws one installed app's icon inside its tile — the "soft mask" the
/// research prescribes (§2.4): a shared rounded container that gives the
/// dock its visual rhythm, WITHOUT cropping the artwork inside it.
///
/// * `variant` is the already-resolved file for THIS container size
///   (`AppIcon::for_container`); `None` means the app has no drawable icon
///   and the generic application icon is drawn instead.
/// * `container_px` / `radius_px` are the tile's own size and corner
///   radius, passed in from the call site so this never invents a second
///   set of geometry — the dock's 44px/10px and the Launcher row's
///   30px/8px are unchanged from before ICON-2.
/// * `fallback_color` is the tint for the generic icon: each call site
///   passes the exact color its text placeholder used, so a degraded tile
///   is the same weight and hue it always was.
///
/// Three things this deliberately does NOT do: it never scales artwork up
/// (`icon_theme::draw_px` already capped `draw_px` at the file's own pixel
/// size), it never recolours it, and it never crops it unless
/// `variant.full_bleed` proved the file is a square opaque raster — see
/// `icon_theme::is_full_bleed_square` for why that proof is deliberately
/// hard to satisfy.
pub(crate) fn app_icon_element(
    variant: Option<&crate::apps::icon_resolve::AppIconVariant>,
    container_px: f32,
    radius_px: f32,
    fallback_color: Hsla,
) -> AnyElement {
    let Some(variant) = variant else {
        return generic_app_icon(crate::apps::icon_theme::content_px(container_px), fallback_color);
    };

    let path = variant.path.clone();
    let miss_key = format!("load:{}", path.display());
    // `img()`'s own fallback: the load happens asynchronously inside gpui
    // and can still fail after resolution succeeded (the file was deleted
    // between the scan and this frame, a PNG is corrupt, a decoder rejects
    // it). Degrading to the same generic icon — rather than to nothing —
    // is what keeps "the tile is always drawn" true on every path.
    let image = gpui::img(path).with_fallback(move || {
        warn_once(&miss_key, &format!("[app-icon] {miss_key} — the resolved icon file failed to load; drawing the generic application icon"));
        generic_app_icon(crate::apps::icon_theme::content_px(container_px), fallback_color)
    });

    if variant.full_bleed {
        // A provably square, provably opaque raster IS the tile: drawn edge
        // to edge and clipped to the container's own radius (the Apple/
        // Android "hard mask" behaviour, applied only where it cannot
        // damage anything). The clip lives on a dedicated wrapper rather
        // than on the tile itself because the tile's other children — the
        // verified-tier dot, the running-window dot — sit deliberately at
        // and beyond its edge and must NOT be clipped away.
        return div()
            .w(px(container_px))
            .h(px(container_px))
            .rounded(px(radius_px))
            .overflow_hidden()
            .flex_none()
            .child(image.w(px(container_px)).h(px(container_px)))
            .into_any_element();
    }

    // The normal case: free-form artwork, centred at its resolved size
    // inside the container, uncropped.
    image.w(px(variant.draw_px)).h(px(variant.draw_px)).flex_none().into_any_element()
}

/// This shell's own generic application icon, on the `svg()` path (it is a
/// shell asset, not third-party artwork). Falls back to nothing drawable
/// only if the embedded asset itself went missing, which
/// `every_registered_key_resolves_to_non_empty_bytes` makes impossible.
pub(crate) fn generic_app_icon(size: f32, color: Hsla) -> AnyElement {
    if bytes(APP_GENERIC).is_none() {
        warn_missing(APP_GENERIC);
        return div().into_any_element();
    }
    tinted_svg(APP_GENERIC, color, size).into_any_element()
}

// ── Slot → icon mapping ──────────────────────────────────────────────────
// Deliberately NOT an extra field on `apps::catalog::CatalogApp` /
// `fake_data::QuickTile` / `fake_data::LauncherFileResult`: "which vector
// icon draws this row" is a presentation concern with no business in data
// tables this crate also unit-tests for content fidelity against the
// boards, and those tables were being rewritten by a concurrent work
// package while this one landed. Keeping the mapping here as a pure
// `id -> layers` function leaves both files untouched and makes the mapping
// itself directly testable — which is what `every_*_key_is_registered`
// below actually tests.

/// The icon for an `apps::catalog::CatalogApp` — the Launcher's 「可安裝」
/// rows and the install-confirmation sheet, which key off the same
/// `CatalogApp::id`. `None` means the boards contain no icon for that
/// entry, and the caller keeps its text placeholder.
///
/// This is the ONE app-shaped slot that legitimately carries board artwork:
/// the catalog is a fixed, hand-authored list this crate ships (not a scan
/// of the machine), and its single entry is the same Chromium the board's
/// browser tile always depicted. Installed apps deliberately have no
/// mapping here — see `MAIL`'s doc comment.
///
/// Layers come back already paired with their palette tokens. Resolving the
/// color HERE rather than at the call site is deliberate: which token tints
/// which icon layer is knowledge read off the design boards, and keeping
/// all of it in this one module is what lets the light and dark boards
/// share a single asset set.
pub(crate) fn catalog_layers(catalog_id: &str, palette: ShellPalette) -> Option<Vec<Layer>> {
    match catalog_id {
        "catalog-chromium" => Some(vec![(GLOBE, palette.icon_globe())]),
        _ => None,
    }
}

/// The icon for a `fake_data::QuickTile` (ControlCenter's Wi-Fi / Bluetooth
/// / Do-not-disturb tiles). `active` picks the stroke the boards use for an
/// ON tile (`#fafafa` on the brand fill) versus an OFF one — the same
/// distinction `overlay/controlcenter.rs` already draws for the tile's own
/// title and subtitle.
pub(crate) fn quick_tile_layers(tile_id: &str, palette: ShellPalette, active: bool) -> Option<Vec<Layer>> {
    let hex = if active { palette.brand_foreground } else { palette.icon_control() };
    let key = match tile_id {
        "tile-wifi" => WIFI_TILE,
        "tile-bluetooth" => BLUETOOTH,
        "tile-dnd" => MOON,
        _ => return None,
    };
    Some(vec![(key, hex)])
}

/// The icon for a `fake_data::LauncherFileResult` row. The spreadsheet is
/// two layers (green body, white rules); the folder is one filled shape.
/// Both keep IDENTITY colors — a folder is blue and an xlsx is green in
/// both boards — which is why `glyph_hex` is threaded in from the row
/// itself rather than resolved from a theme token (the same convention
/// `overlay/launcher.rs` already documents for that field).
pub(crate) fn launcher_file_layers(file_id: &str, glyph_hex: u32, palette: ShellPalette) -> Option<Vec<Layer>> {
    match file_id {
        "launcher-file-folder" => Some(vec![(FOLDER_FILLED, glyph_hex)]),
        "launcher-file-xlsx" => Some(vec![(SPREADSHEET_BODY, glyph_hex), (SPREADSHEET_LINES, palette.icon_on_colored_tile())]),
        _ => None,
    }
}

// ── ICON-3 slot → icon mapping ───────────────────────────────────────────
// Same convention the four helpers above establish: a pure `id -> layers`
// function keyed by a STABLE `&str` slug (never a display label, which
// varies by locale), returning layers already paired with their palette
// token — so "which token tints which layer" is knowledge that lives in this
// one module, and one asset set keeps serving both themes.

/// The five accessibility categories the OOBE language step lists, keyed by
/// `crate::oobe::steps::language::A11yCategory::slug()`. Tinted at
/// text-ladder rank 2 (`#52525c` light — the boards' own literal for these
/// rows).
pub(crate) fn a11y_category_layers(slug: &str, palette: ShellPalette) -> Option<Vec<Layer>> {
    let key = match slug {
        "a11y-seeing" => A11Y_SEEING,
        "a11y-hearing" => A11Y_HEARING,
        "a11y-typing" => A11Y_TYPING,
        "a11y-pointing" => A11Y_POINTING,
        "a11y-zoom" => A11Y_ZOOM,
        _ => return None,
    };
    Some(vec![(key, palette.text_secondary)])
}

/// The Privacy step's four rows, keyed by `crate::oobe::PrivacyToggle::
/// slug()` — the SAME slug that already ids the row's own element, so a
/// fifth toggle added to that enum without an icon here fails the
/// `every_privacy_toggle_has_an_icon` test rather than silently rendering a
/// gap where its neighbours have icons.
pub(crate) fn privacy_toggle_layers(slug: &str, palette: ShellPalette) -> Option<Vec<Layer>> {
    let key = match slug {
        "usage-stats" => CHART_BARS,
        "error-reports" => ALERT_TRIANGLE,
        "personalization" => SLIDERS,
        "marketing" => BELL,
        _ => return None,
    };
    Some(vec![(key, palette.text_secondary)])
}

/// The five-level Wi-Fi signal family. `bars` is
/// `crate::oobe::network::AccessPoint::signal_bars` (0..=4), which maps 1:1
/// onto the board's own `none`/`weak`/`ok`/`good`/`excellent` — a value
/// above 4 is clamped rather than refused, since a backend reporting 7 bars
/// still means "as strong as it gets", not "draw nothing".
///
/// Every level draws all four layers; only the TINT changes, exactly as the
/// board does it ("同一組同心弧，只換顏色階…不另外出五張圖").
pub(crate) fn wifi_signal_layers(bars: u8, palette: ShellPalette) -> Vec<Layer> {
    let level = bars.min(4);
    let on = palette.brand;
    let off = palette.icon_inactive();
    vec![
        (WIFI_ARC_OUTER, if level >= 4 { on } else { off }),
        (WIFI_ARC_MID, if level >= 3 { on } else { off }),
        (WIFI_ARC_INNER, if level >= 2 { on } else { off }),
        (WIFI_DOT, if level >= 1 { on } else { off }),
    ]
}

/// The same four layers as `wifi_signal_layers`, all tinted alike — for the
/// slots that draw the Wi-Fi GLYPH rather than a signal READING (the OOBE
/// Network step's 32px title icon). Kept as its own entry point instead of
/// `wifi_signal_layers(4, ..)` so a title icon can never accidentally read
/// as "excellent signal".
pub(crate) fn wifi_plain_layers(hex: u32) -> Vec<Layer> {
    vec![(WIFI_ARC_OUTER, hex), (WIFI_ARC_MID, hex), (WIFI_ARC_INNER, hex), (WIFI_DOT, hex)]
}

/// Which cursor shape a pointer-settings preview draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorShape {
    /// The plain arrow — the ONE shape the brand theme replaces.
    Default,
    /// The I-beam. Identical in both cards.
    Text,
    /// The pointing hand. Identical in both cards: 「爪印不覆蓋 pointer」是
    /// 既定拍板 (a paw loses the hand's directional meaning).
    Pointer,
}

/// A cursor preview's layers. These are the ONLY icons in this module whose
/// colours are IDENTITY, never themed: they depict real cursor artwork,
/// which looks the same on every background by design (that is the entire
/// point of a dark outline around a light interior). `brand_theme` picks the
/// paw over the arrow for `CursorShape::Default` and changes nothing else.
pub(crate) fn cursor_shape_layers(shape: CursorShape, brand_theme: bool) -> Vec<Layer> {
    /// The cursor artwork's outline, `#1C1917` — the shipping asset's own
    /// value (`crates/duduclaw-comp/assets/cursors/svg/default.svg`).
    const CURSOR_OUTLINE: u32 = 0x1c1917;
    /// The system arrow / I-beam halo interior.
    const CURSOR_WHITE: u32 = 0xffffff;
    /// DuDuClaw's brand crimson — the paw's fill, same shipping asset.
    const CURSOR_BRAND: u32 = 0xe85055;

    match (shape, brand_theme) {
        (CursorShape::Default, false) => vec![(CURSOR_ARROW_OUTLINE, CURSOR_OUTLINE), (CURSOR_ARROW_FILL, CURSOR_WHITE)],
        (CursorShape::Default, true) => vec![(CURSOR_PAW_OUTLINE, CURSOR_OUTLINE), (CURSOR_PAW_FILL, CURSOR_BRAND)],
        (CursorShape::Text, _) => vec![(CURSOR_TEXT_HALO, CURSOR_WHITE), (CURSOR_TEXT_CORE, CURSOR_OUTLINE)],
        (CursorShape::Pointer, _) => vec![(CURSOR_HAND_OUTLINE, CURSOR_OUTLINE), (CURSOR_HAND_FILL, CURSOR_WHITE)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AssetSource as _;

    /// The load-bearing one. A key referenced by a call site but absent
    /// from `ICONS` is a hole in the UI, and the render-time fallback is
    /// designed to HIDE that (it quietly draws the old placeholder) — so
    /// the miss has to fail here instead.
    #[test]
    fn every_registered_key_resolves_to_non_empty_bytes() {
        for (key, _) in ICONS {
            let found = bytes(key).unwrap_or_else(|| panic!("{key} is registered but does not resolve"));
            assert!(!found.is_empty(), "{key} resolved to zero bytes");
        }
    }

    /// Resolving is not the same as being drawable: `paint_svg` swallows a
    /// parse error and paints nothing, and the byte-level guard above
    /// cannot see that. This runs the exact `usvg` configuration the
    /// renderer uses (`gpui::SvgRenderer`, the type `Application::
    /// with_assets` installs) over every payload.
    #[test]
    fn every_registered_asset_parses_as_svg() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        for (key, data) in ICONS {
            assert!(renderer.parse_svg(data).is_ok(), "{key} does not parse as SVG");
        }
    }

    /// The alpha mask is built from what actually rasterizes, so a document
    /// that parses but paints nothing (a typo'd path, a zero-size viewBox)
    /// would still reach the screen as a blank hole.
    #[test]
    fn every_registered_asset_rasterizes_to_a_non_empty_image() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(()));
        for (key, data) in ICONS {
            let parsed = renderer.parse_svg(data).unwrap_or_else(|e| panic!("{key} failed to parse: {e}"));
            let image = renderer
                .render_parsed(&parsed, gpui::SvgSize::ExactSize(gpui::size(gpui::DevicePixels(48), gpui::DevicePixels(48))))
                .unwrap_or_else(|e| panic!("{key} failed to rasterize: {e}"));
            let size = image.size(0);
            assert!(i32::from(size.width) > 0 && i32::from(size.height) > 0, "{key} rasterized to an empty image");
        }
    }

    #[test]
    fn keys_are_unique() {
        let mut keys: Vec<&str> = ICONS.iter().map(|(k, _)| *k).collect();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), ICONS.len(), "duplicate key in ICONS");
    }

    #[test]
    fn every_key_lives_under_the_icons_prefix_and_ends_in_svg() {
        for (key, _) in ICONS {
            assert!(key.starts_with("icons/"), "{key} is outside the icons/ namespace");
            assert!(key.ends_with(".svg"), "{key} is not a .svg path");
        }
    }

    #[test]
    fn unknown_paths_resolve_to_none() {
        assert!(bytes("icons/does-not-exist.svg").is_none());
        assert!(bytes("").is_none());
        // Not a prefix match — a lookup must be the whole key, so a
        // truncated or extended path can never silently resolve to a
        // neighbour.
        assert!(bytes("icons/mail").is_none());
        assert!(bytes("icons/mail.svg.bak").is_none());
    }

    #[test]
    fn asset_source_load_matches_the_registry() {
        for (key, data) in ICONS {
            let loaded = ShellAssets.load(key).expect("load must not error").expect("every registered key must load");
            assert_eq!(loaded.as_ref(), *data, "{key} loaded different bytes than the registry holds");
        }
        assert!(ShellAssets.load("icons/nope.svg").expect("a miss is Ok(None), not an error").is_none());
    }

    #[test]
    fn asset_source_list_enumerates_the_namespace() {
        let listed = ShellAssets.list("icons/").expect("list must not error");
        assert_eq!(listed.len(), ICONS.len());
        assert!(ShellAssets.list("fonts/").expect("list must not error").is_empty());
    }

    // ── Slot mapping ─────────────────────────────────────────────────────

    /// Every mapped slot must point at keys that actually exist — the
    /// mapping is where a typo would otherwise land, and a typo'd key
    /// degrades to the old text placeholder silently at runtime.
    #[test]
    fn every_catalog_entry_that_is_mapped_resolves_to_registered_keys() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for entry in crate::apps::catalog::INSTALL_CATALOG {
                let Some(layers) = catalog_layers(entry.id, palette) else {
                    continue;
                };
                assert!(!layers.is_empty(), "{} maps to an empty layer list", entry.id);
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "{} maps to unregistered key {key}", entry.id);
                }
            }
        }
    }

    /// The catalog is small and hand-authored, so "every entry has an icon"
    /// is a claim worth holding to — an entry added without one silently
    /// falls back to a CJK character, which is the state this whole work
    /// package exists to end.
    #[test]
    fn every_catalog_entry_has_an_icon() {
        let palette = ShellPalette::light();
        for entry in crate::apps::catalog::INSTALL_CATALOG {
            assert!(catalog_layers(entry.id, palette).is_some(), "catalog entry {} has no icon mapping", entry.id);
        }
    }

    #[test]
    fn every_mapped_quick_tile_key_is_registered() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for active in [false, true] {
                for tile in crate::fake_data::QUICK_TILES {
                    let layers = quick_tile_layers(tile.id, palette, active).unwrap_or_else(|| panic!("{} has no icon mapping", tile.id));
                    for (key, _) in &layers {
                        assert!(bytes(key).is_some(), "{} maps to unregistered key {key}", tile.id);
                    }
                }
            }
        }
    }

    #[test]
    fn every_mapped_launcher_file_key_is_registered() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for row in crate::fake_data::LAUNCHER_FILE_RESULTS {
                let layers =
                    launcher_file_layers(row.id, row.glyph_hex, palette).unwrap_or_else(|| panic!("{} has no icon mapping", row.id));
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "{} maps to unregistered key {key}", row.id);
                }
            }
        }
    }

    // ── ICON-3 (2026-08-23) ──────────────────────────────────────────────

    /// The operator's ruling ⑤ is explicitly all-or-nothing: the board's own
    /// note says 「要翻案就是整頁四列都加，不能只加一半」. A fifth privacy
    /// toggle added without an icon would ship exactly that half-iconed list,
    /// and would do it silently (the row would just render without one), so
    /// it has to fail here.
    #[test]
    fn every_privacy_toggle_has_an_icon() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for toggle in crate::oobe::PrivacyToggle::ALL {
                let layers = privacy_toggle_layers(toggle.slug(), palette)
                    .unwrap_or_else(|| panic!("privacy toggle {} has no icon mapping", toggle.slug()));
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "{} maps to unregistered key {key}", toggle.slug());
                }
            }
        }
    }

    /// Same guarantee for the language step's five accessibility rows: a
    /// sixth category without an icon would render as a gap next to five
    /// that have one.
    #[test]
    fn every_accessibility_category_has_an_icon() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for category in crate::oobe::A11yCategory::ALL {
                let layers = a11y_category_layers(category.slug(), palette)
                    .unwrap_or_else(|| panic!("accessibility category {} has no icon mapping", category.slug()));
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "{} maps to unregistered key {key}", category.slug());
                }
            }
        }
    }

    /// The five-level family, level by level. The board's own strip is the
    /// spec: `none` lights nothing, each step up lights one more arc from
    /// the inside out, `excellent` lights all four.
    #[test]
    fn the_wifi_signal_family_lights_one_more_layer_per_level() {
        let palette = ShellPalette::light();
        let on = palette.brand;
        let lit = |bars: u8| wifi_signal_layers(bars, palette).iter().filter(|(_, hex)| *hex == on).count();
        assert_eq!(lit(0), 0, "level `none` must light nothing at all");
        assert_eq!(lit(1), 1, "level `weak` lights only the dot");
        assert_eq!(lit(2), 2);
        assert_eq!(lit(3), 3);
        assert_eq!(lit(4), 4, "level `excellent` lights every arc");
    }

    /// A backend reporting more bars than the family has levels still means
    /// "as strong as it gets" — clamped, never a panic and never a blank
    /// indicator.
    #[test]
    fn an_out_of_range_signal_reading_clamps_to_the_top_level() {
        let palette = ShellPalette::light();
        assert_eq!(wifi_signal_layers(200, palette), wifi_signal_layers(4, palette));
    }

    /// Every level draws all four layers regardless — the board changes
    /// colour, never the shape ("只換顏色階…不另外出五張圖").
    #[test]
    fn every_wifi_level_draws_the_same_four_registered_layers() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            for bars in 0..=4u8 {
                let layers = wifi_signal_layers(bars, palette);
                assert_eq!(layers.len(), 4, "level {bars} drew a different number of layers");
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "level {bars} maps to unregistered key {key}");
                }
            }
        }
        for (key, _) in wifi_plain_layers(0x000000) {
            assert!(bytes(key).is_some(), "{key} is not registered");
        }
    }

    /// The unlit arcs must be a DIFFERENT colour from the lit ones in both
    /// themes — a regression here shows up on screen as a signal indicator
    /// that always reads full strength.
    #[test]
    fn lit_and_unlit_wifi_arcs_differ_in_both_themes() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            assert_ne!(palette.brand, palette.icon_inactive());
        }
    }

    /// The pointer-settings cursor previews. Two layers each (outline +
    /// fill), both registered, and — the load-bearing part — the brand theme
    /// changes ONLY the default arrow: 「爪印不覆蓋 pointer」是既定拍板, and
    /// the same is true of the I-beam.
    #[test]
    fn only_the_default_cursor_shape_changes_with_the_brand_theme() {
        for shape in [CursorShape::Default, CursorShape::Text, CursorShape::Pointer] {
            for brand in [false, true] {
                let layers = cursor_shape_layers(shape, brand);
                assert_eq!(layers.len(), 2, "{shape:?} (brand={brand}) is not an outline+fill pair");
                for (key, _) in &layers {
                    assert!(bytes(key).is_some(), "{shape:?} maps to unregistered key {key}");
                }
            }
        }
        assert_ne!(cursor_shape_layers(CursorShape::Default, false), cursor_shape_layers(CursorShape::Default, true));
        assert_eq!(cursor_shape_layers(CursorShape::Text, false), cursor_shape_layers(CursorShape::Text, true));
        assert_eq!(cursor_shape_layers(CursorShape::Pointer, false), cursor_shape_layers(CursorShape::Pointer, true));
    }

    /// A cursor's two layers must never be the same colour, or the artwork
    /// collapses into a flat silhouette and the outline disappears.
    #[test]
    fn each_cursor_previews_outline_and_fill_are_different_colours() {
        for shape in [CursorShape::Default, CursorShape::Text, CursorShape::Pointer] {
            for brand in [false, true] {
                let layers = cursor_shape_layers(shape, brand);
                assert_ne!(layers[0].1, layers[1].1, "{shape:?} (brand={brand}) would render as one flat colour");
            }
        }
    }

    #[test]
    fn unknown_slot_ids_map_to_none() {
        let palette = ShellPalette::light();
        assert!(catalog_layers("catalog-nope", palette).is_none());
        assert!(quick_tile_layers("tile-nope", palette, true).is_none());
        assert!(launcher_file_layers("launcher-file-nope", 0x000000, palette).is_none());
        assert!(privacy_toggle_layers("privacy-nope", palette).is_none());
        assert!(a11y_category_layers("a11y-nope", palette).is_none());
        // Not a prefix match either — the same "the lookup is the WHOLE key"
        // contract `unknown_paths_resolve_to_none` pins for asset paths.
        assert!(privacy_toggle_layers("usage", palette).is_none());
        assert!(a11y_category_layers("a11y-seeing-extra", palette).is_none());
    }

    /// The whole point of one asset set serving two themes: a mapped layer
    /// must resolve to a DIFFERENT tint in light and dark wherever the
    /// boards themselves differ, and to the SAME one where they do not
    /// (identity colors: white on a colored tile, the amber pencil, a blue
    /// folder). A regression here shows up on screen as an icon that
    /// vanishes into its own background in one theme.
    #[test]
    fn icon_tints_follow_the_theme_where_the_boards_do() {
        let globe_light = catalog_layers("catalog-chromium", ShellPalette::light()).expect("chromium is mapped");
        let globe_dark = catalog_layers("catalog-chromium", ShellPalette::dark()).expect("chromium is mapped");
        assert_ne!(globe_light[0].1, globe_dark[0].1, "the globe is themed");

        // The spreadsheet's green body is identity in both themes; its
        // white rules are the shared colored-tile white, also identity.
        let sheet_light = launcher_file_layers("launcher-file-xlsx", 0x21a366, ShellPalette::light()).expect("mapped");
        let sheet_dark = launcher_file_layers("launcher-file-xlsx", 0x21a366, ShellPalette::dark()).expect("mapped");
        assert_eq!(sheet_light.len(), 2);
        assert_eq!(sheet_light[0].1, sheet_dark[0].1);
        assert_eq!(sheet_light[1].1, sheet_dark[1].1);
    }

    #[test]
    fn quick_tile_active_and_inactive_tints_differ() {
        for palette in [ShellPalette::light(), ShellPalette::dark()] {
            let on = quick_tile_layers("tile-wifi", palette, true).expect("mapped");
            let off = quick_tile_layers("tile-wifi", palette, false).expect("mapped");
            assert_ne!(on[0].1, off[0].1);
        }
    }

    /// See the block comment on `MAIL`. A test fixture rather than a
    /// crate-level const precisely BECAUSE nothing in the shipping binary
    /// consumes it — the whole claim being pinned is that these keys have
    /// no call site.
    const UNBOUND_APP_ICONS: &[&str] = &[MAIL, DOCUMENT_OUTLINE, DOCUMENT_LINES, DOCUMENT_PENCIL, MUSIC, CHAT, FOLDER];

    /// Pins the boundary `MAIL`'s doc comment describes: the board's five
    /// conceptual app icons ship but have no call site, because the dock
    /// and the Launcher render a real inventory now and a real app's icon
    /// comes from its own `Icon=`. If a future round binds one of these,
    /// this list should shrink deliberately — and if someone adds a new
    /// asset and forgets to wire it, it will not silently join them.
    #[test]
    fn unbound_app_icons_are_shipped_and_still_unbound() {
        for key in UNBOUND_APP_ICONS {
            assert!(bytes(key).is_some(), "{key} is listed as shipped-but-unbound yet does not resolve");
        }
        let palette = ShellPalette::light();
        let bound: Vec<&str> = ICONS
            .iter()
            .map(|(k, _)| *k)
            .filter(|k| {
                crate::apps::catalog::INSTALL_CATALOG.iter().any(|e| {
                    catalog_layers(e.id, palette).is_some_and(|layers| layers.iter().any(|(lk, _)| lk == k))
                })
            })
            .collect();
        for key in UNBOUND_APP_ICONS {
            assert!(!bound.contains(key), "{key} is listed as unbound but the catalog mapping uses it");
        }
    }
}
