// Pointer settings — ICON-3 (2026-08-23).
//
// Visual spec: `commercial/design/duduclaw-lockscreen-oobe-icons/
// PointerSettings.dc.html` — 「協助工具 › 指向與點按」, a 造型 card over a
// 大小 card. Research: `research/native-os-2026-08/
// lockscreen-oobe-icons-2026-08.md` §C/§F, whose three load-bearing findings
// are all implemented literally here:
//
//   1. **Discrete size buttons, never a slider** (§C.4). macOS/Windows use a
//      slider because their cursors are vectors scaled live; DuDuClaw, like
//      GNOME and KDE, ships one raster per size, so a slider would land on
//      sizes that have no artwork. GNOME's own `cc-cursor-size-page.c`
//      `cursor_data[]` is copied verbatim: 24 預設 / 32 中 / 48 大 / 64 特大
//      / 96 最大 — five, including the 96 our earlier plan was missing.
//   2. **Each button draws the real cursor at that size** (§C.2), which is
//      exactly what GNOME does (five pre-rendered `left_ptr_*px.png`).
//   3. **Zero feedback on change** (§C.1/§F). Not a toast, not a "takes
//      effect next login" line, nothing. All four surveyed OSes write the
//      setting, let the pointer change under the operator's hand, and say
//      nothing at all. The feedback IS the pointer.
//
// ── Where this lives, and why it is not a settings app ──────────────────
// The board frames this as one page of a full 系統設定 window with a
// twelve-row sidebar. This shell has no settings application and inventing
// one is explicitly out of scope, so the page lands as its OWN overlay —
// the same class of surface as Launcher / Notifications / ControlCenter —
// reached from a 「協助工具 · 指向與點按」 row inside ControlCenter, which
// IS this shell's settings surface (the dock's gear opens it; see
// `home/home_dock.rs::dock_settings`). One section, one panel, one entry
// point.
//
// Two parts of the board are deliberately NOT drawn, because drawing them
// would be inventing affordances:
//
//   * the five category chips (視覺/聽覺/打字/指向與點按/縮放) — four of the
//     five lead to pages that do not exist, and a chip that goes nowhere is
//     worse than no chip. The screen's own subtitle already carries the
//     「協助工具 ·」 breadcrumb the chips were there to provide.
//   * the 「其他」 card (滑鼠鍵 / 停留即點按) — two toggles with no backend
//     anywhere in this OS. Same call `steps::language`'s accessibility
//     preview makes for its own five rows.
//
// ── Degradation, which is most of this module ──────────────────────────
// Everything here depends on `duduclaw-comp` over the shell-control socket,
// and there are three honest states short of "it works", each rendered
// differently and none of them an error dialog:
//
//   * **No socket at all** (`CompClientError::NotAvailable`) — the ordinary
//     case on a dev Mac, and the honest one on a Linux box whose compositor
//     is down. The whole 指標 card is replaced by one sentence. Nothing is
//     rendered as selected, because nothing is known.
//   * **Comp is there but has no `size`** — an older build. The style cards
//     work; the five size buttons render disabled with one line saying so.
//   * **A set call was refused** — one line, and the previous state stays on
//     screen. This never optimistically re-paints the selection: the
//     compositor's answer is the only thing that moves it.

