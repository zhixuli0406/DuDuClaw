// 共駕 row inside ControlCenter's 「AI 團隊」 card — A2 (2026-08-23).
//
// Design: `commercial/docs/DESIGN-codrive-desktop-2026-08.md` §3.5「殼層
// UX」 asks for three things on the shell side; this file is the SMALLEST
// of them and deliberately stops there:
//
//   * goal 卡 driving badges  — the management UI's job, not this panel's.
//   * panel agent 頭像狀態點  — Home's dock, a different surface.
//   * 控制中心「接管模式」開關 — THIS row.
//
// What it renders is one line of status plus one button. Anything deeper
// (per-agent driving state, a session log, the shadow/watch flags comp also
// reports) is left for a later round rather than guessed at here — see the
// "欠帳" section of this round's own handover note.
//
// ── The vocabulary problem, stated once ─────────────────────────────────
// comp's three modes are `human` / `codrive` / `handover`. NONE of those
// words appear on screen. A person sitting at this machine is being told one
// thing — who has the keyboard and mouse right now — and the answer is a
// sentence, not a state name:
//
//   human    -> 「目前由你操作」
//   codrive  -> 「AI 正在操作這台電腦」
//   handover -> 「已交還給你，AI 暫停中」
//
// The same rule governs the button: 「接管」 and 「交還給 AI」, never
// `take_wheel`/`hand_back`. The wire tokens live in `codrive_client` and
// stop there.
//
// ── Copy lives here as literals, on purpose ─────────────────────────────
// `overlay/controlcenter.rs`'s own header comment establishes that panel's
// convention: its own strings are plain zh-TW literals, and only copy that
// belongs to an already-i18n'd SCREEN (the pointer settings entry) is routed
// through `crate::i18n`. This row is new copy with no i18n'd screen behind
// it, so it follows the file it renders inside rather than importing a
// catalog for eleven strings.
//
// ── Zero emoji ──────────────────────────────────────────────────────────
// This crate's shell convention (see `crate::icons`): icons are hand-drawn
// SVG, never emoji. There is no driving/steering glyph in the icon set yet,
// so this row uses a coloured status dot plus text — the same treatment
// `home/home_dock.rs` uses for its own agent status dots — rather than
// reaching for a placeholder emoji.

use gpui::{div, prelude::*, px, Context, Div, FontWeight, Stateful};

use duduclaw_native_gui::theme;

use crate::codrive_client::{self, CodriveState, DriveMode};
use crate::comp_client::CompClientError;
use crate::palette::ShellPalette;
use crate::ShellView;

// ── Copy ─────────────────────────────────────────────────────────────────

const ROW_LABEL: &str = "共駕";
const STATUS_HUMAN: &str = "目前由你操作";
const STATUS_CODRIVE: &str = "AI 正在操作這台電腦";
const STATUS_HANDOVER: &str = "已交還給你，AI 暫停中";
const STATUS_LOADING: &str = "正在確認…";
/// comp answered, and answered "I have no such thing" — an older compositor.
const STATUS_UNSUPPORTED: &str = "這台機器目前不支援共駕";
/// comp could not be reached at all.
const STATUS_UNAVAILABLE: &str = "目前無法確認共駕狀態";
/// comp answered with a mode this build does not understand. Honest — and
/// specifically NOT 「目前由你操作」, which would be a claim nobody verified.
const STATUS_UNKNOWN_MODE: &str = "無法確認目前由誰操作";
const BUTTON_TAKE_WHEEL: &str = "接管";
const BUTTON_HAND_BACK: &str = "交還給 AI";
/// Shown only while the 交還 button is the one on offer. Super+Enter is the
/// pre-existing hand-back gesture (already verified on real hardware); this
/// button is its twin, so the row says so rather than letting the two look
/// like different features.
const HINT_HAND_BACK: &str = "也可以按 Super+Enter 交還";
const FAILED_LINE: &str = "剛才的操作沒有成功，請再試一次";

// ── State ────────────────────────────────────────────────────────────────

