//! The agent-count cap, applied to imports.
//!
//! `agents.create`, `templates.create` and MCP `create_agent` all refuse a
//! new employee once the edition / license cap is reached; the pack
//! importer used to create any number of employees without asking. It now
//! counts the employees an import would add and refuses **before anything
//! is written** when `current + adding` exceeds the cap.
//!
//! The cap is the stricter of two readings: the one resolved from disk
//! (license file + environment, the same function MCP `create_agent` uses)
//! and, when the dashboard started the import, the gateway's own decision
//! passed as `--agent-cap` (the gateway can also see a config-level
//! edition override the CLI cannot). Either one can only narrow.

use std::path::Path;

use serde_json::json;

use duduclaw_core::error::Result;

use super::import_error::import_err;

/// Employee directories (with an `agent.toml`) currently in the home.
pub fn count_agents(home: &Path) -> usize {
    std::fs::read_dir(home.join("agents"))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().join("agent.toml").is_file())
                .count()
        })
        .unwrap_or(0)
}

/// The smaller of two caps where `0` means unlimited.
pub fn stricter(a: usize, b: usize) -> usize {
    match (a, b) {
        (0, x) | (x, 0) => x,
        (x, y) => x.min(y),
    }
}

/// Pure decision: `Err((max, current))` when adding `adding` employees to
/// `current` would exceed `max` (`0` = unlimited).
pub fn check(max: usize, current: usize, adding: usize) -> std::result::Result<(), (usize, usize)> {
    if adding == 0 || max == 0 || current.saturating_add(adding) <= max {
        Ok(())
    } else {
        Err((max, current))
    }
}

/// Refuse the import when it would exceed the cap. `freed` employees are
/// about to be replaced (a replace dry run still sees their directories).
pub async fn ensure(home: &Path, gateway_cap: Option<usize>, adding: usize, freed: usize) -> Result<()> {
    if adding == 0 {
        return Ok(());
    }
    let disk = duduclaw_gateway::license_runtime::agent_cap_from_disk(home).await;
    let max = stricter(disk.max, gateway_cap.unwrap_or(0));
    let current = count_agents(home).saturating_sub(freed);
    match check(max, current, adding) {
        Ok(()) => Ok(()),
        Err((max, current)) => Err(import_err(
            "import.quota_exceeded",
            json!({ "max": max, "current": current, "adding": adding }),
            format!(
                "目前的授權最多 {max} 位 AI 員工，已有 {current} 位，這次要新增 {adding} 位，超過上限，什麼都沒有寫入。\
                 請先移除不需要的 AI 員工，或洽詢授權調整"
            ),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_counts_every_employee_the_import_adds() {
        assert!(check(0, 50, 10).is_ok(), "0 = unlimited");
        assert!(check(5, 3, 2).is_ok());
        assert_eq!(check(5, 3, 3), Err((5, 3)));
        assert!(check(5, 5, 0).is_ok(), "nothing added");
        assert_eq!(check(1, 1, 1), Err((1, 1)));
    }

    #[test]
    fn stricter_cap_wins_and_zero_never_widens() {
        assert_eq!(stricter(0, 0), 0);
        assert_eq!(stricter(0, 3), 3);
        assert_eq!(stricter(4, 0), 4);
        assert_eq!(stricter(4, 3), 3);
    }

    #[test]
    fn count_ignores_folders_without_agent_toml() {
        let h = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(h.path().join("agents/a")).unwrap();
        std::fs::write(h.path().join("agents/a/agent.toml"), "").unwrap();
        std::fs::create_dir_all(h.path().join("agents/_trash/b_20260101000000")).unwrap();
        std::fs::create_dir_all(h.path().join("agents/c")).unwrap();
        assert_eq!(count_agents(h.path()), 1);
    }
}
