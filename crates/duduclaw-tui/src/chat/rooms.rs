//! Conversation routing on the single authenticated WebChat connection.
use super::{ChatState, Frame};
use crate::{sanitize::TerminalText, strings};
use std::collections::BTreeMap;
#[derive(Default, Clone)]
pub struct ChatRooms {
    pub connected: bool,
    pub rooms: BTreeMap<String, ChatState>,
    base_session: Option<String>,
}
impl ChatRooms {
    pub fn retain(&mut self, ids: &[String]) {
        self.rooms.retain(|id, _| ids.contains(id));
    }
    pub(crate) fn room(&mut self, id: &str) -> Option<&mut ChatState> {
        if !self.rooms.contains_key(id) {
            if self.rooms.len() >= 4 { return None; }
            let state = ChatState {
                connected: self.connected,
                stale: !self.connected,
                status: TerminalText::new(if self.connected { strings::CONNECTED } else { strings::CONNECTING }.into()),
                base_session: self.base_session.clone(),
                ..ChatState::default()
            };
            self.rooms.insert(id.into(), state);
        }
        self.rooms.get_mut(id)
    }
    pub fn disconnected(&mut self) {
        self.connected = false;
        for room in self.rooms.values_mut() { room.disconnected(); }
    }
    pub fn apply(&mut self, frame: Frame) {
        // Only the first session_info is the connection handshake. Later
        // uncorrelated announcements must not rebind arbitrary conversations.
        if !self.connected && frame.kind == "session_info" {
            self.connected = true;
            self.base_session = frame.session_id.clone();
            for room in self.rooms.values_mut() {
                room.connected = true; room.stale = false;
                if !room.running { room.status = TerminalText::new(strings::CONNECTED.into()); }
                room.base_session = self.base_session.clone();
            }
            return;
        }
        // Older gateways identify admission errors only by request_id = msg_id.
        // A known request must agree with every other supplied identity. Cancel
        // outcomes can carry a server-generated request id, so those still use
        // their session/conv/msg identifiers when the request is unknown here.
        let request_known = frame.request_id.as_ref().is_some_and(|request| {
            self.rooms.values().any(|room| room.msg_id.as_ref() == Some(request)
                || room.downstream_request.as_ref() == Some(request))
        });
        let admission_error = frame.kind == "error"
            && matches!(frame.code.as_deref(), Some("turn_limit" | "duplicate_message"));
        let matches: Vec<_> = self.rooms.iter().filter_map(|(id, room)| {
            let request = frame.request_id.as_ref().is_some_and(|r| room.downstream_request.as_ref() == Some(r)
                || room.msg_id.as_ref() == Some(r));
            let conv = frame.conv.as_ref().is_some_and(|c| c == &room.conv);
            let msg = frame.msg_id.as_ref().is_some_and(|m| room.msg_id.as_ref() == Some(m));
            let session = frame.session_id.as_ref().is_some_and(|s| room.session_id.as_ref() == Some(s)
                || room.base_session.as_ref().zip(room.agent.as_ref()).is_some_and(|(base, agent)| s == &format!("{base}#agent:{agent}#conv:{}", room.conv)));
            let consistent = (!(request_known || admission_error) || frame.request_id.is_none() || request)
                && (frame.session_id.is_none() || session) && frame.conv.as_ref().is_none_or(|c| c == &room.conv)
                && frame.msg_id.as_ref().is_none_or(|m| room.msg_id.as_ref() == Some(m));
            (consistent && (request || conv || msg || session)).then(|| id.clone())
        }).collect();
        if matches.len() == 1 { self.rooms.get_mut(&matches[0]).unwrap().apply(frame); }
    }
}