/// What this row knows about the compositor's driving state.
///
/// Five states, not `Option<CodriveState>`: "haven't asked", "asking", "this
/// machine has no co-driving", "could not reach the compositor" and "here it
/// is" are five different facts. Collapsing any of them together is how a
/// surface ends up showing a blank that means "loading" — or, far worse
/// here, a calm 「目前由你操作」 that means "we have no idea".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) enum CodriveLoad {
    #[default]
    NotLoaded,
    Loading,
    Loaded(CodriveState),
    /// comp answered `{"ok":false,…}` — it is running, and it does not have
    /// these ops. A build older than A2.
    Unsupported,
    /// comp could not be reached (no socket, timeout, I/O). The ordinary
    /// case on a dev Mac.
    Unavailable,
}

/// The two things a person can do from this row. A closed enum on the shell
/// side, mapped to comp's wire tokens in exactly one place ([`Self::wire`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DriveAction {
    /// Freeze the agent's seat — the person takes the wheel.
    TakeWheel,
    /// Resume the agent's seat — the person gives it back.
    HandBack,
}

impl DriveAction {
    fn wire(self) -> &'static str {
        match self {
            DriveAction::TakeWheel => codrive_client::CODRIVE_ACTION_TAKE_WHEEL,
            DriveAction::HandBack => codrive_client::CODRIVE_ACTION_HAND_BACK,
        }
    }

    fn label(self) -> &'static str {
        match self {
            DriveAction::TakeWheel => BUTTON_TAKE_WHEEL,
            DriveAction::HandBack => BUTTON_HAND_BACK,
        }
    }
}

/// Which colour the status dot carries. Named by MEANING, not by hue, so the
/// decision table below stays testable without a palette (and so a theme
/// change cannot silently re-map what a colour means).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DotTone {
    /// Nothing is being driven for you, or nothing is known.
    Neutral,
    /// The agent is driving this desktop right now.
    Driving,
    /// A session exists but is paused in the person's hands.
    Paused,
}

/// Ephemeral state for the 共駕 row — lives on `OverlayUiState` (see
/// `overlay.rs`), which is what lets `controlcenter::ai_team_card` render
/// this row without a new parameter threaded through four call sites.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct CodriveUiState {
    load: CodriveLoad,
    /// A compositor call is in flight. The authoritative guard — every
    /// kick-off checks this FIRST, the same contract
    /// `overlay::controlcenter::kick_off_audio_call` and
    /// `overlay::pointer_settings::PointerUiState` both document.
    in_flight: bool,
    /// The most recent action that failed, if any. Cleared by the next
    /// success.
    last_failure: Option<DriveAction>,
}

impl CodriveUiState {
    /// The sentence and dot this row shows right now. A pure function of the
    /// state — no palette, no gpui — so the whole table is testable.
    fn status(&self) -> (&'static str, DotTone) {
        match &self.load {
            CodriveLoad::NotLoaded | CodriveLoad::Loading => (STATUS_LOADING, DotTone::Neutral),
            CodriveLoad::Unsupported => (STATUS_UNSUPPORTED, DotTone::Neutral),
            CodriveLoad::Unavailable => (STATUS_UNAVAILABLE, DotTone::Neutral),
            CodriveLoad::Loaded(state) => match &state.mode {
                DriveMode::Human => (STATUS_HUMAN, DotTone::Neutral),
                DriveMode::CoDrive => (STATUS_CODRIVE, DotTone::Driving),
                DriveMode::Handover => (STATUS_HANDOVER, DotTone::Paused),
                DriveMode::Unknown(_) => (STATUS_UNKNOWN_MODE, DotTone::Neutral),
            },
        }
    }

    /// Which action the button performs, or `None` when there is nothing for
    /// it to do and it renders disabled.
    ///
    /// Three of the four arms are the obvious ones. The fourth is a judgment
    /// call worth stating: an UNKNOWN mode still offers 接管. The A2
    /// contract §4.2 makes `take_wheel` **always allowed** — "fail-safe
    /// 方向：任何東西都可以叫 agent 停" — and a mode this build cannot read
    /// means comp is newer than us and something may well be driving. Taking
    /// the stop affordance away precisely when we are least sure would be
    /// the wrong way to be cautious. `human` is different: there we DO know,
    /// from comp, that no session exists to take over.
    fn offered_action(&self) -> Option<DriveAction> {
        let CodriveLoad::Loaded(state) = &self.load else {
            return None;
        };
        match &state.mode {
            DriveMode::CoDrive => Some(DriveAction::TakeWheel),
            DriveMode::Handover => Some(DriveAction::HandBack),
            DriveMode::Unknown(_) => Some(DriveAction::TakeWheel),
            DriveMode::Human => None,
        }
    }

