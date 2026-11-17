//! R6 gateway update readback endpoint `/run/duduclaw-gateway-update/readback.sock`
//! (plan §4.6), the server of `duduclaw_updated::linux::activation`'s gateway
//! readback. Started right after `isolation_readiness::initialize`.
//!
//! Only a uid 0 / gid 0 peer (`SO_PEERCRED`) is answered: one frame in, one
//! frame out, through the updater's own transport functions (no ancillary
//! data, unknown fields refused, 5 s socket timeouts). The record describes
//! this running process: `build_sha256` is the SHA-256 of `/proc/self/exe`
//! (the loaded image), `effective_uid` is `geteuid()`, `ready` is
//! `isolation_readiness::status().ready`. A value that cannot be computed
//! makes the record `ready: false`; nothing is guessed.
#![allow(unsafe_code)]
use duduclaw_isolation::{ids::Sha256Digest, update::{InstallTarget, Product, ReleaseVersion}};
use duduclaw_updated::activation_contract::{ComponentReadback, ControlCommand, ControlOutcome, ControlRequest, ControlResponse};

const SCHEMA: u16 = 1;
/// The gateway's component protocol in the signed manifest (plan §4.6: 1).
const PROTOCOL: u16 = 1;
/// The only product the release pipeline builds (`validate_bootstrap_manifest`).
const PRODUCT: Product = Product::Duduclaw;

/// What this process observed about itself.
#[derive(Clone, Debug)]
pub struct Identity {
    pub build: Option<Sha256Digest>,
    pub version: Option<ReleaseVersion>,
    pub effective_uid: u32,
}

/// `ready`: the gateway's isolation readiness (`isolation_readiness::status`).
pub fn readback(identity: &Identity, ready: bool) -> ComponentReadback {
    let artifact_id = duduclaw_updated::publication::artifact_name(&InstallTarget::Gateway).ok();
    let complete = identity.build.is_some() && identity.version.is_some() && artifact_id.is_some()
        && identity.effective_uid != 0;
    ComponentReadback { artifact_id: artifact_id.unwrap_or_default(), target: InstallTarget::Gateway, product: PRODUCT,
        version: identity.version.unwrap_or(ReleaseVersion { major: 0, minor: 0, patch: 0 }), protocol: PROTOCOL,
        build_sha256: identity.build.unwrap_or(Sha256Digest([0; 32])), effective_uid: identity.effective_uid,
        ready: ready && complete }
}

/// Only `Readback` of a non-zero generation on schema 1 is answered with data;
/// readiness is evaluated only for such a request.
pub fn answer(request: &ControlRequest, identity: &Identity, ready: impl FnOnce() -> bool) -> ControlResponse {
    let outcome = match (request.schema_version, &request.command) {
        (SCHEMA, ControlCommand::Readback { generation }) if generation.0 != [0; 32] =>
            ControlOutcome::Readback { components: vec![readback(identity, ready())] },
        _ => ControlOutcome::Refused,
    };
    ControlResponse { schema_version: SCHEMA, nonce: request.nonce, generation: request.command.generation(), outcome }
}

