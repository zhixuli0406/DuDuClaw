//! A terminal-independent decision state machine. Both clocks are injected.
use crate::{decide::{Decision, GoalAction}, sanitize::TerminalText, state::{InboxItem, Model}, strings};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rand::Rng;
use std::time::{Duration, Instant};

#[derive(Debug, PartialEq, Eq)]
pub enum Effect { None, Close, Quit, Snooze, Send(Decision) }
#[derive(Clone)]
pub struct Card {
    pub item: InboxItem,
    pub body: TerminalText,
    pub high_risk: bool,
    pub scroll: usize,
    pub code: String,
    pub input: String,
    pub blocked: Option<TerminalText>,
    pub hint: Option<&'static str>,
    opened: Instant,
    bottom_seen: bool,
    max_scroll: usize,
    page: usize,
    viewport_ready: bool,
    armed: Option<GoalAction>,
    sending: bool,
    message: String,
    editing: bool,
}
/// Each control room owns its opening sequence; concurrent clients/tests cannot
/// replace its previous code between two openings.
#[derive(Clone, Default)]
pub struct Codes { previous: Option<u8> }
impl Codes {
    fn next(&mut self) -> String {
        let mut rng = rand::thread_rng();
        let mut next = rng.gen_range(0..100);
        while self.previous == Some(next) { next = rng.gen_range(0..100); }
        self.previous = Some(next);
        format!("{next:02}")
    }
}
impl Card {
    pub fn open(item: InboxItem, model: &Model, now: Instant, codes: &mut Codes) -> Self {
        let blocked = super::card_data::blocked(&item, model);
        let high_risk = matches!(&item, InboxItem::Approval(_)) && super::card_data::high_risk(&item.data());
        let body = item.detail_text();
        Self { item, body, high_risk, scroll: 0, code: codes.next(), input: String::new(),
            blocked, hint: None, opened: now, bottom_seen: false, max_scroll: 0, page: 1, viewport_ready: false,
            armed: None, sending: false, message: String::new(), editing: false }
    }
    /// Called after rendering the full, visibly sanitized body. Resize re-locks
    /// input until the newly wrapped bottom is visible (or explicitly scrolled).
    pub fn viewport(&mut self, lines: usize, height: usize) {
        self.viewport_ready = height > 0;
        self.page = height.max(1);
        self.max_scroll = lines.saturating_sub(height);
        self.scroll = self.scroll.min(self.max_scroll);
        self.bottom_seen = height > 0 && self.scroll >= self.max_scroll;
        if !self.bottom_seen { self.input.clear(); }
    }
    pub fn invalidate_viewport(&mut self) {
        self.input.clear();
        self.message.clear();
        self.armed = None;
        self.bottom_seen = false;
        self.viewport_ready = false;
    }
    fn digest(&self) -> Option<String> {
        self.item.data().get("digest").and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty()).map(str::to_owned)
    }
    pub fn expired(&self, epoch: i64) -> bool {
        matches!(&self.item, InboxItem::Approval(a) if a.expires_at.is_some_and(|e| epoch >= e))
    }
    fn approval_live(&self, epoch: i64) -> bool {
        matches!(&self.item, InboxItem::Approval(a) if a.status.is("pending") && a.expires_at.is_some_and(|e| epoch < e))
    }
    fn continuing(&self) -> bool {
        matches!(&self.item, InboxItem::Task(t) if t.status.is("done") || t.status.is("failed") || t.status.is("cancelled"))
    }
    pub fn key(&mut self, key: KeyEvent, now: Instant, epoch: i64, fresh: bool) -> Effect {
        if now.saturating_duration_since(self.opened) < Duration::from_secs(1)
            || key.kind != KeyEventKind::Press || self.sending { return Effect::None; }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Effect::Quit;
        }
        if key.modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
            self.armed = None;
            return Effect::None;
        }
        if key.code == KeyCode::Esc {
            if self.editing { self.editing = false; return Effect::None; }
            return Effect::Close;
        }
        if self.editing {
            match key.code {
                KeyCode::Char(c) if self.message.chars().count() < 2000 => self.message.push(c),
                KeyCode::Backspace => { self.message.pop(); },
                _ => {}
            }
            return Effect::None;
        }
        match key.code {
            KeyCode::PageDown | KeyCode::Down => {
                self.scroll = self.scroll.saturating_add(if key.code == KeyCode::Down { 1 } else { self.page }).min(self.max_scroll);
                self.bottom_seen = self.viewport_ready && self.scroll >= self.max_scroll;
                self.armed = None; self.hint = None;
                return Effect::None;
            }
            KeyCode::PageUp | KeyCode::Up => {
                self.scroll = self.scroll.saturating_sub(if key.code == KeyCode::Up { 1 } else { self.page });
                self.bottom_seen = self.viewport_ready && self.scroll >= self.max_scroll;
                self.input.clear(); self.armed = None; self.hint = None;
                return Effect::None;
            }
            _ => {}
        }
        if self.expired(epoch) { self.hint = Some(strings::CARD_EXPIRED); return Effect::None; }
        if key.code == KeyCode::Char('s') {
            return Effect::Snooze;
        }
        if !fresh { self.armed = None; self.input.clear(); self.hint = Some(strings::STALE); return Effect::None; }
        if self.blocked.is_some() { return Effect::None; }
        let decision = match &self.item {
            InboxItem::Approval(a) if self.approval_live(epoch) => {
                if self.digest().is_none() && key.code != KeyCode::Char('n') {
                    self.input.clear(); self.hint = Some(strings::CARD_UNBOUND);
                    return Effect::None;
                }
                let approve = match key.code {
                    KeyCode::Char('n') => Some(false),
                    KeyCode::Char(c) if c.is_ascii_digit() => {
                        if self.high_risk && !self.bottom_seen { self.hint = Some(strings::CARD_SCROLL); }
                        else if self.input.len() < 3 { self.input.push(c); self.hint = None; }
                        None
                    }
                    KeyCode::Backspace => { self.input.pop(); None },
                    KeyCode::Enter if !self.high_risk || self.bottom_seen => {
                        if self.input == self.code { Some(true) }
                        else { self.input.clear(); self.hint = Some(strings::CARD_WRONG_CODE); None }
                    }
                    _ => { self.input.clear(); None },
                };
                approve.map(|approve| Decision::Approval { id: a.id().into(), approve,
                    digest: self.digest() })
            }
            InboxItem::Task(t) => {
                let action = match key.code {
                    KeyCode::Char('r') if t.status.is("needs_human") => Some(GoalAction::Retry),
                    KeyCode::Char('a') if t.status.is("needs_human") => Some(GoalAction::Abort),
                    KeyCode::Char('d') if t.status.is("needs_human") => Some(GoalAction::Done),
                    KeyCode::Char('t') if t.status.is("needs_human") => Some(GoalAction::Takeover),
                    KeyCode::Char('i') if self.continuing() => { self.editing = true; None },
                    KeyCode::Char('c') if self.continuing() && !self.message.trim().is_empty() => Some(GoalAction::Continue),
                    _ => None,
                };
                if let Some(action) = action {
                    if action.terminal() && self.armed != Some(action) {
                        self.armed = Some(action); self.hint = Some(strings::CARD_SECOND_PRESS); None
                    } else {
                        let data = self.item.data();
                        let Some(revision) = data.get("updated_at").and_then(serde_json::Value::as_str).filter(|s| !s.is_empty()) else {
                            self.hint = Some(strings::CARD_NO_REVISION);
                            return Effect::None;
                        };
                        Some(Decision::Goal { task_id: t.id().into(), action, message: self.message.trim().into(), expected_updated_at: revision.into() })
                    }
                } else { self.armed = None; self.hint = None; None }
            }
            _ => None,
        };
        if let Some(decision) = decision { self.sending = true; Effect::Send(decision) }
        else { Effect::None }
    }
    pub fn controls(&self, now: Instant, epoch: i64, fresh: bool) -> TerminalText {
        let controls = if self.expired(epoch) { strings::CARD_EXPIRED.into() }
            else if let Some(reason) = &self.blocked { reason.visible() }
            else if !fresh { strings::STALE.into() }
            else if now.saturating_duration_since(self.opened) < Duration::from_secs(1) { strings::CARD_WAIT.into() }
            else { match &self.item {
                InboxItem::Approval(_) if !self.approval_live(epoch) => strings::CARD_NO_DEADLINE.into(),
                InboxItem::Approval(_) if self.digest().is_none() => strings::CARD_DENY_KEY.into(),
                InboxItem::Approval(_) if self.high_risk && !self.bottom_seen => strings::CARD_SCROLL.into(),
                InboxItem::Approval(_) => format!("{} {}  [{}]\n{}", strings::CARD_CODE, self.code, self.input, strings::CARD_APPROVAL_KEYS),
                InboxItem::Task(_) if self.continuing() => format!("{}\n{}", strings::CARD_CONTINUE_KEYS, TerminalText::new(self.message.clone()).visible()),
                InboxItem::Task(t) if t.status.is("needs_human") => strings::CARD_GOAL_KEYS.into(),
                _ => strings::CARD_NOT_PENDING.into(),
            }};
        let unbound = matches!(&self.item, InboxItem::Approval(_)) && self.digest().is_none();
        let controls = if unbound { format!("{}\n{controls}", strings::CARD_UNBOUND) } else { controls };
        let hint = self.hint.filter(|hint| !unbound || *hint != strings::CARD_UNBOUND);
        TerminalText::new(format!("{controls}\n{}\n{}", hint.unwrap_or(""),
            if self.expired(epoch) || matches!(&self.item, InboxItem::Approval(a) if a.expires_at.is_none()) {
                strings::CARD_NAV_NO_SNOOZE
            } else { strings::CARD_NAV }))
    }
}
