//! `CopyPolicy` — what a fork workspace copy may carry across a tree boundary.
//!
//! Every copy between a parent workspace and a branch workspace (materializing a
//! branch, promoting a winner, retaining a workspace for later manual selection)
//! goes through one policy so the two directions cannot drift apart:
//!
//! - **Symlinks are never followed.** Classification uses `symlink_metadata`.
//!   A symlink whose canonical target lies outside the source root is dropped
//!   (neither copied as content nor recreated). A symlink that stays inside the
//!   root is recreated as a symlink (absolute in-root targets are rewritten to
//!   the equivalent relative form so the copy never points back into the source
//!   tree). Dangling symlinks cannot be proven to stay inside, so they are
//!   dropped too (fail closed). On non-Unix hosts every symlink is dropped.
//! - **Hardlinks** are copied as plain files (a fresh inode).
//! - **Special files** (devices, FIFOs, sockets) are dropped.
//! - **Excluded names** (glob-ish patterns, ASCII case-insensitive) are matched
//!   against every path component; a matching file or directory is skipped
//!   entirely, and is never written into the destination.
//! - **Excluded paths** (exact relative paths, ASCII case-insensitive, matched
//!   component-wise from the copy root) skip exactly that entry — used by
//!   promotion so the agent-structure files at the root of an agent directory
//!   are never carried back, while a same-named file deeper in the tree is.
//!
//! When merging into an existing tree (promote), regular files are written to a
//! sibling temp file and renamed into place, so a destination entry that is a
//! symlink or a hardlink shared with a file elsewhere is *replaced*, never
//! written through.

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::error::{ForkError, Result};

/// Secret-bearing names excluded from every fork copy.
const SECRET_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    ".npmrc",
    ".pypirc",
    ".netrc",
];

/// Additional agent-identity / repository names excluded by [`CopyPolicy::strict`].
const STRICT_EXTRA_PATTERNS: &[&str] = &[
    ".git",
    ".mcp.json",
    ".claude",
    "SOUL.md",
    "CLAUDE.md",
    "agent.toml",
    "secrets",
];

/// Entries at the root of an agent directory that a branch must never carry
/// back into it on promotion: every [`duduclaw_core::AGENT_STRUCTURE_FILES`]
/// name plus the whole `.claude/` directory.
///
/// A branch workspace lives outside `<home>/agents/`, so the agent-file-guard
/// PreToolUse hook (and its `org_field_guard`) cannot tell a branch-side
/// `SOUL.md` / `CONTRACT.toml` / `agent.toml` / `.mcp.json` /
/// `.claude/settings.json` from a project file. Without this list a fork whose
/// parent is the agent's own directory could rewrite its persona, delete its
/// `must_not` boundaries, widen `[capabilities]` / change `reports_to`, swap
/// the MCP identity block, or unregister the hook itself — all by promotion.
/// The parent's own copies stay untouched; branches still read them, because
/// materializing a branch uses [`CopyPolicy::fork_default`].
const AGENT_DIR_EXTRA_ROOT_ENTRIES: &[&str] = &[".claude"];

/// Counts of what a copy did, for logging and tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CopyReport {
    pub files_copied: usize,
    pub symlinks_recreated: usize,
    pub symlinks_dropped: usize,
    pub special_dropped: usize,
    pub excluded: usize,
}

/// Rules applied to every fork workspace copy. See the module docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyPolicy {
    exclude: Vec<String>,
    /// Exact relative paths (lowercased components) skipped from the copy root.
    exclude_paths: Vec<Vec<String>>,
}

/// What to do with one symlink found in the source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LinkDecision {
    Drop,
    /// Recreate with this link text.
    Recreate(PathBuf),
}

