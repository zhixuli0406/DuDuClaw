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
    InstallRecord, PackKind, Report, ScanVerdict, cfg_err, now_iso, scan_external,
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
    bundle: ImportBundle,
    root: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    report.imported("format", &bundle.format_label);
    report.tag("format.detected", json!({ "format": bundle.format_code }));
    for w in &bundle.warnings {
        report.warning("import", "", w.clone());
        report.tag("import.note", json!({}));
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
    // The cap counts every employee this import would add, before anything
    // (even the archived original) is written.
    ctx.ensure_quota(id_map.len()).await?;
    ctx.begin_writes()?;
    report.meta("slug", &slug);
    report.meta("format", bundle.format_code);

    let archive_base = ctx.home.join("imported").join(&slug).join("source");
    if !ctx.dry_run {
        placement::archive_source(root, &bundle, &archive_base, report);
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
    let stage = tempfile::tempdir().map_err(|e| cfg_err(format!("建立暫存目錄失敗：{e}")))?;
    let mut quarantined: Vec<QuarantinedSkill> = Vec::new();
    for draft in &order {
        if ctx.stopping() {
            break;
        }
        let Some(id) = id_map.get(&draft.ref_id).cloned() else {
            continue;
        };
        let parent = draft
            .reports_to
            .as_ref()
            .and_then(|r| id_map.get(r).cloned())
            .or_else(|| ctx.attach_under.clone())
            .unwrap_or_default();
        let job = AgentJob {
            draft,
            id: &id,
            parent: &parent,
            slug: &slug,
            format: bundle.format_code,
            lineage_prefix: &lineage_prefix,
        };
        if install_agent(ctx, &job, stage.path(), &mut book, report, &mut quarantined).await? {
            record.agents.push(id);
        } else {
            id_map.remove(&draft.ref_id);
        }
    }
    for u in &bundle.unmapped {
        report.ignored("archive", &u.rel, u.reason.clone());
        report.tag("archive.unmapped", json!({ "file": u.rel }));
    }
    placement::file_quarantine(ctx, &slug, quarantined, report).await;
    if record.agents.is_empty() {
        if !ctx.dry_run {
            let _ = std::fs::remove_dir_all(ctx.home.join("imported").join(&slug));
        }
        let problems = report.agent_problems();
        return Err(import_err(
            "import.nothing_imported",
            json!({ "agents": problems }),
            "沒有任何 AI 員工匯入成功（原因見匯入報告裡每位員工的說明）",
        ));
    }
    Ok(record)
}

struct AgentJob<'a> {
    draft: &'a AgentDraft,
    id: &'a str,
    parent: &'a str,
    slug: &'a str,
    format: &'static str,
    lineage_prefix: &'a str,
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
    ctx: &InstallCtx,
    job: &AgentJob<'_>,
    stage: &Path,
    book: &mut NameBook,
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) -> Result<bool> {
    let d = job.draft;
    let id = job.id;
    // SOUL is the one review point (employees cannot edit it later).
    let soul = match &d.soul {
        Some(doc) => match scan_external(&doc.text) {
            ScanVerdict::Ok => Some(doc.text.clone()),
            ScanVerdict::Blocked(reason) => {
                report.skipped(
                    "agent",
                    id,
                    format!("人格設定（{}）未通過內容安全檢查，整位不匯入：{reason}", doc.source),
                );
                report.tag("agent.soul_blocked", json!({ "id": id }));
                return Ok(false);
            }
        },
        None => None,
    };
    let wanted = {
        let n = clean_display_name(&d.display_name);
        if n.is_empty() { id.to_string() } else { n }
    };
    // `@<display name>` calls an employee in every channel: never hand a new
    // employee a name someone on the team already answers to.
    let (display, renamed) = book.unique_display(&wanted, id);
    report.imported("agent", &format!("{display}（{id}）"));
    report.tag("agent.created", json!({ "display": display, "id": id }));
    if renamed {
        report.warning(
            "agent",
            id,
            format!("「{wanted}」已是其他 AI 員工的名稱或呼叫詞，改用「{display}」，呼叫詞為 @{display}"),
        );
        report.tag("agent.display_changed", json!({ "from": wanted, "to": display }));
    }
    match &d.soul {
        Some(doc) => {
            report.imported("soul", &format!("{} → 人格設定（SOUL.md，AI 員工只能讀）", doc.source));
            report.tag("slot.written", json!({ "slot": "soul" }));
        }
        None => {
            report.ignored("soul", id, "來源沒有人格設定，使用預設人格");
            report.tag("slot.soul_default", json!({}));
        }
    }
    let model = map_model(d.model.as_deref(), id, report);

    if !ctx.dry_run {
        let scaffold = crate::AgentScaffold {
            name: id.to_string(),
            display_name: display.clone(),
            role: "worker".into(),
            reports_to: job.parent.to_string(),
            icon: d.emoji.clone().unwrap_or_else(|| "🐾".into()),
            trigger: format!("@{display}"),
            provider: duduclaw_core::types::RuntimeType::Claude,
            model_preferred: model,
            soul_body: soul,
        };
        ctx.note_agent(id);
        if let Err(e) = crate::scaffold_agent_dir(&ctx.home, &scaffold).await {
            report.skipped("agent", id, format!("建立失敗：{e}"));
            report.tag("agent.failed", json!({ "id": id }));
            return Ok(false);
        }
    }

    let slot_ctx = SlotCtx {
        home: &ctx.home,
        agent_id: id,
        dry_run: ctx.dry_run,
        lineage_prefix: job.lineage_prefix,
        format_code: job.format,
        apply_identity_fields: false,
    };
    for o in write_slots(&slot_ctx, d).await {
        report.add_outcome(&o);
    }
    if let Err(e) = apply_tools(ctx, d, id, report) {
        report.warning("agent-tools", id, format!("工具限制未套用：{e}"));
        report.tag("tools.failed", json!({}));
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
    let skill_job = SkillJob {
        draft: d,
        id,
        slug: job.slug,
        format: job.format,
    };
    placement::install_skills(ctx, &skill_job, stage, report, quarantined);
    Ok(true)
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
fn apply_tools(ctx: &InstallCtx, d: &AgentDraft, id: &str, report: &mut Report) -> Result<()> {
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
        return Ok(());
    }
    let list = |v: &[String]| if v.is_empty() { "—".to_string() } else { v.join(", ") };
    report.imported(
        "agent-tools",
        &format!("{id}：只允許 {}；禁用 {}", list(&allowed), list(&denied)),
    );
    report.tag("tools.narrowed", json!({ "allowed": list(&allowed), "denied": list(&denied) }));
    if ctx.dry_run {
        return Ok(());
    }
    crate::expert::patch_agent_toml(&ctx.home, id, |t| {
        crate::expert::set_capabilities(t, &allowed, &denied)
    })
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
}
