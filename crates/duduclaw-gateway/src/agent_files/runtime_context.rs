//! The context files the scaffold writes for an employee's CLI (`CLAUDE.md`,
//! `AGENTS.md`), shown read-only next to the profile files.
//!
//! These are generated, not hand-edited: a person's rules belong in
//! `INSTRUCTIONS.md`. This module is the only reader and it applies the same
//! rules as the profile files: the agent directory comes from
//! [`super::agent_dir`] (valid id, no symlink, inside `<home>/agents`), and a
//! file is read only when it is a regular file that is not a symlink. Nothing
//! here writes.

use std::io::Read;
use std::path::Path;

/// Names shown, in display order. Closed list: never a caller-chosen path.
pub(crate) const RUNTIME_CONTEXT_FILES: [&str; 2] = ["CLAUDE.md", "AGENTS.md"];

/// At most this many bytes of one file are returned.
pub(crate) const MAX_RUNTIME_CONTEXT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RuntimeContextFile {
    pub name: &'static str,
    pub content: String,
    pub size: u64,
    pub truncated: bool,
}

/// Read one file; `Ok(None)` when absent, a symlink, or not a regular file.
fn read_one(dir: &Path, name: &'static str) -> Result<Option<RuntimeContextFile>, String> {
    let path = dir.join(name);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("cannot read {name}: {e}")),
    };
    if meta.file_type().is_symlink() || !meta.is_file() {
        return Ok(None);
    }
    let file = std::fs::File::open(&path).map_err(|e| format!("cannot read {name}: {e}"))?;
    let mut buf = Vec::new();
    file.take(MAX_RUNTIME_CONTEXT_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("cannot read {name}: {e}"))?;
    let truncated = buf.len() > MAX_RUNTIME_CONTEXT_BYTES;
    buf.truncate(MAX_RUNTIME_CONTEXT_BYTES);
    // A cut in the middle of a multi-byte character is dropped, not an error.
    let content = match String::from_utf8(buf) {
        Ok(s) => s,
        Err(e) => {
            let valid = e.utf8_error().valid_up_to();
            let bytes = e.into_bytes();
            if truncated {
                String::from_utf8_lossy(&bytes[..valid]).into_owned()
            } else {
                return Err(format!("{name} is not text"));
            }
        }
    };
    Ok(Some(RuntimeContextFile { name, content, size: meta.len(), truncated }))
}

/// The generated context files that exist in the employee directory `dir`
/// (already validated by [`super::agent_dir`]).
pub(crate) fn read_all(dir: &Path) -> Result<Vec<RuntimeContextFile>, String> {
    let mut out = Vec::new();
    for name in RUNTIME_CONTEXT_FILES {
        if let Some(f) = read_one(dir, name)? {
            out.push(f);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_regular_files_in_order_and_skips_absent() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("AGENTS.md"), "agents").unwrap();
        std::fs::write(d.path().join("CLAUDE.md"), "claude").unwrap();
        let got = read_all(d.path()).unwrap();
        assert_eq!(got.iter().map(|f| f.name).collect::<Vec<_>>(), ["CLAUDE.md", "AGENTS.md"]);
        assert_eq!(got[0].content, "claude");
        assert!(!got[0].truncated);

        let d2 = tempfile::tempdir().unwrap();
        assert!(read_all(d2.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_file_is_not_followed() {
        let d = tempfile::tempdir().unwrap();
        let secret = d.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();
        std::os::unix::fs::symlink(&secret, d.path().join("CLAUDE.md")).unwrap();
        assert!(read_all(d.path()).unwrap().is_empty());
    }

    #[test]
    fn directory_with_the_name_is_skipped() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("AGENTS.md")).unwrap();
        assert!(read_all(d.path()).unwrap().is_empty());
    }

    #[test]
    fn oversized_file_is_truncated_on_a_char_boundary() {
        let d = tempfile::tempdir().unwrap();
        // 3-byte chars so the 64 KiB cut lands mid-character.
        let text = "中".repeat(MAX_RUNTIME_CONTEXT_BYTES);
        std::fs::write(d.path().join("CLAUDE.md"), &text).unwrap();
        let got = read_all(d.path()).unwrap();
        assert!(got[0].truncated);
        assert!(got[0].content.len() <= MAX_RUNTIME_CONTEXT_BYTES);
        assert!(got[0].content.chars().all(|c| c == '中'));
        assert_eq!(got[0].size, text.len() as u64);
    }
}
