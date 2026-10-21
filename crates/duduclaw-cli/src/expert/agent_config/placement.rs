//! Where the writer puts things besides the slots: skills (enabled,
//! quarantined behind an approval, or refused), the approval request, and the
//! allowlisted archive of the original files. Split out of `writer.rs`.

use std::collections::BTreeSet;
use std::path::Path;

use serde_json::json;

use super::ir::{AgentDraft, ImportBundle};
use super::skills::{self, PreparedSkill, Verdict};
use crate::expert::Report;
use crate::expert::install::InstallCtx;
use duduclaw_gateway::expert_skills::{self, QuarantinedSkill};

/// One employee's skills.
pub(super) struct SkillJob<'a> {
    pub draft: &'a AgentDraft,
    pub id: &'a str,
    pub slug: &'a str,
    pub format: &'static str,
}

pub(super) fn install_skills(
    ctx: &InstallCtx,
    job: &SkillJob<'_>,
    stage: &Path,
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) {
    let agent_stage = stage.join(job.id);
    let mut taken = BTreeSet::new();
    for draft in &job.draft.skills {
        let p = skills::prepare(draft, &agent_stage, job.format, &mut taken);
        let shown = skill_label(&p);
        let base = json!({ "skill": shown });
        if let Some(old) = &p.renamed_from {
            report.ignored(
                "skill-name",
                &shown,
                format!("技能代號由「{old}」改為「{}」（原名保留在技能資訊）", p.name),
            );
            report.tag("skill.renamed", json!({ "skill": shown, "from": old, "to": p.name }));
        }
        if !p.dropped.is_empty() {
            report.ignored(
                "skill-files",
                &shown,
                format!("已略過含金鑰、隱藏或捷徑的檔案：{}", p.dropped.join("、")),
            );
            report.tag("skill.files_dropped", json!({ "skill": shown, "count": p.dropped.len() }));
        }
        for need in p.needs.iter().filter(|n| !n.hard) {
            report.warning(
                "skill-needs",
                &shown,
                format!("內容提到{}，在 DuDuClaw 可能需要改用其他方式交付", need.label),
            );
            report.tag("skill.soft_need", json!({ "skill": shown, "need": need.code }));
        }
        match &p.verdict {
            Verdict::Blocked(why) => {
                report.skipped("skill", &shown, why.clone());
                report.tag("skill.blocked", base);
            }
            Verdict::Enable => place_enabled(ctx, job.id, &p, &shown, report),
            Verdict::Quarantine(reasons) => quarantine(ctx, job, &p, &shown, reasons, report, quarantined),
        }
    }
}

fn quarantine(
    ctx: &InstallCtx,
    job: &SkillJob<'_>,
    p: &PreparedSkill,
    shown: &str,
    reasons: &[String],
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) {
    let hard: Vec<_> = p.needs.iter().filter(|n| n.hard).collect();
    let mut digest = String::new();
    if !ctx.dry_run {
        let dest = expert_skills::quarantined_skill_dir(&ctx.home, job.slug, job.id, &p.name);
        let stored = move_dir(&p.staged_dir, &dest)
            .map_err(|e| e.to_string())
            .and_then(|_| expert_skills::tree_digest(&dest));
        match stored {
            Ok(d) => digest = d,
            Err(e) => {
                report.skipped("skill", shown, format!("暫存失敗：{e}"));
                report.tag("skill.failed", json!({ "skill": shown }));
                return;
            }
        }
    }
    report.warning("skill-disabled", shown, format!("先停用，待管理者核准：{}", reasons.join("；")));
    report.tag(
        "skill.quarantined",
        json!({
            "skill": shown,
            "scripts": p.scripts.len(),
            "needs": hard.iter().map(|n| n.code).collect::<Vec<_>>(),
        }),
    );
    quarantined.push(QuarantinedSkill {
        agent: job.id.to_string(),
        name: p.name.clone(),
        label: p.label.clone().unwrap_or_default(),
        reasons: reasons.to_vec(),
        needs: hard.iter().map(|n| n.label.to_string()).collect(),
        digest,
    });
}

fn skill_label(p: &PreparedSkill) -> String {
    match &p.label {
        Some(l) if l != &p.name => format!("{l}（{}）", p.name),
        _ => p.name.clone(),
    }
}

fn place_enabled(ctx: &InstallCtx, id: &str, p: &PreparedSkill, shown: &str, report: &mut Report) {
    let params = json!({ "skill": shown });
    if ctx.dry_run {
        report.imported("skill", &format!("{shown} → {id}"));
        report.tag("skill.enabled", params);
        return;
    }
    let dest = ctx.home.join("agents").join(id).join("SKILLS").join(&p.name);
    if dest.exists() {
        report.conflict("skill", shown, "這位 AI 員工已有同名技能，未覆蓋");
        report.tag("skill.conflict", params);
        return;
    }
    match move_dir(&p.staged_dir, &dest) {
        Ok(()) => {
            report.imported("skill", &format!("{shown} → {id}"));
            report.tag("skill.enabled", params);
        }
        Err(e) => {
            report.skipped("skill", shown, format!("安裝失敗：{e}"));
            report.tag("skill.failed", params);
        }
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

pub(super) async fn file_quarantine(
    ctx: &InstallCtx,
    slug: &str,
    skills: Vec<QuarantinedSkill>,
    report: &mut Report,
) {
    if skills.is_empty() {
        return;
    }
    let count = skills.len();
    report.meta("disabled_skills", &count.to_string());
    if ctx.dry_run {
        return;
    }
    match expert_skills::request_enable(&ctx.home, slug, skills).await {
        Ok(id) => {
            report.meta("skills_approval_id", &id);
            report.warning(
                "skill-disabled",
                slug,
                format!(
                    "已送出審核（編號 {id}）：在儀表板「待辦」核准後，按「套用審核結果」或執行 `duduclaw expert skills {slug}` 啟用"
                ),
            );
            report.tag("skills.approval_filed", json!({ "count": count }));
        }
        Err(e) => {
            report.warning("skill-disabled", slug, format!("審核請求建立失敗，技能維持停用：{e}"));
            report.tag("skills.approval_failed", json!({ "count": count }));
        }
    }
}

/// Keep the files the import read or could not map under
/// `<home>/imported/<slug>/source/` — the archive for unmapped files.
/// Allowlist only: credential files, vendor config files (`openclaw.json`,
/// Hermes `config.yaml`), session logs and hidden folders are never copied,
/// and the total stays under the archive size cap.
pub(super) fn archive_source(root: &Path, bundle: &ImportBundle, dest: &Path, report: &mut Report) {
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
            report.tag("archive.cap", json!({}));
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
            Err(e) => {
                report.warning("archive", rel, format!("備份失敗：{e}"));
                report.tag("archive.failed", json!({}));
            }
        }
    }
    if copied > 0 {
        report.imported("archive", &format!("原始檔 {copied} 個已保存"));
        report.tag("archive.saved", json!({ "count": copied }));
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
        let docs = [&a.soul, &a.identity, &a.instructions, &a.user_profile, &a.tools_notes, &a.heartbeat];
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
