//! Where the writer puts things besides the slots: skills (enabled,
//! quarantined behind an approval, or refused), the approval request, and the
//! allowlisted archive of the original files. Split out of `writer.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::json;

use super::ir::ImportBundle;
use super::skills::{PreparedSkill, Verdict};
use crate::expert::Report;
use crate::expert::install::InstallCtx;
use duduclaw_gateway::expert_skills::{self, QuarantinedSkill};

/// One employee's skills.
pub(super) struct SkillJob<'a> {
    pub id: &'a str,
    pub slug: &'a str,
}

pub(super) fn install_skills(
    ctx: &InstallCtx,
    job: &SkillJob<'_>,
    prepared: &[(PreparedSkill, BTreeMap<String, Vec<u8>>)],
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) -> duduclaw_core::error::Result<()> {
    for (p, files) in prepared {
        let shown = skill_label(p);
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
            Verdict::Enable => place_enabled(ctx, job.id, p, files, &shown, report)?,
            Verdict::Quarantine(reasons) => quarantine(ctx, job, p, files, &shown, reasons, report, quarantined)?,
        }
    }
    Ok(())
}

fn quarantine(
    ctx: &InstallCtx,
    job: &SkillJob<'_>,
    p: &PreparedSkill,
    files: &BTreeMap<String, Vec<u8>>,
    shown: &str,
    reasons: &[String],
    report: &mut Report,
    quarantined: &mut Vec<QuarantinedSkill>,
) -> duduclaw_core::error::Result<()> {
    let hard: Vec<_> = p.needs.iter().filter(|n| n.hard).collect();
    let mut digest = String::new();
    if !ctx.dry_run {
        let dest = expert_skills::quarantined_skill_dir(&ctx.home, job.slug, job.id, &p.name);
        let stored = write_skill_files(&ctx.home, files, &dest)
            .map_err(|e| e.to_string())
            .and_then(|_| expert_skills::tree_digest(&dest));
        match stored {
            Ok(d) => digest = d,
            Err(e) => {
                report.skipped("skill", shown, format!("暫存失敗：{e}"));
                report.tag("skill.failed", json!({ "skill": shown }));
                return Err(crate::expert::cfg_err(e));
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
    Ok(())
}

fn skill_label(p: &PreparedSkill) -> String {
    match &p.label {
        Some(l) if l != &p.name => format!("{l}（{}）", p.name),
        _ => p.name.clone(),
    }
}

fn place_enabled(
    ctx: &InstallCtx, id: &str, p: &PreparedSkill, files: &BTreeMap<String, Vec<u8>>,
    shown: &str, report: &mut Report,
) -> duduclaw_core::error::Result<()> {
    if !ctx.dry_run {
        let dest = ctx.home.join("agents").join(id).join("SKILLS").join(&p.name);
        write_skill_files(&ctx.home, files, &dest).map_err(|e| crate::expert::cfg_err(e.to_string()))?;
    }
    report.imported("skill", &format!("{shown} → {id}"));
    report.tag("skill.enabled", json!({"skill": shown}));
    Ok(())
}

fn write_skill_files(home: &Path, files: &BTreeMap<String, Vec<u8>>, dest: &Path) -> std::io::Result<()> {
    match dest.symlink_metadata() {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
        Ok(_) => return Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, "技能目的地已存在")),
    }
    let prefix = dest.strip_prefix(home).map_err(std::io::Error::other)?;
    for (rel, bytes) in files {
        let path = prefix.join(rel);
        let rel = path.to_str().ok_or_else(|| std::io::Error::other("技能目的地不是 UTF-8"))?;
        let rel = rel.replace('\\', "/");
        crate::expert::install::write_planned_file(home, &rel, bytes)
            .map_err(|e| std::io::Error::other(e.to_string()))?;
    }
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

/// Relative paths the archive may hold: every slot document's source file
/// plus every unmapped file, minus anything secret-shaped.
pub(super) fn archive_list(bundle: &ImportBundle) -> Vec<String> {
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
        let docs = [&a.soul, &a.identity, &a.instructions, &a.user_profile, &a.contract, &a.tools_notes, &a.heartbeat];
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
