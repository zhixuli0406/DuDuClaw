//! Host-side egress contracts (design §4.3, §17.7 R7-M2).
//!
//! Control and data sockets are separate. Only spawnd, authenticated as uid 0
//! with SO_PEERCRED, may register a socket. The host retains its run/profile
//! binding; data frames never carry authority. No network policy is implemented
//! here: package C must normalize targets, reject every non-public DNS answer,
//! pin the checked address, enforce budgets and close on malformed frames.

use std::num::{NonZeroU16, NonZeroU32};

use serde::{Deserialize, Serialize};

use crate::bounded::{List, Text};
use crate::ids::{RunId, Sha256Digest};
use crate::ContractError;

/// Phase-0 wire version for §4.3; unknown versions close the connection.
pub const EGRESS_PROTOCOL_VERSION: u16 = 1;
/// Phase-0 maximum JSON payload, excluding the transport length prefix (§4.3).
pub const MAX_EGRESS_FRAME_BYTES: usize = 64 * 1024;
/// Phase-0 raw bytes per data frame; JSON expansion still fits the frame (§4.3).
pub const MAX_TUNNEL_CHUNK_BYTES: usize = 8 * 1024;
/// Phase-0 CONNECT request-line and header budget (§4.3).
pub const MAX_CONNECT_HEADER_BYTES: u32 = 8 * 1024;
/// Phase-0 maximum number of CONNECT headers (§4.3).
pub const MAX_CONNECT_HEADERS: u16 = 32;

/// Root-published profile digest, resolved only on the host (design §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(pub Sha256Digest);

/// Root-selected DNS route; never supplied by a data frame (design §4.3, R7-M2).
/// The broker forwards only DNS to fixed upstreams; do not grant the egress
/// process a private resolver IP exception in its IPAddressAllow policy.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DnsRoute {
    ResolvedVarlink,
    DedicatedDnsBroker,
}

/// Host-enforced per-connection budgets (design §4.3).
/// Values are finite, positive and may only narrow protocol hard caps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressLimits {
    pub max_frame_bytes: NonZeroU32,
    pub max_tunnels: NonZeroU16,
    pub frames_per_second: NonZeroU32,
    pub bytes_per_second: NonZeroU32,
    pub connect_timeout_ms: NonZeroU32,
    pub idle_timeout_ms: NonZeroU32,
    pub max_header_bytes: NonZeroU32,
    pub max_headers: NonZeroU16,
}

impl EgressLimits {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.max_frame_bytes.get() as usize > MAX_EGRESS_FRAME_BYTES
            || self.max_header_bytes.get() > MAX_CONNECT_HEADER_BYTES
            || self.max_headers.get() > MAX_CONNECT_HEADERS
        {
            return Err(ContractError::LimitExceeded("egress limits"));
        }
        Ok(())
    }
}

/// Root-owned CONNECT policy (design §4.3); this is not a domain allowlist.
/// Default deployment permits only port 443. Other ports require an explicit
/// root profile; all profiles still reject non-public addresses and translation
/// ranges (IPv4-mapped, NAT64, 6to4 and Teredo).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressProfile {
    pub id: ProfileId,
    pub allowed_connect_ports: List<NonZeroU16, 64>,
    pub dns_route: DnsRoute,
    pub limits: EgressLimits,
}

impl EgressProfile {
    pub fn validate(&self) -> Result<(), ContractError> {
        self.limits.validate()?;
        let ports = self.allowed_connect_ports.as_slice();
        if ports.is_empty() {
            return Err(ContractError::InvalidValue("empty CONNECT port policy"));
        }
        for (index, port) in ports.iter().enumerate() {
            if ports.iter().take(index).any(|earlier| earlier == port) {
                return Err(ContractError::InvalidValue("duplicate CONNECT port"));
            }
        }
        Ok(())
    }
}

/// Spawnd-only control operation (design §4.3, R4-M5).
/// Register carries exactly one unnamed AF_UNIX SOCK_STREAM socketpair end via
/// SCM_RIGHTS, received CLOEXEC. Unregister carries zero fds and closes all run
/// connections/tunnels. Reject truncated ancillary data, unexpected cmsgs and
/// incorrect counts, closing all received fds. Registration is one-shot for
/// the received connection; data frames cannot replace its binding.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EgressControl {
    Register { run_id: RunId, profile_id: ProfileId },
    Unregister { run_id: RunId },
}

/// Root-only control framing (§4.3): exactly ipc::Envelope<EgressControl> with
/// protocol version 1, a big-endian u32 length and one JSON payload per seqpacket.
/// Use ipc::encode/decode (256 KiB hard cap), not the data-plane EgressFrame.
/// The envelope request_id correlates a reply; it is not a capability.
pub type EgressControlFrame = crate::ipc::Envelope<EgressControl>;

