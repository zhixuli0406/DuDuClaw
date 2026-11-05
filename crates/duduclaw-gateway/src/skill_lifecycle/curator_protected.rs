//! Isolated discovery with a host-approved, content-bound maintenance pass.
use super::*;
use crate::skill_lifecycle::isolated::{self, Snapshot};
use serde_json::json;

async fn discover(home: &Path) -> Result<Vec<(String, String, Snapshot)>, String> {
    let home = home.to_owned();
    tokio::task::spawn_blocking(move || {
        let mut files = isolated::snapshots(&home, &["skills"])?.into_iter()
            .map(|s| (SCOPE_GLOBAL.to_owned(), s)).collect::<Vec<_>>();
        for agent in isolated::agents(&home)? {
            for snapshot in isolated::snapshots(&home, &["agents", &agent, "SKILLS"])? {
                files.push((format!("agent:{agent}"), snapshot));
                if files.len() > 1000 { return Err("skill inventory limit".into()); }
            }
        }
        let mut out: Vec<(String,String,Snapshot)> = vec![];
        for (scope, snapshot) in files {
            let spec = snapshot.filename.eq_ignore_ascii_case("SKILL.md");
            let layout_name = if spec { snapshot.directories.last().ok_or("invalid skill layout")?.clone() }
                else { std::path::Path::new(&snapshot.filename).file_stem().and_then(|s|s.to_str()).ok_or("invalid skill layout")?.to_owned() };
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
    if live_config(home)? != *cfg { return Err("維護設定已變更".into()); }
    if !cfg.enabled { return Err("技能維護已停用".into()); }
    let files = discover(home).await?;
    let mut records = store.curation_list().await?;
    records.sort_by(|a, b| (&a.scope, &a.skill_name).cmp(&(&b.scope, &b.skill_name)));
    let usage = store.registry_last_used_map().await?;
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
    // BTreeMap is essential: randomized HashMap iteration cannot bind retries.
    let usage_state = usage.iter().map(|(k,v)| (k.clone(),v.clone())).collect::<std::collections::BTreeMap<_,_>>();
    let state = json!({"records":records,"usage":usage_state,"files":file_state,"actions":actions,
        "stale_days":cfg.stale_days,"archive_days":cfg.archive_days,"margin":cfg.approaching_margin_days});
    isolated::approve(home, "system", "curate", "skill-curator", &state,
        json!({"actions":actions,"files":file_state,"report":"套用上述維護動作後，發布維護報告；巢狀技能只標記，不移動。"})).await?;
    // A consumed approval is never reused, including after a stale refusal.
    let mut live = store.curation_list().await?;
    live.sort_by(|a,b| (&a.scope,&a.skill_name).cmp(&(&b.scope,&b.skill_name)));
    if serde_json::to_value(&live).map_err(|e| e.to_string())? != serde_json::to_value(&records).map_err(|e| e.to_string())?
        || store.registry_last_used_map().await? != usage || live_config(home)? != *cfg {
        return Err("技能狀態或維護設定已變更，請重新建立提案".into());
    }
    let checked = discover(home).await?;
    let checked_state = checked.iter().map(|(name,scope,s)| json!({"name":name,"scope":scope,
        "directories":s.directories,"filename":s.filename,"sha256":isolated::digest(&s.bytes)})).collect::<Vec<_>>();
    if checked_state != file_state { return Err("技能清單已變更，請重新建立提案".into()); }
    let stamp = now.to_rfc3339();
    let mut report = CuratorReport { ran_at: stamp.clone(), ..Default::default() };
    for (name, scope, _) in &files {
        if store.curation_upsert_seen(name, scope, &stamp).await? { report.newly_tracked.push(fmt_key(name,scope)); }
    }
    for rec in &records {
        let key = fmt_key(&rec.skill_name,&rec.scope);
        let artifact = files.iter().find(|(name,scope,_)| name == &rec.skill_name && scope == &rec.scope);
        // Recheck live usage/pin immediately before each mutation, not just once
        // for the whole pass. A concurrent change refuses remaining actions.
        let live = store.curation_get(&rec.skill_name,&rec.scope).await?.ok_or("curation row disappeared")?;
        if serde_json::to_value(&live).map_err(|e| e.to_string())? != serde_json::to_value(rec).map_err(|e| e.to_string())?
            || store.registry_last_used_map().await? != usage { return Err("技能使用紀錄已變更；部分維護可能已套用".into()); }
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
    let rows = store.curation_list().await?;
    report.tracked_total = rows.len();
    for row in rows {
        if row.pinned { report.pinned_total += 1; }
        match row.status { CurationStatus::Stale => report.stale_total += 1,
            CurationStatus::Archived => report.archived_total += 1, _ => {} }
    }
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
