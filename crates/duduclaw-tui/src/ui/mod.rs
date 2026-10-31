//! Full screen with guarded gateway decisions and owner-local PTY blocks.
pub mod app;
pub mod card;
mod card_data;
mod control_card;
mod block_render;
mod pty_render;
mod m2;
mod palette;
mod exit_summary;
mod card_render;
#[cfg(test)]
mod card_tests;
mod backend;
mod panels;
mod chat_render;
pub mod render;
pub(crate) mod terminal;
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

type BlockBatch = (u64, Vec<(usize, crate::Result<crate::blocks::Data>)>);
type BlockFetch = futures_util::future::BoxFuture<'static, BlockBatch>;

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
    app.blocks = settings.default_layout.blocks();
    let notices: Vec<_> = [settings_notice, read_notice]
        .into_iter()
        .flatten()
        .collect();
    if !notices.is_empty() {
        app.notice = Some(notices.join(" / "));
    }
    let guard = terminal::TerminalGuard::enter()?;
    let shutdown_client = options.client.clone();
    let mut exit_text = None;
    let result = std::panic::AssertUnwindSafe(async {
        let mut terminal = Terminal::new(backend::ScreenBackend(std::io::stdout()))
            .map_err(|_| Error::Terminal)?;
        terminal.clear().map_err(|_| Error::Terminal)?;
        let connection = options.client.connection();
        let client = options.client.clone();
        let chat = client.chat();
        let mut chat_state = chat.state();
        app.chat = chat_state.borrow_and_update().clone();
        let route = if settings.notifications { crate::notifications::route().await } else { crate::notifications::Route::Skip };
        let mut notifications = crate::notifications::Notifications::new(settings.notifications, route);
        let started = std::time::Instant::now();
        let mut block_events = client.events();
        let mut block_cursor = crate::blocks::EventCursor::default();
        let mut block_fetch: Option<BlockFetch> = None;
        let mut fetched_revision = u64::MAX;
        let mut block_poll = std::time::Instant::now();
        let mut block_generation = None;
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
            let info = connection.borrow().clone();
            let generation = match info { crate::client::ConnectionState::Connected(ref info) => Some(info.generation), _ => None };
            if generation != block_generation {
                block_generation = generation; app.blocks.invalidate(); app.logs = crate::blocks::LogTail::default();
                block_cursor = crate::blocks::EventCursor::default();
                for pane in &mut app.blocks.panes { pane.data.subscription_checked = false; pane.data.subscribed = false; }
            }
            if !m2::manager(&model) { app.logs = crate::blocks::LogTail::default(); }
            for _ in 0..256 {
                let event = match block_events.try_recv() {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => { app.blocks.invalidate(); app.logs.lost = true; continue; },
                    _ => break,
                };
                if block_cursor.observe(&event) { app.blocks.invalidate(); app.logs.lost = true; }
                match event.name.as_str() {
                    "logs.entry" if m2::manager(&model) && app.blocks.panes.iter().any(|p| p.kind == crate::blocks::Block::Log) => app.logs.push(&event.payload),
                    "events_lost" => { app.blocks.invalidate(); app.logs.lost = true; },
                    "run.step" => app.blocks.run_step(&event.payload),
                    "agent.turn_finished" | "agent.status_changed" | "task.updated" | "task.created" => app.blocks.invalidate(),
                    _ => {},
                }
            }
            if generation.is_some() && decision.is_none() && block_fetch.is_none()
                && (fetched_revision != app.blocks.revision || block_poll.elapsed() >= Duration::from_secs(5)) {
                let revision = app.blocks.revision;
                let panes = app.blocks.panes.clone(); let gateway = client.clone(); let manager = m2::manager(&model);
                block_fetch = Some(Box::pin(async move {
                    let mut results = Vec::new();
                    for (i, pane) in panes.iter().enumerate() {
                        if matches!(pane.kind, crate::blocks::Block::Run | crate::blocks::Block::Task | crate::blocks::Block::Log) {
                            results.push((i, crate::blocks::fetch::pane(&gateway, pane, manager).await));
                        }
                    }
                    (revision, results)
                }));
                fetched_revision = revision; block_poll = std::time::Instant::now();
            }
            for pane in &app.blocks.panes {
                if let Some(pty) = &pane.pty {
                    let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
                    pty.start();
                    pty.poll();
                }
            }
            app.refresh(&model);
            if chat_state.has_changed().unwrap_or(false) {
                app.chat = chat_state.borrow_and_update().clone();
            }
            app.spinner = (started.elapsed().as_millis() / 150) as usize;
            if let Some(sequence) = notifications.update(model.inbox.count(), std::time::Instant::now()) {
                use std::io::Write;
                std::io::stdout().write_all(sequence.as_bytes()).map_err(|_| Error::Output)?;
                std::io::stdout().flush().map_err(|_| Error::Output)?;
            }
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
            // Bound each batch for redraw/shutdown fairness. Always read host events,
            // including escape, quit and resize, even when child input is full.
            for _ in 0..256 {
                let Some(event) = read_host_event(&mut app, || {
                    if !event::poll(Duration::ZERO).map_err(|_| Error::Terminal)? { return Ok(None); }
                    event::read().map(Some).map_err(|_| Error::Terminal)
                })? else { break; };
                let key = match event {
                    Event::Resize(_, _) => {
                        if let Some(card) = &mut app.card { card.invalidate_viewport(); }
                        if let Some(card) = &mut app.control_card { card.resize(); }
                        // Stop this batch: queued keys must see the newly rendered viewport.
                        break;
                    }
                    Event::FocusGained => { notifications.focus(true); continue; }
                    Event::FocusLost => { notifications.focus(false); continue; }
                    Event::Paste(text) => { app.paste(&text); continue; }
                    Event::Key(key) => key,
                    _ => continue,
                };
                let before = app.read.clone();
                match app.key(key, &model) {
                    Action::Quit => {
                        let mut final_model = feed.receiver.borrow().clone();
                        if !matches!(*connection.borrow(), crate::client::ConnectionState::Connected(_)) {
                            final_model.inbox.stale = true; final_model.employees.stale = true;
                        }
                        exit_text = Some(exit_summary::summary(&final_model, &home));
                        return Ok(());
                    },
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
                    Action::Control(request) => {
                        let client = client.clone();
                        decision = Some(Box::pin(async move { crate::controls::send(&client, &request).await }));
                    }
                    Action::Chat(input) => match chat.send(input) {
                        Ok(()) => { app.input.clear(); app.chat_scroll = 0; app.notice = None; app.chat.running = true; }
                        Err(error) => app.notice = Some(error.to_string()),
                    },
                    Action::CancelChat => if let Err(error) = chat.cancel() { app.notice = Some(error.to_string()); },
                    Action::None => {}
                }
            }
            for pane in &app.blocks.panes {
                if let Some(pty) = &pane.pty {
                    pty.lock().unwrap_or_else(|e| e.into_inner()).flush_input();
                }
            }
            tokio::select! {
                outcome = async { decision.as_mut().expect("guarded decision").await }, if decision.is_some() => {
                    decision = None;
                    app.decision_finished(outcome);
                    app.blocks.invalidate();
                    feed.refetch();
                }
                (revision, results) = async { block_fetch.as_mut().expect("guarded fetch").await }, if block_fetch.is_some() => {
                    block_fetch = None;
                    if revision == app.blocks.revision && block_generation.is_some() {
                        for (i, result) in results {
                            if let Some(pane) = app.blocks.panes.get_mut(i) {
                                match result {
                                    Ok(data) => pane.data = data,
                                    Err(error) => { pane.data = crate::blocks::Data::default(); pane.data.error = Some(TerminalText::new(error.to_string())); }
                                }
                            }
                        }
                    }
                }
                _ = tokio::time::sleep(Duration::from_millis(50)) => {},
                result = &mut shutdown => {
                    let mut final_model = feed.receiver.borrow().clone();
                    if !matches!(*connection.borrow(), crate::client::ConnectionState::Connected(_)) {
                        final_model.inbox.stale = true; final_model.employees.stale = true;
                    }
                    exit_text = Some(exit_summary::summary(&final_model, &home));
                    return result;
                },
            }
        }
    })
    .catch_unwind()
    .await;
    for pane in &app.blocks.panes {
        if let Some(pty) = &pane.pty {
            let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
            pty.stop();
            if !pty.cleanup_complete {
                exit_text.get_or_insert_with(String::new).push_str(&format!("\n{}\n", strings::PTY_STOP_FAILED));
            }
        }
    }
    drop(guard);
    shutdown_client.shutdown().await;
    let result = result.unwrap_or(Err(Error::Terminal));
    if result.is_ok() && let Some(text) = exit_text {
        use std::io::Write;
        let mut output = std::io::stdout().lock();
        output.write_all(text.as_bytes()).map_err(|_| Error::Output)?;
        output.flush().map_err(|_| Error::Output)?;
    }
    result
}

/// Host events are independent of child writer capacity. Keep this path shared
/// with the stalled-child regression test so control keys cannot be gated again.
fn read_host_event(app: &mut App, read: impl FnOnce() -> Result<Option<Event>>) -> Result<Option<Event>> {
    let event = read()?;
    if event.is_some() && app.pty_focus
        && let Some(pty) = &app.blocks.panes[app.blocks.focused].pty
    {
        pty.lock().unwrap_or_else(|e| e.into_inner()).begin_input_batch();
    }
    Ok(event)
}
