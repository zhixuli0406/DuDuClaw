//! Line-oriented snapshot output. No terminal initialization or TTY requirement.
use crate::{
    Error, Result,
    client::Client,
    read_state::ReadState,
    sanitize::TerminalText,
    settings::{Settings, resolve_home},
    state::{InboxItem, Model, StateFeed},
    strings,
};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf, time::Duration};

pub struct InboxOptions {
    /// Phase 3 authenticates and checks the AI-session guard before constructing this.
    pub client: Client,
    pub home: Option<PathBuf>,
    pub follow: bool,
    pub json: bool,
}
impl InboxItem {
    pub fn key(&self) -> String {
        match self {
            Self::Approval(a) => format!("approval:{}", a.id()),
            Self::Task(t) => format!("task:{}", t.id()),
        }
    }
    pub fn title(&self) -> &TerminalText {
        match self {
            Self::Approval(a) => &a.summary,
            Self::Task(t) => &t.title,
        }
    }
    pub fn agent(&self) -> &TerminalText {
        match self {
            Self::Approval(a) => &a.agent_id,
            Self::Task(t) => &t.assigned_to,
        }
    }
    pub fn data(&self) -> Value {
        // Preserve every gateway field, including unknown fields, for JSON consumers.
        match self {
            Self::Approval(a) => a.detail.json_value().unwrap_or_else(|| json!(a)),
            Self::Task(t) => t.detail.json_value().unwrap_or_else(|| json!(t)),
        }
    }
    pub fn detail_text(&self) -> TerminalText {
        fn visible(value: &mut Value) {
            match value {
                Value::String(s) => *s = TerminalText::new(s.clone()).visible(),
                Value::Array(a) => a.iter_mut().for_each(visible),
                Value::Object(o) => {
                    let old = std::mem::take(o);
                    for (k, mut v) in old {
                        visible(&mut v);
                        o.insert(TerminalText::new(k).visible(), v);
                    }
                }
                _ => {}
            }
        }
        let mut data = self.data();
        visible(&mut data);
        TerminalText::new(serde_json::to_string_pretty(&data).unwrap_or_default())
    }
}
/// One snapshot envelope per line, including empty and stale snapshots.
/// JSON escapes controls but preserves data (including bidi characters). Consumers
/// MUST NOT write decoded fields raw to a terminal; use sanitize_for_terminal.
pub fn json_snapshot(model: &Model, read: &ReadState) -> Value {
    json!({"type":"inbox", "stale":model.inbox.count().is_none(), "count":model.inbox.count(),
        "error":model.login_required.or(model.inbox.error).map(|e| e.to_string()),
        "items": model.inbox.items.iter().map(|item| json!({"id":item.key(), "source":item.source_label(), "read":read.contains(&item.key()), "data":item.data()})).collect::<Vec<_>>()})
}
pub fn text_snapshot(model: &Model, read: &ReadState) -> String {
    let count = model
        .inbox
        .count()
        .map(|n| n.to_string())
        .unwrap_or_else(|| strings::UNKNOWN_COUNT.into());
    let mut text = format!(
        "{} ({count}){}\n",
        strings::INBOX,
        if model.inbox.count().is_none() {
            format!(" - {}", strings::STALE)
        } else {
            String::new()
        }
    );
    if let Some(error) = model.login_required.or(model.inbox.error) {
        text.push_str(&format!("{error}\n"));
    }
    if model.inbox.count() == Some(0) {
        text.push_str(strings::EMPTY_INBOX);
        text.push('\n');
    }
    for item in &model.inbox.items {
        text.push_str(&format!(
            "[{}] {}: {}{}\n",
            item.source_label(),
            item.agent().plain().replace('\n', " "),
            item.title().plain().replace('\n', " "),
            if read.contains(&item.key()) {
                format!(" ({})", strings::READ)
            } else {
                String::new()
            }
        ));
    }
    text
}
pub async fn run_inbox(options: InboxOptions) -> Result<()> {
    run_inbox_to(options, &mut std::io::stdout()).await
}
/// Writer-injected entry point for scripts, embedding, and fake-gateway tests.
pub async fn run_inbox_to<W: Write>(options: InboxOptions, output: &mut W) -> Result<()> {
    let home = resolve_home(options.home)?;
    let (settings, notice) = Settings::load(&home);
    let (read, read_notice) = ReadState::load(&home);
    for n in [notice, read_notice].into_iter().flatten() {
        eprintln!("{n}");
    }
    let mut feed = StateFeed::start(options.client, settings.poll_interval);
    // Wait for the first completed fetch, but bound an unavailable gateway.
    let _ = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let m = feed.receiver.borrow_and_update().clone();
            if m.inbox.count().is_some() || m.inbox.error.is_some() || m.login_required.is_some() {
                break;
            }
            if feed.receiver.changed().await.is_err() {
                break;
            }
        }
    })
    .await;
    let mut previous = String::new();
    loop {
        let model = feed.receiver.borrow_and_update().clone();
        let line = if options.json {
            format!("{}\n", json_snapshot(&model, &read))
        } else {
            text_snapshot(&model, &read)
        };
        if line != previous {
            output
                .write_all(line.as_bytes())
                .and_then(|_| output.flush())
                .map_err(|_| Error::Output)?;
            previous = line;
        }
        if let Some(error) = model.login_required {
            return Err(error);
        }
        if !options.follow {
            return Ok(());
        }
        tokio::select! {
            changed = feed.receiver.changed() => if changed.is_err() { return Err(Error::Disconnected); },
            signal = tokio::signal::ctrl_c() => { signal.map_err(|_| Error::Terminal)?; return Ok(()); }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_is_unknown_not_empty() {
        let model = Model::default();
        let read = ReadState::default();
        let value = json_snapshot(&model, &read);
        assert_eq!(value["stale"], true);
        assert!(value["count"].is_null());
        let text = text_snapshot(&model, &read);
        assert!(text.contains(strings::STALE));
        assert!(!text.contains(strings::EMPTY_INBOX));
    }
}
