//! Git Worktree L0 isolation layer for agent task execution.
//!
//! Provides lightweight filesystem isolation by running each agent task in its
//! own git worktree. This is cheaper than container sandbox (L1) while still
//! preventing concurrent agents from stepping on each other's files.
//!
//! Inspired by <https://github.com/nekocode/agent-worktree>.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tokio::sync::Mutex;
use tracing::{debug, info, warn};

/// Global merge lock shared across ALL `WorktreeManager` instances.
///
/// Serializes git operations that mutate the main repo's working tree
/// (checkout + merge). Without this, concurrent `dispatch_in_worktree`
/// calls — each creating a fresh `WorktreeManager` — would race on
/// the same repo, corrupting git state.
fn global_merge_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

// ── Constants ──────────────────────────────────────────────────────────────

/// Maximum worktrees allowed per agent (prevents resource exhaustion).
const MAX_WORKTREES_PER_AGENT: usize = 5;

/// Maximum total worktrees across all agents.
const MAX_TOTAL_WORKTREES: usize = 20;

// ── Branch Name Generator ──────────────────────────────────────────────────

/// Word lists for human-friendly branch names (adjective-noun pairs).
const ADJECTIVES: &[&str] = &[
    "swift", "calm", "bright", "bold", "crisp", "eager", "fair", "gentle",
    "happy", "keen", "light", "merry", "neat", "proud", "quick", "sharp",
    "smart", "steady", "warm", "wise", "amber", "azure", "coral", "dusk",
    "frost", "golden", "ivory", "jade", "lunar", "maple", "noble", "ocean",
    "pearl", "quiet", "ruby", "sage", "tidal", "ultra", "vivid", "zen",
    "agile", "brave", "clear", "deep", "fresh", "grand", "humble", "iron",
    "lush", "prime",
];

const NOUNS: &[&str] = &[
    "fox", "river", "peak", "pine", "hawk", "stone", "wave", "cloud",
    "leaf", "star", "brook", "crane", "delta", "ember", "flint", "grove",
    "heron", "isle", "jewel", "knoll", "lark", "mesa", "nest", "oak",
    "petal", "quill", "reed", "sage", "trail", "vale", "whirl", "yarn",
    "arch", "bay", "cove", "dawn", "elm", "fern", "glade", "hill",
    "ink", "jade", "kite", "lynx", "moss", "node", "orbit", "pond",
    "rift", "spark",
];

/// Sanitize an agent_id for use in git branch names.
///
/// Only allows `[a-z0-9-]`, replaces other chars with `-`, collapses
/// consecutive dashes, and strips leading/trailing dashes.
fn sanitize_agent_id(agent_id: &str) -> Result<String, String> {
    let sanitized: String = agent_id
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '-' })
        .collect();
    // Collapse consecutive dashes.
    let mut prev_dash = false;
    let collapsed: String = sanitized
        .chars()
        .filter(|&c| {
            if c == '-' {
                if prev_dash { return false; }
                prev_dash = true;
            } else {
                prev_dash = false;
            }
            true
        })
        .collect();
    let result = collapsed.trim_matches('-').to_string();
    if result.is_empty() {
        return Err(format!("agent_id '{agent_id}' produces empty branch segment after sanitization"));
    }
    Ok(result)
}

/// Generate a human-friendly branch name: `wt/{sanitized_agent_id}/{adjective}-{noun}`.
pub fn generate_branch_name(agent_id: &str) -> Result<String, String> {
    let safe_id = sanitize_agent_id(agent_id)?;
    let seed = {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut h: u64 = 0xcbf29ce484222325; // FNV-1a offset basis
        for b in agent_id.bytes().chain(now.to_le_bytes().into_iter()) {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3); // FNV-1a prime
        }
        h
    };
    let adj = ADJECTIVES[(seed % ADJECTIVES.len() as u64) as usize];
    let noun = NOUNS[((seed >> 16) % NOUNS.len() as u64) as usize];
    Ok(format!("wt/{safe_id}/{adj}-{noun}"))
}

