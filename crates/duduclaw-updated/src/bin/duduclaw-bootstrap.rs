//! duduclaw-bootstrap: the independently hash-checked root transition verifier
//! (plan R1). It takes no path argument: the bundle is read only from the fixed
//! root-only inbox, and on success this process becomes the verified gateway's
//! `service bootstrap-continue`. stderr carries only a step number, an error code
//! and fixed zh-TW guidance; never a path, file content or OS error text.
#[cfg(target_os = "linux")]
fn main() {
    use duduclaw_updated::linux::bootstrap_verify::{self, BootstrapMode};
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["--print-root-pin"] => match bootstrap_verify::pinned_root() {
            Ok((digest, kind)) => {
                println!("{} {}", kind.label(), hex::encode(digest.0));
                std::process::exit(0);
            }
            Err(_) => {
                println!("UNSET");
                std::process::exit(1);
            }
        },
        ["install"] => BootstrapMode::Install,
        ["upgrade"] => BootstrapMode::Upgrade,
        ["rollback"] => BootstrapMode::Rollback,
        _ => {
            eprintln!("用法：duduclaw-bootstrap install | upgrade | rollback | --print-root-pin");
            std::process::exit(2);
        }
    };
    let refusal = match bootstrap_verify::run(mode) {
        Ok(never) => match never {},
        Err(refusal) => refusal,
    };
    eprintln!("bootstrap_refused: step {} {:?}", refusal.step.number(), refusal.error.code());
    eprintln!("{}", refusal.step.hint());
    std::process::exit(1);
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("duduclaw-bootstrap requires native Linux");
    std::process::exit(1);
}
