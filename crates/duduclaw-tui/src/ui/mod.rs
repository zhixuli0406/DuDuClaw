//! Full screen with guarded gateway decisions and owner-local PTY blocks.
pub mod app;
pub mod card;
mod card_data;
mod control_card;
mod block_render;
mod pty_render;
mod m2;
mod navigate;
mod notices;
pub(crate) mod timeline;
mod palette;
mod exit_summary;
mod card_render;
#[cfg(test)]
mod card_tests;
mod backend;
mod osc11;
mod panels;
mod shell;
mod workspace;
mod chat_render;
mod inline_decision;
mod motion;
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
use crossterm::event::Event;
use futures_util::{FutureExt, StreamExt};
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
    let (mut settings, settings_notice) = Settings::load(&home);
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
    // Negotiate only after raw mode, using the same event reader as the loop.
    let mut reply_filter = if (matches!(settings.theme, crate::theme::ThemeMode::Auto | crate::theme::ThemeMode::Ansi)
        || settings.background == crate::theme::Background::Terminal)
        && settings.colors.capability != crate::theme::Capability::NoColor {
        let query = osc11::query_background();
        settings.resolve_theme(query.background);
        query.filter
    } else {
        settings.resolve_theme(None);
        osc11::ReplyFilter::default()
    };
    let shutdown_client = options.client.clone();
    let mut exit_text = None;
    let result = std::panic::AssertUnwindSafe(async {
        let mut terminal = Terminal::new(backend::ScreenBackend(std::io::stdout()))
            .map_err(|_| Error::Terminal)?;
        terminal.clear().map_err(|_| Error::Terminal)?;
        let mut connection = options.client.connection();
        let client = options.client.clone();
        let chat = client.chat();
        let mut chat_state = chat.state();
        app.sync_chats(&chat_state.borrow_and_update());
        let mut chat_revision = app.blocks.revision;
        let route = if settings.notifications { crate::notifications::route().await } else { crate::notifications::Route::Skip };
        let mut notifications = crate::notifications::Notifications::new(settings.notifications, route);
        let mut dirty = true;
        let mut motion = motion::Motion::default();
        let mut timers = motion::Timers::default();
        let mut block_events = client.events();
        let mut block_cursor = crate::blocks::EventCursor::default();
        let mut block_fetch: Option<BlockFetch> = None;
        let mut fetched_revision = u64::MAX;
        let mut block_poll = std::time::Instant::now();
        let mut block_generation = None;
        let mut feed = StateFeed::start(options.client, settings.poll_interval);
        let mut decision: Option<futures_util::future::BoxFuture<'_, crate::decide::Outcome>> = None;
        let mut autonomy_requests = futures_util::stream::FuturesUnordered::<futures_util::future::BoxFuture<'static, crate::decide::Outcome>>::new();
        let home_text = TerminalText::new(home.to_string_lossy().into_owned());
        let shutdown = terminal::shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            if terminal::panicked() {
                return Err(Error::Terminal);
            }
            let mut model = if feed.receiver.has_changed().unwrap_or(false) {
                dirty = true; feed.receiver.borrow_and_update().clone()
            } else { feed.receiver.borrow().clone() };
            model.inbox.items.retain(|item| !app.read.hidden(item, crate::snooze::epoch_now()));
            let info = if connection.has_changed().unwrap_or(false) {
                dirty = true; connection.borrow_and_update().clone()
            } else { connection.borrow().clone() };
            let generation = match info { crate::client::ConnectionState::Connected(ref info) => Some(info.generation), _ => None };
            if generation != block_generation {
                block_generation = generation; app.blocks.invalidate(); app.logs = crate::blocks::LogTail::default();
                block_cursor = crate::blocks::EventCursor::default();
                for pane in app.blocks.panes.iter_mut().chain(&mut app.blocks.parked) { pane.data.subscription_checked = false; pane.data.subscribed = false; }
            }
            if !m2::manager(&model) { app.logs = crate::blocks::LogTail::default(); }
            for _ in 0..256 {
                let event = match block_events.try_recv() {
                    Ok(e) => e,
                    Err(tokio::sync::broadcast::error::TryRecvError::Lagged(_)) => { dirty = true; app.blocks.invalidate(); app.logs.lost = true; continue; },
                    _ => break,
                };
                dirty = true;
                if block_cursor.observe(&event) { app.blocks.invalidate(); app.logs.lost = true; }
                app.notices.event(&event, &model, std::time::Instant::now());
                match event.name.as_str() {
                    "logs.entry" if m2::manager(&model) && app.blocks.panes.iter().any(|p| p.kind == crate::blocks::Block::Log) => app.logs.push(&event.payload),
                    "events_lost" => { app.blocks.invalidate(); app.logs.lost = true; },
                    "dashboard.navigate" => app.navigate_hint(&event.payload, &model),
                    "run.step" => app.blocks.run_step(&event.payload),
                    "agent.turn_finished" | "agent.status_changed" | "task.updated" | "task.created" | "budget.breaker_open" | "autopilot.breaker_open" => app.blocks.invalidate(),
                    _ => {},
                }
            }
            if generation.is_some() && decision.is_none() && block_fetch.is_none()
                && (fetched_revision != app.blocks.revision || block_poll.elapsed() >= Duration::from_secs(5)) {
                let revision = app.blocks.revision;
                let panes = app.blocks.panes.clone(); let gateway = client.clone(); let role = model.role;
                block_fetch = Some(Box::pin(async move {
                    let mut results = Vec::new();
                    for (i, pane) in panes.iter().enumerate() {
                        if pane.kind.remote() {
                            results.push((i, crate::blocks::fetch::pane_for_role(&gateway, pane, role).await));
                        }
                    }
                    (revision, results)
                }));
                fetched_revision = revision; block_poll = std::time::Instant::now();
            }
            for pane in &app.blocks.panes {
                if let Some(pty) = &pane.pty {
                    let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
                    let previous_status = pty.status.clone();
                    pty.start();
                    dirty |= pty.poll() || previous_status != pty.status;
                }
            }
            app.refresh(&model);
            app.observe_notices(&model, std::time::Instant::now());
            chat.retain(app.blocks.chat_ids());
            dirty |= sync_chat_state(&mut app, &mut chat_state, &mut chat_revision);
            let animating = motion::running(&app);
            dirty |= motion.tick(std::time::Instant::now(), &settings, animating, &mut app.spinner);
            dirty |= timers.tick(std::time::Instant::now(), &app);
            let now = std::time::Instant::now();
            let failed = model.login_required.is_some() || matches!(info, crate::client::ConnectionState::Disconnected);
            let sequence = notifications.connection_error(failed, now)
                .or_else(|| notifications.update(model.inbox.actionable_count(), now));
            if let Some(sequence) = sequence {
                use std::io::Write;
                std::io::stdout().write_all(sequence.as_bytes()).map_err(|_| Error::Output)?;
                std::io::stdout().flush().map_err(|_| Error::Output)?;
            }
            if let Some(error) = model.login_required { return Err(error); }
            if dirty {
                dirty = false;
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
            }
            // Never block a Tokio worker waiting for user input; only read ready events.
            // Bound each batch for redraw/shutdown fairness. Always read host events,
            // including escape, quit and resize, even when child input is full.
            for _ in 0..256 {
                let Some(event) = read_host_event(&mut app, || {
                    reply_filter.read().map_err(|_| Error::Terminal)
                })? else { break; };
                dirty = true;
                let key = match event {
                    Event::Resize(_, _) => {
                        if let Some(card) = &mut app.card { card.invalidate_viewport(); }
                        if let Some(card) = &mut app.control_card { card.resize(); }
                        // Stop this batch: queued keys must see the newly rendered viewport.
                        break;
                    }
                    Event::FocusGained => { notifications.focus(true); continue; }
                    Event::FocusLost => { notifications.focus(false); continue; }
                    Event::Mouse(mouse) => { app.mouse(mouse, &settings); continue; }
                    Event::Paste(text) => { app.paste(&text); continue; }
                    Event::Key(key) => key,
                };
                let before = app.read.clone();
                let previous_pane = app.blocks.panes[app.blocks.focused].id.clone();
                let action = app.key(key, &model);
                if app.blocks.panes[app.blocks.focused].id != previous_pane { app.sync_chats(&chat_state.borrow()); }
                match action {
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
                        if matches!(request, crate::controls::Control::Autonomy { .. }) {
                            // Poll once now: Client::call_detailed enqueues before its first await.
                            // Later keys cannot replace an unsent request, and replies stay independent.
                            let mut pending = async move { crate::controls::send(&client, &request).await }.boxed();
                            if let Some(outcome) = pending.as_mut().now_or_never() {
                                dirty = true;
                                app.notice = Some(outcome.notice());
                                feed.refetch();
                            } else {
                                autonomy_requests.push(pending);
                            }
                        } else {
                            decision = Some(Box::pin(async move { crate::controls::send(&client, &request).await }));
                        }
                    }
                    Action::Chat(input) => {
                        let agent = input.agent.clone();
                        let active = app.blocks.chat_ids();
                        match chat.send(&app.blocks.panes[app.blocks.focused].id, input, active) {
                            Ok(()) => {
                                app.input.clear(); app.chat_scroll = 0; app.notice = None; app.chat.running = true;
                                app.input_agent = Some(agent.clone());
                                app.blocks.panes[app.blocks.focused].agent = Some(agent);
                            }
                            Err(error) => app.notice = Some(error.to_string()),
                        }
                    },
                    Action::CancelChat => if let Err(error) = chat.cancel(&app.blocks.panes[app.blocks.focused].id) { app.notice = Some(error.to_string()); },
                    Action::CancelDownstream(reference) => if let Err(error) = chat.cancel_downstream(&app.blocks.panes[app.blocks.focused].id, reference) {
                        app.answered_downstream = None;
                        app.notice = Some(error.to_string());
                    },
                    Action::DismissDownstream(reference) => chat.dismiss_downstream(&app.blocks.panes[app.blocks.focused].id, reference),
                    Action::None => {}
                }
            }
            for pane in &app.blocks.panes {
                if let Some(pty) = &pane.pty {
                    pty.lock().unwrap_or_else(|e| e.into_inner()).flush_input();
                }
            }
            tokio::select! {
                Some(outcome) = autonomy_requests.next(), if !autonomy_requests.is_empty() => {
                    dirty = true;
                    app.notice = Some(outcome.notice());
                    feed.refetch();
                }
                outcome = async { decision.as_mut().expect("guarded decision").await }, if decision.is_some() => {
                    dirty = true;
                    decision = None;
                    app.decision_finished(outcome);
                    app.blocks.invalidate();
                    feed.refetch();
                }
                (revision, results) = async { block_fetch.as_mut().expect("guarded fetch").await }, if block_fetch.is_some() => {
                    dirty = true;
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
                _ = tokio::time::sleep(Duration::from_millis(if animating { 16 } else { 50 })) => {},
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

/// Synchronize the chat projection after model-driven workspace changes.
fn sync_chat_state(app: &mut App, state: &mut tokio::sync::watch::Receiver<crate::chat::ChatRooms>, revision: &mut u64) -> bool {
    // A handshake may already be consumed while the layout still contains only
    // Detail panes. Re-project the latest value when refresh/navigation creates
    // or restores panes, even if the server sends no further chat frames.
    let changed = state.has_changed().unwrap_or(false) || *revision != app.blocks.revision;
    if changed { app.sync_chats(&state.borrow_and_update()); }
    *revision = app.blocks.revision;
    changed
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

#[cfg(test)]
mod autonomy_tests;
