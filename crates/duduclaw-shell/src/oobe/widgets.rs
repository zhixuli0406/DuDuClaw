// Shared OOBE visual primitives — Shell-S1.
//
// OOBE has no `.dc.html` design board this round (see `fake_data.rs`'s own
// header comment), so these are originated for this task — but follow the
// SAME token discipline `home.rs` and `overlay/*.rs` already establish:
// every color/radius/shadow/text-size comes from `duduclaw_native_gui::
// theme::*` / `theme::RADIUS_*` / `theme::TEXT_*`, nothing ad-hoc. Buttons
// are hand-built `div()` composition, NOT `mds_gpui::button()` — that
// facade is hardwired to the crate's DARK re-exported top-level tokens
// (`home.rs`'s own header comment explains why), which would paint OOBE's
// light surfaces wrong, exactly the same reason `home.rs` hand-rolls its
// own elements instead of using the facade.
//
// ── Theme (2026-08-20) ────────────────────────────────────────────────
// Every helper below that paints anything now takes a `ShellPalette` (see
// `palette.rs`) instead of reaching for `theme::light::*` directly — passed
// BY VALUE (the type is `Copy`, ~70 bytes of plain fields) rather than by
// reference, which sidesteps borrow-checker friction against the `move`
// closures several of these helpers build (`step_button`'s hover closure,
// `toggle_pill`'s caller-supplied click handler, …). Every call site resolves
// its palette the same way `locale` already is (`flow.palette()`, fresh per
// render call — see that method's own doc comment in `oobe/mod.rs`), so
// there is nothing to keep in sync here: swap the operator's pick and the
// very next render call threads a different `ShellPalette` through this
// entire file. The one exception is `OobeTextField` below, which reads the
// ambient `ShellPalette` global instead — see its own doc comment for why.

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, CursorStyle, Div, Entity, FocusHandle, Focusable, FontWeight, MouseButton, Render,
    SharedString, Stateful, Window,
};

use duduclaw_native_gui::ime_input::{ImeTextInput, TextInputStyle};
use duduclaw_native_gui::theme;

use crate::palette::ShellPalette;
// Only needed by `theme_preview` below (promoted here from `oobe/steps/
// theme.rs` — see that fn's own doc comment) — `super::` since `widgets` is
// a direct child of `oobe`, the module that re-exports this type.
use super::ThemeChoice;

// Y20-P2 (2026-08-29): `title`/`subtitle`/`card`/`step_button`/
// `progress_dots`/`StepButtonVariant` are promoted `pub(super)` ->
// `pub(crate)` here so `crate::live_install` (the live-image installer
// wizard's own separate flow, 4 steps at the time of this promotion, 6 as of
// installer-settings-integration WP1 below — see that module's own header
// comment for why it's a SEPARATE state machine from `OobeFlow`, not another
// `OobeStep`) can reuse these visual primitives instead of re-deriving
// near-identical copies. They already take plain `ShellPalette`/`&str`/
// `usize` parameters with zero `OobeFlow`/`OobeStep` coupling (see this
// file's own header comment), so widening reach is the only change — no
// signature or behavior differs for any existing `oobe::*` call site.
// `subtitle_dynamic`/`toggle_pill`/the `OobeTextField` family stay
// `pub(super)`/`pub(crate)` at their prior scope: nothing outside `oobe`
// needs them yet.
//
// Installer-settings-integration WP1 (2026-08-29): `theme_preview` (the
// `Theme` step's two mini-desktop illustration cards) is promoted the same
// way, moved bodily from `oobe/steps/theme.rs` — see that fn's own doc
// comment below for the full "why literal hex, not palette" reasoning it
// carries with it. `crate::live_install::steps::theme` is the new consumer;
// `oobe::steps::theme::render` keeps calling it too, unchanged.
pub(crate) fn title(text: &'static str, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_2XL)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.foreground, 1.0)).child(text)
}

pub(crate) fn subtitle(text: &'static str, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_BASE)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text)
}

/// Same as `subtitle`, for a runtime-computed string (e.g. "已連線：
/// <ssid>") — `.child()` accepts an owned `String` directly (gpui's own
/// `impl IntoElement for String`), so this is just the `&'static str`
/// version's twin with a different parameter type.
pub(super) fn subtitle_dynamic(text: String, palette: ShellPalette) -> Div {
    div().text_size(px(theme::TEXT_BASE)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text)
}