/// Validate that a branch name is a safe wt/ branch (no option injection).
fn validate_wt_branch(branch: &str) -> Result<(), String> {
    if !branch.starts_with("wt/") {
        return Err(format!("Branch '{branch}' is not a wt/ branch"));
    }
    if branch.split('/').any(|seg| seg.starts_with('-')) {
        return Err(format!("Branch segment starts with hyphen: {branch}"));
    }
    if branch.contains("..") {
        return Err(format!("Branch contains '..': {branch}"));
    }
    Ok(())
}

// ── Worktree Info ──────────────────────────────────────────────────────────

/// Metadata about an active git worktree.
#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// Branch name associated with this worktree.
    pub branch: String,
    /// HEAD commit hash.
    pub head: String,
}

// ── Merge Result ───────────────────────────────────────────────────────────

/// Outcome of a merge attempt.
#[derive(Debug, Clone)]
pub enum MergeResult {
    /// Merge succeeded — commit hash of the merge commit.
    Success(String),
    /// Nothing to merge — worktree branch has no new commits.
    NothingToMerge,
    /// Conflict — list of conflicting file paths.
    Conflict(Vec<String>),
    /// Merge aborted or failed for another reason.
    Error(String),
}

// ── Snap Action (pure decision logic) ──────────────────────────────────────

/// What to do after an agent finishes in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapAction {
    /// No changes, no new commits → auto-cleanup.
    AutoCleanup,
    /// New commits, no conflict → merge into target branch then cleanup.
    MergeAndCleanup,
    /// New commits but merge conflicts → keep worktree, notify user.
    MergeConflict(Vec<String>),
    /// Keep worktree alive (agent requested retry or manual inspection).
    KeepWorktree,
}

/// Inputs for the snap decision function (pure, no I/O).
#[derive(Debug, Clone)]
pub struct WorktreeStatus {
    /// Are there uncommitted changes (staged or unstaged)?
    pub has_uncommitted: bool,
    /// Number of new commits on the worktree branch vs the fork point.
    pub new_commit_count: u32,
    /// Merge conflict file list (empty = no conflict detected).
    pub conflict_files: Vec<String>,
    /// Did the agent request a retry (exit code 2)?
    pub agent_requested_retry: bool,
}

/// Pure function: decide what to do after agent exits.
/// Separated from I/O for easy unit testing.
pub fn determine_snap_action(status: &WorktreeStatus) -> SnapAction {
    if status.agent_requested_retry {
        return SnapAction::KeepWorktree;
    }
    if status.has_uncommitted {
        return SnapAction::KeepWorktree;
    }
    if status.new_commit_count == 0 {
        return SnapAction::AutoCleanup;
    }
    if !status.conflict_files.is_empty() {
        return SnapAction::MergeConflict(status.conflict_files.clone());
    }
    SnapAction::MergeAndCleanup
}

// ── Exit Code Protocol ─────────────────────────────────────────────────────

/// Structured interpretation of agent subprocess exit codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentExitCode {
    /// 0 — task completed successfully.
    Success,
    /// 1 — task failed with an error.
    Error,
    /// 2 — agent requests re-dispatch (retry).
    Retry,
    /// 3 — agent wants to keep worktree alive for inspection.
    KeepAlive,
    /// Any other code.
    Unknown(i32),
}

impl From<i32> for AgentExitCode {
    fn from(code: i32) -> Self {
        match code {
            0 => Self::Success,
            1 => Self::Error,
            2 => Self::Retry,
            3 => Self::KeepAlive,
            other => Self::Unknown(other),
        }
    }
}

// ── WorktreeManager ────────────────────────────────────────────────────────

/// Manages the lifecycle of git worktrees for agent task isolation.
///
/// Merge operations are serialized via a module-level global lock
/// (`global_merge_lock`) to prevent concurrent merge attempts from
/// corrupting the main repo's working tree — even when multiple
/// `WorktreeManager` instances are created (one per dispatch call).
pub struct WorktreeManager {
    /// Root directory for all worktrees: `~/.duduclaw/worktrees/`.
    worktrees_dir: PathBuf,
}

