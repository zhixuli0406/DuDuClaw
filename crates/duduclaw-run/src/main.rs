#[allow(unsafe_code)]
fn main() {
    #[cfg(target_os = "linux")]
    {
        // PID 1 invokes this binary with no arguments. B1 must replace the explicit
        // unavailable adapter with its root-owned registry integration before use.
        // SAFETY: a fresh single-threaded binary; serve owns the whole process.
        unsafe { duduclaw_run::linux::runner::serve(&mut duduclaw_run::linux::bootstrap::UnavailableRegistry) }
    }
    #[cfg(not(target_os = "linux"))]
    {
        eprintln!("{{\"code\":\"not_ready\",\"reason\":\"unsupported_platform\"}}");
        std::process::exit(125);
    }
}
