//! Deterministic repo intake / threat-modeling signals (DESIGN §3.2 step 1).
//!
//! Everything here is zero-LLM and zero-network: a language census by file
//! extension, an entry-point heuristic, and a git-history hotspot scan
//! (high-churn files whose commit messages carry a security-flavored
//! keyword). All content read here (file paths, commit subjects) is DATA —
//! it is only ever counted/matched by fixed keyword lists, never executed or
//! fed to an LLM in this wave.
//!
//! No `walkdir` dependency — plain `std::fs` recursion (same convention as
//! `expert/safe_zip.rs`), with a fixed skip-list for vendor/build
//! directories and a symlink guard against cycles.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Directory names never descended into — either huge generated trees or
/// irrelevant to a security intake pass. Matched by exact basename.
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    ".nuxt",
    ".cargo",
    ".idea",
    ".vscode",
    "coverage",
    ".pytest_cache",
    ".mypy_cache",
    ".tox",
    ".gradle",
    ".terraform",
];

/// Safety valve against pathological trees (not a normal-repo limit — a
/// healthy repo is nowhere near this).
const MAX_WALK_FILES: usize = 200_000;
const MAX_WALK_DEPTH: usize = 16;

/// Recursively list repo-relative file paths (POSIX separators) under
/// `root`, skipping [`SKIP_DIR_NAMES`] and symlinks (cycle guard). The only
/// filesystem-touching function in this module — `census_languages` and
/// `detect_entry_points` are pure functions over the returned list so they
/// can be unit-tested with literal fixture data.
pub fn walk_repo_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    walk_dir(root, root, 0, &mut out);
    out
}

fn walk_dir(root: &Path, dir: &Path, depth: usize, out: &mut Vec<String>) {
    if depth > MAX_WALK_DEPTH || out.len() >= MAX_WALK_FILES {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        if out.len() >= MAX_WALK_FILES {
            return;
        }
        let path = entry.path();
        // symlink_metadata does not follow symlinks — used to detect (and
        // skip) them without risking a cycle via a followed metadata() call.
        let is_symlink = std::fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_symlink {
            continue;
        }
        let file_type = match entry.file_type() {
            Ok(t) => t,
            Err(_) => continue,
        };
        if file_type.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIP_DIR_NAMES.contains(&name.as_ref()) {
                continue;
            }
            walk_dir(root, &path, depth + 1, out);
        } else if file_type.is_file()
            && let Ok(rel) = path.strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LanguageStat {
    /// Lowercased extension without the leading dot, or `"(no-ext)"`.
    pub extension: String,
    pub file_count: usize,
}

/// Cap on how many language buckets ship in the report (long-tail noise cut).
const MAX_LANGUAGE_BUCKETS: usize = 25;

/// Group `files` (repo-relative paths) by lowercased extension, sorted by
/// count descending then extension ascending for a stable order.
pub fn census_languages(files: &[String]) -> Vec<LanguageStat> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in files {
        let ext = Path::new(f)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .unwrap_or_else(|| "(no-ext)".to_string());
        *counts.entry(ext).or_insert(0) += 1;
    }
    let mut stats: Vec<LanguageStat> = counts
        .into_iter()
        .map(|(extension, file_count)| LanguageStat { extension, file_count })
        .collect();
    stats.sort_by(|a, b| b.file_count.cmp(&a.file_count).then_with(|| a.extension.cmp(&b.extension)));
    stats.truncate(MAX_LANGUAGE_BUCKETS);
    stats
}

/// Fixed basename candidates for the entry-point heuristic (task spec:
/// "main.rs/index.ts/app.py 等"). Exact basename match, case-sensitive
/// (these are conventional names; case-insensitive matching would add noise
/// without adding real coverage).
const ENTRY_POINT_BASENAMES: &[&str] = &[
    "main.rs",
    "main.go",
    "index.ts",
    "index.js",
    "index.mjs",
    "main.ts",
    "main.js",
    "main.mjs",
    "app.ts",
    "app.js",
    "server.ts",
    "server.js",
    "app.py",
    "main.py",
    "manage.py",
    "wsgi.py",
    "asgi.py",
    "Main.java",
    "Application.java",
    "main.c",
    "main.cpp",
    "Program.cs",
    "main.rb",
];

const MAX_ENTRY_POINTS: usize = 50;

