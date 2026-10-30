//! The one writer: turns an [`ImportBundle`] into employees, following the
//! `hermes claw migrate` playbook — dry run first, explicit conflict policy
//! (`--rename`), the original kept under `<home>/imported/<slug>/source/`
//! (credential files excluded), secrets never migrated, and every file
//! accounted for in the report. Each report line carries a message code and
//! parameters so the dashboard can render it in the viewer's language.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use duduclaw_core::error::Result;

use super::ir::{AgentDraft, ImportBundle};
use super::names::NameBook;
use super::outcome::{Outcome, Status};
use super::placement::{self, SkillJob};
use super::slots::{SlotCtx, clean_display_name, write_slots};
use crate::expert::install::InstallCtx;
use crate::expert::import_error::import_err;
use crate::expert::{
    InstallRecord, PackKind, Report, cfg_err, now_iso,
};
use duduclaw_gateway::expert_skills::QuarantinedSkill;

/// Claude Code / DuDuClaw tool names an imported allowlist may keep. A
/// source name outside this set (Copilot `search`, Dify tool ids, …) has
/// no meaning here and is dropped (reported).
const KNOWN_TOOLS: &[&str] = &[
    "Read",
    "Write",
    "Edit",
    "MultiEdit",
    "Bash",
    "Grep",
    "Glob",
    "LS",
    "WebFetch",
    "WebSearch",
    "NotebookEdit",
    "NotebookRead",
    "TodoWrite",
    "Task",
    "Agent",
];

/// Tools denied when a source restricted tools but none of its names map:
/// the restriction must not silently turn into "everything allowed".
const RISKY_TOOLS: &[&str] = &[
    "Bash",
    "Write",
    "Edit",
    "MultiEdit",
    "NotebookEdit",
    "WebFetch",
];

/// Kept in every imported allowlist: an employee without DuDuClaw's own
/// tools (wiki, memory, tasks, delegation) cannot do its job, and the
/// source platform had no way to name them.
const DUDUCLAW_TOOLS: &str = "mcp__duduclaw__*";

fn is_known_tool(t: &str) -> bool {
    KNOWN_TOOLS.contains(&t) || (t.starts_with("mcp__") && t.len() > 5)
}

