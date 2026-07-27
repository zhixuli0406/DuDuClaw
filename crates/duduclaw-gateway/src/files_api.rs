//! Dashboard file panel — path resolution + validation (WP1.4).
//!
//! Pure, filesystem-facing helpers behind the two dashboard file endpoints
//! (`GET /api/files`, `GET /api/files/download`, wired in `server.rs`). Kept in
//! its own module so the path-traversal defenses are unit-testable without an
//! `AppState`/HTTP harness.
//!
//! Security stance (fail-closed): a download target must clear an input
//! allowlist (`is_safe_agent_id` + `is_safe_filename`) *and* survive a
//! `canonicalize()` containment check against the whitelisted attachments
//! directory. Any ambiguity — bad input, missing file, symlink escaping the
//! directory — is a DENY, never a fall-through to allow.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;

/// A single listable attachment file.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Bare filename (no directory component).
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Last-modified time as Unix epoch milliseconds (`0` when unavailable).
    pub mtime: u64,
}

/// True when `id` is a valid agent directory name.
///
/// Allowlist: ASCII alphanumeric + `-` + `_`, 1-64 chars. Blocks every
/// traversal character (`/`, `\`, `.`) and any Unicode surprise. Mirrors
/// `autopilot_engine::is_safe_agent_id` / `duduclaw-cli::is_valid_agent_id`.
pub fn is_safe_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// True when `name` is a *bare* filename safe to join onto a directory.
///
/// Rejects empty, over-long (>255 bytes), path separators (`/`, `\`), NUL,
/// and the `.` / `..` directory entries. CJK / Unicode filenames are allowed
/// (the canonicalize containment check in [`resolve_download`] is the real
/// backstop). Fail-closed — anything suspicious is rejected before it ever
/// reaches the filesystem.
pub fn is_safe_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && !name.contains('/')
        && !name.contains('\\')
        && !name.contains('\0')
        && name != "."
        && name != ".."
}

/// Resolve the whitelisted attachments directory for an optional agent id.
///
/// - `Some(agent)` → `<home>/agents/<agent>/attachments/`
/// - `None` → the shared fallback `<home>/attachments/`
///
/// Returns `None` when the agent id fails the allowlist (fail-closed). The
/// returned path is NOT guaranteed to exist — callers treat a missing
/// directory as "no files yet".
pub fn attachments_dir(home: &Path, agent: Option<&str>) -> Option<PathBuf> {
    match agent {
        Some(a) => {
            if !is_safe_agent_id(a) {
                return None;
            }
            Some(home.join("agents").join(a).join("attachments"))
        }
        None => Some(home.join("attachments")),
    }
}

/// List the regular files directly inside `dir`, newest-first.
///
/// A missing directory yields an empty vec (not an error): an agent that has
/// produced nothing yet simply has no attachments. Dotfiles and
/// subdirectories are skipped.
pub fn list_files(dir: &Path) -> Vec<FileEntry> {
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return out,
    };
    for entry in rd.flatten() {
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.push(FileEntry {
            name,
            size: meta.len(),
            mtime,
        });
    }
    // Newest first; stable tie-break on name.
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    out
}

/// Why a requested download could not be served.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    /// `name` failed the bare-filename allowlist.
    BadRequest,
    /// File does not exist or is not a regular file.
    NotFound,
    /// Canonicalized path escaped the whitelist directory (traversal /
    /// symlink) — fail-closed deny.
    Denied,
}

/// Resolve `dir/name` to a concrete, existing regular file guaranteed to be
/// contained by `dir` after canonicalization.
///
/// Defense in depth: the `is_safe_filename` allowlist already blocks path
/// separators, so `name` cannot traverse lexically; the `canonicalize()`
/// containment check additionally defeats a symlink *inside* `dir` that points
/// outside it. Both must pass.
pub fn resolve_download(dir: &Path, name: &str) -> Result<PathBuf, ResolveError> {
    if !is_safe_filename(name) {
        return Err(ResolveError::BadRequest);
    }
    let candidate = dir.join(name);
    // The whitelist directory must itself resolve (agent produced files ⇒ dir
    // exists). Canonicalize both sides and require containment.
    let canon_dir = std::fs::canonicalize(dir).map_err(|_| ResolveError::NotFound)?;
    let canon = std::fs::canonicalize(&candidate).map_err(|_| ResolveError::NotFound)?;
    if !canon.starts_with(&canon_dir) {
        return Err(ResolveError::Denied);
    }
    let meta = std::fs::metadata(&canon).map_err(|_| ResolveError::NotFound)?;
    if !meta.is_file() {
        return Err(ResolveError::NotFound);
    }
    Ok(canon)
}

/// Best-effort `Content-Type` from the filename extension. Unknown types fall
/// back to `application/octet-stream` (forces a download rather than guessing).
pub fn content_type_for(name: &str) -> &'static str {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "txt" | "log" | "md" => "text/plain; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "json" => "application/json",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "doc" => "application/msword",
        "xls" => "application/vnd.ms-excel",
        "ppt" => "application/vnd.ms-powerpoint",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Whether a file type is safe to render inline (`Content-Disposition: inline`)
