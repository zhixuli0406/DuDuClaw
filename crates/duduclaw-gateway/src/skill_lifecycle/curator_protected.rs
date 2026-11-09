//! Isolated discovery with a host-approved, content-bound maintenance pass.
//!
//! H12a / D-H4: a protected employee's `skill_curator_status run=true` is an
//! *owned* pass (`owner = Some(agent)`): it discovers, binds, approves and
//! mutates only the caller's own `agent:<id>` scope. The agent id comes from
//! the capability-bound dispatcher, never from tool arguments. The scheduled
//! system pass (`owner = None`) keeps the whole-home inventory.
use super::*;
use crate::skill_lifecycle::isolated::{self, Snapshot};
use serde_json::json;

/// `agent:<id>` for a valid, non-reserved employee id; anything else refuses.
pub(super) fn owned_scope(agent: &str) -> Result<String, String> {
    if !duduclaw_core::is_valid_agent_id(agent) || duduclaw_core::is_reserved_agent_id(agent)
        || agent.starts_with('_') || !is_safe_component(agent) {
        return Err("技能維護的員工身分無效".into());
    }
    Ok(format!("agent:{agent}"))
}

/// Refuse any curation row or artifact outside the owner's own scope. Checked
/// before every mutation of an owned pass (defence in depth over the filters).
pub(super) fn require_owned(scope: &str, owner: Option<&str>) -> Result<(), String> {
    match owner {
        None => Ok(()),
        Some(agent) if scope == owned_scope(agent)? => Ok(()),
        Some(_) => Err("技能維護只能處理自己的技能".into()),
    }
}

fn in_scope(scope: &str, owner: Option<&str>) -> bool { require_owned(scope, owner).is_ok() }

/// Host-selected raw snapshots; no parser runs here. An owned pass never opens
/// `skills/` or another employee's `SKILLS/` directory.
pub(super) fn collect(home: &Path, owner: Option<&str>) -> Result<Vec<(String, Snapshot)>, String> {
    if let Some(agent) = owner {
        let scope = owned_scope(agent)?;
        let files = isolated::snapshots(home, &["agents", agent, "SKILLS"])?;
        if files.len() > 1000 { return Err("skill inventory limit".into()); }
        return Ok(files.into_iter().map(|s| (scope.clone(), s)).collect());
    }
    let mut files = isolated::snapshots(home, &["skills"])?.into_iter()
        .map(|s| (SCOPE_GLOBAL.to_owned(), s)).collect::<Vec<_>>();
    for agent in isolated::agents(home)? {
        for snapshot in isolated::snapshots(home, &["agents", &agent, "SKILLS"])? {
            files.push((format!("agent:{agent}"), snapshot));
            if files.len() > 1000 { return Err("skill inventory limit".into()); }
        }
    }
    Ok(files)
}

async fn discover(home: &Path, owner: Option<&str>) -> Result<Vec<(String, String, Snapshot)>, String> {
    let home = home.to_owned();
    let owner = owner.map(str::to_owned);
    tokio::task::spawn_blocking(move || {
        let files = collect(&home, owner.as_deref())?;
        let mut out: Vec<(String,String,Snapshot)> = vec![];
        for (scope, snapshot) in files {
            let spec = snapshot.filename.eq_ignore_ascii_case("SKILL.md");
            let layout_name = if spec { snapshot.directories.last().ok_or("invalid skill layout")?.clone() }
                else { std::path::Path::new(&snapshot.filename).file_stem().and_then(|s|s.to_str()).ok_or("invalid skill layout")?.to_owned() };
            // Skill bytes are untrusted data: identity is read by the E worker.
            let name = duduclaw_parse_worker::adapters::inspect_skill_identity(&snapshot.bytes, &layout_name)
                .map_err(|_| "isolated skill discovery refused")?;
            if !duduclaw_agent::skill_loader::is_safe_skill_name(&name) { return Err("invalid skill identity".into()); }
            if let Some(index) = out.iter().position(|(n,s,_)| n.eq_ignore_ascii_case(&name) && s == &scope) {
                let previous_spec = out[index].2.filename.eq_ignore_ascii_case("SKILL.md");
                if spec && !previous_spec { out[index] = (name,scope,snapshot); }
                else if !previous_spec { return Err("ambiguous skill inventory".into()); }
                else if spec { return Err("ambiguous nested skill inventory".into()); }
            } else { out.push((name, scope, snapshot)); }
        }
        out.sort_by(|a, b| (&a.1, &a.0).cmp(&(&b.1, &b.0))); Ok(out)
    }).await.map_err(|e| e.to_string())?
}