pub async fn install(
    ctx: &InstallCtx,
    mut bundle: ImportBundle,
    root: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    super::credentials::sanitize_bundle(&mut bundle)?;
    report.imported("format", &bundle.format_label);
    report.tag("format.detected", json!({ "format": bundle.format_code }));
    for w in &bundle.warnings {
        report.warning("import", "", w.clone());
        report.tag("import.note", json!({}));
    }

    let refs: BTreeSet<_> = bundle.agents.iter().map(|draft| &draft.ref_id).collect();
    if refs.len() != bundle.agents.len() {
        return Err(cfg_err("來源含重複員工識別，無法建立安裝計畫".into()));
    }
    let order = topo_order(&bundle.agents);
    // The import name first: a re-import is recognised before any employee
    // is reported as clashing with its own earlier version.
    let slug = choose_slug(ctx, &bundle, &order)?;
    let mut book = ctx.name_book();
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let mut planned_ids: BTreeSet<String> = BTreeSet::new();
    for draft in &order {
        let id = resolve_id(ctx, draft, bundle.format_code, &planned_ids, &book, report);
        if let Some(id) = id {
            planned_ids.insert(id.clone());
            book.reserve(&id);
            id_map.insert(draft.ref_id.clone(), id);
        }
    }
    let stage = tempfile::tempdir().map_err(|e| cfg_err(format!("建立暫存目錄失敗：{e}")))?;
    let mut plans = Vec::new();
    for draft in &order {
        let Some(id) = id_map.get(&draft.ref_id) else { continue };
        let parent = draft.reports_to.as_ref().and_then(|r| id_map.get(r))
            .cloned().or_else(|| ctx.attach_under.clone()).unwrap_or_default();
        plans.push(plan_agent(draft, id, &parent, bundle.format_code, &slug, stage.path(), &mut book, report)?);
    }
    if plans.is_empty() {
        return Err(import_err("import.nothing_imported", json!({"agents": report.agent_problems()}),
            "沒有任何 AI 員工可匯入"));
    }
    let archive = plan_archive(root, &bundle)?;
    let archive_root = ctx.home.join("imported").join(&slug);
    if !archive.is_empty() && ctx.replace.as_ref().is_none_or(|target| target.slug != slug)
        && archive_root.symlink_metadata().is_ok() {
        return Err(cfg_err("原始檔備份目的地已存在，請重新預覽".into()));
    }
    let effects = json!({
        "slug": slug, "format": bundle.format_code, "display_name": bundle.display_name,
        "description": bundle.description, "version": bundle.version,
        "source": bundle, "attach_under": ctx.attach_under,
        "replace": ctx.replace.as_ref().map(|target| &target.slug),
        "archive": file_hashes(&format!("imported/{slug}/source"), &archive),
        "agents": plans.iter().map(|plan| json!({"id": plan.draft.id_hint,
            "files": file_hashes(&format!("agents/{}", plan.draft.id_hint), &plan.files),
            "skills": plan.skill_effects,
            "wiki": file_hashes(&format!("agents/{}/wiki", plan.draft.id_hint), &plan.wiki),
            "profile": plan.draft,
        })).collect::<Vec<_>>()
    });
    let preview_bundle = ImportBundle {
        // Source-native settings have already been allowlist-validated; these
        // are the fully merged settings, including host-owned scaffolding.
        format_code: "resolved", agents: plans.iter().map(|plan| plan.draft.clone()).collect(),
        ..Default::default()
    };
    super::preview::require_plan_confirmation(&preview_bundle, super::preview::bind_context(ctx, effects)?, ctx.dry_run, ctx.confirm_profile.as_deref(), report)?;
    // The cap counts every employee this import would add, before anything
    // (even the archived original) is written.
    ctx.ensure_quota(id_map.len()).await?;
    ctx.begin_writes()?;
    report.meta("slug", &slug);
    report.meta("format", bundle.format_code);

    if !ctx.dry_run && !archive.is_empty() {
        if archive_root.symlink_metadata().is_ok() {
            return Err(cfg_err("原始檔備份目的地已變更，請重新預覽".into()));
        }
        for (rel, bytes) in &archive {
            crate::expert::install::write_planned_file(&ctx.home, &format!("imported/{slug}/source/{rel}"), bytes)?;
        }
        report.imported("archive", &format!("原始檔 {} 個已保存", archive.len()));
        report.tag("archive.saved", json!({"count": archive.len()}));
    }
    let lineage_prefix = format!("agent-import:{slug}/");

    let mut record = InstallRecord {
        slug: slug.clone(),
        kind: PackKind::AgentImport,
        display_name: if bundle.display_name.is_empty() {
            slug.clone()
        } else {
            bundle.display_name.clone()
        },
        version: bundle.version.clone(),
        description: bundle.description.clone(),
        agents: Vec::new(),
        global_skills: Vec::new(),
        wiki_files: Vec::new(),
        agent_skills: Vec::new(),
        agent_ids: Default::default(),
        installed_at: now_iso(),
        source_format: bundle.format_code.to_string(),
    };
    let mut quarantined: Vec<QuarantinedSkill> = Vec::new();
    for plan in &plans {
        if ctx.stopping() { return Err(crate::expert::install::interrupted()); }
        install_agent(ctx, plan, &slug, bundle.format_code, &lineage_prefix, report, &mut quarantined).await?;
        record.agents.push(plan.draft.id_hint.clone());
    }
    for u in &bundle.unmapped {
        report.ignored("archive", &u.rel, u.reason.clone());
        report.tag("archive.unmapped", json!({ "file": u.rel }));
    }
    placement::file_quarantine(ctx, &slug, quarantined, report).await;
    Ok(record)
}

struct AgentPlan {
    draft: AgentDraft,
    scaffold: crate::AgentScaffold,
    files: BTreeMap<String, Vec<u8>>,
    skills: Vec<(super::skills::PreparedSkill, BTreeMap<String, Vec<u8>>)>,
    wiki: BTreeMap<String, Vec<u8>>,
    skill_effects: Vec<serde_json::Value>,
}

fn file_hashes(prefix: &str, files: &BTreeMap<String, Vec<u8>>) -> Vec<serde_json::Value> {
    files.iter().map(|(rel, bytes)| json!({"path": format!("{prefix}/{rel}"),
        "sha256": crate::expert::registry::sha256_hex(bytes)})).collect()
}

