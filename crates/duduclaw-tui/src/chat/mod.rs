//! One WebChat transport per RPC client; transcript data is never terminal code.
#[cfg(test)]
mod tests;
mod transport;
mod rooms;
pub use rooms::ChatRooms;
use crate::{sanitize::TerminalText, strings};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
pub use transport::ChatClient;
pub(crate) use transport::Credentials;

pub const MAX_LINES: usize = 2_000;
pub const MAX_TRANSCRIPT_BYTES: usize = 256 * 1024;
const MAX_LINE_CHARS: usize = 512;
pub const MAX_INPUT_CHARS: usize = 8_000;
pub(crate) const DOWNSTREAM_TTL: Duration = Duration::from_secs(600);
const MAX_DOWNSTREAM_COUNT: u64 = 10_000;

/// Additive fields deliberately do not require the unmerged concurrency PR.
#[derive(Clone, Debug, Deserialize)]
pub struct Frame {
    #[serde(rename = "type")]
    pub kind: String,
    pub content: Option<String>,
    pub message: Option<String>,
    pub code: Option<String>,
    pub request_id: Option<String>,
    pub downstream_ref: Option<String>,
    pub downstream_running: Option<serde_json::Value>,
    pub stopped: Option<serde_json::Value>,
    pub still_stopping: Option<serde_json::Value>,
    pub phase: Option<String>,
    pub depth: Option<usize>,
    pub ts: Option<u64>,
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
pub use crate::blocks::{Block, Blocks};
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
struct DownstreamOffer {
    reference: String,
    count: u64,
    expires: Instant,
}

#[derive(Clone, Debug)]
pub(crate) struct Message {
    pub user: bool,
    pub label: TerminalText,
    pub content: TerminalText,
    pub time: String,
    pub turn: u64,
    physical_lines: usize,
}

#[derive(Clone)]
pub struct ChatState {
    pub lines: VecDeque<TerminalText>,
    pub(crate) messages: VecDeque<Message>,
    message_bytes: usize,
    message_lines: usize,
    pub(crate) activity: crate::ui::timeline::ChatActivity,
    pub(crate) revision: u64,
    transcript_bytes: usize,
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
    downstream: Option<DownstreamOffer>,
    downstream_request: Option<String>,
}
impl Default for ChatState {
    fn default() -> Self {
        Self {
            lines: VecDeque::new(),
            messages: VecDeque::new(),
            message_bytes: 0,
            message_lines: 0,
            activity: crate::ui::timeline::ChatActivity::default(),
            revision: 0,
            transcript_bytes: 0,
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
            downstream: None,
            downstream_request: None,
        }
    }
}
pub(crate) fn nonce() -> String {
    format!("tui-{:016x}", rand::random::<u64>())
}
impl ChatState {
    pub fn append(&mut self, label: &str, content: &str) {
        // Roles and turn ownership come from the local append call, never text markup.
        let label_safe = TerminalText::new(TerminalText::new(label.chars().take(512).collect()).plain());
        let budget = MAX_TRANSCRIPT_BYTES.saturating_sub(label_safe.plain().len());
        let safe = TerminalText::new(content.into()).plain();
        let mut start = safe.len().saturating_sub(budget);
        while !safe.is_char_boundary(start) { start += 1; }
        let safe = &safe[start..];
        // Bound retained physical lines as well as bytes, including newline-only input.
        let mut chunks = VecDeque::new();
        for line in safe.split('\n') {
            let chars: Vec<_> = line.chars().collect();
            if chars.is_empty() { chunks.push_back((String::new(), true)); }
            else {
                for (index, part) in chars.chunks(MAX_LINE_CHARS).enumerate() {
                    chunks.push_back((part.iter().collect::<String>(), index == 0));
                }
            }
            while chunks.len() > MAX_LINES { chunks.pop_front(); }
        }
        let mut message_text = String::new();
        for (index, (chunk, starts_line)) in chunks.into_iter().enumerate() {
            if index > 0 && starts_line { message_text.push('\n'); }
            message_text.push_str(&chunk);
        }
        let mut start = message_text.len().saturating_sub(budget);
        while !message_text.is_char_boundary(start) { start += 1; }
        message_text.drain(..start);
        let physical_lines = message_text.split('\n').map(|line| line.chars().count().max(1).div_ceil(MAX_LINE_CHARS)).sum::<usize>();
        let content_safe = TerminalText::new(message_text);
        self.message_lines += physical_lines;
        self.message_bytes += content_safe.plain().len() + label_safe.plain().len();
        self.messages.push_back(Message {
            user: label == strings::CHAT_USER || label.starts_with(&format!("{} @", strings::CHAT_USER)),
            label: label_safe, content: content_safe,
            time: chrono::Local::now().format("%H:%M").to_string(), turn: self.activity.turn(), physical_lines,
        });
        while self.message_lines > MAX_LINES || self.message_bytes > MAX_TRANSCRIPT_BYTES {
            if let Some(old) = self.messages.pop_front() {
                self.message_lines -= old.physical_lines;
                self.message_bytes -= old.content.plain().len() + old.label.plain().len();
            }
        }
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
        self.transcript_bytes += line.len();
        self.lines.push_back(TerminalText::new(line));
        while self.lines.len() > MAX_LINES || self.transcript_bytes > MAX_TRANSCRIPT_BYTES {
            if let Some(old) = self.lines.pop_front() {
                self.transcript_bytes -= old.plain().len();
            }
        }
        self.revision = self.revision.wrapping_add(1);
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
        self.downstream = None;
        self.downstream_request = None;
        self.status = TerminalText::new(strings::DISCONNECTED.into());
    }
    pub(crate) fn prepare(&mut self, input: Submission) -> crate::Result<SendFrame> {
        if !self.connected {
            return Err(crate::Error::Disconnected);
        }
        if self.running {
            return Err(crate::Error::Busy);
        }
        self.activity.begin_turn();
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
    pub(crate) fn downstream_reference(&self, now: Instant) -> Option<&str> {
        self.downstream.as_ref()
            .filter(|offer| self.connected && now < offer.expires)
            .map(|offer| offer.reference.as_str())
    }
    pub(crate) fn downstream_prompt(&self, now: Instant) -> Option<TerminalText> {
        self.downstream_reference(now)?;
        self.downstream.as_ref().map(|offer| TerminalText::new(strings::downstream_prompt(offer.count)))
    }
    pub(crate) fn downstream_prompt_compact(&self, now: Instant) -> Option<TerminalText> {
        self.downstream_reference(now)?;
        self.downstream.as_ref().map(|offer| TerminalText::new(strings::downstream_prompt_compact(offer.count)))
    }
    pub(crate) fn dismiss_downstream(&mut self, reference: &str) {
        if self.downstream.as_ref().is_some_and(|offer| offer.reference == reference) {
            self.downstream = None;
        }
    }
    pub(crate) fn downstream_frame(&mut self, reference: &str, now: Instant) -> Option<serde_json::Value> {
        if self.downstream_reference(now) != Some(reference) { return None; }
        // Consume before transport send. Never replay an uncertain stop request.
        self.downstream = None;
        let request_id = nonce();
        self.downstream_request = Some(request_id.clone());
        self.status = TerminalText::new(strings::DOWNSTREAM_STOPPING.into());
        Some(serde_json::json!({"type":"cancel_downstream", "downstream_ref":reference, "request_id":request_id}))
    }
    pub fn apply(&mut self, frame: Frame) {
        self.apply_at(frame, Instant::now());
    }
    pub(crate) fn apply_at(&mut self, frame: Frame, now: Instant) {
        // These replies belong to a separate stop request, possibly after a new
        // chat turn has started. They must never terminate that newer turn.
        if frame.kind == "error" && frame.code.as_deref() == Some("downstream_stop_busy") {
            if frame.request_id.is_some() && frame.request_id == self.downstream_request {
                let message = frame.message.as_deref().unwrap_or("下游停止請求處理中，請稍後再試。");
                self.append(strings::CHAT_SYSTEM, message);
            }
            return;
        }
        if frame.kind == "downstream_stopped"
            || (frame.kind == "error" && frame.code.as_deref() == Some("downstream_ref_unknown"))
        {
            if frame.request_id.is_none() || frame.request_id != self.downstream_request {
                return;
            }
            self.downstream_request = None;
            let message = if frame.kind == "downstream_stopped" {
                strings::downstream_stopped(count(frame.stopped.as_ref()), count(frame.still_stopping.as_ref()))
            } else {
                strings::DOWNSTREAM_UNKNOWN.into()
            };
            self.append(strings::CHAT_SYSTEM, &message);
            self.status = safe_status(&message);
            return;
        }
        if frame.kind == "error" && self.downstream_request.is_some()
            && frame.request_id == self.downstream_request
        {
            self.downstream_request = None;
            let message = frame.message.as_deref().unwrap_or(strings::CHAT_ERROR);
            self.append(strings::CHAT_SYSTEM, message);
            self.status = safe_status(message);
            return;
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
        if frame.session_id.as_ref().is_some_and(|id| self.session_id.as_ref().is_some_and(|known| known != id)) { return; }
        if frame.msg_id.as_ref().is_some_and(|s| !s.is_empty()) { self.cancel_supported = true; }
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
        // Admission failure does not prove a resumable session was created.
        // Preserve an existing session, but do not turn a rejected first send
        // into a resume request for a session that does not exist yet.
        let admission_error = frame.kind == "error"
            && matches!(frame.code.as_deref(), Some("turn_limit" | "duplicate_message"));
        self.activity.record(&frame);
        if !admission_error && let Some(sid) = frame.session_id {
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
                let message = if frame.code.as_deref() == Some("cancelled") {
                    let running = count(frame.downstream_running.as_ref());
                    if running > 0 && let Some(reference) = frame.downstream_ref.filter(|s| !s.is_empty() && s.len() <= 128) {
                        self.downstream = Some(DownstreamOffer { reference, count: running, expires: now + DOWNSTREAM_TTL });
                    }
                    strings::CHAT_CANCELLED
                } else if frame.code.as_deref() == Some("turn_limit") {
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

fn count(value: Option<&serde_json::Value>) -> u64 {
    value.and_then(serde_json::Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .map(|n| n.min(MAX_DOWNSTREAM_COUNT as f64) as u64).unwrap_or(0)
}
