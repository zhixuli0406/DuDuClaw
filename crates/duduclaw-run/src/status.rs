//! B2 private bootstrap status decoder shared with the B1 adapter.
//! These receipts describe stages, not successful exec or systemd admission.
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusRecord {
    Error(u8),
    /// Selected ruleset ABI (1..=7), not the kernel's possibly newer ABI.
    /// This privilege receipt precedes Landlock enforcement, not proof of exec.
    EmployeeReady { landlock_abi: u32 },
    Pty,
    ChildrenForked,
    Exited(u8),
    Signalled(u8),
    SupervisorReady,
    /// Final fd readback, Landlock and seccomp completed for runtime/forwarder.
    /// Does not prove exec succeeded or that either child remains alive.
    IsolationReady,
}

impl StatusRecord {
    /// The caller must own/close all received rights even if decoding fails.
    /// PTY identity still requires B1's independent devpts/statx readback.
    pub fn decode(packet: &[u8], fd_count: usize) -> Result<Self> {
        if packet.len() != 12 || &packet[..4] != b"DCR1" || packet[6..8] != [0, 0] {
            return Err(Error::Status);
        }
        let kind = packet[4];
        let code = packet[5];
        let value = u32::from_be_bytes(packet[8..12].try_into().map_err(|_| Error::Status)?);
        if fd_count != usize::from(kind == 2) || (kind != 0 && code != 0) {
            return Err(Error::Status);
        }
        match kind {
            0 if (1..=16).contains(&code) && value == 0 => Ok(Self::Error(code)),
            1 if (1..=7).contains(&value) => Ok(Self::EmployeeReady { landlock_abi: value }),
            2 if value == 0 => Ok(Self::Pty),
            3 if value == 0 => Ok(Self::ChildrenForked),
            4 if value <= 255 => Ok(Self::Exited(value as u8)),
            5 if (1..=64).contains(&value) => Ok(Self::Signalled(value as u8)),
            6 if value == 0 => Ok(Self::SupervisorReady),
            7 if value == 0 => Ok(Self::IsolationReady),
            _ => Err(Error::Status),
        }
    }

    pub fn encode(self) -> Result<[u8; 12]> {
        let (kind, code, value) = match self {
            Self::Error(code) => (0, code, 0),
            Self::EmployeeReady { landlock_abi } => (1, 0, landlock_abi),
            Self::Pty => (2, 0, 0),
            Self::ChildrenForked => (3, 0, 0),
            Self::Exited(code) => (4, 0, u32::from(code)),
            Self::Signalled(signal) => (5, 0, u32::from(signal)),
            Self::SupervisorReady => (6, 0, 0),
            Self::IsolationReady => (7, 0, 0),
        };
        let mut bytes = [0u8; 12];
        bytes[..4].copy_from_slice(b"DCR1");
        bytes[4] = kind; bytes[5] = code;
        bytes[8..].copy_from_slice(&value.to_be_bytes());
        Self::decode(&bytes, usize::from(kind == 2))?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_roundtrip_and_exact_ancillary_counts() {
        for record in [StatusRecord::Error(16), StatusRecord::EmployeeReady { landlock_abi: 1 },
            StatusRecord::Pty, StatusRecord::ChildrenForked, StatusRecord::Exited(255),
            StatusRecord::Signalled(64), StatusRecord::SupervisorReady, StatusRecord::IsolationReady] {
            let packet = record.encode().unwrap();
            let fds = usize::from(record == StatusRecord::Pty);
            assert_eq!(StatusRecord::decode(&packet, fds), Ok(record));
            assert_eq!(StatusRecord::decode(&packet, 1 - fds), Err(Error::Status));
            assert_eq!(StatusRecord::decode(&packet, 2), Err(Error::Status));
        }
    }

    #[test]
    fn status_rejects_noncanonical_and_out_of_range_records() {
        let packet = StatusRecord::SupervisorReady.encode().unwrap();
        for index in 0..12 {
            let mut invalid = packet;
            invalid[index] = 255;
            assert_eq!(StatusRecord::decode(&invalid, 0), Err(Error::Status));
        }
        for length in 0..12 { assert!(StatusRecord::decode(&packet[..length], 0).is_err()); }
        let mut trailing = packet.to_vec(); trailing.push(0);
        assert!(StatusRecord::decode(&trailing, 0).is_err());
        for record in [StatusRecord::Error(0), StatusRecord::Error(17),
            StatusRecord::EmployeeReady { landlock_abi: 0 },
            StatusRecord::EmployeeReady { landlock_abi: 8 },
            StatusRecord::Signalled(0), StatusRecord::Signalled(65)] {
            assert_eq!(record.encode(), Err(Error::Status));
        }
    }
}
