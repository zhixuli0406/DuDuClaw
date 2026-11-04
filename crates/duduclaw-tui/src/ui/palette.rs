//! Palette actions share the key handler and authorized control builders.
use super::{
    app::{Action, App, Selection},
    m2,
};
use crate::{
    blocks::Block, chat::Focus, sanitize::TerminalText, settings::Settings, state::Model, strings,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::{Frame, layout::Rect};
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
    control: Option<crate::controls::Control>,
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
                control: None,
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
        add(KeyCode::Char('I'), none, strings::INBOX, "I", "inbox");
        add(KeyCode::Char('M'), none, "通知信箱", "M", "mailbox notifications");
        add(KeyCode::Char('z'), none, "放大／還原區塊", super::shell::ZOOM_KEY, "zoom fullscreen");
        if m2::manager(model) { add(KeyCode::Char('S'), none, strings::NEXT_RUNS, "S", "schedule heartbeat cron"); }
        add(KeyCode::Tab, none, "下一個員工分頁", "Tab", "tab workspace");
        add(KeyCode::F(1), none, strings::CMD_HELP, &format!("F1 / {}", super::shell::HELP_KEY), "help");
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
        if self.blocks.active == Block::Chat && self.downstream_reference(Instant::now()).is_some() {
            add(KeyCode::Char('y'), none, strings::CMD_DOWNSTREAM_STOP, "y", "stop downstream");
            add(KeyCode::Char('n'), none, strings::CMD_DOWNSTREAM_DISMISS, "n / Esc", "continue downstream dismiss");
        }
        for (c, label, alias) in [('W', strings::WORKFLOW_BLOCK, "workflow"), ('A', strings::AUTOPILOT_BLOCK, "autopilot"), ('E', strings::EVOLUTION_BLOCK, "evolution playbook"), ('B', strings::BUDGET_BLOCK, "budget"), ('G', strings::DIGEST_BLOCK, "digest")] {
            add(KeyCode::Char(c), none, label, &c.to_string(), alias);
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
        if m2::operator(model, self.block_agent(model).as_deref().or(self.blocks.panes[self.blocks.focused].agent.as_deref())) {
            add(KeyCode::Char('N'), none, strings::GOAL_CREATE, "N", "goal create plan");
        }
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
        if matches!(pane.kind, Block::Run | Block::Task | Block::Workflow) {
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
        if pane.kind == Block::Task && !pane.responsibilities && pane.data.selected["goal_mode"] == true {
            if pane.data.selected["plan_pending"].as_str().is_some_and(|s| !s.is_empty()) {
                add(KeyCode::Enter, none, strings::PLAN_APPROVE, "Enter", "approve plan");
            }
            if matches!(pane.data.selected["status"].as_str(), Some("done" | "failed" | "cancelled")) {
                add(KeyCode::Char('c'), none, strings::CONTINUE_OPEN, "c", "continue goal");
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
            ('s', if pane.kind == Block::Workflow { strings::WORKFLOW_CANCEL } else { strings::CMD_STOP }, "stop cancel workflow"),
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
        let mut controls = Vec::new();
        if model.role == Some(crate::auth::Role::Admin) {
            for resume in [false, true] {
                controls.push(crate::controls::Control::Autonomy { agent: None, resume });
            }
        }
        if let Some(Selection::Employee(id)) = &self.selected
            && m2::manager(model) && model.goal_operators.contains(id) {
            for resume in [false, true] {
                controls.push(crate::controls::Control::Autonomy { agent: Some(id.clone()), resume });
            }
            if self.control_subject(&super::control_card::Source::Employee(id.clone()), model).is_some() {
                for resume in [false, true] {
                    controls.push(crate::controls::Control::Employee { agent: id.clone(), resume });
                }
            }
        }
        for control in controls {
            let aliases = match &control {
                crate::controls::Control::Autonomy { agent: None, resume: false } => "all autonomy pause",
                crate::controls::Control::Autonomy { agent: None, resume: true } => "all autonomy resume",
                crate::controls::Control::Autonomy { resume: false, .. } => "autonomy pause",
                crate::controls::Control::Autonomy { resume: true, .. } => "autonomy resume",
                crate::controls::Control::Employee { resume: false, .. } => "employee pause",
                _ => "employee resume",
            };
            commands.push(Command { label: control.label(), key: KeyEvent::new(KeyCode::Null, none),
                hint: "Enter".into(), aliases, control: Some(control) });
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
            .filter(|c| fuzzy(query, c.label) || fuzzy(query, c.aliases) || fuzzy(query, &c.hint))
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
                    // An explicit palette command leaves the local notification surface.
                    self.mailbox = None;
                    self.toast_focus = None;
                    match command.control {
                        Some(crate::controls::Control::Autonomy { agent, resume }) => return self.autonomy_control(agent, resume, model),
                        Some(control @ crate::controls::Control::Employee { .. }) => {
                            if let crate::controls::Control::Employee { agent, .. } = &control {
                                let source = super::control_card::Source::Employee(agent.clone());
                                self.open_control(control, source, model, now);
                            }
                            return Action::None;
                        }
                        _ => return self.key_at(command.key, model, now, epoch),
                    }
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
    let inner = super::widgets::overlay(frame, area, strings::PALETTE_TITLE, settings);
    let commands = app.palette_commands(model);
    let selected = palette.selected.min(commands.len().saturating_sub(1));
    let visible = usize::from(inner.height.saturating_sub(3));
    let offset = (selected + 1).saturating_sub(visible);
    super::widgets::styled(frame, Rect::new(inner.x, inner.y, inner.width, 1),
        &format!("> {}_", palette.query), settings, settings.colors.base());
    super::widgets::styled(frame, Rect::new(inner.x, inner.y + 1, inner.width, 1),
        "指令", settings, settings.colors.muted());
    if commands.is_empty() {
        super::widgets::styled(frame, Rect::new(inner.x, inner.y + 2, inner.width, 1), strings::PALETTE_EMPTY, settings, settings.colors.muted());
    }
    for (row, (index, command)) in commands.iter().enumerate().skip(offset).take(visible).enumerate() {
        let area = Rect::new(inner.x, inner.y + 2 + row as u16, inner.width, 1);
        let style = if index == selected { settings.colors.selected() } else { settings.colors.base() };
        frame.buffer_mut().set_style(area, style);
        let chip = format!(" {} ", command.hint);
        let chip_width = super::text::columns(&chip, settings.ambiguous_width).min(usize::from(area.width / 3)) as u16;
        let label_area = Rect::new(area.x + 2, area.y, area.width.saturating_sub(chip_width + 3), 1);
        super::widgets::styled(frame, Rect::new(area.x, area.y, 2.min(area.width), 1),
            if index == selected { if settings.glyphs == crate::settings::Glyphs::Unicode { "›" } else { ">" } } else { " " }, settings, style);
        super::widgets::styled(frame, label_area, command.label, settings, style);
        // Highlight the same subsequence used by fuzzy matching; aliases need not match the label.
        let query = palette.query.to_lowercase();
        let mut query = query.chars().filter(|c| !c.is_whitespace()).peekable();
        let mut x = label_area.x;
        for g in command.label.graphemes(true) {
            let width = super::text::columns(g, settings.ambiguous_width) as u16;
            if x + width > label_area.right() { break; }
            if query.peek().is_some_and(|ch| g.to_lowercase().contains(*ch)) {
                query.next();
                super::widgets::styled(frame, Rect::new(x, area.y, width, 1), g, settings,
                    settings.colors.on(style.bg.unwrap_or(settings.colors.surface_raised)).foreground(settings.colors.accent));
            }
            x += width;
        }
        super::widgets::styled(frame, Rect::new(area.right() - chip_width, area.y, chip_width, 1),
            &chip, settings, settings.colors.on(settings.colors.hover).muted());
    }
    super::widgets::key_hints(frame, Rect::new(inner.x, inner.bottom().saturating_sub(1), inner.width, 1),
        TerminalText::new("↑↓ 選取  Enter 執行  Esc 關閉".into()), settings);
}
