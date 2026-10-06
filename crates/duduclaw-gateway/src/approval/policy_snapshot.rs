//! Normalized authority snapshot behind `policy_revision` (F1b, E-H5a).
//!
//! The revision used to hash the raw bytes of `agent.toml`, `config.toml`,
//! `org.toml` and friends, so rotating a channel token or adding a tick
//! source invalidated every pending approval and activated workflow. It now
//! hashes only the parsed fields that decide what an employee may do,
//! grouped into the categories of [`POLICY_CATEGORIES`]:
//!
//! | category        | source                                                        |
//! |-----------------|---------------------------------------------------------------|
//! | `capabilities`  | effective `agent.toml [capabilities]` (preset resolution included) |
//! | `permissions`   | effective `agent.toml [permissions]`                          |
//! | `agent_authority` | effective `agent.toml [agent]` `reports_to` / `department` / `role` |
//! | `contract`      | `agents/<id>/CONTRACT.toml`                                   |
//! | `preset`        | the employee's `preset_bindings.toml` entry and whether a resolution exists |
//! | `org_chain`     | `org.toml` records of the employee and every ancestor         |
//! | `delegation`, `acp`, `provenance`, `integrations` | those `config.toml` sections |
//! | `killswitch`    | `KILLSWITCH.toml`                                             |
//!
//! A file that exists but cannot be read is an error (fail closed, as
//! before); one that cannot be parsed contributes a digest of its raw bytes
//! marked `unparseable`, so any edit to it reads as a change — never as
//! "unchanged". The effective `agent.toml` is required.
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// Every authority category, in hashing order. Changing this list changes
/// every revision, which suspends every activated workflow once.
pub const POLICY_CATEGORIES: &[&str] = &[
    "capabilities",
    "permissions",
    "agent_authority",
    "contract",
    "preset",
    "org_chain",
    "delegation",
    "acp",
    "provenance",
    "integrations",
    "redaction",
    "killswitch",
];

const CONFIG_SECTIONS: &[&str] = &["delegation", "acp", "provenance", "integrations"];
const AGENT_AUTHORITY_KEYS: &[&str] = &["reports_to", "department", "role"];
const MAX_ORG_DEPTH: usize = 64;

/// One digest per category of [`POLICY_CATEGORIES`].
pub type PolicyDigests = BTreeMap<String, String>;

enum Doc {
    Missing,
    Parsed(toml::Table),
    Unparseable(String),
}

fn digest(value: &Value) -> String {
    super::payload_hash(value)
}

fn raw_digest(bytes: &[u8]) -> String {
    format!("unparseable:{}", hex::encode(Sha256::digest(bytes)))
}

fn read_doc(path: &Path) -> Result<Doc, String> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(
            match std::str::from_utf8(&bytes)
                .ok()
                .and_then(|s| s.parse::<toml::Table>().ok())
            {
                Some(table) => Doc::Parsed(table),
                None => Doc::Unparseable(raw_digest(&bytes)),
            },
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Doc::Missing),
        Err(_) => Err("policy snapshot unreadable".into()),
    }
}

fn section_digest(doc: &Doc, section: &str) -> String {
    match doc {
        Doc::Missing => digest(&Value::Null),
        Doc::Unparseable(d) => d.clone(),
        Doc::Parsed(table) => digest(
            &table
                .get(section)
                .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
        ),
    }
}

fn whole_digest(doc: &Doc) -> String {
    match doc {
        Doc::Missing => digest(&Value::String("absent".into())),
        Doc::Unparseable(d) => d.clone(),
        Doc::Parsed(table) => digest(&serde_json::to_value(table).unwrap_or(Value::Null)),
    }
}

/// The employee's effective `agent.toml`: the preset resolution when one
/// exists (the same redirect `agent_toml::load` applies), else its own file.
fn effective_agent_doc(home: &Path, actor: &str) -> Result<(Doc, bool), String> {
    let resolved = duduclaw_core::preset::agent_resolved_path(home, actor);
    match read_doc(&resolved)? {
        Doc::Missing => {}
        doc => return Ok((doc, true)),
    }
    match read_doc(&home.join("agents").join(actor).join("agent.toml"))? {
        Doc::Missing => Err("policy snapshot unreadable".into()),
        doc => Ok((doc, false)),
    }
}