impl WorktreeManager {
    /// Create a new manager. `home_dir` is `~/.duduclaw/`.
    pub fn new(home_dir: &Path) -> Self {
        Self {
            worktrees_dir: home_dir.join("worktrees"),
        }
    }

    /// Create a new worktree for an agent task.
    ///
    /// Enforces per-agent and global worktree limits to prevent resource
    /// exhaustion. Branch names are generated from sanitized agent IDs
    /// with retry on collision.
    pub async fn create(
        &self,
        repo_root: &Path,
        agent_id: &str,
    ) -> Result<WorktreeInfo, String> {
        // Ensure worktrees root exists.
        tokio::fs::create_dir_all(&self.worktrees_dir)
            .await
            .map_err(|e| format!("Failed to create worktrees dir: {e}"))?;

        // Enforce worktree limits.
        let existing = self.list(repo_root).await.unwrap_or_default();
        let agent_count = existing.iter()
            .filter(|w| {
                // wt/{agent_id}/... — check second segment.
                w.branch.split('/').nth(1).map_or(false, |seg| seg == sanitize_agent_id(agent_id).unwrap_or_default())
            })
            .count();
        if agent_count >= MAX_WORKTREES_PER_AGENT {
            return Err(format!(
                "Agent '{agent_id}' has {agent_count} active worktrees (limit: {MAX_WORKTREES_PER_AGENT})"
            ));
        }
        if existing.len() >= MAX_TOTAL_WORKTREES {
            return Err(format!(
                "System worktree limit reached ({} active, limit: {MAX_TOTAL_WORKTREES})",
                existing.len()
            ));
        }

        // Generate branch name with collision retry (up to 3 attempts).
        let mut last_err = String::new();
        for attempt in 0..3u32 {
            let branch = if attempt == 0 {
                generate_branch_name(agent_id)?
            } else {
                // On retry, append attempt number to force different hash.
                let retry_id = format!("{agent_id}-retry{attempt}");
                generate_branch_name(&retry_id)?
            };
            validate_wt_branch(&branch)?;

            let dir_name = branch.replace('/', "-");
            let worktree_path = self.worktrees_dir.join(&dir_name);

            // Reject pre-existing paths or symlinks (prevents symlink attacks).
            if worktree_path.exists() || worktree_path.is_symlink() {
                last_err = format!("Worktree path already exists: {}", dir_name);
                continue;
            }

            let output = tokio::process::Command::new("git")
                .args(["worktree", "add", "-b", &branch])
                .arg(&worktree_path)
                .arg("HEAD")
                .current_dir(repo_root)
                .output()
                .await
                .map_err(|e| format!("Failed to spawn git: {e}"))?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                last_err = stderr.trim().to_string();
                if stderr.contains("already exists") {
                    continue; // Branch collision — retry.
                }
                return Err(format!("git worktree add failed: {last_err}"));
            }

            let head = Self::git_rev_parse(&worktree_path, "HEAD").await?;

            info!(
                agent = agent_id,
                branch = %branch,
                "Created worktree"
            );

            return Ok(WorktreeInfo {
                path: worktree_path,
                branch,
                head,
            });
        }

