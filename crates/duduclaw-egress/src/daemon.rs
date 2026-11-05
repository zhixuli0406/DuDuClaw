//! systemd-activated host service; spawnd owns the control socket endpoint.
#![allow(unsafe_code)]
use std::{collections::BTreeMap, fs::File, io::Read, os::{fd::{AsRawFd, FromRawFd, OwnedFd}, unix::fs::{MetadataExt, OpenOptionsExt}}, path::Path, sync::Arc, time::{Duration, Instant}};
use duduclaw_isolation::{bounded::List, egress::*, ids::{RunId, EGRESS_CONTROL_SOCKET, EGRESS_USER}, ipc::{self, Envelope}};
use serde::Deserialize;
use tokio::task::JoinSet;
use crate::{budget::{RunBudget, RunCaps, default_limits}, resolver::{HostResolver, DNS_BROKER_SOCKET}, session::serve_run_limited, transport::{CheckedSocket, socket_info}};

pub const CONFIG_PATH: &str = "/etc/duduclaw/egress.json";
pub const BROKER_CONFIG_PATH: &str = "/etc/duduclaw/dns-broker.json";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub profiles: List<EgressProfile, 128>,
    pub run_caps: RunCaps,
    pub max_runs: u16,
}
impl Config {
    pub fn validate(&self) -> Result<(), EgressError> {
        self.run_caps.validate()?;
        if self.max_runs == 0 || self.max_runs > 128 || self.profiles.as_slice().is_empty() { return Err(EgressError::ResourceLimit); }
        for (index, profile) in self.profiles.as_slice().iter().enumerate() {
            profile.validate().map_err(|_| EgressError::InvalidRequest)?;
            if u32::from(self.max_runs).checked_mul(u32::from(profile.limits.max_tunnels.get()))
                .is_none_or(|total| total > crate::budget::MAX_CONFIGURED_TUNNELS) {
                return Err(EgressError::ResourceLimit);
            }
            if profile.limits.max_tunnels.get() > 256 || profile.limits.max_frame_bytes.get() < 256
                || self.profiles.as_slice().iter().take(index).any(|p| p.id == profile.id) { return Err(EgressError::InvalidRequest); }
        }
        Ok(())
    }
}
fn io_error(_: impl std::fmt::Debug) -> EgressError { EgressError::Unavailable }

/// Fixed configuration path, checked from root through every ancestor. Root is
/// trusted not to rename these directories while a service starts.
pub fn read_root_config<T: serde::de::DeserializeOwned>(path: &str) -> Result<T, EgressError> {
    for ancestor in Path::new(path).ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(io_error)?;
        if meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o022 != 0 || meta.file_type().is_symlink()
            || (ancestor != Path::new(path) && !meta.is_dir()) { return Err(EgressError::Unavailable); }
    }
    let file = File::options().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path).map_err(io_error)?;
    let meta = file.metadata().map_err(io_error)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != 0 || meta.gid() != 0 || meta.mode() & 0o022 != 0
        || meta.len() > 256 * 1024 { return Err(EgressError::Unavailable); }
    let mut bytes = Vec::new();
    file.take(256 * 1024 + 1).read_to_end(&mut bytes).map_err(io_error)?;
    if bytes.len() > 256 * 1024 { return Err(EgressError::Unavailable); }
    serde_json::from_slice(&bytes).map_err(io_error)
}

pub fn harden(user: &str) -> Result<(), EgressError> {
    let account = nix::unistd::User::from_name(user).map_err(io_error)?.ok_or(EgressError::Unavailable)?;
    if account.uid.is_root() || nix::unistd::getuid() != account.uid || nix::unistd::geteuid() != account.uid
        || nix::unistd::getgid() != account.gid || nix::unistd::getegid() != account.gid { return Err(EgressError::Unavailable); }
    // prctl has scalar arguments; capget buffers match Linux capability ABI v3.
    #[repr(C)] struct Header { version: u32, pid: i32 }
    #[repr(C)] #[derive(Clone, Copy)] struct Data { effective: u32, permitted: u32, inheritable: u32 }
    unsafe {
        if libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) != 0 || libc::prctl(libc::PR_GET_DUMPABLE, 0, 0, 0, 0) != 0
            || libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 || libc::prctl(libc::PR_GET_NO_NEW_PRIVS, 0, 0, 0, 0) != 1 { return Err(EgressError::Unavailable); }
        let mut header = Header { version: 0x20080522, pid: 0 };
        let mut data = [Data { effective: 0, permitted: 0, inheritable: 0 }; 2];
        if libc::syscall(libc::SYS_capget, &mut header, data.as_mut_ptr()) != 0
            || data.iter().any(|d| d.effective != 0 || d.permitted != 0 || d.inheritable != 0) { return Err(EgressError::Unavailable); }
    }
    Ok(())
}

