// Per-step content dispatcher — Shell-S1.
//
// `render.rs`'s frame owns the chrome (background, progress dots, bottom
// button row); each module below owns only the middle "內容區" for one
// step — title/subtitle/body, per the task brief's shared step-template
// wording. `input_detection` and `update` take no interactive state at all
// (static content only); every other step is a real `cx.listener`-backed
// screen as of round 2 (round 1 shipped `runtime_auth`/`privacy`/
// `templates`/`finish` as one shared honest-placeholder page — see git
// history for that file, since removed).

mod account;
mod finish;
mod input_detection;
mod language;
mod network;
mod privacy;
mod runtime_auth;
mod templates;
mod theme;
mod update;

/// ICON-3 (2026-08-23): the language step's five accessibility categories.
/// Re-exported up through `oobe` (see `oobe/mod.rs`'s own `pub(crate) use`)
/// purely so `crate::icons`' slot-mapping tests can iterate them the same
/// way they iterate `PrivacyToggle::ALL` — nothing outside this step
/// RENDERS them. Same shape `network_ui`'s own enums already establish for
/// their re-export.
pub(crate) use language::A11yCategory;

use gpui::{Context, Div};

use super::state::EnterOutcome;
use super::widgets::{AccountFields, NetworkFields};
use super::{OobeFlow, OobeStep, OobeUiState};
use crate::ShellView;

pub(super) fn render(
    step: OobeStep,
    flow: &OobeFlow,
    ui: &OobeUiState,
    account_fields: &AccountFields,
    network_fields: &NetworkFields,
    cx: &mut Context<ShellView>,
) -> Div {
    match step {
        // `input_detection`/`update` now take the whole `&OobeFlow` (not
        // just `flow.locale()`, round 2's shape) — as of the `Theme` step
        // (2026-08-20) both also need `flow.palette()` for their own
        // `widgets::title`/`subtitle`/`card` calls, so they compute BOTH
        // `locale`/`palette` internally, matching the "only take what you
        // need, but take it FROM `flow`" shape every other arm already has.
        OobeStep::InputDetection => input_detection::render(flow),
        OobeStep::LanguageAccessibility => language::render(flow, ui, cx),
        OobeStep::Network => network::render(flow, ui, network_fields, cx),
        OobeStep::Update => update::render(flow),
        OobeStep::AccountCreate => account::render(flow, ui, account_fields, cx),
        OobeStep::RuntimeAuth => runtime_auth::render(flow, cx),
        OobeStep::Privacy => privacy::render(flow, cx),
        OobeStep::Templates => templates::render(flow, cx),
        OobeStep::Theme => theme::render(flow, cx),
        OobeStep::Finish => finish::render(flow),
    }
}

/// Routes an `EnterOutcome::Submit*` decision (see `OobeFlow::enter_
/// outcome`'s own doc comment in `state.rs`) to whichever step actually
/// owns that submit action — `main.rs`'s `on_oobe_next` (Enter's handler)
/// is the one caller, re-exported as `oobe::handle_enter_submit`. `Advance`/
/// `Blocked` never reach here (the caller handles `Advance` itself via
/// `OobeFlow::next_with_wired`, and `Blocked` is a plain no-op) — the match
/// stays exhaustive anyway so a THIRD submit-shaped step added later can't
/// silently fall through unhandled.
// `pub(crate)`, not `pub(super)`: `oobe/mod.rs` re-exports this as
// `oobe::handle_enter_submit` for `main.rs`'s Enter handler (exactly what
// this fn's own doc comment above says), and a `pub(super)` item cannot be
// re-exported that widely — E0364. Corrected 2026-08-23 while wiring D4b,
// which could not compile the crate until this resolved.
pub(crate) fn handle_enter_submit(outcome: EnterOutcome, view: &mut ShellView, cx: &mut Context<ShellView>) {
    match outcome {
        EnterOutcome::SubmitAccount => account::try_submit(view, cx),
        EnterOutcome::SubmitNetworkConnect => network::try_submit(view, cx),
        EnterOutcome::Advance | EnterOutcome::Blocked => {}
    }
}
