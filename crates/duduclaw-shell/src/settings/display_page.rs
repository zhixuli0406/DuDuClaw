// D4b — 顯示 (display).
//
// The one page in this app that does NOT talk to the gateway: outputs belong
// to `duduclaw-comp`, which is the only process that knows what screens
// exist, and it is reached over the shell-control socket
// (`crate::comp_client`) — the same channel `overlay::pointer_settings` uses
// for the cursor. Routing screen configuration through the gateway would
// mean the gateway asking comp anyway, with one more hop to go wrong.
//
// ── What is real here, and what is honestly refused ────────────────────
// `get_outputs` is a real read. The two setters are only OFFERED when comp
// itself says it can perform them:
//
//   * 解析度 segments render only when comp reports `mode_switch_supported`
//     AND a non-empty mode list. Under QEMU/virtio — the environment this
//     first ships into — the mode list is empty and the flag is false, so
//     the card says so in one sentence instead of drawing a control that
//     would refuse. That is the expected outcome on that hardware, not a
//     failure.
//   * 縮放 segments are offered optimistically (comp reports a current
//     scale, so the axis exists), and a `scale_change_unsupported` refusal
//     makes them STICKILY disabled with the reason shown — the exact shape
//     `pointer_settings::PointerUiState::size_refused` uses, and for the
//     same reason: an older compositor does not become newer between two
//     clicks.
//
// Nothing on this page ever repaints optimistically. The selection moves
// only on comp's own answer.

use gpui::{div, prelude::*, px, Context, Div};

use duduclaw_native_gui::theme;

use super::widgets::{self, Tone};
use super::spawn_rpc;
use crate::comp_client::{self, CompClientError, CompOutput};
use crate::palette::ShellPalette;
use crate::ShellView;

/// How many modes a screen's picker shows before it stops and says how many
/// more there are. Nothing in this crate scrolls (see `settings/mod.rs`), and
/// a real monitor can report 30+ modes.
const MAX_MODES_SHOWN: usize = 8;

/// What this page knows about comp's screens. A local four-state enum rather
/// than `super::Load<T>`: this page's failures are `CompClientError`s, not
/// gateway `SettingsRpcError`s, and flattening the two error vocabularies
/// into one would mean rendering "沒有系統設定權限" for a dead compositor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum OutputsLoad {
    #[default]
    NotLoaded,
    Loading,
    Loaded(Vec<CompOutput>),
    /// comp could not be reached, or refused the read. Carries the operator-
    /// facing line, already classified — see `unavailable_message`.
    Unavailable(String),
}

