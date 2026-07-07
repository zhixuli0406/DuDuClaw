//! Live smoke test for `platform::self_restart()`.
//!
//! Run: `cargo run -p duduclaw-core --example self_restart_demo`
//! Expected output — two lines, SAME pid on Unix (exec keeps the PID):
//!   first-run pid=12345
//!   restarted-ok pid=12345
//!
//! Used to verify the auto-update restart mechanic without a real release.

fn main() {
    if std::env::var("DUDUCLAW_RESTART_DEMO").is_ok() {
        println!("restarted-ok pid={}", std::process::id());
        return;
    }

    println!("first-run pid={}", std::process::id());
    // Child inherits the environment across exec/spawn.
    unsafe { std::env::set_var("DUDUCLAW_RESTART_DEMO", "1") };

    duduclaw_core::platform::request_restart_after_shutdown();
    assert!(duduclaw_core::platform::restart_requested());

    let err = duduclaw_core::platform::self_restart();
    // self_restart returns only on failure.
    eprintln!("self_restart failed: {err}");
    std::process::exit(1);
}