use gpui::{div, prelude::*, px, AnyElement, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::comp_client::{self, CompClientError, CursorState};
use crate::i18n::{t, Key, Locale};
use crate::icons::{self, CursorShape};
use crate::palette::ShellPalette;
use crate::ShellView;

/// The five sizes, in the board's own order, each with its label key.
/// GNOME's `cursor_data[]` verbatim — see this module's header comment.
const POINTER_SIZES: [(u32, Key); 5] = [
    (24, Key::PointerSizeDefault),
    (32, Key::PointerSizeMedium),
    (48, Key::PointerSizeLarge),
    (64, Key::PointerSizeExtraLarge),
    (96, Key::PointerSizeLargest),
];

/// The panel's fixed width and the left offset that centres it in this
/// crate's fixed 1440px dev-mode window — the same derivation
/// `lockscreen::render::cat_watermark` already does, and the same "no resize
/// handling this round" scope boundary it states.
///
/// 720, not the board's 760 content column: the widest thing inside is the
/// five size buttons (5×118 + 4×10 gaps = 630), and 720 clears that with the
/// panel's own 20px padding and the card's 18px still intact.
const PANEL_WIDTH: f32 = 720.;
const PANEL_LEFT: f32 = (1440. - PANEL_WIDTH) / 2.;

/// Board geometry for one size button (`.dc-size` / `.dc-sizebox`).
const SIZE_BUTTON_WIDTH: f32 = 118.;
const SIZE_BUTTON_PREVIEW_HEIGHT: f32 = 100.;

// ── State ────────────────────────────────────────────────────────────────

/// What this surface knows about the compositor's cursor configuration.
///
/// Deliberately NOT `Option<CursorState>`: "haven't asked yet", "asking",
/// "asked and there's no compositor" and "here it is" are four different
/// facts, and collapsing the middle two into `None` is exactly how a surface
/// ends up rendering an empty state that means "loading" and a loading state
/// that means "broken".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum PointerLoad {
    #[default]
    NotLoaded,
    Loading,
    Loaded(CursorState),
    /// The compositor could not be reached. Terminal for this panel's
    /// lifetime — reopening the panel retries, because `close` resets this
    /// back to `NotLoaded`.
    Unavailable,
}

/// Which kind of change is in flight — only used to decide which failure
/// message to show, since a refused SIZE change is the one failure that also
/// tells us something durable about the compositor (it is too old).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointerApply {
    Source,
    Size,
}

/// Ephemeral state for the pointer-settings surface — lives on `ShellView`
/// as `pointer_ui`, the same shape `audio_ui` already has (see that field's
/// own doc comment in `main.rs` for why a plain struct, not a gpui entity).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct PointerUiState {
    pub(crate) load: PointerLoad,
    /// A compositor call is in flight. The authoritative guard — every
    /// kick-off checks this FIRST, before touching anything else, the same
    /// contract `overlay::controlcenter::kick_off_audio_call` documents for
    /// its own `in_flight`.
    in_flight: bool,
    /// Set once a `set_cursor_size` call comes back refused. Sticky for the
    /// panel's lifetime: an older compositor does not become newer between
    /// two clicks, and re-asking on every click would just produce a stream
    /// of identical failures.
    size_refused: bool,
    /// The most recent failed change, if any. Cleared by the next successful
    /// one.
    last_failure: Option<PointerApply>,
}

impl PointerUiState {
    /// Whether the size control can be used at all: the compositor has to
    /// have reported a size AND must not have refused a change already.
    /// Either miss disables the five buttons and shows one honest line.
    ///
    /// Reads `drawn_size()` rather than `size` directly, so a compositor
    /// that reports only one of the two fields still counts as answering.
    pub(crate) fn size_supported(&self) -> bool {
        !self.size_refused && matches!(&self.load, PointerLoad::Loaded(cursor) if cursor.drawn_size().is_some())
    }

    fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        true
    }

    fn settle_load(&mut self, result: Result<CursorState, CompClientError>) {
        self.in_flight = false;
        self.load = match result {
            Ok(cursor) => PointerLoad::Loaded(cursor),
            Err(e) => {
                eprintln!("[pointer] get_cursor_source failed: {e}");
                PointerLoad::Unavailable
            }
        };
    }

    fn settle_apply(&mut self, kind: PointerApply, result: Result<CursorState, CompClientError>) {
        self.in_flight = false;
        match result {
            Ok(cursor) => {
                self.load = PointerLoad::Loaded(cursor);
                self.last_failure = None;
            }
            Err(e) => {
                eprintln!("[pointer] applying {kind:?} failed: {e}");
                // A REFUSAL (comp answered, and said no) to a size change
                // is the signal that this build predates the op — EXCEPT
                // for `invalid_cursor_size`, which means "that VALUE is not
                // offered", not "this build cannot change the size". Marking
                // the control unsupported on that code would disable all
                // five buttons because one of them disagreed with comp's
                // step list, which is the wrong repair by a wide margin.
                // Transport errors say nothing about what comp can do and
                // are excluded too.
                let op_missing = matches!(&e, CompClientError::Comp(code) if code != comp_client::CURSOR_ERR_INVALID_SIZE);
                if kind == PointerApply::Size && op_missing {
                    self.size_refused = true;
                }
                self.last_failure = Some(kind);
            }
        }
    }

    /// Called whenever the panel closes, so the next open re-reads the
    /// compositor rather than showing a stale snapshot (the cursor can have
    /// been changed by anything else in between). Deliberately keeps
    /// nothing: even `size_refused` is dropped, because a compositor restart
    /// is exactly the kind of thing that happens between two panel opens.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

