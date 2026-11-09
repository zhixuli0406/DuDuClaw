#[cfg(target_os = "linux")]
mod linux;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    #[cfg(target_os = "linux")]
    if linux::run().await.is_ok() { return; }
    eprintln!("isolated API execution unavailable");
    std::process::exit(125);
}
