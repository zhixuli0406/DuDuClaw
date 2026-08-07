//! `duduclaw playbook migrate-soul` — WP1.4: harvest the behaviour rules the
//! GVU era accumulated inside an agent's SOUL.md into reviewable playbook
//! `Add` drafts, then (after human review) apply them through the normal
//! delta validation pipeline.
//!
//! Two-step, human-in-the-middle (D1=A: evolution's destination is playbook
//! entries; D4: relaxation/promotion is always human-reviewed):
//!
//! 1. `duduclaw playbook migrate-soul --agent <id>` — parse the agent's
//!    CURRENT SOUL.md (the version store keeps only hashes + 200-char
//!    summaries, so full historical diffs are not reconstructable — the live
//!    file IS the accumulated state), extract candidate rules from
//!    Evolvable/Observable sections (identity sections are never touched),
//!    dedup, and write `playbook_migration_draft.toml` beside the agent for
//!    review. Nothing is written to the playbook.
//! 2. The operator reviews the draft: flips `apply = true` on the keepers
//!    and fills each keeper's `eval_cases` + E1 `assertions` (both are hard
//!    requirements of the `Add` pipeline — G6 and WP2.8/D8; the draft cannot
//!    invent them, and auto-inventing weak ones would be exactly the
//!    surface WP2.10's H2 exists to catch).
//! 3. `duduclaw playbook migrate-soul --agent <id> --apply` — every
//!    `apply = true` rule goes through `playbook::store::apply_deltas` (the
//!    same write path AEE uses: full validation, dedup, novelty). Per-rule
//!    accept/reject is reported; a rejection never aborts the batch.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::gvu::soul_partition::{PartitionedSoul, SectionMutability};
use duduclaw_gateway::playbook::{self, delta::PlaybookDelta, entry::EntryAssertions, gene::EvalCaseRef};

/// Flags for `duduclaw playbook migrate-soul`.
pub struct MigrateOptions {
    pub agent: String,
    /// Step 2: apply the reviewed draft instead of (re)generating it.
    pub apply: bool,
    /// Step 1 only: report what would be drafted without writing the file.
    pub dry_run: bool,
}