/// Filter `files` for basenames matching [`ENTRY_POINT_BASENAMES`], sorted
/// for a stable order and capped at [`MAX_ENTRY_POINTS`].
pub fn detect_entry_points(files: &[String]) -> Vec<String> {
    let mut hits: Vec<String> = files
        .iter()
        .filter(|f| {
            Path::new(f)
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| ENTRY_POINT_BASENAMES.contains(&n))
                .unwrap_or(false)
        })
        .cloned()
        .collect();
    hits.sort();
    hits.truncate(MAX_ENTRY_POINTS);
    hits
}

/// Commit-subject keywords that mark a commit as security-flavored (task
/// spec: "fix/security/vuln/CVE/injection/auth/overflow 等"). Matched via
/// `word_contains_ci` (whole-word, case-insensitive) so e.g. "auth" does not
/// false-positive inside "author".
pub const SECURITY_COMMIT_KEYWORDS: &[&str] = &[
    "fix",
    "security",
    "vuln",
    "vulnerability",
    "cve",
    "injection",
    "auth",
    "overflow",
    "xss",
    "csrf",
    "sqli",
    "rce",
    "exploit",
    "secret",
    "leak",
    "credential",
    "password",
    "unsafe",
    "sanitize",
    "backdoor",
    "bypass",
    "privilege",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HotspotFile {
    pub file: String,
    /// Times this file was touched by ANY commit in the scan window.
    pub total_touches: usize,
    /// Times this file was touched by a commit whose subject matched a
    /// [`SECURITY_COMMIT_KEYWORDS`] entry — the ranking key.
    pub security_touches: usize,
}

const MAX_HOTSPOTS: usize = 20;

/// One-record-per-commit marker this module's `git log` invocation uses to
/// delimit commit boundaries in `--name-only` output. `\x01` (SOH) never
/// appears in a commit subject in practice and is never treated specially by
/// the shell, so no quoting concerns at the call site.
pub const GIT_LOG_COMMIT_MARKER: &str = "\u{1}COMMIT\u{1}";

/// Parse `git log --name-only --pretty=format:<GIT_LOG_COMMIT_MARKER>%s`
/// output into per-file touch/security-touch counts, then rank into the top
/// [`MAX_HOTSPOTS`] files that have at least one security-flavored touch.
/// Pure function — testable with literal fixture text, no real git needed.
pub fn parse_git_log_hotspots(raw: &str) -> Vec<HotspotFile> {
    let mut totals: HashMap<String, usize> = HashMap::new();
    let mut security: HashMap<String, usize> = HashMap::new();
    let mut current_is_security = false;
    let mut in_commit = false;

    for line in raw.lines() {
        if let Some(subject) = line.strip_prefix(GIT_LOG_COMMIT_MARKER) {
            in_commit = true;
            current_is_security = SECURITY_COMMIT_KEYWORDS
                .iter()
                .any(|kw| duduclaw_core::word_contains_ci(subject, kw));
            continue;
        }
        let file = line.trim();
        if !in_commit || file.is_empty() {
            continue;
        }
        *totals.entry(file.to_string()).or_insert(0) += 1;
        if current_is_security {
            *security.entry(file.to_string()).or_insert(0) += 1;
        }
    }

    let mut hotspots: Vec<HotspotFile> = security
        .into_iter()
        .map(|(file, security_touches)| {
            let total_touches = totals.get(&file).copied().unwrap_or(security_touches);
            HotspotFile {
                file,
                total_touches,
                security_touches,
            }
        })
        .collect();
    hotspots.sort_by(|a, b| {
        b.security_touches
            .cmp(&a.security_touches)
            .then_with(|| b.total_touches.cmp(&a.total_touches))
            .then_with(|| a.file.cmp(&b.file))
    });
    hotspots.truncate(MAX_HOTSPOTS);
    hotspots
}

/// Git history availability. Honest-degradation per task spec: "git 不可用
/// 或非 git repo → 誠實降級，不失敗". Internally tagged (`status` field) so
/// the JSON shape is a flat, stable object instead of the default
/// externally-tagged `{"unavailable": {...}}` nesting — easier for report
/// consumers to pattern-match on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GitHistoryStatus {
    Available { hotspots: Vec<HotspotFile> },
    Unavailable { reason: String },
}

