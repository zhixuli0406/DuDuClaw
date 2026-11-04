use duduclaw_isolation::ipc::ErrorCode;
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, Error>;

/// No dynamic strings, errno, paths, identities or descriptor contents (§4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum Error {
    #[error("unsupported platform")] UnsupportedPlatform,
    #[error("kernel readiness failed")] UnsupportedKernel,
    #[error("root registry adapter unavailable")] RegistryUnavailable,
    #[error("invalid bootstrap")] Bootstrap,
    #[error("invalid descriptors")] Descriptors,
    #[error("descriptor budget exceeded")] DescriptorLimit,
    #[error("executable identity rejected")] Executable,
    #[error("mount construction failed")] Mount,
    #[error("namespace creation failed")] Namespace,
    #[error("privilege drop failed")] Privileges,
    #[error("landlock enforcement failed")] Landlock,
    #[error("seccomp enforcement failed")] Seccomp,
    #[error("invalid state transition")] State,
    #[error("process creation failed")] Process,
    #[error("status reporting failed")] Status,
    #[error("execution failed")] Exec,
}

impl Error {
    pub fn public_code(self) -> ErrorCode {
        match self {
            Self::UnsupportedPlatform | Self::UnsupportedKernel | Self::RegistryUnavailable => ErrorCode::NotReady,
            Self::Bootstrap => ErrorCode::InvalidRequest,
            Self::Descriptors | Self::Executable => ErrorCode::InvalidFds,
            Self::DescriptorLimit => ErrorCode::ResourceLimit,
            _ => ErrorCode::IsolationUnavailable,
        }
    }
}
