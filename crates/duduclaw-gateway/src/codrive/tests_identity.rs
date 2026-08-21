//! Regression tests for `identity::resolve_run_identity` (CD-2 task brief
//! item 2) — see that module's doc comment for the semantics pinned here.
//!
//! New codrive test scenarios belong in their own `tests_<topic>.rs` file
//! rather than growing `tests.rs`/`driver.rs` further — both are already
//! near this project's per-file size convention (200-400 lines typical, 800
//! max). See `tests.rs`'s and `driver.rs`'s own header notes.

use std::path::{Path, PathBuf};

use super::identity::{resolve_run_identity, RunIdentityError};

fn tempdir(label: &str) -> PathBuf {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let p = std::env::temp_dir().join(format!("cdt-identity-{label}-{}", &suffix[..8]));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_agent_toml(home: &Path, agent_id: &str, codrive: bool) {
    let dir = home.join("agents").join(agent_id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("agent.toml"),
        format!("[capabilities]\ncodrive = {codrive}\n"),
    )
    .unwrap();
}

#[test]
fn no_override_uses_caller_identity_and_its_own_capability() {
    let home = tempdir("no-override");
    write_agent_toml(&home, "caller-agent", true);

    let resolved = resolve_run_identity(&home, "caller-agent", None).expect("must authorize");
    assert_eq!(resolved, "caller-agent");
}

#[test]
fn no_override_is_refused_when_caller_itself_has_codrive_disabled() {
    let home = tempdir("no-override-off");
    write_agent_toml(&home, "caller-agent", false);

    let err = resolve_run_identity(&home, "caller-agent", None)
        .expect_err("a caller with codrive disabled must be refused");
    assert_eq!(err, RunIdentityError::CapabilityMissing("caller-agent".to_string()));
}

/// The load-bearing regression test: an `agent` override must be gated on
/// the NAMED agent's own capability, not the caller's — even when the
/// caller itself is fully authorized. This is the "capability re-check"
/// half of the semantics `resolve_run_identity`'s doc comment pins.
#[test]
fn override_to_a_codrive_disabled_agent_is_refused_even_when_caller_is_enabled() {
    let home = tempdir("override-cap-off");
    write_agent_toml(&home, "caller-agent", true); // caller itself is fine...
    write_agent_toml(&home, "target-agent", false); // ...but the named override is not

    let err = resolve_run_identity(&home, "caller-agent", Some("target-agent"))
        .expect_err("an override naming a codrive-disabled agent must be refused");
    assert_eq!(err, RunIdentityError::CapabilityMissing("target-agent".to_string()));
}

/// The mirror case: an override to an agent that DOES have codrive enabled
/// must be authorized under THAT agent's capability, even when the caller
/// itself does not have it — proving the override fully replaces identity
/// for the check, in both directions, rather than being a widening-only
/// escape hatch.
#[test]
fn override_to_a_codrive_enabled_agent_is_authorized_even_when_caller_is_disabled() {
    let home = tempdir("override-cap-on");
    write_agent_toml(&home, "caller-agent", false);
    write_agent_toml(&home, "target-agent", true);

    let resolved = resolve_run_identity(&home, "caller-agent", Some("target-agent"))
        .expect("override target's own capability must govern, not the caller's");
    assert_eq!(resolved, "target-agent");
}

#[test]
fn missing_agent_toml_fails_closed_as_capability_missing() {
    let home = tempdir("missing-toml");
    // No agents/ghost-agent/agent.toml at all — duduclaw_core::agent_toml
    // ::load's own Err(_) => AgentTomlSections::default() fallback means
    // this must resolve to codrive = false, not panic or silently pass.
    let err = resolve_run_identity(&home, "ghost-agent", None).expect_err("must fail closed");
    assert_eq!(err, RunIdentityError::CapabilityMissing("ghost-agent".to_string()));
}

#[test]
fn blank_or_whitespace_override_falls_back_to_caller_identity() {
    let home = tempdir("blank-override");
    write_agent_toml(&home, "caller-agent", true);

    let resolved = resolve_run_identity(&home, "caller-agent", Some("   "))
        .expect("a blank override must fall back to the caller, not be treated as an id");
    assert_eq!(resolved, "caller-agent");
}

#[test]
fn override_is_trimmed_before_use() {
    let home = tempdir("trim-override");
    write_agent_toml(&home, "target-agent", true);

    let resolved = resolve_run_identity(&home, "caller-agent", Some("  target-agent  "))
        .expect("surrounding whitespace must not change which agent is resolved");
    assert_eq!(resolved, "target-agent");
}

#[test]
fn invalid_agent_id_format_is_rejected_before_any_capability_read() {
    let home = tempdir("invalid-id");
    let err = resolve_run_identity(&home, "caller-agent", Some("../../etc/passwd"))
        .expect_err("a malformed agent id must be rejected");
    assert_eq!(err, RunIdentityError::InvalidAgentId);
}
