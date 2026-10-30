//! Full screen with explicit, locally guarded decisions. No subprocesses.
pub mod app;
pub mod card;
mod card_data;
mod card_render;
#[cfg(test)]
mod card_tests;
mod backend;
mod panels;
pub mod render;
mod terminal;
#[cfg(test)]
mod tests;
mod text;
mod widgets;

use crate::{
    Error, Result,
    client::Client,
    read_state::ReadState,
    sanitize::TerminalText,
    settings::{Settings, resolve_home},
    state::StateFeed,
    strings,
};
use app::{Action, App};
use crossterm::event::{self, Event};
use futures_util::FutureExt;
use ratatui::Terminal;
use std::{path::PathBuf, time::Duration};

pub struct ControlRoomOptions {
    /// Phase 3 handles AI-session refusal, core-dump protection and login first.
    pub client: Client,
    pub home: Option<PathBuf>,
}
/// Returns LoginRequired to the phase-3 login loop. Dropping this future cancels
/// its feed and restores the terminal. Employees continue running in the gateway.
pub async fn run_control_room(options: ControlRoomOptions) -> Result<()> {
    let home = resolve_home(options.home)?;
    let (settings, settings_notice) = Settings::load(&home);
    let (read, read_notice) = ReadState::load(&home);
    let mut app = App::default();
    app.read = read;
    let notices: Vec<_> = [settings_notice, read_notice]
        .into_iter()
        .flatten()
        .collect();
    if !notices.is_empty() {
        app.notice = Some(notices.join(" / "));
    }
    let guard = terminal::TerminalGuard::enter()?;
    let result = std::panic::AssertUnwindSafe(async {
        let mut terminal = Terminal::new(backend::ScreenBackend(std::io::stdout()))
            .map_err(|_| Error::Terminal)?;
        terminal.clear().map_err(|_| Error::Terminal)?;
        let connection = options.client.connection();
        let client = options.client.clone();
        let feed = StateFeed::start(options.client, settings.poll_interval);
        let mut decision: Option<futures_util::future::BoxFuture<'_, crate::decide::Outcome>> = None;
        let home_text = TerminalText::new(home.to_string_lossy().into_owned());
        let shutdown = terminal::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            if terminal::panicked() {
                return Err(Error::Terminal);
            }
            let mut model = feed.receiver.borrow().clone();
            model.inbox.items.retain(|item| !app.read.hidden(item, crate::snooze::epoch_now()));
            if let Some(error) = model.login_required {
                return Err(error);
            }
            app.refresh(&model);
            terminal
                .draw(|frame| {
                    render::draw(
                        frame,
                        &model,
                        &connection.borrow(),
                        &home_text,
                        &mut app,
                        &settings,
                    )
                })
                .map_err(|_| Error::Terminal)?;
            // Never block a Tokio worker waiting for user input; only read ready events.
            while event::poll(Duration::ZERO).map_err(|_| Error::Terminal)? {
                let key = match event::read().map_err(|_| Error::Terminal)? {
                    Event::Resize(_, _) => {
                        if let Some(card) = &mut app.card { card.invalidate_viewport(); }
                        // Stop this batch: queued keys must see the newly rendered viewport.
                        break;
                    }
                    Event::Key(key) => key,
                    _ => continue,
                };
                let before = app.read.clone();
                match app.key(key, &model) {
                    Action::Quit => return Ok(()),
                    action @ (Action::Save | Action::Snooze) => match app.read.save(&home) {
                        Ok(()) => app.notice = Some(if action == Action::Snooze { strings::SNOOZED } else { strings::SAVED }.into()),
                        Err(error) => {
                            app.read = before;
                            app.notice = Some(error.to_string());
                        }
                    },
                    Action::Decide(request) => {
                        let client = client.clone();
                        decision = Some(Box::pin(async move { crate::decide::send(&client, &request).await }));
                    }
                    Action::None => {}
                }
            }
            tokio::select! {
                outcome = async { decision.as_mut().expect("guarded decision").await }, if decision.is_some() => {
                    decision = None;
                    app.decision_finished(outcome);
                    feed.refetch();
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
                result = &mut shutdown => return result,
            }
        }
    })
    .catch_unwind()
    .await;
    drop(guard);
    result.unwrap_or(Err(Error::Terminal))
}
