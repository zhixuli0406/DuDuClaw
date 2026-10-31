//! Read-only control room and line-oriented inbox. The host CLI owns login and guards.
pub mod auth;
pub mod budget;
pub mod client;
pub mod sanitize;
pub mod state;
pub mod state_extra;
pub mod strings;
pub mod transport;
pub mod settings;
pub mod read_state;
mod local_file;
pub mod inbox;
pub mod ui;
pub use inbox::{InboxOptions, run_inbox};
pub use ui::{ControlRoomOptions, run_control_room};

use std::fmt;

/// Deliberately contains no server text, request body, URL, or credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidUrl,
    TlsRequired,
    LocalOnly,
    Network,
    Protocol,
    SnapshotBudget,
    LoginRequired,
    SetupRequired,
    PasswordChangeRequired,
    LegacyIdentity,
    IdentityChanged,
    PromptCancelled,
    CoreDumpProtection,
    Timeout,
    Disconnected,
    RpcRejected,
    Busy,
    Terminal,
    LocalState,
    Output,
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(strings::error(*self))
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

/// TUI entry never installs logging. Also silence an embedding host's subscriber
/// for every poll; tasks that own secrets must use this wrapper when spawned.
pub async fn without_tracing<F: std::future::Future>(future: F) -> F::Output {
    use tracing::instrument::WithSubscriber;
    future.with_subscriber(tracing::subscriber::NoSubscriber::default()).await
}