impl CopyPolicy {
    /// A policy with an explicit exclude list (glob-ish: `*` and `?`).
    pub fn with_excludes<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        CopyPolicy {
            exclude: patterns.into_iter().map(Into::into).collect(),
            exclude_paths: Vec::new(),
        }
    }

    /// This policy plus an exact relative path (from the copy root) to skip.
    /// `rel` is split on `/`; matching is ASCII case-insensitive per component.
    fn with_excluded_path(mut self, rel: &str) -> Self {
        let comps: Vec<String> = rel
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .map(|c| c.to_ascii_lowercase())
            .collect();
        if !comps.is_empty() && !self.exclude_paths.contains(&comps) {
            self.exclude_paths.push(comps);
        }
        self
    }

    /// Default fork policy: excludes secrets only. `.claude/` and `.git` are kept
    /// because a fork branch still needs its hooks and its repository.
    pub fn fork_default() -> Self {
        Self::with_excludes(SECRET_PATTERNS.iter().copied())
    }

    /// Promotion policy for an **agent directory** parent (branch → parent):
    /// [`Self::fork_default`] plus, at the copy root only, every
    /// [`duduclaw_core::AGENT_STRUCTURE_FILES`] name and the whole `.claude/`
    /// directory (see `AGENT_DIR_EXTRA_ROOT_ENTRIES`). A same-named file in a
    /// subdirectory (`docs/CLAUDE.md`) is still promoted. Materializing a
    /// branch keeps using `fork_default`, so a branch can still read them.
    ///
    /// This is also the fail-closed choice when the parent's kind is unknown;
    /// callers that know the DuDuClaw home use [`Self::promote_for_parent`].
    pub fn promote_default() -> Self {
        duduclaw_core::AGENT_STRUCTURE_FILES
            .iter()
            .chain(AGENT_DIR_EXTRA_ROOT_ENTRIES)
            .fold(Self::fork_default(), |policy, name| policy.with_excluded_path(name))
    }

    /// Promotion policy chosen by what the parent workspace is:
    ///
    /// - an agent directory (`<home>/agents/<id>` or
    ///   `<home>/agents/.ephemeral/<id>`) → [`Self::promote_default`];
    /// - an ancestor of `<home>/agents` (the DuDuClaw home itself, or a
    ///   directory above it) → [`Self::fork_default`] plus the whole agents
    ///   tree, since any path below it would land in some agent's directory;
    /// - any other directory (an ordinary project, a subdirectory of an agent
    ///   directory) → [`Self::fork_default`], so a project file that happens to
    ///   be named `CLAUDE.md` or `agent.toml` is promoted normally;
    /// - `<home>/agents` itself or `<home>/agents/.ephemeral` → nothing is
    ///   promoted (every child is an agent directory);
    /// - undeterminable (the parent cannot be canonicalized, or `<home>/agents`
    ///   exists but cannot be) → [`Self::promote_default`] (fail closed).
    pub fn promote_for_parent(parent: &Path, home: &Path) -> Self {
        match classify_parent(parent, home) {
            ParentKind::Project => Self::fork_default(),
            ParentKind::AgentsAncestor(rel) => Self::fork_default().with_excluded_path(&rel),
            ParentKind::AgentDir | ParentKind::Unknown => Self::promote_default(),
            // Every child of the agents root is some agent's directory; there
            // is no legitimate promotion into it, so nothing is carried back.
            ParentKind::AgentsRoot => Self::with_excludes(["*"]),
        }
    }

    /// Strict policy: secrets plus repository and agent-identity files.
    pub fn strict() -> Self {
        Self::with_excludes(SECRET_PATTERNS.iter().chain(STRICT_EXTRA_PATTERNS).copied())
    }

    /// The exclude patterns in effect.
    pub fn excludes(&self) -> &[String] {
        &self.exclude
    }

    /// Whether a single path component / file name is excluded.
    pub fn is_excluded(&self, name: &OsStr) -> bool {
        let name = name.to_string_lossy();
        name.eq_ignore_ascii_case(crate::publication::PUBLICATION_LOCK_NAME)
            || name.eq_ignore_ascii_case("fork_resolution.lock.lock")
            || name.eq_ignore_ascii_case("fork_recovery")
            || glob_match_ci(".duduclaw-fork-locks-*", &name)
            || self.exclude.iter().any(|p| glob_match_ci(p, &name))
    }

    /// Whether any normal component of a relative path is excluded, or the
    /// path lies at or below one of the exact excluded paths.
    fn path_has_excluded_component(&self, rel: &Path) -> bool {
        rel.components().any(|c| match c {
            Component::Normal(n) => self.is_excluded(n),
            _ => false,
        }) || self.is_excluded_path_prefix(rel)
    }

    /// Whether `rel` (relative to the copy root) equals or lies below one of
    /// the exact excluded paths.
    fn is_excluded_path_prefix(&self, rel: &Path) -> bool {
        if self.exclude_paths.is_empty() {
            return false;
        }
        let comps: Vec<String> = rel
            .components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n.to_string_lossy().to_ascii_lowercase()),
                _ => None,
            })
            .collect();
        self.exclude_paths
            .iter()
            .any(|p| comps.len() >= p.len() && comps[..p.len()] == p[..])
    }

    /// Copy the tree `src` into `dst` (created if missing), applying the policy.
    ///
    /// Works both for materializing a fresh copy and for merging into an existing
    /// tree: existing destination files are replaced (temp + rename), never
    /// written through, and nothing excluded or escaping is ever written.
    pub fn copy_tree(&self, src: &Path, dst: &Path) -> Result<CopyReport> {
        let root = src
            .canonicalize()
            .map_err(|e| ForkError::Overlay(format!("canonicalize {}: {e}", src.display())))?;
        let mut report = CopyReport::default();
        // The destination root is the caller's choice (it may legitimately be
        // reached through a symlink); only entries *below* it are guarded.
        if !dst.is_dir() {
            std::fs::create_dir_all(dst)
                .map_err(|e| ForkError::Overlay(format!("create {}: {e}", dst.display())))?;
        }
        self.copy_dir(&root, src, dst, Path::new(""), &mut report)?;
        tracing::debug!(?report, "fork copy {} -> {}", src.display(), dst.display());
        Ok(report)
    }

    /// Post-pass over a tree cloned from `src` into `dst` by an external tool
    /// (native CoW `cp`), removing everything [`Self::copy_tree`] would not have
    /// produced: excluded names, escaping or dangling symlinks, special files.
    /// In-root absolute symlinks are rewritten to their relative form so both
    /// backends end in the same state.
    pub fn sanitize_clone(&self, src: &Path, dst: &Path) -> Result<CopyReport> {
        let root = src
            .canonicalize()
            .map_err(|e| ForkError::Overlay(format!("canonicalize {}: {e}", src.display())))?;
        let is_real_dir = std::fs::symlink_metadata(dst)
            .map(|m| m.file_type().is_dir())
            .unwrap_or(false);
        if !is_real_dir {
            return Err(ForkError::Overlay(format!(
                "cloned workspace root is not a real directory: {}",
                dst.display()
            )));
        }
        let mut report = CopyReport::default();
        self.sanitize_dir(&root, src, dst, Path::new(""), &mut report)?;
        Ok(report)
    }

    fn copy_dir(
        &self,
        root: &Path,
        src_root: &Path,
        dst_root: &Path,
        rel: &Path,
        report: &mut CopyReport,
    ) -> Result<()> {
        let src_dir = src_root.join(rel);
        let dst_dir = dst_root.join(rel);
        for entry in std::fs::read_dir(&src_dir)
            .map_err(|e| ForkError::Overlay(format!("read_dir {}: {e}", src_dir.display())))?
        {
            let entry = entry
                .map_err(|e| ForkError::Overlay(format!("dir entry in {}: {e}", src_dir.display())))?;
            let name = entry.file_name();
            let child_rel = rel.join(&name);
            if self.is_excluded(&name) || self.is_excluded_path_prefix(&child_rel) {
                report.excluded += 1;
                continue;
            }
            let from = src_dir.join(&name);
            let to = dst_dir.join(&name);
            // Never `metadata` (which follows links) — classify the entry itself.
            let meta = std::fs::symlink_metadata(&from)
                .map_err(|e| ForkError::Overlay(format!("lstat {}: {e}", from.display())))?;
            let ft = meta.file_type();

            if ft.is_symlink() {
                match self.classify_link(root, &from, &child_rel) {
                    LinkDecision::Drop => report.symlinks_dropped += 1,
                    LinkDecision::Recreate(target) => {
                        if place_symlink(&target, &to)? {
                            report.symlinks_recreated += 1;
                        } else {
                            report.symlinks_dropped += 1;
                        }
                    }
                }
            } else if ft.is_dir() {
                ensure_real_dir(&to)?;
                self.copy_dir(root, src_root, dst_root, &child_rel, report)?;
            } else if ft.is_file() {
                // Regular file, including a hardlink: a fresh plain file.
                place_file(&from, &to)?;
                report.files_copied += 1;
            } else {
                report.special_dropped += 1;
            }
        }
        Ok(())
    }

    fn sanitize_dir(
        &self,
        root: &Path,
        src_root: &Path,
        dst_root: &Path,
        rel: &Path,
        report: &mut CopyReport,
    ) -> Result<()> {
        let dir = dst_root.join(rel);
        for entry in std::fs::read_dir(&dir)
            .map_err(|e| ForkError::Overlay(format!("read_dir {}: {e}", dir.display())))?
        {
            let entry = entry
                .map_err(|e| ForkError::Overlay(format!("dir entry in {}: {e}", dir.display())))?;
            let name = entry.file_name();
            let child_rel = rel.join(&name);
            let path = dir.join(&name);
            let meta = std::fs::symlink_metadata(&path)
                .map_err(|e| ForkError::Overlay(format!("lstat {}: {e}", path.display())))?;
            let ft = meta.file_type();

            if self.is_excluded(&name) || self.is_excluded_path_prefix(&child_rel) {
                remove_entry(&path, ft.is_dir() && !ft.is_symlink())?;
                report.excluded += 1;
                continue;
            }
            if ft.is_symlink() {
                // Judge the link as it sits in the *source* tree: the clone copied
                // the link text verbatim.
                let src_link = src_root.join(&child_rel);
                match self.classify_link(root, &src_link, &child_rel) {
                    LinkDecision::Drop => {
                        remove_entry(&path, false)?;
                        report.symlinks_dropped += 1;
                    }
                    LinkDecision::Recreate(target) => {
                        let current = std::fs::read_link(&path).ok();
                        if current.as_deref() != Some(target.as_path()) {
                            remove_entry(&path, false)?;
                            if place_symlink(&target, &path)? {
                                report.symlinks_recreated += 1;
                            } else {
                                report.symlinks_dropped += 1;
                            }
                        } else {
                            report.symlinks_recreated += 1;
                        }
                    }
                }
            } else if ft.is_dir() {
                self.sanitize_dir(root, src_root, dst_root, &child_rel, report)?;
            } else if ft.is_file() {
                report.files_copied += 1;
            } else {
                remove_entry(&path, false)?;
                report.special_dropped += 1;
            }
        }
        Ok(())
    }

    /// Decide whether a source symlink may be recreated in the copy.
    ///
    /// `root` is the canonical source root, `link` the symlink's path in the
    /// source tree, `rel` its path relative to the source root.
    fn classify_link(&self, root: &Path, link: &Path, rel: &Path) -> LinkDecision {
        if !cfg!(unix) {
            // Recreating a link needs file-vs-dir knowledge and privileges on
            // Windows; dropping is the fail-closed choice.
            return LinkDecision::Drop;
        }
        let raw = match std::fs::read_link(link) {
            Ok(t) => t,
            Err(_) => return LinkDecision::Drop,
        };
        let link_dir = link.parent().unwrap_or(Path::new(""));
        let resolved = if raw.is_absolute() { raw.clone() } else { link_dir.join(&raw) };
        // Dangling ⇒ cannot prove containment ⇒ drop.
        let canon = match resolved.canonicalize() {
            Ok(c) => c,
            Err(_) => return LinkDecision::Drop,
        };
        let inside = match canon.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => return LinkDecision::Drop,
        };
        // A link that resolves onto an excluded name would re-expose it.
        if self.path_has_excluded_component(&inside) {
            return LinkDecision::Drop;
        }
        if raw.is_absolute() {
            // Rewrite to the equivalent relative form: `../` per directory level
            // of the link's own location, then the in-root target path.
            let depth = rel
                .parent()
                .map(|p| p.components().filter(|c| matches!(c, Component::Normal(_))).count())
                .unwrap_or(0);
            let mut rewritten = PathBuf::new();
            for _ in 0..depth {
                rewritten.push("..");
            }
            if inside.as_os_str().is_empty() {
                rewritten.push(".");
            } else {
                rewritten.push(&inside);
            }
            LinkDecision::Recreate(rewritten)
        } else {
            LinkDecision::Recreate(raw)
        }
    }
}