    /// What the button SAYS, including while it is disabled. A disabled
    /// button still has to be labelled something, and 接管 is the honest
    /// placeholder: it is the action that would exist if a session did.
    fn button_label(&self) -> &'static str {
        self.offered_action().unwrap_or(DriveAction::TakeWheel).label()
    }

    /// Whether the button can be pressed. Separate from
    /// [`Self::offered_action`] because an in-flight call disables the
    /// button without changing which action it would perform.
    fn button_enabled(&self) -> bool {
        !self.in_flight && self.offered_action().is_some()
    }

    fn begin(&mut self) -> bool {
        if self.in_flight {
            return false;
        }
        self.in_flight = true;
        true
    }

    fn settle_load(&mut self, result: Result<CodriveState, CompClientError>) {
        self.in_flight = false;
        self.load = match result {
            Ok(state) => CodriveLoad::Loaded(state),
            // An explicit refusal to a READ op means comp is there and does
            // not have it — a build older than A2. Everything else (no
            // socket, timeout, I/O, a malformed line) says nothing about
            // what comp can do, only that we could not ask.
            Err(CompClientError::Comp(code)) => {
                eprintln!("[codrive] codrive_status refused by comp ({code}) — treating this build as having no co-driving");
                CodriveLoad::Unsupported
            }
            Err(e) => {
                eprintln!("[codrive] codrive_status failed: {e}");
                CodriveLoad::Unavailable
            }
        };
    }

    fn settle_drive(&mut self, action: DriveAction, result: Result<CodriveState, CompClientError>) {
        self.in_flight = false;
        match result {
            Ok(state) => {
                self.load = CodriveLoad::Loaded(state);
                self.last_failure = None;
            }
            Err(e) => {
                if matches!(&e, CompClientError::Comp(code) if code == codrive_client::CODRIVE_ERR_INVALID_ACTION) {
                    // Not an ordinary refusal: this build sent a token comp
                    // does not accept, which means the two sides' closed
                    // sets have drifted apart. Worth its own line — the
                    // generic one below would bury a real bug.
                    eprintln!(
                        "[codrive] comp rejected the action token {:?} as invalid — the shell and compositor action vocabularies have drifted",
                        action.wire()
                    );
                } else {
                    eprintln!("[codrive] {action:?} failed: {e}");
                }
                // The displayed mode is deliberately left ALONE. A refused
                // action changed nothing, so the last state comp actually
                // reported is still the truest thing this row knows — the
                // same discipline `pointer_settings::settle_apply` follows.
                self.last_failure = Some(action);
            }
        }
    }

    /// Called whenever an overlay closes, so the next time ControlCenter
    /// opens the row re-reads instead of showing a snapshot from minutes
    /// ago. Driving state changes without anyone touching this panel — a
    /// stale 「AI 正在操作這台電腦」 would be a lie with a straight face.
    ///
    /// Same contract, same call sites, as `pointer_settings::PointerUiState
    /// ::reset` (`main.rs`'s three overlay-close paths plus
    /// `chrome::windows`' own). Cheap: a no-op when nothing was ever read.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

// ── Kick-offs ────────────────────────────────────────────────────────────
// Same background-thread -> `std::sync::mpsc` -> `cx.spawn` poll-loop bridge
// `overlay::pointer_settings` and `overlay::controlcenter::
// kick_off_audio_call` already use: `codrive_client` is blocking and gpui's
// main thread must never wait on a socket.

/// Reads comp's driving state once, if it has not been read yet.
///
/// Safe to call from a render body: it claims `NotLoaded` before spawning
/// anything, so a repaint mid-flight cannot stack a second read. That
/// single-arm discipline is what makes "refresh when the panel opens" work
/// WITHOUT a resident polling thread — the panel's first render arms it, the
/// settle disarms it, and `CodriveUiState::reset` on overlay close is what
/// re-arms it for the next open.
pub(crate) fn ensure_loaded(view: &mut ShellView, cx: &mut Context<ShellView>) {
    if view.overlay_ui.codrive.load != CodriveLoad::NotLoaded || !view.overlay_ui.codrive.begin() {
        return;
    }
    view.overlay_ui.codrive.load = CodriveLoad::Loading;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(codrive_client::codrive_status());
    });
    poll_into(cx, rx, move |view, result, cx| {
        view.overlay_ui.codrive.settle_load(result);
        cx.notify();
    });
}

