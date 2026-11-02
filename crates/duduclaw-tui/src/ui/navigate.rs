use super::app::App;
use crate::{chat::Focus, state::Model, strings};
impl App {
    pub(super) fn navigate_hint(&mut self, payload: &serde_json::Value, model: &Model) {
        if self.focus == Focus::Input || self.pty_focus || self.pty_confirm.is_some()
            || self.card.is_some() || self.control_card.is_some() || self.palette.is_some() || self.help { return; }
        let Some(path) = payload.get("path").and_then(serde_json::Value::as_str) else { return; };
        if path.len() > 512 || path.chars().any(crate::sanitize::forbidden) { return; }
        let Some(id) = path.strip_prefix("/inbox?item=") else { return; };
        if id.is_empty() || id.contains(['&', '#', '%', '/', '?', '\\']) { return; }
        let known = model.inbox.items.iter().any(|item| item.key() == id || match item {
            crate::state::InboxItem::Approval(a) => a.id() == id,
            crate::state::InboxItem::Task(t) => t.id() == id,
            _ => false,
        });
        // An unsolicited hint must not retarget a queued operator keystroke.
        if known { self.notice = Some(strings::NAVIGATE_INBOX_HINT.into()); }
    }
}
