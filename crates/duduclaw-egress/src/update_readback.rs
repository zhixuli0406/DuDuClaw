//! R6 egress update readback endpoint `/run/duduclaw-egress-update/readback.sock`
//! (plan §4.6), the server of `duduclaw_updated::linux::activation`'s egress
//! readback. Only a uid 0 / gid 0 peer is answered; one frame in, one frame
//! out, no ancillary data, unknown fields refused, 5 s per exchange.
//!
//! The record describes this running process: `build_sha256` is the SHA-256 of
//! `/proc/self/exe` (the loaded image, hashed once before seccomp), the version
//! and protocol are this build's, `effective_uid` is `geteuid()`. A value that
//! cannot be computed makes the record `ready: false`; nothing is guessed.
#![allow(unsafe_code)]
use duduclaw_isolation::{egress::EGRESS_PROTOCOL_VERSION, ids::Sha256Digest, update::{InstallTarget, Product, ReleaseVersion}};
use duduclaw_updated::activation_contract::{ComponentReadback, ControlCommand, ControlOutcome, ControlRequest, ControlResponse};

const SCHEMA: u16 = 1;
/// The only product the release pipeline builds (`validate_bootstrap_manifest`).
const PRODUCT: Product = Product::Duduclaw;

/// What this process observed about itself at startup.
#[derive(Clone, Debug)]
pub struct Identity {
    pub build: Option<Sha256Digest>,
    pub version: Option<ReleaseVersion>,
    pub effective_uid: u32,
}
impl Identity {
    #[cfg(target_os = "linux")]
    pub fn observe() -> Self {
        Self { build: linux::running_image().ok(), version: duduclaw_updated::parse_version(env!("CARGO_PKG_VERSION")).ok(),
            effective_uid: nix::unistd::geteuid().as_raw() }
    }
}

/// `serving`: the host service finished its startup checks (configuration,
/// resolver probe, activated control socket, seccomp) and is accepting control.
pub fn readback(identity: &Identity, serving: bool) -> ComponentReadback {
    let artifact_id = duduclaw_updated::publication::artifact_name(&InstallTarget::Egress).ok();
    let complete = identity.build.is_some() && identity.version.is_some() && artifact_id.is_some()
        && identity.effective_uid != 0;
    ComponentReadback { artifact_id: artifact_id.unwrap_or_default(), target: InstallTarget::Egress, product: PRODUCT,
        version: identity.version.unwrap_or(ReleaseVersion { major: 0, minor: 0, patch: 0 }),
        protocol: EGRESS_PROTOCOL_VERSION, build_sha256: identity.build.unwrap_or(Sha256Digest([0; 32])),
        effective_uid: identity.effective_uid, ready: serving && complete }
}

/// Only `Readback` of a non-zero generation on schema 1 is answered with data.
pub fn answer(request: &ControlRequest, identity: &Identity, serving: bool) -> ControlResponse {
    let outcome = match (request.schema_version, &request.command) {
        (SCHEMA, ControlCommand::Readback { generation }) if generation.0 != [0; 32] =>
            ControlOutcome::Readback { components: vec![readback(identity, serving)] },
        _ => ControlOutcome::Refused,
    };
    ControlResponse { schema_version: SCHEMA, nonce: request.nonce, generation: request.command.generation(), outcome }
}

#[cfg(target_os = "linux")]
pub use linux::{bind, start};

#[cfg(target_os = "linux")]
mod linux {
    use std::{ffi::CString, fs::File, io::{self, Read}, os::fd::{AsRawFd, FromRawFd, OwnedFd}, sync::atomic::{AtomicBool, Ordering},
        time::Duration};
    use duduclaw_isolation::{ids::Sha256Digest, ipc};
    use duduclaw_updated::activation_contract::ControlRequest;
    use sha2::{Digest, Sha256};
    use crate::transport::{socket_info, CheckedSocket};
    use super::Identity;

    pub const SOCKET_DIRECTORY: &str = "duduclaw-egress-update";
    pub const SOCKET_NAME: &str = "readback.sock";
    const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(5);
    const MAX_IMAGE_BYTES: u64 = 768 * 1024 * 1024;
    static SERVING: AtomicBool = AtomicBool::new(false);

    fn invalid() -> io::Error { io::Error::new(io::ErrorKind::InvalidData, "egress update readback refused") }
    fn last() -> io::Error { io::Error::last_os_error() }
    fn literal(value: &str) -> io::Result<CString> { CString::new(value).map_err(|_| invalid()) }

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

