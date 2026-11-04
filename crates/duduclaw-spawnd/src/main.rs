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
            let host = LinuxHost::open(gateway, Components::default(), audit.clone())?;
            let listener = server::listener(gateway.1)?;
            let snapshots = host.snapshots;
            let limits = Limits { host_ptys: host.host_ptys, ..Limits::default() };
            server::serve(listener, gateway, snapshots, host, limits, audit)
        })();
        if result.is_ok() { return; }
    }
    eprintln!("duduclaw-spawnd: isolation unavailable");
    std::process::exit(1);
}
