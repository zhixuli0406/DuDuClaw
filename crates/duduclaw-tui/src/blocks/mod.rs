//! Local tiles and bounded, authoritative gateway snapshots.
pub mod fetch;
#[cfg(test)]
mod tests;
use crate::{Error, Result, sanitize::TerminalText, strings};
use ratatui::layout::Rect;
use serde_json::Value;

/// Confirms a runs.list row and runs.get response refer to the same watched run.
pub(crate) fn run_matches(list_row: &Value, detail: &Value, agent: &str, id: &str) -> bool {
    list_row["agent_id"] == agent
        && detail["run"].is_object()
        && detail["run"]["id"] == id
        && detail["run"]["agent_id"] == agent
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    #[default]
    Detail,
    Chat,
    Run,
    Task,
    Log,
    Pty,
}
impl Block {
    pub fn label(self) -> &'static str {
        match self {
            Self::Detail => strings::DETAIL,
            Self::Chat => strings::CHAT_BLOCK,
            Self::Run => strings::RUN_BLOCK,
            Self::Task => strings::TASK_BLOCK,
            Self::Log => strings::LOG_BLOCK,
            Self::Pty => strings::PTY_BLOCK,
        }
    }
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Split {
    #[default]
    Vertical,
    Horizontal,
}
#[derive(Clone)]
pub struct Pane {
    pub pty: Option<crate::pty::Shared>,
    pub kind: Block,
    pub agent: Option<String>,
    pub task_id: Option<String>,
    pub index: usize,
    pub scroll: usize,
    pub content: bool,
    pub responsibilities: bool,
    pub data: Data,
}
impl Pane {
    fn new(kind: Block) -> Self {
        Self {
            pty: (kind == Block::Pty).then(crate::pty::Session::shared),
            kind,
            agent: None,
            task_id: None,
            index: 0,
            scroll: 0,
            content: false,
            responsibilities: false,
            data: Data::default(),
        }
    }
}
#[derive(Clone)]
pub struct Blocks {
    pub active: Block,
    pub panes: Vec<Pane>,
    pub focused: usize,
    pub split: Split,
    pub revision: u64,
}
impl Default for Blocks {
    fn default() -> Self {
        Self {
            active: Block::Detail,
            panes: vec![Pane::new(Block::Detail)],
            focused: 0,
            split: Split::Vertical,
            revision: 0,
        }
    }
}
impl Blocks {
    pub fn select(&mut self, index: usize) {
        if let Some(pane) = self.panes.get(index) {
            self.focused = index;
            self.active = pane.kind;
        }
    }
    pub fn open(&mut self, kind: Block, agent: Option<String>) {
        let pane = &mut self.panes[self.focused];
        *pane = Pane::new(kind);
        pane.agent = agent;
        self.active = kind;
        self.revision += 1;
    }
    pub fn chat(&mut self) {
        if let Some(i) = self.panes.iter().position(|p| p.kind == Block::Chat) {
            self.select(i);
        } else {
            if self.panes.len() < 4 {
                self.add(Split::Vertical);
            }
            self.open(Block::Chat, None);
        }
    }
    pub fn add(&mut self, split: Split) {
        if self.panes.len() == 4 {
            return;
        }
        self.panes.push(Pane::new(Block::Detail));
        self.split = split;
        self.select(self.panes.len() - 1);
        self.revision += 1;
    }
    pub fn close(&mut self) {
        if self.panes.len() == 1 {
            self.open(Block::Detail, None);
        } else {
            self.panes.remove(self.focused);
            self.select(self.focused.min(self.panes.len() - 1));
            self.revision += 1;
        }
    }
    pub fn run_step(&mut self, payload: &Value) {
        for pane in &mut self.panes {
            if pane.kind != Block::Run || !pane.data.subscribed || pane.data.stale {
                continue;
            }
            if payload["run_id"].as_str().is_none()
                || payload["run_id"] != pane.data.selected["id"]
                || payload["agent_id"].as_str() != pane.agent.as_deref()
            {
                continue;
            }
            if let Some((_, Value::Array(events))) =
                pane.data.sections.iter_mut().find(|(k, _)| *k == "events")
            {
                if payload
                    .get("seq")
                    .is_some_and(|seq| events.iter().any(|e| e.get("seq") == Some(seq)))
                {
                    continue;
                }
                // Keep only the known wire fields, with the same bounded preview as history.
                let mut event = serde_json::Map::new();
                for key in ["kind", "ts", "agent_id", "status", "tool", "preview", "seq"] {
                    if let Some(v) = payload.get(key) {
                        event.insert(
                            key.into(),
                            if let Some(s) = v.as_str() {
                                Value::String(s.chars().take(2048).collect())
                            } else if v.is_number() {
                                v.clone()
                            } else {
                                Value::Null
                            },
                        );
                    }
                }
                events.push(Value::Object(event));
                if events.len() > 256 {
                    events.remove(0);
                }
            }
        }
    }
    pub fn invalidate(&mut self) {
        self.revision += 1;
        for pane in &mut self.panes {
            pane.data.stale = true;
        }
    }
    pub fn areas(&self, area: Rect) -> (Vec<(usize, Rect)>, usize) {
        let count = self.panes.len();
        if count == 1 {
            return (vec![(0, area)], 0);
        }
        // The caller reserves the hidden-count line. Resizing never changes state.
        if area.width < 64 || area.height < 16 {
            return (vec![(self.focused, area)], count - 1);
        }
        let mut regions = vec![area];
        while regions.len() < count {
            let i = regions
                .iter()
                .enumerate()
                .max_by_key(|(_, r)| u32::from(r.width) * u32::from(r.height))
                .map(|(i, _)| i)
                .unwrap_or(0);
            let r = regions.remove(i);
            let horizontal = if regions.is_empty() {
                self.split == Split::Vertical
            } else {
                r.width >= r.height * 3
            };
            let (a, b) = if horizontal {
                let n = r.width / 2;
                (
                    Rect::new(r.x, r.y, n, r.height),
                    Rect::new(r.x + n, r.y, r.width - n, r.height),
                )
            } else {
                let n = r.height / 2;
                (
                    Rect::new(r.x, r.y, r.width, n),
                    Rect::new(r.x, r.y + n, r.width, r.height - n),
                )
            };
            regions.insert(i, b);
            regions.insert(i, a);
        }
        (regions.into_iter().enumerate().collect(), 0)
    }
}
#[derive(Clone)]
pub struct Data {
    pub stale: bool,
    pub error: Option<TerminalText>,
    pub rows: Vec<Value>,
    pub total: usize,
    pub selected: Value,
    pub sections: Vec<(&'static str, Value)>,
    pub subscribed: bool,
    pub subscription_checked: bool,
    pub runtime: Option<TerminalText>,
}
impl Default for Data {
    fn default() -> Self {
        Self {
            stale: true,
            error: None,
            rows: vec![],
            total: 0,
            selected: Value::Null,
            sections: vec![],
            subscribed: false,
            subscription_checked: false,
            runtime: None,
        }
    }
}
impl Data {
    pub fn task(&self) -> Option<crate::state::InboxItem> {
        let mut task: crate::state::HumanTask =
            serde_json::from_value(self.selected.clone()).ok()?;
        task.detail = TerminalText::new(self.selected.to_string());
        Some(crate::state::InboxItem::Task(task))
    }
    pub fn section(&self, name: &str) -> &Value {
        self.sections
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, v)| v)
            .unwrap_or(&Value::Null)
    }
}
pub fn field(v: &Value, key: &str) -> String {
    let text = match &v[key] {
        Value::String(s) => s.clone(),
        Value::Null => strings::NOT_PROVIDED.into(),
        other => other.to_string(),
    };
    TerminalText::new(text).plain().replace('\n', " ")
}
pub fn array(v: &Value, key: &str) -> Result<Vec<Value>> {
    let mut budget = crate::budget::SnapshotBudget::default();
    budget.charge(v, key)?;
    v[key].as_array().cloned().ok_or(Error::Protocol)
}
#[derive(Default, Clone)]
pub struct LogTail {
    pub lines: std::collections::VecDeque<TerminalText>,
    pub lost: bool,
}
impl LogTail {
    pub fn push(&mut self, value: &Value) {
        let raw = value
            .as_str()
            .map(str::to_owned)
            .unwrap_or_else(|| value.to_string());
        self.lines
            .push_back(TerminalText::new(raw.chars().take(2048).collect()));
        if self.lines.len() > 256 {
            self.lines.pop_front();
            self.lost = true;
        }
    }
}
/// The block feed has its own receiver and must detect loss independently.
#[derive(Default)]
pub struct EventCursor {
    seq: Option<u64>,
    boot: Option<String>,
}
impl EventCursor {
    pub fn observe(&mut self, event: &crate::client::ServerEvent) -> bool {
        let boot_changed = event
            .boot_id
            .as_ref()
            .is_some_and(|b| self.boot.as_ref().is_some_and(|old| old != b));
        if let Some(boot) = &event.boot_id {
            self.boot = Some(boot.clone());
        }
        if boot_changed {
            self.seq = None;
        }
        let gap = event
            .seq
            .zip(self.seq)
            .is_some_and(|(new, old)| new != old && old.checked_add(1) != Some(new));
        if event.seq.is_some() {
            self.seq = event.seq;
        }
        boot_changed || gap || event.name == "events_lost"
    }
}
