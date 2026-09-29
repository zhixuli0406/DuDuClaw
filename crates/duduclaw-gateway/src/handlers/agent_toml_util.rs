//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

// ── F6: archive/unarchive evolution+heartbeat snapshot round-trip ────────────
//
// Off-boarding (archive / soft-delete) freezes `evolution.enabled` and
// `heartbeat.enabled` to `false`. Many production agents run with those
// intentionally disabled, so `unarchive` must restore the operator's ORIGINAL
// choice rather than force both on. `offboard_freeze_table` snapshots the prior
// values into `[archive.restore]`; `unarchive_restore_table` restores them and
// drops the snapshot. Both are pure table transforms so the round-trip is
// unit-testable without a full handler.

pub(crate) fn table_enabled(table: &toml::Table, section: &str) -> bool {
    table
        .get(section)
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
}

pub(crate) fn archive_restore_snapshot(table: &toml::Table, key: &str) -> Option<bool> {
    table
        .get("archive")
        .and_then(|v| v.as_table())
        .and_then(|a| a.get("restore"))
        .and_then(|v| v.as_table())
        .and_then(|r| r.get(key))
        .and_then(|v| v.as_bool())
}

/// Freeze evolution/heartbeat and record their pre-freeze values so a later
/// unarchive can restore them. Preserves an already-present snapshot across a
/// double-archive so the *original* value is never lost.
pub(crate) fn offboard_freeze_table(table: &mut toml::Table, status: &str) -> Result<(), String> {
    let snap_evolution = archive_restore_snapshot(table, "evolution_enabled")
        .unwrap_or_else(|| table_enabled(table, "evolution"));
    let snap_heartbeat = archive_restore_snapshot(table, "heartbeat_enabled")
        .unwrap_or_else(|| table_enabled(table, "heartbeat"));

    let agent_section = table
        .get_mut("agent")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
    agent_section.insert("status".into(), toml::Value::String(status.to_string()));

    for section in ["evolution", "heartbeat"] {
        let sub = table
            .entry(section.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(t) = sub.as_table_mut() {
            t.insert("enabled".into(), toml::Value::Boolean(false));
        }
    }

    let archive = table
        .entry("archive".to_string())
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    if let Some(a) = archive.as_table_mut() {
        let restore = a
            .entry("restore".to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(r) = restore.as_table_mut() {
            r.insert(
                "evolution_enabled".into(),
                toml::Value::Boolean(snap_evolution),
            );
            r.insert(
                "heartbeat_enabled".into(),
                toml::Value::Boolean(snap_heartbeat),
            );
        }
    }
    Ok(())
}

/// Restore evolution/heartbeat from the `[archive.restore]` snapshot (absent ⇒
/// conservative `false`), set status active, and drop the snapshot block.
pub(crate) fn unarchive_restore_table(table: &mut toml::Table) -> Result<(), String> {
    let restore_evolution = archive_restore_snapshot(table, "evolution_enabled").unwrap_or(false);
    let restore_heartbeat = archive_restore_snapshot(table, "heartbeat_enabled").unwrap_or(false);

    let agent_section = table
        .get_mut("agent")
        .and_then(|v| v.as_table_mut())
        .ok_or_else(|| "agent.toml missing [agent] section".to_string())?;
    agent_section.insert("status".into(), toml::Value::String("active".into()));

    for (section, restored) in [
        ("evolution", restore_evolution),
        ("heartbeat", restore_heartbeat),
    ] {
        let sub = table
            .entry(section.to_string())
            .or_insert_with(|| toml::Value::Table(toml::Table::new()));
        if let Some(t) = sub.as_table_mut() {
            t.insert("enabled".into(), toml::Value::Boolean(restored));
        }
    }
    // Snapshot consumed — drop it so a later re-archive re-snapshots the
    // current (restored) values.
    table.remove("archive");
    Ok(())
}