/// Control acknowledgement on the same versioned envelope, carrying no fds (§4.3).
/// B must receive Registered before releasing the forwarder; a timeout denies.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum EgressControlResponse {
    Registered,
    Unregistered,
    Refused { code: EgressError },
}

impl EgressControl {
    pub const fn expected_fd_count(&self) -> usize {
        match self {
            Self::Register { .. } => 1,
            Self::Unregister { .. } => 0,
        }
    }
}

/// A connection-local tunnel number, not a run or authorization id (design §4.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TunnelId(pub u32);

/// Untrusted CONNECT authority; package C must normalize and authorize it (§4.3).
/// `host` is a DNS name or unbracketed standard IP literal, never a URL, header,
/// resolver, path or pre-resolved address supplied in place of a hostname.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectTarget {
    pub host: Text<253>,
    pub port: NonZeroU16,
}

impl ConnectTarget {
    pub fn validate(&self) -> Result<(), ContractError> {
        if self.host.as_str().is_empty()
            || self.host.as_str().chars().any(|c| {
                c.is_control() || c.is_whitespace()
                    || matches!(c, '/' | '\\' | '@' | '?' | '#' | '[' | ']')
            })
        {
            return Err(ContractError::InvalidValue("CONNECT authority"));
        }
        Ok(())
    }
}

/// Sanitized protocol refusal, without resolved addresses or internal paths (§4.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EgressError {
    InvalidRequest,
    DestinationDenied,
    PortDenied,
    ResourceLimit,
    Timeout,
    Unavailable,
}

/// Data-plane messages with no authority fields (design §4.3, R4-M5).
/// Client sends Connect/Data/Close; host sends Connected/Data/Close/Refused.
/// Any message in the wrong direction/state closes the connection. No frame
/// carries fds. The HTTP adapter rejects all methods other than CONNECT with
/// 405, including absolute-form HTTP; it must not translate them to Connect.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EgressMessage {
    Connect { tunnel_id: TunnelId, target: ConnectTarget },
    Connected { tunnel_id: TunnelId },
    Data { tunnel_id: TunnelId, bytes: List<u8, MAX_TUNNEL_CHUNK_BYTES> },
    Close { tunnel_id: TunnelId },
    Refused { tunnel_id: TunnelId, code: EgressError },
}

impl std::fmt::Debug for EgressMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (kind, tunnel) = match self {
            Self::Connect { tunnel_id, .. } => ("Connect", tunnel_id),
            Self::Connected { tunnel_id } => ("Connected", tunnel_id),
            Self::Data { tunnel_id, .. } => ("Data", tunnel_id),
            Self::Close { tunnel_id } => ("Close", tunnel_id),
            Self::Refused { tunnel_id, .. } => ("Refused", tunnel_id),
        };
        f.debug_struct(kind).field("tunnel_id", tunnel).finish_non_exhaustive()
    }
}

/// Versioned data payload; unknown fields at either level are refused (§4.3).
/// Transport is a big-endian u32 payload length followed by this JSON object;
/// check the length before allocating. EOF, trailing data or a size mismatch
/// is fatal. `decode_payload` expects only the already bounded JSON payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EgressFrame {
    pub version: u16,
    pub message: EgressMessage,
}

impl EgressFrame {
    pub fn encode_payload(&self, limits: &EgressLimits) -> Result<Vec<u8>, ContractError> {
        limits.validate()?;
        if self.version != EGRESS_PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        if let EgressMessage::Connect { target, .. } = &self.message {
            target.validate()?;
        }
        let payload = serde_json::to_vec(self).map_err(|_| ContractError::InvalidFrame)?;
        if payload.len() > limits.max_frame_bytes.get() as usize {
            return Err(ContractError::LimitExceeded("egress frame"));
        }
        Ok(payload)
    }

