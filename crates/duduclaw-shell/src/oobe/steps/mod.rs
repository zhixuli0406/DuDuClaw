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

use gpui::{Context, Div};

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
