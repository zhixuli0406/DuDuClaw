//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::{offboard_freeze_table, unarchive_restore_table};

fn table_from(toml_src: &str) -> toml::Table {
    toml::from_str(toml_src).unwrap()
}

fn enabled(t: &toml::Table, section: &str) -> bool {
    t.get(section)
        .and_then(|v| v.as_table())
        .and_then(|s| s.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap()
}

/// F6: an agent that runs with evolution/heartbeat intentionally OFF must
/// come back OFF after archive → unarchive, not force-enabled.
#[test]
fn round_trip_preserves_disabled_flags() {
    let mut t = table_from(
        "[agent]\nname = \"x\"\nstatus = \"active\"\n\
             [evolution]\nenabled = false\n[heartbeat]\nenabled = false\n",
    );
    offboard_freeze_table(&mut t, "archived").unwrap();
    assert!(!enabled(&t, "evolution"), "archive freezes evolution");
    assert!(!enabled(&t, "heartbeat"), "archive freezes heartbeat");

    unarchive_restore_table(&mut t).unwrap();
    assert!(
        !enabled(&t, "evolution"),
        "unarchive must NOT force evolution on"
    );
    assert!(
        !enabled(&t, "heartbeat"),
        "unarchive must NOT force heartbeat on"
    );
    assert_eq!(
        t.get("agent").unwrap().get("status").unwrap().as_str(),
        Some("active")
    );
    assert!(t.get("archive").is_none(), "snapshot block consumed");
}

/// F6: an agent that ran with evolution/heartbeat ON is restored to ON.
#[test]
fn round_trip_restores_enabled_flags() {
    let mut t = table_from(
        "[agent]\nname = \"x\"\nstatus = \"active\"\n\
             [evolution]\nenabled = true\n[heartbeat]\nenabled = true\n",
    );
    offboard_freeze_table(&mut t, "archived").unwrap();
    assert!(
        !enabled(&t, "evolution"),
        "archive still freezes while off-boarded"
    );

    unarchive_restore_table(&mut t).unwrap();
    assert!(
        enabled(&t, "evolution"),
        "unarchive restores the original ON"
    );
    assert!(
        enabled(&t, "heartbeat"),
        "unarchive restores the original ON"
    );
}

/// F6: a double-archive must not lose the true original value.
#[test]
fn double_archive_preserves_original() {
    let mut t = table_from(
        "[agent]\nname = \"x\"\nstatus = \"active\"\n\
             [evolution]\nenabled = true\n[heartbeat]\nenabled = false\n",
    );
    offboard_freeze_table(&mut t, "archived").unwrap();
    // Re-archive the already-frozen agent (enabled is now false).
    offboard_freeze_table(&mut t, "deleted").unwrap();
    unarchive_restore_table(&mut t).unwrap();
    assert!(
        enabled(&t, "evolution"),
        "original ON survives double-archive"
    );
    assert!(
        !enabled(&t, "heartbeat"),
        "original OFF survives double-archive"
    );
}

/// F6: no snapshot present (legacy archived agent) ⇒ conservative OFF.
#[test]
fn missing_snapshot_defaults_off() {
    let mut t = table_from(
        "[agent]\nname = \"x\"\nstatus = \"archived\"\n\
             [evolution]\nenabled = false\n[heartbeat]\nenabled = false\n",
    );
    unarchive_restore_table(&mut t).unwrap();
    assert!(!enabled(&t, "evolution"));
    assert!(!enabled(&t, "heartbeat"));
}