/// Start the endpoint once, on its own thread. Hosts without the isolated
/// update service have no `/run/duduclaw-gateway-update` and serve nothing.
pub fn start() {
    #[cfg(target_os = "linux")]
    linux::start();
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{ffi::CString, fs::File, io::{self, Read}, os::fd::{AsRawFd, FromRawFd, OwnedFd},
        sync::atomic::{AtomicBool, Ordering}, time::Duration};
    use duduclaw_isolation::ids::Sha256Digest;
    use duduclaw_updated::{activation_contract::ControlRequest, linux::transport};
    use nix::{errno::Errno, sys::socket::{self, sockopt, Backlog, SockFlag, UnixAddr}};
    use sha2::{Digest, Sha256};
    use super::Identity;

    pub const SOCKET_DIRECTORY: &str = "duduclaw-gateway-update";
    pub const SOCKET_NAME: &str = "readback.sock";
    const MAX_IMAGE_BYTES: u64 = 1024 * 1024 * 1024;

    fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "gateway update readback refused") }
    fn last() -> io::Error { io::Error::last_os_error() }
    fn updated(error: duduclaw_updated::Error) -> io::Error {
        match error { duduclaw_updated::Error::Io(error) => error, _ => invalid() }
    }
    fn literal(value: &str) -> io::Result<CString> { CString::new(value).map_err(|_| invalid()) }

    impl Identity {
        pub fn observe() -> Self {
            // SAFETY: geteuid takes no arguments and cannot fail.
            Self { build: running_image().ok(), version: duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).ok(),
                effective_uid: unsafe { libc::geteuid() } }
        }
    }

    /// SHA-256 of the running image, never of an installed path.
    pub(super) fn running_image() -> io::Result<Sha256Digest> {
        let mut file = File::open("/proc/self/exe")?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES { return Err(invalid()); }
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        let mut total = 0u64;
        loop {
            let count = match file.read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            };
            if count == 0 { break; }
            total = total.checked_add(count as u64).filter(|total| *total <= MAX_IMAGE_BYTES).ok_or_else(invalid)?;
            hash.update(buffer.get(..count).ok_or_else(invalid)?);
        }
        Ok(Sha256Digest(hash.finalize().into()))
    }

    fn stat_at(directory: &OwnedFd, name: &CString, flags: i32) -> io::Result<libc::stat> {
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: valid dirfd, NUL-terminated name, output written on success only.
        if unsafe { libc::fstatat(directory.as_raw_fd(), name.as_ptr(), stat.as_mut_ptr(), flags) } != 0 { return Err(last()); }
        // SAFETY: fstatat succeeded and initialized the buffer.
        Ok(unsafe { stat.assume_init() })
    }
    fn open_directory(parent: Option<&OwnedFd>, name: &CString) -> io::Result<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
        // SAFETY: NUL-terminated path; a successful descriptor is owned only here.
        let raw = unsafe { match parent {
            Some(parent) => libc::openat(parent.as_raw_fd(), name.as_ptr(), flags),
            None => libc::open(name.as_ptr(), flags),
        } };
        if raw < 0 { return Err(last()); }
        // SAFETY: open/openat returned a new descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(raw) })
    }

    /// Bind `<runtime>/duduclaw-gateway-update/readback.sock` beneath verified
    /// descriptors: `runtime` (production `/run`) root-owned and not
    /// group/other-writable; the leaf the unit's `RuntimeDirectory=` (owned by
    /// the effective uid/gid, no group write, no other access). A socket of this
    /// account left by an earlier process is removed; anything else stops. The
    /// socket inherits the unit's 0077 umask and is verified to be owner-only.
    pub(super) fn bind(runtime: &str) -> io::Result<OwnedFd> {
        let anchor = open_directory(None, &literal(runtime)?)?;
        let here = literal(".")?;
        let meta = stat_at(&anchor, &here, 0)?;
        if meta.st_uid != 0 || meta.st_mode & 0o022 != 0 { return Err(invalid()); }
        let leaf = open_directory(Some(&anchor), &literal(SOCKET_DIRECTORY)?)?;
        let meta = stat_at(&leaf, &here, 0)?;
        // SAFETY: geteuid/getegid take no arguments and cannot fail.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        if uid == 0 || meta.st_uid != uid || meta.st_gid != gid || meta.st_mode & 0o027 != 0 { return Err(invalid()); }
        let name = literal(SOCKET_NAME)?;
        match stat_at(&leaf, &name, libc::AT_SYMLINK_NOFOLLOW) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(existing) if existing.st_mode & libc::S_IFMT == libc::S_IFSOCK && existing.st_uid == uid => {
                // SAFETY: valid dirfd and NUL-terminated name; removes the entry only.
                if unsafe { libc::unlinkat(leaf.as_raw_fd(), name.as_ptr(), 0) } != 0 { return Err(last()); }
            }
            _ => return Err(invalid()),
        }
        let fd = transport::socket().map_err(updated)?;
        // Resolve the bind path through the verified leaf descriptor.
        let path = format!("/proc/self/fd/{}/{SOCKET_NAME}", leaf.as_raw_fd());
        socket::bind(fd.as_raw_fd(), &UnixAddr::new(path.as_str())?)?;
        let meta = stat_at(&leaf, &name, libc::AT_SYMLINK_NOFOLLOW)?;
        if meta.st_mode & libc::S_IFMT != libc::S_IFSOCK || meta.st_uid != uid || meta.st_mode & 0o077 != 0 {
            return Err(invalid());
        }
        socket::listen(&fd, Backlog::new(4)?)?;
        // SO_RCVTIMEO also bounds accept(): the thread never blocks indefinitely.
        transport::timeout(&fd).map_err(updated)?;
        Ok(fd)
    }

    /// One connection: peer check, one request frame, one reply frame, 5 s
    /// socket timeouts. Any error closes the connection without a reply.
    pub(super) fn serve_one(fd: OwnedFd, peer: (u32, u32), identity: &Identity, ready: impl FnOnce() -> bool) -> io::Result<()> {
        transport::timeout(&fd).map_err(updated)?;
        let credentials = socket::getsockopt(&fd, sockopt::PeerCredentials)?;
        if (credentials.uid(), credentials.gid()) != peer { return Err(invalid()); }
        let request: ControlRequest = transport::receive(&fd).map_err(updated)?;
        transport::send(&fd, &super::answer(&request, identity, ready)).map_err(updated)
    }

    fn ready() -> bool { crate::isolation_readiness::status().is_some_and(|readiness| readiness.ready) }

    fn run() {
        // Hash first: the socket exists only once every answer can be computed.
        let identity = Identity::observe();
        let listener = match bind("/run") {
            Ok(listener) => listener,
            Err(_) => {
                if crate::isolation_readiness::status().is_some() {
                    tracing::error!("受保護團隊的更新讀回端點無法啟動，更新服務將拒絕套用新版本。");
                }
                return;
            }
        };
        loop {
            match socket::accept4(listener.as_raw_fd(), SockFlag::SOCK_CLOEXEC) {
                Ok(raw) => {
                    // SAFETY: accept4 returned a new descriptor owned only here.
                    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
                    let _ = serve_one(fd, (0, 0), &identity, ready);
                }
                Err(Errno::EAGAIN | Errno::EINTR | Errno::ECONNABORTED) => {}
                Err(_) => std::thread::sleep(Duration::from_millis(100)),
            }
        }
    }

    pub(super) fn start() {
        static STARTED: AtomicBool = AtomicBool::new(false);
        if STARTED.swap(true, Ordering::AcqRel) { return; }
        let spawned = std::thread::Builder::new().name("isolation-update-readback".into()).spawn(run);
        if spawned.is_err() { tracing::error!("更新讀回端點無法啟動，更新服務將拒絕套用新版本。"); }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::{io::IoSlice, time::Instant};
        use duduclaw_isolation::ids::RequestId;
        use duduclaw_updated::activation_contract::{validate_response, ControlCommand, ControlResponse};
        use nix::sys::socket::{socketpair, sendmsg, AddressFamily, ControlMessage, MsgFlags, SockType};

        fn identity() -> Identity {
            Identity { build: Some(Sha256Digest([1; 32])),
                version: Some(duduclaw_isolation::update::ReleaseVersion { major: 1, minor: 83, patch: 0 }), effective_uid: 999 }
        }
        fn me() -> (u32, u32) {
            // SAFETY: getuid/getgid take no arguments and cannot fail.
            unsafe { (libc::getuid(), libc::getgid()) }
        }
        fn request() -> ControlRequest {
            ControlRequest { schema_version: 1, nonce: RequestId([2; 16]), command: ControlCommand::Readback { generation: Sha256Digest([3; 32]) } }
        }
        fn exchange(send: impl FnOnce(&OwnedFd), peer: (u32, u32)) -> (io::Result<()>, Option<ControlResponse>) {
            let (client, server) = socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
            transport::timeout(&client).unwrap();
            send(&client);
            let result = serve_one(server, peer, &identity(), || true);
            (result, transport::receive::<ControlResponse>(&client).ok())
        }
        fn raw(client: &OwnedFd, bytes: &[u8]) {
            sendmsg::<UnixAddr>(client.as_raw_fd(), &[IoSlice::new(bytes)], &[], MsgFlags::MSG_NOSIGNAL, None).unwrap();
        }

        #[test]
        fn gateway_readback_endpoint_answers_root_frame_only() {
            let request = request();
            let (result, response) = exchange(|c| transport::send(c, &request).unwrap(), me());
            result.unwrap();
            validate_response(&request, &response.unwrap()).unwrap();
            for peer in [(me().0.wrapping_add(1), me().1), (me().0, me().1.wrapping_add(1))] {
                let (result, response) = exchange(|c| transport::send(c, &request).unwrap(), peer);
                assert!(result.is_err() && response.is_none());
            }
        }

        #[test]
        fn gateway_readback_endpoint_refuses_bad_frames_ancillary_and_silence() {
            use duduclaw_isolation::ipc::{frame_payload, MAX_FRAME_BYTES};
            let body = serde_json::to_vec(&request()).unwrap();
            let mut extra = serde_json::to_value(request()).unwrap(); extra["command"]["path"] = "/tmp".into();
            let mut trailing = frame_payload(&body).unwrap(); trailing.push(0);
            let mut long = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(); long.extend_from_slice(&body);
            for frame in [vec![0u8; 2], trailing, long, body.clone(), frame_payload(&serde_json::to_vec(&extra).unwrap()).unwrap()] {
                let (result, response) = exchange(|c| raw(c, &frame), me());
                assert!(result.is_err() && response.is_none());
            }
            let framed = frame_payload(&body).unwrap();
            let file = File::open("/dev/null").unwrap();
            let (result, response) = exchange(|c| {
                let rights = [file.as_raw_fd()];
                sendmsg::<UnixAddr>(c.as_raw_fd(), &[IoSlice::new(&framed)], &[ControlMessage::ScmRights(&rights)],
                    MsgFlags::MSG_NOSIGNAL, None).unwrap();
            }, me());
            assert!(result.is_err() && response.is_none());
            let started = Instant::now();
            let (result, response) = exchange(|_| {}, me());
            assert!(result.is_err() && response.is_none());
            assert!(started.elapsed() >= Duration::from_secs(4), "{:?}", started.elapsed());
        }

        #[test]
        fn gateway_readback_hashes_the_running_image() {
            let size = std::fs::metadata("/proc/self/exe").unwrap().len();
            match running_image() {
                Ok(digest) => {
                    let mut hasher = Sha256::new();
                    std::io::copy(&mut File::open("/proc/self/exe").unwrap(), &mut hasher).unwrap();
                    assert_eq!(digest.0, <[u8; 32]>::from(hasher.finalize()));
                }
                // An oversized (debug) test image is refused, never truncated.
                Err(_) => assert!(size > MAX_IMAGE_BYTES),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_isolation::ids::RequestId;

    fn identity() -> Identity {
        Identity { build: Some(Sha256Digest([1; 32])), version: Some(ReleaseVersion { major: 1, minor: 83, patch: 0 }), effective_uid: 999 }
    }

    #[test]
    fn gateway_readback_fields_and_not_ready_cases() {
        let record = readback(&identity(), true);
        assert!(record.ready);
        assert_eq!((record.target, record.product, record.protocol, record.effective_uid),
            (InstallTarget::Gateway, Product::Duduclaw, 1, 999));
        assert_eq!((record.artifact_id.as_str(), record.build_sha256), ("duduclaw", Sha256Digest([1; 32])));
        // Isolation not ready, or any field that could not be computed.
        assert!(!readback(&identity(), false).ready);
        assert!(!readback(&Identity { build: None, ..identity() }, true).ready);
        assert!(!readback(&Identity { version: None, ..identity() }, true).ready);
        assert!(!readback(&Identity { effective_uid: 0, ..identity() }, true).ready);
        let unknown = readback(&Identity { build: None, version: None, ..identity() }, true);
        assert_eq!((unknown.build_sha256, unknown.version), (Sha256Digest([0; 32]), ReleaseVersion { major: 0, minor: 0, patch: 0 }));
    }

    #[test]
    fn gateway_readback_answers_only_readback_and_reads_readiness_lazily() {
        let generation = Sha256Digest([3; 32]);
        let request = |schema_version, command| ControlRequest { schema_version, nonce: RequestId([2; 16]), command };
        let good = request(1, ControlCommand::Readback { generation });
        let response = answer(&good, &identity(), || true);
        assert!(duduclaw_updated::activation_contract::validate_response(&good, &response).is_ok());
        assert!(matches!(response.outcome, ControlOutcome::Readback { ref components } if components.len() == 1 && components[0].ready));
        let response = answer(&good, &identity(), || false);
        assert!(matches!(response.outcome, ControlOutcome::Readback { ref components } if !components[0].ready));
        for refused in [request(2, ControlCommand::Readback { generation }),
            request(1, ControlCommand::Readback { generation: Sha256Digest([0; 32]) }),
            request(1, ControlCommand::Drain { generation }), request(1, ControlCommand::Resume { generation })] {
            let response = answer(&refused, &identity(), || panic!("readiness read for a refused request"));
            assert!(matches!(response.outcome, ControlOutcome::Refused));
            assert_eq!((response.nonce, response.generation), (refused.nonce, refused.command.generation()));
        }
    }
}
