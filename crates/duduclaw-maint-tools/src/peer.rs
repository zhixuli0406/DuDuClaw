//! Peer-credential gate for the helper socket.
//!
//! The uid comes from the kernel (`SO_PEERCRED` on Linux, `getpeereid` on the
//! BSDs/macOS used for development), recorded when the client connected —
//! never from anything the client sends. Every failure denies.

use std::io;

/// Something that can report the uid of the process on the other end.
/// Implemented for `UnixStream`; tests implement it with fixed answers.
pub trait PeerCredentials {
    fn peer_uid(&self) -> io::Result<u32>;
}

/// Why a peer was refused.
#[derive(Debug)]
pub enum PeerRejection {
    /// The credentials could not be read. Fail closed.
    Unreadable(io::Error),
    /// A different uid.
    WrongUid(u32),
}

impl std::fmt::Display for PeerRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerRejection::Unreadable(e) => write!(f, "peer credentials unreadable: {e}"),
            PeerRejection::WrongUid(uid) => write!(f, "peer uid {uid} is not permitted"),
        }
    }
}

/// Admit the peer only if its uid is exactly `expected`.
pub fn authorize_peer<P: PeerCredentials + ?Sized>(
    peer: &P,
    expected: u32,
) -> Result<(), PeerRejection> {
    match peer.peer_uid() {
        Ok(uid) if uid == expected => Ok(()),
        Ok(uid) => Err(PeerRejection::WrongUid(uid)),
        Err(e) => Err(PeerRejection::Unreadable(e)),
    }
}

#[cfg(unix)]
impl PeerCredentials for std::os::unix::net::UnixStream {
    fn peer_uid(&self) -> io::Result<u32> {
        use std::os::fd::AsRawFd;
        sys::peer_uid(self.as_raw_fd())
    }
}

#[cfg(target_os = "linux")]
mod sys {
    use std::io;
    use std::os::fd::RawFd;

    pub fn peer_uid(fd: RawFd) -> io::Result<u32> {
        let mut cred = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let expected_len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        let mut len = expected_len;
        // SAFETY: `cred` and `len` are valid, writable and live for the call;
        // `len` holds the exact size of `cred`, so the kernel cannot write
        // past it. `fd` is borrowed from a live socket owned by the caller.
        #[allow(unsafe_code)]
        let rc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                (&mut cred as *mut libc::ucred).cast::<libc::c_void>(),
                &mut len,
            )
        };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        if len != expected_len {
            return Err(io::Error::other("SO_PEERCRED returned a short ucred"));
        }
        Ok(cred.uid)
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod sys {
    use std::io;
    use std::os::fd::RawFd;

    pub fn peer_uid(fd: RawFd) -> io::Result<u32> {
        let mut uid: libc::uid_t = 0;
        let mut gid: libc::gid_t = 0;
        // SAFETY: both out-pointers are valid for the call; `fd` is borrowed
        // from a live socket owned by the caller.
        #[allow(unsafe_code)]
        let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(uid)
    }
}

#[cfg(all(
    unix,
    not(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))
))]
mod sys {
    use std::io;
    use std::os::fd::RawFd;

    /// No known peer-credential call on this target: deny.
    pub fn peer_uid(_fd: RawFd) -> io::Result<u32> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "peer credentials are not supported on this platform",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MAINT_UID;

    struct Fixed(io::Result<u32>);
    impl PeerCredentials for Fixed {
        fn peer_uid(&self) -> io::Result<u32> {
            match &self.0 {
                Ok(u) => Ok(*u),
                Err(e) => Err(io::Error::new(e.kind(), e.to_string())),
            }
        }
    }

    #[test]
    fn maint_uid_is_admitted() {
        assert!(authorize_peer(&Fixed(Ok(MAINT_UID)), MAINT_UID).is_ok());
    }

    #[test]
    fn other_uids_are_refused() {
        // gateway / AI employees, root, the old r2 uid (Android AID_RADIO),
        // nobody, and a uid one off.
        for uid in [1000, 0, 1001, 65534, 4799, 4801] {
            assert!(
                matches!(authorize_peer(&Fixed(Ok(uid)), MAINT_UID), Err(PeerRejection::WrongUid(u)) if u == uid),
                "uid {uid} must be refused"
            );
        }
    }

    #[test]
    fn unreadable_credentials_are_refused() {
        let err = Fixed(Err(io::Error::new(io::ErrorKind::Unsupported, "nope")));
        assert!(matches!(
            authorize_peer(&err, MAINT_UID),
            Err(PeerRejection::Unreadable(_))
        ));
    }

    /// The real kernel path on a socketpair: the peer is this test process.
    #[cfg(unix)]
    #[test]
    fn real_socketpair_reports_own_uid() {
        use std::os::unix::net::UnixStream;
        let (a, _b) = UnixStream::pair().unwrap();
        // SAFETY: getuid has no preconditions.
        #[allow(unsafe_code)]
        let me = unsafe { libc::getuid() };
        assert_eq!(a.peer_uid().unwrap(), me);
        assert!(authorize_peer(&a, me).is_ok());
        if me != MAINT_UID {
            assert!(matches!(
                authorize_peer(&a, MAINT_UID),
                Err(PeerRejection::WrongUid(_))
            ));
        }
    }
}