impl OutputsLoad {
    fn needs_load(&self) -> bool {
        matches!(self, OutputsLoad::NotLoaded)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DisplayPageState {
    pub(crate) outputs: OutputsLoad,
    /// A comp call is in flight. The authoritative guard — every kick-off
    /// checks this FIRST, same contract `PointerUiState::in_flight` states.
    in_flight: bool,
    /// Set once comp refuses a scale change with `scale_change_unsupported`.
    /// Sticky for the panel's lifetime, and cleared by `reset` when the
    /// panel closes (a compositor restart is exactly the kind of thing that
    /// happens between two opens).
    scale_refused: bool,
    /// Set once comp refuses a mode change. Same stickiness, same reason.
    mode_refused: bool,
    /// The last failed change's operator-facing line, cleared by the next
    /// success.
    last_failure: Option<String>,
}

impl DisplayPageState {
    fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        true
    }

    /// Whether the 縮放 control may be offered at all.
    pub(crate) fn scale_supported(&self) -> bool {
        !self.scale_refused
    }

    /// Whether the 解析度 control may be offered for `output`. Both halves
    /// have to hold: comp must claim the capability AND must have reported
    /// modes to choose between.
    pub(crate) fn mode_supported(&self, output: &CompOutput) -> bool {
        !self.mode_refused && output.mode_switch_supported && !output.modes.is_empty()
    }

    fn settle_load(&mut self, result: Result<Vec<CompOutput>, CompClientError>) {
        self.in_flight = false;
        self.outputs = match result {
            Ok(outputs) => OutputsLoad::Loaded(outputs),
            Err(e) => {
                eprintln!("[settings/display] get_outputs failed: {e}");
                OutputsLoad::Unavailable(unavailable_message(&e))
            }
        };
    }

    /// Applies a settled setter. `kind` decides which capability a REFUSAL
    /// (comp answered, and said no) marks as unsupported — a transport
    /// failure says nothing about what comp can do and marks nothing, the
    /// same distinction `pointer_settings::settle_apply` draws.
    fn settle_apply(&mut self, kind: Change, result: Result<Option<Vec<CompOutput>>, CompClientError>) {
        self.in_flight = false;
        match result {
            Ok(Some(outputs)) => {
                self.outputs = OutputsLoad::Loaded(outputs);
                self.last_failure = None;
            }
            Ok(None) => {
                // Accepted, but comp told us nothing — re-read rather than
                // asserting a state we did not observe.
                self.outputs = OutputsLoad::NotLoaded;
                self.last_failure = None;
            }
            Err(e) => {
                eprintln!("[settings/display] applying {kind:?} failed: {e}");
                if let CompClientError::Comp(code) = &e {
                    match (kind, code.as_str()) {
                        (Change::Scale, comp_client::OUTPUT_ERR_SCALE_UNSUPPORTED) => self.scale_refused = true,
                        (Change::Mode, comp_client::OUTPUT_ERR_MODE_UNSUPPORTED) => self.mode_refused = true,
                        _ => {}
                    }
                }
                self.last_failure = Some(apply_failure_message(kind, &e));
            }
        }
    }
}

/// Which axis a setter was changing — only used to pick the failure copy and
/// to decide which capability a refusal disables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    Mode,
    Scale,
}

/// Pure: a comp read failure -> the operator's line. Transport detail stays
/// in the journal (the `eprintln!` at the call site), because an operator can
/// act on "顯示服務沒有回應" and not on a socket path.
pub(crate) fn unavailable_message(e: &CompClientError) -> String {
    match e {
        CompClientError::NotAvailable(_) => "找不到顯示服務，畫面設定暫時無法讀取。".to_string(),
        CompClientError::Timeout => "顯示服務沒有在時限內回應。".to_string(),
        CompClientError::Comp(_) => "目前的畫面服務版本不支援顯示設定。".to_string(),
        CompClientError::Io(_) | CompClientError::Protocol(_) => "無法讀取畫面設定。".to_string(),
    }
}

/// Pure: a comp setter failure -> the operator's line.
pub(crate) fn apply_failure_message(kind: Change, e: &CompClientError) -> String {
    if let CompClientError::Comp(code) = e {
        match code.as_str() {
            comp_client::OUTPUT_ERR_UNKNOWN_OUTPUT => return "找不到這個螢幕，請重新整理後再試。".to_string(),
            comp_client::OUTPUT_ERR_MODE_UNSUPPORTED => return "目前的畫面服務不支援切換解析度。".to_string(),
            comp_client::OUTPUT_ERR_SCALE_UNSUPPORTED => return "目前的畫面服務不支援調整縮放。".to_string(),
            _ => {}
        }
    }
    match kind {
        Change::Mode => "解析度沒有變更成功。".to_string(),
        Change::Scale => "縮放沒有變更成功。".to_string(),
    }
}

// ── Kick-offs ────────────────────────────────────────────────────────────

pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if !view.settings_ui.display.outputs.needs_load() || !view.settings_ui.display.begin() {
        return;
    }
    view.settings_ui.display.outputs = OutputsLoad::Loading;
    spawn_rpc(cx, comp_client::get_outputs, |view, result, cx| {
        view.settings_ui.display.settle_load(result);
        cx.notify();
    });
}

fn apply_scale(view: &mut ShellView, output: String, scale_pct: u32, cx: &mut Context<ShellView>) {
    if !view.settings_ui.display.begin() {
        return;
    }
    cx.notify();
    spawn_rpc(
        cx,
        move || comp_client::set_output_scale(&output, scale_pct),
        |view, result, cx| {
            view.settings_ui.display.settle_apply(Change::Scale, result);
            ensure_loaded(view, cx);
            cx.notify();
        },
    );
}

