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
#[derive(Default)]
pub struct App {
    pub selected: Option<Selection>,
    pub detail_open: bool,
    pub help: bool,
    pub detail_scroll: usize,
    pub read: ReadState,
    pub notice: Option<String>,
    initialized: bool,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Save,
}
impl App {
    pub fn refresh(&mut self, model: &Model) {
        // Establish an initial selection once. Later insertions, removals and
        // reordering never move it; a removed ID remains a visible tombstone.
        if !self.initialized && model.inbox.count().is_some() && model.employees.count().is_some() {
            self.selected = Selection::all(model).into_iter().next();
            self.initialized = true;
        }
    }
    pub fn key(&mut self, key: KeyEvent, model: &Model) -> Action {
        if key.kind == KeyEventKind::Release {
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
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Char('?') | KeyCode::F(1) => self.help = !self.help,
            KeyCode::Esc => {
                self.help = false;
                self.detail_open = false;
            }
            KeyCode::Enter => {
                self.detail_open = true;
                self.help = false;
                self.detail_scroll = 0;
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
            _ => {}
        }
        Action::None
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
