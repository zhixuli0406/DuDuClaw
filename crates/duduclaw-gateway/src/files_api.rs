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
///
/// The three provenance fields (I-2b) are filled from the artifacts ledger by
/// [`attach_provenance`]; a file with no ledger row keeps them `None`, which
/// the dashboard renders as 「來源不明」 rather than guessing a direction.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FileEntry {
    /// Bare filename (no directory component).
    pub name: String,
    /// File size in bytes.
    pub size: u64,
    /// Last-modified time as Unix epoch milliseconds (`0` when unavailable).
    pub mtime: u64,
    /// `declared` / `swept` / `uploaded` / `unknown` — see
    /// `artifacts::ArtifactOrigin`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// The task this file was delivered for, when the ledger recorded one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Goal-loop round, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub round: Option<u32>,
    /// The original filename before the `<ts>_` archive prefix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// True when `id` is a valid agent directory name.
///
/// WP-4I (2026-08): was an independent hand-rolled copy, byte-identical to
/// [`duduclaw_core::is_valid_agent_id`] (ASCII alphanumeric of either case +
/// `-` + `_`, 1-64 chars, blocking every traversal character and any Unicode
/// surprise) — now delegates there directly.
pub fn is_safe_agent_id(id: &str) -> bool {
    duduclaw_core::is_valid_agent_id(id)
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
            origin: None,
            task_id: None,
            round: None,
            display_name: None,
        });
    }
    // Newest first; stable tie-break on name.
    out.sort_by(|a, b| b.mtime.cmp(&a.mtime).then_with(|| a.name.cmp(&b.name)));
    out
}

/// I-2b: fill each listed file's provenance from the artifacts ledger index
/// (`archived_name → provenance`). Files with no row are left untouched — the
/// listing never invents an origin for a file whose history we do not have.
pub fn attach_provenance(
    files: &mut [FileEntry],
    index: &std::collections::BTreeMap<String, crate::artifacts::FileProvenance>,
) {
    for f in files.iter_mut() {
        let Some(p) = index.get(&f.name) else { continue };
        f.origin = Some(p.origin.as_str().to_string());
        f.task_id = p.task_id.clone();
        f.round = p.round;
        if !p.display_name.is_empty() {
            f.display_name = Some(p.display_name.clone());
        }
    }
}

/// I-4: `/files` search + task/date filters, applied AFTER
/// [`attach_provenance`] so `query` can match the ledger's display name and
/// origin, not just the raw on-disk archived name.
///
/// All fields optional and AND-combined; the zero-value filter
/// ([`FileListFilter::is_noop`]) is the identity function, so a plain
/// `/api/files?agent=` request is byte-identical to pre-I-4 behavior.
#[derive(Debug, Default, Clone)]
pub struct FileListFilter {
    /// Case-insensitive substring match against the archived name, display
    /// name, and origin (`declared`/`swept`/`uploaded`/`produced`/`unknown`)
    /// — "檔名/來源" per the I-4 design brief. Empty/whitespace-only is
    /// treated as absent so an accidental empty query string never turns
    /// into a "match nothing" surprise.
    pub query: Option<String>,
    /// Exact match against the ledger's `task_id` (I-2b `artifacts.jsonl`
    /// attribution — the same field `tasks.artifacts` reads). A file the
    /// ledger cannot tie to any task never matches a non-empty filter:
    /// fail-closed, no guessing, mirroring `collect_task_artifacts`'s
    /// attribution honesty.
    pub task_id: Option<String>,
    /// Inclusive lower bound on `mtime` (Unix epoch milliseconds — the same
    /// unit [`FileEntry::mtime`] already reports, so no new date-parsing
    /// surface is introduced).
    pub since_ms: Option<u64>,
    /// Inclusive upper bound on `mtime` (Unix epoch milliseconds).
    pub until_ms: Option<u64>,
}

