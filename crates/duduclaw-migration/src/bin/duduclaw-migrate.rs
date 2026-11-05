//! Signed-bootstrap entry point; never invoked from a gateway/employee process.
#[cfg(target_os = "linux")]
fn run() -> Result<(), duduclaw_migration::Error> {
    use duduclaw_migration::{Error, root_host::{RootMigration, load_plan, pipeline::InstalledIntegration}};
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() < 3 || args.get(1).and_then(|v| v.to_str()) != Some("--plan") { return Err(Error::Invalid); }
    let plan = load_plan(std::path::Path::new(&args[2]))?;
    let mut flow = RootMigration::open(plan, InstalledIntegration::default())?;
    match args[0].to_str() {
        Some("run") if args.len() == 3 => {
            let state = flow.run()?;
            println!("{}", serde_json::to_string(&state)?);
        }
        Some("prepare") if args.len() == 4 && args[3] == "--stop-after-keys" => {
            eprintln!("maintenance: the installation will remain offline after quarantine and key rotation");
            let state = flow.prepare_stop_after_keys()?;
            println!("{}", serde_json::to_string(&state)?);
        }
        Some("new-setup-token") if args.len() == 3 => {
            use std::io::IsTerminal;
            use duduclaw_migration::root_host::bootstrap::GatewayBootstrapIssuer;
            if !std::io::stdout().is_terminal() { return Err(Error::Authentication); }
            // The live gateway owns its fresh user database and authenticates root.
            flow.new_setup_token(&mut GatewayBootstrapIssuer)?.print_once()?;
        }
        Some("expire") if args.len() == 3 => {
            let now = u64::try_from(chrono::Utc::now().timestamp()).map_err(|_| Error::Invalid)?;
            println!("{}", if flow.expire(now)? { "retention_expired" } else { "retention_pending" });
        }
        Some("status") if args.len() == 3 => println!("{}", serde_json::to_string(&flow.state()?)?),
        Some("export-records") if args.len() == 5 && args[3] == "--destination" => {
            flow.export_records(std::path::Path::new(&args[4]))?;
            println!("recovery_records_exported");
        }
        _ => return Err(Error::Invalid),
    }
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), duduclaw_migration::Error> { Err(duduclaw_migration::Error::Unavailable) }
fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(3);
    }
}