    /// Bind `<runtime>/<SOCKET_DIRECTORY>/readback.sock` beneath verified
    /// descriptors: `runtime` (production `/run`) root-owned and not
    /// group/other-writable; the leaf this service's own `RuntimeDirectory=`
    /// (owned by the effective uid/gid, no group write, no other access). The
    /// socket inherits the 0077 umask and is verified to be owner-only. The
    /// leaf is fresh on every start, so an existing entry is refused, never removed.
    pub fn bind(runtime: &str) -> io::Result<OwnedFd> {
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
            _ => return Err(invalid()),
        }
        let fd = nix::sys::socket::socket(nix::sys::socket::AddressFamily::Unix, nix::sys::socket::SockType::SeqPacket,
            nix::sys::socket::SockFlag::SOCK_CLOEXEC, None)?;
        // Resolve the bind path through the verified leaf descriptor.
        let path = format!("/proc/self/fd/{}/{SOCKET_NAME}", leaf.as_raw_fd());
        nix::sys::socket::bind(fd.as_raw_fd(), &nix::sys::socket::UnixAddr::new(path.as_str())?)?;
        let meta = stat_at(&leaf, &name, libc::AT_SYMLINK_NOFOLLOW)?;
        if meta.st_mode & libc::S_IFMT != libc::S_IFSOCK || meta.st_uid != uid || meta.st_mode & 0o077 != 0 {
            return Err(invalid());
        }
        nix::sys::socket::listen(&fd, nix::sys::socket::Backlog::new(4)?)?;
        Ok(fd)
    }

    /// Serve on the current-thread runtime. Call after seccomp is installed;
    /// from then on the host service is serving and readbacks may be ready.
    pub fn start(listener: OwnedFd, identity: Identity) {
        SERVING.store(true, Ordering::Release);
        tokio::spawn(async move {
            let Ok(listener) = CheckedSocket::new(listener) else { return };
            loop {
                let fd = match listener.accept().await {
                    Ok(fd) => fd,
                    Err(_) => { tokio::time::sleep(Duration::from_millis(100)).await; continue; }
                };
                // One exchange at a time, bounded as a whole.
                let _ = tokio::time::timeout(EXCHANGE_TIMEOUT, serve_one(fd, &identity, (0, 0))).await;
            }
        });
    }

    /// Peer `(uid, gid)` must be exactly `peer` (production: root).
    pub(crate) async fn serve_one(fd: OwnedFd, identity: &Identity, peer: (u32, u32)) -> io::Result<()> {
        socket_info(&fd, libc::SOCK_SEQPACKET, false, false)?;
        let credentials = nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::PeerCredentials)?;
        if (credentials.uid(), credentials.gid()) != peer { return Err(invalid()); }
        let socket = CheckedSocket::new(fd)?;
        let mut buffer = vec![0u8; ipc::MAX_FRAME_BYTES + 4];
        // Truncation, any control message and any descriptor are refused.
        let (size, rights) = socket.receive(&mut buffer).await?;
        if size == 0 || !rights.is_empty() { return Err(invalid()); }
        let payload = ipc::payload(buffer.get(..size).ok_or_else(invalid)?).map_err(|_| invalid())?;
        let request: ControlRequest = serde_json::from_slice(payload).map_err(|_| invalid())?;
        let response = super::answer(&request, identity, SERVING.load(Ordering::Acquire));
        let frame = ipc::frame_payload(&serde_json::to_vec(&response).map_err(|_| invalid())?).map_err(|_| invalid())?;
        socket.send_packet(&frame).await
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::IoSlice;
        use duduclaw_isolation::ids::RequestId;
        use duduclaw_updated::activation_contract::{validate_response, ControlCommand, ControlResponse};
        use nix::sys::socket::{socketpair, sendmsg, AddressFamily, ControlMessage, MsgFlags, SockFlag, SockType, UnixAddr};

        fn identity() -> Identity {
            Identity { build: Some(Sha256Digest([1; 32])), version: Some(duduclaw_isolation::update::ReleaseVersion { major: 1, minor: 83, patch: 0 }),
                effective_uid: 999 }
        }
        fn me() -> (u32, u32) { (nix::unistd::getuid().as_raw(), nix::unistd::getgid().as_raw()) }
        fn request() -> ControlRequest {
            ControlRequest { schema_version: 1, nonce: RequestId([2; 16]), command: ControlCommand::Readback { generation: Sha256Digest([3; 32]) } }
        }
        /// Runs one server exchange to completion, then reads what the peer got.
        fn exchange(send: impl FnOnce(&OwnedFd), peer: (u32, u32)) -> (io::Result<()>, Option<ControlResponse>) {
            let (client, server) = socketpair(AddressFamily::Unix, SockType::SeqPacket, None, SockFlag::SOCK_CLOEXEC).unwrap();
            duduclaw_updated::linux::transport::timeout(&client).unwrap();
            send(&client);
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
            let result = runtime.block_on(async {
                tokio::time::timeout(Duration::from_millis(300), serve_one(server, &identity(), peer)).await
                    .unwrap_or_else(|_| Err(io::ErrorKind::TimedOut.into()))
            });
            drop(runtime);
            (result, duduclaw_updated::linux::transport::receive::<ControlResponse>(&client).ok())
        }
        fn raw(client: &OwnedFd, bytes: &[u8]) {
            sendmsg::<UnixAddr>(client.as_raw_fd(), &[IoSlice::new(bytes)], &[], MsgFlags::MSG_NOSIGNAL, None).unwrap();
        }

        #[test]
        fn egress_readback_endpoint_answers_root_frame_only() {
            let request = request();
            let (result, response) = exchange(|c| duduclaw_updated::linux::transport::send(c, &request).unwrap(), me());
            result.unwrap();
            let response = response.unwrap();
            validate_response(&request, &response).unwrap();
            for peer in [(me().0.wrapping_add(1), me().1), (me().0, me().1.wrapping_add(1))] {
                let (result, response) = exchange(|c| duduclaw_updated::linux::transport::send(c, &request).unwrap(), peer);
                assert!(result.is_err() && response.is_none());
            }
        }

        #[test]
        fn egress_readback_endpoint_refuses_bad_frames_ancillary_and_silence() {
            let body = serde_json::to_vec(&request()).unwrap();
            let mut extra = serde_json::to_value(request()).unwrap(); extra["path"] = "/tmp".into();
            let mut trailing = ipc::frame_payload(&body).unwrap(); trailing.push(0);
            let mut long = ((ipc::MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(); long.extend_from_slice(&body);
            for frame in [vec![0u8; 2], trailing, long, body.clone(),
                ipc::frame_payload(&serde_json::to_vec(&extra).unwrap()).unwrap()] {
                let (result, response) = exchange(|c| raw(c, &frame), me());
                assert!(result.is_err() && response.is_none());
            }
            let framed = ipc::frame_payload(&body).unwrap();
            let file = File::open("/dev/null").unwrap();
            let (result, response) = exchange(|c| {
                let rights = [file.as_raw_fd()];
                sendmsg::<UnixAddr>(c.as_raw_fd(), &[IoSlice::new(&framed)], &[ControlMessage::ScmRights(&rights)],
                    MsgFlags::MSG_NOSIGNAL, None).unwrap();
            }, me());
            assert!(result.is_err() && response.is_none());
            // A silent peer is cut off by the exchange deadline.
            let (result, response) = exchange(|_| {}, me());
            assert!(result.is_err() && response.is_none());
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
    fn egress_readback_fields_and_not_ready_cases() {
        let record = readback(&identity(), true);
        assert!(record.ready);
        assert_eq!((record.target, record.product, record.protocol, record.effective_uid),
            (InstallTarget::Egress, Product::Duduclaw, EGRESS_PROTOCOL_VERSION, 999));
        assert_eq!((record.artifact_id.as_str(), record.build_sha256), ("duduclaw-egress", Sha256Digest([1; 32])));
        assert!(!readback(&identity(), false).ready);
        assert!(!readback(&Identity { build: None, ..identity() }, true).ready);
        assert!(!readback(&Identity { version: None, ..identity() }, true).ready);
        assert!(!readback(&Identity { effective_uid: 0, ..identity() }, true).ready);
        assert_eq!(readback(&Identity { build: None, ..identity() }, true).build_sha256, Sha256Digest([0; 32]));
    }

    #[test]
    fn egress_readback_answers_only_readback() {
        let generation = Sha256Digest([3; 32]);
        let request = |schema_version, command| ControlRequest { schema_version, nonce: RequestId([2; 16]), command };
        let good = request(1, ControlCommand::Readback { generation });
        let response = answer(&good, &identity(), true);
        assert!(duduclaw_updated::activation_contract::validate_response(&good, &response).is_ok());
        assert!(matches!(response.outcome, ControlOutcome::Readback { ref components } if components.len() == 1));
        for refused in [request(2, ControlCommand::Readback { generation }),
            request(1, ControlCommand::Readback { generation: Sha256Digest([0; 32]) }),
            request(1, ControlCommand::Drain { generation }), request(1, ControlCommand::Resume { generation })] {
            let response = answer(&refused, &identity(), true);
            assert!(matches!(response.outcome, ControlOutcome::Refused));
            assert_eq!((response.nonce, response.generation), (refused.nonce, refused.command.generation()));
        }
    }
}
