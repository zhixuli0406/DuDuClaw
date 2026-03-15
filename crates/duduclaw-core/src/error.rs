use thiserror::Error;

/// Core error type for the DuDuClaw system.
#[derive(Debug, Error)]
pub enum DuDuClawError {
    #[error("config error: {0}")]
    Config(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("container error: {0}")]
    Container(String),

    #[error("security error: {0}")]
    Security(String),

    #[error("memory error: {0}")]
    Memory(String),

    #[error("gateway error: {0}")]
    Gateway(String),

    #[error("channel error: {0}")]
    Channel(String),

    #[error("bridge error: {0}")]
    Bridge(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serde json error: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("toml deserialization error: {0}")]
    TomlDeser(#[from] toml::de::Error),
}

/// Convenience result type for DuDuClaw operations.
pub type Result<T> = std::result::Result<T, DuDuClawError>;
