//! A filter for the viewer → VNC server half of an RFB 3.7/3.8 stream
//! (RFC 6143), run by the gateway between the dashboard's noVNC client and
//! the in-container relay. It splits the client's bytes into whole messages
//! and classifies each one:
//!
//! - [`Kind::Forward`]: the handshake and display messages (pixel format,
//!   encodings, update requests, continuous updates) pass through;
//! - [`Kind::Input`]: key and pointer events, the clipboard and the QEMU
//!   extended key event: the bridge forwards these only while this viewer
//!   holds the session's takeover lease, and counts them as viewer input
//!   (the lease's idle clock);
//! - [`Kind::Drop`]: messages that would change the machine rather than
//!   show it (`SetDesktopSize`, which would break the screenshot masking
//!   geometry, and `xvp` shutdown/reboot) are never forwarded.
//!
//! Anything the filter does not understand (another protocol version, a
//! security type other than None or VNC authentication, an unknown message
//! type, an oversized clipboard) is an error and the bridge closes the
//! connection: fail closed. The server → viewer half is not parsed.

/// Largest client clipboard message accepted (header excluded).
pub const MAX_CUT_TEXT: usize = 256 * 1024;
/// Largest buffered partial message (an encodings list is the long case).
const MAX_PENDING: usize = MAX_CUT_TEXT + 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Forward,
    Input,
    Drop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub kind: Kind,
    pub bytes: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Version,
    Security,
    AuthResponse,
    ClientInit,
    Normal,
}

/// Why the stream was refused (closed taxonomy; never echoes the bytes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RfbError {
    Version,
    SecurityType,
    UnknownMessage(u8),
    TooLarge,
}

impl std::fmt::Display for RfbError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Version => write!(f, "unsupported RFB version"),
            Self::SecurityType => write!(f, "unsupported security type"),
            Self::UnknownMessage(t) => write!(f, "unknown client message type {t}"),
            Self::TooLarge => write!(f, "client message too large"),
        }
    }
}

/// Incremental client-stream parser. Feed it whatever arrived; it returns
/// the whole messages and keeps the rest for the next call.
#[derive(Debug)]
pub struct ClientFilter {
    phase: Phase,
    pending: Vec<u8>,
    failed: bool,
}

impl Default for ClientFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientFilter {
    pub fn new() -> Self {
        Self {
            phase: Phase::Version,
            pending: Vec::new(),
            failed: false,
        }
    }

    /// Whether the handshake is done.
    pub fn in_normal_phase(&self) -> bool {
        self.phase == Phase::Normal
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Message>, RfbError> {
        if self.failed {
            return Err(RfbError::TooLarge);
        }
        self.pending.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            match self.next_len() {
                Ok(Some((len, kind))) if self.pending.len() >= len => {
                    let bytes: Vec<u8> = self.pending.drain(..len).collect();
                    self.advance(&bytes)?;
                    out.push(Message { kind, bytes });
                }
                Ok(_) => break,
                Err(e) => {
                    self.failed = true;
                    return Err(e);
                }
            }
        }
        if self.pending.len() > MAX_PENDING {
            self.failed = true;
            return Err(RfbError::TooLarge);
        }
        Ok(out)
    }

    /// Length and kind of the next message, `None` when more bytes are
    /// needed to know.
    fn next_len(&self) -> Result<Option<(usize, Kind)>, RfbError> {
        let p = &self.pending;
        Ok(Some(match self.phase {
            Phase::Version => (12, Kind::Forward),
            Phase::Security => (1, Kind::Forward),
            Phase::AuthResponse => (16, Kind::Forward),
            Phase::ClientInit => (1, Kind::Forward),
            Phase::Normal => {
                let Some(&t) = p.first() else { return Ok(None) };
                match t {
                    0 => (20, Kind::Forward),
                    2 => {
                        if p.len() < 4 {
                            return Ok(None);
                        }
                        let n = u16::from_be_bytes([p[2], p[3]]) as usize;
                        (4 + 4 * n, Kind::Forward)
                    }
                    3 => (10, Kind::Forward),
                    4 => (8, Kind::Input),
                    5 => (6, Kind::Input),
                    6 => {
                        if p.len() < 8 {
                            return Ok(None);
                        }
                        // Negative = extended clipboard format, |len| bytes.
                        let len = i32::from_be_bytes([p[4], p[5], p[6], p[7]]).unsigned_abs() as usize;
                        if len > MAX_CUT_TEXT {
                            return Err(RfbError::TooLarge);
                        }
                        (8 + len, Kind::Input)
                    }
                    150 => (10, Kind::Forward),
                    248 => {
                        if p.len() < 2 {
                            return Ok(None);
                        }
                        if p[1] != 0 {
                            return Err(RfbError::UnknownMessage(248));
                        }
                        (12, Kind::Input)
                    }
                    250 => (4, Kind::Drop),
                    251 => {
                        if p.len() < 8 {
                            return Ok(None);
                        }
                        let n = p[6] as usize;
                        (8 + 16 * n, Kind::Drop)
                    }
                    other => return Err(RfbError::UnknownMessage(other)),
                }
            }
        }))
    }