/// Called once at process entry: claims the one systemd activation descriptor.
pub fn activated_listener(path: &str, kind: i32) -> Result<OwnedFd, EgressError> {
    if std::env::var("LISTEN_PID").ok().and_then(|v| v.parse::<u32>().ok()) != Some(std::process::id())
        || std::env::var("LISTEN_FDS").as_deref() != Ok("1") { return Err(EgressError::Unavailable); }
    nix::fcntl::fcntl(3, nix::fcntl::FcntlArg::F_GETFD).map_err(io_error)?;
    // fd 3 is valid and uniquely owned after checking the activation contract.
    let fd = unsafe { OwnedFd::from_raw_fd(3) };
    socket_info(&fd, kind, false, false).map_err(io_error)?;
    let name: nix::sys::socket::UnixAddr = nix::sys::socket::getsockname(fd.as_raw_fd()).map_err(io_error)?;
    if name.path() != Some(Path::new(path)) { return Err(EgressError::Unavailable); }
    let accept = nix::sys::socket::getsockopt(&fd, nix::sys::socket::sockopt::AcceptConn).map_err(io_error)?;
    if !accept { return Err(EgressError::Unavailable); }
    for ancestor in Path::new(path).ancestors() {
        let meta = std::fs::symlink_metadata(ancestor).map_err(io_error)?;
        let allowed_mode = if ancestor == Path::new(path) && path == DNS_BROKER_SOCKET {
            let egress = nix::unistd::User::from_name(EGRESS_USER).map_err(io_error)?.ok_or(EgressError::Unavailable)?;
            meta.gid() == egress.gid.as_raw() && meta.mode() & 0o777 == 0o660
        } else { meta.mode() & 0o022 == 0 };
        if meta.uid() != 0 || !allowed_mode || meta.file_type().is_symlink() { return Err(EgressError::Unavailable); }
    }
    Ok(fd)
}

pub async fn host_main() -> Result<(), EgressError> {
    harden(EGRESS_USER)?;
    let config: Config = read_root_config(CONFIG_PATH)?;
    config.validate()?;
    let resolver = HostResolver::new();
    for profile in config.profiles.as_slice() { resolver.probe(profile.dns_route).await?; }
    let listener = CheckedSocket::new(activated_listener(EGRESS_CONTROL_SOCKET, libc::SOCK_SEQPACKET)?).map_err(io_error)?;
    crate::seccomp::install()?;
    loop {
        let fd = listener.accept().await.map_err(io_error)?;
        if socket_info(&fd, libc::SOCK_SEQPACKET, true, false).is_err() { continue; }
        // One authenticated spawnd controller owns all registrations. Losing it
        // drops JoinSet, cancelling all runs and their nested tunnel tasks.
        let control = CheckedSocket::new(fd).map_err(io_error)?;
        return serve_control(control, &config).await;
    }
}

