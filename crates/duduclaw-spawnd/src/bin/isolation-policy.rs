//! `duduclaw-isolation-policy` (R7d, feature `policy-tool`): build and verify
//! the signed isolation policy artifact (`runtime-isolation-policy-v1`) with
//! spawnd's own readers and publication checks; R8r-4 `accept` (the MCP
//! acceptance harness, root on a disposable host only). An offline vendor/test
//! tool; never a release binary (scripts/package-linux-release.py refuses it).
//! Usage: see crates/duduclaw-spawnd/README.md "Isolation policy generator".

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    std::process::exit(duduclaw_spawnd::installed::policy_tool::main(&args));
}

// spawnd's policy readers and checks (crate::installed) exist only on Linux.
#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("duduclaw-isolation-policy: requires Linux (spawnd's policy readers are Linux-only); \
               build and run it on a Linux host (any architecture: --arch selects the generation's)");
    std::process::exit(1);
}
