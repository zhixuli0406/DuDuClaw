//! paperclip (`paperclipai/paperclip`, TypeScript) importer — consumes any
//! **agentcompanies/v1** package directory (the official
//! `paperclipai company export` output, or a package authored/exported by any
//! other tool in that format), NOT a direct Postgres read.
//!
//! Package shape per the spec (verified 2026-07-11 against
//! `paperclipai/paperclip` `docs/companies/companies-spec.md`, document
//! version `agentcompanies/v1-draft`; frontmatter `schema:` value is
//! `agentcompanies/v1`):
//! - `COMPANY.md` → shared wiki page.
//! - `agents/<slug>/AGENTS.md` — YAML frontmatter
//!   `slug/name/title/reportsTo/skills` + body=instructions → DuDuClaw agent
//!   (`reportsTo` → `reports_to`; body → SOUL.md).
//! - `teams/<slug>/TEAM.md` — organizational subtree with optional `manager`
//!   + `includes`; members without their own `reportsTo` inherit the team
//!   manager as `reports_to` (bridged mapping, reported).
//! - `tasks/<slug>/TASK.md` AND `projects/<slug>/tasks/<slug>/TASK.md` —
//!   frontmatter `name/assignee/project/recurring` → Task Board;
//!   `recurring` → cron_store.
//! - `skills/<slug>/SKILL.md` → agent SKILLS/.
//! - `.paperclip.yaml` vendor sidecar (`schema: paperclip/v1`) —
//!   `routines.<name>.triggers[].{kind: schedule, cronExpression}` → cron
//!   (only when the routine carries a task prompt; nothing is fabricated).
//! - The spec's export rules REQUIRE omitting secrets/DB ids → channels +
//!   keys always SKIPPED.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use duduclaw_core::error::Result;

use super::apply::*;
use super::report::Report;
use super::*;

/// Parsed `AGENTS.md` (frontmatter + instructions body). Pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AgentCard {
    /// Spec common field `slug` — preferred identity when present (keeps
    /// round-trips stable for CJK display names that sanitize to empty).
    pub slug: Option<String>,
    pub name: String,
    pub title: Option<String>,
    pub reports_to: Option<String>,
    pub skills: Vec<String>,
    pub instructions: String,
}

/// Parsed `teams/<slug>/TEAM.md`. Pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct TeamCard {
    pub name: String,
    pub manager: Option<String>,
    /// Raw `includes` entries (paths or slugs; resolved defensively).
    pub includes: Vec<String>,
}

/// A schedule routine from the `.paperclip.yaml` vendor sidecar. Pure.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SidecarRoutine {
    pub name: String,
    pub cron: String,
    /// Task prompt when the routine declares one; `None` → PARTIAL (a cron
    /// without a task would be fabricated, which we never do).
    pub task: Option<String>,
    pub agent: Option<String>,
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
        card.slug = ystr(&fm, "slug");
        card.name = ystr(&fm, "name").unwrap_or_default();
        card.title = ystr(&fm, "title");
        card.reports_to = ystr(&fm, "reportsTo").or_else(|| ystr(&fm, "reports_to"));
        card.skills = yseq(&fm, "skills");
    }
    card
}

/// Parse `teams/<slug>/TEAM.md`. Pure + unit-tested.
pub(super) fn parse_team_card(content: &str) -> TeamCard {
    let (fm, _body) = parse_frontmatter(content);
    let mut card = TeamCard::default();
    if let Some(fm) = fm {
        card.name = ystr(&fm, "name").unwrap_or_default();
        card.manager = ystr(&fm, "manager");
        card.includes = yseq(&fm, "includes");
    }
    card
}

