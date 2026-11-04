//! Process-local notices. Remote payloads never supply notification wording.
use super::{app::{App, Selection}, text::{SafeText, middle_ellipsis}, widgets::panel_focus};
use crate::{sanitize::TerminalText, settings::Settings, state::{InboxItem, Model}};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{Frame, buffer::CellWidth, layout::Rect, widgets::Clear};
use std::{collections::{HashSet, VecDeque}, time::{Duration, Instant}};

const LIMIT: usize = 100;
const LIFETIME: Duration = Duration::from_secs(6);
pub(super) const MAIL_KEYS: &str = "Tab 篩選  ↑↓ 選取  Enter 前往  Esc 關閉";
pub(super) const TOAST_KEYS: &str = "↑↓ 選取通知  x 關閉通知  Esc 返回";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind { Done, NeedYou, Error }
impl Kind {
    fn label(self) -> &'static str {
        match self { Self::Done => "完成", Self::NeedYou => "需要你", Self::Error => "錯誤" }
    }
    fn color(self, settings: &Settings) -> ratatui::style::Color {
        match self { Self::Done => settings.colors.success, Self::NeedYou => settings.colors.warning, Self::Error => settings.colors.danger }
    }
}
#[derive(Clone)]
pub(super) struct Notice {
    pub id: u64,
    pub kind: Kind,
    pub name: TerminalText,
    pub agent: Option<String>,
    pub inbox: Option<String>,
    pub unread: bool,
    pub timestamp: String,
    created: Instant,
    dismissed: bool,
}
impl Notice {
    fn active(&self, now: Instant) -> bool {
        !self.dismissed && (self.kind == Kind::NeedYou || now.saturating_duration_since(self.created) < LIFETIME)
    }
    fn wording(&self, width: usize, settings: &Settings) -> TerminalText {
        let prefix = format!("{} · ", self.kind.label());
        let budget = width.saturating_sub(super::text::columns(&prefix, settings.ambiguous_width));
        TerminalText::new(format!("{prefix}{}", middle_ellipsis(&self.name.plain(), budget, settings.ambiguous_width)))
    }
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Filter { #[default] All, Unread, Errors }
impl Filter {
    fn accepts(self, n: &Notice) -> bool {
        match self { Self::All => true, Self::Unread => n.unread, Self::Errors => n.kind == Kind::Error }
    }
    fn next(self, backward: bool) -> Self {
        match (self, backward) {
            (Self::All, false) | (Self::Errors, true) => Self::Unread,
            (Self::Unread, false) | (Self::All, true) => Self::Errors,
            _ => Self::All,
        }
    }
}
#[derive(Default, Clone)]
pub(super) struct Mailbox {
    pub filter: Filter,
    pub selected: Option<u64>,
}
#[derive(Default, Clone)]
pub(super) struct Notices {
    pub items: VecDeque<Notice>,
    next: u64,
    inbox_keys: HashSet<String>,
    run_keys: VecDeque<String>,
    run_agents: HashSet<String>,
    breaker_keys: VecDeque<String>,
    event_boot: Option<String>,
    event_seq: Option<u64>,
}
impl Notices {
    pub fn push(&mut self, kind: Kind, name: TerminalText, agent: Option<String>, inbox: Option<String>, now: Instant) {
        self.next = self.next.wrapping_add(1);
        self.items.push_front(Notice { id: self.next, kind,
            name: TerminalText::new(name.plain().replace('\n', " ").chars().take(256).collect()),
            agent, inbox, unread: true, timestamp: chrono::Local::now().to_rfc3339(), created: now, dismissed: false });
        self.items.truncate(LIMIT);
    }
    pub fn visible(&self, now: Instant) -> Vec<&Notice> {
        self.items.iter().filter(|n| n.active(now)).take(2).collect()
    }
    pub fn filtered(&self, filter: Filter) -> Vec<&Notice> {
        self.items.iter().filter(|n| filter.accepts(n)).collect()
    }
    pub fn dismiss(&mut self, id: u64) {
        if let Some(n) = self.items.iter_mut().find(|n| n.id == id) { n.dismissed = true; }
    }
    pub fn seen(&mut self, inbox: &str) {
        for n in &mut self.items {
            if n.inbox.as_deref() == Some(inbox) { n.unread = false; n.dismissed = true; }
        }
    }
    pub fn read_all(&mut self) {
        for n in &mut self.items { n.unread = false; n.dismissed = true; }
    }
    fn remember_run(&mut self, key: String) -> bool {
        if self.run_keys.contains(&key) { return false; }
        self.run_keys.push_back(key);
        if self.run_keys.len() > 4096 { self.run_keys.pop_front(); }
        true
    }
    /// Breakers can open repeatedly with the same inbox key. A sequenced event
    /// represents a new opening; a replay does not. Match the inbox's Admin gate.
    pub fn event(&mut self, event: &crate::client::ServerEvent, model: &Model, now: Instant) {
        if model.role != Some(crate::auth::Role::Admin) { return; }
        let autopilot = match event.name.as_str() {
            "budget.breaker_open" => false,
            "autopilot.breaker_open" => true,
            _ => return,
        };
        let field = if autopilot { "rule_id" } else { "agent_id" };
        let Some(id) = event.payload[field].as_str().filter(|s| !s.is_empty() && s.len() <= 1024) else { return; };
        let key = format!("{}:{id}", event.name);
        if event.boot_id == self.event_boot && event.seq.zip(self.event_seq).is_some_and(|(new, old)| new <= old) { return; }
        if event.seq.is_none() && self.breaker_keys.contains(&key) { return; }
        if self.event_boot != event.boot_id { self.event_seq = None; }
        self.event_boot = event.boot_id.clone();
        if let Some(seq) = event.seq { self.event_seq = Some(seq); }
        self.breaker_keys.retain(|old| old != &key);
        self.breaker_keys.push_back(key.clone());
        if self.breaker_keys.len() > 128 { self.breaker_keys.pop_front(); }
        let employee = (!autopilot).then(|| model.employees.items.iter().find(|e| e.id() == id)).flatten();
        let name = employee.map(|e| TerminalText::new(super::workspace::name(e))).unwrap_or_else(|| TerminalText::new("系統".into()));
        self.push(Kind::Error, name, employee.map(|e| e.id().to_owned()), Some(key), now);
    }
    /// Existing event-invalidated snapshots are authoritative. A turn-finished
    /// event has no outcome, so only runs.list results may report success/error.
    pub fn observe(&mut self, model: &Model, now: Instant) {
        if model.inbox.count().is_some() {
            let keys: HashSet<_> = model.inbox.items.iter().map(InboxItem::key).collect();
            for item in &model.inbox.items {
                let key = item.key();
                if self.inbox_keys.contains(&key) || self.breaker_keys.contains(&key) { continue; }
                let employee = model.employees.items.iter().find(|e| item.agent().is(e.id()));
                let agent = employee.map(|e| e.id().to_owned());
                let name = employee.map(|e| TerminalText::new(super::workspace::name(e)))
                    .unwrap_or_else(|| TerminalText::new("系統".into()));
                let kind = if matches!(item, InboxItem::Breaker(_)) { Kind::Error } else { Kind::NeedYou };
                self.push(kind, name, agent, Some(key), now);
            }
            self.inbox_keys = keys;
        }
        if model.employees.count().is_some() {
            self.run_agents.retain(|id| model.employees.items.iter().any(|e| e.id() == id));
            for employee in &model.employees.items {
                if let Some(activity) = model.recent_activity.get(employee.id()) {
                    let seeded = !self.run_agents.insert(employee.id().into());
                    for (key, failed) in activity.finished_runs.iter().rev() {
                        if self.remember_run(key.clone()) && seeded {
                            self.push(if *failed { Kind::Error } else { Kind::Done },
                                TerminalText::new(super::workspace::name(employee)), Some(employee.id().into()), None, now);
                        }
                    }
                }
            }
        }
    }
}
impl App {
    pub(super) fn observe_notices(&mut self, model: &Model, now: Instant) {
        self.notices.observe(model, now);
        // Looking at the matching decision card acknowledges the notification,
        // but does not write a decision or change the persisted inbox read state.
        if let Some(card) = &self.card { self.notices.seen(&card.item.key()); }
        for item in &model.inbox.items {
            if self.read.contains(&item.key()) { self.notices.seen(&item.key()); }
        }
        // Keep explicit toast focus even if its row expires: a queued x must
        // never fall through to split the underlying workspace.
    }
    /// Called only after card/input/PTY guards. Arrival never changes focus.
    pub(super) fn notices_key(&mut self, key: KeyEvent, model: &Model, now: Instant) -> bool {
        if let Some(mail) = &mut self.mailbox {
            let rows = self.notices.filtered(mail.filter);
            let index = mail.selected.and_then(|id| rows.iter().position(|n| n.id == id)).unwrap_or(0);
            match key.code {
                KeyCode::Esc | KeyCode::Char('M') => self.mailbox = None,
                KeyCode::Tab | KeyCode::BackTab => {
                    mail.filter = mail.filter.next(key.code == KeyCode::BackTab);
                    mail.selected = self.notices.filtered(mail.filter).first().map(|n| n.id);
                }
                KeyCode::Up | KeyCode::Down => {
                    let index = if key.code == KeyCode::Up { index.saturating_sub(1) } else { (index + 1).min(rows.len().saturating_sub(1)) };
                    mail.selected = rows.get(index).map(|n| n.id);
                }
                KeyCode::Enter => {
                    let target = rows.get(index).map(|n| (n.id, n.inbox.clone(), n.agent.clone()));
                    if let Some((id, inbox, agent)) = target {
                        if let Some(n) = self.notices.items.iter_mut().find(|n| n.id == id) { n.unread = false; n.dismissed = true; }
                        if let Some(key) = inbox.filter(|key| model.inbox.items.iter().any(|i| i.key() == *key)) {
                            self.zoomed = false;
                            self.main_view = super::workspace::MainView::Inbox;
                            self.selected = Some(Selection::Inbox(key));
                            self.detail_open = false;
                            self.mailbox = None;
                        } else if let Some(agent) = agent.filter(|id| model.employees.items.iter().any(|e| e.id() == id)) {
                            self.selected = Some(Selection::Employee(agent));
                            self.open_workspace(model, 0);
                            self.mailbox = None;
                        } else { self.notice = Some("通知對應的項目目前不在清單中".into()); }
                    }
                }
                _ => {}
            }
            return true;
        }
        if let Some(id) = self.toast_focus {
            match key.code {
                KeyCode::Esc | KeyCode::F(8) => self.toast_focus = None,
                KeyCode::Char('x') => { self.notices.dismiss(id); self.toast_focus = None; }
                KeyCode::Up | KeyCode::Down => {
                    let rows = self.notices.visible(now);
                    let index = rows.iter().position(|n| n.id == id).unwrap_or(0);
                    let index = if key.code == KeyCode::Up { index.saturating_sub(1) } else { (index + 1).min(rows.len().saturating_sub(1)) };
                    self.toast_focus = rows.get(index).map(|n| n.id);
                }
                KeyCode::Char('M') => { self.toast_focus = None; self.open_mailbox(); }
                _ => {}
            }
            return true;
        }
        match key.code {
            KeyCode::Char('M') => self.open_mailbox(),
            KeyCode::F(8) => self.toast_focus = self.notices.visible(now).first().map(|n| n.id),
            _ => return false,
        }
        true
    }
    fn open_mailbox(&mut self) {
        self.notices.read_all();
        self.mailbox = Some(Mailbox { filter: Filter::All, selected: self.notices.items.front().map(|n| n.id) });
    }
}
pub(super) fn mailbox(frame: &mut Frame, area: Rect, app: &mut App, settings: &Settings) {
    let Some(mail) = &mut app.mailbox else { return; };
    let inner = panel_focus(frame, area, "通知信箱", settings, false, true);
    if inner.is_empty() { return; }
    let tabs = [Filter::All, Filter::Unread, Filter::Errors].into_iter().zip(["全部", "未讀", "錯誤"])
        .map(|(filter, label)| if filter == mail.filter { format!("[{label}]") } else { label.into() }).collect::<Vec<_>>().join("  ");
    line(frame, Rect::new(inner.x, inner.y, inner.width, 1), &TerminalText::new(tabs), settings, settings.colors.emphasis(settings.colors.accent));
    let rows = app.notices.filtered(mail.filter);
    let selected = mail.selected.and_then(|id| rows.iter().position(|n| n.id == id)).unwrap_or(0);
    mail.selected = rows.get(selected).map(|n| n.id);
    let height = usize::from(inner.height.saturating_sub(1));
    let offset = (selected + 1).saturating_sub(height);
    if rows.is_empty() && inner.height > 1 {
        line(frame, Rect::new(inner.x, inner.y + 1, inner.width, 1), &TerminalText::new("目前沒有符合篩選的通知".into()), settings, settings.colors.muted());
    }
    for (index, n) in rows.iter().enumerate().skip(offset).take(height) {
        let prefix = format!("{} {} {} ", if Some(n.id) == mail.selected { ">" } else { " " }, if n.unread { "*" } else { " " }, crate::human_time::format(&n.timestamp));
        let budget = usize::from(inner.width).saturating_sub(super::text::columns(&prefix, settings.ambiguous_width));
        let value = TerminalText::new(format!("{prefix}{}", n.wording(budget, settings).plain()));
        line(frame, Rect::new(inner.x, inner.y + 1 + (index - offset) as u16, inner.width, 1), &value, settings,
            if Some(n.id) == mail.selected { settings.colors.selected() } else { settings.colors.emphasis(n.kind.color(settings)) });
    }
}
pub(super) fn toasts(frame: &mut Frame, area: Rect, app: &App, settings: &Settings, now: Instant) {
    // Never cover decision/review cards, a palette, or the notification mailbox.
    if app.card.is_some() || app.control_card.is_some() || app.palette.is_some() || app.mailbox.is_some()
        || app.content_warning || app.pty_confirm.is_some() || app.help { return; }
    let rows = app.notices.visible(now);
    let count = rows.len().min(usize::from(area.height / 3));
    let width = area.width.min(48);
    for (index, n) in rows.into_iter().take(count).enumerate() {
        let mut rect = Rect::new(area.right() - width, area.bottom() - (count as u16 * 3) + index as u16 * 3, width, 3);
        // Clear the whole underlying glyph when the overlay starts on its tail.
        // Otherwise a CJK lead cell just outside Clear can corrupt terminal columns.
        let mut left = rect.x;
        for y in rect.y..rect.bottom() {
            let mut x = area.x;
            while x < rect.x {
                let end = x.saturating_add(frame.buffer_mut()[(x, y)].cell_width().max(1));
                if end > rect.x { left = left.min(x); break; }
                x = end;
            }
        }
        rect.width += rect.x - left; rect.x = left;
        frame.render_widget(Clear, rect);
        let focused = app.toast_focus == Some(n.id);
        let inner = panel_focus(frame, rect, if focused { "通知 · x 關閉" } else { "通知 · F8 選取" }, settings, false, focused);
        line(frame, inner, &n.wording(usize::from(inner.width), settings), settings, settings.colors.emphasis(n.kind.color(settings)));
    }
}
fn line(frame: &mut Frame, area: Rect, value: &TerminalText, settings: &Settings, style: ratatui::style::Style) {
    frame.render_widget(SafeText { text: value, width: settings.ambiguous_width, visible: false, wrap: false, scroll: 0, style }, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifetime_cap_persistence_and_dismissal() {
        let now = Instant::now();
        let mut notices = Notices::default();
        notices.push(Kind::NeedYou, TerminalText::new("阿嘟".into()), None, Some("approval:1".into()), now);
        notices.push(Kind::Done, TerminalText::default(), None, None, now);
        notices.push(Kind::Error, TerminalText::default(), None, None, now);
        assert_eq!(notices.visible(now).len(), 2);
        assert_eq!(notices.visible(now)[0].kind, Kind::Error);
        assert_eq!(notices.visible(now + Duration::from_millis(5999)).len(), 2);
        assert_eq!(notices.visible(now + LIFETIME).len(), 1);
        assert_eq!(notices.visible(now + Duration::from_secs(9999))[0].kind, Kind::NeedYou);
        notices.seen("approval:1");
        assert!(notices.visible(now + LIFETIME).is_empty());
        assert!(!notices.items.back().unwrap().unread);
        notices.dismiss(notices.items[0].id);
        assert_eq!(notices.visible(now).len(), 1);
    }
    #[test]
    fn filters_bounded_history_and_open_acknowledgement() {
        let now = Instant::now();
        let mut n = Notices::default();
        for _ in 0..105 { n.push(Kind::Done, TerminalText::default(), None, None, now); }
        assert_eq!(n.items.len(), 100);
        assert_eq!(n.items.back().unwrap().id, 6);
        n.push(Kind::Error, TerminalText::default(), None, None, now);
        assert_eq!(n.filtered(Filter::Errors).len(), 1);
        assert_eq!(n.filtered(Filter::Unread).len(), 100);
        n.read_all();
        assert!(n.filtered(Filter::Unread).is_empty());
        assert!(n.visible(now).is_empty());
        assert_eq!(n.filtered(Filter::All).len(), 100);
    }
    #[test]
    fn hostile_names_are_bounded_safe_and_width_aware() {
        let now = Instant::now();
        let mut n = Notices::default();
        n.push(Kind::NeedYou, TerminalText::new(format!("\x1b\u{7}\u{202e}\n{}", "界Ω".repeat(2000))), None, None, now);
        assert_eq!(n.items[0].name.plain().chars().count(), 256);
        for width in [crate::settings::AmbiguousWidth::Narrow, crate::settings::AmbiguousWidth::Wide] {
            let mut settings = Settings::test_defaults(); settings.ambiguous_width = width;
            let text = n.items[0].wording(30, &settings).plain();
            assert!(super::super::text::columns(&text, width) <= 30);
            assert!(!text.chars().any(crate::sanitize::forbidden));
            assert!(!text.contains('\n'));
        }
    }
}