/// Run `git log --since=1.year.ago --no-merges --name-only` against
/// `repo_root` and reduce it to hotspots. Any failure (git missing, not a
/// repo, empty history) degrades to `Unavailable { reason }` — never an
/// `Err` that would abort the whole intake pass.
pub async fn compute_git_history(repo_root: &Path) -> GitHistoryStatus {
    if duduclaw_core::which_cli("git").is_none() {
        return GitHistoryStatus::Unavailable {
            reason: "git not found on PATH or common install locations".to_string(),
        };
    }
    if !repo_root.join(".git").exists() {
        return GitHistoryStatus::Unavailable {
            reason: "not a git repository (no .git found at repo root)".to_string(),
        };
    }

    // Bound to locals first so every element of the `.args([...])` array
    // literal is a plain `&str` — mixing `&str` literals with a `&Cow<str>`
    // (from `to_string_lossy()`) or an inline `&format!(...)` temporary
    // directly in the array does not type-unify.
    let repo_root_str = repo_root.to_string_lossy().into_owned();
    let pretty_arg = format!("--pretty=format:{GIT_LOG_COMMIT_MARKER}%s");
    let output = tokio::process::Command::new("git")
        .args([
            "-C",
            repo_root_str.as_str(),
            "log",
            "--since=1.year.ago",
            "--no-merges",
            "--name-only",
            pretty_arg.as_str(),
        ])
        .output()
        .await;

    match output {
        Ok(out) if out.status.success() => {
            let raw = String::from_utf8_lossy(&out.stdout);
            GitHistoryStatus::Available {
                hotspots: parse_git_log_hotspots(&raw),
            }
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            GitHistoryStatus::Unavailable {
                reason: format!(
                    "git log exited with {}: {}",
                    out.status,
                    duduclaw_core::truncate_bytes(stderr.trim(), 300)
                ),
            }
        }
        Err(e) => GitHistoryStatus::Unavailable {
            reason: format!("failed to spawn git: {e}"),
        },
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoProfile {
    pub language_census: Vec<LanguageStat>,
    pub entry_points: Vec<String>,
    pub git_history: GitHistoryStatus,
}

/// Build the full intake profile (`--profile deep` only — quick profile
/// skips this entirely per task spec).
pub async fn build_profile(repo_root: &Path) -> RepoProfile {
    let files = walk_repo_files(repo_root);
    let language_census = census_languages(&files);
    let entry_points = detect_entry_points(&files);
    let git_history = compute_git_history(repo_root).await;
    RepoProfile {
        language_census,
        entry_points,
        git_history,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── walk_repo_files ──────────────────────────────────────────────

    #[test]
    fn walk_skips_vendor_dirs_and_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();
        std::fs::write(dir.path().join("node_modules/pkg.js"), "//").unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/lib.rs"), "//").unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A symlink cycle back to root — must not hang or recurse forever.
            let _ = symlink(dir.path(), dir.path().join("src/loop"));
        }

        let files = walk_repo_files(dir.path());
        assert!(files.contains(&"main.rs".to_string()));
        assert!(files.contains(&"src/lib.rs".to_string()));
        assert!(!files.iter().any(|f| f.contains("node_modules")));
        assert!(!files.iter().any(|f| f.contains("loop")));
    }

    #[test]
    fn walk_uses_posix_separators() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/b.txt"), "x").unwrap();
        let files = walk_repo_files(dir.path());
        assert_eq!(files, vec!["a/b.txt".to_string()]);
    }

    // ── census_languages ─────────────────────────────────────────────

    #[test]
    fn census_counts_by_lowercased_extension() {
        let files = vec![
            "a.rs".to_string(),
            "b.RS".to_string(),
            "c.py".to_string(),
            "README".to_string(),
        ];
        let stats = census_languages(&files);
        let rs = stats.iter().find(|s| s.extension == "rs").unwrap();
        assert_eq!(rs.file_count, 2);
        let noext = stats.iter().find(|s| s.extension == "(no-ext)").unwrap();
        assert_eq!(noext.file_count, 1);
    }

    #[test]
    fn census_sorts_descending_by_count() {
        let files = vec!["a.rs".to_string(), "b.rs".to_string(), "c.py".to_string()];
        let stats = census_languages(&files);
        assert_eq!(stats[0].extension, "rs");
        assert_eq!(stats[0].file_count, 2);
    }

    // ── detect_entry_points ──────────────────────────────────────────

    #[test]
    fn entry_points_match_known_basenames_only() {
        let files = vec![
            "src/main.rs".to_string(),
            "crates/foo/src/main.rs".to_string(),
            "src/other.rs".to_string(),
            "web/src/index.ts".to_string(),
            "app.py".to_string(),
            "not_an_entry.py".to_string(),
        ];
        let hits = detect_entry_points(&files);
        assert!(hits.contains(&"src/main.rs".to_string()));
        assert!(hits.contains(&"crates/foo/src/main.rs".to_string()));
        assert!(hits.contains(&"web/src/index.ts".to_string()));
        assert!(hits.contains(&"app.py".to_string()));
        assert!(!hits.contains(&"src/other.rs".to_string()));
        assert!(!hits.contains(&"not_an_entry.py".to_string()));
    }

    // ── parse_git_log_hotspots ───────────────────────────────────────

    fn commit(subject: &str, files: &[&str]) -> String {
        let mut s = format!("{GIT_LOG_COMMIT_MARKER}{subject}\n");
        for f in files {
            s.push_str(f);
            s.push('\n');
        }
        s.push('\n');
        s
    }

    #[test]
    fn hotspots_only_include_files_touched_by_security_commits() {
        let raw = commit("chore: bump deps", &["package.json"])
            + &commit("fix: auth bypass in login", &["src/auth.rs", "src/login.rs"])
            + &commit("docs: typo", &["README.md"]);
        let hotspots = parse_git_log_hotspots(&raw);
        let files: Vec<&str> = hotspots.iter().map(|h| h.file.as_str()).collect();
        assert!(files.contains(&"src/auth.rs"));
        assert!(files.contains(&"src/login.rs"));
        assert!(!files.contains(&"package.json"));
        assert!(!files.contains(&"README.md"));
    }

    #[test]
    fn hotspots_rank_by_security_touch_count_desc() {
        let raw = commit("fix: sql injection", &["db.py"])
            + &commit("fix: another injection path", &["db.py"])
            + &commit("security: patch overflow", &["parser.c"]);
        let hotspots = parse_git_log_hotspots(&raw);
        assert_eq!(hotspots[0].file, "db.py");
        assert_eq!(hotspots[0].security_touches, 2);
        assert_eq!(hotspots[1].file, "parser.c");
        assert_eq!(hotspots[1].security_touches, 1);
    }

    #[test]
    fn hotspots_word_boundary_avoids_author_false_positive() {
        // "author" contains "auth" as a substring but must not match the
        // whole-word "auth" keyword (word_contains_ci discipline).
        let raw = commit("chore: update author list", &["AUTHORS.md"]);
        let hotspots = parse_git_log_hotspots(&raw);
        assert!(hotspots.is_empty());
    }

    #[test]
    fn hotspots_handles_empty_log() {
        assert!(parse_git_log_hotspots("").is_empty());
    }

    #[test]
    fn hotspots_ignore_stray_lines_before_first_commit_marker() {
        // Defensive: malformed/truncated output must not panic or misattribute.
        let raw = format!("garbage-line-with-no-marker\n{}", commit("fix: x", &["a.rs"]));
        let hotspots = parse_git_log_hotspots(&raw);
        assert_eq!(hotspots.len(), 1);
        assert_eq!(hotspots[0].file, "a.rs");
    }

    // ── compute_git_history (async, touches real git/fs) ────────────

    #[tokio::test]
    async fn git_history_unavailable_when_not_a_repo() {
        let dir = tempfile::tempdir().unwrap();
        let status = compute_git_history(dir.path()).await;
        match status {
            GitHistoryStatus::Unavailable { reason } => {
                assert!(reason.contains("not a git repository") || reason.contains("git not found"));
            }
            GitHistoryStatus::Available { .. } => panic!("expected Unavailable for a non-git dir"),
        }
    }

    #[test]
    fn git_history_status_serializes_with_tag() {
        let status = GitHistoryStatus::Unavailable {
            reason: "no git".to_string(),
        };
        let json = serde_json::to_value(&status).unwrap();
        assert_eq!(json["reason"], serde_json::json!("no git"));
    }
}