/// Curation rows of the pass, sorted; an owned pass sees only its own scope.
pub(super) fn owned_records(mut records: Vec<crate::custom_skills::SkillCurationRecord>, owner: Option<&str>)
    -> Vec<crate::custom_skills::SkillCurationRecord> {
    records.retain(|rec| in_scope(&rec.scope, owner));
    records.sort_by(|a, b| (&a.scope, &a.skill_name).cmp(&(&b.scope, &b.skill_name)));
    records
}

/// Registry usage bound into the approval. An owned pass binds only the names
/// of its own rows/files, so another employee's usage neither appears in the
/// card nor invalidates this employee's approval.
pub(super) fn owned_usage(usage: std::collections::HashMap<String, String>, owner: Option<&str>,
    names: &std::collections::BTreeSet<String>) -> std::collections::BTreeMap<String, String> {
    usage.into_iter().filter(|(name, _)| owner.is_none() || names.contains(name)).collect()
}

fn live_config(home: &Path) -> Result<CuratorConfig, String> {
    let bytes = crate::isolation_worker_plans::absolute_snapshot(&home.join("config.toml"), 64 * 1024)
        .map_err(|_| "維護設定無法讀取")?.ok_or("維護設定不存在")?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "維護設定格式錯誤")?;
    let table: toml::Table = text.parse().map_err(|_| "維護設定格式錯誤")?;
    if let Some(value) = table.get("skill_curator") {
        let values = value.as_table().ok_or("維護設定格式錯誤")?;
        if values.get("enabled").is_some_and(|v| !v.is_bool())
            || ["stale_days","archive_days"].iter().any(|k| values.get(*k).is_some_and(|v| !v.is_integer())) {
            return Err("維護設定格式錯誤".into());
        }
    }
    Ok(CuratorConfig::from_config_str(text))
}

pub(super) async fn run(home: &Path, store: &CustomSkillStore, cfg: &CuratorConfig,
    now: DateTime<Utc>) -> Result<CuratorReport, String> {
    run_scoped(home, store, cfg, now, None).await
}

pub(super) async fn run_owned(home: &Path, store: &CustomSkillStore, cfg: &CuratorConfig,
    now: DateTime<Utc>, agent: &str) -> Result<CuratorReport, String> {
    owned_scope(agent)?;
    run_scoped(home, store, cfg, now, Some(agent)).await
}