/// Resolve all names and bytes before presenting anything for confirmation.
fn plan_agent(
    source: &AgentDraft, id: &str, parent: &str, format: &str, slug: &str, stage: &Path,
    book: &mut NameBook, report: &mut Report,
) -> Result<AgentPlan> {
    let mut source = source.clone();
    super::credentials::sanitize_draft(&mut source)?;
    let source = &source;
    if format == "duduclaw" {
        duduclaw_core::agent_profile::validate_settings(&source.settings).map_err(|e| cfg_err(e.to_string()))?;
    }
    super::preview::validate_section_limits(source)?;
    let mut draft = source.clone();
    let wanted = clean_display_name(&draft.display_name);
    let wanted = if wanted.is_empty() { id } else { &wanted };
    let display = [wanted.to_string(), format!("{wanted}（{id}）"), format!("{wanted}-{id}")]
        .into_iter().find(|name| duduclaw_core::agent_names::norm(name) == duduclaw_core::agent_names::norm(id) || !book.is_taken(name))
        .ok_or_else(|| cfg_err("員工名稱衝突，未產生安裝計畫".into()))?;
    book.reserve(&display);
    if display != wanted {
        report.warning("agent", id, format!("「{wanted}」已被使用，改用「{display}」"));
        report.tag("agent.display_changed", json!({"from": wanted, "to": display}));
    }
    let scaffold = crate::AgentScaffold {
        name: id.into(), display_name: display.clone(), role: "worker".into(),
        reports_to: parent.into(), icon: draft.emoji.clone().unwrap_or_else(|| "🐾".into()),
        trigger: format!("@{display}"), provider: duduclaw_core::types::RuntimeType::Claude,
        model_preferred: map_model(draft.model.as_deref(), id, report),
        soul_body: draft.soul.as_ref().map(|doc| doc.text.clone()),
    };
    let mut files = crate::scaffold_agent_files(&scaffold)?;
    let mut settings: toml::Table = std::str::from_utf8(&files["agent.toml"])
        .map_err(|e| cfg_err(e.to_string()))?.parse().map_err(|e| cfg_err(format!("員工設定無效：{e}")))?;
    crate::expert::merge_toml(&mut settings, &draft.settings);
    let (allowed, denied) = plan_tools(&draft, id, report);
    if !allowed.is_empty() || !denied.is_empty() {
        crate::expert::set_capabilities(&mut settings, &allowed, &denied);
    }
    let _: duduclaw_core::types::AgentConfig = toml::Value::Table(settings.clone()).try_into()
        .map_err(|e| cfg_err(format!("員工設定型別不正確：{e}")))?;
    files.insert("agent.toml".into(), toml::to_string_pretty(&settings).map_err(|e| cfg_err(e.to_string()))?.into_bytes());
    for (file, doc) in [("IDENTITY.md", &draft.identity), ("USER.md", &draft.user_profile),
        ("INSTRUCTIONS.md", &draft.instructions), ("CONTRACT.toml", &draft.contract)] {
        if let Some(doc) = doc { files.insert(file.into(), doc.text.as_bytes().to_vec()); }
    }
    draft.id_hint = id.into();
    draft.display_name = display;
    draft.reports_to = Some(parent.into());
    draft.model = settings.get("model").and_then(|v| v.get("preferred")).and_then(|v| v.as_str()).map(str::to_string);
    let tool_list = |key| settings.get("capabilities").and_then(|v| v.get(key)).and_then(|v| v.as_array())
        .map(|values| values.iter().filter_map(|v| v.as_str().map(str::to_string)).collect::<Vec<_>>()).unwrap_or_default();
    draft.tools = Some(tool_list("allowed_tools"));
    draft.disallowed_tools = tool_list("denied_tools");
    draft.settings = settings;
    let mut skills = Vec::new();
    let mut skill_effects = Vec::new();
    let mut taken = BTreeSet::new();
    for (index, skill) in draft.skills.iter().enumerate() {
        // Prepare from an owned masked snapshot, so normalization never stages raw secrets.
        let mut dropped = Vec::new();
        let input_files = snapshot_tree_filtered(&skill.dir, true, &mut dropped)?;
        let folder = skill.dir.file_name().ok_or_else(|| cfg_err("技能目錄名稱無效".into()))?;
        let input_dir = stage.join(id).join("inputs").join(index.to_string()).join(folder);
        for (rel, bytes) in &input_files {
            let path = input_dir.join(rel);
            if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| cfg_err(e.to_string()))?; }
            std::fs::write(&path, bytes).map_err(|e| cfg_err(e.to_string()))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let meta = std::fs::symlink_metadata(skill.dir.join(rel)).map_err(|e| cfg_err(e.to_string()))?;
                if !meta.is_file() { return Err(cfg_err("技能來源已變更".into())); }
                let mode = if meta.permissions().mode() & 0o111 != 0 { 0o700 } else { 0o600 };
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).map_err(|e| cfg_err(e.to_string()))?;
            }
        }
        let mut masked_skill = skill.clone();
        masked_skill.dir = input_dir;
        let mut prepared = super::skills::prepare(&masked_skill, &stage.join(id).join("prepared"), format, &mut taken);
        prepared.dropped.extend(dropped);
        if matches!(prepared.verdict, super::skills::Verdict::Enable)
            && files.keys().any(|file| file.starts_with(&format!("SKILLS/{}/", prepared.name))) {
            prepared.verdict = super::skills::Verdict::Blocked("與內建技能同名，未覆蓋".into());
        }
        let (disposition, prefix) = match &prepared.verdict {
            super::skills::Verdict::Enable => ("enabled", format!("agents/{id}/SKILLS/{}", prepared.name)),
            super::skills::Verdict::Quarantine(_) => ("quarantined", format!("experts/{slug}/skills-disabled/{id}/{}", prepared.name)),
            super::skills::Verdict::Blocked(_) => ("blocked", String::new()),
        };
        let skill_files = if disposition == "blocked" { BTreeMap::new() }
            else { snapshot_tree(&prepared.staged_dir)? };
        skill_effects.push(json!({"name": prepared.name, "disposition": disposition,
            "verdict": format!("{:?}", prepared.verdict), "files": file_hashes(&prefix, &skill_files)}));
        skills.push((prepared, skill_files));
    }
    let wiki = super::slots::plan_wiki_files(&draft, format).map_err(cfg_err)?;
    Ok(AgentPlan { draft, scaffold, files, skills, skill_effects, wiki })
}