/// The one "card" every step's content area sits inside — same
/// `surface_shadow()` + `RADIUS_XL` + `border()` recipe `overlay/
/// notifications.rs`'s own floating panel uses, just full-width within the
/// step's centered column instead of docked to a screen edge.
pub(crate) fn card(content: impl IntoElement, palette: ShellPalette) -> Div {
    div()
        .w_full()
        .bg(theme::alpha(palette.surface, 1.0))
        .border_1()
        .border_color(palette.border())
        .rounded(px(theme::RADIUS_XL))
        .shadow(palette.surface_shadow())
        .p(px(24.))
        .flex()
        .flex_col()
        .gap(px(16.))
        .child(content)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StepButtonVariant {
    Primary,
    Secondary,
    Ghost,
}

/// Hand-rolled button — see this file's header comment for why it isn't
/// `mds_gpui::button()`. `disabled` drops the click handler entirely (not
/// just a visual dim), matching that facade's own documented disabled
/// contract (`duduclaw-native-gui/src/mds_gpui/button.rs`: "no hover/active
/// styles and NO click handler attached at all").
pub(crate) fn step_button(
    id: &'static str,
    label: &'static str,
    variant: StepButtonVariant,
    disabled: bool,
    palette: ShellPalette,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let (bg, bg_hover, text) = match variant {
        StepButtonVariant::Primary => (palette.brand, palette.brand, palette.brand_foreground),
        StepButtonVariant::Secondary => (palette.secondary, palette.surface_hover, palette.secondary_foreground),
        StepButtonVariant::Ghost => (palette.app_shell, palette.surface_hover, palette.muted_foreground),
    };

    let mut el = div()
        .id(id)
        .h(px(36.))
        .px(px(18.))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_LG))
        .text_size(px(theme::TEXT_SM))
        .font_weight(FontWeight::MEDIUM)
        .bg(theme::alpha(if disabled { palette.muted } else { bg }, 1.0))
        .text_color(theme::alpha(if disabled { palette.muted_foreground } else { text }, 1.0))
        .child(label);

    if !disabled {
        el = el.cursor_pointer().hover(move |style| style.bg(theme::alpha(bg_hover, 0.90))).on_click(on_click);
    }

    el
}

/// The fixed light/dark mini-desktop illustration used by the `Theme` step's
/// two option cards — promoted here from `oobe/steps/theme.rs`
/// (installer-settings-integration WP1, 2026-08-29) so `live_install::steps::
/// theme` can reuse it verbatim instead of re-deriving a near-identical copy,
/// same "promote to `widgets` for crate-wide reuse" precedent this file's own
/// header comment already sets for `title`/`subtitle`/`card`/`step_button`/
/// `progress_dots` (Y20-P2). `oobe::steps::theme::render` now calls this same
/// fn rather than owning its own private copy — behavior is byte-identical
/// for every existing OOBE call site, only the fn's address moved.
///
/// See the ORIGINAL file's header comment (still accurate, reproduced here
/// since the illustration's own reasoning travels with the code, not the
/// call site) for why every color below is a literal hex/rgba, never
/// `palette`/`theme::light::*`/`theme::dark::*`:
///
/// The two mini-desktop PREVIEWS inside each option card are a fixed
/// side-by-side illustration of what light/dark look like — not live chrome.
/// They stay pixel-identical to the design board regardless of which theme
/// is currently active: the dark preview must still look dark even while the
/// operator is ON the light theme (and vice versa), since showing both
/// options side by side at once is the entire point of this screen. So this
/// fn uses literal hex/rgba copied straight from the board, never
/// `palette`/`theme::light::*`/`theme::dark::*` — including two spots where
/// the board itself does NOT reuse the exact semantic token: the dark
/// preview's window-pane border is authored as `rgba(255,255,255,0.12)`,
/// distinct from the top-bar hairline's `rgba(255,255,255,0.10)`
/// (`theme::dark::SURFACE_BORDER_ALPHA`) — two different translucent-white
/// weights in the same illustration, both kept exactly as drawn — and BOTH
/// previews' brand pill is the LIGHT theme's brand hex `#2171cc`, never
/// `theme::dark::BRAND`'s brighter `#4390ee` (the board's own dark-preview
/// swatch was authored with the light brand color, so that is what ships
/// here too).
pub(crate) fn theme_preview(choice: ThemeChoice) -> Div {
    let (bg, bar_bg, bar_border, pane_bg, pane_border, line1, line2) = match choice {
        ThemeChoice::Light => (
            theme::alpha(0xf3f3f4, 1.0),
            theme::alpha(0xffffff, 1.0),
            theme::alpha(0xececef, 1.0),
            theme::alpha(0xffffff, 1.0),
            theme::alpha(0xe4e4e7, 1.0),
            theme::alpha(0xe4e4e7, 1.0),
            theme::alpha(0xececef, 1.0),
        ),
        ThemeChoice::Dark => (
            theme::alpha(0x0c0c0e, 1.0),
            theme::alpha(0x18181b, 1.0),
            theme::alpha(0xffffff, 0.10),
            theme::alpha(0x18181b, 1.0),
            theme::alpha(0xffffff, 0.12),
            theme::alpha(0xffffff, 0.22),
            theme::alpha(0xffffff, 0.12),
        ),
    };
    // Same literal in both branches on purpose — see this fn's own doc
    // comment ("both previews' brand pill is the LIGHT theme's brand hex").
    let brand_pill = theme::alpha(0x2171cc, 1.0);

    div()
        .relative()
        .h(px(140.))
        .rounded(px(8.))
        .overflow_hidden()
        .bg(bg)
        .child(div().absolute().top(px(0.)).left(px(0.)).right(px(0.)).h(px(14.)).bg(bar_bg).border_b_1().border_color(bar_border))
        .child(div().absolute().top(px(26.)).left(px(22.)).w(px(160.)).h(px(86.)).rounded(px(6.)).bg(pane_bg).border_1().border_color(pane_border))
        .child(div().absolute().top(px(40.)).left(px(34.)).w(px(90.)).h(px(6.)).rounded(px(3.)).bg(line1))
        .child(div().absolute().top(px(54.)).left(px(34.)).w(px(60.)).h(px(6.)).rounded(px(3.)).bg(line2))
        .child(div().absolute().top(px(78.)).left(px(34.)).w(px(44.)).h(px(12.)).rounded(px(6.)).bg(brand_pill))
}