/// for in-browser preview. Deliberately excludes `svg` (script vector) and
/// everything else — only formats browsers render natively and safely. All
/// other types get `attachment` (forced download).
pub fn is_inline_previewable(name: &str) -> bool {
    matches!(
        content_type_for(name),
        "application/pdf" | "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// Percent-encode `name` for an RFC 5987 `filename*=UTF-8''…` parameter so a
/// CJK / non-ASCII filename survives the `Content-Disposition` header intact.
pub fn encode_filename_star(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for &b in name.as_bytes() {
        // RFC 5987 attr-char: ALPHA / DIGIT / !#$&+-.^_`|~
        let ok = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'!' | b'#' | b'$' | b'&' | b'+' | b'-' | b'.' | b'^' | b'_' | b'`' | b'|' | b'~'
            );
        if ok {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn agent_id_allowlist() {
        assert!(is_safe_agent_id("assistant"));
        assert!(is_safe_agent_id("agent-1_x"));
        assert!(!is_safe_agent_id(""));
        assert!(!is_safe_agent_id("../etc"));
        assert!(!is_safe_agent_id("a/b"));
        assert!(!is_safe_agent_id("a.b")); // `.` blocked
        assert!(!is_safe_agent_id(&"x".repeat(65)));
    }

    #[test]
    fn filename_allowlist_rejects_traversal() {
        assert!(is_safe_filename("report.pdf"));
        assert!(is_safe_filename("彙總報告.docx")); // CJK allowed
        assert!(!is_safe_filename(""));
        assert!(!is_safe_filename("."));
        assert!(!is_safe_filename(".."));
        assert!(!is_safe_filename("../secret.txt"));
        assert!(!is_safe_filename("..\\secret.txt"));
        assert!(!is_safe_filename("sub/dir.pdf"));
        assert!(!is_safe_filename("a\0b.pdf"));
        assert!(!is_safe_filename(&format!("{}.pdf", "x".repeat(300))));
    }

    #[test]
    fn attachments_dir_agent_vs_shared() {
        let home = Path::new("/home/x");
        assert_eq!(
            attachments_dir(home, Some("bot")),
            Some(PathBuf::from("/home/x/agents/bot/attachments"))
        );
        assert_eq!(
            attachments_dir(home, None),
            Some(PathBuf::from("/home/x/attachments"))
        );
        // Malicious agent id ⇒ fail-closed None.
        assert_eq!(attachments_dir(home, Some("../../etc")), None);
        assert_eq!(attachments_dir(home, Some("a/b")), None);
    }

    #[test]
    fn resolve_download_happy_path_and_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("attachments");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("ok.pdf"), b"%PDF-1.4").unwrap();
        // A secret sitting OUTSIDE the whitelist dir.
        fs::write(tmp.path().join("secret.txt"), b"top secret").unwrap();

        // Happy path resolves inside the dir.
        let got = resolve_download(&dir, "ok.pdf").unwrap();
        assert!(got.ends_with("ok.pdf"));
        assert!(got.starts_with(fs::canonicalize(&dir).unwrap()));

        // Lexical traversal is rejected at the allowlist (contains `/`).
        assert_eq!(
            resolve_download(&dir, "../secret.txt"),
            Err(ResolveError::BadRequest)
        );
        // A bare name that doesn't exist ⇒ NotFound.
        assert_eq!(
            resolve_download(&dir, "nope.pdf"),
            Err(ResolveError::NotFound)
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_download_denies_symlink_escape() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("attachments");
        fs::create_dir_all(&dir).unwrap();
        let secret = tmp.path().join("secret.txt");
        fs::write(&secret, b"top secret").unwrap();
        // A symlink INSIDE the dir pointing OUTSIDE it — bare filename passes
        // the lexical allowlist, so canonicalize containment is the only line
        // of defense.
        symlink(&secret, dir.join("escape.txt")).unwrap();
        assert_eq!(
            resolve_download(&dir, "escape.txt"),
            Err(ResolveError::Denied)
        );
    }

    #[test]
    fn list_files_missing_dir_is_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(list_files(&tmp.path().join("nope")).is_empty());
    }

    #[test]
    fn list_files_sorts_and_skips_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("a.txt"), b"a").unwrap();
        fs::write(tmp.path().join(".hidden"), b"h").unwrap();
        fs::create_dir(tmp.path().join("sub")).unwrap();
        let files = list_files(tmp.path());
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "a.txt");
        assert_eq!(files[0].size, 1);
    }

    #[test]
    fn content_type_and_preview() {
        assert_eq!(content_type_for("x.pdf"), "application/pdf");
        assert_eq!(content_type_for("X.PNG"), "image/png");
        assert_eq!(content_type_for("data"), "application/octet-stream");
        assert!(is_inline_previewable("a.pdf"));
        assert!(is_inline_previewable("a.jpeg"));
        assert!(!is_inline_previewable("a.docx"));
        assert!(!is_inline_previewable("a.svg")); // XSS vector ⇒ forced download
    }

    #[test]
    fn filename_star_encoding() {
        assert_eq!(encode_filename_star("report.pdf"), "report.pdf");
        // CJK bytes are percent-encoded.
        assert_eq!(encode_filename_star("報告.pdf"), "%E5%A0%B1%E5%91%8A.pdf");
        assert_eq!(encode_filename_star("a b.pdf"), "a%20b.pdf");
    }
}
