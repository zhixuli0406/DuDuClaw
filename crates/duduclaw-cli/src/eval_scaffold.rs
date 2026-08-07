//! `duduclaw eval-scaffold` — free-tier eval authoring bootstrap.
//!
//! The AEE playbook `Add` pipeline hard-requires ≥1 linked eval case (G6)
//! plus E1 assertions (WP2.8) — evidence-based evolution by design. Paid
//! industry packs ship a reviewed suite; a self-authored agent has none, and
//! writing the first case from a blank page is the real barrier. This
//! command closes that gap WITHOUT any LLM and WITHOUT shipping content:
//! it derives draft cases from what the operator already wrote — the
//! agent's own SOUL.md behaviour rules (identity sections excluded, same
//! partition rules as `playbook migrate-soul`).
//!
//! Drafts are deliberately NOT runnable as-is: each `prompt` is a TODO the
//! operator must write (inventing user messages deterministically would be
//! fabrication), and drafts land in `<home>/evals-drafts/<agent>/` — outside
//! the live suites root — so a half-reviewed draft can never leak into a
//! real run. Review flow: fill in `prompt` + tighten `[expect]`, move the
//! file to `<home>/evals/<agent>/`, then `duduclaw eval <suite> --record`.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};

use crate::playbook_migrate::extract_rules;

/// Flags for `duduclaw eval-scaffold`.
pub struct ScaffoldOptions {
    pub agent: String,
    /// Overwrite existing drafts (default: skip files that already exist so
    /// an operator's in-progress edits are never clobbered).
    pub force: bool,
}

fn slugify(rule: &str, idx: usize) -> String {
    let mut s: String = rule
        .chars()
        .take(24)
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-');
    if s.len() < 4 {
        format!("rule-{:03}", idx + 1)
    } else {
        format!("{s}-{:03}", idx + 1)
    }
}

fn draft_case(agent: &str, section: &str, rule: &str, name: &str) -> String {
    let escaped_rule = rule.replace('"', "\\\"");
    format!(
        r#"# DRAFT — 由 `duduclaw eval-scaffold` 從 SOUL.md 產生,人審後才可用。
# 來源:SOUL.md「{section}」段規則:{rule}
# 步驟:1) 把 prompt 改成「會誘發這條規則」的真實使用者訊息
#       2) 依規則語意收緊 [expect](工具/輸出斷言至少一項)
#       3) 移到 <home>/evals/{agent}/ 後執行:duduclaw eval <該目錄> --record

[case]
name         = "{name}"
agent        = "{agent}"
prompt       = """
TODO: 寫一個會誘發上述規則的使用者訊息(草稿不可直接執行,防止未審閱的題目混入基線)
"""

[expect]
# 至少留一項並依規則調整;範例:
# must_use_tools     = ["memory_search"]
# must_not_use_tools = ["send_message"]
output_contains    = ["{escaped_rule_head}"]
min_text_blocks    = 1

[judge]
rubric = "回覆是否遵守:{escaped_rule}"
"#,
        section = section,
        rule = rule,
        name = name,
        agent = agent,
        escaped_rule = escaped_rule,
        escaped_rule_head = duduclaw_core::truncate_chars(&escaped_rule, 12),
    )
}

/// Entry point for `duduclaw eval-scaffold`.
pub async fn cmd_eval_scaffold(home: &Path, opts: ScaffoldOptions) -> Result<()> {
    if !duduclaw_core::is_valid_agent_id(&opts.agent) {
        return Err(DuDuClawError::Agent(format!(
            "invalid agent id {:?}: use 1-64 chars of [a-zA-Z0-9_-]",
            opts.agent
        )));
    }
    let soul_path = home.join("agents").join(&opts.agent).join("SOUL.md");
    let soul = std::fs::read_to_string(&soul_path)
        .map_err(|e| DuDuClawError::Agent(format!("read {}: {e}", soul_path.display())))?;

    let rules = extract_rules(&soul);
    if rules.is_empty() {
        println!(
            "查無可出題規則:SOUL.md 的行為分區(Evolvable/Observable)沒有條列規則。\n\
             提示:在 SOUL.md 的行為規則段用「- 」條列具體行為,再重跑本指令。"
        );
        return Ok(());
    }

    let out_dir = home.join("evals-drafts").join(&opts.agent);
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| DuDuClawError::Agent(format!("create {}: {e}", out_dir.display())))?;

    let mut written = 0usize;
    let mut skipped = 0usize;
    for (idx, (section, rule)) in rules.iter().enumerate() {
        let name = format!("draft-{}-{}", opts.agent, slugify(rule, idx));
        let path = out_dir.join(format!("{name}.toml"));
        if path.exists() && !opts.force {
            skipped += 1;
            continue;
        }
        std::fs::write(&path, draft_case(&opts.agent, section, rule, &name))
            .map_err(|e| DuDuClawError::Agent(format!("write {}: {e}", path.display())))?;
        written += 1;
    }

    println!(
        "已產生 {written} 份草稿題(略過既有 {skipped} 份)→ {}\n\
         下一步:填 prompt、收緊 [expect],移到 {} 後執行 `duduclaw eval` --record。",
        out_dir.display(),
        home.join("evals").join(&opts.agent).display(),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOUL: &str = "# A\n\n## 核心價值\n\n- 誠實\n\n## 行為規則\n\n- 回覆前先確認需求與驗收標準\n- 退款請求超過三十天時先查例外清單\n";

    fn home_with_soul() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agents").join("my-bot");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(agent.join("SOUL.md"), SOUL).unwrap();
        dir
    }

    #[tokio::test]
    async fn scaffolds_drafts_outside_the_live_suites_root() {
        let home = home_with_soul();
        cmd_eval_scaffold(home.path(), ScaffoldOptions { agent: "my-bot".into(), force: false })
            .await
            .unwrap();
        let drafts = home.path().join("evals-drafts").join("my-bot");
        let files: Vec<_> = std::fs::read_dir(&drafts).unwrap().flatten().collect();
        assert_eq!(files.len(), 2, "one draft per behaviour rule, identity skipped");
        // Live suites root untouched — a draft can never run by accident.
        assert!(!home.path().join("evals").join("my-bot").exists());
        let body = std::fs::read_to_string(files[0].path()).unwrap();
        assert!(body.contains("TODO"), "prompt must demand human authorship");
        assert!(body.contains("agent        = \"my-bot\""));
        // A draft parses as a valid case file once the prompt is filled — the
        // structure must already satisfy the loader's schema.
        assert!(body.contains("[case]") && body.contains("[expect]"));
    }

    #[tokio::test]
    async fn rerun_never_clobbers_edited_drafts_without_force() {
        let home = home_with_soul();
        let opts = || ScaffoldOptions { agent: "my-bot".into(), force: false };
        cmd_eval_scaffold(home.path(), opts()).await.unwrap();
        let drafts = home.path().join("evals-drafts").join("my-bot");
        let first = std::fs::read_dir(&drafts).unwrap().flatten().next().unwrap().path();
        std::fs::write(&first, "operator edits").unwrap();
        cmd_eval_scaffold(home.path(), opts()).await.unwrap();
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "operator edits");
    }
}