fn apply_mode(view: &mut ShellView, output: String, width: u32, height: u32, refresh_mhz: u32, cx: &mut Context<ShellView>) {
    if !view.settings_ui.display.begin() {
        return;
    }
    cx.notify();
    spawn_rpc(
        cx,
        move || comp_client::set_output_mode(&output, width, height, refresh_mhz),
        |view, result, cx| {
            view.settings_ui.display.settle_apply(Change::Mode, result);
            ensure_loaded(view, cx);
            cx.notify();
        },
    );
}

// ── Render ───────────────────────────────────────────────────────────────

pub(crate) fn render(body: Div, state: &DisplayPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    let mut body = body;
    match &state.outputs {
        OutputsLoad::NotLoaded | OutputsLoad::Loading => {
            body = body.child(widgets::card(palette).child(widgets::notice_static("讀取中…", Tone::Muted, palette)));
        }
        OutputsLoad::Unavailable(message) => {
            body = body.child(
                widgets::card(palette)
                    .child(widgets::card_header("螢幕", None, palette))
                    .child(widgets::notice(message.clone(), Tone::Warning, palette)),
            );
        }
        OutputsLoad::Loaded(outputs) if outputs.is_empty() => {
            // Structurally near-impossible (this shell is drawn ON a screen),
            // but an empty list is still an answer and must not render as a
            // blank page.
            body = body.child(
                widgets::card(palette)
                    .child(widgets::card_header("螢幕", None, palette))
                    .child(widgets::notice_static("畫面服務沒有回報任何螢幕。", Tone::Warning, palette)),
            );
        }
        OutputsLoad::Loaded(outputs) => {
            for output in outputs {
                body = body.child(output_card(output, state, palette, cx));
            }
        }
    }
    if let Some(failure) = &state.last_failure {
        body = body.child(widgets::notice(failure.clone(), Tone::Danger, palette));
    }
    body
}

fn output_card(output: &CompOutput, state: &DisplayPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let mut card = widgets::card(palette)
        .child(widgets::card_header("螢幕", Some(widgets::status_pill(output.name.clone(), Tone::Muted, palette).into_any_element()), palette))
        .child(widgets::value_row("名稱", output.display_name(), palette))
        .child(widgets::value_row("目前解析度", output.current_mode_label().unwrap_or_else(|| "—".to_string()), palette));

    if let (Some(w), Some(h)) = (output.physical_width_mm, output.physical_height_mm) {
        if w > 0 && h > 0 {
            card = card.child(widgets::value_row("實體尺寸", format!("{w} × {h} mm"), palette));
        }
    }

    card.child(scale_section(output, state, palette, cx)).child(mode_section(output, state, palette, cx))
}

fn scale_section(output: &CompOutput, state: &DisplayPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let mut section = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(12.))
        .border_t_1()
        .border_color(palette.border())
        .child(widgets::field_label("縮放", palette));

    let Some(current) = output.scale_pct else {
        // comp reported no scale at all — an older build. Nothing to select
        // against, so nothing is offered.
        return section.child(widgets::notice_static("目前的畫面服務沒有回報縮放比例，因此無法調整。", Tone::Warning, palette));
    };

    let enabled = state.scale_supported() && !state.in_flight;
    let mut row = div().flex().gap(px(8.));
    for (index, step) in comp_client::OUTPUT_SCALE_STEPS.into_iter().enumerate() {
        let name = output.name.clone();
        row = row.child(widgets::segment(
            ("settings-display-scale", index),
            format!("{step}%"),
            step == current,
            enabled,
            palette,
            cx.listener(move |view, _ev, _window, cx| apply_scale(view, name.clone(), step, cx)),
        ));
    }
    section = section.child(row);
    if !state.scale_supported() {
        section = section.child(widgets::notice_static("目前的畫面服務不支援調整縮放。", Tone::Warning, palette));
    }
    section
}