fn read_plan_file(path: &Path, expected: u64) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path).map_err(|e| cfg_err(e.to_string()))?;
    if !file.metadata().map_err(|e| cfg_err(e.to_string()))?.is_file() {
        return Err(cfg_err("來源不是一般檔案".into()));
    }
    let mut bytes = Vec::new();
    file.take(expected.saturating_add(1)).read_to_end(&mut bytes).map_err(|e| cfg_err(e.to_string()))?;
    if bytes.len() as u64 != expected { return Err(cfg_err("來源檔案讀取期間變更".into())); }
    Ok(bytes)
}

fn snapshot_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    snapshot_tree_filtered(root, false, &mut Vec::new())
}

fn snapshot_tree_filtered(root: &Path, skip_private: bool, dropped: &mut Vec<String>) -> Result<BTreeMap<String, Vec<u8>>> {
    fn walk(
        root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>, total: &mut u64,
        skip_private: bool, dropped: &mut Vec<String>,
    ) -> Result<()> {
        for entry in std::fs::read_dir(dir).map_err(|e| cfg_err(e.to_string()))? {
            let entry = entry.map_err(|e| cfg_err(e.to_string()))?;
            let name = entry.file_name();
            if name.to_str().map(|name| name.contains('\\')).unwrap_or(true) {
                return Err(cfg_err("安裝計畫檔名無效".into()));
            }
            let path = entry.path();
            let meta = std::fs::symlink_metadata(&path).map_err(|e| cfg_err(e.to_string()))?;
            let name = name.to_str().ok_or_else(|| cfg_err("安裝計畫檔名不是 UTF-8".into()))?;
            if skip_private && (meta.file_type().is_symlink() || name.starts_with('.')
                || super::adapters::is_secret_file(name)
                || (meta.is_dir() && super::adapters::is_secret_dir(name))) {
                let rel = path.strip_prefix(root).map_err(|e| cfg_err(e.to_string()))?.to_str()
                    .ok_or_else(|| cfg_err("安裝計畫路徑不是 UTF-8".into()))?.replace('\\', "/");
                dropped.push(rel);
                continue;
            }
            if meta.is_dir() { walk(root, &path, files, total, skip_private, dropped)?; }
            else if meta.is_file() {
                *total = total.saturating_add(meta.len());
                if *total > crate::expert::safe_zip::MAX_UNPACK_BYTES || files.len() >= 10_000 { return Err(cfg_err("安裝計畫超過大小限制".into())); }
                let rel = path.strip_prefix(root).map_err(|e| cfg_err(e.to_string()))?.to_str()
                    .ok_or_else(|| cfg_err("安裝計畫路徑不是 UTF-8".into()))?.replace('\\', "/");
                files.insert(rel, super::credentials::mask_bytes(read_plan_file(&path, meta.len())?)?);
            } else { return Err(cfg_err("安裝計畫不接受連結或特殊檔案".into())); }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    walk(root, root, &mut files, &mut 0, skip_private, dropped)?;
    Ok(files)
}

fn plan_archive(root: &Path, bundle: &ImportBundle) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut total = 0u64;
    for rel in placement::archive_list(bundle) {
        if rel.contains('\\') { return Err(cfg_err("原始檔路徑無效".into())); }
        let mut path = root.to_path_buf();
        for part in Path::new(&rel).components() {
            if !matches!(part, std::path::Component::Normal(_)) { return Err(cfg_err("原始檔路徑無效".into())); }
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(cfg_err("原始檔不能是連結".into())),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
                Err(e) => return Err(cfg_err(e.to_string())),
                _ => {}
            }
        }
        let meta = match std::fs::symlink_metadata(root.join(&rel)) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(cfg_err(e.to_string())),
        };
        if !meta.is_file() { return Err(cfg_err("原始檔不是一般檔案".into())); }
        total = total.saturating_add(meta.len());
        if total > crate::expert::safe_zip::MAX_UNPACK_BYTES { return Err(cfg_err("原始檔超過備份上限".into())); }
        let bytes = read_plan_file(&root.join(&rel), meta.len())?;
        let bytes = super::credentials::mask_bytes(bytes)?;
        files.insert(rel.clone(), bytes);
    }
    Ok(files)
}

