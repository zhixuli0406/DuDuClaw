#[cfg(target_os = "linux")]
fn run() -> Result<(), &'static str> {
    use std::os::unix::process::CommandExt;
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 2 || args[0] != "--duduclaw-protocol"
        || !(70000..=99999).contains(&rustix::process::geteuid().as_raw()) { return Err("invalid launch"); }
    let bytes = duduclaw_api_worker::sealed::sealed_request()?;
    let request: duduclaw_api_worker::native::Request = serde_json::from_slice(&bytes).map_err(|_| "invalid input")?;
    if request.protocol != args[1] { return Err("input protocol mismatch"); }
    let argv = request.arguments()?;
    if request.protocol == "codex-v1" {
        use std::io::Read;
        let mut input = Vec::new();
        std::io::stdin().take(duduclaw_api_worker::MAX_INPUT as u64 + 1).read_to_end(&mut input)
            .map_err(|_| "input unavailable")?;
        if input != bytes { return Err("input mismatch"); }
    }
    let binary = match request.protocol.as_str() {
        "codex-v1" => "/runtime/runtime-vendor-codex",
        "grok-v1" => "/runtime/runtime-vendor-grok",
        _ => return Err("invalid protocol"),
    };
    // The signed generation supplies the vendor ELF alongside this adapter.
    // The process stays inside B's uid/namespaces/cgroup and keeps only D's fd 3.
    let mut command = std::process::Command::new(binary);
    command.args(argv);
    if request.protocol == "codex-v1" { command.stdin(std::process::Stdio::null()); }
    // HOME is B's private root-owned config home. No inherited host override.
    for key in ["CODEX_HOME", "GROK_HOME", "XDG_CONFIG_HOME", "XDG_CONFIG_DIRS"] { command.env_remove(key); }
    let _ = command.exec();
    Err("vendor exec failed")
}
fn main() {
    #[cfg(target_os = "linux")]
    if run().is_ok() { return; }
    eprintln!("isolated native execution unavailable");
    std::process::exit(125);
}