/// Resolve an `includes` entry (path or slug) to its target slug: the last
/// non-`.md` path segment. `agents/alice/AGENTS.md` → `alice`; `alice` →
/// `alice`. Returns `None` for empty/degenerate entries.
pub(super) fn include_target_slug(entry: &str) -> Option<String> {
    let segs: Vec<&str> = entry
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty() && *s != "." && *s != "..")
        .collect();
    let last = segs.last()?;
    let slug = if last.to_ascii_lowercase().ends_with(".md") && segs.len() >= 2 {
        segs[segs.len() - 2]
    } else if last.to_ascii_lowercase().ends_with(".md") {
        return None;
    } else {
        last
    };
    let slug = slug.trim();
    if slug.is_empty() {
        None
    } else {
        Some(slug.to_string())
    }
}

/// Parse schedule routines out of a `.paperclip.yaml` sidecar. Defensive:
/// unknown shapes are simply omitted, never guessed. Deterministic order
/// (sorted by routine name).
pub(super) fn parse_sidecar_routines(yaml: &str) -> Vec<SidecarRoutine> {
    let Ok(root) = serde_yaml::from_str::<serde_yaml::Value>(yaml) else {
        return Vec::new();
    };
    let Some(routines) = root.get("routines").and_then(|v| v.as_mapping()) else {
        return Vec::new();
    };
    let mut out: Vec<SidecarRoutine> = Vec::new();
    for (k, spec) in routines {
        let Some(name) = k.as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
        else {
            continue;
        };
        // First trigger with kind==schedule and a cron expression.
        let cron = spec
            .get("triggers")
            .and_then(|t| t.as_sequence())
            .and_then(|seq| {
                seq.iter().find_map(|t| {
                    let kind = t.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                    if kind != "schedule" {
                        return None;
                    }
                    ["cronExpression", "cron_expression", "cron"]
                        .iter()
                        .find_map(|key| ystr(t, key))
                })
            });
        let Some(cron) = cron else { continue };
        let task = ["prompt", "task", "message", "instruction"]
            .iter()
            .find_map(|key| ystr(spec, key));
        let agent = ystr(spec, "agent").or_else(|| ystr(spec, "assignee"));
        out.push(SidecarRoutine {
            name,
            cron,
            task,
            agent,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
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

/// Map an AGENTS.md `title` onto a canonical DuDuClaw role. Only exact,
/// known role tokens (plus their documented aliases) map; anything else —
/// free-form job titles like "Lead Engineer" — falls back to `specialist`,
/// never a guess. Keeps `duduclaw export` → import round-trips role-stable.
pub(super) fn map_role_title(title: Option<&str>) -> &'static str {
    match title.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        Some("main") => "main",
        Some("worker") => "worker",
        Some("developer") | Some("engineer") => "developer",
        Some("qa") | Some("quality-assurance") | Some("quality") => "qa",
        Some("planner") => "planner",
        Some("team-leader") | Some("tl") | Some("lead") | Some("teamlead") => "team-leader",
        Some("product-manager") | Some("pm") => "product-manager",
        _ => "specialist",
    }
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

    // Fail-closed: an existing directory that carries none of the
    // agentcompanies package markers is malformed input, not an empty run.
    let is_package = src.join("COMPANY.md").exists()
        || src.join("agents").is_dir()
        || src.join("teams").is_dir()
        || src.join("projects").is_dir()
        || src.join("tasks").is_dir()
        || src.join("skills").is_dir();
    if !is_package {
        return Err(duduclaw_core::error::DuDuClawError::Config(format!(
            "'{}' 不是有效的 agentcompanies 套件目錄（缺 COMPANY.md 與 agents/ teams/ projects/ tasks/ skills/ 任一標記）。\
             請指向 `paperclipai company export` 的輸出目錄，或 `duduclaw export --format agentcompanies` 產生的套件。",
            src.display()
        )));
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
            // Identity preference: frontmatter `slug` (spec common field) →
            // directory slug → display name. Keeps round-trips stable even
            // when the display name sanitizes badly (e.g. CJK-only names).
            let node_id = card
                .slug
                .as_deref()
                .map(sanitize_agent_id)
                .filter(|s| !s.is_empty())
                .or_else(|| Some(sanitize_agent_id(&slug)).filter(|s| !s.is_empty()))
                .unwrap_or_else(|| sanitize_agent_id(&card.name));
            raw_to_node.insert(slug.to_lowercase(), node_id.clone());
            if let Some(s) = &card.slug {
                raw_to_node.insert(s.to_lowercase(), node_id.clone());
            }
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

    // ── Teams: members without their own reportsTo inherit the team manager ──
    apply_teams(&src, &mut report, &raw_to_node, &mut agents);

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
            map_role_title(pa.card.title.as_deref()),
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

    // ── `.paperclip.yaml` sidecar routines → cron ──
    import_sidecar_routines(ctx, &mut report, &src, &raw_to_node, &final_ids).await;

    Ok(report)
}

/// Apply `teams/<slug>/TEAM.md` hierarchy: a member listed in `includes`
/// whose own AGENTS.md has no `reportsTo` inherits the team `manager` as its
/// parent. Bridged mapping — the spec expresses hierarchy per-agent via
/// `reportsTo`; TEAM.md only adds organizational grouping, so this is the
/// closest honest projection onto DuDuClaw `reports_to`.
fn apply_teams(
    src: &Path,
    report: &mut Report,
    raw_to_node: &HashMap<String, String>,
    agents: &mut [PaperclipAgent],
) {
    let Ok(rd) = std::fs::read_dir(src.join("teams")) else {
        return;
    };
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
        let content = match std::fs::read_to_string(dir.join("TEAM.md")) {
            Ok(c) => c,
            Err(_) => {
                report.skipped("team", &slug, "找不到 TEAM.md");
                continue;
            }
        };
        let card = parse_team_card(&content);
        let team_name = if card.name.is_empty() {
            slug.clone()
        } else {
            card.name.clone()
        };
        let Some(manager_node) = card
            .manager
            .as_ref()
            .and_then(|m| raw_to_node.get(&m.to_lowercase()).cloned())
        else {
            report.partial(
                "team",
                &team_name,
                "無 manager 或 manager 不在套件內，僅記錄不映射",
            );
            continue;
        };
        let mut bridged = 0usize;
        for entry in &card.includes {
            let Some(member_slug) = include_target_slug(entry) else {
                continue;
            };
            let Some(member_node) = raw_to_node.get(&member_slug.to_lowercase()).cloned() else {
                continue;
            };
            if member_node == manager_node {
                continue;
            }
            if let Some(member) = agents.iter_mut().find(|a| a.node_id == member_node)
                && member.card.reports_to.is_none()
                && member.reports_to_node.is_none()
            {
                member.reports_to_node = Some(manager_node.clone());
                bridged += 1;
            }
        }
        if bridged > 0 {
            report.partial(
                "team",
                &team_name,
                format!("{bridged} 位成員無自身 reportsTo，已橋接為 manager '{manager_node}'"),
            );
        } else {
            report.imported("team", &team_name);
        }
    }
}

/// `.paperclip.yaml` routines with a schedule trigger → cron_store. A routine
/// without a task prompt is PARTIAL (we never fabricate a task body).
async fn import_sidecar_routines(
    ctx: &Ctx,
    report: &mut Report,
    src: &Path,
    raw_to_node: &HashMap<String, String>,
    final_ids: &HashMap<String, String>,
) {
    let Ok(content) = std::fs::read_to_string(src.join(".paperclip.yaml")) else {
        return;
    };
    let routines = parse_sidecar_routines(&content);
    for r in routines {
        let Some(task) = r.task.clone() else {
            report.partial(
                "cron",
                &r.name,
                "sidecar routine 無任務內容（prompt/task），無法建立 cron",
            );
            continue;
        };
        let agent = r
            .agent
            .as_ref()
            .and_then(|a| raw_to_node.get(&a.to_lowercase()))
            .and_then(|node| final_ids.get(node))
            .cloned()
            .unwrap_or_else(|| "main".to_string());
        let job = CronJob {
            name: r.name.clone(),
            cron: r.cron.clone(),
            task,
        };
        import_cron_jobs(ctx, report, &agent, std::slice::from_ref(&job)).await;
    }
}

async fn import_tasks(
    ctx: &Ctx,
    report: &mut Report,
    src: &Path,
    raw_to_node: &HashMap<String, String>,
    final_ids: &HashMap<String, String>,
) {
    // Task dirs live at `tasks/<slug>/` and — per the spec's implicit task
    // discovery — nested under `projects/<slug>/tasks/<slug>/`.
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(src.join("tasks")) {
        dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
    }
    if let Ok(projects) = std::fs::read_dir(src.join("projects")) {
        let mut proj_dirs: Vec<PathBuf> = projects
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        proj_dirs.sort();
        for proj in proj_dirs {
            if let Ok(rd) = std::fs::read_dir(proj.join("tasks")) {
                dirs.extend(rd.flatten().map(|e| e.path()).filter(|p| p.is_dir()));
            }
        }
    }
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

    #[test]
    fn parse_agent_card_prefers_spec_slug() {
        let doc = "---\nschema: agentcompanies/v1\nkind: agent\nslug: xiao-mei\nname: 小美\n---\nbody\n";
        let card = parse_agent_card(doc);
        assert_eq!(card.slug.as_deref(), Some("xiao-mei"));
        assert_eq!(card.name, "小美");
    }

    #[test]
    fn map_role_title_exact_tokens_only() {
        assert_eq!(map_role_title(Some("main")), "main");
        assert_eq!(map_role_title(Some("Engineer")), "developer");
        assert_eq!(map_role_title(Some("pm")), "product-manager");
        assert_eq!(map_role_title(Some("tl")), "team-leader");
        // Free-form titles never guess a role.
        assert_eq!(map_role_title(Some("Lead Engineer")), "specialist");
        assert_eq!(map_role_title(None), "specialist");
    }

    #[test]
    fn parse_team_card_manager_and_includes() {
        let doc = "---\nname: Core Team\nmanager: boss\nincludes:\n  - agents/alice/AGENTS.md\n  - bob\n---\nTeam body.\n";
        let card = parse_team_card(doc);
        assert_eq!(card.name, "Core Team");
        assert_eq!(card.manager.as_deref(), Some("boss"));
        assert_eq!(card.includes, vec!["agents/alice/AGENTS.md", "bob"]);
    }

    #[test]
    fn include_target_slug_resolves_paths_and_slugs() {
        assert_eq!(
            include_target_slug("agents/alice/AGENTS.md").as_deref(),
            Some("alice")
        );
        assert_eq!(include_target_slug("bob").as_deref(), Some("bob"));
        assert_eq!(
            include_target_slug("../teams/qa/TEAM.md").as_deref(),
            Some("qa")
        );
        assert_eq!(include_target_slug(""), None);
        assert_eq!(include_target_slug("AGENTS.md"), None);
    }

    #[test]
    fn sidecar_routines_parse_schedule_triggers_only() {
        let yaml = r#"
schema: paperclip/v1
agents:
  boss:
    adapter:
      type: duduclaw
routines:
  monday-review:
    prompt: Review the sprint board
    agent: boss
    triggers:
      - kind: schedule
        cronExpression: "0 9 * * 1"
  no-task:
    triggers:
      - kind: schedule
        cronExpression: "0 8 * * *"
  webhook-only:
    prompt: not a schedule
    triggers:
      - kind: webhook
"#;
        let routines = parse_sidecar_routines(yaml);
        assert_eq!(routines.len(), 2, "webhook-only must be omitted");
        assert_eq!(routines[0].name, "monday-review");
        assert_eq!(routines[0].cron, "0 9 * * 1");
        assert_eq!(routines[0].task.as_deref(), Some("Review the sprint board"));
        assert_eq!(routines[0].agent.as_deref(), Some("boss"));
        assert_eq!(routines[1].name, "no-task");
        assert!(routines[1].task.is_none(), "no prompt → task None (PARTIAL)");
        // Malformed YAML → empty, never a panic.
        assert!(parse_sidecar_routines(":::not yaml:::").is_empty());
    }
}
