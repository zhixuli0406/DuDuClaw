fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("duduclaw-spawnd {} protocol=1", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        use duduclaw_spawnd::{server, admission::Limits, host::{LinuxHost, Components, EvidenceReport}, audit::disk::AuditWorker,
            update_control};
        // duduclaw-isolation-ready.service: exits 0 only once the live spawnd
        // reports the verified, opened, consistent set. Reads the root-only
        // status file and PID 1's view of duduclaw-spawnd.service (system bus,
        // read-only); binds nothing. Not on cgroup v2: exits 1 at once.
        if arguments.len() == 1 && arguments[0] == "wait-consistent" {
            if update_control::wait_consistent().is_ok() { return; }
            eprintln!("duduclaw-spawnd: wait-consistent failed");
            std::process::exit(1);
        }
        let result = (|| -> std::io::Result<()> {
            if arguments.len() == 1 && arguments[0] == "--check" {
                let (_, gid) = server::gateway_identity()?;
                let report = EvidenceReport::read(gid)?;
                println!("{}", serde_json::to_string(&report).map_err(std::io::Error::other)?);
                return report.check().map_err(|_| std::io::Error::other("isolation unavailable"));
            }
            server::harden_process()?;
            if !arguments.is_empty() { return Err(std::io::Error::other("invalid arguments")); }
            let gateway = server::gateway_identity()?;
            duduclaw_spawnd::transport::prepare_runtime_directory()?;
            let audit = AuditWorker::start()?;
            // R6: the drain barrier is restored and the installed set verified
            // (journal re-verified and republished / rolled back, or the
            // selected generation verified) before any socket is bound.
            let update = update_control::recover(&audit)?;
            let consistent = update.consistent();
            let mut serving = None;
            // READY=1 always follows the update endpoint (the updater depends on
            // it); the allocation socket (LinuxHost::open), the control listener
            // and admission open only on a set verified consistent at this start.
            let opened = update_control::start(consistent,
                || { serving = Some(update.serve()?); Ok(()) },
                |state| { let _ = update_control::notify(state); },
                || {
                    let host = LinuxHost::open(gateway, Components::installed(), audit.clone())?;
                    let listener = server::listener(gateway.1)?;
                    Ok((host, listener))
                })?;
            let serving = serving.ok_or_else(|| std::io::Error::other("update endpoint not serving"))?;
            let Some((host, listener)) = opened else {
                // Not consistent: only the root update endpoint runs, until a
                // repairing Apply exits the process for a verified restart.
                return serving.wait();
            };
            // duduclaw-isolation-ready.service completes on this status.
            serving.opened()?;
            let snapshots = host.snapshots;
            let limits = Limits { host_ptys: host.host_ptys, ..Limits::default() };
            server::serve_with_drain(listener, gateway, snapshots, host, limits, audit, Some(serving.barrier()))
        })();
        if result.is_ok() { return; }
    }
    eprintln!("duduclaw-spawnd: isolation unavailable");
    std::process::exit(1);
}
