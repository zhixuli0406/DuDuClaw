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

fn completion_ids<'a>(model: &'a Model, id: &str) -> impl Iterator<Item = &'a String> {
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
                    .filter(|_| model.role == Some(crate::auth::Role::Admin))
                    .map(|s| Self::Schedule(s.id.clone())),
            )
            .collect()
    }
}
#[derive(Default, Clone)]
pub struct App {
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
    pub detail_scroll: usize,
    pub read: ReadState,
    pub notice: Option<String>,
    pub card: Option<super::card::Card>,
    pub waiting_refresh: Option<u64>,
    selected_position: usize,
    codes: super::card::Codes,
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
}
impl App {
    pub fn focus_label(&self) -> &'static str {
        if self.card.is_some() { strings::CARD_FOCUS }
        else if self.help { strings::HELP_FOCUS }
        else if self.focus == crate::chat::Focus::Input { strings::INPUT_FOCUS }
        else { strings::LIST_FOCUS }
    }
    pub fn decision_finished(&mut self, outcome: crate::decide::Outcome) {
        self.card = None;
        self.notice = Some(outcome.notice());
        // waiting_refresh is cleared only by an authoritative StateFeed result.
    }
    pub fn refresh(&mut self, model: &Model) {
        if self.waiting_refresh.is_some_and(|n| model.local_refresh > n) {
            self.waiting_refresh = None;
        }
        if model.inbox.count().is_some() && let Some(card) = &mut self.card {
            match model.inbox.items.iter().find(|i| i.key() == card.item.key()) {
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
        if let Some(card) = &mut self.card {
            let fresh = model.inbox.count().is_some() && self.waiting_refresh.is_none();
            return match card.key(key, now, epoch, fresh) {
                super::card::Effect::None => Action::None,
                super::card::Effect::Quit => Action::Quit,
                super::card::Effect::Close => { self.card = None; Action::None },
                super::card::Effect::Snooze => {
                    if self.read.snooze(&card.item, epoch) { self.card = None; Action::Snooze }
                    else { self.notice = Some(strings::SNOOZE_UNAVAILABLE.into()); Action::None }
                }
                super::card::Effect::Send(decision) => {
                    self.card = None;
                    self.waiting_refresh = Some(model.local_refresh);
                    self.notice = Some(strings::DECISION_SENDING.into());
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
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
        {
            return Action::None;
        }
        match key.code {
            KeyCode::Char('i' | '/') if !self.help => {
                self.focus = crate::chat::Focus::Input;
                self.blocks.select(1);
                self.input_agent = match &self.selected {
                    Some(Selection::Employee(id)) => Some(id.clone()),
                    _ => None,
                };
                if key.code == KeyCode::Char('/') && self.input.is_empty() { self.input.push('/'); }
            }
            KeyCode::Char('1') if !self.help => self.blocks.select(0),
            KeyCode::Char('2') if !self.help => self.blocks.select(1),
            KeyCode::Char('x') if !self.help && self.blocks.active == crate::chat::Block::Chat => {
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
            KeyCode::PageDown if self.blocks.active == crate::chat::Block::Chat && !self.help => self.chat_scroll = self.chat_scroll.saturating_sub(5),
            KeyCode::PageUp if self.blocks.active == crate::chat::Block::Chat && !self.help => self.chat_scroll = self.chat_scroll.saturating_add(5),
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
                        "{}\n{}\n{}",
                        e.display_name.visible(),
                        e.name_text().visible(),
                        employee_status(e, model, &self.read).label()
                    ));
                }
            }
            Some(Selection::Schedule(id)) if model.role == Some(crate::auth::Role::Admin) => {
                if let Some(s) = model.schedules.items.iter().find(|s| &s.id == id) {
                    return TerminalText::new(format!(
                        "{}\n{}",
                        s.agent.visible(),
                        s.next_run.visible()
                    ));
                }
            }
            _ => {}
        }
        TerminalText::new(strings::REMOVED.into())
    }
}
