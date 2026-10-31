//! Presentation-only commands dispatch through the ordinary key handler.
use super::{
    app::{Action, App, Selection},
    m2,
};
use crate::{
    blocks::Block, chat::Focus, sanitize::TerminalText, settings::Settings, state::Model, strings,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Frame, layout::Rect, widgets::Clear};
use std::time::Instant;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Default, Clone)]
pub(super) struct Palette {
    pub query: String,
    pub selected: usize,
}
impl Palette {
    pub fn paste(&mut self, text: &str) {
        let remaining = 128usize.saturating_sub(self.query.chars().count());
        self.query.extend(
            text.chars()
                .filter(|c| !crate::sanitize::forbidden(*c))
                .map(|c| if c == '\n' { ' ' } else { c })
                .take(remaining),
        );
        self.selected = 0;
    }
}
#[derive(Clone)]
pub(super) struct Command {
    pub label: &'static str,
    pub key: KeyEvent,
    pub hint: String,
    aliases: &'static str,
}
fn fuzzy(query: &str, text: &str) -> bool {
    let text = text.to_lowercase();
    let mut chars = text.chars();
    query
        .to_lowercase()
        .chars()
        .filter(|c| !c.is_whitespace())
        .all(|c| chars.any(|n| n == c))
}
impl App {
    pub(super) fn commands(&self, model: &Model) -> Vec<Command> {
        if self.card.is_some() || self.control_card.is_some() || self.content_warning {
            return vec![];
        }
        let mut commands = Vec::new();
        let mut add = |code, modifiers, label, hint: &str, aliases| {
            commands.push(Command {
                label,
                key: KeyEvent::new(code, modifiers),
                hint: hint.into(),
                aliases,
            })
        };
        let none = KeyModifiers::NONE;
        add(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
            strings::CMD_QUIT,
            "Ctrl-C",
            "quit exit",
        );
        add(KeyCode::Esc, none, strings::CMD_BACK, "Esc", "back escape");
        if self.focus == Focus::Input {
            if self.chat.connected
                && !self.chat.running
                && crate::chat::submission(&self.input, self.input_agent.as_deref()).is_some()
            {
                add(
                    KeyCode::Enter,
                    none,
                    strings::CMD_SEND,
                    "Enter",
                    "send chat",
                );
            }
            return commands;
        }
        add(KeyCode::F(1), none, strings::CMD_HELP, "F1 / ?", "help");
        add(
            KeyCode::PageUp,
            none,
            strings::CMD_UP,
            "PgUp",
            "page up scroll",
        );
        add(
            KeyCode::PageDown,
            none,
            strings::CMD_DOWN,
            "PgDn",
            "page down scroll",
        );
        if self.help {
            return commands;
        }
        add(KeyCode::Char('P'), none, strings::CMD_PTY, "P", "pty terminal shell");
        add(KeyCode::Char('D'), none, strings::CMD_DETAIL, "D", "detail");
        add(KeyCode::Char('C'), none, strings::CMD_CHAT, "C", "chat");
        add(
            KeyCode::Char('i'),
            none,
            strings::CMD_INPUT,
            "i / /",
            "input chat",
        );
        if self.block_agent(model).is_some() {
            add(KeyCode::Char('R'), none, strings::CMD_RUN, "R", "run");
            add(KeyCode::Char('T'), none, strings::CMD_TASK, "T", "task");
        }
        if m2::manager(model) {
            add(KeyCode::Char('L'), none, strings::CMD_LOG, "L", "log");
        }
        if self.blocks.panes.len() < 4 {
            add(
                KeyCode::Char('v'),
                none,
                strings::CMD_VERTICAL,
                "v",
                "split vertical",
            );
            add(
                KeyCode::Char('x'),
                none,
                strings::CMD_HORIZONTAL,
                "x",
                "split horizontal",
            );
        }
        add(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
            strings::CMD_CLOSE,
            "Ctrl-W",
            "close block",
        );
        for i in 0..self.blocks.panes.len() {
            add(
                KeyCode::Char(char::from(b'1' + i as u8)),
                none,
                strings::CMD_FOCUS,
                &(i + 1).to_string(),
                "focus block",
            );
        }
        let pane = &self.blocks.panes[self.blocks.focused];
        if matches!(pane.kind, Block::Run | Block::Task) {
            if pane.index > 0 {
                add(
                    KeyCode::Char('['),
                    none,
                    strings::CMD_PREVIOUS,
                    "[",
                    "previous record",
                );
            }
            if pane.index + 1 < pane.data.total {
                add(
                    KeyCode::Char(']'),
                    none,
                    strings::CMD_NEXT,
                    "]",
                    "next record",
                );
            }
        }
        if pane.kind == Block::Task {
            add(
                KeyCode::Char('b'),
                none,
                strings::CMD_RESP,
                "b",
                "responsibility task",
            );
        }
        if pane.kind == Block::Run {
            add(
                KeyCode::Char('c'),
                none,
                strings::CMD_CONTENT,
                "c",
                "content preview",
            );
        }
        if pane.kind == Block::Chat && self.chat.can_cancel() {
            add(
                KeyCode::Char('X'),
                none,
                strings::CMD_CANCEL,
                "X",
                "cancel chat",
            );
        }
        let can_open = if pane.kind == Block::Task && !pane.responsibilities {
            pane.data.task().is_some()
        } else {
            self.selected.is_some()
        };
        if can_open {
            add(
                KeyCode::Enter,
                none,
                strings::CMD_OPEN,
                "Enter",
                "open inspect card",
            );
        }
        let can_read = match &self.selected {
            Some(Selection::Inbox(id)) => model.inbox.items.iter().any(|i| i.key() == *id),
            Some(Selection::Employee(id)) => super::app::completion_ids(model, id).next().is_some(),
            _ => false,
        };
        if can_read {
            add(
                KeyCode::Char('r'),
                none,
                strings::CMD_READ,
                "r",
                "read seen",
            );
        }
        if pane.kind != Block::Task
            && let Some(Selection::Inbox(id)) = &self.selected
            && model.inbox.items.iter().any(|i| {
                i.key() == *id
                    && crate::snooze::Snooze::new(i, crate::snooze::epoch_now(), 600).is_some()
            })
        {
            add(KeyCode::Char('s'), none, strings::CMD_SNOOZE, "s", "snooze");
        }
        if pane.kind == Block::Task
            && pane.responsibilities
            && pane.data.section("summary")["summary"]["open_occurrence"]["task_id"]
                .as_str()
                .is_some()
        {
            add(
                KeyCode::Char('o'),
                none,
                strings::CMD_OCCURRENCE,
                "o",
                "occurrence",
            );
        }
        // Probe only local key handling on a clone. Offer a control only if it
        // opens an authorized confirmation card; no RPC is sent here.
        for (c, label, aliases) in [
            ('e', strings::CMD_STEER, "steer"),
            ('s', strings::CMD_STOP, "stop"),
            ('p', strings::CMD_PAUSE, "pause resume"),
            ('d', strings::CMD_DISABLE, "disable"),
            ('n', strings::CMD_NOW, "run now heartbeat"),
        ] {
            let mut probe = self.clone();
            probe.palette = None;
            probe.m2_key(c, model, Instant::now());
            if probe.control_card.is_some() {
                add(KeyCode::Char(c), none, label, &c.to_string(), aliases);
            }
        }
        commands
    }
    pub(super) fn palette_commands(&self, model: &Model) -> Vec<Command> {
        let query = self
            .palette
            .as_ref()
            .map(|p| p.query.as_str())
            .unwrap_or("");
        self.commands(model)
            .into_iter()
            .filter(|c| fuzzy(query, &format!("{} {} {}", c.label, c.aliases, c.hint)))
            .collect()
    }
    pub(super) fn palette_key(
        &mut self,
        key: KeyEvent,
        model: &Model,
        now: Instant,
        epoch: i64,
    ) -> Action {
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        if key.code == KeyCode::Esc
            || key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('p')
        {
            self.palette = None;
            return Action::None;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return Action::None;
        }
        let commands = self.palette_commands(model);
        let palette = self.palette.as_mut().expect("open palette");
        palette.selected = palette.selected.min(commands.len().saturating_sub(1));
        match key.code {
            KeyCode::Enter => {
                let command = commands.get(palette.selected).cloned();
                self.palette = None;
                if let Some(command) = command {
                    return self.key_at(command.key, model, now, epoch);
                }
            }
            KeyCode::Up => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Down => {
                palette.selected = (palette.selected + 1).min(commands.len().saturating_sub(1))
            }
            KeyCode::Backspace => {
                if let Some((index, _)) = palette.query.grapheme_indices(true).next_back() {
                    palette.query.truncate(index);
                }
                palette.selected = 0;
            }
            KeyCode::Char(c) => palette.paste(&c.to_string()),
            _ => {}
        }
        Action::None
    }
}
pub(super) fn draw(frame: &mut Frame, model: &Model, app: &App, settings: &Settings) {
    let Some(palette) = &app.palette else {
        return;
    };
    let bounds = frame.area();
    let width = bounds.width.saturating_sub(4).min(76);
    let height = bounds.height.saturating_sub(2).min(20);
    let area = Rect::new(
        bounds.x + (bounds.width - width) / 2,
        bounds.y + (bounds.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let inner = super::widgets::panel(frame, area, strings::PALETTE_TITLE, settings, false);
    let commands = app.palette_commands(model);
    let selected = palette.selected.min(commands.len().saturating_sub(1));
    let visible = usize::from(inner.height.saturating_sub(2));
    let offset = (selected + 1).saturating_sub(visible);
    let mut lines = vec![
        format!("> {}_", palette.query),
        strings::PALETTE_KEYS.into(),
    ];
    if commands.is_empty() {
        lines.push(strings::PALETTE_EMPTY.into());
    }
    lines.extend(
        commands
            .iter()
            .enumerate()
            .skip(offset)
            .take(visible)
            .map(|(i, c)| {
                format!(
                    "{} [{}] {}",
                    if i == selected { ">" } else { " " },
                    c.hint,
                    c.label
                )
            }),
    );
    for (i, line) in lines
        .into_iter()
        .take(usize::from(inner.height))
        .enumerate()
    {
        super::widgets::text(
            frame,
            Rect::new(inner.x, inner.y + i as u16, inner.width, 1),
            TerminalText::new(line),
            settings,
            false,
            false,
        );
    }
}
