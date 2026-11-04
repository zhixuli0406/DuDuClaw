//! Root-owned protected update implementation. Wire contracts remain in isolation.
pub mod metadata;
pub mod sshsig;
pub mod trust;
pub mod state;
pub mod activation_contract;
pub mod trust_store;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub mod client;

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid update data")] Invalid,
    #[error("signature rejected")] Signature,
    #[error("metadata expired")] Expired,
    #[error("rollback refused")] Rollback,
    #[error("incompatible release")] Incompatible,
    #[error("update service requires maintenance")] Maintenance,
    #[error("update already in progress")] Busy,
    #[error("update I/O unavailable")] Io(#[from] std::io::Error),
    #[error("invalid update encoding")] Json(#[from] serde_json::Error),
    #[error("invalid update contract")] Contract(#[from] duduclaw_isolation::ContractError),
}
impl Error {
    pub fn code(&self) -> duduclaw_isolation::update::UpdateErrorCode {
        use duduclaw_isolation::update::UpdateErrorCode as C;
        match self {
            Self::Invalid | Self::Json(_) | Self::Contract(_) => C::InvalidRequest,
            Self::Signature => C::SignatureRejected,
            Self::Expired => C::Expired,
            Self::Rollback => C::RollbackRefused,
            Self::Incompatible => C::Incompatible,
            Self::Busy => C::Busy,
            Self::Maintenance | Self::Io(_) => C::MaintenanceRequired,
        }
    }
}
pub fn parse_version(text: &str) -> Result<duduclaw_isolation::update::ReleaseVersion> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let parts: Vec<_> = text.split('.').collect();
    if parts.len() != 3 { return Err(Error::Invalid); }
    let mut values = [0; 3];
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() || (part.len() > 1 && part.as_bytes()[0] == b'0')
            || !part.bytes().all(|b| b.is_ascii_digit()) { return Err(Error::Invalid); }
        values[index] = part.parse().map_err(|_| Error::Invalid)?;
    }
    Ok(duduclaw_isolation::update::ReleaseVersion { major: values[0], minor: values[1], patch: values[2] })
}
pub fn version_text(v: duduclaw_isolation::update::ReleaseVersion) -> String {
    format!("{}.{}.{}", v.major, v.minor, v.patch)
}
#[cfg(test)]
mod tests {
    #[test]
    fn stable_versions_only() {
        for s in ["1.80.0", "v1.80.0", "0.0.0"] { assert!(super::parse_version(s).is_ok()); }
        for s in ["1.80.0-beta", "1.80.0+build", "1.80", "01.80.0", "1.80.0\n", "１.80.0", "4294967296.0.0", "vv1.80.0"] {
            assert!(super::parse_version(s).is_err(), "{s}");
        }
    }
}