/// Low-key step-progress dots (task brief: "進度指示（低調 dots）"). Kept
/// deliberately, per `research/native-os-2026-08/oobe-first-run-
/// reference.md` §B-4's own list of reusable web assets ("StepDots 視覺語
/// 彙（同 KDE dots...）") — the survey's majority finding that macOS/
/// Windows/ChromeOS show NO progress indicator is explicitly attributed to
/// their step COUNT being dynamic/conditional ("步驟數動態做不了誠實步驟
/// 條", §A "主要分歧" progress-indicator row) which does not apply here:
/// DuDuClaw OS's OOBE is a fixed ten-step linear sequence, so a dot per
/// step is an honest indicator, not a promise the flow can't keep.
///
/// ── ICON-3 (2026-08-23): re-measured against the board ────────────────
/// `OOBE-ProgressAndIcons.dc.html`'s own before/after strip replaces the
/// 16×6 pill + 6×6 dots (top of the content column) with **12px / 8px
/// CIRCLES at gap 8, in the bottom toolbar** — the measurements it lifts
/// from Ubuntu, the one surveyed OS that shows a step indicator at all.
/// The move to the toolbar is `render::button_row`'s doing; this fn only
/// owns the dots' own geometry.
///
/// Honest gap: the same board also asks for a 「第 N 步，共 10 步」 screen-
/// reader label. gpui exposes no accessibility/AT API at the pinned rev
/// (nothing in `gpui::` sets an accessible name or role), so there is
/// nowhere to put one — and rendering it as VISIBLE text would be a
/// different design than the board's, not an implementation of it. Left
/// undone and reported, rather than faked.
pub(crate) fn progress_dots(current_index: usize, total: usize, palette: ShellPalette) -> Div {
    let mut row = div().flex().items_center().justify_center().gap(px(8.));
    for i in 0..total {
        let active = i == current_index;
        let size = if active { 12. } else { 8. };
        row = row.child(
            div()
                .w(px(size))
                .h(px(size))
                .rounded(px(size))
                // The board paints an unlit dot `#d4d4d8`, which is exactly
                // `icon_inactive()` in light — switched off `surface_border`
                // (a near-miss inherited from the pill-shaped original) so
                // the dots match the board they were re-measured from.
                .bg(theme::alpha(if active { palette.brand } else { palette.icon_inactive() }, 1.0)),
        );
    }
    row
}