fn mode_section(output: &CompOutput, state: &DisplayPageState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    let section = div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .pt(px(12.))
        .border_t_1()
        .border_color(palette.border())
        .child(widgets::field_label("解析度", palette));

    if output.modes.is_empty() {
        return section.child(widgets::notice_static(
            "這個螢幕沒有回報可選的解析度，只能使用目前的設定。虛擬機的顯示卡通常就是這樣。",
            Tone::Muted,
            palette,
        ));
    }
    if !state.mode_supported(output) {
        let mut listing = div().flex().flex_col().gap(px(4.));
        for mode in output.modes.iter().take(MAX_MODES_SHOWN) {
            listing = listing.child(
                div()
                    .text_size(px(theme::TEXT_XS))
                    .text_color(theme::alpha(palette.text_faint, 1.0))
                    .child(mode.label()),
            );
        }
        return section
            .child(widgets::notice_static("目前的畫面服務不支援切換解析度，以下是這個螢幕回報的模式：", Tone::Warning, palette))
            .child(listing)
            .child(overflow_note(output.modes.len(), palette));
    }

    let enabled = !state.in_flight;
    let mut row = div().flex().flex_wrap().gap(px(8.));
    for (index, mode) in output.modes.iter().take(MAX_MODES_SHOWN).enumerate() {
        let name = output.name.clone();
        let (w, h, mhz) = (mode.width, mode.height, mode.refresh_mhz.unwrap_or(0));
        row = row.child(widgets::segment(
            ("settings-display-mode", index),
            mode.label(),
            mode.current,
            enabled,
            palette,
            cx.listener(move |view, _ev, _window, cx| apply_mode(view, name.clone(), w, h, mhz, cx)),
        ));
    }
    section.child(row).child(overflow_note(output.modes.len(), palette))
}

