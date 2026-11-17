fn main() {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments.len() == 1 && arguments[0] == "--version" {
        println!("duduclaw-spawnd {} protocol=1", env!("CARGO_PKG_VERSION"));
        return;
    }
    #[cfg(target_os = "linux")]
    {
        use duduclaw_spawnd::{server, admission::Limits, host::{LinuxHost, Components, EvidenceReport}, audit::disk::AuditWorker};
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
            // R6: the drain barrier and the publication journal are settled
            // (roll forward / roll back / not ready) before any socket is bound
            // (LinuxHost::open binds the allocation socket) and before admission.
            let update = duduclaw_spawnd::update_control::recover(&audit)?;
            let host = LinuxHost::open(gateway, Components::installed(), audit.clone())?;
            let listener = server::listener(gateway.1)?;
            let ready = update.ready();
            let drain = update.serve()?;
            // Type=notify: units ordered after spawnd start only on a consistent
            // set. Not ready: no READY=1 (the unit stays activating, admission
            // closed, update.sock answering root) until an operator acts.
            if ready {
                duduclaw_spawnd::update_control::notify("READY=1")?;
            } else {
                let _ = duduclaw_spawnd::update_control::notify("STATUS=update publication incomplete; see /run/duduclaw-spawnd/update-status.json");
            }
            let snapshots = host.snapshots;
            let limits = Limits { host_ptys: host.host_ptys, ..Limits::default() };
            server::serve_with_drain(listener, gateway, snapshots, host, limits, audit, Some(drain))
        })();
        if result.is_ok() { return; }
    }
    eprintln!("duduclaw-spawnd: isolation unavailable");
    std::process::exit(1);
}