/// Takes or hands back the wheel. The response IS the new state (comp
/// answers `codrive_drive` with the post-action status block), so nothing
/// here is repainted optimistically — the row only ever shows what the
/// compositor said.
fn apply_drive(view: &mut ShellView, action: DriveAction, cx: &mut Context<ShellView>) {
    if !view.overlay_ui.codrive.begin() {
        return;
    }
    cx.notify();
    let wire = action.wire();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(codrive_client::codrive_drive(wire));
    });
    poll_into(cx, rx, move |view, result, cx| {
        view.overlay_ui.codrive.settle_drive(action, result);
        cx.notify();
    });
}

/// The shared `try_recv` + paced-timer poll loop.
///
/// Byte-for-byte the shape `overlay::pointer_settings::poll_into` uses (30ms
/// tick, break on either arm). It is duplicated rather than shared because
/// that one is private to its own module and the alternatives are both
/// worse: widening a sibling surface's private helper so an unrelated one
/// can borrow it, or standing up a new shared module for twelve lines.
/// Consolidating all three copies (this one, pointer_settings', and
/// `controlcenter::kick_off_audio_call`'s inline one) into a single
/// `overlay/async_bridge.rs` is a real cleanup, and it belongs to a round
/// that is allowed to touch all three files.
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

/// One row: status dot + 共駕 label + a sentence, with the drive button on
/// the right.
///
/// Geometry deliberately matches `controlcenter::switch_row` (px 14 / py 11,
/// 10px gap, 13px medium label over an 11px description) so this reads as a
/// sibling of the three toggles below it rather than a bolted-on card.
pub(super) fn render(state: &CodriveUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Div {
    // Reading comp is I/O, and this crate's rule is that I/O is
    // click-triggered rather than a render-time side effect. This one call is
    // the documented exception, exactly as `overlay::pointer_settings::
    // render` makes it: opening the panel IS the click, and there is no other
    // moment to hang it on without making the operator press something twice
    // to see the state they just asked for. It is idempotent — see
    // `ensure_loaded`.
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, ensure_loaded);
    })
    .detach();

    let (status_text, tone) = state.status();
    // Same divider pair every other row in this card uses.
    let divider = if palette.is_dark() { theme::alpha(0xffffff, 0.08) } else { theme::alpha(0xf0f0f2, 1.0) };

    let mut text_column = div()
        .flex_1()
        .flex()
        .flex_col()
        .child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::alpha(palette.foreground, 1.0))
                .child(ROW_LABEL),
        )
        // `#9f9fa9` in BOTH themes — the same non-inverting literal every
        // sibling row in this card uses for its description line; see
        // `overlay/controlcenter.rs`'s own header comment for why this one
        // role does not go through `palette.text_faint`.
        .child(div().text_size(px(11.)).text_color(theme::alpha(0x9f9fa9, 1.0)).child(status_text));

    // The Super+Enter hint rides UNDER the status line rather than literally
    // beside the button: this panel is 372px wide, and a second column next
    // to a 「交還給 AI」 button would squeeze the sentence above it. It stays
    // visually adjacent to the button either way, and only appears while
    // 交還 is the action on offer.
    if state.offered_action() == Some(DriveAction::HandBack) {
        text_column = text_column.child(
            div().text_size(px(10.)).text_color(theme::alpha(palette.text_faint, 1.0)).child(HINT_HAND_BACK),
        );
    }

    if state.last_failure.is_some() {
        text_column =
            text_column.child(div().text_size(px(10.)).text_color(theme::alpha(palette.destructive, 1.0)).child(FAILED_LINE));
    }

    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .px(px(14.))
        .py(px(11.))
        .border_b_1()
        .border_color(divider)
        .child(status_dot(tone, palette))
        .child(text_column)
        .child(drive_button(state, palette, cx))
}

