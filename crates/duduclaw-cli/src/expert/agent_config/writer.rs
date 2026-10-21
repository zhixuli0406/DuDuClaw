//! The one writer: turns an [`ImportBundle`] into employees, following the
//! `hermes claw migrate` playbook — dry run first, explicit conflict policy
//! (`--rename`), the original kept under `<home>/imported/<slug>/source/`
//! (credential files excluded), secrets never migrated, and every file
//! accounted for in the report.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use duduclaw_core::error::Result;

use super::ir::{AgentDraft, ImportBundle};
use super::outcome::{Outcome, Status};
use super::skills::{self, PreparedSkill, Verdict};
use super::slots::{SlotCtx, clean_display_name, write_slots};
use crate::expert::install::InstallCtx;
use crate::expert::{
    InstallRecord, PackKind, Report, ScanVerdict, cfg_err, now_iso, scan_external,
};
use duduclaw_gateway::expert_skills::{self, QuarantinedSkill};

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
    for w in &bundle.warnings {
        report.warning("import", "", w.clone());
    }

    let order = topo_order(&bundle.agents);
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let mut planned_ids: BTreeSet<String> = BTreeSet::new();
    for draft in &order {
        let id = resolve_id(ctx, draft, bundle.format_code, &planned_ids, report);
        if let Some(id) = id {
            planned_ids.insert(id.clone());
            id_map.insert(draft.ref_id.clone(), id);
        }
    }
    let slug = choose_slug(ctx, &bundle, &order, &id_map)?;
    report.meta("slug", &slug);
    report.meta("format", bundle.format_code);

    let archive_base = ctx.home.join("imported").join(&slug).join("source");
    if !ctx.dry_run {
        archive_source(root, &bundle, &archive_base, report);
    }

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
        installed_at: now_iso(),
        source_format: bundle.format_code.to_string(),
    };
    let stage = tempfile::tempdir().map_err(|e| cfg_err(format!("建立暫存目錄失敗：{e}")))?;
    let mut quarantined: Vec<QuarantinedSkill> = Vec::new();
    for draft in &order {
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
        };
        if install_agent(
            ctx,
            &job,
            &archive_base,
            stage.path(),
            report,
            &mut quarantined,
        )
        .await?
        {
            record.agents.push(id);
        } else {
            id_map.remove(&draft.ref_id);
        }
    }
    for u in &bundle.unmapped {
        report.ignored("archive", &u.rel, u.reason.clone());
    }
    file_quarantine(ctx, &slug, quarantined, report).await;
    if record.agents.is_empty() && !ctx.dry_run {
        let _ = std::fs::remove_dir_all(ctx.home.join("imported").join(&slug));
        return Err(cfg_err("沒有任何 AI 員工匯入成功（請看上方原因）".into()));
    }
    Ok(record)
}

struct AgentJob<'a> {
    draft: &'a AgentDraft,
    id: &'a str,
    parent: &'a str,
    slug: &'a str,
    format: &'static str,
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
/// hash; then the conflict policy (`--rename` → `-imported`).
fn resolve_id(
    ctx: &InstallCtx,
    draft: &AgentDraft,
    format: &str,
    planned: &BTreeSet<String>,
    report: &mut Report,
) -> Option<String> {
    let candidates = [
        crate::sanitize_agent_id(&draft.id_hint),
        crate::sanitize_agent_id(&format!("{format}-{}", draft.ref_id)),
        format!(
            "{format}-{}",
            &crate::expert::registry::sha256_hex(draft.display_name.as_bytes())[..8]
        ),
    ];
    let base = candidates
        .into_iter()
        .find(|c| crate::is_valid_agent_id(c) && !duduclaw_core::is_reserved_agent_id(c))?;
    let taken = |id: &str| ctx.home.join("agents").join(id).exists() || planned.contains(id);
    if !taken(&base) {
        return Some(base);
    }
    if ctx.rename {
        let renamed = crate::sanitize_agent_id(&format!("{base}-imported"));
        if crate::is_valid_agent_id(&renamed) && !taken(&renamed) {
            report.warning(
                "agent",
                &base,
                format!("同名 AI 員工已存在，改以 {renamed} 匯入"),
            );
            return Some(renamed);
        }
        report.conflict("agent", &base, "重新命名後仍與既有 AI 員工衝突");
        return None;
    }
    report.conflict(
        "agent",
        &base,
        "同名 AI 員工已存在（加 --rename 以 -imported 結尾匯入）",
    );
    None
}

fn choose_slug(
    ctx: &InstallCtx,
    bundle: &ImportBundle,
    order: &[AgentDraft],
    ids: &BTreeMap<String, String>,
) -> Result<String> {
    let hint = crate::sanitize_agent_id(&bundle.slug_hint);
    let first = order
        .first()
        .and_then(|a| ids.get(&a.ref_id))
        .cloned()
        .unwrap_or_default();
    let slug = [hint, first, format!("{}-import", bundle.format_code)]
        .into_iter()
        .find(|s| crate::is_valid_agent_id(s))
        .ok_or_else(|| cfg_err("無法產生匯入代號".into()))?;
    if crate::expert::read_record(&ctx.home, &slug).is_some() {
        return Err(cfg_err(format!(
            "「{slug}」已經匯入過（先在儀表板移除，或執行 `duduclaw expert remove {slug}` 後再匯入）"
        )));
    }
    Ok(slug)
}