/// Pill-shaped toggle switch — the `Privacy` step's four opt-in rows (task
/// brief step 6: "3-4 個 opt-in 開關"). Palette-driven re-derivation of
/// `overlay/controlcenter.rs`'s own `toggle_pill` (same shape: a rounded
/// track + a circular handle that slides left/right), not a shared function
/// — that one (as of Shell-S1) IS palette-aware too, but its
/// "off" track color is a bespoke gray (`#e4e4e7`/`#3f3f46`) private to that
/// file's own ControlCenter-specific literals, distinct from this step's
/// own `surface_border`-based off state, so sharing one fn would mean
/// threading a THIRD divergent color parameter through both call sites for
/// no real gain — and pulling the type in would still mean either reaching
/// across module boundaries into a sibling `overlay::` submodule that
/// doesn't expose it (`fn toggle_pill` there is private to
/// `controlcenter.rs`) or making it public for a single OOBE call site — a
/// one-screen-only re-derivation is the smaller change either way. Purely
/// presentational — no click handling of its own
/// (the caller's own `.on_click(...)` sits on the row, not this pill, same
/// division `controlcenter.rs`'s `switch_row`/`toggle_pill` split
/// establishes). The knob itself stays hardcoded white regardless of theme —
/// same convention iOS/macOS toggle switches use (a white knob reads
/// correctly against both a colored "on" track and a muted "off" track in
/// either theme), so this is not a residual un-palette-driven color, it's
/// the one part of this widget the design intentionally never re-skins.
pub(super) fn toggle_pill(on: bool, palette: ShellPalette) -> Div {
    let track = if on { theme::alpha(palette.brand, 1.0) } else { palette.surface_border };
    let mut handle = div().absolute().top(px(2.)).w(px(19.)).h(px(19.)).rounded(px(19.)).bg(theme::alpha(0xffffff, 1.0));
    handle = if on { handle.right(px(2.)) } else { handle.left(px(2.)) };
    div().relative().w(px(40.)).h(px(23.)).rounded(px(23.)).bg(track).child(handle)
}

/// Single-line editable text field entity — the chrome (rounded surface,
/// focus ring, masking policy) around ONE shared
/// `duduclaw_native_gui::ime_input::ImeTextInput`, which owns the actual
/// text buffer, caret/selection painting and — the point of D3-b — the
/// `gpui::EntityInputHandler` implementation that makes OS-level IME
/// composition reach this widget at all.
///
/// ── What changed in D3-b (2026-08-23) and why ─────────────────────────────
/// Until now this type held a bare `content: String` and appended
/// `keystroke.key_char` from its own `on_key_down`. That is the exact shape
/// `research/native-os-2026-08/ime-fcitx5-gpui-2026-08.md` §2.3 predicted
/// would fail on DuDuClaw OS: with no `EntityInputHandler` installed, gpui's
/// Wayland backend drops every `zwp_text_input_v3` commit on the floor
/// (`WaylandWindow::handle_ime` early-returns when `input_handler` is
/// `None`), so an operator with fcitx5 running would see English type fine
/// and Chinese do nothing at all. The buffer/composition/hit-test machinery
/// now comes from the shared widget instead of being re-derived here; this
/// file keeps only what is genuinely shell-specific — the palette-driven
/// chrome and the masked/plain decision.
///
/// The inner entity is NOT exposed: callers keep using `Entity<
/// OobeTextField>` exactly as before, so `AccountFields`/`NetworkFields`/
/// `LockPasswordField` and every render call site are unchanged apart from
/// `content` becoming a method (it has to read the inner entity, which needs
/// `&App`).
/// Which surface this field paints around itself. The text machinery is
/// identical either way — only the chrome differs, which is why this is one
/// enum on one widget rather than two near-duplicate entity types.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldChrome {
    /// OOBE account/PSK and the lockscreen password: a 36px rounded input
    /// surface with its own background, border and focus ring.
    Boxed,
    /// The Launcher's search row: no surface of its own (the panel already
    /// provides one) and larger text, matching `Launcher.dc.html`'s 17px
    /// medium query line.
    Bare,
}

impl FieldChrome {
    fn text_size(self) -> f32 {
        match self {
            FieldChrome::Boxed => theme::TEXT_SM,
            FieldChrome::Bare => 17.,
        }
    }
}

pub(crate) struct OobeTextField {
    inner: Entity<ImeTextInput>,
    masked: bool,
    chrome: FieldChrome,
    /// W7-3 (`IME-account-fields-zhuyin`, 2026-08-24): does this field ever
    /// legitimately hold non-ASCII content? `false` (the default via
    /// `new_ascii_only`'s siblings below) means "no" — on focus-gained this
    /// field proactively switches fcitx5 to `keyboard-us` (and back to
    /// `chewing` on focus-lost), via `super::ime_focus::on_focus_transition`
    /// in `Render::render` below. See `ime_focus.rs`'s own header comment
    /// for the full bug writeup and why this is safe (does NOT touch
    /// `accepts_text_input`/text-input-disable, which `TextInputStyle::
    /// masked`'s own doc comment already found unsafe on this appliance).
    /// Independent of `masked`: the account NAME field is ASCII-only but
    /// not masked, so a single flag cannot serve both purposes.
    ascii_only: bool,
    /// Edge-detect state for the focus-transition call above — `render` runs
    /// every frame and recomputes `focused` fresh each time (no separate
    /// focus-change subscription exists on this widget), so the transition
    /// itself has to be diffed against the previous pass's read.
    was_focused: bool,
}