/// The honest tail of a capped list — never silent clipping.
fn overflow_note(total: usize, palette: ShellPalette) -> Div {
    if total <= MAX_MODES_SHOWN {
        return div();
    }
    widgets::notice(format!("另有 {} 個模式未顯示。", total - MAX_MODES_SHOWN), Tone::Muted, palette)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::comp_client::CompOutputMode;

    fn mode(w: u32, h: u32, mhz: Option<u32>, current: bool) -> CompOutputMode {
        CompOutputMode { width: w, height: h, refresh_mhz: mhz, preferred: false, current }
    }

    fn output(modes: Vec<CompOutputMode>, supported: bool) -> CompOutput {
        CompOutput {
            name: "Virtual-1".into(),
            description: None,
            make: None,
            model: None,
            width: Some(1920),
            height: Some(1080),
            refresh_mhz: Some(60000),
            scale_pct: Some(100),
            physical_width_mm: Some(0),
            physical_height_mm: Some(0),
            modes,
            mode_switch_supported: supported,
        }
    }

    #[test]
    fn a_fresh_page_has_asked_nothing_and_assumes_no_capability_is_broken() {
        let state = DisplayPageState::default();
        assert!(state.outputs.needs_load());
        assert!(state.scale_supported(), "nothing has been refused yet");
    }

    /// The QEMU/virtio case this first ships into: comp answers, reports a
    /// screen, and offers no modes. The page must not present a picker.
    #[test]
    fn a_screen_with_no_modes_never_offers_a_resolution_picker() {
        let state = DisplayPageState::default();
        assert!(!state.mode_supported(&output(vec![], false)));
        // …not even if comp claims the capability: with nothing to choose
        // between, a picker would be an empty control.
        assert!(!state.mode_supported(&output(vec![], true)));
    }

    /// Both halves are required — a mode list comp cannot act on is a
    /// listing, not a control.
    #[test]
    fn modes_without_the_capability_flag_are_listed_but_not_offered() {
        let state = DisplayPageState::default();
        assert!(!state.mode_supported(&output(vec![mode(1920, 1080, Some(60000), true)], false)));
        assert!(state.mode_supported(&output(vec![mode(1920, 1080, Some(60000), true)], true)));
    }

    #[test]
    fn a_refused_scale_change_permanently_disables_the_scale_control() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        state.settle_apply(Change::Scale, Err(CompClientError::Comp(comp_client::OUTPUT_ERR_SCALE_UNSUPPORTED.to_string())));
        assert!(!state.scale_supported());
        // A later successful MODE change must not resurrect it.
        assert!(state.begin());
        state.settle_apply(Change::Mode, Ok(None));
        assert!(!state.scale_supported());
    }

    #[test]
    fn a_refused_mode_change_permanently_disables_the_mode_control() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        state.settle_apply(Change::Mode, Err(CompClientError::Comp(comp_client::OUTPUT_ERR_MODE_UNSUPPORTED.to_string())));
        assert!(!state.mode_supported(&output(vec![mode(1920, 1080, Some(60000), true)], true)));
    }

    /// A transport failure says nothing about what comp can do — only an
    /// explicit refusal does.
    #[test]
    fn a_timeout_does_not_mark_anything_unsupported() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        state.settle_apply(Change::Scale, Err(CompClientError::Timeout));
        assert!(state.scale_supported(), "a timeout is not evidence the compositor lacks the op");
        assert!(state.last_failure.is_some(), "…but it is still reported");
    }

    /// An unrelated refusal code must not disable the whole axis — same
    /// distinction `pointer_settings` draws for `invalid_cursor_size`.
    #[test]
    fn an_unknown_output_refusal_does_not_disable_the_control() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        state.settle_apply(Change::Scale, Err(CompClientError::Comp(comp_client::OUTPUT_ERR_UNKNOWN_OUTPUT.to_string())));
        assert!(state.scale_supported());
        assert_eq!(state.last_failure.as_deref(), Some("找不到這個螢幕，請重新整理後再試。"));
    }

    /// A setter that acked without echoing state must trigger a re-read, not
    /// a locally-invented one.
    #[test]
    fn an_ack_with_no_echoed_state_forces_a_reread() {
        let mut state = DisplayPageState::default();
        state.outputs = OutputsLoad::Loaded(vec![output(vec![], false)]);
        assert!(state.begin());
        state.settle_apply(Change::Scale, Ok(None));
        assert!(state.outputs.needs_load(), "the page must go ask rather than assert what it did not observe");
    }

    #[test]
    fn only_one_comp_call_may_be_in_flight() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        assert!(!state.begin());
        state.settle_load(Ok(vec![]));
        assert!(state.begin(), "settling releases the slot");
    }

    #[test]
    fn an_unreachable_compositor_is_reported_without_borrowing_the_gateways_vocabulary() {
        let mut state = DisplayPageState::default();
        assert!(state.begin());
        state.settle_load(Err(CompClientError::NotAvailable("no socket".into())));
        match &state.outputs {
            OutputsLoad::Unavailable(msg) => {
                assert!(msg.contains("顯示服務"), "{msg}");
                assert!(!msg.contains("權限"), "a dead compositor is not a permission problem: {msg}");
            }
            other => panic!("unexpected state: {other:?}"),
        }
    }

    // ── the label helpers (`comp_client`, exercised from their consumer) ──

    #[test]
    fn refresh_rates_render_without_inventing_precision() {
        assert_eq!(comp_client::format_refresh(60000), "60 Hz");
        assert_eq!(comp_client::format_refresh(59940), "59.94 Hz");
        assert_eq!(comp_client::format_refresh(75000), "75 Hz");
    }

    #[test]
    fn a_mode_without_a_reported_refresh_rate_omits_it_rather_than_faking_60hz() {
        assert_eq!(mode(1280, 720, None, false).label(), "1280 × 720");
        assert_eq!(mode(1280, 720, Some(60000), false).label(), "1280 × 720 · 60 Hz");
    }

    #[test]
    fn a_screen_name_falls_back_through_description_make_model_and_connector() {
        let mut o = output(vec![], false);
        assert_eq!(o.display_name(), "Virtual-1", "connector name is the last resort");
        o.make = Some("Dell".into());
        o.model = Some("U2720Q".into());
        assert_eq!(o.display_name(), "Dell U2720Q");
        o.description = Some("Dell 27-inch".into());
        assert_eq!(o.display_name(), "Dell 27-inch");
    }

    #[test]
    fn a_screen_that_reports_no_size_says_so_rather_than_showing_zero() {
        let mut o = output(vec![], false);
        o.width = None;
        assert_eq!(o.current_mode_label(), None);
    }
}
