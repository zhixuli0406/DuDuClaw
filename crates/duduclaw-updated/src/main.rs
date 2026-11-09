#[cfg(target_os = "linux")]
fn main() {
    use duduclaw_updated::{Error, linux};
    use duduclaw_isolation::update::UpdateSlot;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--launch"] => linux::launcher::launch(),
        ["--bootstrap-stage"] => linux::bootstrap::stage().map(|digest| {
            println!("bootstrap_staged:{}", hex::encode(digest.0));
        }),
        ["--serve-slot", "a"] => linux::service::run(UpdateSlot::A),
        ["--serve-slot", "b"] => linux::service::run(UpdateSlot::B),
        ["--check-latest"] => linux::service::check_latest(false),
        ["--check-latest", "--dry-run"] => linux::service::check_latest(true),
        ["--rotate-root"] => linux::trust_admin::rotate_from_inbox(),
        ["--emergency"] => linux::emergency::prepare_from_inbox(),
        _ => Err(Error::Invalid),
    };
    if let Err(error) = result {
        // Never print paths, subprocess output or the detailed OS error.
        eprintln!("update_refused: {:?}", error.code());
        std::process::exit(1);
    }
}
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("duduclaw-updated requires native Linux");
    std::process::exit(1);
}