impl OobeTextField {
    fn new(
        cx: &mut App,
        placeholder: impl Into<SharedString>,
        masked: bool,
        ascii_only: bool,
        chrome: FieldChrome,
    ) -> Entity<Self> {
        // Colors are pushed per render pass (see `Render` below) — the style
        // handed in here only needs to carry the shape decisions that never
        // change for this field: single line, no submit-on-Enter (`enter` is
        // a globally bound `OobeNext` action in this crate, so it never
        // reaches a raw key listener), and whether it masks.
        let inner = ImeTextInput::with_style(cx, placeholder, TextInputStyle::single_line().masked(masked));
        cx.new(|_cx| Self { inner, masked, chrome, ascii_only, was_focused: false })
    }

    /// Everything typed so far. Returns an owned `String` rather than a
    /// borrow because reaching it means reading a second entity out of `cx`,
    /// and because every caller (password submit, account claim, PSK
    /// connect) needs an owned value anyway.
    pub(crate) fn content(&self, cx: &App) -> String {
        self.inner.read(cx).content().to_string()
    }

    /// Resets typed content back to empty — `steps::network`'s "取消"
    /// (cancel) handler on the PSK prompt, so re-picking a secured network
    /// after backing out never shows a stale password from a previous
    /// attempt; also every path that closes the Launcher, so a reopen starts
    /// from an empty search box.
    pub(crate) fn clear(&mut self, cx: &mut Context<Self>) {
        self.inner.update(cx, |input, cx| input.clear(cx));
        cx.notify();
    }
}

impl Focusable for OobeTextField {
    /// The INNER entity's handle — that is the element carrying
    /// `.track_focus(...)` and the one `Window::handle_input` checks before
    /// installing the IME input handler. Returning this type's own handle
    /// instead would focus a div that neither types nor composes.
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.inner.read(cx).focus_handle(cx)
    }
}

impl Render for OobeTextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Ambient theme — see this module's header comment ("Global") for
        // why this entity reads it from `cx` instead of taking it as a
        // render parameter like every other OOBE widget helper. `main.rs`'s
        // `ShellView::render` sets it once per pass before any surface
        // renders, and `oobe::render::render` overwrites it with the OOBE
        // flow's own palette; `unwrap_or_default` is a defensive fail-open
        // (light), never a panic, on the theoretical chance nothing has set
        // it yet.
        let palette = cx.try_global::<ShellPalette>().copied().unwrap_or_default();
        let handle = self.inner.read(cx).focus_handle(cx);
        let focused = handle.is_focused(window);
        // W7-3: edge-detect a focus transition on this ASCII-only field and
        // proactively switch fcitx5's active engine — see `ime_focus.rs`'s
        // own header comment. A no-op for every non-`ascii_only` field
        // (Launcher search, chat) and for every steady (non-transitioning)
        // frame, which is the overwhelming majority of render passes.
        super::ime_focus::on_focus_transition(self.ascii_only, self.was_focused, focused);
        self.was_focused = focused;
        let is_empty = self.inner.read(cx).is_empty();

        // `Bare` (the Launcher row) uses the faint text-ladder rank for its
        // placeholder, exactly the color the pre-D3-b static query line
        // painted; `Boxed` uses `muted_foreground`, likewise unchanged.
        let placeholder_color = match self.chrome {
            FieldChrome::Boxed => theme::alpha(palette.muted_foreground, 1.0),
            FieldChrome::Bare => theme::alpha(palette.text_faint, 1.0),
        };
        let text_size = self.chrome.text_size();
        let style = TextInputStyle::single_line()
            .masked(self.masked)
            .with_colors(
                theme::alpha(palette.foreground, 1.0).into(),
                placeholder_color.into(),
                theme::alpha(palette.brand, 1.0).into(),
                theme::alpha(palette.brand, 0.20).into(),
            )
            .with_metrics(px(text_size), px(text_size * 1.4));
        // Re-pushed every pass so an operator flipping the OOBE theme step
        // restyles the caret/selection on the very next frame — `set_style`
        // no-ops (no `cx.notify()`) when nothing actually changed, so this
        // cannot spin the render loop.
        self.inner.update(cx, |input, cx| input.set_style(style, cx));

        let base = div()
            .id("oobe-text-field")
            // Click anywhere on the chrome (including padding the inner
            // element does not cover) focuses the field. The inner widget
            // focuses itself on a direct hit; this covers the rest.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _ev, window, cx| {
                    let handle = this.inner.read(cx).focus_handle(cx);
                    window.focus(&handle, cx);
                }),
            )
            .flex()
            .items_center()
            .cursor(CursorStyle::IBeam)
            .text_size(px(text_size))
            .text_color(if is_empty { placeholder_color } else { theme::alpha(palette.foreground, 1.0) });

        match self.chrome {
            FieldChrome::Boxed => base
                .w_full()
                .h(px(36.))
                .px(px(12.))
                .rounded(px(theme::RADIUS_LG))
                .bg(palette.input_bg())
                .border_1()
                .border_color(if focused { theme::alpha(palette.ring, 1.0).into() } else { palette.input_border() })
                .child(self.inner.clone()),
            // No focus ring here on purpose: the Launcher row is the only
            // focusable thing in an overlay that opens focused, so a ring
            // would be permanent decoration rather than information — the
            // live caret already says where typing goes. `flex_1` lets it
            // take the row's remaining width beside the magnifier icon.
            FieldChrome::Bare => {
                let _ = focused;
                base.flex_1().font_weight(FontWeight::MEDIUM).child(self.inner.clone())
            }
        }
    }
}