// ── Kick-offs ────────────────────────────────────────────────────────────
// Same background-thread -> `std::sync::mpsc` -> `cx.spawn` poll-loop bridge
// `overlay::controlcenter::kick_off_audio_call` and `oobe::steps::network`
// already established: `comp_client` is blocking and gpui's main thread must
// never wait on a socket.

/// Reads the compositor's current state if it has not been read yet. Safe to
/// call from a render body — it claims `NotLoaded` before spawning anything,
/// so a repaint mid-flight cannot stack a second read (the same single-arm
/// discipline `LockScreenState::try_arm_clock_timer` uses for its timer).
pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.pointer_ui.load != PointerLoad::NotLoaded || !view.pointer_ui.begin() {
        return;
    }
    view.pointer_ui.load = PointerLoad::Loading;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(comp_client::get_cursor_source());
    });
    poll_into(cx, rx, move |view, result, cx| {
        view.pointer_ui.settle_load(result);
        cx.notify();
    });
}

/// Switches the cursor style. Re-reads the state afterwards when the
/// compositor's ack carried no cursor object — see `comp_client`'s own
/// section comment on why a setter's response shape cannot be assumed.
fn apply_source(view: &mut ShellView, brand: bool, cx: &mut Context<ShellView>) {
    if !view.pointer_ui.begin() {
        return;
    }
    cx.notify();
    let source = if brand { comp_client::CURSOR_SOURCE_BRAND } else { comp_client::CURSOR_SOURCE_SYSTEM };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(comp_client::set_cursor_source(source).and_then(confirm));
    });
    poll_into(cx, rx, move |view, result, cx| {
        view.pointer_ui.settle_apply(PointerApply::Source, result);
        cx.notify();
    });
}

fn apply_size(view: &mut ShellView, size: u32, cx: &mut Context<ShellView>) {
    if !view.pointer_ui.begin() {
        return;
    }
    cx.notify();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(comp_client::set_cursor_size(size).and_then(confirm));
    });
    poll_into(cx, rx, move |view, result, cx| {
        view.pointer_ui.settle_apply(PointerApply::Size, result);
        cx.notify();
    });
}

/// Turns a setter's `Option<CursorState>` into a definite one: use what the
/// compositor echoed, or go ask. Runs on the background thread, so the extra
/// round trip never touches the render thread. This is what keeps the panel
/// from ever displaying a state it merely assumed.
fn confirm(echoed: Option<CursorState>) -> Result<CursorState, CompClientError> {
    match echoed {
        Some(cursor) => Ok(cursor),
        None => comp_client::get_cursor_source(),
    }
}

/// The shared `try_recv` + paced-timer poll loop. Extracted because all
/// three call sites above are byte-identical apart from what they do with
/// the settled value.
fn poll_into<T: Send + 'static>(
    cx: &mut Context<ShellView>,
    rx: std::sync::mpsc::Receiver<T>,
    apply: impl Fn(&mut ShellView, T, &mut Context<ShellView>) + 'static,
) {
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(value) => {
                let _ = weak.update(cx, |view, cx| apply(view, value, cx));
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(30)).await;
    })
    .detach();
}

// ── Render ───────────────────────────────────────────────────────────────

