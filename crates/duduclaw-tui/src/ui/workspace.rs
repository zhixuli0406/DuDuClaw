//! Sidebar navigation and workspace tabs reuse existing blocks and snapshots.
use super::app::{App, EmployeeStatus, Selection, employee_status};
use crate::{blocks::Block, read_state::ReadState, sanitize::TerminalText, state::{Employee, Model}, strings};
use crossterm::event::{KeyCode, KeyEvent};

pub(super) fn name(employee: &Employee) -> String {
    let display = employee.display_name.plain();
    if display.trim().is_empty() { employee.name_text().plain() } else { display }.replace('\n', " ")
}

pub(super) fn employees<'a>(model: &'a Model, read: &ReadState) -> Vec<&'a Employee> {
    let mut employees: Vec<_> = model.employees.items.iter().collect();
    employees.sort_by_cached_key(|employee| {
        let rank = match employee_status(employee, model, read) {
            EmployeeStatus::Awaiting => 0,
            EmployeeStatus::Error => 1,
            EmployeeStatus::Working => 2,
            EmployeeStatus::Idle | EmployeeStatus::Done => 3,
            _ => 4,
        };
        (rank, name(employee), employee.id().to_owned())
    });
    employees
}

pub(super) fn context(employee: &Employee, model: &Model, app: &App) -> TerminalText {
    if let Some(item) = model.inbox.items.iter().filter(|i| i.agent().is(employee.id()))
        .min_by_key(|i| (app.read.contains(&i.key()), matches!(i, crate::state::InboxItem::Breaker(_)))) {
        return TerminalText::new(format!("{}：{}", item.source_label(), item.title().plain().replace('\n', " ")));
    }
    // No inferred rounds or elapsed time: only an already fetched task's dispatch record.
    for pane in app.blocks.panes.iter().chain(&app.blocks.parked) {
        let task = &pane.data.selected;
        if pane.kind != Block::Task || pane.responsibilities || pane.data.stale || pane.data.error.is_some()
            || task["assigned_to"].as_str() != Some(employee.id()) || task["goal_mode"] != true
            || !matches!(task["status"].as_str(), Some("doing" | "in_progress" | "running" | "review")) { continue; }
        if let Some(round) = pane.data.section("iterations")["iterations"].as_array()
            .and_then(|rows| rows.iter().filter(|r| r["round"].as_u64().is_some()).max_by_key(|r| r["round"].as_u64())) {
            let n = round["round"].as_u64().unwrap_or_default();
            let limit = task["iteration_cap"].as_u64().filter(|n| *n > 0).map(|m| format!("/{m}")).unwrap_or_default();
            let elapsed = round["dispatched_at"].as_str().and_then(crate::human_time::elapsed_minutes)
                .map(|minutes| format!(" · 已 {minutes} 分")).unwrap_or_default();
            return TerminalText::new(format!("第 {n}{limit} 回合{elapsed}"));
        }
    }
    if super::m2::manager(model) && model.schedules.count().is_some()
        && let Some(schedule) = model.schedules.items.iter().find(|s| s.agent.is(employee.id())
            && matches!(s.source, crate::state_extra::ScheduleSource::Heartbeat)
            && !s.next_run.is(strings::NOT_SCHEDULED)) {
        return TerminalText::new(format!("下次心跳 {}", crate::human_time::schedule(&schedule.next_run)));
    }
    if !model.autonomy.unconfirmed() && let Some(status) = model.autonomy.items.first()
        && let Some(pause) = status.global.as_ref().or_else(|| status.agents.get(employee.id())) {
        return TerminalText::new(format!("暫停：{}", pause.reason.plain().replace('\n', " ")));
    }
    if model.autonomy.unconfirmed() { return TerminalText::new(strings::AUTONOMY_ROW_UNKNOWN.into()); }
    TerminalText::new(strings::IDLE.into())
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MainView {
    #[default]
    Workspace,
    Inbox,
    Schedules,
}

impl App {
    pub(super) fn list_view(&self) -> bool { self.main_view != MainView::Workspace }

    pub(super) fn selections(&self, model: &Model) -> Vec<Selection> {
        match self.selected {
            Some(Selection::Inbox(_)) => model.inbox.items.iter().map(|i| Selection::Inbox(i.key())).collect(),
            Some(Selection::Schedule(_)) if super::m2::manager(model) => model.schedules.items.iter().map(|s| Selection::Schedule(s.id.clone())).collect(),
            _ => Selection::all(model, &self.read),
        }
    }
    pub(super) fn workspace_tab(&self) -> usize {
        match self.blocks.active { Block::Task => 1, Block::Run | Block::Log => 2, _ => 0 }
    }
    pub(super) fn open_workspace(&mut self, model: &Model, tab: usize) -> bool {
        let Some(agent) = self.block_agent(model).or_else(|| self.blocks.panes[self.blocks.focused].agent.clone()) else { return false; };
        // A tab switch never kills or replaces a local terminal.
        if self.blocks.active == Block::Pty {
            if self.blocks.panes.len() >= 4 { self.notice = Some("請先關閉一個區塊，再開啟員工分頁".into()); return false; }
            let old_workspace = self.blocks.panes[self.blocks.focused].workspace_id.clone();
            self.blocks.add(crate::blocks::Split::Vertical);
            let new_workspace = self.blocks.panes[self.blocks.focused].workspace_id.clone();
            for pane in &mut self.blocks.parked {
                if pane.workspace_id == old_workspace { pane.workspace_id = new_workspace.clone(); }
            }
        }
        self.workspace_initialized = true;
        let previous = self.chat_view();
        let id = self.blocks.panes[self.blocks.focused].id.clone();
        self.blocks.panes[self.blocks.focused].chat_view = previous.clone();
        let kind = match tab { 1 => Block::Task, 2 => Block::Run, _ => Block::Chat };
        if !self.blocks.workspace(kind, agent.clone()) {
            self.notice = Some("已有四個對話，請切回不需要的對話並以 Ctrl-W 關閉".into());
            return false;
        }
        self.main_view = MainView::Workspace;
        if self.blocks.panes[self.blocks.focused].id != id {
            if let Some(pane) = self.blocks.panes.iter_mut().chain(&mut self.blocks.parked).find(|p| p.id == id) {
                pane.chat_view = previous.clone();
            }
            let mut view = self.blocks.panes[self.blocks.focused].chat_view.clone();
            if view.agent.is_none() { view.agent = Some(agent.clone()); }
            view.state.connected = previous.state.connected;
            view.state.stale = !previous.state.connected;
            self.set_chat_view(view);
        }
        if kind == Block::Task && self.blocks.panes[self.blocks.focused].task_id.is_none() {
            self.blocks.panes[self.blocks.focused].task_id = model.activity.get(&agent).and_then(|a| a.task_id.clone());
        }
        self.focus = crate::chat::Focus::List;
        true
    }
    pub(super) fn leave_list(&mut self) {
        self.main_view = MainView::Workspace;
        match self.selected {
            Some(Selection::Inbox(_)) => self.selected = Some(Selection::InboxEntry),
            Some(Selection::Schedule(_)) => self.selected = Some(Selection::ScheduleEntry),
            _ => {}
        }
    }
    pub(super) fn workspace_key(&mut self, key: KeyEvent, model: &Model, now: std::time::Instant) -> bool {
        match key.code {
            KeyCode::Char('I') => {
                self.zoomed = false;
                self.main_view = MainView::Inbox;
                self.selected = Some(model.inbox.items.first().map(|i| Selection::Inbox(i.key())).unwrap_or(Selection::InboxEntry));
                self.detail_open = false;
            }
            KeyCode::Char('S') if super::m2::manager(model) => {
                self.zoomed = false;
                self.main_view = MainView::Schedules;
                self.selected = Some(model.schedules.items.first().map(|s| Selection::Schedule(s.id.clone())).unwrap_or(Selection::ScheduleEntry));
                self.detail_open = false;
            }
            KeyCode::Enter if self.selected == Some(Selection::InboxEntry) => {
                self.zoomed = false;
                self.main_view = MainView::Inbox;
                self.selected = Some(model.inbox.items.first().map(|i| Selection::Inbox(i.key())).unwrap_or(Selection::InboxEntry));
            }
            KeyCode::Enter if self.selected == Some(Selection::ScheduleEntry) => {
                if super::m2::manager(model) {
                    self.zoomed = false;
                    self.main_view = MainView::Schedules;
                    self.selected = Some(model.schedules.items.first().map(|s| Selection::Schedule(s.id.clone())).unwrap_or(Selection::ScheduleEntry));
                }
            }
            KeyCode::Tab | KeyCode::BackTab => {
                let tab = (self.workspace_tab() + if key.code == KeyCode::BackTab { 2 } else { 1 }) % 3;
                let opened = self.open_workspace(model, tab);
                if opened && let Some(agent) = self.blocks.panes[self.blocks.focused].agent.clone() {
                    self.selected = Some(Selection::Employee(agent));
                }
            }
            KeyCode::Enter if matches!(self.selected, Some(Selection::Employee(_))) && self.blocks.active != Block::Task && self.blocks.active != Block::Detail => {
                if let Some(item) = super::inline_decision::item(self, model).cloned() {
                    self.notices.seen(&item.key());
                    self.task_card = false;
                    self.card = Some(super::card::Card::open(item, model, now, &mut self.codes));
                } else { self.open_workspace(model, self.workspace_tab()); }
            }
            _ => return false,
        }
        true
    }
    pub(super) fn help_text(&self) -> TerminalText {
        TerminalText::new(format!("{}\n\nHome：{}", strings::HELP, self.home.visible()))
    }
}
