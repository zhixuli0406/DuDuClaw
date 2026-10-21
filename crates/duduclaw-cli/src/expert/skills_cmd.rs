//! `duduclaw expert skills <slug>` — apply the approval decision for the
//! skills an agent import left disabled (scripts, or another vendor's
//! tools). Thin console renderer over
//! [`duduclaw_gateway::expert_skills::apply_skills_decision`], which also
//! backs the dashboard `experts.skills_apply` RPC. Never enables without an
//! approved request.

use std::path::Path;

use console::style;

use duduclaw_core::error::Result;
use duduclaw_gateway::expert_skills::{self, SkillsApplyOutcome};

use super::cfg_err;

pub async fn cmd_skills(home: &Path, slug: &str) -> Result<()> {
    if !crate::is_valid_agent_id(slug) {
        return Err(cfg_err(format!(
            "'{}' 不是合法的匯入代號",
            slug.escape_debug()
        )));
    }
    match expert_skills::apply_skills_decision(home, slug)
        .await
        .map_err(cfg_err)?
    {
        SkillsApplyOutcome::Enabled { count, not_enabled } => {
            println!("\n  {} 已啟用 {count} 個匯入的技能。", style("✓").green());
            if !not_enabled.is_empty() {
                println!(
                    "  {} 以下技能未啟用（AI 員工已不存在、已有同名技能或檔案缺失）：{}",
                    style("⚠").yellow(),
                    not_enabled.join("、")
                );
            }
            println!();
        }
        SkillsApplyOutcome::Disabled => {
            println!(
                "\n  {} '{slug}' 匯入的技能目前停用（沒有有效的核准）。\n",
                style("ℹ").cyan()
            );
        }
        SkillsApplyOutcome::StillPending { approval_id } => {
            println!(
                "\n  {} 審核仍在等待決定（編號 {approval_id}）。請在儀表板「待辦」核准或拒絕後再執行一次。\n",
                style("…").yellow()
            );
        }
        SkillsApplyOutcome::DeniedOrExpired { status } => {
            let verdict = if status == "denied" {
                "已被拒絕"
            } else {
                "已逾期（視同拒絕）"
            };
            println!(
                "\n  {} 審核{verdict}，'{slug}' 匯入的技能維持停用。\n",
                style("✗").red()
            );
        }
    }
    Ok(())
}