pub(super) fn render(state: &PointerUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    // Reading the compositor is I/O, and this crate's rule is that I/O is
    // click-triggered rather than a render-time side effect (see
    // `oobe::steps::network`'s own header comment). This one call is the
    // documented exception, for the same reason `overlay::notifications`
    // makes it: the panel opening IS the click, and there is no other moment
    // to hang it on without the operator having to press something a second
    // time to see the state they just asked for. It is idempotent — see
    // `ensure_loaded`.
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.12).into() } else { palette.border() };
    let mut panel = div()
        .id("overlay-pointer-panel")
        .absolute()
        .top(px(110.))
        .left(px(PANEL_LEFT))
        .w(px(PANEL_WIDTH))
        .flex()
        .flex_col()
        .gap(px(14.))
        .rounded(px(theme::RADIUS_XL))
        .bg(theme::alpha(palette.surface_raised, 0.98))
        .border_1()
        .border_color(border_color)
        .shadow(palette.floating_shadow())
        .p(px(20.));
    if palette.is_dark() {
        // Same dark-only panel-root text colour every sibling overlay sets —
        // see `overlay/controlcenter.rs`'s own header comment.
        panel = panel.text_color(theme::alpha(palette.foreground, 1.0));
    }

    panel.child(header(palette)).child(pointer_card(state, palette, cx))
}

fn header(palette: ShellPalette) -> Div {
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .child(
            div()
                .w(px(34.))
                .h(px(34.))
                .rounded(px(10.))
                .bg(theme::alpha(palette.secondary, 1.0))
                .flex()
                .items_center()
                .justify_center()
                .child(icons::icon_or_none(&[(icons::ACCESSIBILITY, palette.brand)], 18.).unwrap_or_else(|| div().into_any_element())),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .child(div().text_size(px(17.)).font_weight(FontWeight::BOLD).child(t(Locale::ZhTw, Key::PointerTitle)))
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(theme::alpha(palette.muted_foreground, 1.0))
                        .child(t(Locale::ZhTw, Key::PointerSubtitle)),
                ),
        )
}

/// The one real card: 造型 over 大小, or a single honest sentence when there
/// is no compositor to ask.
fn pointer_card(state: &PointerUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let card = div()
        .bg(theme::alpha(palette.surface, 1.0))
        .border_1()
        .border_color(palette.border())
        .rounded(px(12.))
        .px(px(18.))
        .py(px(16.))
        .flex()
        .flex_col()
        .gap(px(16.));

    match &state.load {
        PointerLoad::NotLoaded | PointerLoad::Loading => card.child(notice_line(t(Locale::ZhTw, Key::PointerLoading), palette.muted_foreground)),
        PointerLoad::Unavailable => card.child(notice_line(t(Locale::ZhTw, Key::PointerUnavailable), palette.muted_foreground)),
        PointerLoad::Loaded(cursor) => {
            let mut body = card.child(shape_section(cursor, state, palette, cx)).child(size_section(cursor, state, palette, cx));
            // Two standing conditions comp reports and this screen must not
            // swallow, because each one means a choice made here does not
            // do what it appears to.
            if cursor.theme_missing() {
                body = body.child(notice_line(t(Locale::ZhTw, Key::PointerThemeMissing), palette.warning));
            }
            if cursor.any_env_pinned() {
                body = body.child(notice_line(t(Locale::ZhTw, Key::PointerEnvPinned), palette.warning));
            }
            if let Some(kind) = state.last_failure {
                // The size-unsupported line is NOT a failure message — it is
                // already rendered inside `size_section` as a standing
                // condition, so a refused size change would otherwise say
                // the same thing twice.
                let already_explained = kind == PointerApply::Size && !state.size_supported();
                if !already_explained {
                    body = body.child(notice_line(t(Locale::ZhTw, Key::PointerApplyFailed), palette.destructive));
                }
            }
            body
        }
    }
}

/// One honest one-liner: loading, unavailable, size-unsupported, apply
/// failed, and GNOME's zoom help text all render through this. Colour is the
/// caller's call because these are four different SEVERITIES, not four
/// variants of one message.
fn notice_line(text: &'static str, color: u32) -> Div {
    div().text_size(px(theme::TEXT_XS)).text_color(theme::alpha(color, 1.0)).child(text)
}

