//! One WebChat transport per RPC client; transcript data is never terminal code.
#[cfg(test)]
mod tests;
mod transport;
use crate::{sanitize::TerminalText, strings};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
pub use transport::ChatClient;
pub(crate) use transport::Credentials;

pub const MAX_LINES: usize = 2_000;
const MAX_LINE_CHARS: usize = 512;
pub const MAX_INPUT_CHARS: usize = 8_000;

/// Additive fields deliberately do not require the unmerged concurrency PR.
#[derive(Clone, Debug, Deserialize)]
pub struct Frame {
    #[serde(rename = "type")]
    pub kind: String,
    pub content: Option<String>,
    pub message: Option<String>,
    pub code: Option<String>,
    pub phase: Option<String>,
    pub tool: Option<String>,
    pub summary: Option<String>,
    pub line: Option<String>,
    pub session_id: Option<String>,
    pub conv: Option<String>,
    pub msg_id: Option<String>,
    #[serde(default)]
    pub refresh: bool,
}
impl Frame {
    pub fn parse(wire: &str) -> crate::Result<Self> {
        if wire.len() > crate::client::MAX_FRAME_BYTES {
            return Err(crate::Error::Protocol);
        }
        serde_json::from_str(wire).map_err(|_| crate::Error::Protocol)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submission {
    pub agent: String,
    pub content: String,
}
pub fn submission(input: &str, selected: Option<&str>) -> Option<Submission> {
    let input = input.trim();
    let (agent, content) = if let Some(rest) = input.strip_prefix('@') {
        let split = rest.find(char::is_whitespace)?;
        (&rest[..split], rest[split..].trim_start())
    } else {
        (selected?, input)
    };
    if agent.is_empty() || content.is_empty() || input.chars().count() > MAX_INPUT_CHARS {
        return None;
    }
    Some(Submission {
        agent: agent.into(),
        content: content.into(),
    })
}
pub fn command_note(input: &str) -> Option<&'static str> {
    let content = submission(input, Some("selected"))?.content;
    matches!(
        content.split_whitespace().next(),
        Some("/pause" | "/stop" | "/resume")
    )
    .then_some(strings::COMPUTER_COMMAND_NOTE)
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Focus {
    #[default]
    List,
    Input,
}
#[derive(Default, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    #[default]
    Detail,
    Chat,
}
/// M1 has two alternatives in one slot. M2 can expand the slots without
/// teaching the input state machine about a particular pane layout.
#[derive(Default, Clone)]
pub struct Blocks {
    pub active: Block,
}
impl Blocks {
    pub fn select(&mut self, index: usize) {
        const BLOCKS: [Block; 2] = [Block::Detail, Block::Chat];
        if let Some(block) = BLOCKS.get(index) {
            self.active = *block;
        }
    }
}
#[derive(Clone, Serialize)]
pub(crate) struct SendFrame {
    #[serde(rename = "type")]
    kind: &'static str,
    content: String,
    agent: String,
    conv: String,
    msg_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}
#[derive(Clone)]
pub struct ChatState {
    pub lines: VecDeque<TerminalText>,
    pub status: TerminalText,
    pub connected: bool,
    pub stale: bool,
    pub running: bool,
    pub cancel_supported: bool,
    pub agent: Option<String>,
    pub conv: String,
    pub msg_id: Option<String>,
    session_id: Option<String>,
    base_session: Option<String>,
    counter: u64,
}
impl Default for ChatState {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            status: TerminalText::new(strings::CONNECTING.into()),
            connected: false,
            stale: true,
            running: false,
            cancel_supported: false,
            agent: None,
            conv: nonce(),
            msg_id: None,
            session_id: None,
            base_session: None,
            counter: 0,
        }
    }
}
fn nonce() -> String {
    format!("tui-{:016x}", rand::random::<u64>())
}
impl ChatState {
    pub fn append(&mut self, label: &str, content: &str) {
        // Limit both physical lines and line length, including a single huge line.
        let safe = TerminalText::new(format!("{label}: {content}")).plain();
        for line in safe.split('\n') {
            let mut part = String::new();
            let mut count = 0;
            for c in line.chars() {
                part.push(c);
                count += 1;
                if count == MAX_LINE_CHARS {
                    self.push_line(std::mem::take(&mut part));
                    count = 0;
                }
            }
            if !part.is_empty() || line.is_empty() {
                self.push_line(part);
            }
        }
    }
    fn push_line(&mut self, line: String) {
        self.lines.push_back(TerminalText::new(line));
        while self.lines.len() > MAX_LINES {
            self.lines.pop_front();
        }
    }
    pub fn disconnected(&mut self) {
        if self.running {
            self.append(strings::CHAT_SYSTEM, strings::CHAT_INTERRUPTED);
        }
        self.connected = false;
        self.stale = true;
        self.running = false;
        self.cancel_supported = false;
        self.msg_id = None;
        self.status = TerminalText::new(strings::DISCONNECTED.into());
    }
    pub(crate) fn prepare(&mut self, input: Submission) -> crate::Result<SendFrame> {
        if !self.connected {
            return Err(crate::Error::Disconnected);
        }
        if self.running {
            return Err(crate::Error::Busy);
        }
        if self.agent.as_deref() != Some(&input.agent) {
            self.conv = nonce();
            self.session_id = None;
        }
        self.agent = Some(input.agent.clone());
        self.counter = self.counter.wrapping_add(1);
        let msg_id = format!("{}-{}", self.conv, self.counter);
        self.msg_id = Some(msg_id.clone());
        self.running = true;
        self.status = TerminalText::new(strings::WORKING.into());
        self.append(
            &format!("{} @{}", strings::CHAT_USER, input.agent),
            &input.content,
        );
        Ok(SendFrame {
            kind: "user_message",
            content: input.content,
            agent: input.agent,
            conv: self.conv.clone(),
            msg_id,
            session_id: self.session_id.clone(),
        })
    }
    pub fn can_cancel(&self) -> bool {
        self.connected && self.running && self.cancel_supported && self.msg_id.is_some()
    }
    pub(crate) fn cancel_frame(&self) -> Option<serde_json::Value> {
        self.can_cancel().then(|| {
            serde_json::json!({"type":"cancel", "msg_id":self.msg_id,
            "conv":self.conv, "session_id":self.session_id})
        })
    }
    pub fn apply(&mut self, frame: Frame) {
        if frame.msg_id.as_ref().is_some_and(|s| !s.is_empty()) {
            self.cancel_supported = true;
        }
        if frame.conv.as_ref().is_some_and(|c| c != &self.conv) {
            return;
        }
        if frame
            .msg_id
            .as_ref()
            .is_some_and(|id| Some(id) != self.msg_id.as_ref())
        {
            return;
        }
        if frame.kind == "session_info" {
            if !frame.refresh {
                if !self.connected {
                    self.base_session = frame.session_id.clone();
                } else if self.running {
                    self.session_id = frame.session_id.clone();
                }
            }
            self.connected = true;
            self.stale = false;
            if !self.running {
                self.status = TerminalText::new(strings::CONNECTED.into());
            }
            return;
        }
        if !self.running {
            return;
        }
        if let Some(sid) = frame.session_id {
            self.session_id = Some(sid);
        }
        match frame.kind.as_str() {
            "progress" => {
                self.status = safe_status(frame.content.as_deref().unwrap_or(strings::WORKING))
            }
            "step" => {
                self.status = safe_status(&format!(
                    "{} {} {}",
                    strings::CHAT_STEP,
                    frame.tool.as_deref().unwrap_or(""),
                    frame.summary.as_deref().unwrap_or("")
                ))
            }
            // Never retain raw thinking or an old summary after a redaction frame.
            "thinking" => self.status = TerminalText::new(strings::CHAT_THINKING.into()),
            "assistant_chunk" => self.status = TerminalText::new(strings::CHAT_RESPONDING.into()),
            "assistant_done" => {
                self.append(
                    strings::CHAT_ASSISTANT,
                    frame.content.as_deref().unwrap_or(""),
                );
                self.running = false;
                self.status = TerminalText::new(strings::CHAT_DONE.into());
                // Current gateway compose_session_id() appends agent + conv.
                // Only a completed turn is known to have created a resumable row.
                if self.session_id.is_none()
                    && let (Some(base), Some(agent)) = (&self.base_session, &self.agent)
                {
                    self.session_id = Some(format!("{base}#agent:{agent}#conv:{}", self.conv));
                }
            }
            "error" => {
                let message = if frame.code.as_deref() == Some("turn_limit") {
                    strings::CHAT_TURN_LIMIT
                } else {
                    frame.message.as_deref().unwrap_or(strings::CHAT_ERROR)
                };
                self.append(strings::CHAT_SYSTEM, message);
                self.status = safe_status(message);
                self.running = false;
            }
            "cancel_accepted" => self.status = TerminalText::new(strings::CHAT_CANCELLING.into()),
            "cancelled" => {
                self.running = false;
                self.status = TerminalText::new(strings::CHAT_CANCELLED.into());
            }
            _ => {}
        }
    }
}
fn safe_status(text: &str) -> TerminalText {
    TerminalText::new(
        TerminalText::new(text.chars().take(MAX_LINE_CHARS).collect())
            .plain()
            .replace('\n', " "),
    )
}
