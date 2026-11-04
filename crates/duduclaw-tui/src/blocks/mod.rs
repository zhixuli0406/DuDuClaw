//! Local tiles and bounded, authoritative gateway snapshots.
pub mod fetch;
pub mod autonomy;
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
    Workflow,
    Autopilot,
    Evolution,
    Budget,
    Digest,
}
impl Block {
    pub fn remote(self) -> bool {
        !matches!(self, Self::Detail | Self::Chat | Self::Pty)
    }
    pub fn autonomy(self) -> bool {
        matches!(self, Self::Workflow | Self::Autopilot | Self::Evolution | Self::Budget | Self::Digest)
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Detail => strings::DETAIL,
            Self::Chat => strings::CHAT_BLOCK,
            Self::Run => strings::RUN_BLOCK,
            Self::Task => strings::TASK_BLOCK,
            Self::Log => strings::LOG_BLOCK,
            Self::Pty => strings::PTY_BLOCK,
            Self::Workflow => strings::WORKFLOW_BLOCK,
            Self::Autopilot => strings::AUTOPILOT_BLOCK,
            Self::Evolution => strings::EVOLUTION_BLOCK,
            Self::Budget => strings::BUDGET_BLOCK,
            Self::Digest => strings::DIGEST_BLOCK,
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
    pub id: String,
    pub workspace_id: String,
    pub chat_view: crate::ui::app::ChatView,
    pub pty: Option<crate::pty::Shared>,
    pub pty_area: Rect,
    pub kind: Block,
    pub agent: Option<String>,
    pub task_id: Option<String>,
    pub index: usize,
    pub scroll: usize,
    pub content: bool,
    pub responsibilities: bool,
    pub data: Data,
    pub(crate) timeline: crate::ui::timeline::View,
}
impl Pane {
    fn new(kind: Block) -> Self {
        let id = crate::chat::nonce();
        Self {
            workspace_id: id.clone(),
            id,
            pty_area: Rect::default(),
            chat_view: crate::ui::app::ChatView::default(),
            pty: (kind == Block::Pty).then(crate::pty::Session::shared),
            kind,
            agent: None,
            task_id: None,
            index: 0,
            scroll: 0,
            content: false,
            responsibilities: false,
            data: Data::default(),
            timeline: crate::ui::timeline::View::default(),
        }
    }
}
#[derive(Clone)]
pub struct Blocks {
    pub active: Block,
    pub panes: Vec<Pane>,
    pub parked: Vec<Pane>,
    pub focused: usize,
    pub split: Split,
    pub revision: u64,
}
impl Default for Blocks {
    fn default() -> Self {
        Self {
            active: Block::Detail,
            panes: vec![Pane::new(Block::Detail)],
            parked: vec![],
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
    fn reserve_chat(&mut self) -> bool {
        if self.chat_ids().len() < 4 { return true; }
        // Only discard never-used hidden chats. Drafts, transcripts and live turns stay.
        if let Some(i) = self.parked.iter().position(Self::unused_chat) {
            self.parked.remove(i);
            return true;
        }
        false
    }
    fn unused_chat(pane: &Pane) -> bool {
        pane.kind == Block::Chat && pane.chat_view.input.is_empty()
            && pane.chat_view.state.lines.is_empty() && !pane.chat_view.state.running
            && pane.chat_view.state.msg_id.is_none()
    }
    pub fn open(&mut self, kind: Block, agent: Option<String>) {
        if kind == Block::Chat && self.active != Block::Chat && !self.reserve_chat() { return; }
        let pane = &mut self.panes[self.focused];
        let workspace_id = pane.workspace_id.clone();
        *pane = Pane::new(kind);
        pane.workspace_id = workspace_id;
        pane.agent = agent;
        self.active = kind;
        self.revision += 1;
    }
    /// Switch a workspace tab without discarding another tab's draft or running chat.
    pub fn workspace(&mut self, kind: Block, agent: String) -> bool {
        let current = &self.panes[self.focused];
        if current.kind == kind && current.agent.as_deref() == Some(&agent) { return true; }
        let workspace_id = current.workspace_id.clone();
        let cached = self.parked.iter().rposition(|p| p.workspace_id == workspace_id && p.kind == kind && p.agent.as_deref() == Some(&agent));
        if cached.is_none() && kind == Block::Chat
            && !Self::unused_chat(&self.panes[self.focused]) && !self.reserve_chat() {
            return false;
        }
        let mut next = cached.map(|i| self.parked.remove(i)).unwrap_or_else(|| Pane::new(kind));
        next.workspace_id = workspace_id;
        next.agent = Some(agent);
        if next.kind.remote() { next.data.stale = true; }
        let old = std::mem::replace(&mut self.panes[self.focused], next);
        if matches!(old.kind, Block::Chat | Block::Task | Block::Run) && !Self::unused_chat(&old) {
            if self.parked.len() >= 12
                && let Some(i) = self.parked.iter().position(|p| p.kind != Block::Chat) {
                self.parked.remove(i);
            }
            self.parked.push(old);
        }
        self.active = kind;
        self.revision += 1;
        true
    }
    pub fn chat_ids(&self) -> Vec<String> {
        self.panes.iter().chain(&self.parked).filter(|p| p.kind == Block::Chat).map(|p| p.id.clone()).collect()
    }
    pub fn chat(&mut self) {
        if self.active == Block::Chat { return; }
        if let Some(i) = self.panes.iter().position(|p| p.kind == Block::Chat) { self.select(i); }
        else if self.panes.len() < 4 && self.reserve_chat() { self.add(Split::Vertical); self.open(Block::Chat, None); }
    }
    pub fn new_chat(&mut self) {
        if !self.reserve_chat() { return; }
        if self.active == Block::Detail && !self.parked.iter().any(|p| p.workspace_id == self.panes[self.focused].workspace_id && p.kind == Block::Chat) {
            self.open(Block::Chat, None);
        } else if self.panes.len() < 4 {
            self.add(Split::Vertical);
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
        let workspace_id = self.panes[self.focused].workspace_id.clone();
        if self.panes.len() == 1 {
            let agent = self.panes[self.focused].agent.clone();
            self.parked.retain(|pane| pane.workspace_id != workspace_id || pane.agent != agent);
            self.open(Block::Detail, None);
        } else {
            self.parked.retain(|pane| pane.workspace_id != workspace_id);
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
                for key in ["kind", "ts", "agent_id", "status", "tool", "label", "preview", "seq", "ok"] {
                    if let Some(v) = payload.get(key) {
                        event.insert(
                            key.into(),
                            if let Some(s) = v.as_str() {
                                Value::String(s.chars().take(2048).collect())
                            } else if v.is_number() || v.is_boolean() {
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
        for pane in self.panes.iter_mut().chain(&mut self.parked) {
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
    if crate::human_time::is_time_field(key) {
        return TerminalText::new(crate::human_time::field(key, &v[key])).plain();
    }
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
            .unwrap_or_else(|| crate::human_time::metadata_json(value).to_string());
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