/// Supervisors before reports (refs pointing outside the bundle are roots).
fn topo_order(agents: &[AgentDraft]) -> Vec<AgentDraft> {
    let nodes: Vec<(String, Option<String>)> = agents
        .iter()
        .map(|a| (a.ref_id.clone(), a.reports_to.clone()))
        .collect();
    let by_ref: BTreeMap<&str, &AgentDraft> =
        agents.iter().map(|a| (a.ref_id.as_str(), a)).collect();
    match crate::expert::topo::topo_sort(&nodes) {
        crate::expert::topo::TopoOutcome::Sorted(order) => order
            .iter()
            .filter_map(|r| by_ref.get(r.as_str()).map(|a| (*a).clone()))
            .collect(),
        crate::expert::topo::TopoOutcome::Cycle(_) => agents
            .iter()
            .cloned()
            .map(|mut a| {
                a.reports_to = None;
                a
            })
            .collect(),
    }
}

/// Final employee id: ASCII form of the hint, else `<format>-<ref>`, else a
/// hash. An existing employee directory is a conflict unless `--rename`
/// (`-imported`); an id that another employee already answers to as a
/// display name or trigger (channels call `@<id>` too) gets the
/// `-imported` suffix automatically, with a warning.
fn resolve_id(
    ctx: &InstallCtx,
    draft: &AgentDraft,
    format: &str,
    planned: &BTreeSet<String>,
    book: &NameBook,
    report: &mut Report,
) -> Option<String> {
    let base = base_id(draft, format)?;
    let dir_taken = |id: &str| ctx.agent_taken(id) || planned.contains(id);
    if dir_taken(&base) && !ctx.rename {
        report.conflict("agent", &base, "同名 AI 員工已存在（可另存一份副本，改用新代號匯入）");
        report.tag("agent.conflict", json!({ "id": base }));
        return None;
    }
    let mut tries = Vec::new();
    if !dir_taken(&base) {
        tries.push(base.clone());
    }
    tries.push(format!("{base}-imported"));
    tries.extend((2..=9).map(|n| format!("{base}-imported-{n}")));
    for c in tries {
        let c = crate::sanitize_agent_id(&c);
        if !crate::is_valid_agent_id(&c) || dir_taken(&c) || book.is_taken(&c) {
            continue;
        }
        if c != base {
            report.warning("agent", &base, format!("代號已有人使用，改以 {c} 匯入"));
            report.tag("agent.renamed", json!({ "from": base, "to": c }));
        }
        return Some(c);
    }
    report.conflict("agent", &base, "重新命名後仍與既有 AI 員工衝突");
    report.tag("agent.conflict", json!({ "id": base }));
    None
}

/// The id an employee gets before any clash handling: ASCII form of the
/// hint, else `<format>-<ref>`, else a hash of the display name.
fn base_id(draft: &AgentDraft, format: &str) -> Option<String> {
    let candidates = [
        crate::sanitize_agent_id(&draft.id_hint),
        crate::sanitize_agent_id(&format!("{format}-{}", draft.ref_id)),
        format!(
            "{format}-{}",
            &crate::expert::registry::sha256_hex(draft.display_name.as_bytes())[..8]
        ),
    ];
    candidates
        .into_iter()
        .find(|c| crate::is_valid_agent_id(c) && !duduclaw_core::is_reserved_agent_id(c))
}

/// The import name: the source's own hint, else the first employee's base
/// id (taken before clash handling, so importing the same source twice
/// always lands on the same name and is recognised as a re-import), else
/// `<format>-import`.
fn choose_slug(ctx: &InstallCtx, bundle: &ImportBundle, order: &[AgentDraft]) -> Result<String> {
    let hint = crate::sanitize_agent_id(&bundle.slug_hint);
    let first = order
        .first()
        .and_then(|a| base_id(a, bundle.format_code))
        .unwrap_or_default();
    let slug = [hint, first, format!("{}-import", bundle.format_code)]
        .into_iter()
        .find(|s| crate::is_valid_agent_id(s))
        .ok_or_else(|| cfg_err("無法產生匯入代號".into()))?;
    ctx.claim_slug(&slug)
}