/// The small round state indicator. Text plus a dot, never an emoji — see
/// this file's header comment.
fn status_dot(tone: DotTone, palette: ShellPalette) -> Div {
    // `brand` for "the agent is driving" is the design brief's own choice
    // (DESIGN §3.5). `warning_dot` for the paused state is this palette's
    // documented role for exactly this element — a small circular status dot
    // (see `palette.rs`'s own field comment), which is why the paused state
    // does not reach for the plain `warning` badge token. Neutral uses
    // `icon_inactive()`, the same "this control is not doing anything right
    // now" grey the rest of the shell uses.
    let hex = match tone {
        DotTone::Neutral => palette.icon_inactive(),
        DotTone::Driving => palette.brand,
        DotTone::Paused => palette.warning_dot,
    };
    div().w(px(8.)).h(px(8.)).rounded(px(8.)).flex_none().bg(theme::alpha(hex, 1.0))
}

/// 接管 / 交還給 AI.
///
/// Neutral (bordered, not brand-filled) in BOTH states, matching
/// `overlay::notifications::action_button`'s secondary treatment. It is
/// tempting to make 接管 a loud primary button, but the real emergency stop
/// on this machine is Super+Esc — enforced inside the compositor and
/// structurally unreachable by an agent (see `codrive_client`'s own trust
/// boundary note). This button is the convenient version of a stop that
/// already exists, so it does not need to shout.
fn drive_button(state: &CodriveUiState, palette: ShellPalette, cx: &mut Context<ShellView>) -> Stateful<Div> {
    let enabled = state.button_enabled();
    let bg = if palette.is_dark() { palette.surface_hover } else { palette.surface_raised };
    let border_color: gpui::Hsla = if palette.is_dark() { theme::alpha(0xffffff, 0.14).into() } else { palette.border() };
    let text_hex = if enabled { palette.text_secondary } else { palette.text_faint };

    let mut button = div()
        .id("cc-codrive-action")
        .flex_none()
        .bg(theme::alpha(bg, 1.0))
        .text_color(theme::alpha(text_hex, 1.0))
        .border_1()
        .border_color(border_color)
        .rounded(px(8.))
        .px(px(12.))
        .py(px(5.))
        .text_size(px(12.))
        .font_weight(FontWeight::MEDIUM)
        .child(state.button_label());

    if let Some(action) = state.offered_action().filter(|_| enabled) {
        let listener = cx.listener(move |view, _ev, _window, cx| apply_drive(view, action, cx));
        button = button.cursor_pointer().hover(|style| style.bg(theme::alpha(palette.surface_hover, 1.0))).on_click(listener);
    }
    button
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded(mode: DriveMode) -> CodriveUiState {
        CodriveUiState {
            load: CodriveLoad::Loaded(CodriveState { mode, session_active: true, ..CodriveState::default() }),
            ..CodriveUiState::default()
        }
    }

    #[test]
    fn a_fresh_row_has_not_asked_the_compositor_anything() {
        let state = CodriveUiState::default();
        assert_eq!(state.load, CodriveLoad::NotLoaded);
        assert_eq!(state.status(), (STATUS_LOADING, DotTone::Neutral));
        assert!(!state.button_enabled(), "nothing is known yet, so nothing may be offered");
    }

    /// The whole decision table in one place. If this ever disagrees with the
    /// round's own UI table, one of the two is wrong.
    #[test]
    fn the_three_modes_each_get_their_own_sentence_dot_and_button() {
        let human = loaded(DriveMode::Human);
        assert_eq!(human.status(), (STATUS_HUMAN, DotTone::Neutral));
        assert_eq!(human.offered_action(), None, "there is no session to take over");
        assert!(!human.button_enabled());
        assert_eq!(human.button_label(), BUTTON_TAKE_WHEEL, "a disabled button still needs an honest label");

        let codrive = loaded(DriveMode::CoDrive);
        assert_eq!(codrive.status(), (STATUS_CODRIVE, DotTone::Driving));
        assert_eq!(codrive.offered_action(), Some(DriveAction::TakeWheel));
        assert!(codrive.button_enabled());
        assert_eq!(codrive.button_label(), BUTTON_TAKE_WHEEL);

        let handover = loaded(DriveMode::Handover);
        assert_eq!(handover.status(), (STATUS_HANDOVER, DotTone::Paused));
        assert_eq!(handover.offered_action(), Some(DriveAction::HandBack));
        assert!(handover.button_enabled());
        assert_eq!(handover.button_label(), BUTTON_HAND_BACK);
    }

    /// Contract §1: a shadow session leaves the shared desktop's wheel with
    /// the person, so this row must say 「目前由你操作」 and offer nothing —
    /// the flag rides alongside `mode`, it does not become a fourth state.
    #[test]
    fn a_shadow_session_still_reads_as_human_control() {
        let state = CodriveUiState {
            load: CodriveLoad::Loaded(CodriveState {
                mode: DriveMode::Human,
                shadow: true,
                session_active: true,
                ..CodriveState::default()
            }),
            ..CodriveUiState::default()
        };
        assert_eq!(state.status(), (STATUS_HUMAN, DotTone::Neutral));
        assert_eq!(state.offered_action(), None);
    }

    /// A mode this build cannot read must never be dressed up as human
    /// control — and, because `take_wheel` is always allowed by contract, it
    /// must still leave the stop affordance reachable.
    #[test]
    fn an_unknown_mode_is_reported_honestly_and_still_offers_the_stop() {
        let state = loaded(DriveMode::Unknown("supervising".to_string()));
        let (text, tone) = state.status();
        assert_eq!(text, STATUS_UNKNOWN_MODE);
        assert_ne!(text, STATUS_HUMAN, "\"cannot tell\" must not be rendered as \"you are driving\"");
        assert_eq!(tone, DotTone::Neutral);
        assert_eq!(state.offered_action(), Some(DriveAction::TakeWheel));
        assert!(state.button_enabled());
    }

    /// An older compositor: it answered, and it said no.
    #[test]
    fn a_refused_status_read_reports_no_co_driving_on_this_machine() {
        let mut state = CodriveUiState::default();
        state.settle_load(Err(CompClientError::Comp("unknown_op".to_string())));
        assert_eq!(state.load, CodriveLoad::Unsupported);
        assert_eq!(state.status(), (STATUS_UNSUPPORTED, DotTone::Neutral));
        assert!(!state.button_enabled());
    }

    /// The dev-Mac path, and a Linux box whose compositor is down. Distinct
    /// from `Unsupported`: we could not ask, which is not the same as being
    /// told no.
    #[test]
    fn an_unreachable_compositor_is_not_the_same_as_an_unsupported_one() {
        let mut state = CodriveUiState::default();
        state.settle_load(Err(CompClientError::NotAvailable("no socket".to_string())));
        assert_eq!(state.load, CodriveLoad::Unavailable);
        assert_eq!(state.status(), (STATUS_UNAVAILABLE, DotTone::Neutral));
        assert!(!state.button_enabled());

        let mut timed_out = CodriveUiState::default();
        timed_out.settle_load(Err(CompClientError::Timeout));
        assert_eq!(timed_out.load, CodriveLoad::Unavailable, "a timeout says nothing about what comp supports");
    }

    /// The row only ever shows what comp reported. A refused action changed
    /// nothing, so the previous mode must stay on screen.
    #[test]
    fn a_refused_action_leaves_the_previous_mode_showing() {
        let mut state = loaded(DriveMode::CoDrive);
        state.settle_drive(DriveAction::TakeWheel, Err(CompClientError::Comp("busy".to_string())));
        assert_eq!(state.status(), (STATUS_CODRIVE, DotTone::Driving), "the mode must not move on a failure");
        assert_eq!(state.last_failure, Some(DriveAction::TakeWheel));
        assert!(state.button_enabled(), "the person has to be able to try again");
    }

    /// A vocabulary drift between shell and comp is a refusal like any other
    /// as far as the SCREEN is concerned — it gets its own log line, not its
    /// own user-facing state.
    #[test]
    fn an_invalid_action_token_is_still_just_a_failed_action_on_screen() {
        let mut state = loaded(DriveMode::Handover);
        state.settle_drive(
            DriveAction::HandBack,
            Err(CompClientError::Comp(codrive_client::CODRIVE_ERR_INVALID_ACTION.to_string())),
        );
        assert_eq!(state.status(), (STATUS_HANDOVER, DotTone::Paused));
        assert_eq!(state.last_failure, Some(DriveAction::HandBack));
    }

    #[test]
    fn a_successful_action_adopts_comps_new_state_and_clears_the_failure_line() {
        let mut state = loaded(DriveMode::CoDrive);
        state.settle_drive(DriveAction::TakeWheel, Err(CompClientError::Timeout));
        assert!(state.last_failure.is_some());

        state.settle_drive(
            DriveAction::TakeWheel,
            Ok(CodriveState {
                mode: DriveMode::Handover,
                handover_reason: Some("shell_take_wheel".to_string()),
                session_active: true,
                frozen: true,
                ..CodriveState::default()
            }),
        );
        assert_eq!(state.status(), (STATUS_HANDOVER, DotTone::Paused));
        assert_eq!(state.offered_action(), Some(DriveAction::HandBack), "the button flips to 交還 once the wheel is yours");
        assert_eq!(state.last_failure, None);
    }

    #[test]
    fn only_one_compositor_call_may_be_in_flight_and_it_disables_the_button() {
        let mut state = loaded(DriveMode::CoDrive);
        assert!(state.begin());
        assert!(!state.begin());
        assert!(!state.button_enabled(), "a call in flight disables the button…");
        assert_eq!(state.offered_action(), Some(DriveAction::TakeWheel), "…without changing what it would do");
        state.settle_drive(DriveAction::TakeWheel, Ok(CodriveState { mode: DriveMode::Handover, ..CodriveState::default() }));
        assert!(state.begin(), "settling releases the slot");
    }

    #[test]
    fn closing_the_panel_forgets_everything_so_the_next_open_re_reads() {
        let mut state = loaded(DriveMode::CoDrive);
        state.settle_drive(DriveAction::TakeWheel, Err(CompClientError::Timeout));
        state.reset();
        assert_eq!(state, CodriveUiState::default());
        assert_eq!(state.load, CodriveLoad::NotLoaded, "a reset row asks again on its next render");
    }

    /// The action tokens leave this module in exactly one place.
    #[test]
    fn each_action_maps_to_comps_own_spelling() {
        assert_eq!(DriveAction::TakeWheel.wire(), codrive_client::CODRIVE_ACTION_TAKE_WHEEL);
        assert_eq!(DriveAction::HandBack.wire(), codrive_client::CODRIVE_ACTION_HAND_BACK);
    }

    /// The rule from this file's header comment, enforced rather than merely
    /// stated: nothing a person reads may leak the wire vocabulary, an
    /// internal state name, or a file path. This is the test that fails when
    /// someone "helpfully" pastes a mode token into a message.
    #[test]
    fn no_user_facing_string_leaks_internal_vocabulary() {
        const USER_FACING: [&str; 11] = [
            ROW_LABEL,
            STATUS_HUMAN,
            STATUS_CODRIVE,
            STATUS_HANDOVER,
            STATUS_LOADING,
            STATUS_UNSUPPORTED,
            STATUS_UNAVAILABLE,
            STATUS_UNKNOWN_MODE,
            BUTTON_TAKE_WHEEL,
            BUTTON_HAND_BACK,
            FAILED_LINE,
        ];
        // `HINT_HAND_BACK` is deliberately NOT in the list above: it names a
        // real KEY the person presses (Super+Enter), which is user-facing
        // vocabulary, not internal vocabulary.
        const FORBIDDEN: [&str; 9] =
            ["codrive", "co-drive", "handover", "take_wheel", "hand_back", "human_input", "shell_", "comp", ".rs"];
        for text in USER_FACING {
            let lowered = text.to_ascii_lowercase();
            for token in FORBIDDEN {
                assert!(!lowered.contains(token), "user-facing copy {text:?} leaks the internal token {token:?}");
            }
        }
    }

    /// Every sentence a person can be shown has to actually be a sentence —
    /// an empty string would render as a blank line that looks like a bug.
    #[test]
    fn every_status_sentence_is_non_empty() {
        for state in [
            CodriveUiState::default(),
            loaded(DriveMode::Human),
            loaded(DriveMode::CoDrive),
            loaded(DriveMode::Handover),
            loaded(DriveMode::Unknown(String::new())),
            CodriveUiState { load: CodriveLoad::Unsupported, ..CodriveUiState::default() },
            CodriveUiState { load: CodriveLoad::Unavailable, ..CodriveUiState::default() },
        ] {
            let (text, _) = state.status();
            assert!(!text.trim().is_empty(), "a blank status line reads as a bug, not as a state");
        }
    }
}
