//! Read-only control room and line-oriented inbox. The host CLI owns login and guards.
pub mod auth;
pub mod client;
pub mod sanitize;
pub mod state;
pub mod state_extra;
pub mod strings;
pub mod transport;
pub mod settings;
pub mod read_state;
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
