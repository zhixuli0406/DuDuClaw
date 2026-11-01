//! Block commands and permission-aware control construction.
use super::{
    app::{Action, App, Selection},
    control_card::{ControlCard, Source},
};
use crate::{
    auth::Role,
    blocks::Block,
    controls::{Control, CronAction},
    state::Model,
    strings,
};
use serde_json::{Value, json};
use std::time::Instant;
pub(super) fn manager(model: &Model) -> bool {
    matches!(model.role, Some(Role::Admin | Role::Manager))
}
pub(super) fn operator(model: &Model, agent: Option<&str>) -> bool {
    model.role.is_some() && model.employees.count().is_some() && agent.is_some_and(|a| model.goal_operators.contains(a))
}
impl App {
    pub(super) fn block_agent(&self, model: &Model) -> Option<String> {
        match &self.selected {
            Some(Selection::Employee(id)) => Some(id.clone()),
            Some(Selection::Inbox(id)) => model
                .inbox
                .items
                .iter()
                .find(|i| i.key() == *id)
                .and_then(|i| match i {
                    crate::state::InboxItem::Task(_) => {
                        i.data()["assigned_to"].as_str().map(str::to_owned)
                    }
                    _ => i.data()["agent_id"].as_str().map(str::to_owned),
                }),
            _ => None,
        }
    }
    pub(super) fn control_subject(&self, source: &Source, model: &Model) -> Option<Value> {
        match source {
            Source::Employee(id) if manager(model) && operator(model, Some(id)) => model
                .employees
                .items
                .iter()
                .find(|e| e.id() == id)
                .and_then(|e| serde_json::to_value(e).ok()),
            Source::Schedule(id) if model.schedules.count().is_some() => model
                .schedules
                .items
                .iter()
                .find(|s| &s.id == id)
                .filter(|s| {
                    manager(model)
                        && (matches!(s.source, crate::state_extra::ScheduleSource::Heartbeat)
                            || model.role == Some(Role::Admin))
                })
                .map(|s| s.detail.clone()),
            Source::Pane(i) => {
                let p = self.blocks.panes.get(*i)?;
                let row = &p.data.selected;
                let agent = if p.responsibilities {
                    row["owner_agent_id"].as_str()
                } else {
                    row["assigned_to"].as_str()
                };
                if p.kind != Block::Task
                    || p.data.stale
                    || !operator(model, agent)
                    || row["kind"] == "discovery"
                {
                    return None;
                }
                Some(json!({"row":row,"revision":p.data.section("stop")["authority_revision"]}))
            }
            _ => None,
        }
    }
    pub(super) fn open_control(
        &mut self,
        control: Control,
        source: Source,
        model: &Model,
        now: Instant,
    ) {
        if self.waiting_refresh.is_some() {
            return;
        }
        let Some(fingerprint) = self.control_subject(&source, model) else {
            self.notice = Some(strings::CARD_NO_PERMISSION.into());
            return;
        };
        self.control_card = Some(ControlCard::new(
            control,
            source,
            fingerprint,
            self.codes.next(),
            now,
        ));
    }
    pub(super) fn m2_key(&mut self, c: char, model: &Model, now: Instant) -> Option<Action> {
        let i = self.blocks.focused;
        if matches!(c, 'p' | 'n') && self.blocks.active != Block::Detail {
            self.notice = Some(strings::CONTROL_KEY_FOCUS.into());
            return Some(Action::None);
        }
        match c {
            'v' | 'x' => {
                self.blocks.add(if c == 'v' {
                    crate::blocks::Split::Vertical
                } else {
                    crate::blocks::Split::Horizontal
                });
            }
            'P' => { self.blocks.open(Block::Pty, None); self.pty_focus = crate::pty::SUPPORTED; self.focus = crate::chat::Focus::List; },
            'D' => self.blocks.open(Block::Detail, None),
            'C' => self.blocks.chat(),
            'R' | 'T' => {
                if let Some(agent) = self.block_agent(model) {
                    self.blocks
                        .open(if c == 'R' { Block::Run } else { Block::Task }, Some(agent));
                    if c == 'T'
                        && let Some(Selection::Inbox(id)) = &self.selected
                    {
                        self.blocks.panes[i].task_id = model
                            .inbox
                            .items
                            .iter()
                            .find(|item| item.key() == *id)
                            .and_then(|item| {
                                if let crate::state::InboxItem::Task(t) = item {
                                    Some(t.id().to_owned())
                                } else {
                                    None
                                }
                            });
                    }
                } else {
                    self.notice = Some(strings::SELECT_EMPLOYEE.into());
                }
            }
            'L' => self.blocks.open(Block::Log, None),
            'o' if self.blocks.active == Block::Task && self.blocks.panes[i].responsibilities => {
                let p = &mut self.blocks.panes[i];
                let task = p.data.section("summary")["summary"]["open_occurrence"]["task_id"]
                    .as_str()?
                    .to_owned();
                p.task_id = Some(task);
                p.responsibilities = false;
                p.index = 0;
                p.scroll = 0;
                p.data = crate::blocks::Data::default();
                self.blocks.revision += 1;
            }
            '[' | ']' if matches!(self.blocks.active, Block::Run | Block::Task) => {
                let p = &mut self.blocks.panes[i];
                p.index = if c == '[' {
                    p.index.saturating_sub(1)
                } else {
                    (p.index + 1).min(p.data.total.saturating_sub(1))
                };
                p.scroll = 0;
                p.task_id = None;
                p.data = crate::blocks::Data::default();
                self.blocks.revision += 1;
            }
            'b' if self.blocks.active == Block::Task => {
                let p = &mut self.blocks.panes[i];
                p.responsibilities = !p.responsibilities;
                p.index = 0;
                p.scroll = 0;
                p.task_id = None;
                p.data = crate::blocks::Data::default();
                self.blocks.revision += 1;
            }
            'c' if self.blocks.active == Block::Run => {
                if !self.content_warned {
                    self.content_warning = true;
                    self.notice = Some(strings::CONTENT_CONFIRM.into());
                } else {
                    self.blocks.panes[i].content = !self.blocks.panes[i].content;
                }
            }
            'e' | 's' | 'p' | 'd'
                if self.blocks.active == Block::Task
                    && (matches!(c, 'e' | 's') || self.blocks.panes[i].responsibilities) =>
            {
                let p = &self.blocks.panes[i];
                let row = &p.data.selected;
                if !p.responsibilities
                    && c == 'e'
                    && (row["goal_mode"] != true
                        || matches!(
                            row["status"].as_str(),
                            Some("done" | "cancelled" | "failed")
                        ))
                {
                    return Some(Action::None);
                }
                let control = if p.responsibilities {
                    if !matches!(c, 'p' | 'd') {
                        self.notice = Some(strings::RESP_OCCURRENCE.into());
                        return Some(Action::None);
                    }
                    Some(Control::Responsibility {
                        id: row["responsibility_id"].as_str()?.into(),
                        epoch: row["control_epoch"].as_i64()?,
                        disable: c == 'd',
                    })
                } else {
                    let task = row["id"].as_str()?.to_owned();
                    match c {
                        'e' => Some(Control::Steer {
                            task,
                            body: String::new(),
                            request_id: crate::controls::request_id(),
                        }),
                        's' => Some(Control::Stop {
                            task,
                            revision: p.data.section("stop")["authority_revision"].as_i64()?,
                        }),
                        _ => None,
                    }
                };
                if let Some(control) = control {
                    self.open_control(control, Source::Pane(i), model, now);
                }
            }
            'p' | 'n' => match self.selected.clone() {
                Some(Selection::Employee(id)) if c == 'p' => {
                    let resume = model
                        .employees
                        .items
                        .iter()
                        .find(|e| e.id() == id)
                        .is_some_and(|e| e.status.is("paused"));
                    self.open_control(
                        Control::Employee {
                            agent: id.clone(),
                            resume,
                        },
                        Source::Employee(id),
                        model,
                        now,
                    );
                }
                Some(Selection::Schedule(id)) => {
                    if let Some(s) = model.schedules.items.iter().find(|s| s.id == id) {
                        let r = &s.detail;
                        let control = match s.source {
                            crate::state_extra::ScheduleSource::Heartbeat if c == 'n' => {
                                Control::Heartbeat {
                                    agent: r["agent_id"].as_str()?.into(),
                                }
                            }
                            crate::state_extra::ScheduleSource::Cron => Control::Cron {
                                id: r["id"].as_str()?.into(),
                                action: if c == 'n' {
                                    CronAction::Run
                                } else if r["enabled"].as_bool()? {
                                    CronAction::Pause
                                } else {
                                    CronAction::Resume
                                },
                                request_id: crate::controls::request_id(),
                            },
                            _ => return Some(Action::None),
                        };
                        self.open_control(control, Source::Schedule(id), model, now);
                    }
                }
                _ => {}
            },
            _ => return None,
        }
        Some(Action::None)
    }
}