/// What a promotion parent is, relative to a DuDuClaw home.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ParentKind {
    AgentDir,
    /// `<home>/agents` itself or `<home>/agents/.ephemeral`: every child is an
    /// agent directory.
    AgentsRoot,
    /// Ancestor of `<home>/agents`; carries the `/`-joined relative path to it.
    AgentsAncestor(String),
    Project,
    Unknown,
}

fn classify_parent(parent: &Path, home: &Path) -> ParentKind {
    let Ok(parent) = parent.canonicalize() else {
        return ParentKind::Unknown;
    };
    let agents = match home.join("agents").canonicalize() {
        Ok(a) => a,
        // No agents tree at all: nothing to protect, the parent is a project.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return ParentKind::Project,
        Err(_) => return ParentKind::Unknown,
    };
    let lower = |p: &Path| -> Vec<String> {
        p.components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n.to_string_lossy().to_ascii_lowercase()),
                _ => None,
            })
            .collect()
    };
    // Case-insensitive component comparison: the default macOS / Windows
    // filesystems treat a case variant as the same directory.
    let (p, a) = (lower(&parent), lower(&agents));
    if p.len() > a.len() && p[..a.len()] == a[..] {
        let rest = &p[a.len()..];
        return match rest {
            [eph] if eph == ".ephemeral" => ParentKind::AgentsRoot,
            [id] if !id.starts_with('.') => ParentKind::AgentDir,
            [eph, id] if eph == ".ephemeral" && !id.starts_with('.') => ParentKind::AgentDir,
            // A subdirectory of an agent directory (or of `.ephemeral`): its
            // root is not the agent root.
            _ => ParentKind::Project,
        };
    }
    if p == a {
        return ParentKind::AgentsRoot;
    }
    if p.len() < a.len() && a[..p.len()] == p[..] {
        // Use the real (case-preserving) remainder of the agents path.
        let rel: Vec<String> = agents
            .components()
            .filter_map(|c| match c {
                Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                _ => None,
            })
            .skip(p.len())
            .collect();
        return ParentKind::AgentsAncestor(rel.join("/"));
    }
    ParentKind::Project
}