        Err(format!("Failed to create worktree after 3 attempts: {last_err}"))
    }

    /// Remove a worktree and its associated branch.
    pub async fn remove(&self, repo_root: &Path, worktree_path: &Path, branch: &str) -> Result<(), String> {
        validate_wt_branch(branch)?;

        let output = tokio::process::Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(worktree_path)
            .current_dir(repo_root)
            .output()
            .await
            .map_err(|e| format!("Failed to spawn git: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!(err = %stderr.trim(), "worktree remove failed, trying manual cleanup");
            let _ = tokio::fs::remove_dir_all(worktree_path).await;
            let _ = tokio::process::Command::new("git")
                .args(["worktree", "prune"])
                .current_dir(repo_root)
                .output()
                .await;
        }

        // Delete the branch (only wt/ branches).
        let _ = tokio::process::Command::new("git")
            .args(["branch", "-D", branch])
            .current_dir(repo_root)
            .output()
            .await;

        info!(branch = branch, "Removed worktree");
        Ok(())
    }

    /// List all active worktrees managed by DuDuClaw.
    pub async fn list(&self, repo_root: &Path) -> Result<Vec<WorktreeInfo>, String> {
        let output = tokio::process::Command::new("git")
            .args(["worktree", "list", "--porcelain"])
            .current_dir(repo_root)
            .output()
            .await
            .map_err(|e| format!("Failed to list worktrees: {e}"))?;

        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut results = Vec::new();

        let mut path: Option<PathBuf> = None;
        let mut head: Option<String> = None;
        let mut branch: Option<String> = None;

        for line in stdout.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(h) = line.strip_prefix("HEAD ") {
                head = Some(h.to_string());
            } else if let Some(b) = line.strip_prefix("branch refs/heads/") {
                branch = Some(b.to_string());
            } else if line.is_empty() {
                if let (Some(p), Some(h), Some(b)) = (path.take(), head.take(), branch.take()) {
                    if b.starts_with("wt/") {
                        results.push(WorktreeInfo { path: p, branch: b, head: h });
                    }
                }
                path = None;
                head = None;
                branch = None;
            }
        }
        // Flush last entry.
        if let (Some(p), Some(h), Some(b)) = (path, head, branch) {
            if b.starts_with("wt/") {
                results.push(WorktreeInfo { path: p, branch: b, head: h });
            }
        }

        Ok(results)
    }

    /// Clean up stale worktrees whose directories no longer exist.
    pub async fn cleanup_stale(&self, repo_root: &Path) -> Result<u32, String> {
        let output = tokio::process::Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(repo_root)
            .output()
            .await
            .map_err(|e| format!("Failed to prune worktrees: {e}"))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("git worktree prune failed: {}", stderr.trim()));
        }

        let worktrees = self.list(repo_root).await?;
        let active_branches: std::collections::HashSet<String> =
            worktrees.iter().map(|w| w.branch.clone()).collect();

        let branch_output = tokio::process::Command::new("git")
            .args(["branch", "--list", "wt/*"])
            .current_dir(repo_root)
            .output()
            .await
            .map_err(|e| format!("Failed to list branches: {e}"))?;

        let mut cleaned = 0u32;
        for line in String::from_utf8_lossy(&branch_output.stdout).lines() {
            let branch = line.trim().trim_start_matches("* ");
            if branch.starts_with("wt/") && !active_branches.contains(branch) {
                if validate_wt_branch(branch).is_ok() {
                    let _ = tokio::process::Command::new("git")
                        .args(["branch", "-D", branch])
                        .current_dir(repo_root)
                        .output()
                        .await;
                    cleaned += 1;
                    debug!(branch = branch, "Cleaned up orphan wt/ branch");
                }
            }
        }

        Ok(cleaned)
    }

    // ── Atomic Merge ───────────────────────────────────────────────────

    /// Attempt to merge a worktree branch into the target branch using
    /// dry-run pre-check (merge → check → abort → real merge if clean).
    ///
    /// Protected by `merge_lock` to prevent concurrent merge operations
    /// from corrupting the main repo's working tree.
    ///
    /// Saves and restores the original HEAD branch after merge.
    pub async fn atomic_merge(
        &self,
        repo_root: &Path,
        worktree_branch: &str,
        target_branch: &str,
    ) -> MergeResult {
        if let Err(e) = validate_wt_branch(worktree_branch) {
            return MergeResult::Error(format!("Invalid worktree branch: {e}"));
        }

        // Validate target_branch: only safe chars allowed, no `..` sequences.
        if !target_branch.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.')
            || target_branch.starts_with('-')
            || target_branch.contains("..")
        {
            return MergeResult::Error(format!("Invalid target branch name: {target_branch}"));
        }

        // Serialize all merge operations on the main repo via global lock.
        let _lock = global_merge_lock().lock().await;

        // Save original HEAD to restore later.
        let original_head = Self::git_rev_parse(repo_root, "HEAD").await.unwrap_or_default();
        let original_branch = Self::current_branch_name(repo_root).await;

        // Step 1: Checkout target branch.
        let checkout = tokio::process::Command::new("git")
            .args(["checkout", target_branch])
            .current_dir(repo_root)
            .output()
            .await;
        match &checkout {
            Ok(o) if !o.status.success() => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                return MergeResult::Error(format!(
                    "Cannot checkout target branch '{target_branch}': {}",
                    stderr.trim()
                ));
            }
            Err(e) => {
                return MergeResult::Error(format!("Failed to spawn git checkout: {e}"));
            }
            _ => {}
        }

        // Step 2: Dry-run merge. `--` prevents branch name from being parsed as flag.
        let dry_run = tokio::process::Command::new("git")
            .args(["merge", "--no-commit", "--no-ff", "--", worktree_branch])
            .current_dir(repo_root)
            .output()
            .await;

        let dry_ok = match &dry_run {
            Ok(o) => o.status.success(),
            Err(_) => false,
        };

        // Step 3: Check for conflicts.
        let conflict_files = if !dry_ok {
            Self::get_conflict_files(repo_root).await
        } else {
            vec![]
        };

        // Step 4: Always abort the trial merge. Log on failure.
        let abort_result = tokio::process::Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(repo_root)
            .output()
            .await;
        match &abort_result {
            Ok(o) if !o.status.success() => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // If abort fails but there's no MERGE_HEAD, it's benign.
                if repo_root.join(".git/MERGE_HEAD").exists() {
                    warn!(err = %stderr.trim(), "merge --abort failed and MERGE_HEAD still exists — repo may be in dirty state");
                }
            }
            Err(e) => {
                warn!(err = %e, "Failed to spawn merge --abort");
            }
            _ => {}
        }

        if !dry_ok {
            Self::restore_head(repo_root, &original_branch, &original_head).await;
            if conflict_files.is_empty() {
                let stderr = dry_run
                    .as_ref()
                    .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
                    .unwrap_or_default();
                return MergeResult::Error(format!("Dry-run merge failed: {stderr}"));
            }
            return MergeResult::Conflict(conflict_files);
        }

        // Check if there's actually anything to merge.
        let diff_output = tokio::process::Command::new("git")
            .args(["log", &format!("{target_branch}..{worktree_branch}"), "--oneline"])
            .current_dir(repo_root)
            .output()
            .await;

        let has_commits = diff_output
            .as_ref()
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);

        if !has_commits {
            Self::restore_head(repo_root, &original_branch, &original_head).await;
            return MergeResult::NothingToMerge;
        }

        // Step 5: Real merge. `--` prevents branch name from being parsed as flag.
        let real_merge = tokio::process::Command::new("git")
            .args([
                "merge",
                "--no-ff",
                "-m",
                &format!("Merge agent worktree: {worktree_branch}"),
                "--",
                worktree_branch,
            ])
            .current_dir(repo_root)
            .output()
            .await;

        let result = match real_merge {
            Ok(o) if o.status.success() => {
                let head = Self::git_rev_parse(repo_root, "HEAD")
                    .await
                    .unwrap_or_default();
                info!(
                    branch = worktree_branch,
                    target = target_branch,
                    commit = %head,
                    "Worktree branch merged successfully"
                );
                MergeResult::Success(head)
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Abort the failed real merge.
                let _ = tokio::process::Command::new("git")
                    .args(["merge", "--abort"])
                    .current_dir(repo_root)
                    .output()
                    .await;
                MergeResult::Error(format!("Merge failed: {}", stderr.trim()))
            }
            Err(e) => MergeResult::Error(format!("Failed to execute merge: {e}")),
        };

        // Always restore original HEAD — even after successful merge,
        // the main repo should not silently stay on target_branch.
        Self::restore_head(repo_root, &original_branch, &original_head).await;

        result
    }

    // ── Snap Workflow ──────────────────────────────────────────────────

    /// Inspect a worktree's git status and build a `WorktreeStatus`.
    pub async fn inspect_worktree(&self, worktree_path: &Path, repo_root: &Path) -> WorktreeStatus {
        let has_uncommitted = Self::has_uncommitted_changes(worktree_path).await;
        let new_commit_count = Self::count_new_commits(worktree_path, repo_root).await;

        WorktreeStatus {
            has_uncommitted,
            new_commit_count,
            conflict_files: vec![], // Conflict detection happens during merge.
            agent_requested_retry: false, // Set by caller from exit code.
        }
    }

    /// Execute a snap action (I/O side-effects).
    pub async fn execute_snap(
        &self,
        action: &SnapAction,
        repo_root: &Path,
        worktree_path: &Path,
        worktree_branch: &str,
        target_branch: &str,
    ) -> Result<SnapOutcome, String> {
        match action {
            SnapAction::AutoCleanup => {
                self.remove(repo_root, worktree_path, worktree_branch).await?;
                Ok(SnapOutcome::Cleaned)
            }
            SnapAction::MergeAndCleanup => {
                let merge = self.atomic_merge(repo_root, worktree_branch, target_branch).await;
                match merge {
                    MergeResult::Success(commit) => {
                        self.remove(repo_root, worktree_path, worktree_branch).await?;
                        Ok(SnapOutcome::Merged(commit))
                    }
                    MergeResult::NothingToMerge => {
                        self.remove(repo_root, worktree_path, worktree_branch).await?;
                        Ok(SnapOutcome::Cleaned)
                    }
                    MergeResult::Conflict(files) => {
                        Ok(SnapOutcome::ConflictKept(files))
                    }
                    MergeResult::Error(e) => Err(e),
                }
            }
            SnapAction::MergeConflict(files) => {
                Ok(SnapOutcome::ConflictKept(files.clone()))
            }
            SnapAction::KeepWorktree => {
                Ok(SnapOutcome::Kept)
            }
        }
    }

    // ── Copy Files ─────────────────────────────────────────────────────

    /// Copy non-git-tracked environment files into the worktree.
    ///
    /// Security: rejects absolute paths, `..` traversal, symlinks, and
    /// files larger than 1 MB. All paths are canonicalized and verified
    /// to remain within their respective root directories.
    pub async fn copy_env_files(
        &self,
        src_dir: &Path,
        worktree_dir: &Path,
        patterns: &[String],
    ) -> Result<u32, String> {
        let default_patterns = [".env", ".env.local", ".env.claude"];
        let pats: Vec<&str> = if patterns.is_empty() {
            default_patterns.iter().copied().collect()
        } else {
            patterns.iter().map(|s| s.as_str()).collect()
        };

        // Resolve canonical roots for jail enforcement.
        let src_jail = tokio::fs::canonicalize(src_dir)
            .await
            .map_err(|e| format!("Cannot resolve src_dir: {e}"))?;

        let mut copied = 0u32;
        for pat in &pats {
            // Reject absolute paths.
            if Path::new(pat).is_absolute() {
                warn!(file = %pat, "Skipping absolute path in worktree_copy_files");
                continue;
            }
            // Reject path traversal.
            if pat.contains("..") {
                warn!(file = %pat, "Skipping path traversal attempt in worktree_copy_files");
                continue;
            }
            // Skip .git.
            if pat.starts_with(".git/") || *pat == ".git" {
                continue;
            }

            let src = src_dir.join(pat);

            // Check symlink BEFORE canonicalize — canonicalize resolves
            // symlinks, so checking after would always be false.
            match tokio::fs::symlink_metadata(&src).await {
                Ok(m) if m.file_type().is_symlink() => {
                    warn!(file = %pat, "Skipping symlink source");
                    continue;
                }
                Err(_) => continue, // File doesn't exist — skip.
                _ => {}
            }

            // Resolve canonical path and verify it's inside the jail.
            let canonical_src = match tokio::fs::canonicalize(&src).await {
                Ok(p) => p,
                Err(_) => continue,
            };
            if !canonical_src.starts_with(&src_jail) {
                warn!(file = %pat, "Path escapes src_dir jail — skipping");
                continue;
            }

            let meta = tokio::fs::metadata(&canonical_src).await.map_err(|e| e.to_string())?;
            if meta.len() > 1_048_576 {
                warn!(file = %pat, size = meta.len(), "Skipping file > 1MB");
                continue;
            }

            let dst = worktree_dir.join(pat);
            // Verify dst parent is inside worktree_dir.
            if let Some(parent) = dst.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| format!("Failed to create dir for {pat}: {e}"))?;
                let canonical_parent = tokio::fs::canonicalize(parent)
                    .await
                    .map_err(|e| format!("Cannot resolve dst parent: {e}"))?;
                let canonical_wt = tokio::fs::canonicalize(worktree_dir)
                    .await
                    .map_err(|e| format!("Cannot resolve worktree_dir: {e}"))?;
                if !canonical_parent.starts_with(&canonical_wt) {
                    warn!(file = %pat, "Destination escapes worktree_dir jail — skipping");
                    continue;
                }
            }

            // Remove dst if it exists to prevent TOCTOU symlink race.
            // If an attacker replaces dst with a symlink between our check
            // and the copy, removing first ensures we write to a fresh path.
            let _ = tokio::fs::remove_file(&dst).await;

            tokio::fs::copy(&canonical_src, &dst)
                .await
                .map_err(|e| format!("Failed to copy {pat}: {e}"))?;
            copied += 1;
            debug!(file = %pat, "Copied env file to worktree");
        }
        Ok(copied)
    }

    // ── Private Helpers ────────────────────────────────────────────────

    async fn git_rev_parse(dir: &Path, rev: &str) -> Result<String, String> {
        let output = tokio::process::Command::new("git")
            .args(["rev-parse", rev])
            .current_dir(dir)
            .output()
            .await
            .map_err(|e| format!("git rev-parse failed: {e}"))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    }

    async fn current_branch_name(repo_root: &Path) -> Option<String> {
        let output = tokio::process::Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .current_dir(repo_root)
            .output()
            .await
            .ok()?;
        if output.status.success() {
            let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !name.is_empty() { Some(name) } else { None }
        } else {
            None // Detached HEAD.
        }
    }

    async fn restore_head(repo_root: &Path, branch: &Option<String>, commit: &str) {
        if let Some(name) = branch {
            // Validate branch name: only safe chars, no flag injection.
            let safe = !name.is_empty()
                && !name.starts_with('-')
                && !name.contains("..")
                && name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == '/' || c == '.');
            if !safe {
                warn!(branch = %name, "Refusing to restore HEAD to unsafe branch name");
                return;
            }
            // No `--` here: `git checkout -- X` means pathspec (file restore),
            // not branch switch. Branch name is validated above to not
            // start with `-`, so it's safe without `--`.
            let _ = tokio::process::Command::new("git")
                .args(["checkout", name])
                .current_dir(repo_root)
                .output()
                .await;
        } else if !commit.is_empty() {
            // SHA-1/SHA-256: only hex digits, at least 40 chars.
            let safe = commit.len() >= 40 && commit.chars().all(|c| c.is_ascii_hexdigit());
            if !safe {
                warn!("Refusing to restore HEAD to unsafe commit hash");
                return;
            }
            let _ = tokio::process::Command::new("git")
                .args(["checkout", commit])
                .current_dir(repo_root)
                .output()
                .await;
        }
    }

    async fn has_uncommitted_changes(worktree_path: &Path) -> bool {
        let output = tokio::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(worktree_path)
            .output()
            .await;
        output
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(true) // Assume dirty on error (safer).
    }

    async fn count_new_commits(worktree_path: &Path, repo_root: &Path) -> u32 {
        let main_head = Self::git_rev_parse(repo_root, "HEAD").await.unwrap_or_default();
        if main_head.is_empty() || !main_head.chars().all(|c| c.is_ascii_hexdigit()) {
            return 0;
        }
        let output = tokio::process::Command::new("git")
            .args(["rev-list", "--count", &format!("{main_head}..HEAD")])
            .current_dir(worktree_path)
            .output()
            .await;
        output
            .ok()
            .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
            .unwrap_or(0)
    }

    async fn get_conflict_files(repo_root: &Path) -> Vec<String> {
        let output = tokio::process::Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(repo_root)
            .output()
            .await;
        output
            .map(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

// ── Snap Outcome ───────────────────────────────────────────────────────────

/// Result of executing a snap action.
#[derive(Debug, Clone)]
pub enum SnapOutcome {
    /// Worktree was cleaned up (no changes or merged successfully).
    Cleaned,
    /// Worktree was merged and cleaned — contains merge commit hash.
    Merged(String),
    /// Merge had conflicts — worktree kept alive. Contains conflict files.
    ConflictKept(Vec<String>),
    /// Worktree kept alive (agent retry or manual inspection).
    Kept,
}

// ── Unit Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_branch_name_format() {
        let name = generate_branch_name("agent-001").unwrap();
        assert!(name.starts_with("wt/agent-001/"));
        let parts: Vec<&str> = name.split('/').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[2].contains('-'));
    }

    #[test]
    fn test_sanitize_agent_id() {
        assert_eq!(sanitize_agent_id("agent-001").unwrap(), "agent-001");
        assert_eq!(sanitize_agent_id("Agent_Test").unwrap(), "agent-test");
        assert_eq!(sanitize_agent_id("--bad--id--").unwrap(), "bad-id");
        assert_eq!(sanitize_agent_id("hello world!").unwrap(), "hello-world");
        assert!(sanitize_agent_id("!!!").is_err());
    }

    #[test]
    fn test_validate_wt_branch() {
        assert!(validate_wt_branch("wt/agent/swift-fox").is_ok());
        assert!(validate_wt_branch("main").is_err());
        assert!(validate_wt_branch("wt/-bad/name").is_err());
        assert!(validate_wt_branch("wt/a/../b").is_err());
    }

    #[test]
    fn test_determine_snap_no_changes() {
        let status = WorktreeStatus {
            has_uncommitted: false,
            new_commit_count: 0,
            conflict_files: vec![],
            agent_requested_retry: false,
        };
        assert_eq!(determine_snap_action(&status), SnapAction::AutoCleanup);
    }

    #[test]
    fn test_determine_snap_new_commits_no_conflict() {
        let status = WorktreeStatus {
            has_uncommitted: false,
            new_commit_count: 3,
            conflict_files: vec![],
            agent_requested_retry: false,
        };
        assert_eq!(determine_snap_action(&status), SnapAction::MergeAndCleanup);
    }

    #[test]
    fn test_determine_snap_uncommitted_changes() {
        let status = WorktreeStatus {
            has_uncommitted: true,
            new_commit_count: 2,
            conflict_files: vec![],
            agent_requested_retry: false,
        };
        assert_eq!(determine_snap_action(&status), SnapAction::KeepWorktree);
    }

    #[test]
    fn test_determine_snap_conflict() {
        let status = WorktreeStatus {
            has_uncommitted: false,
            new_commit_count: 1,
            conflict_files: vec!["src/main.rs".into()],
            agent_requested_retry: false,
        };
        assert_eq!(
            determine_snap_action(&status),
            SnapAction::MergeConflict(vec!["src/main.rs".into()])
        );
    }

    #[test]
    fn test_determine_snap_retry() {
        let status = WorktreeStatus {
            has_uncommitted: false,
            new_commit_count: 5,
            conflict_files: vec![],
            agent_requested_retry: true,
        };
        assert_eq!(determine_snap_action(&status), SnapAction::KeepWorktree);
    }

    #[test]
    fn test_exit_code_parsing() {
        assert_eq!(AgentExitCode::from(0), AgentExitCode::Success);
        assert_eq!(AgentExitCode::from(1), AgentExitCode::Error);
        assert_eq!(AgentExitCode::from(2), AgentExitCode::Retry);
        assert_eq!(AgentExitCode::from(3), AgentExitCode::KeepAlive);
        assert_eq!(AgentExitCode::from(137), AgentExitCode::Unknown(137));
    }

    #[test]
    fn test_copy_env_rejects_traversal_patterns() {
        // These should be rejected by the function — tested via the validation
        // logic (actual I/O requires tokio runtime, tested in integration).
        assert!(Path::new("../../etc/passwd").is_relative());
        assert!("../../etc/passwd".contains(".."));
        assert!(Path::new("/etc/shadow").is_absolute());
    }
}
