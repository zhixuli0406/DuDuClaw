#[cfg(target_os = "linux")]
fn main() {
    use duduclaw_updated::{Error, linux, bootstrap_cli, updated_cli::{self, UpdatedCommand}};
    // First step, before hardening, any file open, socket connect or lock: parse
    // argv (exact fixed words, `updated_cli`) and refuse a state-changing operator
    // command inside an AI session with the same fixed line as duduclaw-bootstrap
    // (never the marker or its value). The service modes and read-only queries
    // pass. A guard against mistakes, not a privilege boundary.
    let command = match updated_cli::parse_guarded(std::env::args_os().skip(1), bootstrap_cli::process_env_presence) {
        Ok(command) => command,
        Err(refused) => {
            eprintln!("{}", refused.line());
            std::process::exit(1);
        }
    };
    let result = match command {
        UpdatedCommand::Launch => linux::launcher::launch(),
        UpdatedCommand::BootstrapStage => linux::bootstrap::stage().map(|digest| {
            println!("bootstrap_staged:{}", hex::encode(digest.0));
        }),
        UpdatedCommand::ServeSlot(slot) => linux::service::run(slot),
        UpdatedCommand::CheckLatest => linux::service::check_latest(false),
        UpdatedCommand::CheckLatestDryRun => linux::service::check_latest(true),
        UpdatedCommand::RotateRoot => linux::trust_admin::rotate_from_inbox(),
        UpdatedCommand::Emergency => linux::emergency::prepare_from_inbox(),
        UpdatedCommand::Usage => Err(Error::Invalid),
    };
    if let Err(error) = result {
        // Never print paths, subprocess output or the detailed OS error.
        eprintln!("update_refused: {:?}", error.code());
        // Fixed stage names only (`duduclaw_updated::stage`).
        if let Some(stage) = duduclaw_updated::stage::describe() { eprintln!("update_stage: {stage}"); }
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("duduclaw-updated requires native Linux");
    std::process::exit(1);
}
