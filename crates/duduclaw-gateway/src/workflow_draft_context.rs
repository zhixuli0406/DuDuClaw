//! Server derivation of installed skill and creator policy, never client grants.
use crate::workflow::CreatorGrantSnapshot;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// Same agent-first/global fallback order as skills.share. Only an installed
/// Markdown skill can be pinned; arbitrary paths or invented IDs are refused.
pub fn installed_skill_revision(
    home: &Path,
    owner: &str,
    skill_id: &str,
) -> Result<(PathBuf, String), String> {
    if !duduclaw_core::is_valid_agent_id(owner) || !duduclaw_core::is_valid_agent_id(skill_id) {
        return Err("invalid installed skill id".into());
    }
    let home = home.canonicalize().map_err(|_| "skill home unavailable")?;
    for path in [
        home.join("agents")
            .join(owner)
            .join("SKILLS")
            .join(format!("{skill_id}.md")),
        home.join("skills").join(format!("{skill_id}.md")),
    ] {
        match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err("installed skill unavailable".into()),
            Ok(m) if !m.is_file() || m.file_type().is_symlink() || m.len() > 262144 => {
                return Err("installed skill path refused".into());
            }
            Ok(_) => (),
        }
        let expected = path.clone();
        let path = path
            .canonicalize()
            .map_err(|_| "installed skill unavailable")?;
        if path != expected || !path.starts_with(&home) {
            return Err("installed skill outside home".into());
        }
        let bytes = std::fs::read(&path).map_err(|_| "installed skill unreadable")?;
        return Ok((path, hex::encode(Sha256::digest(bytes))));
    }
    Err("installed skill not found".into())
}

/// Capture the host's declared tool allowlist. Native permission flags,
/// resource scopes, record ownership and approval still run in the dispatcher;
/// this catalog projection cannot bypass any of them.
pub fn creator_grant(home: &Path, owner: &str) -> Result<CreatorGrantSnapshot, String> {
    if !duduclaw_core::is_valid_agent_id(owner) {
        return Err("invalid creator actor".into());
    }
    let config = creator_policy_config(home, owner)?;
    let allowed: BTreeSet<String> = duduclaw_core::tool_catalog::builtin_tool_catalog()
        .into_iter()
        .filter(|entry| {
            entry.kind == "mcp"
                && duduclaw_core::tool_catalog::tool_list_verdict(
                    entry.name,
                    &config.capabilities.denied_tools,
                    &config.capabilities.allowed_tools,
                ) == duduclaw_core::tool_catalog::ToolListVerdict::Allowed
        })
        .map(|entry| entry.name.to_string())
        .collect();
    Ok(CreatorGrantSnapshot {
        actor: owner.into(),
        allowed_tools: allowed,
        policy_revision: crate::approval::policy_revision(home, owner)?,
    })
}

/// The creator's effective `AgentConfig`, read from the same authority the
/// agent registry publishes.
///
/// Unbound (or bound but unresolvable) agents: the registry loads the raw
/// `agents/<owner>/agent.toml`, so that file is the authority here too.
///
/// Preset-bound agents (`preset_bindings.toml` resolves to `Applied`): the
/// authority is the resolution the registry materialized at
/// `<home>/agent_resolved/<owner>.toml` (`AgentRegistry::load_agent`), which
/// is also what `agent_toml::load` redirects every shadow reader to. That file
/// lives outside the employee's directory, so editing the directory copy
/// cannot widen the snapshot. A missing, symlinked or unreadable resolved file
/// is refused rather than falling back to the directory copy.
fn creator_policy_config(home: &Path, owner: &str) -> Result<duduclaw_core::AgentConfig, String> {
    let raw = std::fs::read_to_string(home.join("agents").join(owner).join("agent.toml"))
        .map_err(|_| "creator policy unavailable")?;
    let table: toml::Table = raw.parse().map_err(|_| "creator policy invalid")?;
    let (_, resolution) = duduclaw_core::preset::resolve_for_agent(home, owner, &table);
    let text = if resolution.is_applied() {
        let resolved = duduclaw_core::preset::agent_resolved_path(home, owner);
        match std::fs::symlink_metadata(&resolved) {
            Ok(m) if m.is_file() && !m.file_type().is_symlink() => (),
            _ => return Err("creator policy unavailable".into()),
        }
        std::fs::read_to_string(&resolved).map_err(|_| "creator policy unavailable")?
    } else {
        raw
    };
    toml::from_str(&text).map_err(|_| "creator policy invalid".into())
}

