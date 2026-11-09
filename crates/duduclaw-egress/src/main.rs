#[cfg(target_os = "linux")]
fn main() {
    // First operation after exec: TCB processes cannot expose memory dumps.
    #[allow(unsafe_code)]
    let secured = unsafe { libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0) == 0 };
    if !secured { std::process::exit(1); }
    if let Err(code) = start() {
        eprintln!("egress stopped: {code:?}");
        std::process::exit(1);
    }
}
#[cfg(target_os = "linux")]
fn start() -> Result<(), duduclaw_isolation::egress::EgressError> {
    use duduclaw_isolation::egress::EgressError;
    use std::os::fd::{FromRawFd, OwnedFd};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|_| EgressError::Unavailable)?;
    match args.as_slice() {
        [] => runtime.block_on(duduclaw_egress::daemon::host_main()),
        [mode] if mode == "dns-broker" => runtime.block_on(duduclaw_egress::daemon::broker_main()),
        [mode, port] | [mode, port, _] if mode == "forwarder" => {
            let port = port.to_str().and_then(|s| s.parse::<std::num::NonZeroU16>().ok()).ok_or(EgressError::InvalidRequest)?;
            let limits = if let Some(json) = args.get(2) {
                let json = json.to_str().filter(|s| s.len() <= 2048).ok_or(EgressError::InvalidRequest)?;
                let limits: duduclaw_isolation::egress::EgressLimits = serde_json::from_str(json).map_err(|_| EgressError::InvalidRequest)?;
                limits.validate().map_err(|_| EgressError::InvalidRequest)?;
                limits
            } else { duduclaw_egress::budget::default_limits() };
            nix::fcntl::fcntl(3, nix::fcntl::FcntlArg::F_GETFD).map_err(|_| EgressError::Unavailable)?;
            // The trampoline gives this branch exclusive ownership of fd 3.
            #[allow(unsafe_code)]
            let fd = unsafe { OwnedFd::from_raw_fd(3) };
            duduclaw_egress::transport::socket_info(&fd, libc::SOCK_STREAM, true, true).map_err(|_| EgressError::Unavailable)?;
            let _guard = runtime.enter();
            let socket = duduclaw_egress::transport::CheckedSocket::new(fd).map_err(|_| EgressError::Unavailable)?;
            runtime.block_on(duduclaw_egress::forwarder::run_with_limits(socket, port, limits))
        }
        _ => Err(EgressError::InvalidRequest),
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("native egress requires Linux");
    std::process::exit(1);
}