/// Create `path` as a real directory. An existing symlink at `path` is removed
/// first so a copy can never descend through it into another tree.
fn ensure_real_dir(path: &Path) -> Result<()> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        let ft = meta.file_type();
        if ft.is_dir() {
            return Ok(());
        }
        if ft.is_symlink() {
            std::fs::remove_file(path)
                .map_err(|e| ForkError::Overlay(format!("remove symlink {}: {e}", path.display())))?;
        } else {
            return Err(ForkError::Overlay(format!(
                "cannot merge a directory over a non-directory: {}",
                path.display()
            )));
        }
    }
    std::fs::create_dir_all(path)
        .map_err(|e| ForkError::Overlay(format!("create {}: {e}", path.display())))
}

/// Write the contents of regular file `from` to `to`. When `to` already exists
/// the bytes go to a sibling temp file that is renamed over it, which replaces
/// a symlink or hardlink at `to` instead of writing through it.
fn place_file(from: &Path, to: &Path) -> Result<()> {
    match std::fs::symlink_metadata(to) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| ForkError::Overlay(format!("copy {} -> {}: {e}", from.display(), to.display()))),
        Err(e) => Err(ForkError::Overlay(format!("lstat {}: {e}", to.display()))),
        Ok(meta) if meta.file_type().is_dir() => Err(ForkError::Overlay(format!(
            "cannot merge a file over a directory: {}",
            to.display()
        ))),
        Ok(_) => {
            let dir = to.parent().unwrap_or(Path::new("."));
            let tmp = dir.join(format!(".duduclaw-fork-{}.tmp", uuid::Uuid::new_v4().simple()));
            let res = std::fs::copy(from, &tmp).and_then(|_| std::fs::rename(&tmp, to));
            if let Err(e) = res {
                let _ = std::fs::remove_file(&tmp);
                return Err(ForkError::Overlay(format!(
                    "replace {} with {}: {e}",
                    to.display(),
                    from.display()
                )));
            }
            Ok(())
        }
    }
}

