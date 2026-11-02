use super::app::{App, Selection};
use crate::{chat::Focus, state::Model};
impl App {
    pub(super) fn navigate_hint(&mut self, payload: &serde_json::Value, model: &Model) {
        if self.focus == Focus::Input || self.pty_focus || self.pty_confirm.is_some()
            || self.card.is_some() || self.control_card.is_some() || self.palette.is_some() || self.help { return; }
        let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) else { return; };
        if path.len() > 512 || path.chars().any(crate::sanitize::forbidden) { return; }
        let target = if let Some(id) = path.strip_prefix("/inbox?item=") {
            if id.is_empty() || id.contains(['&', '#', '%', '/', '?', '\\']) { return; }
            model.inbox.items.iter().find(|item| item.key() == id || match item { crate::state::InboxItem::Approval(a) => a.id() == id, crate::state::InboxItem::Task(t) => t.id() == id, _ => false })
                .map(|item| Selection::Inbox(item.key()))
        } else if let Some(id) = path.strip_prefix("/agents/") {
            model.employees.items.iter().find(|employee| employee.id() == id)
                .map(|employee| Selection::Employee(employee.id().into()))
        } else { None };
        if let Some(target) = target { self.selected = Some(target); self.detail_scroll = 0; }
    }
}
