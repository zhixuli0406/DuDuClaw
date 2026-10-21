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
/// size ceiling. A leading UTF-8 byte-order mark is dropped. The file type is
/// checked on the opened handle (and the final component is opened with
/// `O_NOFOLLOW` on Unix), so a swap between check and read cannot slip in.
pub fn read_notice_file(path: &Path, max_bytes: usize) -> Result<String, String> {
    let unreadable =
        |e: std::io::Error| format!("distributor notice {} is unreadable: {e}", path.display());
    if std::fs::symlink_metadata(path)
        .map_err(unreadable)?
        .file_type()
        .is_symlink()
    {
        return Err(symlink_refused(path));
    }
    let file = open_no_follow(path).map_err(|e| {
        if is_symlink_error(&e) {
            symlink_refused(path)
        } else {
            unreadable(e)
        }
    })?;
    if !file.metadata().map_err(unreadable)?.is_file() {
        return Err(format!("distributor notice {} must be a regular file", path.display()));
    }
    let mut bytes = Vec::new();
    file.take(max_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
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

fn symlink_refused(path: &Path) -> String {
    format!(
        "distributor notice {} must be a regular file (symlinks are refused)",
        path.display()
    )
}

#[cfg(unix)]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_no_follow(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(unix)]
fn is_symlink_error(e: &std::io::Error) -> bool {
    e.raw_os_error() == Some(libc::ELOOP)
}

#[cfg(not(unix))]
fn is_symlink_error(_e: &std::io::Error) -> bool {
    false
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
