use duduclaw_isolation::worker::{ParseError, ParseRequest};

pub const SOCKET_PATH: &str = "/run/duduclaw-parse/parse.sock";
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_MEMORY_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_WALL_TIME_MS: u32 = 30_000;

/// Implementation ceilings only narrow the shared contract. The fixed systemd
/// template supplies host-wide admission and cgroup limits independently.
pub fn validate(request: &ParseRequest) -> Result<(), ParseError> {
    request.validate().map_err(|_| ParseError::InvalidContent)?;
    if request.limits.memory_bytes.get() > MAX_MEMORY_BYTES
        || request.limits.wall_time_ms.get() > MAX_WALL_TIME_MS
        || request.inputs.as_slice().iter().map(|i| i.size_bytes).sum::<u64>() > MAX_FILE_BYTES {
        return Err(ParseError::LimitExceeded);
    }
    // One bounded output is enough for image conversion; accepting unused
    // writable descriptors would unnecessarily expand a compromised parser.
    if request.outputs.as_slice().len() > 1 { return Err(ParseError::UnsupportedFormat); }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn local_limits_only_narrow_contract() {
        assert!(super::MAX_FILE_BYTES <= duduclaw_isolation::worker::MAX_INPUT_BYTES);
        assert!(super::MAX_FILE_BYTES <= duduclaw_isolation::worker::MAX_TOTAL_INPUT_BYTES);
    }
}
