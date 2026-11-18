#[cfg(target_os = "linux")]
mod linux;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    #[cfg(target_os = "linux")]
    // No input-derived text may reach stderr; stderr is relayed to the gateway.
    let reason: &'static str = match linux::run().await {
        Ok(()) => return,
        Err(reason) => reason,
    };
    #[cfg(not(target_os = "linux"))]
    let reason: &'static str = "unsupported platform";
    eprintln!("isolated API execution unavailable: {reason}");
    std::process::exit(125);
}
