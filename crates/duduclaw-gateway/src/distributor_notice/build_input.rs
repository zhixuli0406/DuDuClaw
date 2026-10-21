//! Build-input reader for the distributor notice (used by `build.rs`).
//!
//! Kept separate from the installation policy on purpose: the policy must
//! never carry identities, while this input exists only to show distributor
//! information. Unset means "no notice" and leaves public builds unchanged.

use std::io::Read;
use std::path::Path;

/// Environment variable naming the absolute path of the notice HTML file.
pub const NOTICE_PATH_ENV: &str = "DUDUCLAW_DISTRIBUTOR_NOTICE_PATH";

/// Validate the path text taken from [`NOTICE_PATH_ENV`].
pub fn validate_notice_path(value: &str) -> Result<&Path, String> {
    if value.is_empty() {
        return Err(format!("{NOTICE_PATH_ENV} is set but empty; unset it for a build without a notice"));
    }
    if value.len() > 4096 || value.chars().any(char::is_control) {
        return Err(format!("{NOTICE_PATH_ENV} contains invalid characters"));
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return Err(format!("{NOTICE_PATH_ENV} must be an absolute path"));
    }
    Ok(path)
}

/// Read the notice file: a regular file (not a symlink), UTF-8, within the
/// size ceiling. A leading UTF-8 byte-order mark is dropped.
pub fn read_notice_file(path: &Path, max_bytes: usize) -> Result<String, String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|e| format!("distributor notice {} is unreadable: {e}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!(
            "distributor notice {} must be a regular file (symlinks are refused)",
            path.display()
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(max_bytes as u64 + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("distributor notice {} is unreadable: {e}", path.display()))?;
    if bytes.len() > max_bytes {
        return Err(format!(
            "distributor notice {} is larger than {} KB",
            path.display(),
            max_bytes / 1024
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("distributor notice {} is not valid UTF-8", path.display()))?;
    Ok(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
}

/// Rust source for `OUT_DIR/distributor_notice.rs`.
pub fn generate(notice: Option<&str>) -> String {
    let value = match notice {
        Some(html) => format!("Some({html:?})"),
        None => "None".to_string(),
    };
    format!(
        "/// Build-time sanitized distributor notice (`{NOTICE_PATH_ENV}`).\n\
         pub(crate) const COMPILED_NOTICE: Option<&str> = {value};\n"
    )
}
