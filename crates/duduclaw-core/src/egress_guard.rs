//! Turn egress policy. Every layer can only narrow the global policy.

use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Mode {
    Off,
    Shadow,
    Ask,
    Block,
}

impl Mode {
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "off" => Self::Off,
            "shadow" => Self::Shadow,
            "ask" => Self::Ask,
            "block" => Self::Block,
            _ => return None,
        })
    }
}

fn read_mode(path: &Path, section: &str, absent: Mode) -> Result<Mode, String> {
    let result = (|| {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(absent),
            Err(e) => return Err(e.to_string()),
        };
        let doc = text.parse::<toml::Table>().map_err(|e| e.to_string())?;
        let Some(table) = doc.get(section) else {
            return Ok(absent);
        };
        let table = table.as_table().ok_or("section is not a table")?;
        let Some(value) = table.get("egress_guard") else {
            return Ok(absent);
        };
        value
            .as_str()
            .and_then(Mode::parse)
            .ok_or_else(|| "egress_guard must be off, shadow, ask or block".to_string())
    })();
    match result {
        Ok(mode) => Ok(mode),
        Err(reason) => {
            tracing::warn!(path = %path.display(), %reason, "egress guard config unreadable; blocking egress");
            Err(format!("egress guard config unreadable ({}): {reason}", path.display()))
        }
    }
}

/// Missing files or keys use shadow globally and inheritance for the employee.
/// Other read errors and malformed values block this call.
pub fn effective_mode(home: &Path, agent_dir: Option<&Path>) -> Mode {
    effective_mode_checked(home, agent_dir).unwrap_or(Mode::Block)
}

/// Preserve parse failures so a sink refuses even on a currently clean turn.
pub fn effective_mode_checked(home: &Path, agent_dir: Option<&Path>) -> Result<Mode, String> {
    let global = read_mode(&home.join("config.toml"), "security", Mode::Shadow)?;
    let employee = agent_dir
        .map(|dir| {
            let resolved = crate::preset::agent_home_dir(dir).and_then(|home| {
                dir.file_name()
                    .and_then(|id| id.to_str())
                    .map(|id| crate::preset::agent_resolved_path(&home, id))
            });
            let path = resolved
                .filter(|path| match std::fs::symlink_metadata(path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
                    _ => true,
                })
                .unwrap_or_else(|| dir.join("agent.toml"));
            read_mode(&path, "capabilities", Mode::Off)
        })
        .transpose()?
        .unwrap_or(Mode::Off);
    Ok(global.max(employee))
}

/// Host-only execution turn, separate from inherited memory provenance.
/// Takes precedence over the provenance turn. Memory lineage does not read it.
pub const ENV_TURN_ALIAS: &str = "DUDUCLAW_EGRESS_TURN_ID";

/// Response MACs use a domain outside the numeric request timestamp space.
pub const RESPONSE_DOMAIN: &str = "egress-guard-response-v1";
pub const RESPONSE_SIGNATURE_HEADER: &str = "x-duduclaw-egress-signature";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_config_defaults_to_shadow() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(effective_mode_checked(tmp.path(), None), Ok(Mode::Shadow));
        let dir = tmp.path().join("agents/bot");
        assert_eq!(effective_mode_checked(tmp.path(), Some(&dir)), Ok(Mode::Shadow));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\negress_guard='ask'").unwrap();
        assert_eq!(effective_mode_checked(tmp.path(), Some(&dir)), Ok(Mode::Ask));
    }

    #[test]
    fn missing_employee_policy_inherits_but_read_errors_block() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = home.join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='off'").unwrap();
        assert_eq!(effective_mode_checked(home, Some(&dir)), Ok(Mode::Off));
        // A directory produces a real read error even when tests run as root.
        std::fs::create_dir(dir.join("agent.toml")).unwrap();
        assert!(effective_mode_checked(home, Some(&dir)).is_err());
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Block);
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(other.path().join("config.toml")).unwrap();
        assert!(effective_mode_checked(other.path(), None).is_err());
        assert_eq!(effective_mode(other.path(), None), Mode::Block);
    }

    #[test]
    fn modes_only_narrow_and_bad_config_blocks() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let dir = home.join("agents/bot");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Shadow);
        std::fs::write(home.join("config.toml"), "").unwrap();
        std::fs::write(dir.join("agent.toml"), "").unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Shadow);
        for (global, override_, expected) in [
            ("ask", "off", Mode::Ask),
            ("off", "block", Mode::Block),
            ("shadow", "ask", Mode::Ask),
            ("off", "off", Mode::Off),
        ] {
            std::fs::write(
                home.join("config.toml"),
                format!("[security]\negress_guard='{global}'\n"),
            )
            .unwrap();
            std::fs::write(
                dir.join("agent.toml"),
                format!("[capabilities]\negress_guard='{override_}'\n"),
            )
            .unwrap();
            assert_eq!(effective_mode(home, Some(&dir)), expected);
        }
        for bad in [
            "[security]\negress_guard='typo'",
            "[security]\negress_guard=true",
            "security=4",
            "[",
        ] {
            std::fs::write(home.join("config.toml"), bad).unwrap();
            assert_eq!(effective_mode(home, Some(&dir)), Mode::Block);
        }
        std::fs::write(home.join("config.toml"), "").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\negress_guard=1").unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Block);
    }

    #[test]
    fn resolved_policy_wins_and_malformed_resolved_blocks() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path();
        let dir = home.join("agents/alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(home.join("config.toml"), "[security]\negress_guard='off'").unwrap();
        std::fs::write(dir.join("agent.toml"), "[capabilities]\negress_guard='off'").unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Off);
        let resolved = crate::preset::agent_resolved_path(home, "alice");
        std::fs::create_dir_all(resolved.parent().unwrap()).unwrap();
        std::fs::write(&resolved, "[capabilities]\negress_guard='block'").unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Block);
        std::fs::write(&resolved, "[").unwrap();
        assert_eq!(effective_mode(home, Some(&dir)), Mode::Block);
    }
}
