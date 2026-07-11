//! paperclip (`paperclipai/paperclip`, TypeScript) importer — via the official
//! `paperclipai company export` directory, NOT a direct Postgres read.
//!
//! Export shape per spec §1.3 (agentcompanies/v1-draft):
//! - `COMPANY.md` → shared wiki page.
//! - `agents/<slug>/AGENTS.md` — YAML frontmatter `name/title/reportsTo/skills`
//!   + body=instructions → DuDuClaw agent (`reportsTo` → `reports_to`; body → SOUL.md).
//! - `tasks/<slug>/TASK.md` — frontmatter `name/assignee/project/recurring`
//!   → Task Board; `recurring` → cron_store.
//! - `skills/<slug>/SKILL.md` → agent SKILLS/.
//! - The official export contains NO secrets/DB ids → channels + keys SKIPPED.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use duduclaw_core::error::Result;

use super::apply::*;
use super::report::Report;
use super::*;

/// Parsed `AGENTS.md` (frontmatter + instructions body). Pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AgentCard {
    pub name: String,
    pub title: Option<String>,
    pub reports_to: Option<String>,
    pub skills: Vec<String>,
    pub instructions: String,
}

/// Parsed `TASK.md`. Pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct TaskCard {
    pub name: String,
    pub assignee: Option<String>,
    pub project: Option<String>,
    /// `Some(expr)` = cron expression; `Some("")` = recurring flag with no
    /// schedule (cannot build a cron); `None` = one-off task.
    pub recurring: Option<String>,
    pub description: String,
}