const DRAFT_FILE: &str = "playbook_migration_draft.toml";

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct Draft {
    /// Where the rules came from (for the reviewer's context).
    source: String,
    #[serde(default)]
    rule: Vec<DraftRule>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DraftRule {
    /// The extracted rule text (edit freely before applying).
    text: String,
    /// Which SOUL.md section it came from.
    section: String,
    /// Reviewer gate: only `true` rules are applied.
    #[serde(default)]
    apply: bool,
    /// `repair` / `optimize` / `innovate` — see PLAYBOOK_EDITING_GUIDE.
    #[serde(default = "default_category")]
    category: String,
    /// Signal tokens (see the guide's vocabulary). `*` = always-on (quota'd).
    #[serde(default)]
    signals_match: Vec<String>,
    /// REQUIRED before apply (G6): at least one `"<suite>/<case>"` ref.
    #[serde(default)]
    eval_cases: Vec<String>,
    /// REQUIRED before apply (WP2.8 E1): at least one non-empty list.
    #[serde(default)]
    must_use_tools: Vec<String>,
    #[serde(default)]
    must_not_use_tools: Vec<String>,
    #[serde(default)]
    output_contains: Vec<String>,
    #[serde(default)]
    output_not_contains: Vec<String>,
}

fn default_category() -> String {
    "repair".to_string()
}

/// Draft-file category string → enum; unknown values fall back to `Repair`
/// (the conservative default for migrated legacy rules).
fn parse_category(s: &str) -> playbook::entry::PlaybookCategory {
    use playbook::entry::PlaybookCategory as C;
    match s.trim().to_ascii_lowercase().as_str() {
        "optimize" => C::Optimize,
        "innovate" => C::Innovate,
        "regulatory" => C::Regulatory,
        _ => C::Repair,
    }
}

/// Normalize a rule line for dedup: trim bullet prefix + whitespace collapse.
fn normalize(line: &str) -> String {
    line.trim()
        .trim_start_matches(['-', '*', '•'])
        .trim()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Extract candidate rules from a SOUL.md body: bullet lines inside
/// Evolvable/Observable sections. Immutable (identity) sections are skipped
/// entirely — they are the operator's, not GVU's, and never belonged in the
/// playbook.
pub fn extract_rules(soul: &str) -> Vec<(String, String)> {
    let partitioned = PartitionedSoul::parse(soul);
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for section in &partitioned.sections {
        if section.mutability == SectionMutability::Immutable {
            continue;
        }
        for line in section.content.lines() {
            let t = line.trim();
            if !(t.starts_with('-') || t.starts_with('*')) {
                continue;
            }
            let norm = normalize(t);
            // Too short to be a behaviour rule / pure decoration.
            if norm.chars().count() < 6 {
                continue;
            }
            if seen.insert(norm.to_lowercase()) {
                out.push((section.name.clone(), norm));
            }
        }
    }
    out
}

fn agent_dir(home: &Path, agent: &str) -> PathBuf {
    home.join("agents").join(agent)
}

/// Entry point for `duduclaw playbook migrate-soul`.
pub async fn cmd_migrate_soul(home: &Path, opts: MigrateOptions) -> Result<()> {
    if !duduclaw_core::is_valid_agent_id(&opts.agent) {
        return Err(DuDuClawError::Agent(format!(
            "invalid agent id {:?}: use 1-64 chars of [a-zA-Z0-9_-]",
            opts.agent
        )));
    }
    let dir = agent_dir(home, &opts.agent);
    let draft_path = dir.join(DRAFT_FILE);

    if opts.apply {
        return apply_draft(home, &opts.agent, &draft_path).await;
    }

    let soul_path = dir.join("SOUL.md");
    let soul = std::fs::read_to_string(&soul_path)
        .map_err(|e| DuDuClawError::Agent(format!("read {}: {e}", soul_path.display())))?;
    let rules = extract_rules(&soul);
    if rules.is_empty() {
        println!("查無可遷移規則:SOUL.md 的 Evolvable/Observable 分區沒有條列規則。");
        return Ok(());
    }

    println!("從 {} 抽出 {} 條候選規則:", soul_path.display(), rules.len());
    for (section, text) in &rules {
        println!("  [{section}] {text}");
    }
    if opts.dry_run {
        println!("(--dry-run:未寫入草稿檔)");
        return Ok(());
    }

    let draft = Draft {
        source: format!("SOUL.md of agent `{}` (extracted {})", opts.agent, chrono::Utc::now().to_rfc3339()),
        rule: rules
            .into_iter()
            .map(|(section, text)| DraftRule {
                text,
                section,
                apply: false,
                category: default_category(),
                signals_match: Vec::new(),
                eval_cases: Vec::new(),
                must_use_tools: Vec::new(),
                must_not_use_tools: Vec::new(),
                output_contains: Vec::new(),
                output_not_contains: Vec::new(),
            })
            .collect(),
    };
    let body = toml::to_string_pretty(&draft)
        .map_err(|e| DuDuClawError::Agent(format!("serialize draft: {e}")))?;
    let header = "# WP1.4 SOUL.md → playbook 遷移草稿(人審後執行 --apply)\n\
                  # 每條要入庫的規則:apply 改 true,並填 eval_cases(至少一個 \"<suite>/<case>\")\n\
                  # 與至少一項 E1 斷言(must_use_tools / must_not_use_tools / output_contains / output_not_contains)。\n\
                  # 沒填會在 apply 時被驗證管線拒絕(G6 / WP2.8),屬預期行為。\n\n";
    std::fs::write(&draft_path, format!("{header}{body}"))
        .map_err(|e| DuDuClawError::Agent(format!("write draft: {e}")))?;
    println!("草稿已寫入 {}(全部 apply=false,請人工審閱後執行 --apply)", draft_path.display());
    Ok(())
}

async fn apply_draft(home: &Path, agent: &str, draft_path: &Path) -> Result<()> {
    let content = std::fs::read_to_string(draft_path)
        .map_err(|e| DuDuClawError::Agent(format!("read {}: {e}(先跑一次不帶 --apply 產生草稿)", draft_path.display())))?;
    let draft: Draft = toml::from_str(&content)
        .map_err(|e| DuDuClawError::Agent(format!("parse draft: {e}")))?;

    let selected: Vec<&DraftRule> = draft.rule.iter().filter(|r| r.apply).collect();
    if selected.is_empty() {
        println!("草稿中沒有 apply=true 的規則,未寫入任何條目。");
        return Ok(());
    }

    let deltas: Vec<PlaybookDelta> = selected
        .iter()
        .map(|r| PlaybookDelta::Add {
            content: r.text.clone(),
            category: parse_category(&r.category),
            signals_match: if r.signals_match.is_empty() {
                vec!["*".to_string()]
            } else {
                r.signals_match.clone()
            },
            eval_cases: r.eval_cases.iter().map(|c| EvalCaseRef(c.clone())).collect(),
            assertions: EntryAssertions {
                must_use_tools: r.must_use_tools.clone(),
                must_not_use_tools: r.must_not_use_tools.clone(),
                output_contains: r.output_contains.clone(),
                output_not_contains: r.output_not_contains.clone(),
            },
            strategy: Vec::new(),
            rationale: format!("WP1.4 migrated from SOUL.md section `{}`", r.section),
        })
        .collect();

    let engine = duduclaw_memory::SqliteMemoryEngine::new(&home.join("memory.db"))
        .map_err(|e| DuDuClawError::Memory(format!("open memory.db: {e}")))?;
    let eval_root = duduclaw_gateway::gvu::aee::eval_scorer::resolve_eval_suites_root(home);
    let outcome =
        playbook::store::apply_deltas(&engine, agent, deltas, &[], &eval_root, chrono::Utc::now())
            .await;

    println!("套用完成:accepted {},rejected {}", outcome.applied.len(), outcome.rejected.len());
    for (d, why) in &outcome.rejected {
        if let PlaybookDelta::Add { content, .. } = d {
            println!("  ✗ {}: {why}", duduclaw_core::truncate_chars(content, 40));
        }
    }
    if !outcome.rejected.is_empty() {
        println!("被拒條目請修正草稿(補 eval_cases / 斷言)後重跑 --apply;已入庫的不會重複(dedup)。");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOUL: &str = "# Agent\n\n## 核心價值\n\n- 誠實\n\n## 行為規則\n\n- 回覆前先確認需求與驗收標準\n- 回覆前先確認需求與驗收標準\n- 短\n- 退款請求超過三十天時先查例外清單\n\n## 觀察\n\n- 客戶偏好簡短回覆\n";

    #[test]
    fn extract_skips_identity_dedups_and_drops_short_lines() {
        let rules = extract_rules(SOUL);
        let texts: Vec<&str> = rules.iter().map(|(_, t)| t.as_str()).collect();
        assert!(!texts.iter().any(|t| t.contains("誠實")), "identity section must be skipped: {texts:?}");
        assert_eq!(
            texts.iter().filter(|t| t.contains("確認需求")).count(),
            1,
            "duplicates collapse: {texts:?}"
        );
        assert!(!texts.iter().any(|t| *t == "短"), "too-short lines dropped");
        assert!(texts.iter().any(|t| t.contains("例外清單")));
        assert!(texts.iter().any(|t| t.contains("簡短回覆")), "Observable section included");
    }

    #[test]
    fn draft_round_trips_through_toml() {
        let draft = Draft {
            source: "test".to_string(),
            rule: vec![DraftRule {
                text: "退款請求超過三十天時先查例外清單".to_string(),
                section: "行為規則".to_string(),
                apply: true,
                category: "repair".to_string(),
                signals_match: vec!["kw:退款".to_string()],
                eval_cases: vec!["s/c".to_string()],
                must_use_tools: vec![],
                must_not_use_tools: vec![],
                output_contains: vec!["例外清單".to_string()],
                output_not_contains: vec![],
            }],
        };
        let s = toml::to_string_pretty(&draft).unwrap();
        let back: Draft = toml::from_str(&s).unwrap();
        assert_eq!(back.rule.len(), 1);
        assert!(back.rule[0].apply);
        assert_eq!(back.rule[0].output_contains, vec!["例外清單"]);
    }
}
