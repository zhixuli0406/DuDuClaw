//! Root wiki policy: one bounded, no-follow, strictly typed loader.
use std::io::Read;
use std::path::Path;

pub const MAX_POLICY_BYTES: usize = 64 * 1024;
pub const MODES: &[&str] = &["agent_writable", "read_only", "operator_only", "agent_allowlist"];

#[derive(Debug, thiserror::Error)]
#[error("wiki scope policy refused: {0}")]
pub struct ScopePolicyError(pub String);

#[derive(Debug, Clone, Default)]
pub struct ScopePolicy {
    pub table: toml::Table,
    /// Exact bytes for liveness checks; None means the file was absent.
    pub source: Option<String>,
}

impl ScopePolicy {
    pub fn parse(raw: &str) -> Result<Self, ScopePolicyError> {
        if raw.len() > MAX_POLICY_BYTES { return Err(ScopePolicyError("exceeds 64 KB".into())); }
        let table: toml::Table = raw.parse().map_err(|e| ScopePolicyError(format!("{e}")))?;
        if let Some(value) = table.get("namespaces") {
            let namespaces = value.as_table().ok_or_else(|| ScopePolicyError("namespaces must be a table".into()))?;
            for (name, value) in namespaces {
                let entry = value.as_table().ok_or_else(|| ScopePolicyError(format!("{name}: expected table")))?;
                for field in ["mode", "synced_from", "knowledge_owner", "sensitivity"] {
                    if entry.get(field).is_some_and(|v| !v.is_str()) {
                        return Err(ScopePolicyError(format!("{name}.{field}: expected string")));
                    }
                }
                if let Some(owner) = entry.get("knowledge_owner").and_then(|v| v.as_str()) {
                    if !matches!(owner, "memory" | "wiki") {
                        return Err(ScopePolicyError(format!("{name}: unknown knowledge_owner")));
                    }
                }
                for field in ["agents", "visible_to_departments"] {
                    if let Some(value) = entry.get(field) {
                        if !value.as_array().is_some_and(|a| a.iter().all(|v| v.is_str())) {
                            return Err(ScopePolicyError(format!("{name}.{field}: expected string array")));
                        }
                    }
                }
                let mode = entry.get("mode").and_then(|v| v.as_str()).unwrap_or("agent_writable");
                if !MODES.contains(&mode) { return Err(ScopePolicyError(format!("{name}: unknown mode"))); }
                if mode == "read_only" && entry.get("synced_from").and_then(|v| v.as_str()).unwrap_or("").trim().is_empty() {
                    return Err(ScopePolicyError(format!("{name}: read_only requires synced_from")));
                }
                if let Some(sensitivity) = entry.get("sensitivity").and_then(|v| v.as_str()) {
                    if crate::Sensitivity::parse(sensitivity).is_none() {
                        return Err(ScopePolicyError(format!("{name}: unknown sensitivity")));
                    }
                }
            }
        }
        Ok(Self { table, source: Some(raw.to_string()) })
    }

    /// A differently spelled or ambiguous policy declaration never grants access.
    pub fn namespace(&self, name: &str) -> Result<Option<&toml::Table>, ScopePolicyError> {
        let Some(namespaces) = self.table.get("namespaces").and_then(|v| v.as_table()) else { return Ok(None); };
        let mut matches = namespaces.iter().filter(|(key, _)| crate::wiki_auto_namespace::segment_opens_as(key, name));
        let first = matches.next();
        if matches.next().is_some() || first.is_some_and(|(key, _)| key != name) {
            return Err(ScopePolicyError("ambiguous namespace spelling".into()));
        }
        Ok(first.and_then(|(_, value)| value.as_table()))
    }
}

/// Missing is no policy. A dangling link is an error, never missing.
pub fn load(path: &Path) -> Result<ScopePolicy, ScopePolicyError> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ScopePolicy::default()),
        Err(e) => return Err(ScopePolicyError(e.to_string())),
        Ok(_) => {}
    }
    let file = crate::fs_safety::open_regular_file_nofollow(path).map_err(|e| ScopePolicyError(e.to_string()))?;
    if file.metadata().map_err(|e| ScopePolicyError(e.to_string()))?.len() > MAX_POLICY_BYTES as u64 {
        return Err(ScopePolicyError("exceeds 64 KB".into()));
    }
    let mut raw = String::new();
    file.take(MAX_POLICY_BYTES as u64 + 1).read_to_string(&mut raw).map_err(|e| ScopePolicyError(e.to_string()))?;
    ScopePolicy::parse(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_knowledge_owner_denies_the_whole_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".scope.toml");
        for owner in ["memory", "wiki", "unknown", "Wiki", ""] {
            std::fs::write(&path, format!("[namespaces.notes]\nknowledge_owner = '{owner}'")).unwrap();
            assert_eq!(load(&path).is_ok(), matches!(owner, "memory" | "wiki"));
        }
    }

    #[test]
    fn rm209_size_types_and_namespace_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".scope.toml");
        std::fs::write(&path, " ".repeat(MAX_POLICY_BYTES)).unwrap();
        assert!(load(&path).is_ok());
        std::fs::write(&path, " ".repeat(MAX_POLICY_BYTES + 1)).unwrap();
        assert!(load(&path).is_err());
        for raw in ["namespaces = []", "[namespaces]\nnotes = 1", "[namespaces.notes]\nagents = [false]",
            "[namespaces.notes]\nknowledge_owner = []", "[namespaces.notes]\nsensitivity = 7",
            "[namespaces.notes]\nsynced_from = false"] {
            assert!(ScopePolicy::parse(raw).is_err(), "{raw}");
        }
        let policy = ScopePolicy::parse("[namespaces.auto]\nmode = 'operator_only'\n[namespaces.AUTO]\nmode = 'agent_writable'").unwrap();
        assert!(policy.namespace("auto").is_err());
        let policy = ScopePolicy::parse("[namespaces.AUTO]\nmode = 'operator_only'").unwrap();
        assert!(policy.namespace("auto").is_err());
    }
}
