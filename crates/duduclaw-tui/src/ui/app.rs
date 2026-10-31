use crate::{
    read_state::ReadState,
    sanitize::TerminalText,
    settings::Glyphs,
    state::{Employee, Model},
    strings,
};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmployeeStatus {
    Working,
    Awaiting,
    Error,
    Done,
    Idle,
    Paused,
    Ended,
}
impl EmployeeStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Working => strings::WORKING,
            Self::Awaiting => strings::AWAITING,
            Self::Error => strings::FAILED,
            Self::Done => strings::DONE,
            Self::Idle => strings::IDLE,
            Self::Paused => strings::PAUSED,
            Self::Ended => strings::ENDED,
        }
    }
    pub fn glyph(self, glyphs: Glyphs) -> &'static str {
        match (glyphs, self) {
            (Glyphs::Ascii, Self::Working) => "*",
            (Glyphs::Ascii, Self::Awaiting) => "?",
            (Glyphs::Ascii, Self::Error) => "!",
            (Glyphs::Ascii, Self::Done) => "+",
            (Glyphs::Ascii, Self::Idle) => ".",
            (Glyphs::Ascii, Self::Paused) => "-",
            (Glyphs::Ascii, Self::Ended) => "x",
            (Glyphs::Unicode, Self::Working) => "●",
            (Glyphs::Unicode, Self::Awaiting) => "◇",
            (Glyphs::Unicode, Self::Error) => "!",
            (Glyphs::Unicode, Self::Done) => "✓",
            (Glyphs::Unicode, Self::Idle) => "·",
            (Glyphs::Unicode, Self::Paused) => "Ⅱ",
            (Glyphs::Unicode, Self::Ended) => "×",
        }
    }
}
pub fn employee_status(employee: &Employee, model: &Model, read: &ReadState) -> EmployeeStatus {
    // A pending approval remains awaiting even after it has been read locally.
    if model
        .inbox
        .items
        .iter()
        .any(|i| i.agent().is(employee.id()))
        || employee.status.is("awaiting")
    {
        return EmployeeStatus::Awaiting;
    }
    let activity = model.activity.get(employee.id());
    if activity.is_some_and(|a| a.error)
        || model
            .recent_activity
            .get(employee.id())
            .is_some_and(|a| a.error)
        || employee.status.is("error")
    {
        return EmployeeStatus::Error;
    }
    if completion_ids(model, employee.id()).any(|id| !read.contains(id)) {
        return EmployeeStatus::Done;
    }
    if activity.is_some_and(|a| a.working) || employee.status.is("working") {
        return EmployeeStatus::Working;
    }
    if employee.archived || employee.status.is("ended") || employee.status.is("terminated") {
        return EmployeeStatus::Ended;
    }
    if employee.status.is("paused") {
        return EmployeeStatus::Paused;
    }
    EmployeeStatus::Idle
}