async fn install_agent(
    ctx: &InstallCtx,
    job: &AgentJob<'_>,
    archive_base: &Path,
    stage: &Path,
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
                    format!(
                        "人格設定（{}）未通過內容安全檢查，整位不匯入：{reason}",
                        doc.source
                    ),
                );
                return Ok(false);
            }
        },
        None => None,
    };
    let display = {
        let n = clean_display_name(&d.display_name);
        if n.is_empty() { id.to_string() } else { n }
    };
    report.imported("agent", &format!("{display}（{id}）"));
    match &d.soul {
        Some(doc) => report.add_outcome(&Outcome::imported(
            "soul",
            &doc.source,
            "→ 人格設定（SOUL.md，AI 員工只能讀）",
        )),
        None => report.ignored("soul", id, "來源沒有人格設定，使用預設人格"),
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
        if let Err(e) = crate::scaffold_agent_dir(&ctx.home, &scaffold).await {
            report.skipped("agent", id, format!("建立失敗：{e}"));
            return Ok(false);
        }
    }

    let slot_ctx = SlotCtx {
        home: &ctx.home,
        agent_id: id,
        dry_run: ctx.dry_run,
        lineage_base: archive_base,
        format_code: job.format,
        apply_identity_fields: false,
    };
    for o in write_slots(&slot_ctx, d).await {
        report.add_outcome(&o);
    }
    if let Err(e) = apply_tools(ctx, d, id, report) {
        report.warning("agent-tools", id, format!("工具限制未套用：{e}"));
    }
    report_mcp(d, id, report);
    for (field, why) in &d.ignored_fields {
        report.ignored("field", field, why.clone());
    }
    for (src, why) in &d.skipped {
        report.skipped("file", src, why.clone());
    }
    for u in &d.unmapped {
        report.ignored("archive", &u.rel, u.reason.clone());
    }
    install_skills(ctx, job, stage, report, quarantined);
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
                format!(
                    "原本使用的模型「{other}」不是 Claude，已改用預設模型；需要時請在儀表板調整"
                ),
            );
            return None;
        }
    };
    report.imported("agent-model", &format!("{id} → {mapped}"));
    Some(mapped)
}

/// Imported tool lists only narrow.
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
        }
    }
    for t in &d.disallowed_tools {
        if is_known_tool(t) {
            denied.push(t.clone())
        } else {
            dropped.push(t.clone())
        }
    }
    if !dropped.is_empty() {
        report.ignored(
            "agent-tools",
            id,
            format!("對不上的工具名稱未套用：{}", dropped.join("、")),
        );
    }
    if allowed.is_empty() && denied.is_empty() {
        return Ok(());
    }
    report.imported(
        "agent-tools",
        &format!("{id}：可用 {} 項、禁用 {} 項", allowed.len(), denied.len()),
    );
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
    }
}

fn install_skills(
    ctx: &InstallCtx,
    job: &AgentJob<'_>,
    stage: &Path,
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) {
    let agent_stage = stage.join(job.id);
    let mut taken = BTreeSet::new();
    for draft in &job.draft.skills {
        let p = skills::prepare(draft, &agent_stage, job.format, &mut taken);
        let shown = skill_label(&p);
        if let Some(old) = &p.renamed_from {
            report.ignored(
                "skill-name",
                &shown,
                format!(
                    "技能代號由「{old}」改為「{}」（原名保留在技能資訊）",
                    p.name
                ),
            );
        }
        for need in p.needs.iter().filter(|n| !n.hard) {
            report.warning(
                "skill-needs",
                &shown,
                format!(
                    "內容提到{}，在 DuDuClaw 可能需要改用其他方式交付",
                    need.label
                ),
            );
        }
        match &p.verdict {
            Verdict::Blocked(why) => report.skipped("skill", &shown, why.clone()),
            Verdict::Enable => place_enabled(ctx, job.id, &p, &shown, report),
            Verdict::Quarantine(reasons) => {
                let needs: Vec<String> = p
                    .needs
                    .iter()
                    .filter(|n| n.hard)
                    .map(|n| n.label.to_string())
                    .collect();
                let mut digest = String::new();
                if !ctx.dry_run {
                    let dest =
                        expert_skills::quarantined_skill_dir(&ctx.home, job.slug, job.id, &p.name);
                    if let Err(e) = move_dir(&p.staged_dir, &dest) {
                        report.skipped("skill", &shown, format!("暫存失敗：{e}"));
                        continue;
                    }
                    match expert_skills::tree_digest(&dest) {
                        Ok(d) => digest = d,
                        Err(e) => {
                            report.skipped("skill", &shown, format!("暫存失敗：{e}"));
                            continue;
                        }
                    }
                }
                report.warning(
                    "skill-disabled",
                    &shown,
                    format!("先停用，待管理者核准：{}", reasons.join("；")),
                );
                quarantined.push(QuarantinedSkill {
                    agent: job.id.to_string(),
                    name: p.name.clone(),
                    label: p.label.clone().unwrap_or_default(),
                    reasons: reasons.clone(),
                    needs,
                    digest,
                });
            }
        }
    }
}

