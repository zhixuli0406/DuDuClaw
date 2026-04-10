//! CE binary entry point — delegates to `duduclaw_cli::entry_point`.

#[tokio::main]
async fn main() {
    duduclaw_cli::entry_point().await;
}
