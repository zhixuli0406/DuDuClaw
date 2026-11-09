#[allow(unsafe_code)]
fn main() {
    #[cfg(target_os = "linux")]
    {
        // PID 1 invokes this binary with no arguments. Authority arrives only
        // over the root-created bootstrap socket as a sealed, nonce-bound plan.
        // SAFETY: a fresh single-threaded binary; serve owns the whole process.
        unsafe { duduclaw_run::linux::runner::serve(&mut duduclaw_run::linux::sealed_registry::SealedRegistry::default()) }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("{{\"code\":\"not_ready\",\"reason\":\"unsupported_platform\"}}");
        std::process::exit(125);
    }
}