/// The `AccountCreate` step's two `OobeTextField` entities, bundled so
/// `main.rs` only needs one field on `ShellView` (not two loose `Entity<
/// OobeTextField>`s) and the render chain only needs one extra parameter
/// threaded through `oobe::render` → `steps::render` → `steps::account::
/// render` (every OTHER step's render fn ignores it — only `AccountCreate`
/// reads it). Created ONCE, at window-open time (`main()`'s `|_window, cx|`
/// closure, which has the `&mut App` `OobeTextField::new` needs — same call
/// site `duduclaw-native-gui/src/main.rs` creates ITS OWN `email_field`/
/// `password_field` at), unconditionally — whether or not this boot
/// actually reaches the `AccountCreate` step, creating two cheap entities
/// upfront is simpler than lazily creating them on first visit and had no
/// measurable cost worth the extra `Option<...>` bookkeeping.
pub(crate) struct AccountFields {
    pub(crate) name: Entity<OobeTextField>,
    pub(crate) password: Entity<OobeTextField>,
}

impl AccountFields {
    pub(crate) fn new(cx: &mut App) -> Self {
        // Placeholders reuse `fake_data`'s former PREFILLED constants as
        // HINT text instead — round 1's fake name/password are still
        // honest example values to show an empty field's shape, just no
        // longer typed in for the operator (task brief: replace the static
        // fake VALUES with real typing, not invent new placeholder copy).
        Self {
            // Both fields are Linux account credentials — ASCII-only by
            // definition, and exactly the two fields
            // `IME-account-fields-zhuyin` was filed against. `ascii_only:
            // true` on both.
            name: OobeTextField::new(cx, super::fake_data::FAKE_ACCOUNT_NAME, false, true, FieldChrome::Boxed),
            password: OobeTextField::new(cx, super::fake_data::FAKE_ACCOUNT_PASSWORD_MASK, true, true, FieldChrome::Boxed),
        }
    }
}

/// The `Network` step's PSK entry field (Shell-S3, 2026-08-21) — same
/// "bundle the one-per-step `Entity<OobeTextField>` so `main.rs` only needs
/// one field on `ShellView`" shape `AccountFields` already establishes
/// above, re-derived here rather than folded INTO `AccountFields` because
/// the two steps' fields have unrelated lifecycles (this one is only ever
/// read while `NetConnectState::AwaitingPsk` holds; `steps::network`'s
/// cancel handler clears its typed content back to empty via
/// `OobeTextField::clear`, something `AccountFields`'s two fields never
/// need since the `AccountCreate` step has no "cancel and pick a different
/// target" affordance).
pub(crate) struct NetworkFields {
    pub(crate) psk: Entity<OobeTextField>,
}