fn skill_label(p: &PreparedSkill) -> String {
    match &p.label {
        Some(l) if l != &p.name => format!("{l}（{}）", p.name),
        _ => p.name.clone(),
    }
}

fn place_enabled(ctx: &InstallCtx, id: &str, p: &PreparedSkill, shown: &str, report: &mut Report) {
    if ctx.dry_run {
        report.imported("skill", &format!("{shown} → {id}"));
        return;
    }
    let dest = ctx
        .home
        .join("agents")
        .join(id)
        .join("SKILLS")
        .join(&p.name);
    if dest.exists() {
        report.conflict("skill", shown, "這位 AI 員工已有同名技能，未覆蓋");
        return;
    }
    match move_dir(&p.staged_dir, &dest) {
        Ok(()) => report.imported("skill", &format!("{shown} → {id}")),
        Err(e) => report.skipped("skill", shown, format!("安裝失敗：{e}")),
    }
}

fn move_dir(src: &Path, dest: &Path) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::expert::copy_dir(src, dest)?;
    let _ = std::fs::remove_dir_all(src);
    Ok(())
}

async fn file_quarantine(
    ctx: &InstallCtx,
    slug: &str,
    skills: Vec<QuarantinedSkill>,
    report: &mut Report,
) {
    if skills.is_empty() {
        return;
    }
    report.meta("disabled_skills", &skills.len().to_string());
    if ctx.dry_run {
        return;
    }
    match expert_skills::request_enable(&ctx.home, slug, skills).await {
        Ok(id) => {
            report.meta("skills_approval_id", &id);
            report.warning(
                "skill-disabled",
                slug,
                format!("已送出審核（編號 {id}）：在儀表板「待辦」核准後，按「套用審核結果」或執行 `duduclaw expert skills {slug}` 啟用"),
            );
        }
        Err(e) => report.warning(
            "skill-disabled",
            slug,
            format!("審核請求建立失敗，技能維持停用：{e}"),
        ),
    }
}

/// Keep the files the import read or could not map under
/// `<home>/imported/<slug>/source/` — the archive for unmapped files and the
/// canonical lineage base for imported memory. Allowlist only: credential
/// files, vendor config files (`openclaw.json`, Hermes `config.yaml`),
/// session logs and hidden folders are never copied, and the total stays
/// under the archive size cap.
fn archive_source(root: &Path, bundle: &ImportBundle, dest: &Path, report: &mut Report) {
    let files = archive_list(bundle);
    let mut copied = 0usize;
    let mut total: u64 = 0;
    for rel in &files {
        let src = root.join(rel);
        let Ok(meta) = std::fs::symlink_metadata(&src) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        total = total.saturating_add(meta.len());
        if total > crate::expert::safe_zip::MAX_UNPACK_BYTES {
            report.warning("archive", "", "原始檔超過備份上限，其餘檔案未保存");
            break;
        }
        let to = dest.join(rel);
        let res = to
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| std::fs::copy(&src, &to).map(|_| ()));
        match res {
            Ok(()) => copied += 1,
            Err(e) => report.warning("archive", rel, format!("備份失敗：{e}")),
        }
    }
    if copied > 0 {
        report.imported(
            "archive",
            &format!("原始檔 {copied} 個 → imported/{}", rel_tail(dest)),
        );
    }
}

/// Relative paths the archive may hold: every slot document's source file
/// plus every unmapped file, minus anything secret-shaped.
fn archive_list(bundle: &ImportBundle) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let mut add = |src: &str| {
        let file = src.split('#').next().unwrap_or(src);
        let parts: Vec<&str> = file.split('/').collect();
        let unsafe_part = parts.iter().any(|p| {
            p.is_empty()
                || *p == ".."
                || p.starts_with('.')
                || super::adapters::is_secret_file(p)
                || super::adapters::is_secret_dir(p)
        });
        if !unsafe_part {
            out.insert(file.to_string());
        }
    };
    for u in &bundle.unmapped {
        add(&u.rel);
    }
    for a in &bundle.agents {
        let docs = [
            &a.soul,
            &a.identity,
            &a.instructions,
            &a.user_profile,
            &a.tools_notes,
            &a.heartbeat,
        ];
        for d in docs.into_iter().flatten() {
            add(&d.source);
        }
        for d in a.memory.iter().chain(&a.knowledge) {
            add(&d.source);
        }
        for u in &a.unmapped {
            add(&u.rel);
        }
    }
    out.into_iter().collect()
}

fn rel_tail(p: &Path) -> String {
    let parts: Vec<String> = p
        .components()
        .rev()
        .take(2)
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
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