async fn install_agent(
    ctx: &InstallCtx, plan: &AgentPlan, slug: &str, format: &'static str,
    lineage_prefix: &str, report: &mut Report, quarantined: &mut Vec<QuarantinedSkill>,
) -> Result<()> {
    let d = &plan.draft;
    let id = &d.id_hint;
    if !ctx.dry_run {
        ctx.note_agent(id);
        crate::scaffold_planned_agent_dir(&ctx.home, &plan.scaffold, &plan.files).await?;
    }
    if let Some(doc) = &d.soul {
        report.imported("soul", &format!("{} → 人格設定（SOUL.md）", doc.source));
        report.tag("slot.written", json!({"slot": "soul"}));
    }
    for (slot, doc) in [("identity", &d.identity), ("user-profile", &d.user_profile),
        ("instructions", &d.instructions), ("contract", &d.contract)] {
        if let Some(doc) = doc {
            report.imported(slot, &doc.source);
            report.tag("slot.written", json!({"slot": slot}));
        }
    }
    report.imported("agent", &format!("{}（{id}）", d.display_name));
    report.tag("agent.created", json!({"display": d.display_name, "id": id}));
    // These slots are already exact files in the plan. Only the data imports
    // remain; their source text is owned by the plan, never re-read from disk.
    let mut data = d.clone();
    data.identity = None;
    data.user_profile = None;
    data.instructions = None;
    data.contract = None;
    data.tools_notes = None;
    data.heartbeat = None;
    data.knowledge.clear();
    if !ctx.dry_run && !plan.wiki.is_empty() {
        let store = duduclaw_memory::WikiStore::new(ctx.home.join("agents").join(id).join("wiki"));
        store.ensure_scaffold()?;
        for (path, bytes) in &plan.wiki {
            let content = std::str::from_utf8(bytes).map_err(|e| cfg_err(e.to_string()))?;
            store.write_page_with_author(path, content, "import")?;
        }
    }
    let slot_ctx = SlotCtx { home: &ctx.home, agent_id: id, dry_run: ctx.dry_run,
        lineage_prefix, format_code: format, apply_identity_fields: false };
    for outcome in write_slots(&slot_ctx, &data).await {
        report.add_outcome(&outcome);
        if outcome.status == Status::Skipped {
            return Err(cfg_err(format!("安裝計畫資料未能完整寫入：{}", outcome.detail)));
        }
    }
    report_mcp(d, id, report);
    for (field, why) in &d.ignored_fields {
        report.ignored("field", field, why.clone());
        report.tag("field.ignored", json!({ "field": field }));
    }
    for (src, why) in &d.skipped {
        report.skipped("file", src, why.clone());
        report.tag("file.skipped", json!({ "file": src }));
    }
    for u in &d.unmapped {
        report.ignored("archive", &u.rel, u.reason.clone());
        report.tag("archive.unmapped", json!({ "file": u.rel }));
    }
    let job = SkillJob { id, slug };
    placement::install_skills(ctx, &job, &plan.skills, report, quarantined)?;
    Ok(())
}

/// Claude ids are kept; anything else (doubao-*, gpt-*, …) falls back to the
/// default model with a warning — a source never switches the runtime.
fn map_model(raw: Option<&str>, id: &str, report: &mut Report) -> Option<String> {
    let raw = raw?.trim();
    let stripped = raw.strip_prefix("anthropic/").unwrap_or(raw);
    let mapped = match stripped {
        "" | "inherit" => return None,
        "sonnet" => "claude-sonnet-4-6".to_string(),
        "opus" => "claude-opus-4-5".to_string(),
        "haiku" => "claude-haiku-4-5".to_string(),
        other if other.starts_with("claude") => other.to_string(),
        other => {
            report.warning(
                "agent-model",
                id,
                format!("原本使用的模型「{other}」不是 Claude，已改用預設模型；需要時請在儀表板調整"),
            );
            report.tag("model.fallback", json!({ "model": other }));
            return None;
        }
    };
    report.imported("agent-model", &format!("{id} → {mapped}"));
    report.tag("model.set", json!({ "model": mapped }));
    Some(mapped)
}

