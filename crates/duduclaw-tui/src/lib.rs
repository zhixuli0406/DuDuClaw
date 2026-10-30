//! Read-only control-room foundations. CLI entry and rendering are separate phases.
pub mod auth;
pub mod client;
pub mod sanitize;
pub mod state;
pub mod strings;
pub mod transport;

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
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(strings::error(*self))
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;
