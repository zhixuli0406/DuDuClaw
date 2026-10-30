//! Read-only full screen. No CLI parsing, credentials, approvals or subprocesses.
pub mod app;
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
        let feed = StateFeed::start(options.client, settings.poll_interval);
        let home_text = TerminalText::new(home.to_string_lossy().into_owned());
        let shutdown = terminal::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            if terminal::panicked() {
                return Err(Error::Terminal);
            }
            let model = feed.receiver.borrow().clone();
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
                        &app,
                        &settings,
                    )
                })
                .map_err(|_| Error::Terminal)?;
            // Never block a Tokio worker waiting for user input; only read ready events.
            if event::poll(Duration::ZERO).map_err(|_| Error::Terminal)?
                && let Event::Key(key) = event::read().map_err(|_| Error::Terminal)?
            {
                let before = app.read.clone();
                match app.key(key, &model) {
                    Action::Quit => return Ok(()),
                    Action::Save => match app.read.save(&home) {
                        Ok(()) => app.notice = Some(strings::SAVED.into()),
                        Err(error) => {
                            app.read = before;
                            app.notice = Some(error.to_string());
                        }
                    },
                    Action::None => {}
                }
            }
            tokio::select! {
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
