//! First-run seed of the operator's default AI employee.
//!
//! 2026-09-05 (QEMU feature walkthrough): a freshly installed DuDuClaw OS
//! had ZERO agents — `agents.list` came back empty — so the Home composer's
//! 交辦 (`overlay::launcher::try_submit_delegate`) could never find anyone
//! to hand a task to, and the OOBE Templates step's "套用 Express" only
//! wrote a local flag (`OobeFlow::set_template_choice`), never a gateway
//! call. The CLI's own `duduclaw init` wizard creates the first agent as
//! part of setup (`duduclaw-cli/src/wizard.rs`); the desktop OOBE replaced
//! that wizard without porting the one step that makes the product usable.
//!
//! This module closes that: when OOBE completes (`ShellView::complete_oobe`,
//! every completion path), spawn one background round trip — list agents,
//! and if there are none, `agents.create` a `role: "main"` assistant named
//! in the operator's OOBE language. Idempotent by construction (an existing
//! roster is left alone), fail-open for the OOBE flow itself (a gateway
//! that is not up yet costs one notification card, never a stuck wizard),
//! and every outcome is a real card on the notification centre, never a
//! silent success or a silent failure.
use std::sync::mpsc;
use std::time::Duration;

use gpui::Context;

use crate::gateway_client::{self, GatewayError};
use crate::i18n::{t, t1, Key, Locale};
use crate::ShellView;

/// `overlay/notifications.rs::POLL_INTERVAL`'s exact value — the "check
/// the mpsc channel" tick for the thread → gpui bridge below.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Stable agent id of the seeded assistant (`is_valid_agent_id`: lowercase
/// alphanumeric + hyphens). The display name is localized; this is not.
pub(crate) const DEFAULT_AGENT_ID: &str = "assistant";

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SeedOutcome {
    /// The roster was empty and the default assistant was created.
    Created(String),
    /// Agents already existed — nothing to do, nothing to announce.
    AlreadyStaffed(usize),
    /// The gateway could not be reached or refused the call.
    Failed(String),
}

/// One synchronous round trip: `agents.list`, then `agents.create` only
/// when the list is empty. Pure gateway I/O — no gpui — so it is unit-
/// testable against the live-test harness and reusable outside OOBE.
pub(crate) fn ensure_default_agent(jwt: &str, display_name: &str, trigger: &str) -> Result<SeedOutcome, GatewayError> {
    let agents = gateway_client::list_agents(jwt)?;
    if !agents.is_empty() {
        return Ok(SeedOutcome::AlreadyStaffed(agents.len()));
    }
    gateway_client::create_agent(jwt, DEFAULT_AGENT_ID, display_name, "main", trigger)?;
    Ok(SeedOutcome::Created(display_name.to_string()))
}

/// Fire the seed from the render thread: worker thread does the I/O, the
/// gpui side polls the channel and turns the outcome into a card. Same
/// thread + mpsc + `cx.spawn` bridge `overlay/notifications_tasks.rs`
/// uses, reused verbatim rather than inventing a fourth shape.
pub(crate) fn spawn_seed_default_agent(view: &mut ShellView, locale: Locale, cx: &mut Context<ShellView>) {
    let display_name = t(locale, Key::OobeDefaultAgentName).to_string();
    let trigger = t(locale, Key::OobeDefaultAgentTrigger).to_string();
    let existing_jwt = view.overlay_ui.notifications.session_jwt().map(str::to_string);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let run = || -> Result<SeedOutcome, GatewayError> {
            let jwt = match existing_jwt {
                Some(jwt) => jwt,
                None => gateway_client::bootstrap_local_session()?,
            };
            ensure_default_agent(&jwt, &display_name, &trigger)
        };
        let outcome = run().unwrap_or_else(|e| SeedOutcome::Failed(format!("{e:?}")));
        let _ = tx.send(outcome);
    });
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok(outcome) => {
                let _ = weak.update(cx, |view, cx| {
                    apply_seed_outcome(view, locale, outcome);
                    cx.notify();
                });
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(POLL_INTERVAL).await;
    })
    .detach();
}

fn apply_seed_outcome(view: &mut ShellView, locale: Locale, outcome: SeedOutcome) {
    match outcome {
        SeedOutcome::Created(name) => {
            post(view, t(locale, Key::OobeSeedAgentCreatedTitle), &t1(locale, Key::OobeSeedAgentCreatedBody, &name));
        }
        SeedOutcome::AlreadyStaffed(_) => {}
        SeedOutcome::Failed(err) => {
            // Always logged, not DIAG-gated: this happens once per OOBE and
            // is the only trace an unreachable gateway leaves besides the card.
            eprintln!("[oobe/seed] default agent seed failed: {err}");
            post(view, t(locale, Key::OobeSeedAgentFailedTitle), t(locale, Key::OobeSeedAgentFailedBody));
        }
    }
}

fn post(view: &mut ShellView, title: &str, body: &str) {
    view.notify_center.post_system(crate::task_result::NOTIFY_APP_NAME, title, body, crate::notifyd::Urgency::Normal, Vec::new(), None);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seeded_assistant_has_a_valid_stable_agent_id() {
        assert!(!DEFAULT_AGENT_ID.is_empty());
        assert!(DEFAULT_AGENT_ID.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'));
        assert!(!DEFAULT_AGENT_ID.starts_with('-') && !DEFAULT_AGENT_ID.ends_with('-'));
    }

    #[test]
    fn every_locale_names_the_default_assistant() {
        for locale in [Locale::ZhTw, Locale::En, Locale::JaJp] {
            assert!(!t(locale, Key::OobeDefaultAgentName).trim().is_empty());
            assert!(!t(locale, Key::OobeDefaultAgentTrigger).trim().is_empty());
        }
    }
}