/// CLI/runner provenance lookup without database creation or schema writes.
pub fn draft_for_workflow_readonly(
    home: &Path,
    workflow_id: &str,
    revision: i64,
) -> Result<Option<crate::workflow_drafts::WorkflowDraft>, String> {
    let c = crate::review_evidence::audience::readonly_db(&home.join("workflow.db"))?;
    crate::workflow::draft_store::lookup_draft_for_workflow(&c, workflow_id, revision)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A complete `AgentConfig` whose own capabilities allow two tools; the
    /// `{caps}` placeholder is replaced per test.
    const AGENT_TOML: &str = "[agent]\nname='alice'\ndisplay_name='alice'\nrole='specialist'\n\
        status='active'\ntrigger=''\nreports_to=''\nicon=''\n[model]\npreferred='m'\nfallback='f'\n\
        account_pool=[]\n[container]\ntimeout_ms=60000\nmax_concurrent=1\nreadonly_project=true\n\
        [heartbeat]\nenabled=false\ninterval_seconds=3600\nmax_concurrent_runs=1\ncron=''\n\
        [budget]\nmonthly_limit_cents=500\nwarn_threshold_percent=80\nhard_stop=false\n\
        [permissions]\ncan_create_agents=true\ncan_send_cross_agent=true\n\
        can_modify_own_skills=true\ncan_modify_own_soul=false\ncan_schedule_tasks=true\n\
        allowed_channels=[]\n[evolution]\nskill_auto_activate=false\nskill_security_scan=true\n\
        gvu_enabled=false\nmax_silence_hours=168.0\nskill_token_budget=500\nmax_active_skills=2\n\
        [capabilities]\nautonomy_level='auto'\n{caps}\n";

    /// The preset denies `tasks_update`; per-agent fields still win where the
    /// agent sets them, but the agent sets no `denied_tools`, so the resolved
    /// configuration denies it while the directory copy alone does not.
    const PRESET: &str = "[preset]\nversion = \"1.0.0\"\nlabel = \"narrow\"\n\
        description = \"test kit\"\n\n[capabilities]\ndenied_tools = [\"tasks_update\"]\n";

    fn agent_toml(caps: &str) -> String {
        AGENT_TOML.replace("{caps}", caps)
    }

    fn tools(snapshot: &CreatorGrantSnapshot) -> Vec<&str> {
        snapshot.allowed_tools.iter().map(String::as_str).collect()
    }

    async fn bound_home() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            agent_toml("allowed_tools=['tasks_update','tasks_list']"),
        )
        .unwrap();
        let preset_dir = duduclaw_core::preset::preset_dir(home.path(), "narrow");
        std::fs::create_dir_all(&preset_dir).unwrap();
        std::fs::write(preset_dir.join(duduclaw_core::preset::PRESET_FILE), PRESET).unwrap();
        duduclaw_core::preset::bind(home.path(), "alice", &dir, "narrow", "tester", "test")
            .unwrap();
        // The registry publishes the resolution exactly as it does at boot.
        duduclaw_agent::registry::AgentRegistry::load_agent(&dir)
            .await
            .unwrap();
        assert!(duduclaw_core::preset::agent_resolved_path(home.path(), "alice").is_file());
        (home, dir)
    }

    #[test]
    fn unbound_creator_snapshot_reads_the_agent_directory_config() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents").join("alice");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("agent.toml"),
            agent_toml("allowed_tools=['tasks_update','tasks_list']"),
        )
        .unwrap();
        let snapshot = creator_grant(home.path(), "alice").unwrap();
        assert_eq!(tools(&snapshot), vec!["tasks_list", "tasks_update"]);
    }

    #[tokio::test]
    async fn preset_bound_creator_snapshot_comes_from_registry_resolution() {
        let (home, _dir) = bound_home().await;
        let snapshot = creator_grant(home.path(), "alice").unwrap();
        // The directory copy alone would allow tasks_update too.
        assert_eq!(tools(&snapshot), vec!["tasks_list"]);
    }

    #[tokio::test]
    async fn widened_directory_copy_does_not_widen_preset_bound_snapshot() {
        let (home, dir) = bound_home().await;
        let before = creator_grant(home.path(), "alice").unwrap();
        // The employee-editable copy is widened: more tools, and its own empty
        // denied list that would override the preset's denial on a reload.
        std::fs::write(
            dir.join("agent.toml"),
            agent_toml("allowed_tools=['tasks_update','tasks_list','tasks_create']\ndenied_tools=[]" ,),
        )
        .unwrap();
        let after = creator_grant(home.path(), "alice").unwrap();
        assert_eq!(tools(&after), vec!["tasks_list"]);
        assert_eq!(after.allowed_tools, before.allowed_tools);
    }

    #[tokio::test]
    async fn preset_bound_creator_without_published_resolution_is_refused() {
        let (home, _dir) = bound_home().await;
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "alice");
        std::fs::remove_file(&resolved).unwrap();
        assert_eq!(
            creator_grant(home.path(), "alice").unwrap_err(),
            "creator policy unavailable"
        );

        #[cfg(unix)]
        {
            let elsewhere = home.path().join("elsewhere.toml");
            std::fs::write(&elsewhere, agent_toml("allowed_tools=['tasks_list']")).unwrap();
            std::os::unix::fs::symlink(&elsewhere, &resolved).unwrap();
            assert_eq!(
                creator_grant(home.path(), "alice").unwrap_err(),
                "creator policy unavailable"
            );
        }
    }

    #[tokio::test]
    async fn unparseable_published_resolution_is_refused() {
        let (home, _dir) = bound_home().await;
        let resolved = duduclaw_core::preset::agent_resolved_path(home.path(), "alice");
        std::fs::write(&resolved, "[capabilities]\nallowed_tools='not-a-list'\n").unwrap();
        assert_eq!(
            creator_grant(home.path(), "alice").unwrap_err(),
            "creator policy invalid"
        );
    }
}
