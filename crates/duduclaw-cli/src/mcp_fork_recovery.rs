//! Durable publication evidence and private source copies, outside fork_ws GC.
use std::{cell::Cell, io::{self, Write}, path::{Path, PathBuf}};
use duduclaw_fork::{ForkResolution, PreparedFork, RetainedBranch};

pub(super) struct Recovery {
    pub sources: Vec<RetainedBranch>,
    directory: PathBuf,
    fork_id: String,
    resolution: ForkResolution,
    partial: Cell<bool>,
}

impl Recovery {
    pub fn prepare(home: &Path, fork_id: &str, prepared: &PreparedFork) -> io::Result<Self> {
        duduclaw_fork::retention::validate_id(fork_id).map_err(io::Error::other)?;
        let root = home.join("fork_recovery");
        for path in [&root, &root.join(fork_id)] { private_directory(path)?; }
        let directory = root.join(fork_id).join(uuid::Uuid::new_v4().to_string());
        private_directory(&directory)?;
        let sources = prepared.archive_sources(&directory).map_err(io::Error::other)?;
        sync_tree(&directory)?;
        let recovery = Self { sources, directory, fork_id: fork_id.into(),
            resolution: prepared.resolution().clone(), partial: Cell::new(false) };
        recovery.write("prepared", None)?;
        Ok(recovery)
    }

    pub fn before_parent_write(&self) -> io::Result<()> {
        self.partial.set(true);
        self.write("publishing", None)
    }

    pub fn failed(&self, error: &str) -> io::Result<()> {
        self.write("publication_failed", Some(error))
    }

    pub fn complete(self) {
        if let Err(error) = std::fs::remove_dir_all(&self.directory) {
            // A committed recovery marker cannot be mistaken for an unfinished
            // operation even if cleanup fails; source copies remain private.
            let _ = self.write("committed", Some(&error.to_string()));
            tracing::warn!("fork recovery cleanup {} failed: {error}", self.directory.display());
        }
    }

    fn write(&self, status: &str, error: Option<&str>) -> io::Result<()> {
        let data = serde_json::json!({
            "schema":"duduclaw.fork.publication.v1", "fork_id":self.fork_id,
            "status":status, "parent_may_be_partial":self.partial.get(), "error":error,
            "aggregate_spent_usd":self.resolution.aggregate_spent_usd,
            "results":self.resolution.results,
            "selected_winner":self.resolution.decision.winner,
            "sources":self.sources.iter().map(|source| serde_json::json!({
                "branch_id":source.branch_id,"workspace":source.workspace,
            })).collect::<Vec<_>>(),
        });
        write_journal(&self.directory, &data)
    }
}

pub(super) fn emergency_journal(home: &Path, fork_id: &str, resolution: &ForkResolution,
    sources: &[RetainedBranch], error: &str) -> io::Result<()> {
    duduclaw_fork::retention::validate_id(fork_id).map_err(io::Error::other)?;
    let root = home.join("fork_recovery");
    private_directory(&root)?;
    let directory = root.join(fork_id);
    private_directory(&directory)?;
    let directory = directory.join(uuid::Uuid::new_v4().to_string());
    private_directory(&directory)?;
    write_journal(&directory, &serde_json::json!({
        "schema":"duduclaw.fork.publication.v1", "fork_id":fork_id,
        "status":"publication_failed", "parent_may_be_partial":false, "error":error,
        "aggregate_spent_usd":resolution.aggregate_spent_usd,"results":resolution.results,
        "sources":sources.iter().map(|source| serde_json::json!({
            "branch_id":source.branch_id,"workspace":source.workspace,
        })).collect::<Vec<_>>(),
    }))
}

fn private_directory(path: &Path) -> io::Result<()> {
    duduclaw_fork::retention::ensure_checked_private_dir(path).map_err(io::Error::other)
}

fn write_journal(directory: &Path, value: &serde_json::Value) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
    temporary.write_all(&serde_json::to_vec_pretty(value).map_err(io::Error::other)?)?;
    temporary.as_file().sync_all()?;
    temporary.persist(directory.join("publication.json")).map_err(|error| error.error)?;
    #[cfg(unix)] std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}

fn sync_tree(directory: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() { sync_tree(&entry.path())?; }
        else if kind.is_file() { std::fs::File::open(entry.path())?.sync_all()?; }
    }
    #[cfg(unix)] std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