fn ystr(v: &serde_yaml::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(|x| x.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn yseq(v: &serde_yaml::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(|x| x.as_sequence())
        .map(|seq| {
            seq.iter()
                .filter_map(|i| i.as_str().map(|s| s.trim().to_string()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Parse `agents/<slug>/AGENTS.md`. Pure + unit-tested.
pub(super) fn parse_agent_card(content: &str) -> AgentCard {
    let (fm, body) = parse_frontmatter(content);
    let mut card = AgentCard {
        instructions: body,
        ..Default::default()
    };
    if let Some(fm) = fm {
        card.name = ystr(&fm, "name").unwrap_or_default();
        card.title = ystr(&fm, "title");
        card.reports_to = ystr(&fm, "reportsTo").or_else(|| ystr(&fm, "reports_to"));
        card.skills = yseq(&fm, "skills");
    }
    card
}

/// Parse `tasks/<slug>/TASK.md`. Pure + unit-tested.
pub(super) fn parse_task_card(content: &str) -> TaskCard {
    let (fm, body) = parse_frontmatter(content);
    let mut card = TaskCard {
        description: body,
        ..Default::default()
    };
    if let Some(fm) = fm {
        card.name = ystr(&fm, "name").unwrap_or_default();
        card.assignee = ystr(&fm, "assignee");
        card.project = ystr(&fm, "project");
        card.recurring = match fm.get("recurring") {
            Some(serde_yaml::Value::String(s)) if !s.trim().is_empty() => {
                Some(s.trim().to_string())
            }
            Some(serde_yaml::Value::Bool(true)) => Some(String::new()),
            _ => None,
        };
    }
    card
}

/// Teaching message printed when `--source` is omitted (paperclip cannot be
/// auto-discovered — data lives in an embedded Postgres, exported explicitly).
pub(super) fn print_export_help() {
    use console::style;
    println!();
    println!(
        "  {} paperclip 轉移需要官方匯出目錄（不直連資料庫）。",
        style("🐾").cyan()
    );
    println!();
    println!("  請先在 paperclip 端執行：");
    println!(
        "    {}",
        style("paperclipai company export <company-id> --out ./export \\\n      --include company,agents,projects,issues,tasks,skills")
            .bold()
    );
    println!();
    println!("  再執行：");
    println!(
        "    {}",
        style("duduclaw migrate-from paperclip --source ./export [--apply]").bold()
    );
    println!();
}

struct PaperclipAgent {
    card: AgentCard,
    node_id: String,
    reports_to_node: Option<String>,
}

pub(super) async fn migrate(ctx: &Ctx, source: Option<PathBuf>) -> Result<Report> {
    // `run()` guarantees source is Some for paperclip.
    let src = source.expect("paperclip source is required (guaranteed by run())");
    let mut report = Report::new("paperclip", &src.display().to_string(), ctx.apply);

    if !src.exists() {
        report.skipped("export", &src.display().to_string(), "匯出目錄不存在");
        return Ok(report);
    }

    // Secrets are out of scope by the official export format.
    report
        .note("paperclip 官方匯出格式不含機密（channel token / API key / DB id），故一律未匯入。");

    // ── COMPANY.md → shared wiki ──
    let company = src.join("COMPANY.md");
    if company.exists() {
        let dest = ctx
            .home
            .join("shared")
            .join("wiki")
            .join("imported")
            .join("paperclip-company.md");
        if ctx.apply {
            let ok = std::fs::create_dir_all(dest.parent().unwrap())
                .and_then(|_| std::fs::copy(&company, &dest).map(|_| ()));
            match ok {
                Ok(()) => report.imported("wiki", "paperclip-company.md"),
                Err(e) => report.skipped("wiki", "COMPANY.md", format!("複製失敗: {e}")),
            }
        } else {
            report.imported("wiki", "paperclip-company.md");
        }
    } else {
        report.skipped("wiki", "COMPANY.md", "匯出目錄內無 COMPANY.md");
    }

    // ── Agents: parse cards → topo sort by reportsTo → create ──
    let mut agents: Vec<PaperclipAgent> = Vec::new();
    let mut raw_to_node: HashMap<String, String> = HashMap::new();
    if let Ok(rd) = std::fs::read_dir(src.join("agents")) {
        let mut dirs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let slug = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let content = match std::fs::read_to_string(dir.join("AGENTS.md")) {
                Ok(c) => c,
                Err(_) => {
                    report.skipped("agent", &slug, "找不到 AGENTS.md");
                    continue;
                }
            };
            let card = parse_agent_card(&content);
            let display = if card.name.is_empty() {
                slug.clone()
            } else {
                card.name.clone()
            };
            let node_id = sanitize_agent_id(&display);
            raw_to_node.insert(slug.to_lowercase(), node_id.clone());
            if !card.name.is_empty() {
                raw_to_node.insert(card.name.to_lowercase(), node_id.clone());
            }
            agents.push(PaperclipAgent {
                card,
                node_id,
                reports_to_node: None,
            });
        }
    }

    // Resolve reportsTo → node id space now that all agents are known.
    for a in &mut agents {
        a.reports_to_node = a
            .card
            .reports_to
            .as_ref()
            .and_then(|r| raw_to_node.get(&r.to_lowercase()).cloned());
    }

    let nodes: Vec<(String, Option<String>)> = agents
        .iter()
        .map(|a| (a.node_id.clone(), a.reports_to_node.clone()))
        .collect();
    let (order, cycle) = match topo_sort_agents(&nodes) {
        TopoOutcome::Sorted(o) => (o, false),
        TopoOutcome::Cycle(stuck) => {
            report.partial(
                "agent",
                "reports_to",
                format!("偵測到 reports_to 環 ({})，全部改為無上級", stuck.join(",")),
            );
            (agents.iter().map(|a| a.node_id.clone()).collect(), true)
        }
    };

    let by_node: HashMap<String, &PaperclipAgent> =
        agents.iter().map(|a| (a.node_id.clone(), a)).collect();
    let mut final_ids: HashMap<String, String> = HashMap::new();
    let engine = open_memory(ctx);
    let _ = &engine; // reserved for future paperclip memory import

    for node_id in &order {
        let Some(pa) = by_node.get(node_id) else {
            continue;
        };
        let parent_final = if cycle {
            String::new()
        } else {
            pa.reports_to_node
                .as_ref()
                .and_then(|pid| final_ids.get(pid).cloned())
                .unwrap_or_default()
        };
        let display = if pa.card.name.is_empty() {
            node_id.clone()
        } else {
            pa.card.name.clone()
        };
        let soul_body = if pa.card.instructions.trim().is_empty() {
            None
        } else {
            Some(pa.card.instructions.clone())
        };

        if let Some(fid) = scaffold_agent(
            ctx,
            &mut report,
            node_id,
            &display,
            "specialist",
            &parent_final,
            None,
            soul_body,
        )
        .await
        {
            final_ids.insert(node_id.clone(), fid.clone());
            // Per-agent skills from frontmatter `skills` list.
            let skill_dirs: Vec<PathBuf> = pa
                .card
                .skills
                .iter()
                .map(|s| src.join("skills").join(s))
                .filter(|d| d.join("SKILL.md").exists())
                .collect();
            if !skill_dirs.is_empty() {
                install_skills(ctx, &mut report, &fid, &skill_dirs);
                report.note(
                    "已安裝的 skills 皆通過 duduclaw-security 注入掃描 (input_guard, 6 規則)。",
                );
            }
        }
    }

    // ── Tasks → Task Board (+ recurring → cron) ──
    import_tasks(ctx, &mut report, &src, &raw_to_node, &final_ids).await;

    Ok(report)
}

async fn import_tasks(
    ctx: &Ctx,
    report: &mut Report,
    src: &Path,
    raw_to_node: &HashMap<String, String>,
    final_ids: &HashMap<String, String>,
) {
    let Ok(rd) = std::fs::read_dir(src.join("tasks")) else {
        return;
    };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    if dirs.is_empty() {
        return;
    }

    // Open the task store once (apply mode only).
    let store = if ctx.apply {
        match duduclaw_gateway::task_store::TaskStore::open(&ctx.home) {
            Ok(s) => Some(s),
            Err(e) => {
                report.skipped("task", "task_store", format!("開啟失敗: {e}"));
                None
            }
        }
    } else {
        None
    };

    for dir in dirs {
        let slug = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let content = match std::fs::read_to_string(dir.join("TASK.md")) {
            Ok(c) => c,
            Err(_) => {
                report.skipped("task", &slug, "找不到 TASK.md");
                continue;
            }
        };
        let card = parse_task_card(&content);
        let title = if card.name.is_empty() {
            slug.clone()
        } else {
            card.name.clone()
        };

        // Resolve assignee slug/name → final agent id (unassigned if unknown).
        let assignee = card
            .assignee
            .as_ref()
            .and_then(|a| raw_to_node.get(&a.to_lowercase()))
            .and_then(|node| final_ids.get(node))
            .cloned()
            .unwrap_or_default();

        if ctx.apply {
            if let Some(store) = &store {
                let row = duduclaw_gateway::task_store::TaskRow::new(
                    uuid::Uuid::new_v4().to_string(),
                    title.clone(),
                    card.description.clone(),
                    "medium".to_string(),
                    assignee.clone(),
                    "migrate-from-paperclip".to_string(),
                );
                match store.insert_task(&row).await {
                    Ok(()) => report.imported("task", &title),
                    Err(e) => report.skipped("task", &title, format!("寫入失敗: {e}")),
                }
            }
        } else {
            report.imported("task", &title);
        }

        // Recurring → cron_store.
        match &card.recurring {
            Some(expr) if !expr.is_empty() => {
                let agent = if assignee.is_empty() {
                    "main".to_string()
                } else {
                    assignee.clone()
                };
                let job = CronJob {
                    name: title.clone(),
                    cron: expr.clone(),
                    task: title.clone(),
                };
                import_cron_jobs(ctx, report, &agent, std::slice::from_ref(&job)).await;
            }
            Some(_) => {
                report.partial(
                    "cron",
                    &title,
                    "recurring=true 但無排程表達式，無法建立 cron",
                );
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_agent_card_full() {
        let doc = "---\nname: Alice\ntitle: Lead Engineer\nreportsTo: bob\nskills:\n  - code-review\n  - deploy\n---\nYou are Alice, the lead engineer.\nAlways be precise.\n";
        let card = parse_agent_card(doc);
        assert_eq!(card.name, "Alice");
        assert_eq!(card.title.as_deref(), Some("Lead Engineer"));
        assert_eq!(card.reports_to.as_deref(), Some("bob"));
        assert_eq!(card.skills, vec!["code-review", "deploy"]);
        assert!(card.instructions.contains("You are Alice"));
    }

    #[test]
    fn parse_agent_card_no_frontmatter() {
        let card = parse_agent_card("Just instructions, no frontmatter.");
        assert_eq!(card.name, "");
        assert!(card.reports_to.is_none());
        assert_eq!(card.instructions, "Just instructions, no frontmatter.");
    }

    #[test]
    fn parse_task_card_recurring_string() {
        let doc = "---\nname: Daily standup\nassignee: alice\nproject: ops\nrecurring: \"0 9 * * *\"\n---\nPost the standup summary.\n";
        let card = parse_task_card(doc);
        assert_eq!(card.name, "Daily standup");
        assert_eq!(card.assignee.as_deref(), Some("alice"));
        assert_eq!(card.project.as_deref(), Some("ops"));
        assert_eq!(card.recurring.as_deref(), Some("0 9 * * *"));
        assert!(card.description.contains("standup summary"));
    }

    #[test]
    fn parse_task_card_recurring_bool_no_schedule() {
        let doc = "---\nname: T\nrecurring: true\n---\nbody\n";
        let card = parse_task_card(doc);
        assert_eq!(card.recurring.as_deref(), Some("")); // recurring flag, no cron expr
    }

    #[test]
    fn parse_task_card_one_off() {
        let doc = "---\nname: T\nassignee: bob\n---\nbody\n";
        let card = parse_task_card(doc);
        assert!(card.recurring.is_none());
        assert_eq!(card.assignee.as_deref(), Some("bob"));
    }
}