    pub fn decode_payload(payload: &[u8], limits: &EgressLimits) -> Result<Self, ContractError> {
        limits.validate()?;
        if payload.len() > limits.max_frame_bytes.get() as usize {
            return Err(ContractError::LimitExceeded("egress frame"));
        }
        let frame: Self = serde_json::from_slice(payload).map_err(|_| ContractError::InvalidFrame)?;
        if frame.version != EGRESS_PROTOCOL_VERSION {
            return Err(ContractError::UnsupportedVersion);
        }
        if let EgressMessage::Connect { target, .. } = &frame.message {
            target.validate()?;
        }
        Ok(frame)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> EgressLimits {
        EgressLimits {
            max_frame_bytes: NonZeroU32::new(MAX_EGRESS_FRAME_BYTES as u32).unwrap(),
            max_tunnels: NonZeroU16::new(16).unwrap(),
            frames_per_second: NonZeroU32::new(100).unwrap(),
            bytes_per_second: NonZeroU32::new(1024 * 1024).unwrap(),
            connect_timeout_ms: NonZeroU32::new(5000).unwrap(),
            idle_timeout_ms: NonZeroU32::new(30000).unwrap(),
            max_header_bytes: NonZeroU32::new(MAX_CONNECT_HEADER_BYTES).unwrap(),
            max_headers: NonZeroU16::new(MAX_CONNECT_HEADERS).unwrap(),
        }
    }

    #[test]
    fn egress_round_trip_and_fd_plan() {
        let control = EgressControl::Register {
            run_id: RunId([1; 16]),
            profile_id: ProfileId(Sha256Digest([2; 32])),
        };
        let json = serde_json::to_vec(&control).unwrap();
        assert_eq!(serde_json::from_slice::<EgressControl>(&json).unwrap(), control);
        assert_eq!(control.expected_fd_count(), 1);
        let control_frame = EgressControlFrame {
            version: crate::ipc::PROTOCOL_VERSION,
            request_id: crate::ids::RequestId([3; 16]),
            message: control.clone(),
        };
        let packet = crate::ipc::encode(&control_frame).unwrap();
        assert_eq!(crate::ipc::decode::<EgressControl>(&packet).unwrap(), control_frame);
        assert_eq!(EgressControl::Unregister { run_id: RunId([1; 16]) }.expected_fd_count(), 0);
        let frame = EgressFrame {
            version: EGRESS_PROTOCOL_VERSION,
            message: EgressMessage::Connect {
                tunnel_id: TunnelId(1),
                target: ConnectTarget {
                    host: Text::new("example.com".to_owned()).unwrap(),
                    port: NonZeroU16::new(443).unwrap(),
                },
            },
        };
        assert_eq!(EgressFrame::decode_payload(&serde_json::to_vec(&frame).unwrap(), &limits()).unwrap(), frame);
    }

    #[test]
    fn egress_rejects_authority_and_unknown_fields() {
        for json in [
            r#"{"version":1,"run_id":"other","message":{"type":"close","tunnel_id":1}}"#,
            r#"{"version":1,"message":{"type":"close","tunnel_id":1,"profile_id":"other"}}"#,
            r#"{"version":1,"message":{"type":"connect","tunnel_id":1,"target":{"host":"example.com","port":443,"employee":"other"}}}"#,
            r#"{"version":1,"message":{"type":"get","tunnel_id":1}}"#,
            r#"{"version":2,"message":{"type":"close","tunnel_id":1}}"#,
        ] {
            assert!(EgressFrame::decode_payload(json.as_bytes(), &limits()).is_err(), "{json}");
        }
    }

    #[test]
    fn egress_frame_and_policy_limits() {
        assert!(EgressFrame::decode_payload(&vec![b' '; MAX_EGRESS_FRAME_BYTES + 1], &limits()).is_err());
        let mut policy = EgressProfile {
            id: ProfileId(Sha256Digest([2; 32])),
            allowed_connect_ports: List::new(vec![NonZeroU16::new(443).unwrap()]).unwrap(),
            dns_route: DnsRoute::ResolvedVarlink,
            limits: limits(),
        };
        assert!(policy.validate().is_ok());
        policy.allowed_connect_ports = List::new(vec![]).unwrap();
        assert!(policy.validate().is_err());
        assert!(List::<u8, MAX_TUNNEL_CHUNK_BYTES>::new(vec![0; MAX_TUNNEL_CHUNK_BYTES + 1]).is_err());
        let max_data = EgressFrame {
            version: EGRESS_PROTOCOL_VERSION,
            message: EgressMessage::Data { tunnel_id: TunnelId(u32::MAX), bytes: List::new(vec![255; MAX_TUNNEL_CHUNK_BYTES]).unwrap() },
        };
        assert!(max_data.encode_payload(&limits()).is_ok());
        assert!(!format!("{:?}", max_data).contains("255"));
        assert!(serde_json::from_str::<ConnectTarget>(r#"{"host":"example.com","port":0}"#).is_err());
        for host in ["", "http://example.com", "user@example.com", "example.com\r\nX: a"] {
            assert!(ConnectTarget { host: Text::new(host.to_owned()).unwrap(), port: NonZeroU16::new(443).unwrap() }.validate().is_err());
        }
    }
}
