//! H12d: the one layout of a protected recording under the employee work root.
//!
//! The recording helper (`duduclaw-execution::recording_io::publish`) writes
//! here and the `skill_from_recording` reader (`mcp-boundary`
//! `ResourceRoots::read_recording_artifact`) opens exactly these relative
//! paths through root's work broker. Before H12d the reader looked for
//! `<home>/recordings/<id>/{meta.json,done.json,session.har}` on the host,
//! which the protected helper never produces. Both sides must use this module.

/// Directory under the employee work root (`/work`) holding recordings.
pub const RECORDINGS_DIR: &str = "recordings";
/// Artifacts of a completed protected recording, in manifest order.
pub const ARTIFACTS: [&str; 3] = ["trace.zip", "actions.json", "network.json"];
/// Largest single artifact the helper publishes (and the broker returns).
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// A protected recording id: the 32 lowercase hex characters of its job handle.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `recordings/<id>/<artifact>` relative to the work root, or `None` for an
/// invalid id or an artifact name outside [`ARTIFACTS`].
pub fn artifact_path(id: &str, artifact: &str) -> Option<String> {
    (valid_id(id) && ARTIFACTS.contains(&artifact)).then(|| format!("{RECORDINGS_DIR}/{id}/{artifact}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_artifact_paths_are_fixed_and_relative() {
        let id = "ab".repeat(16);
        assert_eq!(artifact_path(&id, "actions.json").as_deref(), Some(format!("recordings/{id}/actions.json").as_str()));
        assert_eq!(artifact_path(&id, "network.json").as_deref(), Some(format!("recordings/{id}/network.json").as_str()));
        for bad in ["meta.json", "done.json", "session.har", "../trace.zip", "", "actions.json/"] {
            assert!(artifact_path(&id, bad).is_none(), "{bad}");
        }
        for bad in ["AB".repeat(16), "a".repeat(31), "../".repeat(11), String::new()] {
            assert!(artifact_path(&bad, "actions.json").is_none(), "{bad}");
        }
    }
}