async fn run_scoped(home: &Path, store: &CustomSkillStore, cfg: &CuratorConfig,
    now: DateTime<Utc>, owner: Option<&str>) -> Result<CuratorReport, String> {
    if live_config(home)? != *cfg { return Err("維護設定已變更".into()); }
    if !cfg.enabled { return Err("技能維護已停用".into()); }
    let files = discover(home, owner).await?;
    if files.iter().any(|(_, scope, _)| !in_scope(scope, owner)) { return Err("技能維護只能處理自己的技能".into()); }
    let records = owned_records(store.curation_list().await?, owner);
    let names = records.iter().map(|r| r.skill_name.clone()).chain(files.iter().map(|(n,_,_)| n.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    // BTreeMap is essential: randomized HashMap iteration cannot bind retries.
    let usage = owned_usage(store.registry_last_used_map().await?, owner, &names);
    let mut actions = vec![];
    for rec in &records {
        let last = [rec.last_used_at.as_deref(), usage.get(&rec.skill_name).map(String::as_str)]
            .into_iter().flatten().max();
        let days = days_since(last.unwrap_or(&rec.first_seen), now);
        let action = decide(rec.status, rec.pinned, days, last.is_some(), cfg);
        let source = files.iter().find(|(name,scope,_)| name == &rec.skill_name && scope == &rec.scope);
        if rec.status != CurationStatus::Archived && source.is_none() {
            let archive_scope = if rec.scope == SCOPE_GLOBAL { SCOPE_GLOBAL.to_owned() }
                else { format!("agent-{}",rec.scope.strip_prefix("agent:").ok_or("invalid scope")?) };
            if isolated::archive_pending(home,&archive_scope,&rec.skill_name)? {
                return Err("先前封存尚未確認，請先由管理員核對封存收據與檔案；維護已暫停".into());
            }
        }
        let actual = if rec.status != CurationStatus::Archived && source.is_none() { "RemoveMissing".to_owned() }
            else if action == CurationAction::Archive && source.is_some_and(|(_,scope,s)| !flat(scope,s)) { "MarkUnmanaged".to_owned() }
            else { format!("{action:?}") };
        actions.push(json!({"name":rec.skill_name,"scope":rec.scope,"action":actual}));
    }
    let file_state = files.iter().map(|(name, scope, s)| json!({"name":name,"scope":scope,
        "directories":s.directories,"filename":s.filename,"sha256":isolated::digest(&s.bytes)})).collect::<Vec<_>>();
    let mut state = json!({"records":records,"usage":usage,"files":file_state,"actions":actions,
        "stale_days":cfg.stale_days,"archive_days":cfg.archive_days,"margin":cfg.approaching_margin_days});
    let (requester, target, report_note) = match owner {
        Some(agent) => {
            state["owner"] = json!(owned_scope(agent)?);
            (agent.to_owned(), format!("skill-curator:{}", owned_scope(agent)?),
                "只套用此員工自己技能的上述維護動作；維護報告只回傳給該員工，不發布到共用 wiki；巢狀技能只標記，不移動。")
        }
        None => ("system".to_owned(), "skill-curator".to_owned(),
            "套用上述維護動作後，發布維護報告；巢狀技能只標記，不移動。"),
    };
    isolated::approve(home, &requester, "curate", &target, &state,
        json!({"actions":actions,"files":file_state,"report":report_note})).await?;
    // A consumed approval is never reused, including after a stale refusal.
    let live = owned_records(store.curation_list().await?, owner);
    if serde_json::to_value(&live).map_err(|e| e.to_string())? != serde_json::to_value(&records).map_err(|e| e.to_string())?
        || owned_usage(store.registry_last_used_map().await?, owner, &names) != usage || live_config(home)? != *cfg {
        return Err("技能狀態或維護設定已變更，請重新建立提案".into());
    }
    let checked = discover(home, owner).await?;
    let checked_state = checked.iter().map(|(name,scope,s)| json!({"name":name,"scope":scope,
        "directories":s.directories,"filename":s.filename,"sha256":isolated::digest(&s.bytes)})).collect::<Vec<_>>();
    if checked_state != file_state { return Err("技能清單已變更，請重新建立提案".into()); }
    let stamp = now.to_rfc3339();
    let mut report = CuratorReport { ran_at: stamp.clone(), ..Default::default() };
    for (name, scope, _) in &files {
        require_owned(scope, owner)?;
        if store.curation_upsert_seen(name, scope, &stamp).await? { report.newly_tracked.push(fmt_key(name,scope)); }
    }
    for rec in &records {
        require_owned(&rec.scope, owner)?;
        let key = fmt_key(&rec.skill_name,&rec.scope);
        let artifact = files.iter().find(|(name,scope,_)| name == &rec.skill_name && scope == &rec.scope);
        // Recheck live usage/pin immediately before each mutation, not just once
        // for the whole pass. A concurrent change refuses remaining actions.
        let live = store.curation_get(&rec.skill_name,&rec.scope).await?.ok_or("curation row disappeared")?;
        if serde_json::to_value(&live).map_err(|e| e.to_string())? != serde_json::to_value(rec).map_err(|e| e.to_string())?
            || owned_usage(store.registry_last_used_map().await?, owner, &names) != usage {
            return Err("技能使用紀錄已變更；部分維護可能已套用".into());
        }
        if rec.status != CurationStatus::Archived && artifact.is_none() {
            store.curation_remove(&rec.skill_name,&rec.scope).await?; report.removed_missing.push(key); continue;
        }
        let last = [rec.last_used_at.as_deref(),usage.get(&rec.skill_name).map(String::as_str)].into_iter().flatten().max();
        match decide(rec.status,rec.pinned,days_since(last.unwrap_or(&rec.first_seen),now),last.is_some(),cfg) {
            CurationAction::Keep => {},
            CurationAction::MarkStale => {
                store.curation_set_status(&rec.skill_name,&rec.scope,CurationStatus::Stale,None,&stamp).await?;
                report.newly_stale.push(key);
            }
            CurationAction::Reactivate => {
                store.curation_set_status(&rec.skill_name,&rec.scope,CurationStatus::Active,None,&stamp).await?;
                report.reactivated.push(key);
            }
            CurationAction::Archive => {
                let (_,_,snapshot) = artifact.ok_or("archive source missing")?;
                if !flat(&rec.scope, snapshot) {
                    store.curation_set_status(&rec.skill_name,&rec.scope,CurationStatus::Unmanaged,None,&stamp).await?;
                    report.newly_unmanaged.push(key); continue;
                }
                let home = home.to_owned(); let snapshot = snapshot.clone();
                let scope = if rec.scope == SCOPE_GLOBAL { SCOPE_GLOBAL.to_owned() }
                    else { format!("agent-{}",rec.scope.strip_prefix("agent:").ok_or("invalid scope")?) };
                let skill = rec.skill_name.clone();
                let dest = tokio::task::spawn_blocking(move || isolated::archive(&home,&snapshot,&scope,&skill)).await.map_err(|e|e.to_string())??;
                store.curation_set_status(&rec.skill_name,&rec.scope,CurationStatus::Archived,Some(&dest.to_string_lossy()),&stamp).await?;
                report.newly_archived.push(key);
            }
        }
    }
    let rows = owned_records(store.curation_list().await?, owner);
    report.tracked_total = rows.len();
    for row in rows {
        if row.pinned { report.pinned_total += 1; }
        match row.status { CurationStatus::Stale => report.stale_total += 1,
            CurationStatus::Archived => report.archived_total += 1, _ => {} }
    }
    // An owned pass publishes nothing outside the caller's own scope; its
    // report is returned to the caller only (D-H4).
    if owner.is_some() { return Ok(report); }
    let body = render_report(&report,cfg);
    let month = now.format("%Y-%m").to_string();
    let filename = format!("curator-{}-{}.md",now.format("%Y-%m-%d"),uuid::Uuid::new_v4());
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || crate::isolation_worker_plans::write_host_file(&home,
        &["shared","wiki","reports","skill-pipeline",&month],&filename,body.as_bytes()))
        .await.map_err(|e| e.to_string())?.map_err(|_| "維護已套用；報告發布尚未確認")?;
    Ok(report)
}

fn flat(scope: &str, snapshot: &Snapshot) -> bool {
    match scope { SCOPE_GLOBAL => snapshot.directories == ["skills"],
        _ => scope.strip_prefix("agent:").is_some_and(|id| snapshot.directories == ["agents",id,"SKILLS"]) }
}

#[cfg(all(test, target_os = "linux"))]
mod h12a_tests {
    use super::*;
    use crate::custom_skills::SkillCurationRecord;

    fn home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for (dir, file) in [("skills", "global-notes.md"), ("agents/alice/SKILLS", "alice-notes.md"),
            ("agents/bob/SKILLS", "bob-notes.md")] {
            std::fs::create_dir_all(home.path().join(dir)).unwrap();
            std::fs::write(home.path().join(dir).join(file), format!("# {file}\n")).unwrap();
        }
        home
    }
    fn record(name: &str, scope: &str) -> SkillCurationRecord {
        SkillCurationRecord { skill_name: name.into(), scope: scope.into(), first_seen: "2026-01-01T00:00:00Z".into(),
            last_used_at: None, pinned: false, status: CurationStatus::Active, archived_path: None,
            updated_at: "2026-01-01T00:00:00Z".into() }
    }

    #[test]
    fn h12a_curator_run_own_skills_only() {
        let home = home();
        let owned = collect(home.path(), Some("alice")).unwrap();
        assert_eq!(owned.len(), 1);
        assert_eq!(owned[0].0, "agent:alice");
        assert_eq!(owned[0].1.directories, ["agents", "alice", "SKILLS"]);
        assert_eq!(owned[0].1.filename, "alice-notes.md");
        // The system pass still sees the whole home; the owned pass is narrower.
        let all = collect(home.path(), None).unwrap();
        assert_eq!(all.iter().map(|(scope, _)| scope.as_str()).collect::<Vec<_>>(),
            ["global", "agent:alice", "agent:bob"]);
        let records = owned_records(vec![record("bob-notes", "agent:bob"), record("global-notes", "global"),
            record("alice-notes", "agent:alice")], Some("alice"));
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].scope, "agent:alice");
        let usage = [("alice-notes".to_owned(), "2026-02-01T00:00:00Z".to_owned()),
            ("bob-notes".to_owned(), "2026-03-01T00:00:00Z".to_owned())].into_iter().collect();
        let names = ["alice-notes".to_owned()].into_iter().collect();
        let bound = owned_usage(usage, Some("alice"), &names);
        assert_eq!(bound.keys().collect::<Vec<_>>(), ["alice-notes"], "another employee's usage is never bound");
        // An owned pass of an employee without skills is empty, not the global tree.
        std::fs::create_dir_all(home.path().join("agents/carol")).unwrap();
        assert!(collect(home.path(), Some("carol")).unwrap().is_empty());
    }

    #[test]
    fn h12a_curator_refuses_other_employee_skill() {
        assert!(require_owned("agent:alice", Some("alice")).is_ok());
        for scope in ["agent:bob", "global", "department:ops", "agent:alice/../bob", "agent:"] {
            assert!(require_owned(scope, Some("alice")).is_err(), "{scope}");
        }
        assert!(require_owned("agent:bob", None).is_ok(), "the scheduled system pass is unchanged");
        let home = home();
        for agent in ["../bob", "bob/SKILLS", "", "_trash", ".", "a\0b"] {
            assert!(owned_scope(agent).is_err(), "{agent:?}");
            assert!(collect(home.path(), Some(agent)).is_err(), "{agent:?}");
        }
        // A symlinked own SKILLS directory pointing at another employee is refused.
        std::fs::create_dir_all(home.path().join("agents/mallory")).unwrap();
        std::os::unix::fs::symlink(home.path().join("agents/bob/SKILLS"), home.path().join("agents/mallory/SKILLS")).unwrap();
        assert!(collect(home.path(), Some("mallory")).is_err());
    }
}