fn section_label(text: &'static str, palette: ShellPalette) -> Div {
    div().text_size(px(12.5)).font_weight(FontWeight::SEMIBOLD).text_color(theme::alpha(palette.foreground, 1.0)).child(text)
}

// ── 造型 ─────────────────────────────────────────────────────────────────

fn shape_section(cursor: &CursorState, state: &PointerUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // The radio follows what the operator CHOSE (`requested`), not what comp
    // is currently drawing (`source`) — they differ exactly when the chosen
    // theme is not installed, and in that case the honest screen is "your
    // choice is still your choice, and here is why it isn't showing", not a
    // radio that silently snaps back. `theme_missing_note` says the rest.
    let brand = cursor.requested_is_brand();
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(section_label(t(Locale::ZhTw, Key::PointerShapeLabel), palette))
        .child(
            div()
                .flex()
                .gap(px(14.))
                .child(shape_card(false, !brand, state, palette, cx))
                .child(shape_card(true, brand, state, palette, cx)),
        )
}

/// One style card: a preview tray with the three shapes, then a title/
/// subtitle row and the selection radio.
///
/// The paw card shows the paw for `default` and the SYSTEM shapes for `text`
/// and `pointer`, because that is what the theme actually installs —
/// 「爪印不覆蓋 pointer」是既定拍板 (a paw loses the hand's directional
/// meaning). Drawing three paws here would misrepresent what switching does.
fn shape_card(brand: bool, selected: bool, state: &PointerUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let busy = state.in_flight;
    let listener = cx.listener(move |view, _ev, _window, cx| apply_source(view, brand, cx));

    let tray = div()
        .h(px(84.))
        .flex()
        .items_center()
        .justify_center()
        .gap(px(34.))
        .bg(theme::alpha(if selected { palette.surface_selected } else { palette.app_shell }, 1.0))
        .border_b_1()
        .border_color(palette.border())
        .child(cursor_preview(CursorShape::Default, brand, 32.))
        .child(cursor_preview(CursorShape::Text, brand, 32.))
        .child(cursor_preview(CursorShape::Pointer, brand, 32.));

    let (title, subtitle) = if brand {
        (t(Locale::ZhTw, Key::PointerSourceBrand), t(Locale::ZhTw, Key::PointerSourceBrandDesc))
    } else {
        (t(Locale::ZhTw, Key::PointerSourceSystem), t(Locale::ZhTw, Key::PointerSourceSystemDesc))
    };

    let mut card = div()
        .id(if brand { "pointer-shape-brand" } else { "pointer-shape-system" })
        .flex_1()
        .overflow_hidden()
        .rounded(px(12.))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0).into() } else { palette.border() })
        .bg(theme::alpha(palette.surface_raised, 1.0))
        .flex()
        .flex_col()
        .child(tray)
        .child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(10.))
                .px(px(14.))
                .py(px(10.))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).text_color(theme::alpha(palette.foreground, 1.0)).child(title))
                        .child(div().text_size(px(11.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(subtitle)),
                )
                .child(selection_radio(selected, palette)),
        );

    if !busy && !selected {
        card = card.cursor_pointer().on_click(listener);
    }
    card
}

fn selection_radio(selected: bool, palette: ShellPalette) -> Div {
    let base = div().w(px(16.)).h(px(16.)).rounded(px(16.)).flex_none().flex().items_center().justify_center();
    if selected {
        base.bg(theme::alpha(palette.brand, 1.0))
            .child(icons::icon_or_none(&[(icons::CHECK, palette.brand_foreground)], 10.).unwrap_or_else(|| div().into_any_element()))
    } else {
        base.border_1().border_color(theme::alpha(palette.icon_inactive(), 1.0))
    }
}

/// One cursor drawn at `size` px, as its stacked outline+fill layers. Never
/// recoloured by the theme — see `icons::cursor_shape_layers`' own doc
/// comment on why real cursor artwork is identity-coloured.
fn cursor_preview(shape: CursorShape, brand: bool, size: f32) -> AnyElement {
    icons::icon_or_none(&icons::cursor_shape_layers(shape, brand), size).unwrap_or_else(|| div().into_any_element())
}