pub(super) fn completion_ids<'a>(model: &'a Model, id: &str) -> impl Iterator<Item = &'a String> {
    model
        .activity
        .get(id)
        .and_then(|a| a.completed_id.as_ref())
        .into_iter()
        .chain(
            model
                .recent_activity
                .get(id)
                .and_then(|a| a.completed_id.as_ref()),
        )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Selection {
    Inbox(String),
    Employee(String),
    Schedule(String),
}
impl Selection {
    pub fn all(model: &Model) -> Vec<Self> {
        model
            .inbox
            .items
            .iter()
            .map(|i| Self::Inbox(i.key()))
            .chain(
                model
                    .employees
                    .items
                    .iter()
                    .map(|e| Self::Employee(e.id().into())),
            )
            .chain(
                model
                    .schedules
                    .items
                    .iter()
                    .filter(|_| super::m2::manager(model))
                    .map(|s| Self::Schedule(s.id.clone())),
            )
            .collect()
    }
}
#[derive(Default, Clone)]
pub struct App {
    pub pty_focus: bool,
    pub(super) pty_confirm: Option<KeyEvent>,
    pub selected: Option<Selection>,
    pub focus: crate::chat::Focus,
    pub input: String,
    pub input_agent: Option<String>,
    pub blocks: crate::chat::Blocks,
    pub chat: crate::chat::ChatState,
    pub chat_scroll: usize,
    pub(super) chat_cache: super::chat_render::TranscriptCache,
    pub spinner: usize,
    pub detail_open: bool,
    pub help: bool,
    pub help_scroll: usize,
    pub(super) palette: Option<super::palette::Palette>,
    pub detail_scroll: usize,
    pub read: ReadState,
    pub notice: Option<String>,
    pub card: Option<super::card::Card>,
    pub waiting_refresh: Option<u64>,
    selected_position: usize,
    pub(super) codes: super::card::Codes,
    pub(super) control_card: Option<super::control_card::ControlCard>,
    pub(super) task_card: bool,
    pub logs: crate::blocks::LogTail,
    pub content_warned: bool,
    pub content_warning: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Save,
    Snooze,
    Decide(crate::decide::Decision),
    Chat(crate::chat::Submission),
    CancelChat,
    Control(crate::controls::Control),
}
impl App {
    pub fn focus_label(&self) -> &'static str {
        if self.pty_confirm.is_some() { strings::PTY_CONFIRM_FOCUS }
        else if self.pty_focus { strings::PTY_FOCUS }
        else if self.palette.is_some() { strings::PALETTE_FOCUS }
        else if self.card.is_some() || self.control_card.is_some() { strings::CARD_FOCUS }
        else if self.help { strings::HELP_FOCUS }
        else if self.focus == crate::chat::Focus::Input { strings::INPUT_FOCUS }
        else { strings::LIST_FOCUS }
    }
    pub fn decision_finished(&mut self, outcome: crate::decide::Outcome) {
        self.card = None;
        self.control_card = None;
        self.task_card = false;
        self.notice = Some(outcome.notice());
        // waiting_refresh is cleared only by an authoritative StateFeed result.
    }
    pub fn refresh(&mut self, model: &Model) {
        if self.waiting_refresh.is_some_and(|n| model.local_refresh > n) {
            self.waiting_refresh = None;
        }
        if let Some(card) = &self.control_card {
            let current = self.control_subject(&card.source, model);
            if current.as_ref().is_some_and(|v| *v != card.fingerprint) {
                self.control_card.as_mut().expect("present").blocked = true;
            }
        }
        let task_item = self.task_card.then(|| self.blocks.panes[self.blocks.focused].data.task()).flatten();
        if model.inbox.count().is_some() && let Some(card) = &mut self.card {
            match if self.task_card { task_item.as_ref() } else { model.inbox.items.iter().find(|i| i.key() == card.item.key()) } {
                None => card.blocked = Some(TerminalText::new(strings::REMOVED.into())),
                Some(current) if current.data() != card.item.data() => {
                    card.blocked = Some(TerminalText::new(strings::CARD_REOPEN.into()));
                }
                Some(current) if card.blocked.is_none() => card.blocked = super::card_data::blocked(current, model),
                _ => {}
            }
        }
        // Keep a surviving ID through insertions/reordering. If it disappears,
        // select the row now at its previous position, or the last row.
        let all = Selection::all(model);
        if let Some(index) = self.selected.as_ref().and_then(|id| all.iter().position(|row| row == id)) {
            self.selected_position = index;
        } else if match &self.selected {
            Some(Selection::Inbox(_)) => model.inbox.count().is_some(),
            Some(Selection::Employee(_)) => model.employees.count().is_some(),
            Some(Selection::Schedule(_)) => model.schedules.count().is_some(),
            None => model.inbox.count().is_some() && model.employees.count().is_some(),
        } {
            self.selected_position = self.selected_position.min(all.len().saturating_sub(1));
            self.selected = all.get(self.selected_position).cloned();
            self.detail_scroll = 0;
        }
    }

    pub fn key(&mut self, key: KeyEvent, model: &Model) -> Action {
        self.key_at(key, model, std::time::Instant::now(), crate::snooze::epoch_now())
    }
    pub fn key_at(&mut self, key: KeyEvent, model: &Model, now: std::time::Instant, epoch: i64) -> Action {
        if self.pty_focus {
            if crate::pty::escape(key) {
                if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty {
                    pty.lock().unwrap_or_else(|e| e.into_inner()).flush_input();
                }
                self.pty_focus = false; return Action::None;
            }
            if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty {
                let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
                let bytes = crate::pty::key_bytes(key, pty.screen().application_cursor());
                pty.send(bytes);
            }
            return Action::None;
        }
        if let Some(pending) = self.pty_confirm {
            if key.kind != KeyEventKind::Press { return Action::None; }
            if key.code == KeyCode::Esc { self.pty_confirm = None; self.notice = None; }
            else if key.code == KeyCode::Char('y') && key.modifiers.is_empty() {
                self.pty_confirm = None; self.notice = None;
                let mut cleaned = true;
                if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty {
                    let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
                    pty.stop(); cleaned = pty.cleanup_complete;
                }
                let action = self.key_at(pending, model, now, epoch);
                if !cleaned { self.notice = Some(strings::PTY_STOP_FAILED.into()); }
                return action;
            }
            return Action::None;
        }
        if self.palette.is_some() {
            return self.palette_key(key, model, now, epoch);
        }
        if key.kind == KeyEventKind::Press && key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('p') && self.card.is_none()
            && self.control_card.is_none() && !self.content_warning {
            self.palette = Some(super::palette::Palette::default());
            return Action::None;
        }
        if self.content_warning {
            if key.kind == KeyEventKind::Press {
                if key.code == KeyCode::Char('c') && key.modifiers.is_empty() {
                    self.content_warned = true; self.blocks.panes[self.blocks.focused].content = true;
                }
                self.content_warning = false;
            }
            return Action::None;
        }
        if self.control_card.is_some() {
            if !self.control_card.as_ref().expect("present").ready(now) { return Action::None; }
            if key.kind == KeyEventKind::Press && key.code == KeyCode::Esc { self.control_card = None; return Action::None; }
            if key.kind == KeyEventKind::Press && key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) { return Action::Quit; }
            let valid = self.control_card.as_ref().is_some_and(|c| self.control_subject(&c.source, model).as_ref() == Some(&c.fingerprint)) && self.waiting_refresh.is_none();
            if let Some(control) = self.control_card.as_mut().expect("present").key(key, now, valid) {
                self.control_card = None; self.waiting_refresh = Some(model.local_refresh);
                self.blocks.invalidate(); self.notice = Some(strings::DECISION_SENDING.into());
                return Action::Control(control);
            }
            return Action::None;
        }
        if let Some(card) = &mut self.card {
            let fresh = model.inbox.count().is_some() && self.waiting_refresh.is_none()
                && (!self.task_card || !self.blocks.panes[self.blocks.focused].data.stale);
            return match card.key(key, now, epoch, fresh) {
                super::card::Effect::None => Action::None,
                super::card::Effect::Quit => Action::Quit,
                super::card::Effect::Close => { self.card = None; self.task_card = false; Action::None },
                super::card::Effect::Snooze => {
                    if self.read.snooze(&card.item, epoch) { self.card = None; Action::Snooze }
                    else { self.notice = Some(strings::SNOOZE_UNAVAILABLE.into()); Action::None }
                }
                super::card::Effect::Send(decision) => {
                    self.card = None;
                    self.waiting_refresh = Some(model.local_refresh);
                    self.notice = Some(strings::DECISION_SENDING.into());
                    self.blocks.invalidate();
                    Action::Decide(decision)
                }
            };
        }
        if self.focus == crate::chat::Focus::Input {
            if key.kind == KeyEventKind::Release { return Action::None; }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') { return Action::Quit; }
            if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) { return Action::None; }
            match key.code {
                KeyCode::Esc => self.focus = crate::chat::Focus::List,
                KeyCode::Char(c) => self.paste(&c.to_string()),
                KeyCode::Backspace => {
                    use unicode_segmentation::UnicodeSegmentation;
                    if let Some((index, _)) = self.input.grapheme_indices(true).next_back() { self.input.truncate(index); }
                }
                KeyCode::Enter => {
                    if self.chat.running || !self.chat.connected {
                        self.notice = Some(if self.chat.running { crate::Error::Busy } else { crate::Error::Disconnected }.to_string());
                    } else if let Some(input) = crate::chat::submission(&self.input, self.input_agent.as_deref()) {
                        return Action::Chat(input);
                    } else { self.notice = Some(strings::INPUT_NEEDS_AGENT.into()); }
                }
                _ => {},
            }
            return Action::None;
        }
        if key.kind != KeyEventKind::Press {
            return Action::None;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        let replaces = key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('w')
            || !key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
                && matches!(key.code, KeyCode::Char('D' | 'C' | 'R' | 'T' | 'L' | '/')) && !self.help;
        if replaces && self.blocks.panes[self.blocks.focused].pty.as_ref().is_some_and(|p| p.lock().unwrap_or_else(|e| e.into_inner()).pending_or_running()) {
            self.pty_confirm = Some(key); self.notice = Some(strings::PTY_CONFIRM.into());
            return Action::None;
        }
        if key.modifiers == KeyModifiers::CONTROL && key.code == KeyCode::Char('w') {
            self.blocks.close(); return Action::None;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return Action::None;
        }
        if !self.help && self.blocks.active == crate::blocks::Block::Pty {
            match key.code {
                KeyCode::Char('i' | 'P') | KeyCode::Enter => {
                    if !crate::pty::SUPPORTED { self.notice = Some(strings::PTY_DISABLED.into()); return Action::None; }
                    self.pty_focus = true;
                    if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty { pty.lock().unwrap_or_else(|e| e.into_inner()).reset_scroll(); }
                    return Action::None;
                }
                KeyCode::PageUp | KeyCode::PageDown => {
                    if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty { pty.lock().unwrap_or_else(|e| e.into_inner()).scroll(key.code == KeyCode::PageUp); }
                    return Action::None;
                }
                _ => {}
            }
        }
        if !self.help && let KeyCode::Char(c) = key.code && let Some(action) = self.m2_key(c, model, now) { return action; }
        match key.code {
            KeyCode::Char('i' | '/') if !self.help => {
                self.focus = crate::chat::Focus::Input;
                self.blocks.chat();
                self.input_agent = match &self.selected {
                    Some(Selection::Employee(id)) => Some(id.clone()),
                    _ => None,
                };
                if key.code == KeyCode::Char('/') && self.input.is_empty() { self.input.push('/'); }
            }
            KeyCode::Char(c @ '1'..='4') if !self.help => self.blocks.select((c as u8 - b'1') as usize),
            KeyCode::Char('X') if !self.help && self.blocks.active == crate::chat::Block::Chat => {
                if self.chat.can_cancel() { return Action::CancelChat; }
                self.notice = Some(strings::CHAT_CANCEL_UNAVAILABLE.into());
            }
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') | KeyCode::F(1) => self.help = !self.help,
            KeyCode::Esc => {
                self.help = false;
                self.detail_open = false;
            }
            KeyCode::Enter => {
                if self.blocks.active == crate::blocks::Block::Task && !self.blocks.panes[self.blocks.focused].responsibilities {
                    if let Some(item) = self.blocks.panes[self.blocks.focused].data.task() {
                        self.card = Some(super::card::Card::open(item, model, now, &mut self.codes)); self.task_card = true;
                    }
                    return Action::None;
                }
                if let Some(Selection::Inbox(id)) = &self.selected
                    && let Some(item) = model.inbox.items.iter().find(|i| i.key() == *id)
                {
                    self.card = Some(super::card::Card::open(item.clone(), model, now, &mut self.codes));
                    if self.read.resurfaced(item, epoch) {
                        self.card.as_mut().expect("just opened").hint = Some(strings::RESURFACED);
                    }
                    self.help = false;
                    return Action::None;
                }
                self.detail_open = true;
                self.help = false;
                self.detail_scroll = 0;
            }
            KeyCode::PageDown if self.help => self.help_scroll = self.help_scroll.saturating_add(5),
            KeyCode::PageUp if self.help => self.help_scroll = self.help_scroll.saturating_sub(5),
            KeyCode::PageDown if self.blocks.active == crate::chat::Block::Chat && !self.help => self.chat_scroll = self.chat_scroll.saturating_sub(5),
            KeyCode::PageUp if self.blocks.active == crate::chat::Block::Chat && !self.help => self.chat_scroll = self.chat_scroll.saturating_add(5),
            KeyCode::PageDown if matches!(self.blocks.active, crate::blocks::Block::Run | crate::blocks::Block::Task | crate::blocks::Block::Log) => {
                let p = &mut self.blocks.panes[self.blocks.focused]; p.scroll = p.scroll.saturating_add(5);
            }
            KeyCode::PageUp if matches!(self.blocks.active, crate::blocks::Block::Run | crate::blocks::Block::Task | crate::blocks::Block::Log) => {
                let p = &mut self.blocks.panes[self.blocks.focused]; p.scroll = p.scroll.saturating_sub(5);
            }
            KeyCode::PageDown => self.detail_scroll = self.detail_scroll.saturating_add(5),
            KeyCode::PageUp => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            KeyCode::Down | KeyCode::Right | KeyCode::Char('j') => self.move_by(model, true),
            KeyCode::Up | KeyCode::Left | KeyCode::Char('k') => self.move_by(model, false),
            KeyCode::Char('r') => {
                let ids: Vec<_> = match &self.selected {
                    Some(Selection::Inbox(id))
                        if model.inbox.items.iter().any(|i| i.key() == *id) =>
                    {
                        vec![id.clone()]
                    }
                    Some(Selection::Employee(id)) => completion_ids(model, id).cloned().collect(),
                    _ => Vec::new(),
                };
                if !ids.is_empty() {
                    for id in ids {
                        self.read.mark(id);
                    }
                    return Action::Save;
                }
                self.notice = Some(strings::NO_ID.into());
            }
            KeyCode::Char('s') => {
                if let Some(Selection::Inbox(id)) = &self.selected
                    && let Some(item) = model.inbox.items.iter().find(|i| i.key() == *id)
                {
                    if self.read.snooze(item, epoch) { return Action::Snooze; }
                    self.notice = Some(strings::SNOOZE_UNAVAILABLE.into());
                }
            }
            _ => {}
        }
        Action::None
    }
    pub fn paste(&mut self, text: &str) {
        if self.pty_confirm.is_some() { return; }
        if self.pty_focus {
            if let Some(pty) = &self.blocks.panes[self.blocks.focused].pty {
                let mut pty = pty.lock().unwrap_or_else(|e| e.into_inner());
                pty.flush_input();
                if text.len() > crate::pty::MAX_INPUT { pty.status = strings::PTY_INPUT_FULL.into(); }
                else { let bytes = crate::pty::paste_bytes(text, pty.screen().bracketed_paste()); pty.send(bytes); }
            }
            return;
        }
        if let Some(palette) = &mut self.palette { palette.paste(text); return; }
        if let Some(card) = &mut self.control_card { card.paste(text); return; }
        if self.focus != crate::chat::Focus::Input || self.card.is_some() { return; }
        let remaining = crate::chat::MAX_INPUT_CHARS.saturating_sub(self.input.chars().count());
        self.input.extend(text.chars().filter(|c| !crate::sanitize::forbidden(*c))
            .map(|c| if c == '\n' { ' ' } else { c }).take(remaining));
    }
    fn move_by(&mut self, model: &Model, down: bool) {
        let all = Selection::all(model);
        if all.is_empty() {
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|s| all.iter().position(|x| x == s));
        let next = match index {
            Some(i) if down => (i + 1).min(all.len() - 1),
            Some(i) => i.saturating_sub(1),
            None => 0,
        };
        self.selected_position = next;
        self.selected = Some(all[next].clone());
        self.detail_scroll = 0;
    }
    pub fn detail(&self, model: &Model) -> TerminalText {
        if self.help {
            return TerminalText::new(strings::HELP.into());
        }
        self.selection_detail(model)
    }
    pub(super) fn selection_detail(&self, model: &Model) -> TerminalText {
        if !self.detail_open {
            return TerminalText::new(strings::DETAIL_HINT.into());
        }
        match &self.selected {
            Some(Selection::Inbox(id)) => {
                if let Some(item) = model.inbox.items.iter().find(|i| i.key() == *id) {
                    return item.detail_text();
                }
            }
            Some(Selection::Employee(id)) => {
                if let Some(e) = model.employees.items.iter().find(|e| e.id() == id) {
                    return TerminalText::new(format!(
                        "{}\n{}\n{}\n{}",
                        e.display_name.visible(),
                        e.name_text().visible(),
                        employee_status(e, model, &self.read).label(),
                        if super::m2::manager(model) && super::m2::operator(model, Some(id)) { strings::EMPLOYEE_KEYS } else { strings::CARD_NO_PERMISSION }
                    ));
                }
            }
            Some(Selection::Schedule(id)) if super::m2::manager(model) => {
                if let Some(s) = model.schedules.items.iter().find(|s| &s.id == id) {
                    let fields: Vec<_> = ["cron", "cron_timezone", "enabled", "last_run", "last_run_at", "last_status"]
                        .iter().filter(|key| s.detail.get(**key).is_some())
                        .map(|key| format!("{}: {}", strings::field_label(key), crate::blocks::field(&s.detail, key))).collect();
                    return TerminalText::new(format!("{}\n{}: {}\n{}\n{}", s.agent.visible(), strings::NEXT_RUNS,
                        s.next_run.visible(), fields.join("\n"),
                        if matches!(s.source, crate::state_extra::ScheduleSource::Heartbeat) { strings::HEARTBEAT_KEYS } else { strings::CRON_KEYS }));
                }
            }
            _ => {}
        }
        TerminalText::new(strings::REMOVED.into())
    }
}