impl FileListFilter {
    /// `true` when every field is absent — the filter changes nothing.
    fn is_noop(&self) -> bool {
        self.query.as_deref().map(str::trim).unwrap_or("").is_empty()
            && self.task_id.as_deref().map(str::trim).unwrap_or("").is_empty()
            && self.since_ms.is_none()
            && self.until_ms.is_none()
    }
}

/// Apply [`FileListFilter`] to an already-listed (and typically
/// provenance-attached) file set. Pure, order-preserving, and never widens
/// the result — an unparseable/empty filter field is simply not applied
/// rather than matching everything by accident.
pub fn filter_files(files: Vec<FileEntry>, filter: &FileListFilter) -> Vec<FileEntry> {
    if filter.is_noop() {
        return files;
    }
    let q_lower = filter
        .query
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_lowercase);
    let task_id = filter.task_id.as_deref().map(str::trim).filter(|s| !s.is_empty());

    files
        .into_iter()
        .filter(|f| {
            if let Some(q) = &q_lower {
                let haystack = format!(
                    "{} {} {}",
                    f.name,
                    f.display_name.as_deref().unwrap_or(""),
                    f.origin.as_deref().unwrap_or("")
                )
                .to_lowercase();
                if !haystack.contains(q.as_str()) {
                    return false;
                }
            }
            if let Some(tid) = task_id
                && f.task_id.as_deref() != Some(tid)
            {
                return false;
            }
            if let Some(since) = filter.since_ms
                && f.mtime < since
            {
                return false;
            }
            if let Some(until) = filter.until_ms
                && f.mtime > until
            {
                return false;
            }
            true
        })
        .collect()
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

/// Whether the file type can be converted to PDF for in-browser preview via
/// LibreOffice headless (`GET /api/files/preview`). Complements
/// [`is_inline_previewable`] — those types the browser renders natively and
/// need no conversion.
pub fn is_office_convertible(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "docx" | "doc" | "odt" | "xlsx" | "xls" | "ods" | "csv" | "pptx" | "ppt" | "odp"
    )
}

/// Locate the LibreOffice `soffice` binary. PATH lookup first, then the
/// standard install locations (launchd-launched gateways often run without a
/// user PATH). `None` = LibreOffice not installed → preview degrades to an
/// explicit error, never a broken byte stream.
pub fn find_soffice() -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in ["soffice", "soffice.exe"] {
                let p = dir.join(name);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    const CANDIDATES: &[&str] = &[
        "/Applications/LibreOffice.app/Contents/MacOS/soffice",
        "/usr/local/bin/soffice",
        "/opt/homebrew/bin/soffice",
        "/usr/bin/soffice",
        "/usr/bin/libreoffice",
        "C:\\Program Files\\LibreOffice\\program\\soffice.exe",
    ];
    CANDIDATES.iter().map(Path::new).find(|p| p.is_file()).map(Path::to_path_buf)
}