impl NetworkFields {
    pub(crate) fn new(cx: &mut App) -> Self {
        // Same masked-dots placeholder `AccountFields`'s own password field
        // uses — a generic "this field is masked" shape hint, not a
        // localized string (see that field's own construction above for
        // why `fake_data`'s constants stay unlocalized placeholders).
        // A WPA passphrase is ASCII-only (WPA2's own PSK charset) —
        // `ascii_only: true`, same reasoning as `AccountFields`.
        Self { psk: OobeTextField::new(cx, super::fake_data::FAKE_ACCOUNT_PASSWORD_MASK, true, true, FieldChrome::Boxed) }
    }
}

/// The lockscreen surface's password entry field (WP-lock-pw, 2026-08-22) —
/// same "bundle the one-per-surface `Entity<OobeTextField>` so `main.rs`
/// only needs one field on `ShellView`" shape `AccountFields`/`NetworkFields`
/// establish above. Defined HERE (not in `crate::lockscreen`) purely because
/// `OobeTextField::new` is private to this module and `crate::lockscreen` is
/// a crate-root SIBLING of `oobe`, not a descendant of it, so it cannot
/// reach a private `fn` inside `oobe::widgets` directly — re-exported as
/// `oobe::LockPasswordField` (`oobe/mod.rs`'s own `pub(crate) use` list,
/// same shape as `AccountFields`/`NetworkFields`) so `crate::lockscreen`/
/// `main.rs` never need to know the type physically lives under `oobe` at
/// all; this is pure code reuse of an already-proven text-input widget, not
/// a sign the lockscreen is somehow part of the OOBE flow.
///
/// Placeholder text is a literal zh-TW string, not routed through
/// `crate::i18n` at construction time — `cx: &mut App` at window-open (this
/// fn's own call site in `main.rs`) has no OOBE `Locale` selection yet to
/// read (same reason `AccountFields`/`NetworkFields` above hardcode their
/// own placeholders instead), and the lockscreen surface as a whole already
/// hardcodes `Locale::ZhTw` throughout its own rendering (see
/// `lockscreen/render.rs`'s header comment) — not a new limitation this
/// field introduces.
pub(crate) struct LockPasswordField {
    pub(crate) field: Entity<OobeTextField>,
}

impl LockPasswordField {
    pub(crate) fn new(cx: &mut App) -> Self {
        // The unlock password is the same Linux account credential
        // `AccountFields.password` sets — `ascii_only: true`.
        Self {
            field: OobeTextField::new(
                cx,
                crate::i18n::t(crate::i18n::Locale::ZhTw, crate::i18n::Key::LockPasswordPlaceholder),
                true,
                true,
                FieldChrome::Boxed,
            ),
        }
    }
}

/// The Launcher overlay's search field (D3-b, 2026-08-23) — same "bundle the
/// one-per-surface `Entity<OobeTextField>` so `main.rs` only needs one field
/// on `ShellView`" shape `AccountFields`/`NetworkFields`/`LockPasswordField`
/// establish above, and defined HERE for the identical reason
/// `LockPasswordField` is (`OobeTextField::new` is private to this module).
/// Not a sign the Launcher is part of the OOBE flow.
///
/// Before D3-b the Launcher's query was a plain `String` on
/// `OverlayUiState`, appended to by a raw `on_key_down` listener on the
/// shell root — which meant no `EntityInputHandler` was ever installed and a
/// zh-TW operator could not search their apps in Chinese at all (only by an
/// app's ASCII id or `Keywords=` entry). It is a real field now.
pub(crate) struct LauncherQueryField {
    pub(crate) field: Entity<OobeTextField>,
}

impl LauncherQueryField {
    pub(crate) fn new(cx: &mut App) -> Self {
        // Same `Locale::ZhTw`-at-construction-time limitation every other
        // field bundle here documents: `cx: &mut App` at window-open has no
        // operator locale selection to read yet, and this crate hardcodes
        // that locale everywhere outside OOBE anyway.
        Self {
            // The Launcher searches app names/keywords, which are routinely
            // Chinese (`native.*` i18n strings) — `ascii_only: false`, so
            // this field keeps starting in `chewing` (`ActiveByDefault`)
            // exactly as before W7-3. Deliberately verified NOT broken by
            // this round: see `ime_focus.rs`'s own header comment.
            field: OobeTextField::new(
                cx,
                crate::i18n::t(crate::i18n::Locale::ZhTw, crate::i18n::Key::LauncherSearchPlaceholder),
                false,
                false,
                FieldChrome::Bare,
            ),
        }
    }
}