pub async fn serve_control(control: CheckedSocket, config: &Config) -> Result<(), EgressError> {
    control.validate(libc::SOCK_SEQPACKET, true, false).map_err(io_error)?;
    config.validate()?;
    let mut tasks = JoinSet::new();
    let mut runs: BTreeMap<RunId, (tokio::sync::watch::Sender<bool>, tokio::sync::oneshot::Receiver<()>)> = BTreeMap::new();
    let resolver = Arc::new(HostResolver::new());
    let global = Arc::new(tokio::sync::Semaphore::new(crate::budget::MAX_GLOBAL_TUNNELS));
    let mut packets = vec![0; ipc::MAX_FRAME_BYTES + 4];
    let mut limits = default_limits();
    limits.frames_per_second = 100.try_into().unwrap();
    let mut budget = RunBudget::new(&limits, RunCaps { total_bytes: 1, total_connections: 10000 }, Instant::now())?;
    // Remember run IDs for the control lifetime so re-registration cannot reset budgets.
    let mut seen = std::collections::BTreeSet::new();
    let result = async { loop {
        let received = tokio::select! {
            received = control.receive(&mut packets) => received.map_err(io_error)?,
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(Ok(audit)) = result {
                    let audit: crate::session::AuditRecord = audit;
                    runs.remove(&audit.run_id);
                    // One bounded terminal record per run; fields cannot contain content.
                    emit_audit(&audit);
                }
                continue;
            }
        };
        let (size, mut fds) = received;
        if size == 0 { return Err(EgressError::Unavailable); }
        budget.frame(Instant::now())?;
        let request = ipc::decode::<EgressControl>(&packets[..size]).map_err(|_| EgressError::InvalidRequest)?;
        if fds.len() != request.message.expected_fd_count() { return Err(EgressError::InvalidRequest); }
        let reply = match request.message {
            EgressControl::Register { run_id, profile_id } => {
                let profile = config.profiles.as_slice().iter().find(|p| p.id == profile_id);
                if runs.len() >= config.max_runs as usize || seen.len() >= 10000 || seen.contains(&run_id) {
                    EgressControlResponse::Refused { code: EgressError::ResourceLimit }
                } else if let Some(profile) = profile {
                    let fd = fds.pop().ok_or(EgressError::InvalidRequest)?;
                    socket_info(&fd, libc::SOCK_STREAM, true, true).map_err(|_| EgressError::InvalidRequest)?;
                    let socket = CheckedSocket::new(fd).map_err(io_error)?;
                    seen.insert(run_id);
                    let (cancel, receiver) = tokio::sync::watch::channel(false);
                    let (done, completed) = tokio::sync::oneshot::channel();
                    let profile = profile.clone();
                    let caps = config.run_caps;
                    let resolver = resolver.clone();
                    let global = global.clone();
                    tasks.spawn(async move {
                        let audit = serve_run_limited(socket, run_id, profile, caps, resolver, receiver, global).await;
                        let _ = done.send(());
                        audit
                    });
                    runs.insert(run_id, (cancel, completed));
                    EgressControlResponse::Registered
                } else { EgressControlResponse::Refused { code: EgressError::Unavailable } }
            }
            EgressControl::Unregister { run_id } => {
                if let Some((cancel, completed)) = runs.remove(&run_id) {
                    let _ = cancel.send(true);
                    tokio::time::timeout(Duration::from_millis(ipc::CONTROL_TIMEOUT_MS), completed).await
                        .map_err(|_| EgressError::Timeout)?.map_err(io_error)?;
                }
                EgressControlResponse::Unregistered
            }
        };
        let packet = ipc::encode(&Envelope { version: ipc::PROTOCOL_VERSION, request_id: request.request_id, message: reply }).map_err(io_error)?;
        tokio::time::timeout(Duration::from_millis(ipc::CONTROL_TIMEOUT_MS), control.send_packet(&packet)).await.map_err(|_| EgressError::Timeout)?.map_err(io_error)?;
    }}.await;
    for (cancel, _) in runs.values() { let _ = cancel.send(true); }
    // Do not accept a new controller before old run sockets are gone.
    if tokio::time::timeout(Duration::from_millis(ipc::CONTROL_TIMEOUT_MS), async {
        while let Some(result) = tasks.join_next().await {
            if let Ok(audit) = result { emit_audit(&audit); }
        }
    }).await.is_err() { return Err(EgressError::Timeout); }
    result
}

fn emit_audit(audit: &crate::session::AuditRecord) {
    if let Ok(line) = serde_json::to_string(audit) { eprintln!("{line}"); }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BrokerConfig { upstream: std::net::SocketAddr }
pub async fn broker_main() -> Result<(), EgressError> {
    harden("duduclaw-dns")?;
    let config: BrokerConfig = read_root_config(BROKER_CONFIG_PATH)?;
    let egress = nix::unistd::User::from_name(EGRESS_USER).map_err(io_error)?.ok_or(EgressError::Unavailable)?;
    let fd = activated_listener(DNS_BROKER_SOCKET, libc::SOCK_STREAM)?;
    let socket = std::os::unix::net::UnixListener::from(fd);
    socket.set_nonblocking(true).map_err(io_error)?;
    let listener = tokio::net::UnixListener::from_std(socket).map_err(io_error)?;
    crate::seccomp::install()?;
    crate::resolver::serve_dns_broker(listener, config.upstream, egress.uid.as_raw()).await
}