/// Cache directory for converted preview PDFs. Scoped per agent (or the
/// shared bucket) so identically-named files from different agents can't
/// collide: `<home>/cache/preview/<agent | _shared>/`.
pub fn preview_cache_dir(home: &Path, agent: Option<&str>) -> PathBuf {
    home.join("cache").join("preview").join(agent.unwrap_or("_shared"))
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
        // Smoke test that the delegation to `duduclaw_core::is_valid_agent_id`
        // (WP-4I 2026-08) is wired correctly; the exhaustive cases (empty,
        // traversal, CJK, over-length, mixed-case, underscore) live in that
        // function's own `agent_id_tests` module in duduclaw-core/src/lib.rs.
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
    fn provenance_is_attached_only_where_the_ledger_knows() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("1_a.docx"), b"a").unwrap();
        fs::write(tmp.path().join("2_b.pdf"), b"bb").unwrap();
        let mut files = list_files(tmp.path());

        let mut index = std::collections::BTreeMap::new();
        index.insert(
            "1_a.docx".to_string(),
            crate::artifacts::FileProvenance {
                origin: crate::artifacts::ArtifactOrigin::Declared,
                task_id: Some("task-1".into()),
                round: Some(2),
                display_name: "a.docx".into(),
                channel: Some("telegram".into()),
            },
        );
        attach_provenance(&mut files, &index);

        let a = files.iter().find(|f| f.name == "1_a.docx").unwrap();
        assert_eq!(a.origin.as_deref(), Some("declared"));
        assert_eq!(a.task_id.as_deref(), Some("task-1"));
        assert_eq!(a.round, Some(2));
        assert_eq!(a.display_name.as_deref(), Some("a.docx"));
        // Unknown to the ledger ⇒ left blank, never guessed.
        let b = files.iter().find(|f| f.name == "2_b.pdf").unwrap();
        assert!(b.origin.is_none() && b.task_id.is_none() && b.display_name.is_none());
    }

    fn entry(name: &str, mtime: u64, origin: Option<&str>, task_id: Option<&str>) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            size: 1,
            mtime,
            origin: origin.map(str::to_string),
            task_id: task_id.map(str::to_string),
            round: None,
            display_name: None,
        }
    }

    #[test]
    fn filter_noop_is_identity() {
        let files = vec![entry("a.pdf", 100, None, None)];
        let out = filter_files(files.clone(), &FileListFilter::default());
        assert_eq!(out, files);
        // Whitespace-only / empty strings are also a no-op, never a
        // "match nothing" trap.
        let filter = FileListFilter {
            query: Some("   ".into()),
            task_id: Some("".into()),
            ..Default::default()
        };
        assert_eq!(filter_files(files.clone(), &filter), files);
    }

    #[test]
    fn filter_by_query_matches_name_display_and_origin_case_insensitive() {
        let mut a = entry("1_a.docx", 100, Some("declared"), None);
        a.display_name = Some("報告.docx".into());
        let b = entry("2_b.pdf", 100, Some("uploaded"), None);
        let files = vec![a, b];

        // Matches on display name (CJK exact substring).
        let out = filter_files(
            files.clone(),
            &FileListFilter { query: Some("報告".into()), ..Default::default() },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "1_a.docx");

        // Matches on origin, case-insensitively.
        let out = filter_files(
            files.clone(),
            &FileListFilter { query: Some("UPLOADED".into()), ..Default::default() },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "2_b.pdf");

        // No match ⇒ honest empty, not a fallback to "everything".
        let out = filter_files(
            files,
            &FileListFilter { query: Some("nope".into()), ..Default::default() },
        );
        assert!(out.is_empty());
    }

    #[test]
    fn filter_by_task_id_is_exact_and_fail_closed() {
        let a = entry("1_a.docx", 100, Some("declared"), Some("task-1"));
        let b = entry("2_b.pdf", 100, Some("swept"), Some("task-2"));
        // No ledger row at all ⇒ never matches a non-empty task filter.
        let c = entry("3_c.txt", 100, None, None);
        let files = vec![a, b, c];

        let out = filter_files(
            files,
            &FileListFilter { task_id: Some("task-1".into()), ..Default::default() },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "1_a.docx");
    }

    #[test]
    fn filter_by_date_range_is_inclusive() {
        let files = vec![
            entry("old.pdf", 100, None, None),
            entry("mid.pdf", 200, None, None),
            entry("new.pdf", 300, None, None),
        ];
        let out = filter_files(
            files,
            &FileListFilter { since_ms: Some(200), until_ms: Some(300), ..Default::default() },
        );
        let names: Vec<&str> = out.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["mid.pdf", "new.pdf"]);
    }

    #[test]
    fn filter_combines_all_axes_with_and() {
        let mut a = entry("1_report.docx", 500, Some("declared"), Some("task-1"));
        a.display_name = Some("report.docx".into());
        let mut b = entry("2_report.docx", 50, Some("declared"), Some("task-1"));
        b.display_name = Some("report.docx".into()); // same name, outside date window
        let files = vec![a, b];

        let out = filter_files(
            files,
            &FileListFilter {
                query: Some("report".into()),
                task_id: Some("task-1".into()),
                since_ms: Some(400),
                until_ms: None,
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "1_report.docx");
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
