//! Per-employee skill switches: which composed skills an AI employee does
//! **not** load.
//!
//! The state lives at `<home>/skill_state/<agent_id>.json`, outside the
//! employee's own directory, so the employee's file guard keeps it out of the
//! employee's reach (an employee may write its own `SKILLS/`, and a switch file
//! it could edit would be no switch at all).
//!
//! The switch can only narrow: a name listed here is dropped from the
//! employee's composed skill set (global, department and own layers alike),
//! nothing is ever added.
//!
//! ## Unreadable state
//!
//! A missing file is the normal fresh state (nothing disabled). Any other
//! failure (permission error, malformed JSON, unknown version, invalid agent
//! id) is [`DisabledSkills::Unreadable`], which disables **every** skill for
//! that employee until an operator repairs or deletes the file. Falling back to
//! "load everything" would silently re-enable skills an operator switched off,
//! so it is not used. Writes refuse to overwrite an unreadable file for the
//! same reason: replacing it with a fresh list would drop the switches it held.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::registry::SkillFile;

/// Directory under `<home>` holding one switch file per employee.
pub const SKILL_STATE_DIR: &str = "skill_state";

/// Schema version written into every switch file.
const STATE_VERSION: u32 = 1;

/// Upper bound on the number of names one switch file may hold.
pub const MAX_DISABLED_PER_AGENT: usize = 2_000;

/// Upper bound on a switch file's size; anything larger is treated as corrupt.
const MAX_STATE_BYTES: u64 = 512 * 1024;

#[derive(Debug, Serialize, Deserialize)]
struct StateFile {
    version: u32,
    #[serde(default)]
    disabled: Vec<String>,
}

/// The switch state of one employee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisabledSkills {
    /// The file was read (or does not exist); these names are switched off.
    Names(BTreeSet<String>),
    /// The file could not be read; every skill counts as switched off.
    Unreadable(String),
}

impl DisabledSkills {
    /// Nothing switched off.
    pub fn none() -> Self {
        Self::Names(BTreeSet::new())
    }

    /// Whether `skill_name` must not be loaded for this employee.
    pub fn is_disabled(&self, skill_name: &str) -> bool {
        match self {
            Self::Names(set) => set.contains(skill_name),
            Self::Unreadable(_) => true,
        }
    }

    /// The reason the state could not be read, if it could not.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Names(_) => None,
            Self::Unreadable(e) => Some(e.as_str()),
        }
    }

    /// The switched-off names (empty for an unreadable state, where the
    /// caller should treat everything as off via [`Self::is_disabled`]).
    pub fn names(&self) -> Vec<String> {
        match self {
            Self::Names(set) => set.iter().cloned().collect(),
            Self::Unreadable(_) => Vec::new(),
        }
    }

    /// Drop every switched-off skill from `skills`, keeping order.
    pub fn filter(&self, skills: Vec<SkillFile>) -> Vec<SkillFile> {
        skills.into_iter().filter(|s| !self.is_disabled(&s.name)).collect()
    }
}

/// `<home>/skill_state/<agent_id>.json`, or `None` for an invalid agent id.
pub fn state_path(home: &Path, agent_id: &str) -> Option<PathBuf> {
    if !duduclaw_core::is_valid_agent_id(agent_id) {
        return None;
    }
    Some(home.join(SKILL_STATE_DIR).join(format!("{agent_id}.json")))
}

/// Read one employee's switch state. Never panics; see the module docs for
/// what an unreadable file means.
pub fn read_disabled(home: &Path, agent_id: &str) -> DisabledSkills {
    let Some(path) = state_path(home, agent_id) else {
        return DisabledSkills::Unreadable(format!("invalid agent id '{agent_id}'"));
    };
    match read_state_file(&path) {
        Ok(state) => state,
        Err(e) => {
            warn!(agent = agent_id, path = %path.display(), error = %e,
                "skill switch file unreadable; every skill stays off for this employee");
            DisabledSkills::Unreadable(e)
        }
    }
}

fn read_state_file(path: &Path) -> Result<DisabledSkills, String> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(DisabledSkills::none()),
        Err(e) => return Err(format!("cannot read skill switch file: {e}")),
    };
    if !meta.is_file() {
        return Err("skill switch file is not a regular file".into());
    }
    if meta.len() > MAX_STATE_BYTES {
        return Err(format!("skill switch file is larger than {MAX_STATE_BYTES} bytes"));
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read skill switch file: {e}"))?;
    parse_state(&text)
}

fn parse_state(text: &str) -> Result<DisabledSkills, String> {
    let file: StateFile =
        serde_json::from_str(text).map_err(|e| format!("skill switch file is not valid JSON: {e}"))?;
    if file.version != STATE_VERSION {
        return Err(format!("unsupported skill switch file version {}", file.version));
    }
    if file.disabled.len() > MAX_DISABLED_PER_AGENT {
        return Err("skill switch file lists too many skills".into());
    }
    Ok(DisabledSkills::Names(file.disabled.into_iter().collect()))
}

/// Validate a skill name taken from a request. Names are registry keys (a
/// directory or file stem), so they never contain a path separator.
pub fn validate_skill_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed != name {
        return Err("skill name is empty or has surrounding spaces".into());
    }
    if name.chars().count() > 200 {
        return Err("skill name is too long".into());
    }
    if name.contains(['/', '\\', '\0']) || name == "." || name == ".." || name.starts_with('.') {
        return Err("skill name is not a valid skill name".into());
    }
    Ok(())
}

/// Switch `names` off (`disabled = true`) or back on for one employee and
/// return the resulting state. Holds an advisory lock for the whole
/// read-modify-write and replaces the file atomically. Refuses to touch an
/// unreadable file.
pub fn set_disabled(
    home: &Path,
    agent_id: &str,
    names: &[String],
    disabled: bool,
) -> Result<DisabledSkills, String> {
    let path = state_path(home, agent_id).ok_or_else(|| format!("invalid agent id '{agent_id}'"))?;
    for n in names {
        validate_skill_name(n)?;
    }
    let dir = path.parent().ok_or("skill switch directory unavailable")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create skill switch directory: {e}"))?;
    let result = duduclaw_core::with_file_lock(&path, || {
        let current = match read_state_file(&path) {
            Ok(DisabledSkills::Names(set)) => set,
            Ok(DisabledSkills::Unreadable(e)) | Err(e) => {
                return Ok(Err(format!(
                    "the skill switch file for '{agent_id}' cannot be read ({e}); repair or delete {} first",
                    path.display()
                )));
            }
        };
        let next = apply(current, names, disabled);
        if next.len() > MAX_DISABLED_PER_AGENT {
            return Ok(Err("too many switched-off skills for one employee".into()));
        }
        write_atomic(&path, &next)?;
        Ok(Ok(DisabledSkills::Names(next)))
    });
    match result {
        Ok(inner) => inner,
        Err(e) => Err(format!("write skill switch file: {e}")),
    }
}

fn apply(current: BTreeSet<String>, names: &[String], disabled: bool) -> BTreeSet<String> {
    let mut next = current;
    for n in names {
        if disabled {
            next.insert(n.clone());
        } else {
            next.remove(n);
        }
    }
    next
}

fn write_atomic(path: &Path, names: &BTreeSet<String>) -> io::Result<()> {
    let body = StateFile { version: STATE_VERSION, disabled: names.iter().cloned().collect() };
    let text = serde_json::to_string_pretty(&body).map_err(io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
#[path = "skill_state_tests.rs"]
mod tests;
