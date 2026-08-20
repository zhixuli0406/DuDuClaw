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
// Every helper below that paints anything now takes an `OobePalette` (see
// `palette.rs`) instead of reaching for `theme::light::*` directly — passed
// BY VALUE (the type is `Copy`, ~70 bytes of plain fields) rather than by
// reference, which sidesteps borrow-checker friction against the `move`
// closures several of these helpers build (`step_button`'s hover closure,
// `toggle_pill`'s caller-supplied click handler, …). Every call site resolves
// its palette the same way `locale` already is (`flow.palette()`, fresh per
// render call — see that method's own doc comment in `oobe/mod.rs`), so
// there is nothing to keep in sync here: swap the operator's pick and the
// very next render call threads a different `OobePalette` through this
// entire file. The one exception is `OobeTextField` below, which reads the
// ambient `OobePalette` global instead — see its own doc comment for why.

use gpui::{
    div, prelude::*, px, App, ClickEvent, Context, CursorStyle, Div, Entity, FocusHandle, Focusable, FontWeight, KeyDownEvent, MouseButton,
    Render, SharedString, Stateful, Window,
};

use duduclaw_native_gui::theme;

use super::palette::OobePalette;

pub(super) fn title(text: &'static str, palette: OobePalette) -> Div {
    div().text_size(px(theme::TEXT_2XL)).font_weight(FontWeight::BOLD).text_color(theme::alpha(palette.foreground, 1.0)).child(text)
}

pub(super) fn subtitle(text: &'static str, palette: OobePalette) -> Div {
    div().text_size(px(theme::TEXT_BASE)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text)
}

/// Same as `subtitle`, for a runtime-computed string (e.g. "已連線：
/// <ssid>") — `.child()` accepts an owned `String` directly (gpui's own
/// `impl IntoElement for String`), so this is just the `&'static str`
/// version's twin with a different parameter type.
pub(super) fn subtitle_dynamic(text: String, palette: OobePalette) -> Div {
    div().text_size(px(theme::TEXT_BASE)).text_color(theme::alpha(palette.muted_foreground, 1.0)).child(text)
}