/// Imported tool lists only narrow, and always keep DuDuClaw's own tools.
fn plan_tools(d: &AgentDraft, id: &str, report: &mut Report) -> (Vec<String>, Vec<String>) {
    let mut allowed: Vec<String> = Vec::new();
    let mut denied: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    if let Some(tools) = &d.tools {
        for t in tools {
            if is_known_tool(t) {
                allowed.push(t.clone())
            } else {
                dropped.push(t.clone())
            }
        }
        if allowed.is_empty() && !tools.is_empty() {
            denied.extend(RISKY_TOOLS.iter().map(|s| s.to_string()));
            report.warning(
                "agent-tools",
                id,
                "原本限制了可用工具，但名稱都對不上 DuDuClaw；為了不放寬權限，已先關閉執行指令、寫檔與上網抓取",
            );
            report.tag("tools.risky_denied", json!({}));
        }
    }
    if !allowed.is_empty() && !allowed.iter().any(|t| t == DUDUCLAW_TOOLS) {
        allowed.push(DUDUCLAW_TOOLS.to_string());
    }
    for t in &d.disallowed_tools {
        if is_known_tool(t) {
            denied.push(t.clone())
        } else {
            dropped.push(t.clone())
        }
    }
    if !dropped.is_empty() {
        report.ignored("agent-tools", id, format!("對不上的工具名稱未套用：{}", dropped.join("、")));
        report.tag("tools.dropped", json!({ "names": dropped.join(", ") }));
    }
    if allowed.is_empty() && denied.is_empty() {
        return (allowed, denied);
    }
    let list = |v: &[String]| if v.is_empty() { "—".to_string() } else { v.join(", ") };
    report.imported(
        "agent-tools",
        &format!("{id}：只允許 {}；禁用 {}", list(&allowed), list(&denied)),
    );
    report.tag("tools.narrowed", json!({ "allowed": list(&allowed), "denied": list(&denied) }));
    (allowed, denied)
}

/// MCP servers declared in a foreign agent file are **not** imported: a
/// server is something DuDuClaw would execute (a `command`) or connect to (a
/// `url`, possibly with host environment values in its headers), and an
/// import must never widen what an employee can run or reach. They are
/// listed so the operator can add the ones they trust from the dashboard.
fn report_mcp(d: &AgentDraft, id: &str, report: &mut Report) {
    for name in d.mcp_servers.keys() {
        report.skipped(
            "mcp-servers",
            name,
            format!("外部工具連線不會自動匯入（給 {id}）；需要時請在儀表板另行設定"),
        );
        report.tag("mcp.skipped", json!({ "name": name }));
    }
}