// ── 大小 ─────────────────────────────────────────────────────────────────

fn size_section(cursor: &CursorState, state: &PointerUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let supported = state.size_supported();
    // What is DRAWN, not what is stored — see `CursorState::drawn_size`. The
    // two differ when the installed theme has no image at the requested
    // step, and showing the stored value there would claim a size the
    // operator is not actually looking at.
    let current = cursor.drawn_size();
    // The size previews draw whichever style is actually ON SCREEN
    // (`is_brand`, not `requested_is_brand`) — a preview's job is to show
    // the operator their real pointer.
    let brand = cursor.is_brand();

    let mut buttons = div().flex().gap(px(10.));
    for (size, label_key) in POINTER_SIZES {
        buttons = buttons.child(size_button(size, label_key, current == Some(size), supported, brand, state, palette, cx));
    }

    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .pt(px(16.))
        .border_t_1()
        .border_color(palette.border())
        .child(section_label(t(Locale::ZhTw, Key::PointerSizeLabel), palette))
        .child(buttons);

    // §C.1/§F: on SUCCESS this surface says nothing at all. The only line
    // under the buttons is GNOME's own help text — and, when the size
    // control cannot be used, the one sentence explaining why.
    section = section.child(notice_line(t(Locale::ZhTw, Key::PointerZoomHint), palette.muted_foreground));
    if !supported {
        section = section.child(notice_line(t(Locale::ZhTw, Key::PointerSizeUnsupported), palette.warning));
    }
    section
}