/// The one "card" every step's content area sits inside — same
/// `surface_shadow()` + `RADIUS_XL` + `border()` recipe `overlay/
/// notifications.rs`'s own floating panel uses, just full-width within the
/// step's centered column instead of docked to a screen edge.
pub(super) fn card(content: impl IntoElement, palette: OobePalette) -> Div {
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
pub(super) enum StepButtonVariant {
    Primary,
    Secondary,
    Ghost,
}

/// Hand-rolled button — see this file's header comment for why it isn't
/// `mds_gpui::button()`. `disabled` drops the click handler entirely (not
/// just a visual dim), matching that facade's own documented disabled
/// contract (`duduclaw-native-gui/src/mds_gpui/button.rs`: "no hover/active
/// styles and NO click handler attached at all").
pub(super) fn step_button(
    id: &'static str,
    label: &'static str,
    variant: StepButtonVariant,
    disabled: bool,
    palette: OobePalette,
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

/// Low-key step-progress dots (task brief: "進度指示（低調 dots）"). Kept
/// deliberately, per `research/native-os-2026-08/oobe-first-run-
/// reference.md` §B-4's own list of reusable web assets ("StepDots 視覺語
/// 彙（同 KDE dots...）") — the survey's majority finding that macOS/
/// Windows/ChromeOS show NO progress indicator is explicitly attributed to
/// their step COUNT being dynamic/conditional ("步驟數動態做不了誠實步驟
/// 條", §A "主要分歧" progress-indicator row) which does not apply here:
/// DuDuClaw OS's OOBE is a fixed ten-step linear sequence, so a dot per
/// step is an honest indicator, not a promise the flow can't keep.
pub(super) fn progress_dots(current_index: usize, total: usize, palette: OobePalette) -> Div {
    let mut row = div().flex().items_center().gap(px(6.));
    for i in 0..total {
        let active = i == current_index;
        row = row.child(
            div()
                .w(px(if active { 16. } else { 6. }))
                .h(px(6.))
                .rounded(px(3.))
                .bg(if active { theme::alpha(palette.brand, 1.0) } else { palette.surface_border }),
        );
    }
    row
}

/// Pill-shaped toggle switch — the `Privacy` step's four opt-in rows (task
/// brief step 6: "3-4 個 opt-in 開關"). Palette-driven re-derivation of
/// `overlay/controlcenter.rs`'s own `toggle_pill` (same shape: a rounded
/// track + a circular handle that slides left/right), not a shared function
/// — that one is hardwired to `overlay::controlcenter`'s own literal hex
/// constants (`0xe4e4e7`/`0xf0f0f2`, never routed through `theme::light::*`
/// or `theme::dark::*`) rather than this crate's `OobePalette` token set,
/// and pulling it in would mean either reaching across module boundaries
/// into a sibling `overlay::` submodule that doesn't expose it (`fn
/// toggle_pill` there is private to `controlcenter.rs`) or making it public
/// for a single OOBE call site — a one-screen-only re-derivation is the
/// smaller change. Purely presentational — no click handling of its own
/// (the caller's own `.on_click(...)` sits on the row, not this pill, same
/// division `controlcenter.rs`'s `switch_row`/`toggle_pill` split
/// establishes). The knob itself stays hardcoded white regardless of theme —
/// same convention iOS/macOS toggle switches use (a white knob reads
/// correctly against both a colored "on" track and a muted "off" track in
/// either theme), so this is not a residual un-palette-driven color, it's
/// the one part of this widget the design intentionally never re-skins.
pub(super) fn toggle_pill(on: bool, palette: OobePalette) -> Div {
    let track = if on { theme::alpha(palette.brand, 1.0) } else { palette.surface_border };
    let mut handle = div().absolute().top(px(2.)).w(px(19.)).h(px(19.)).rounded(px(19.)).bg(theme::alpha(0xffffff, 1.0));
    handle = if on { handle.right(px(2.)) } else { handle.left(px(2.)) };
    div().relative().w(px(40.)).h(px(23.)).rounded(px(23.)).bg(track).child(handle)
}

/// Minimal single-line editable text field entity — the `AccountCreate`
/// step's real replacement for round 1's static `fake_field` values (task
/// brief §B). Deliberately NOT the full zed `crates/gpui/examples/input.rs`
/// pattern (~780 lines: `EntityInputHandler` for OS-level IME composition,
/// drag-to-select, a hand-rolled `Element` for cursor/selection painting —
/// real scope for a later round, not this one). This is instead a re-
/// derivation of `duduclaw-native-gui/src/text_field.rs`'s own "deliberately
/// smaller alternative" (plain `on_key_down` capture: printable `key_char`
/// appends, `backspace` pops, no selection, no IME composition) — proven
/// already shipping in that crate's own login screen at ~130 lines, well
/// under the "~300 lines of EntityInputHandler" threshold the task brief
/// names as the bar for falling back to a static stub. Re-derived HERE
/// rather than reused from that crate because `duduclaw-native-gui/src/
/// lib.rs` only exposes `theme` and `mds_gpui` to this crate (task brief:
/// "不要改 native-gui") — `TextField`/`text_field` itself stays private to
/// that crate's own binary.
///
/// IME/CJK composition is out of scope by design (task brief: "IME/中文組字
/// 不需要") — account name/password are ASCII-shaped in practice, same
/// judgment call `text_field.rs`'s own header comment makes for its login
/// fields. Tab-between-fields is also out of scope (task brief: "可省
/// 略") — click-to-focus (`on_mouse_down` below) is the only way to move
/// focus between the two fields this round.
pub(crate) struct OobeTextField {
    pub(crate) content: String,
    placeholder: SharedString,
    masked: bool,
    focus_handle: FocusHandle,
}

impl OobeTextField {
    fn new(cx: &mut App, placeholder: impl Into<SharedString>, masked: bool) -> Entity<Self> {
        cx.new(|cx| Self { content: String::new(), placeholder: placeholder.into(), masked, focus_handle: cx.focus_handle() })
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        // Let anything chorded with cmd/ctrl/function fall through — same
        // guard `text_field.rs`'s own `on_key_down` uses, so cmd-k/Escape/
        // Enter (this crate's own global OOBE keybindings, bound on the
        // `ShellView` root — see `main.rs`'s header comment) keep reaching
        // their action handlers instead of being swallowed as "typed text"
        // here. Plain `enter`/`escape` (no modifier) DO reach this match,
        // but neither has a `key_char`, so the `_ =>` arm below is a no-op
        // for them — nothing here calls `cx.stop_propagation()`, so gpui's
        // action-dispatch pass over the SAME keystroke still fires
        // regardless (this field's `.track_focus`ed div is a DESCENDANT of
        // `ShellView`'s own focused root, and gpui walks actions along the
        // whole ancestor path, not just the exact focused leaf).
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        match ks.key.as_str() {
            "backspace" => {
                self.content.pop();
                cx.notify();
            }
            _ => {
                if let Some(ch) = ks.key_char.as_deref() {
                    if !ch.is_empty() && ch.chars().all(|c| !c.is_control()) {
                        self.content.push_str(ch);
                        cx.notify();
                    }
                }
            }
        }
    }
}

impl Focusable for OobeTextField {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for OobeTextField {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus_handle.is_focused(window);
        let is_empty = self.content.is_empty();
        // Ambient theme — see this module's header comment ("Global") for
        // why this entity reads it from `cx` instead of taking it as a
        // render parameter like every other OOBE widget helper. `oobe::
        // render::render` always sets this before this entity's own render
        // pass runs (it's the OOBE frame's top-level fn); `unwrap_or_default`
        // is a defensive fail-open (light), never a panic, on the
        // theoretical chance nothing has set it yet.
        let palette = cx.try_global::<OobePalette>().copied().unwrap_or_default();

        let display: SharedString = if is_empty {
            self.placeholder.clone()
        } else if self.masked {
            "•".repeat(self.content.chars().count()).into()
        } else {
            self.content.clone().into()
        };

        div()
            .id("oobe-text-field")
            .track_focus(&self.focus_handle)
            .key_context("OobeTextField")
            .on_key_down(cx.listener(Self::on_key_down))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _ev, window, cx| window.focus(&this.focus_handle, cx)))
            .w_full()
            .h(px(36.))
            .px(px(12.))
            .flex()
            .items_center()
            .rounded(px(theme::RADIUS_LG))
            .bg(palette.input_bg())
            .border_1()
            .border_color(if focused { theme::alpha(palette.ring, 1.0).into() } else { palette.input_border() })
            .cursor(CursorStyle::IBeam)
            .text_size(px(theme::TEXT_SM))
            .text_color(if is_empty { theme::alpha(palette.muted_foreground, 1.0) } else { theme::alpha(palette.foreground, 1.0) })
            .child(display)
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
            name: OobeTextField::new(cx, super::fake_data::FAKE_ACCOUNT_NAME, false),
            password: OobeTextField::new(cx, super::fake_data::FAKE_ACCOUNT_PASSWORD_MASK, true),
        }
    }
}