impl Report {
    /// Absorb one shared-slot outcome line.
    pub(crate) fn add_outcome(&mut self, o: &Outcome) {
        match o.status {
            Status::Imported => {
                let name = if o.detail.is_empty() {
                    o.name.clone()
                } else {
                    format!("{} {}", o.name, o.detail)
                };
                self.imported(o.kind, &name)
            }
            Status::Skipped => self.skipped(o.kind, &o.name, o.detail.clone()),
            Status::Warning => self.warning(o.kind, &o.name, o.detail.clone()),
        }
        if let Some(code) = o.code {
            self.tag(code, o.params.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tools() {
        assert!(is_known_tool("Read"));
        assert!(is_known_tool("mcp__duduclaw__wiki_read"));
        assert!(!is_known_tool("search"));
        assert!(!is_known_tool("mcp__"));
    }

    fn planned_effects(source: &AgentDraft, id: &str, parent: &str, home: &Path) -> serde_json::Value {
        let stage = tempfile::tempdir().unwrap();
        let plan = plan_agent(source, id, parent, "claude-subagent", "team", stage.path(),
            &mut NameBook::load(home), &mut Report::default()).unwrap();
        json!({"profile": plan.draft, "files": file_hashes(id, &plan.files),
            "skills": plan.skill_effects, "wiki": file_hashes("wiki", &plan.wiki)})
    }

    #[test]
    fn resolved_plan_binds_model_tools_parent_names_settings_and_all_file_bodies() {
        let home = tempfile::tempdir().unwrap();
        let source = AgentDraft { id_hint: "worker".into(), display_name: "Worker".into(),
            soul: Some(super::super::ir::Doc::new("SOUL.md", "Help carefully.")),
            model: Some("sonnet".into()), tools: Some(vec!["Read".into()]),
            disallowed_tools: vec!["Bash".into()], ..Default::default() };
        let baseline = planned_effects(&source, "worker", "boss", home.path());
        for field in ["model", "tools", "denied", "name", "settings", "soul", "contract", "user", "identity", "rules", "wiki"] {
            let mut changed = source.clone();
            match field {
                "model" => changed.model = Some("haiku".into()),
                "tools" => changed.tools = None,
                "denied" => changed.disallowed_tools.clear(),
                "name" => changed.display_name = "Another worker".into(),
                "settings" => changed.settings = toml::from_str("[heartbeat]\nenabled = true").unwrap(),
                "soul" => changed.soul = Some(super::super::ir::Doc::new("SOUL.md", "Different persona")),
                "contract" => changed.contract = Some(super::super::ir::Doc::new("CONTRACT.toml", "[boundaries]\nmust_not = ['refund']")),
                "user" => changed.user_profile = Some(super::super::ir::Doc::new("USER.md", "Concise replies")),
                "identity" => changed.identity = Some(super::super::ir::Doc::new("IDENTITY.md", "Support")),
                "rules" => changed.instructions = Some(super::super::ir::Doc::new("INSTRUCTIONS.md", "Ask first")),
                _ => changed.knowledge.push(super::super::ir::Doc::new("guide.md", "# Guide\nKnowledge")),
            }
            assert_ne!(super::super::preview::digest(&baseline),
                super::super::preview::digest(&planned_effects(&changed, "worker", "boss", home.path())), "{field}");
        }
        assert_ne!(baseline, planned_effects(&source, "worker-imported", "boss", home.path()));
        assert_ne!(baseline, planned_effects(&source, "worker", "other-boss", home.path()));
        let existing = home.path().join("agents/existing");
        std::fs::create_dir_all(&existing).unwrap();
        std::fs::write(existing.join("agent.toml"), "[agent]\nname = 'existing'\ndisplay_name = 'Worker'").unwrap();
        let collision = planned_effects(&source, "worker", "boss", home.path());
        assert_ne!(baseline, collision);
        assert_eq!(collision["profile"]["display_name"], "Worker（worker）");
    }

    #[test]
    fn skill_body_changes_plan_and_confirmed_bytes_survive_source_replacement() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let skill = src.path().join("helper");
        std::fs::create_dir(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: helper\ndescription: Help\n---\nOriginal body").unwrap();
        let source = AgentDraft { id_hint: "worker".into(), display_name: "Worker".into(),
            skills: vec![super::super::ir::SkillDraft { dir: skill.clone(), name_hint: "helper".into(),
                label: None, version: None, source: "helper".into() }], ..Default::default() };
        let stage = tempfile::tempdir().unwrap();
        let plan = plan_agent(&source, "worker", "", "claude-subagent", "team", stage.path(),
            &mut NameBook::load(home.path()), &mut Report::default()).unwrap();
        let before = planned_effects(&source, "worker", "", home.path());
        std::fs::write(skill.join("SKILL.md"), "---\nname: helper\ndescription: Help\n---\nChanged body").unwrap();
        assert_ne!(before, planned_effects(&source, "worker", "", home.path()));
        assert!(std::str::from_utf8(&plan.skills[0].1["SKILL.md"]).unwrap().contains("Original body"));
        assert!(plan.files.contains_key(".mcp.json"));
        assert!(plan.files.contains_key("SKILLS/code-review/SKILL.md"));
    }

    #[test]
    fn skill_inputs_are_masked_before_staging_and_private_files_are_not_read() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let skill = src.path().join("helper");
        std::fs::create_dir_all(skill.join("refs")).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: helper\ndescription: Help\n---\npassword: hunter2").unwrap();
        std::fs::write(skill.join("refs/guide.md"), "password: hunter2").unwrap();
        // Reading this private file through the masker would refuse the plan.
        std::fs::write(skill.join(".env"), duduclaw_security::credential_detect::UNAVAILABLE_MASK).unwrap();
        let source = AgentDraft { id_hint: "worker".into(), display_name: "Worker".into(),
            skills: vec![super::super::ir::SkillDraft { dir: skill, name_hint: "helper".into(),
                label: None, version: None, source: "helper".into() }], ..Default::default() };
        let stage = tempfile::tempdir().unwrap();
        let plan = plan_agent(&source, "worker", "", "claude-subagent", "team", stage.path(),
            &mut NameBook::load(home.path()), &mut Report::default()).unwrap();
        let (prepared, files) = &plan.skills[0];
        assert!(prepared.dropped.iter().any(|path| path == ".env"));
        assert!(!files.contains_key(".env"));
        for file in ["SKILL.md", "refs/guide.md"] {
            let text = std::str::from_utf8(&files[file]).unwrap();
            assert!(!text.contains("hunter2"));
            assert!(text.contains("[已遮罩的密碼]"));
        }
        let staged = std::fs::read_to_string(stage.path().join("worker/inputs/0/helper/SKILL.md")).unwrap();
        assert!(!staged.contains("hunter2"));
        assert!(!stage.path().join("worker/inputs/0/helper/.env").exists());
    }

}