#[allow(clippy::too_many_arguments)]
fn size_button(
    size: u32,
    label_key: Key,
    selected: bool,
    supported: bool,
    brand: bool,
    state: &PointerUiState,
    palette: ShellPalette,
    cx: &mut Context<ShellView>,
) -> Stateful<Div> {
    let interactive = supported && !state.in_flight && !selected;
    let listener = cx.listener(move |view, _ev, _window, cx| apply_size(view, size, cx));

    let mut button = div()
        .id(("pointer-size", size as usize))
        .w(px(SIZE_BUTTON_WIDTH))
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap(px(8.))
        .pt(px(8.))
        .pb(px(10.))
        .rounded(px(12.))
        .border_1()
        .border_color(if selected { theme::alpha(palette.brand, 1.0).into() } else { palette.border() })
        .bg(theme::alpha(if selected { palette.surface_selected } else { palette.surface_raised }, 1.0))
        // The preview is the WHOLE point of discrete buttons (§C.2): each
        // one draws the real cursor at its own size, so the operator picks
        // by looking rather than by reading a number.
        .child(
            div()
                .h(px(SIZE_BUTTON_PREVIEW_HEIGHT))
                .flex()
                .items_center()
                .justify_center()
                .opacity(if supported { 1.0 } else { 0.4 })
                .child(cursor_preview(CursorShape::Default, brand, size as f32)),
        )
        .child(
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(1.))
                .child(
                    div()
                        .text_size(px(13.))
                        .font_weight(if selected { FontWeight::BOLD } else { FontWeight::SEMIBOLD })
                        .text_color(theme::alpha(if selected { palette.brand } else { palette.foreground }, 1.0))
                        .child(size.to_string()),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(theme::alpha(if selected { palette.brand } else { palette.text_faint }, 1.0))
                        .child(t(Locale::ZhTw, label_key)),
                ),
        );

    if interactive {
        button = button.cursor_pointer().hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(listener);
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A compositor answer with the fields this surface reads and nothing
    /// else — `requested` mirrors `source` (the ordinary case; the
    /// they-disagree case gets its own test below).
    fn cursor(source: &str, size: Option<u32>) -> CursorState {
        CursorState {
            source: source.to_string(),
            requested: Some(source.to_string()),
            theme: None,
            size,
            effective_size: size,
            env_pinned: false,
            size_env_pinned: false,
        }
    }

    fn loaded(source: &str, size: Option<u32>) -> PointerUiState {
        PointerUiState { load: PointerLoad::Loaded(cursor(source, size)), ..PointerUiState::default() }
    }

    #[test]
    fn a_fresh_state_has_not_asked_the_compositor_anything() {
        let state = PointerUiState::default();
        assert_eq!(state.load, PointerLoad::NotLoaded);
        assert!(!state.size_supported(), "nothing is known yet, so nothing may be presented as adjustable");
    }

    /// The dev-Mac path. `Unavailable` must NOT report the size control as
    /// usable — there is no compositor to apply it.
    #[test]
    fn an_unreachable_compositor_disables_the_size_control() {
        let mut state = PointerUiState::default();
        state.settle_load(Err(CompClientError::NotAvailable("no socket".to_string())));
        assert_eq!(state.load, PointerLoad::Unavailable);
        assert!(!state.size_supported());
    }

    /// The older-compositor path #1: it answered, but reported no size.
    #[test]
    fn a_compositor_that_reports_no_size_disables_the_size_control() {
        let state = loaded("system", None);
        assert!(!state.size_supported());
    }

    #[test]
    fn a_compositor_that_reports_a_size_enables_the_size_control() {
        let state = loaded("brand", Some(32));
        assert!(state.size_supported());
    }

    /// The older-compositor path #2, learned at runtime: it reported a size
    /// but then REFUSED to change it. That refusal is sticky — re-clicking
    /// must not produce a stream of identical failures.
    #[test]
    fn a_refused_size_change_permanently_disables_the_size_control() {
        let mut state = loaded("system", Some(24));
        assert!(state.size_supported());
        state.settle_apply(PointerApply::Size, Err(CompClientError::Comp("unknown op".to_string())));
        assert!(!state.size_supported());
        // A later successful SOURCE change must not resurrect it.
        state.settle_apply(PointerApply::Source, Ok(cursor("brand", Some(24))));
        assert!(!state.size_supported());
    }

    /// …but NOT `invalid_cursor_size`, which means "that VALUE is not
    /// offered", not "this build cannot change the size". Disabling all five
    /// buttons because one of them disagreed with the compositor's step list
    /// would be the wrong repair — and would hide a real drift between the
    /// two sides behind a plausible-looking "unsupported" message.
    #[test]
    fn an_invalid_size_value_does_not_disable_the_whole_size_control() {
        let mut state = loaded("system", Some(24));
        state.settle_apply(PointerApply::Size, Err(CompClientError::Comp(comp_client::CURSOR_ERR_INVALID_SIZE.to_string())));
        assert!(state.size_supported(), "a rejected VALUE must not read as a missing OP");
        assert_eq!(state.last_failure, Some(PointerApply::Size));
    }

    /// A transport failure says nothing about what the compositor can do —
    /// only an explicit refusal does.
    #[test]
    fn a_transport_failure_on_a_size_change_does_not_mark_it_unsupported() {
        let mut state = loaded("system", Some(24));
        state.settle_apply(PointerApply::Size, Err(CompClientError::Timeout));
        assert!(state.size_supported(), "a timeout is not evidence the compositor lacks the op");
        assert_eq!(state.last_failure, Some(PointerApply::Size));
    }

    /// The selection only ever moves on the compositor's own answer — this
    /// is what makes "zero feedback" honest rather than optimistic.
    #[test]
    fn the_rendered_selection_follows_the_compositors_answer_not_the_request() {
        let mut state = loaded("system", Some(24));
        state.settle_apply(PointerApply::Source, Err(CompClientError::Comp("nope".to_string())));
        match &state.load {
            PointerLoad::Loaded(cursor) => assert_eq!(cursor.source, "system", "a refused switch must leave the old selection showing"),
            other => panic!("unexpected load state: {other:?}"),
        }
        state.settle_apply(PointerApply::Source, Ok(cursor("brand", Some(24))));
        match &state.load {
            PointerLoad::Loaded(cursor) => assert!(cursor.is_brand()),
            other => panic!("unexpected load state: {other:?}"),
        }
        assert_eq!(state.last_failure, None, "a success clears the previous failure line");
    }

    #[test]
    fn only_one_compositor_call_may_be_in_flight() {
        let mut state = PointerUiState::default();
        assert!(state.begin());
        assert!(!state.begin());
        state.settle_load(Ok(cursor("system", Some(24))));
        assert!(state.begin(), "settling releases the slot");
    }

    #[test]
    fn closing_the_panel_forgets_everything_so_the_next_open_re_reads() {
        let mut state = loaded("brand", Some(96));
        state.settle_apply(PointerApply::Size, Err(CompClientError::Comp("unknown op".to_string())));
        state.reset();
        assert_eq!(state, PointerUiState::default());
    }

    /// The board's radio must follow the operator's CHOICE while the screen
    /// tells the truth about what is drawn — the two diverge exactly when
    /// the chosen artwork is not installed.
    #[test]
    fn a_missing_theme_keeps_the_choice_selected_and_still_reports_the_truth() {
        let state = PointerUiState {
            load: PointerLoad::Loaded(CursorState {
                source: "system".to_string(),
                requested: Some("brand".to_string()),
                theme: Some("Adwaita".to_string()),
                size: Some(24),
                effective_size: Some(24),
                env_pinned: false,
                size_env_pinned: false,
            }),
            ..PointerUiState::default()
        };
        match &state.load {
            PointerLoad::Loaded(cursor) => {
                assert!(cursor.requested_is_brand(), "the paw card stays selected — it IS what was chosen");
                assert!(!cursor.is_brand(), "…but the previews draw what is really on screen");
                assert!(cursor.theme_missing(), "…and the screen says why the two differ");
            }
            other => panic!("unexpected load state: {other:?}"),
        }
    }

    /// A theme with no image at the requested step draws the nearest one it
    /// has. The segment control has to highlight what is DRAWN, or it claims
    /// a size the operator is not looking at.
    #[test]
    fn the_selected_segment_follows_the_drawn_size_not_the_stored_one() {
        let cursor = CursorState {
            source: "system".to_string(),
            requested: Some("system".to_string()),
            theme: None,
            size: Some(96),
            effective_size: Some(64),
            env_pinned: false,
            size_env_pinned: false,
        };
        assert_eq!(cursor.drawn_size(), Some(64));
        let state = PointerUiState { load: PointerLoad::Loaded(cursor), ..PointerUiState::default() };
        assert!(state.size_supported());
    }

    /// GNOME's `cursor_data[]`, verbatim — the one set of numbers §C.3 gives
    /// first-hand. A silent edit here would put a button on a size the
    /// cursor theme has no artwork for.
    #[test]
    fn the_five_sizes_match_gnomes_own_table() {
        assert_eq!(POINTER_SIZES.map(|(size, _)| size), [24, 32, 48, 64, 96]);
    }

    /// Every size button draws its cursor at full size inside the tray, so
    /// the tray has to be at least as tall as the largest one.
    #[test]
    fn the_preview_tray_fits_the_largest_cursor() {
        let largest = POINTER_SIZES.iter().map(|(size, _)| *size).max().expect("five sizes");
        assert!(SIZE_BUTTON_PREVIEW_HEIGHT >= largest as f32, "the 96px preview would be clipped");
        assert!(SIZE_BUTTON_WIDTH >= largest as f32, "the 96px preview would be clipped horizontally");
    }

    /// The panel has to fit its own widest row inside the fixed dev window,
    /// with the panel's 20px padding and the card's 18px both intact.
    #[test]
    fn the_panel_is_wide_enough_for_five_size_buttons_and_still_fits_the_window() {
        let (left, width) = (PANEL_LEFT, PANEL_WIDTH);
        let buttons = SIZE_BUTTON_WIDTH * 5. + 10. * 4.;
        let inner = width - 20. * 2. - 18. * 2.;
        assert!(inner >= buttons, "five size buttons ({buttons}) do not fit the card's inner width ({inner})");
        assert!(left > 0. && left * 2. + width <= 1440., "the panel is not centred inside the window");
    }
}