fn agent_authority_digest(doc: &Doc) -> String {
    match doc {
        Doc::Parsed(table) => {
            let agent = table.get("agent").and_then(toml::Value::as_table);
            let fields: BTreeMap<&str, Value> = AGENT_AUTHORITY_KEYS
                .iter()
                .map(|k| {
                    let v = agent
                        .and_then(|a| a.get(*k))
                        .map(|v| serde_json::to_value(v).unwrap_or(Value::Null))
                        .unwrap_or(Value::Null);
                    (*k, v)
                })
                .collect();
            digest(&serde_json::to_value(fields).unwrap_or(Value::Null))
        }
        other => section_digest(other, "agent"),
    }
}

fn preset_digest(home: &Path, actor: &str, resolved: bool) -> Result<String, String> {
    let entry = match std::fs::read_to_string(duduclaw_core::preset::bindings_path(home)) {
        Ok(raw) => match raw.parse::<toml::Table>() {
            Ok(table) => table
                .get("agents")
                .and_then(toml::Value::as_table)
                .and_then(|a| a.get(actor))
                .map(|e| serde_json::to_value(e).unwrap_or(Value::Null))
                .unwrap_or(Value::Null),
            // An uninterpretable store resolves every employee as unbound at
            // runtime; any later edit to it must still read as drift.
            Err(_) => return Ok(raw_digest(raw.as_bytes())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Value::Null,
        Err(_) => return Err("policy snapshot unreadable".into()),
    };
    Ok(digest(
        &serde_json::json!({ "binding": entry, "resolved": resolved }),
    ))
}

/// `org.toml` records of the employee and its ancestors (by `reports_to`).
fn org_chain_digest(home: &Path, actor: &str) -> Result<String, String> {
    let table = match read_doc(&duduclaw_core::org_store::org_store_path(home))? {
        Doc::Missing => return Ok(digest(&Value::Null)),
        Doc::Unparseable(d) => return Ok(d),
        Doc::Parsed(table) => table,
    };
    let agents = table.get("agents").and_then(toml::Value::as_table);
    let mut chain = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    let mut current = actor.to_string();
    for _ in 0..MAX_ORG_DEPTH {
        if !seen.insert(current.clone()) {
            break;
        }
        let record = agents.and_then(|a| a.get(&current));
        chain.push(serde_json::json!({
            "agent": current,
            "record": record.map(|r| serde_json::to_value(r).unwrap_or(Value::Null)),
        }));
        let parent = record
            .and_then(|r| r.get("reports_to"))
            .and_then(toml::Value::as_str)
            .map(str::trim)
            .unwrap_or("");
        if parent.is_empty() || parent.eq_ignore_ascii_case("none") {
            break;
        }
        current = parent.to_string();
    }
    Ok(digest(&Value::Array(chain)))
}

/// Per-category digests of the authority that decides what `actor` may do.
pub fn policy_digests(home: &Path, actor: &str) -> Result<PolicyDigests, String> {
    if !duduclaw_core::is_valid_agent_id(actor) {
        return Err("invalid actor".into());
    }
    let (agent, resolved) = effective_agent_doc(home, actor)?;
    let config = read_doc(&home.join("config.toml"))?;
    let mut out = PolicyDigests::new();
    out.insert(
        "capabilities".into(),
        section_digest(&agent, "capabilities"),
    );
    out.insert("permissions".into(), section_digest(&agent, "permissions"));
    out.insert("agent_authority".into(), agent_authority_digest(&agent));
    out.insert(
        "contract".into(),
        whole_digest(&read_doc(
            &home.join("agents").join(actor).join("CONTRACT.toml"),
        )?),
    );
    out.insert("preset".into(), preset_digest(home, actor, resolved)?);
    out.insert("org_chain".into(), org_chain_digest(home, actor)?);
    for section in CONFIG_SECTIONS {
        out.insert((*section).into(), section_digest(&config, section));
    }
    // R-L7: loosening redaction changes what reads hand to effects.
    out.insert(
        "redaction".into(),
        digest(&serde_json::json!({
            "config": section_digest(&config, "redaction"),
            "custom_profile": whole_digest(&read_doc(
                &crate::redaction_custom_rules::profiles_dir(home).join("custom.toml")
            )?),
        })),
    );
    out.insert(
        "killswitch".into(),
        whole_digest(&read_doc(&home.join("KILLSWITCH.toml"))?),
    );
    debug_assert!(POLICY_CATEGORIES.iter().all(|c| out.contains_key(*c)));
    Ok(out)
}

/// Whether any category came from a file that exists but did not parse.
pub fn has_unparseable(digests: &PolicyDigests) -> bool {
    digests.values().any(|d| d.starts_with("unparseable:"))
}

/// The revision string: one hash over the categories in fixed order.
pub fn revision_of(digests: &PolicyDigests) -> String {
    let mut h = Sha256::new();
    h.update(b"duduclaw-policy-v2");
    for category in POLICY_CATEGORIES {
        h.update(category.as_bytes());
        h.update([0]);
        h.update(
            digests
                .get(*category)
                .map(String::as_str)
                .unwrap_or("")
                .as_bytes(),
        );
        h.update([0]);
    }
    hex::encode(h.finalize())
}

/// Categories whose digest differs (a category missing on either side
/// counts as changed).
pub fn changed_categories(before: &PolicyDigests, after: &PolicyDigests) -> Vec<String> {
    POLICY_CATEGORIES
        .iter()
        .filter(|c| before.get(**c) != after.get(**c) || before.get(**c).is_none())
        .map(|c| (*c).to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const AGENT: &str = "[agent]\nname='alice'\nreports_to='boss'\ndepartment='ops'\nrole='specialist'\ndisplay_name='A'\n\
        [channels.telegram]\nbot_token='t-1'\n[heartbeat]\ninterval_seconds=60\n\
        [capabilities]\nallowed_tools=['tasks_update']\n[permissions]\ncan_schedule_tasks=true\n";
    const CONFIG: &str = "[channels.telegram]\nbot_token='g-1'\n[logging]\nlevel='info'\n\
        [delegation]\npolicy='department'\n[integrations]\ngithub=false\n";
    const ORG: &str = "schema=1\n[agents.alice]\nreports_to='boss'\ndepartment='ops'\n\
        [agents.boss]\nreports_to='none'\ndepartment='ops'\n[agents.other]\nreports_to='none'\ndepartment='x'\n";

    fn home() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("agents/alice")).unwrap();
        std::fs::write(dir.path().join("agents/alice/agent.toml"), AGENT).unwrap();
        std::fs::write(dir.path().join("config.toml"), CONFIG).unwrap();
        std::fs::write(duduclaw_core::org_store::org_store_path(dir.path()), ORG).unwrap();
        dir
    }

    fn edit(home: &Path, rel: &str, from: &str, to: &str) {
        let path = home.join(rel);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains(from), "{rel} lacks {from}");
        std::fs::write(&path, text.replacen(from, to, 1)).unwrap();
    }

    #[test]
    fn unrelated_fields_leave_the_revision_unchanged() {
        let dir = home();
        let h = dir.path();
        let base = policy_digests(h, "alice").unwrap();
        edit(h, "agents/alice/agent.toml", "t-1", "t-2");
        edit(
            h,
            "agents/alice/agent.toml",
            "interval_seconds=60",
            "interval_seconds=90",
        );
        edit(h, "config.toml", "g-1", "g-rotated");
        edit(h, "config.toml", "level='info'", "level='debug'");
        let mut cfg = std::fs::read_to_string(h.join("config.toml")).unwrap();
        cfg.push_str("[[tick.sources]]\nname='px'\nkind='http_poll'\nurl='https://example.com'\n");
        std::fs::write(h.join("config.toml"), cfg).unwrap();
        edit(
            h,
            duduclaw_core::org_store::org_store_path(h)
                .strip_prefix(h)
                .unwrap()
                .to_str()
                .unwrap(),
            "department='x'",
            "department='y'",
        );
        let after = policy_digests(h, "alice").unwrap();
        assert_eq!(revision_of(&base), revision_of(&after));
        assert!(changed_categories(&base, &after).is_empty());
    }

    #[test]
    fn every_listed_field_changes_the_revision_and_names_its_category() {
        let org_rel = {
            let dir = home();
            duduclaw_core::org_store::org_store_path(dir.path())
                .strip_prefix(dir.path())
                .unwrap()
                .to_string_lossy()
                .into_owned()
        };
        let cases: Vec<(&str, Box<dyn Fn(&Path)>)> = vec![
            (
                "capabilities",
                Box::new(|h| {
                    edit(
                        h,
                        "agents/alice/agent.toml",
                        "['tasks_update']",
                        "['tasks_update','web_fetch_cached']",
                    )
                }),
            ),
            (
                "permissions",
                Box::new(|h| {
                    edit(
                        h,
                        "agents/alice/agent.toml",
                        "can_schedule_tasks=true",
                        "can_schedule_tasks=false",
                    )
                }),
            ),
            (
                "agent_authority",
                Box::new(|h| {
                    edit(
                        h,
                        "agents/alice/agent.toml",
                        "reports_to='boss'",
                        "reports_to='other'",
                    )
                }),
            ),
            (
                "agent_authority",
                Box::new(|h| {
                    edit(
                        h,
                        "agents/alice/agent.toml",
                        "department='ops'",
                        "department='sales'",
                    )
                }),
            ),
            (
                "agent_authority",
                Box::new(|h| {
                    edit(
                        h,
                        "agents/alice/agent.toml",
                        "role='specialist'",
                        "role='main'",
                    )
                }),
            ),
            (
                "contract",
                Box::new(|h| {
                    std::fs::write(
                        h.join("agents/alice/CONTRACT.toml"),
                        "[boundaries]\nmust_not=['x']\n",
                    )
                    .unwrap()
                }),
            ),
            (
                "preset",
                Box::new(|h| {
                    std::fs::write(
                        duduclaw_core::preset::bindings_path(h),
                        "[agents.alice]\npreset='sales'\n",
                    )
                    .unwrap()
                }),
            ),
            (
                "org_chain",
                Box::new({
                    let r = org_rel.clone();
                    move |h| {
                        edit(
                            h,
                            &r,
                            "[agents.boss]\nreports_to='none'\ndepartment='ops'",
                            "[agents.boss]\nreports_to='none'\ndepartment='exec'",
                        )
                    }
                }),
            ),
            (
                "delegation",
                Box::new(|h| edit(h, "config.toml", "policy='department'", "policy='open'")),
            ),
            (
                "acp",
                Box::new(|h| {
                    let mut c = std::fs::read_to_string(h.join("config.toml")).unwrap();
                    c.push_str("[acp]\ntrusted=true\n");
                    std::fs::write(h.join("config.toml"), c).unwrap();
                }),
            ),
            (
                "provenance",
                Box::new(|h| {
                    let mut c = std::fs::read_to_string(h.join("config.toml")).unwrap();
                    c.push_str("[provenance]\nsensitive_tools=['x']\n");
                    std::fs::write(h.join("config.toml"), c).unwrap();
                }),
            ),
            (
                "integrations",
                Box::new(|h| edit(h, "config.toml", "github=false", "github=true")),
            ),
            (
                "redaction",
                Box::new(|h| {
                    let dir = crate::redaction_custom_rules::profiles_dir(h);
                    std::fs::create_dir_all(&dir).unwrap();
                    std::fs::write(dir.join("custom.toml"), "[meta]\nname='custom'\n").unwrap()
                }),
            ),
            (
                "redaction",
                Box::new(|h| {
                    let mut c = std::fs::read_to_string(h.join("config.toml")).unwrap();
                    c.push_str("[redaction]\nenabled=false\n");
                    std::fs::write(h.join("config.toml"), c).unwrap();
                }),
            ),
            (
                "killswitch",
                Box::new(|h| {
                    std::fs::write(h.join("KILLSWITCH.toml"), "[triggers]\nmax_cost=1\n").unwrap()
                }),
            ),
        ];
        for (category, change) in cases {
            let dir = home();
            let base = policy_digests(dir.path(), "alice").unwrap();
            change(dir.path());
            let after = policy_digests(dir.path(), "alice").unwrap();
            assert_ne!(revision_of(&base), revision_of(&after), "{category}");
            assert_eq!(
                changed_categories(&base, &after),
                vec![category.to_string()]
            );
        }
    }

    #[test]
    fn unparseable_files_read_as_changed_and_missing_agent_fails_closed() {
        let dir = home();
        let h = dir.path();
        let base = revision_of(&policy_digests(h, "alice").unwrap());
        std::fs::write(h.join("config.toml"), "[[[ not toml").unwrap();
        let broken = revision_of(&policy_digests(h, "alice").unwrap());
        assert_ne!(base, broken);
        std::fs::write(h.join("config.toml"), "[[[ other garbage").unwrap();
        assert_ne!(broken, revision_of(&policy_digests(h, "alice").unwrap()));
        std::fs::remove_file(h.join("agents/alice/agent.toml")).unwrap();
        assert!(policy_digests(h, "alice").is_err());
        assert!(policy_digests(h, "../alice").is_err());
    }
}