/// Create symlink `to -> target`. Returns `Ok(false)` (skipped) when `to` is an
/// existing real directory: an additive merge never replaces a directory.
fn place_symlink(target: &Path, to: &Path) -> Result<bool> {
    if let Ok(meta) = std::fs::symlink_metadata(to) {
        if meta.file_type().is_dir() {
            tracing::warn!("fork copy: not replacing directory {} with a symlink", to.display());
            return Ok(false);
        }
        std::fs::remove_file(to)
            .map_err(|e| ForkError::Overlay(format!("remove {}: {e}", to.display())))?;
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, to).map_err(|e| {
            ForkError::Overlay(format!("symlink {} -> {}: {e}", to.display(), target.display()))
        })?;
        Ok(true)
    }
    #[cfg(not(unix))]
    {
        // Unreachable in practice: `classify_link` drops every link off Unix.
        let _ = target;
        Ok(false)
    }
}

fn remove_entry(path: &Path, is_real_dir: bool) -> Result<()> {
    let res = if is_real_dir { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    res.map_err(|e| ForkError::Overlay(format!("remove {}: {e}", path.display())))
}

/// Glob-ish match supporting `*` (any run) and `?` (one char), ASCII
/// case-insensitive (secret names must not slip through on a case variant,
/// and case-insensitive filesystems treat them as the same file anyway).
fn glob_match_ci(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let n: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn glob_matching() {
        assert!(glob_match_ci(".env", ".env"));
        assert!(glob_match_ci(".env", ".ENV"));
        assert!(!glob_match_ci(".env", ".envrc"));
        assert!(glob_match_ci(".env.*", ".env.local"));
        assert!(glob_match_ci("*.pem", "server.pem"));
        assert!(!glob_match_ci("*.pem", "server.pem.txt"));
        assert!(glob_match_ci("id_rsa*", "id_rsa.pub"));
        assert!(glob_match_ci("a?c", "abc"));
        assert!(!glob_match_ci("a?c", "ac"));
    }

    #[test]
    fn promotion_never_carries_contract_or_soul_back_into_the_parent() {
        let branch = tempfile::tempdir().unwrap();
        let parent = tempfile::tempdir().unwrap();
        fs::write(parent.path().join("CONTRACT.toml"), "must_not = [\"x\"]").unwrap();
        fs::write(parent.path().join("SOUL.md"), "persona").unwrap();
        fs::write(branch.path().join("CONTRACT.toml"), "").unwrap();
        fs::write(branch.path().join("SOUL.md"), "rewritten").unwrap();
        fs::write(branch.path().join("result.txt"), "work").unwrap();
        let report = CopyPolicy::promote_default().copy_tree(branch.path(), parent.path()).unwrap();
        assert_eq!(report.excluded, 2);
        assert_eq!(fs::read_to_string(parent.path().join("CONTRACT.toml")).unwrap(), "must_not = [\"x\"]");
        assert_eq!(fs::read_to_string(parent.path().join("SOUL.md")).unwrap(), "persona");
        assert_eq!(fs::read_to_string(parent.path().join("result.txt")).unwrap(), "work");
    }

    #[test]
    fn presets() {
        let d = CopyPolicy::fork_default();
        assert!(d.is_excluded(OsStr::new(".env")));
        assert!(d.is_excluded(OsStr::new("id_ed25519")));
        assert!(!d.is_excluded(OsStr::new(".git")));
        assert!(!d.is_excluded(OsStr::new(".claude")));
        let s = CopyPolicy::strict();
        assert!(s.is_excluded(OsStr::new(".git")));
        assert!(s.is_excluded(OsStr::new("SOUL.md")));
        assert!(s.is_excluded(OsStr::new("secrets")));
        assert!(s.is_excluded(OsStr::new(".netrc")));
        let p = CopyPolicy::promote_default();
        for root_entry in duduclaw_core::AGENT_STRUCTURE_FILES.iter().chain(&[".claude"]) {
            assert!(p.is_excluded_path_prefix(Path::new(root_entry)), "{root_entry}");
            assert!(
                p.is_excluded_path_prefix(Path::new(&root_entry.to_ascii_uppercase())),
                "case variant of {root_entry}"
            );
            assert!(!p.is_excluded_path_prefix(&Path::new("docs").join(root_entry)), "nested {root_entry}");
            assert!(!d.is_excluded_path_prefix(Path::new(root_entry)), "branches still see {root_entry}");
        }
        assert!(p.is_excluded_path_prefix(Path::new(".claude/settings.json")));
        assert!(p.is_excluded(OsStr::new(".env")));
        assert!(!p.is_excluded(OsStr::new("result.txt")));
        assert!(!p.is_excluded_path_prefix(Path::new("result.txt")));
    }

    /// Lay out `<home>/agents/<id>` plus a project dir, returning (home, agent, project).
    fn home_layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let agent = home.path().join("agents").join("a1");
        fs::create_dir_all(&agent).unwrap();
        fs::create_dir_all(home.path().join("agents/.ephemeral/e1")).unwrap();
        let project = home.path().join("projects/app");
        fs::create_dir_all(&project).unwrap();
        (home, agent, project)
    }

    /// Branch with every agent-structure file, a hook registration, a nested
    /// same-named file and an ordinary result.
    fn hostile_branch() -> tempfile::TempDir {
        let branch = tempfile::tempdir().unwrap();
        for name in duduclaw_core::AGENT_STRUCTURE_FILES {
            fs::write(branch.path().join(name), "branch").unwrap();
        }
        fs::create_dir_all(branch.path().join(".claude")).unwrap();
        fs::write(branch.path().join(".claude/settings.json"), "{}").unwrap();
        fs::create_dir_all(branch.path().join("docs")).unwrap();
        fs::write(branch.path().join("docs/CLAUDE.md"), "nested").unwrap();
        fs::write(branch.path().join("result.txt"), "work").unwrap();
        branch
    }

    #[test]
    fn agent_dir_parent_never_receives_agent_structure_files_or_claude_dir() {
        let (home, agent, _) = home_layout();
        for name in duduclaw_core::AGENT_STRUCTURE_FILES {
            fs::write(agent.join(name), "parent").unwrap();
        }
        let branch = hostile_branch();
        let policy = CopyPolicy::promote_for_parent(&agent, home.path());
        assert_eq!(policy, CopyPolicy::promote_default());
        let report = policy.copy_tree(branch.path(), &agent).unwrap();
        for name in duduclaw_core::AGENT_STRUCTURE_FILES {
            assert_eq!(fs::read_to_string(agent.join(name)).unwrap(), "parent", "{name}");
        }
        assert!(!agent.join(".claude").exists());
        assert_eq!(report.excluded, duduclaw_core::AGENT_STRUCTURE_FILES.len() + 1);
        assert_eq!(fs::read_to_string(agent.join("docs/CLAUDE.md")).unwrap(), "nested");
        assert_eq!(fs::read_to_string(agent.join("result.txt")).unwrap(), "work");
        // Ephemeral agent directories are agent directories too.
        let eph = home.path().join("agents/.ephemeral/e1");
        assert_eq!(CopyPolicy::promote_for_parent(&eph, home.path()), CopyPolicy::promote_default());
    }

    #[test]
    fn project_parent_promotes_same_named_files_normally() {
        let (home, _, project) = home_layout();
        let branch = hostile_branch();
        let policy = CopyPolicy::promote_for_parent(&project, home.path());
        assert_eq!(policy, CopyPolicy::fork_default());
        policy.copy_tree(branch.path(), &project).unwrap();
        assert_eq!(fs::read_to_string(project.join("CLAUDE.md")).unwrap(), "branch");
        assert!(project.join(".claude/settings.json").is_file());
        // A subdirectory of an agent directory is not the agent root either.
        let sub = home.path().join("agents/a1/work");
        fs::create_dir_all(&sub).unwrap();
        assert_eq!(CopyPolicy::promote_for_parent(&sub, home.path()), CopyPolicy::fork_default());
    }

    #[test]
    fn home_and_agents_root_parents_cannot_reach_an_agent_directory() {
        let (home, agent, _) = home_layout();
        fs::write(agent.join("SOUL.md"), "parent").unwrap();
        let branch = tempfile::tempdir().unwrap();
        fs::create_dir_all(branch.path().join("agents/a1")).unwrap();
        fs::write(branch.path().join("agents/a1/SOUL.md"), "branch").unwrap();
        fs::write(branch.path().join("result.txt"), "work").unwrap();
        CopyPolicy::promote_for_parent(home.path(), home.path())
            .copy_tree(branch.path(), home.path())
            .unwrap();
        assert_eq!(fs::read_to_string(agent.join("SOUL.md")).unwrap(), "parent");
        assert_eq!(fs::read_to_string(home.path().join("result.txt")).unwrap(), "work");

        let agents_root = home.path().join("agents");
        let inner = tempfile::tempdir().unwrap();
        fs::create_dir_all(inner.path().join("a1")).unwrap();
        fs::write(inner.path().join("a1/SOUL.md"), "branch").unwrap();
        let report = CopyPolicy::promote_for_parent(&agents_root, home.path())
            .copy_tree(inner.path(), &agents_root)
            .unwrap();
        assert_eq!(report.files_copied, 0);
        assert_eq!(fs::read_to_string(agent.join("SOUL.md")).unwrap(), "parent");
    }

    #[test]
    fn unknown_parent_fails_closed_and_missing_agents_tree_is_a_project() {
        let home = tempfile::tempdir().unwrap();
        let missing = home.path().join("does-not-exist");
        assert_eq!(CopyPolicy::promote_for_parent(&missing, home.path()), CopyPolicy::promote_default());
        let project = tempfile::tempdir().unwrap();
        assert_eq!(CopyPolicy::promote_for_parent(project.path(), home.path()), CopyPolicy::fork_default());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_onto_a_protected_root_entry_is_not_recreated() {
        let branch = tempfile::tempdir().unwrap();
        fs::write(branch.path().join("SOUL.md"), "x").unwrap();
        std::os::unix::fs::symlink("SOUL.md", branch.path().join("notes.md")).unwrap();
        let parent = tempfile::tempdir().unwrap();
        CopyPolicy::promote_default().copy_tree(branch.path(), parent.path()).unwrap();
        assert!(fs::symlink_metadata(parent.path().join("notes.md")).is_err());
        assert!(!parent.path().join("SOUL.md").exists());
    }

    #[test]
    fn reserved_host_locks_cannot_be_copied_or_overwritten_through_case_variants() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        let names = ["FORK_RESOLUTION.LOCK.LOCK", ".DUDUCLAW-FORK-PUBLICATION.LOCK", ".DUDUCLAW-FORK-LOCKS-42", "FORK_RECOVERY"];
        for depth in ["", "nested/"] {
            fs::create_dir_all(src.path().join(depth)).unwrap();
            fs::create_dir_all(dst.path().join(depth)).unwrap();
            for name in names {
                fs::write(src.path().join(format!("{depth}{name}")), "branch replacement").unwrap();
                fs::write(dst.path().join(format!("{depth}{name}")), "host lock").unwrap();
            }
        }
        let policy = CopyPolicy::with_excludes(std::iter::empty::<String>());
        policy.copy_tree(src.path(), dst.path()).unwrap();
        for depth in ["", "nested/"] {
            for name in names {
                assert_eq!(fs::read_to_string(dst.path().join(format!("{depth}{name}"))).unwrap(), "host lock");
            }
        }
        let fresh = tempfile::tempdir().unwrap();
        policy.copy_tree(src.path(), fresh.path()).unwrap();
        for name in names {
            assert!(!fresh.path().join(name).exists());
            assert!(!fresh.path().join("nested").join(name).exists());
        }
    }

    #[test]
    fn excluded_names_are_skipped_at_any_depth() {
        let src = tempfile::tempdir().unwrap();
        let dst = tempfile::tempdir().unwrap();
        fs::create_dir_all(src.path().join("sub")).unwrap();
        fs::write(src.path().join(".env"), "SECRET=1").unwrap();
        fs::write(src.path().join("sub/tls.key"), "k").unwrap();
        fs::write(src.path().join("sub/keep.txt"), "ok").unwrap();
        let out = dst.path().join("copy");
        let r = CopyPolicy::fork_default().copy_tree(src.path(), &out).unwrap();
        assert!(!out.join(".env").exists());
        assert!(!out.join("sub/tls.key").exists());
        assert_eq!(fs::read_to_string(out.join("sub/keep.txt")).unwrap(), "ok");
        assert_eq!(r.excluded, 2);
    }

    #[cfg(unix)]
    #[test]
    fn escaping_symlink_dropped_and_inside_symlink_kept() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("secret.txt"), "outside").unwrap();
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("real.txt"), "inside").unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), src.path().join("esc"))
            .unwrap();
        std::os::unix::fs::symlink("real.txt", src.path().join("rel")).unwrap();
        std::os::unix::fs::symlink(src.path().join("real.txt"), src.path().join("abs_in"))
            .unwrap();
        std::os::unix::fs::symlink("missing", src.path().join("dangling")).unwrap();

        let dst = tempfile::tempdir().unwrap();
        let out = dst.path().join("copy");
        CopyPolicy::fork_default().copy_tree(src.path(), &out).unwrap();
        assert!(fs::symlink_metadata(out.join("esc")).is_err());
        assert!(fs::symlink_metadata(out.join("dangling")).is_err());
        assert!(fs::symlink_metadata(out.join("rel")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_link(out.join("abs_in")).unwrap(), PathBuf::from("real.txt"));
        assert_eq!(fs::read_to_string(out.join("abs_in")).unwrap(), "inside");
    }

    #[cfg(unix)]
    #[test]
    fn merge_replaces_destination_symlink_instead_of_writing_through() {
        let outside = tempfile::tempdir().unwrap();
        fs::write(outside.path().join("victim.txt"), "untouched").unwrap();
        let dst = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path().join("victim.txt"), dst.path().join("a.txt"))
            .unwrap();
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), "branch").unwrap();

        CopyPolicy::fork_default().copy_tree(src.path(), dst.path()).unwrap();
        assert_eq!(fs::read_to_string(outside.path().join("victim.txt")).unwrap(), "untouched");
        assert!(!fs::symlink_metadata(dst.path().join("a.txt")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(dst.path().join("a.txt")).unwrap(), "branch");
    }
}
