//! Pure fd-matrix checks, using observations read from the actual kernel object.
use crate::{Error, Result};
use duduclaw_isolation::ipc::{MAX_ATTACHMENT_TOTAL_BYTES, MAX_INPUT_FDS, MAX_PROMPT_BYTES};
use serde::{Deserialize, Serialize};

pub const REQUIRED_SEALS: u32 = 0x0f;
pub const TMPFS_MAGIC: i64 = 0x01021994;
pub const REQUIRED_STATX: u32 = 0x0001 | 0x0002 | 0x0008 | 0x0100 | 0x1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity { pub inode: u64, pub mount: u64, pub dev_major: u32, pub dev_minor: u32 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind { Regular, Directory, Fifo, Socket, Character, Other }
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access { Read, Write, ReadWrite, Path }

#[derive(Debug, Clone, Copy)]
pub struct Observation {
    pub identity: Identity, pub statx_mask: u32, pub kind: FileKind,
    pub uid: u32, pub mode: u16, pub nlink: u32, pub size: u64,
    pub access: Access, pub cloexec: bool, pub fs_type: i64, pub fs_flags: u64,
    pub seals: Option<u32>, pub rdev_major: u32, pub rdev_minor: u32,
}

#[derive(Debug, Clone, Copy)]
pub enum Expected {
    Executable { identity: Identity, registry_mount: u64 },
    Script { identity: Identity, registry_mount: u64 },
    Prompt { bytes: u64 },
    AttachmentSealed { bytes: u64 },
    Snapshot { bytes: u64, gateway_uid: u32, gateway_mount: u64 },
    Output,
    Pty { mount: u64 },
    Mount { mount: u64, fs_type: i64, required_flags: u64 },
}

pub fn validate(observed: Observation, expected: Expected) -> Result<()> {
    use Expected::*;
    let o = observed;
    if !o.cloexec || o.statx_mask & REQUIRED_STATX != REQUIRED_STATX { return Err(Error::Descriptors); }
    let valid = match expected {
        Executable { identity, registry_mount } | Script { identity, registry_mount } => {
            o.kind == FileKind::Regular && o.uid == 0 && o.mode & 0o6022 == 0
                && o.identity == identity && o.identity.mount == registry_mount
                && o.access == Access::Read
                && (!matches!(expected, Executable { .. }) || o.mode & 0o111 != 0)
        }
        Prompt { bytes } | AttachmentSealed { bytes } => {
            bytes <= MAX_PROMPT_BYTES && o.size == bytes && o.kind == FileKind::Regular
                && o.fs_type == TMPFS_MAGIC && o.seals.is_some_and(|s| s & REQUIRED_SEALS == REQUIRED_SEALS)
                && matches!(o.access, Access::Read | Access::ReadWrite)
        }
        Snapshot { bytes, gateway_uid, gateway_mount } => o.size == bytes
            && bytes <= MAX_ATTACHMENT_TOTAL_BYTES && o.kind == FileKind::Regular
            && o.nlink == 1 && o.uid == gateway_uid && o.identity.mount == gateway_mount
            && o.access == Access::Read,
        Output => o.kind == FileKind::Fifo && o.access == Access::Write,
        Pty { mount } => o.kind == FileKind::Character && (136..=143).contains(&o.rdev_major)
            && o.identity.mount == mount && o.access == Access::ReadWrite,
        Mount { mount, fs_type, required_flags } => o.kind == FileKind::Directory
            && o.identity.mount == mount && o.fs_type == fs_type
            && o.fs_flags & required_flags == required_flags,
    };
    if valid { Ok(()) } else { Err(Error::Descriptors) }
}

/// Independently account actual kernel sizes, not just the bootstrap declarations.
pub fn validate_input_budget(prompt: u64, attachments: &[(u64, bool)]) -> Result<()> {
    if prompt > MAX_PROMPT_BYTES || attachments.len() > MAX_INPUT_FDS { return Err(Error::Descriptors); }
    let mut total = 0u64;
    let mut sealed = prompt;
    for (bytes, memfd) in attachments {
        total = total.checked_add(*bytes).ok_or(Error::Descriptors)?;
        if *memfd {
            if *bytes > MAX_PROMPT_BYTES { return Err(Error::Descriptors); }
            sealed = sealed.checked_add(*bytes).ok_or(Error::Descriptors)?;
        }
    }
    if total > MAX_ATTACHMENT_TOTAL_BYTES || sealed > 64 * 1024 * 1024 { return Err(Error::Descriptors); }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn observation() -> Observation {
        Observation { identity: Identity { inode: 42, mount: 17, dev_major: 8, dev_minor: 1 },
            statx_mask: REQUIRED_STATX, kind: FileKind::Regular, uid: 0, mode: 0o555,
            nlink: 1, size: 8, access: Access::Read, cloexec: true, fs_type: TMPFS_MAGIC,
            fs_flags: 7, seals: Some(15), rdev_major: 0, rdev_minor: 0 }
    }
    #[test]
    fn matrix_rejects_missing_seals_flags_identity_or_ownership() {
        let o = observation(); let e = Expected::Executable { identity: o.identity, registry_mount: 17 };
        assert!(validate(o, e).is_ok());
        for bad in [Observation { uid: 1, ..o }, Observation { mode: 0o777, ..o },
            Observation { cloexec: false, ..o }, Observation { statx_mask: 0, ..o },
            Observation { access: Access::Path, ..o }, Observation { mode: 0o4555, ..o }] {
            assert!(validate(bad, e).is_err());
        }
        for bit in [1, 2, 4, 8] {
            assert!(validate(Observation { seals: Some(15 ^ bit), ..o }, Expected::Prompt { bytes: 8 }).is_err());
        }
        assert!(validate(o, Expected::Prompt { bytes: 8 }).is_ok());
        assert!(validate(o, Expected::Prompt { bytes: 9 }).is_err());
        assert!(validate(o, Expected::Executable { identity: o.identity, registry_mount: 18 }).is_err());
    }
    #[test]
    fn matrix_snapshot_and_mount_checks_are_conjunctive() {
        let o = observation();
        let snapshot = Expected::Snapshot { bytes: 8, gateway_uid: 0, gateway_mount: 17 };
        assert!(validate(o, snapshot).is_ok());
        assert!(validate(Observation { nlink: 2, ..o }, snapshot).is_err());
        assert!(validate(Observation { access: Access::ReadWrite, ..o }, snapshot).is_err());
        let mount = Expected::Mount { mount: 17, fs_type: TMPFS_MAGIC, required_flags: 7 };
        assert!(validate(Observation { kind: FileKind::Directory, ..o }, mount).is_ok());
        assert!(validate(Observation { kind: FileKind::Directory, fs_flags: 3, ..o }, mount).is_err());
    }
    #[test]
    fn input_budget_uses_checked_actual_sizes() {
        assert!(validate_input_budget(0, &[(1, true); 64]).is_ok());
        assert!(validate_input_budget(0, &[(1, true); 65]).is_err());
        assert!(validate_input_budget(MAX_PROMPT_BYTES, &[(MAX_PROMPT_BYTES, true); 4]).is_err());
        assert!(validate_input_budget(0, &[(u64::MAX, false), (1, false)]).is_err());
    }
}