    fn advance(&mut self, msg: &[u8]) -> Result<(), RfbError> {
        self.phase = match self.phase {
            Phase::Version => {
                if msg != b"RFB 003.008\n" && msg != b"RFB 003.007\n" {
                    return Err(RfbError::Version);
                }
                Phase::Security
            }
            Phase::Security => match msg[0] {
                1 => Phase::ClientInit,
                2 => Phase::AuthResponse,
                _ => return Err(RfbError::SecurityType),
            },
            Phase::AuthResponse => Phase::ClientInit,
            Phase::ClientInit | Phase::Normal => Phase::Normal,
        };
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handshake() -> Vec<u8> {
        let mut v = b"RFB 003.008\n".to_vec();
        v.push(2);
        v.extend_from_slice(&[7u8; 16]);
        v.push(1); // shared
        v
    }

    fn kinds(msgs: &[Message]) -> Vec<Kind> {
        msgs.iter().map(|m| m.kind).collect()
    }

    #[test]
    fn handshake_is_forwarded_in_four_messages() {
        let mut f = ClientFilter::new();
        let out = f.feed(&handshake()).unwrap();
        assert_eq!(kinds(&out), vec![Kind::Forward; 4]);
        assert!(f.in_normal_phase());
    }

    #[test]
    fn messages_split_across_feeds_are_reassembled() {
        let mut f = ClientFilter::new();
        let hs = handshake();
        let mut all = Vec::new();
        for b in &hs {
            all.extend(f.feed(&[*b]).unwrap());
        }
        assert_eq!(all.len(), 4);
        // SetEncodings with 3 encodings, fed in two halves.
        let mut enc = vec![2u8, 0, 0, 3];
        enc.extend_from_slice(&[0u8; 12]);
        assert!(f.feed(&enc[..5]).unwrap().is_empty());
        let out = f.feed(&enc[5..]).unwrap();
        assert_eq!(out, vec![Message { kind: Kind::Forward, bytes: enc }]);
    }

    #[test]
    fn input_events_are_classified_as_input() {
        let mut f = ClientFilter::new();
        f.feed(&handshake()).unwrap();
        let mut stream = vec![3u8, 1, 0, 0, 0, 0, 0, 10, 0, 10]; // update request
        stream.extend_from_slice(&[4, 1, 0, 0, 0, 0, 0, 0x61]); // key 'a'
        stream.extend_from_slice(&[5, 1, 0, 10, 0, 20]); // pointer
        stream.extend_from_slice(&[6, 0, 0, 0, 0, 0, 0, 2, b'h', b'i']); // cut text
        stream.extend_from_slice(&[248, 0, 0, 1, 0, 0, 0, 0x61, 0, 0, 0, 30]); // qemu key
        let out = f.feed(&stream).unwrap();
        assert_eq!(
            kinds(&out),
            vec![Kind::Forward, Kind::Input, Kind::Input, Kind::Input, Kind::Input]
        );
    }

    #[test]
    fn resize_and_power_requests_are_dropped() {
        let mut f = ClientFilter::new();
        f.feed(&handshake()).unwrap();
        let mut resize = vec![251u8, 0, 0, 100, 0, 100, 1, 0];
        resize.extend_from_slice(&[0u8; 16]);
        let mut stream = resize.clone();
        stream.extend_from_slice(&[250, 0, 1, 4]); // xvp reboot
        let out = f.feed(&stream).unwrap();
        assert_eq!(kinds(&out), vec![Kind::Drop, Kind::Drop]);
    }

    #[test]
    fn unknown_types_versions_and_security_fail_closed() {
        let mut f = ClientFilter::new();
        f.feed(&handshake()).unwrap();
        assert_eq!(f.feed(&[99]), Err(RfbError::UnknownMessage(99)));
        assert!(f.feed(&[3]).is_err(), "a failed filter stays failed");

        let mut v = ClientFilter::new();
        assert_eq!(v.feed(b"RFB 003.003\n"), Err(RfbError::Version));

        let mut s = ClientFilter::new();
        let mut hs = b"RFB 003.008\n".to_vec();
        hs.push(19); // VeNCrypt
        assert_eq!(s.feed(&hs), Err(RfbError::SecurityType));
    }

    #[test]
    fn oversized_clipboard_is_refused() {
        let mut f = ClientFilter::new();
        f.feed(&handshake()).unwrap();
        let len = (MAX_CUT_TEXT as u32 + 1).to_be_bytes();
        assert_eq!(
            f.feed(&[6, 0, 0, 0, len[0], len[1], len[2], len[3]]),
            Err(RfbError::TooLarge)
        );
    }

    #[test]
    fn security_none_skips_the_auth_response() {
        let mut f = ClientFilter::new();
        let mut hs = b"RFB 003.008\n".to_vec();
        hs.extend_from_slice(&[1, 1]);
        assert_eq!(f.feed(&hs).unwrap().len(), 3);
        assert!(f.in_normal_phase());
    }
}