/// The 系統設定 app's eight text fields (D4b, 2026-08-23) — same "bundle the
/// per-surface `Entity<OobeTextField>`s so `main.rs` only needs one field on
/// `ShellView`" shape `AccountFields`/`NetworkFields`/`LockPasswordField`/
/// `LauncherQueryField` establish above, and defined HERE for the identical
/// reason the last two are (`OobeTextField::new` is private to this module,
/// and `crate::settings` is a crate-root SIBLING of `oobe`, not a descendant
/// of it). Not a sign the settings app is part of the OOBE flow.
///
/// One bundle rather than three (`SettingsTimeFields` / `SettingsUserFields`
/// / `SettingsNetworkFields`) because the settings surface opens and closes
/// as ONE panel and every field's lifecycle is that panel's — unlike
/// `AccountFields` vs `NetworkFields`, which that type's own doc comment
/// splits precisely because their two OOBE steps have unrelated lifecycles.
///
/// Placeholders are literal zh-TW / example values, not `crate::i18n` keys —
/// `cx: &mut App` at window-open has no operator locale to read (the same
/// limitation every bundle above documents), and `crate::settings` hardcodes
/// zh-TW throughout for the reasons its own header comment gives.
pub(crate) struct SettingsFields {
    /// 日期與時間 — a free-typed IANA zone for anything outside the shortcuts.
    pub(crate) timezone: Entity<OobeTextField>,
    /// 使用者 — the three halves of a password rotation. All masked.
    pub(crate) current_password: Entity<OobeTextField>,
    pub(crate) new_password: Entity<OobeTextField>,
    pub(crate) confirm_password: Entity<OobeTextField>,
    /// 網路 — the Wi-Fi passphrase prompt. Masked.
    pub(crate) wifi_psk: Entity<OobeTextField>,
    /// 網路 — the static-IP form.
    pub(crate) ip_address: Entity<OobeTextField>,
    pub(crate) ip_gateway: Entity<OobeTextField>,
    pub(crate) ip_dns: Entity<OobeTextField>,
}

impl SettingsFields {
    pub(crate) fn new(cx: &mut App) -> Self {
        // Every field in this panel is ASCII-only by construction (IANA zone
        // names, account passwords, a WPA passphrase, dotted-quad/CIDR
        // addresses) — `ascii_only: true` throughout, same W7-3 reasoning as
        // `AccountFields`/`NetworkFields`/`LockPasswordField` above.
        Self {
            timezone: OobeTextField::new(cx, "Asia/Taipei", false, true, FieldChrome::Boxed),
            current_password: OobeTextField::new(cx, "••••••••", true, true, FieldChrome::Boxed),
            new_password: OobeTextField::new(cx, "••••••••", true, true, FieldChrome::Boxed),
            confirm_password: OobeTextField::new(cx, "••••••••", true, true, FieldChrome::Boxed),
            wifi_psk: OobeTextField::new(cx, "••••••••", true, true, FieldChrome::Boxed),
            ip_address: OobeTextField::new(cx, "192.168.1.50/24", false, true, FieldChrome::Boxed),
            ip_gateway: OobeTextField::new(cx, "192.168.1.1", false, true, FieldChrome::Boxed),
            ip_dns: OobeTextField::new(cx, "1.1.1.1, 8.8.8.8", false, true, FieldChrome::Boxed),
        }
    }

    /// Drops the three password fields' plaintext. Called on a SUCCESSFUL
    /// rotation and whenever the settings panel closes — the same "do not
    /// leave the plaintext sitting in the widget" discipline the lockscreen's
    /// own password field applies.
    pub(crate) fn clear_passwords(&self, cx: &mut App) {
        for field in [&self.current_password, &self.new_password, &self.confirm_password] {
            field.update(cx, |f, cx| f.clear(cx));
        }
    }

    /// Drops the Wi-Fi passphrase. Called on a successful join, on cancel,
    /// and on panel close — so re-picking a secured network never shows a
    /// password from a previous attempt (exactly what `steps::network`'s own
    /// cancel handler does).
    pub(crate) fn clear_wifi_psk(&self, cx: &mut App) {
        self.wifi_psk.update(cx, |f, cx| f.clear(cx));
    }

    /// Everything a panel close must not leave behind: both secrets, plus
    /// the static-IP form (a half-typed address surviving a close would be
    /// re-submitted by a later 套用 click that the operator did not mean for
    /// it).
    pub(crate) fn clear_all(&self, cx: &mut App) {
        self.clear_passwords(cx);
        self.clear_wifi_psk(cx);
        for field in [&self.timezone, &self.ip_address, &self.ip_gateway, &self.ip_dns] {
            field.update(cx, |f, cx| f.clear(cx));
        }
    }
}
