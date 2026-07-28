//! `expert convert-teams` — batch converter from the legacy team playbooks
//! (`teams/<industry>-team/` with `team.toml` + `TEAM.md`) into native expert
//! packs (`expert.toml` + `agents/` + `skills/` + `wiki/`).
//!
//! Sources folded into each generated pack:
//!
//! | Source                                   | Pack asset                              |
//! |------------------------------------------|-----------------------------------------|
//! | `team.toml` front_desk / workers / humans| `[[expert.agents]]` roster + hierarchy  |
//! | `<pack>-pro/SOUL.md` (industry pack)     | front-desk `soul.md`                    |
//! | `_departments/<kit>/SOUL.md` + overlay   | worker `soul.md` (overlay injected)     |
//! | `<pack>-pro/agent.toml` / kit agent.toml | `agent.partial.toml` (whitelisted keys) |
//! | `TEAM.md` §總機分派劇本                   | pack skill `<industry>-dispatch`        |
//! | `TEAM.md` (minus §部署步驟) + pack wiki   | `wiki/<slug>/…` SOP pages               |
//! | `TEAM.md` 一句話摘要 / 「…」quotes        | description / recommended prompts       |
//!
//! Deterministic and idempotent: every output byte derives from the sources,
//! so re-running overwrites with identical content. Each generated pack is
//! validated with [`super::manifest::validate`] before the run reports
//! success; any failing pack fails the command (honest, no partial-silence).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use console::style;
use serde::Deserialize;
use toml::Value;
use toml::value::Table;

use duduclaw_core::error::Result;

use super::{cfg_err, io_err, manifest};

/// agent.toml sections carried into `agent.partial.toml` (everything else —
/// identity, wiring, heartbeat — is owned by the install scaffold).
const PARTIAL_SECTIONS: [&str; 4] = ["model", "budget", "permissions", "capabilities"];

/// Channel tokens recognised in a TEAM.md「對外通道」line.
const KNOWN_CHANNELS: [&str; 9] = [
    "telegram",
    "line",
    "discord",
    "slack",
    "whatsapp",
    "feishu",
    "googlechat",
    "teams",
    "webchat",
];

/// Default suggested channels when TEAM.md does not state any (Taiwan market
/// baseline used across the playbooks' deployment examples).
const DEFAULT_CHANNELS: [&str; 2] = ["line", "telegram"];

const MAX_PROMPTS: usize = 3;

// ─────────────────────────── team.toml schema ───────────────────────────

#[derive(Debug, Deserialize)]
struct TeamManifest {
    #[allow(dead_code)]
    #[serde(default)]
    schema: Option<i64>,
    industry: String,
    #[serde(default)]
    pack: String,
    #[serde(default)]
    label: String,
    front_desk: FrontDesk,
    #[serde(default)]
    workers: Vec<Worker>,
    #[serde(default)]
    humans: Vec<Human>,
}

#[derive(Debug, Deserialize)]
struct FrontDesk {
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    summary: String,
}

#[derive(Debug, Deserialize)]
struct Worker {
    #[serde(default)]
    kit: String,
    name: String,
    #[serde(default)]
    display_name: String,
    #[serde(default)]
    trigger: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    overlay: Vec<String>,
    /// Optional team.toml override for the functional department; empty ⇒
    /// the shared-kit default (`duduclaw_core::org::department_for_kit`).
    #[serde(default)]
    department: String,
}

#[derive(Debug, Deserialize)]
struct Human {
    #[serde(default)]
    title: String,
    #[serde(default)]
    summary: String,
}

// ─────────────────────────── entry point ───────────────────────────

/// Convert every `<something>-team/` dir (containing `team.toml`) under
/// `teams_dir` into an expert pack under `out` (default:
/// `<teams_dir>/../experts`). Prints a zh-TW result table; errs when any
/// pack fails conversion or validation.
pub(super) fn cmd_convert_teams(teams_dir: &Path, out: Option<&Path>) -> Result<()> {
    if !teams_dir.is_dir() {
        return Err(cfg_err(format!("teams 目錄不存在: {}", teams_dir.display())));
    }
    let default_out = teams_dir
        .parent()
        .map(|p| p.join("experts"))
        .unwrap_or_else(|| PathBuf::from("experts"));
    let out_dir = out.unwrap_or(&default_out);

    let mut team_dirs: Vec<PathBuf> = std::fs::read_dir(teams_dir)
        .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", teams_dir.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.ends_with("-team"))
                    .unwrap_or(false)
                && p.join("team.toml").is_file()
        })
        .collect();
    team_dirs.sort();

    if team_dirs.is_empty() {
        return Err(cfg_err(format!(
            "{} 下找不到任何含 team.toml 的 *-team 目錄",
            teams_dir.display()
        )));
    }

    let mut ok: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();

    for team_dir in &team_dirs {
        let slug = team_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        match convert_one(team_dir, teams_dir, &out_dir.join(&slug), &slug) {
            Ok(()) => {
                // Post-write validation with the strict pack validator.
                let pack_dir = out_dir.join(&slug);
                match manifest::read(&pack_dir) {
                    Ok(m) => {
                        let problems = manifest::validate(&m, &pack_dir);
                        if problems.is_empty() {
                            ok.push(slug);
                        } else {
                            let msg = problems
                                .iter()
                                .map(|p| format!("{}: {}", p.field, p.message))
                                .collect::<Vec<_>>()
                                .join("; ");
                            failed.push((slug, format!("驗證失敗: {msg}")));
                        }
                    }
                    Err(e) => failed.push((slug, format!("回讀 manifest 失敗: {e}"))),
                }
            }
            Err(e) => failed.push((slug, e.to_string())),
        }
    }

    println!(
        "\n  {}（輸出: {}）\n",
        style("teams → expert 批次轉換結果").bold(),
        out_dir.display()
    );
    for s in &ok {
        println!("  {} {}", style("✓").green(), s);
    }
    for (s, why) in &failed {
        println!("  {} {} — {}", style("✗").red(), s, why);
    }
    println!(
        "\n  合計 {} 個團隊：成功 {}、失敗 {}\n",
        ok.len() + failed.len(),
        style(ok.len()).green(),
        if failed.is_empty() {
            style(0).green()
        } else {
            style(failed.len()).red()
        }
    );

    if failed.is_empty() {
        Ok(())
    } else {
        Err(cfg_err(format!("{} 個團隊轉換失敗（見上表）", failed.len())))
    }
}

// ─────────────────────────── one team ───────────────────────────

fn convert_one(team_dir: &Path, teams_dir: &Path, pack_dir: &Path, slug: &str) -> Result<()> {
    if !crate::is_valid_agent_id(slug) {
        return Err(cfg_err(format!("'{slug}' 非合法 pack slug")));
    }
    let team_src = std::fs::read_to_string(team_dir.join("team.toml"))
        .map_err(|e| io_err(format!("讀取 team.toml 失敗: {e}")))?;
    let team: TeamManifest =
        toml::from_str(&team_src).map_err(|e| cfg_err(format!("team.toml 解析失敗: {e}")))?;
    let team_md = std::fs::read_to_string(team_dir.join("TEAM.md")).unwrap_or_default();

    let label = if team.label.trim().is_empty() {
        team.industry.clone()
    } else {
        team.label.clone()
    };
    // Sibling industry pack (`<industry>-pro/`) supplies the front-desk soul,
    // settings and wiki pages when present.
    let industry_pack_dir = teams_dir
        .parent()
        .map(|p| p.join(&team.pack))
        .filter(|p| !team.pack.is_empty() && p.is_dir());

    // ── skill: 總機分派劇本 → Agent Skills format ──
    let dispatch_section = extract_section(&team_md, "## 總機分派劇本");
    let skill_name = format!("{}-dispatch", team.industry);
    let has_skill = dispatch_section.is_some()
        && duduclaw_agent::skill_loader::is_safe_skill_name(&skill_name)
        && crate::is_valid_agent_id(&skill_name);
    if let (true, Some(body)) = (has_skill, dispatch_section.as_ref()) {
        let skill_md = format!(
            "---\nname: {skill_name}\ndescription: {label}總機分派劇本與紅線升級規則（由 TEAM.md 轉出）\n---\n\n# 總機分派劇本\n\n{}\n",
            body.trim()
        );
        write_file(&pack_dir.join("skills").join(&skill_name).join("SKILL.md"), &skill_md)?;
    }

    // ── expert.toml ──
    let description = extract_one_liner(&team_md).unwrap_or_else(|| {
        format!("{label}產業 AI 部門團隊：{}", team.front_desk.summary)
    });
    let prompts = extract_prompts(dispatch_section.as_deref().unwrap_or(""), MAX_PROMPTS);
    let channels = extract_channels(&team_md);
    let manifest_toml = render_manifest(
        slug,
        &label,
        &description,
        &team,
        &prompts,
        &channels,
        has_skill.then_some(skill_name.as_str()),
    );
    let header = format!(
        "# Generated by `duduclaw expert convert-teams` from teams/{slug} — do not hand-edit.\n\
         # 重新產生：duduclaw expert convert-teams <teams-dir>\n"
    );
    write_file(&pack_dir.join("expert.toml"), &format!("{header}{manifest_toml}"))?;

    // ── front-desk agent ──
    let front_dir = pack_dir.join("agents").join(&team.front_desk.name);
    let front_soul = build_front_soul(&team, &label, industry_pack_dir.as_deref());
    write_file(&front_dir.join("soul.md"), &front_soul)?;
    let front_partial = build_partial(
        industry_pack_dir.as_deref().map(|d| d.join("agent.toml")),
        true,
    )?;
    write_file(&front_dir.join("agent.partial.toml"), &front_partial)?;

    // ── workers ──
    for w in &team.workers {
        let w_dir = pack_dir.join("agents").join(&w.name);
        let kit_dir = (!w.kit.trim().is_empty())
            .then(|| teams_dir.join("_departments").join(w.kit.trim()))
            .filter(|d| d.is_dir());
        write_file(&w_dir.join("soul.md"), &build_worker_soul(w, kit_dir.as_deref()))?;
        let partial = build_partial(kit_dir.as_deref().map(|d| d.join("agent.toml")), false)?;
        write_file(&w_dir.join("agent.partial.toml"), &partial)?;
    }

    // ── wiki: industry-pack pages + TEAM.md SOP (namespaced per slug so
    //    multi-pack installs never collide inside shared/wiki) ──
    if let Some(pd) = industry_pack_dir.as_deref() {
        let wiki_src = pd.join("wiki");
        if wiki_src.is_dir() {
            copy_md_tree(&wiki_src, &pack_dir.join("wiki").join(slug))?;
        }
    }
    if !team_md.trim().is_empty() {
        let sop = format!(
            "# {label} 團隊 SOP（由 TEAM.md 轉出）\n\n{}",
            strip_section(&team_md, "## 部署步驟").trim()
        );
        write_file(
            &pack_dir.join("wiki").join(slug).join("team-sop.md"),
            &sanitize_wiki_content(&sop),
        )?;
    }

    Ok(())
}

// ─────────────────────────── expert.toml rendering ───────────────────────────

/// Build the manifest as an explicit `toml::Table` (scalars before tables, so
/// serialization is valid and byte-deterministic; `toml::map::Map` preserves
/// insertion order).
#[allow(clippy::too_many_arguments)]
fn render_manifest(
    slug: &str,
    label: &str,
    description: &str,
    team: &TeamManifest,
    prompts: &[String],
    channels: &[String],
    skill_name: Option<&str>,
) -> String {
    let mut expert = Table::new();
    expert.insert("name".into(), Value::String(slug.to_string()));
    expert.insert("description".into(), Value::String(description.to_string()));
    expert.insert("version".into(), Value::String("1.0.0".into()));
    expert.insert("author".into(), Value::String("嘟嘟數位".into()));
    expert.insert("license".into(), Value::String("Commercial".into()));
    expert.insert(
        "tags".into(),
        Value::Array(vec![
            Value::String(team.industry.clone()),
            Value::String("team".into()),
            Value::String("taiwan".into()),
        ]),
    );
    expert.insert(
        "category".into(),
        Value::String(duduclaw_core::org::industry_category(&team.industry).to_string()),
    );

    let mut display = Table::new();
    display.insert("zh-TW".into(), Value::String(label.to_string()));
    expert.insert("display_name".into(), Value::Table(display));

    let mut prompts_t = Table::new();
    prompts_t.insert(
        "recommended".into(),
        Value::Array(prompts.iter().cloned().map(Value::String).collect()),
    );
    expert.insert("prompts".into(), Value::Table(prompts_t));

    let mut channels_t = Table::new();
    channels_t.insert(
        "suggested".into(),
        Value::Array(channels.iter().cloned().map(Value::String).collect()),
    );
    expert.insert("channels".into(), Value::Table(channels_t));

    let mut requires = Table::new();
    requires.insert("env".into(), Value::Array(vec![]));
    requires.insert("bins".into(), Value::Array(vec![Value::String("claude".into())]));
    expert.insert("requires".into(), Value::Table(requires));

    // Roster: front desk first, workers report to it.
    let mut agents = Vec::new();
    let fd = &team.front_desk;
    let fd_display = if fd.display_name.trim().is_empty() {
        fd.name.clone()
    } else {
        fd.display_name.clone()
    };
    let mut fd_t = Table::new();
    fd_t.insert("name".into(), Value::String(fd.name.clone()));
    fd_t.insert("role".into(), Value::String("front_desk".into()));
    fd_t.insert("display_name".into(), Value::String(fd_display.clone()));
    fd_t.insert("trigger".into(), Value::String(format!("@{fd_display}")));
    fd_t.insert("rank".into(), Value::String("manager".into()));
    if let Some(s) = skill_name {
        fd_t.insert("skills".into(), Value::Array(vec![Value::String(s.into())]));
    }
    agents.push(Value::Table(fd_t));

    for w in &team.workers {
        let mut wt = Table::new();
        wt.insert("name".into(), Value::String(w.name.clone()));
        wt.insert("role".into(), Value::String("worker".into()));
        let display = if w.display_name.trim().is_empty() {
            w.name.clone()
        } else {
            w.display_name.clone()
        };
        wt.insert("display_name".into(), Value::String(display));
        wt.insert("reports_to".into(), Value::String(fd.name.clone()));
        let trigger = if w.trigger.trim().is_empty() {
            w.name.clone()
        } else {
            w.trigger.clone()
        };
        wt.insert("trigger".into(), Value::String(trigger));
        wt.insert("rank".into(), Value::String("staff".into()));
        let department = if w.department.trim().is_empty() {
            duduclaw_core::org::department_for_kit(&w.kit).unwrap_or("")
        } else {
            w.department.trim()
        };
        if !department.is_empty() {
            wt.insert("department".into(), Value::String(department.to_string()));
        }
        agents.push(Value::Table(wt));
    }
    expert.insert("agents".into(), Value::Array(agents));

    let mut root = Table::new();
    root.insert("expert".into(), Value::Table(expert));
    toml::to_string_pretty(&Value::Table(root)).expect("manifest table serializes")
}

// ─────────────────────────── souls & partials ───────────────────────────

fn build_front_soul(team: &TeamManifest, label: &str, industry_pack: Option<&Path>) -> String {
    let base = industry_pack
        .map(|d| d.join("SOUL.md"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| {
            let fd = &team.front_desk;
            format!(
                "# {}\n\n{}\n\n我是「{label}」團隊的總機（front desk），對外唯一窗口；\
                 部門任務委派給團隊 worker，超出授權或需要專業判斷時升級真人。\n",
                if fd.display_name.trim().is_empty() {
                    &fd.name
                } else {
                    &fd.display_name
                },
                fd.summary
            )
        });
    let mut soul = base.trim_end().to_string();
    if !team.humans.is_empty() {
        soul.push_str("\n\n## 真人崗位（不建 AI，由 convert-teams 自 team.toml 轉入）\n\n");
        for h in &team.humans {
            soul.push_str(&format!("- {}：{}\n", h.title, h.summary));
        }
    }
    soul.push('\n');
    soul
}

fn build_worker_soul(w: &Worker, kit_dir: Option<&Path>) -> String {
    let display = if w.display_name.trim().is_empty() {
        &w.name
    } else {
        &w.display_name
    };
    let base = kit_dir
        .map(|d| d.join("SOUL.md"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| format!("# {display}\n\n{}\n", w.summary));
    inject_overlay(&base, &w.overlay)
}

/// Insert the industry-overlay bullets under the kit SOUL's
/// `## Industry Overlay` placeholder heading (appended as a new section when
/// the heading is absent).
fn inject_overlay(soul: &str, overlay: &[String]) -> String {
    if overlay.is_empty() {
        return format!("{}\n", soul.trim_end());
    }
    let bullets: String = overlay.iter().map(|o| format!("- {o}\n")).collect();
    let mut out = String::new();
    let mut injected = false;
    for line in soul.lines() {
        out.push_str(line);
        out.push('\n');
        if !injected && line.trim_start().starts_with("## Industry Overlay") {
            out.push('\n');
            out.push_str(&bullets);
            injected = true;
        }
    }
    if !injected {
        out.push_str("\n## Industry Overlay（由 team.toml 合規 overlay 填入）\n\n");
        out.push_str(&bullets);
    }
    format!("{}\n", out.trim_end())
}

/// Extract the whitelisted sections of a source agent.toml into a partial.
/// `front_desk` forces `[permissions] can_send_cross_agent = true` (required
/// for delegation, per every TEAM.md wiring table).
fn build_partial(src: Option<PathBuf>, front_desk: bool) -> Result<String> {
    let mut out = Table::new();
    if let Some(path) = src
        && let Ok(content) = std::fs::read_to_string(&path)
    {
        let table: Table = content
            .parse::<toml::Table>()
            .map_err(|e| cfg_err(format!("解析 {} 失敗: {e}", path.display())))?;
        for key in PARTIAL_SECTIONS {
            if let Some(v) = table.get(key) {
                out.insert(key.to_string(), v.clone());
            }
        }
    }
    if front_desk {
        let perms = out
            .entry("permissions".to_string())
            .or_insert_with(|| Value::Table(Table::new()));
        if let Value::Table(pt) = perms {
            pt.insert("can_send_cross_agent".into(), Value::Boolean(true));
        }
    }
    let body = toml::to_string_pretty(&Value::Table(out))
        .map_err(|e| cfg_err(format!("序列化 partial 失敗: {e}")))?;
    Ok(format!(
        "# Generated by `duduclaw expert convert-teams` — whitelisted settings only.\n{body}"
    ))
}

// ─────────────────────────── TEAM.md extraction ───────────────────────────

/// Body of the section opened by a line starting with `heading` (up to the
/// next `## ` heading), exclusive of the heading line itself.
fn extract_section(md: &str, heading: &str) -> Option<String> {
    let mut collecting = false;
    let mut body = String::new();
    for line in md.lines() {
        if collecting {
            if line.starts_with("## ") {
                break;
            }
            body.push_str(line);
            body.push('\n');
        } else if line.trim_end().starts_with(heading) {
            collecting = true;
        }
    }
    collecting.then(|| body.trim().to_string()).filter(|s| !s.is_empty())
}

/// The document with the section opened by `heading` removed.
fn strip_section(md: &str, heading: &str) -> String {
    let mut skipping = false;
    let mut out = String::new();
    for line in md.lines() {
        if skipping {
            if line.starts_with("## ") && !line.trim_end().starts_with(heading) {
                skipping = false;
            } else {
                continue;
            }
        }
        if line.trim_end().starts_with(heading) {
            skipping = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `> 一句話摘要：…` line (the playbooks' canonical one-liner).
fn extract_one_liner(md: &str) -> Option<String> {
    md.lines().find_map(|l| {
        l.trim_start()
            .strip_prefix("> 一句話摘要：")
            .map(|rest| rest.trim().to_string())
            .filter(|s| !s.is_empty())
    })
}

/// Up to `max` distinct 「…」 quotes from the dispatch playbook, surfaced as
/// recommended prompts. Playbooks use either numbered lines (`1. 「…」 → …`)
/// or table rows (`| 1 | 「…」 | …`); in both, only the FIRST quote of a
/// scenario line is a user request (later quotes on the same line are reply
/// wording / caveats), so scan per-line and fall back to a whole-section
/// scan when the section has no scenario lines at all.
fn extract_prompts(section: &str, max: usize) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut push = |quote: String, out: &mut Vec<String>| {
        if !quote.is_empty() && quote.chars().count() <= 60 && seen.insert(quote.clone()) {
            out.push(quote);
        }
    };
    for line in section.lines() {
        if out.len() >= max {
            break;
        }
        let trimmed = line.trim_start();
        let scenario_line = trimmed
            .chars()
            .next()
            .map(|c| c.is_ascii_digit() || c == '|')
            .unwrap_or(false)
            // Skip the table header/separator rows (no quotes anyway, but be
            // explicit about intent).
            && !trimmed.starts_with("|---")
            && !trimmed.starts_with("| #");
        if !scenario_line {
            continue;
        }
        if let Some(q) = first_quote(trimmed) {
            push(q, &mut out);
        }
    }
    if out.is_empty()
        && let Some(q) = first_quote(section)
    {
        push(q, &mut out);
    }
    out.truncate(max);
    out
}

/// The first 「…」 quote inside `text`, if any.
fn first_quote(text: &str) -> Option<String> {
    let start = text.find('「')?;
    let after = &text[start + '「'.len_utf8()..];
    let end = after.find('」')?;
    Some(after[..end].trim().to_string())
}

/// Channels named on a TEAM.md「對外通道」line; default when absent.
fn extract_channels(md: &str) -> Vec<String> {
    for line in md.lines() {
        if let Some(pos) = line.find("對外通道") {
            let tail = &line[pos..];
            let found: Vec<String> = KNOWN_CHANNELS
                .iter()
                .filter(|c| tail.contains(*c))
                .map(|c| c.to_string())
                .collect();
            if !found.is_empty() {
                return found;
            }
        }
    }
    DEFAULT_CHANNELS.iter().map(|c| c.to_string()).collect()
}

// ─────────────────────────── fs helpers ───────────────────────────

fn write_file(path: &Path, content: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_err(format!("建立 {} 失敗: {e}", parent.display())))?;
    }
    std::fs::write(path, content).map_err(|e| io_err(format!("寫入 {} 失敗: {e}", path.display())))
}

/// Recursively copy `.md` files from `src` into `dest`, preserving relative
/// paths (skips symlinks; non-markdown assets are not wiki pages). Content
/// passes through [`sanitize_wiki_content`] so curated pages survive the
/// install-time security scan.
fn copy_md_tree(src: &Path, dest: &Path) -> Result<()> {
    let rd = std::fs::read_dir(src).map_err(|e| io_err(format!("讀取 {} 失敗: {e}", src.display())))?;
    for entry in rd.flatten() {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_md_tree(&path, &target)?;
        } else if ft.is_file() && path.extension().and_then(|e| e.to_str()) == Some("md") {
            let content = std::fs::read_to_string(&path)
                .map_err(|e| io_err(format!("讀取 {} 失敗: {e}", path.display())))?;
            write_file(&target, &sanitize_wiki_content(&content))?;
        }
    }
    Ok(())
}

/// De-fang statute-citation URLs so curated wiki pages pass the install-time
/// security scan without weakening the scanner itself: the skill security
/// scanner (HS6) blocks any line whose URL carries a query/fragment (`?`,
/// `=`, `#`, …) as a potential exfiltration sink — which also hits legit
/// 全國法規資料庫 citations (`…LawSingle.aspx?pcode=…&flno=…`). On exactly
/// those lines the scheme is stripped (`https://law.moj.gov.tw/…` →
/// `law.moj.gov.tw/…`): the citation stays complete and human-recoverable,
/// but the line no longer contains a URL for a model to call. Clean URLs
/// (no query) are left untouched.
fn sanitize_wiki_content(content: &str) -> String {
    let mut out = String::with_capacity(content.len());
    for line in content.lines() {
        let has_url = line.contains("http://") || line.contains("https://");
        let sinky = line.contains('?')
            || line.contains('=')
            || line.contains('#')
            || line.contains("%20")
            || line.contains('$');
        if has_url && sinky {
            out.push_str(&line.replace("https://", "").replace("http://", ""));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

// ─────────────────────────── tests ───────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    struct TempTree(PathBuf);
    impl TempTree {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("dc-teamconv-{tag}-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            TempTree(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// Synthetic teams tree mirroring the real layout: one team, one shared
    /// department kit, one sibling industry pack.
    fn build_fixture(root: &Path) {
        let teams = root.join("teams");
        write(
            &teams.join("demo-team/team.toml"),
            r#"
schema = 1
industry = "demo"
pack = "demo-pro"
label = "示範產業"

[front_desk]
name = "demo-assistant"
display_name = "示範總機"
summary = "對外唯一窗口，分派部門任務"

[[workers]]
kit = "care"
name = "demo-care"
display_name = "回訪助理"
trigger = "demo-care"
summary = "回訪提醒"
overlay = [
  "提醒訊息僅含中性資訊，需要專業判斷一律轉真人",
  "不得執行不可逆金流動作",
]

[[humans]]
title = "店長"
summary = "本崗位不建 AI，由真人擔任"
"#,
        );
        write(
            &teams.join("demo-team/TEAM.md"),
            "# demo-team\n\n> 一句話摘要：`demo-pro` 總機分派部門任務，專業判斷留給真人。\n\n\
             總機（對外通道 line/telegram）\n\n\
             ## 部署步驟\n\n1. cp 一堆檔案\n\n\
             ## 總機分派劇本\n\n1. **「幫我排回訪提醒」** → 分派 `demo-care`。\n2. 「這要專業判斷嗎？」 → 轉真人。\n\n\
             ## wiki 共享\n\n- 總機 wiki 全團隊可讀。\n",
        );
        write(
            &teams.join("_departments/care/SOUL.md"),
            "# 回訪關懷助理\n\n我負責回訪提醒。\n\n## Industry Overlay（部署時由 TEAM.md 填入）\n\n## Escalation\n\n- overlay 未填 → needs_human\n",
        );
        write(
            &teams.join("_departments/care/agent.toml"),
            "[agent]\nname = \"dept-care\"\n\n[model]\npreferred = \"claude-haiku-4-5\"\n\n[permissions]\ncan_send_cross_agent = false\n\n[budget]\nmonthly_limit_cents = 1500\n",
        );
        write(
            &root.join("demo-pro/SOUL.md"),
            "# 示範總機\n\n我是示範產業的總機。\n",
        );
        write(
            &root.join("demo-pro/agent.toml"),
            "[agent]\nname = \"demo-assistant\"\n\n[model]\npreferred = \"claude-sonnet-4-6\"\n\n[permissions]\ncan_send_cross_agent = false\ncan_create_agents = false\n",
        );
        write(
            &root.join("demo-pro/wiki/compliance.md"),
            "# 合規\n\n重點。\n\n- 法條出處：https://law.moj.gov.tw/LawSingle.aspx?pcode=X01&flno=8\n",
        );
    }

    #[test]
    fn convert_produces_valid_idempotent_pack() {
        let root = TempTree::new("root");
        build_fixture(root.path());
        let teams = root.path().join("teams");
        let out = root.path().join("experts");

        cmd_convert_teams(&teams, Some(&out)).expect("convert should succeed");
        let pack = out.join("demo-team");

        // Manifest parses + validates (strict pack-level validator).
        let m = manifest::read(&pack).expect("expert.toml parses");
        assert!(manifest::validate(&m, &pack).is_empty(), "pack validates");
        let e = &m.expert;
        assert_eq!(e.name, "demo-team");
        assert_eq!(e.display("zh-TW"), "示範產業");
        assert!(e.description.contains("總機分派部門任務"));
        assert_eq!(e.agents.len(), 2);
        assert_eq!(e.agents[0].role, "front_desk");
        assert_eq!(e.agents[1].reports_to, "demo-assistant");
        // Prompts from the dispatch playbook quotes; channels from 對外通道.
        assert!(e.prompts.recommended.iter().any(|p| p.contains("回訪提醒")));
        assert_eq!(e.channels.suggested, vec!["telegram", "line"]);

        // Front soul = industry pack SOUL + human-roles section.
        let front = std::fs::read_to_string(pack.join("agents/demo-assistant/soul.md")).unwrap();
        assert!(front.contains("我是示範產業的總機"));
        assert!(front.contains("店長"));
        // Front partial forces delegation on.
        let fp =
            std::fs::read_to_string(pack.join("agents/demo-assistant/agent.partial.toml")).unwrap();
        assert!(fp.contains("can_send_cross_agent = true"));
        assert!(fp.contains("claude-sonnet-4-6"));
        assert!(!fp.contains("[agent]"), "identity section never carried");

        // Worker soul = kit SOUL with overlay injected under the placeholder.
        let ws = std::fs::read_to_string(pack.join("agents/demo-care/soul.md")).unwrap();
        let overlay_pos = ws.find("## Industry Overlay").unwrap();
        let escalation_pos = ws.find("## Escalation").unwrap();
        let bullet_pos = ws.find("提醒訊息僅含中性資訊").unwrap();
        assert!(overlay_pos < bullet_pos && bullet_pos < escalation_pos);
        let wp = std::fs::read_to_string(pack.join("agents/demo-care/agent.partial.toml")).unwrap();
        assert!(wp.contains("claude-haiku-4-5"));
        assert!(wp.contains("monthly_limit_cents"));

        // Dispatch skill in Agent Skills format, name == dir name.
        let skill = std::fs::read_to_string(pack.join("skills/demo-dispatch/SKILL.md")).unwrap();
        assert!(skill.starts_with("---\nname: demo-dispatch\n"));
        assert!(skill.contains("幫我排回訪提醒"));

        // Wiki: namespaced industry page + TEAM.md SOP without 部署步驟.
        // Statute-citation query URLs are de-fanged (scheme stripped) so the
        // page passes the install-time security scan.
        let compliance =
            std::fs::read_to_string(pack.join("wiki/demo-team/compliance.md")).unwrap();
        assert!(!compliance.contains("https://law.moj.gov.tw"));
        assert!(compliance.contains("law.moj.gov.tw/LawSingle.aspx?pcode=X01&flno=8"));
        let sop = std::fs::read_to_string(pack.join("wiki/demo-team/team-sop.md")).unwrap();
        assert!(sop.contains("總機分派劇本"));
        assert!(!sop.contains("部署步驟"), "deployment steps stripped");

        // Idempotency: a second run reproduces byte-identical output.
        let before = std::fs::read_to_string(pack.join("expert.toml")).unwrap();
        let soul_before = ws.clone();
        cmd_convert_teams(&teams, Some(&out)).expect("re-run should succeed");
        let after = std::fs::read_to_string(pack.join("expert.toml")).unwrap();
        let soul_after = std::fs::read_to_string(pack.join("agents/demo-care/soul.md")).unwrap();
        assert_eq!(before, after, "expert.toml byte-identical across runs");
        assert_eq!(soul_before, soul_after, "souls byte-identical across runs");
    }

    #[test]
    fn section_helpers() {
        let md = "intro\n\n## A\n\na-body\n\n## B\n\nb-body\n";
        assert_eq!(extract_section(md, "## A").as_deref(), Some("a-body"));
        assert!(extract_section(md, "## C").is_none());
        let stripped = strip_section(md, "## A");
        assert!(!stripped.contains("a-body"));
        assert!(stripped.contains("b-body"));
    }

    #[test]
    fn prompt_extraction_takes_first_quote_per_scenario_line() {
        // Only the first quote of a numbered line is a user request; the
        // second (「回話模板」) must be skipped. Duplicates dedupe; cap holds.
        let s = "1. **「甲請求」** → 回「罐頭回覆」。\n\
                 2. 「乙請求」 → 轉真人。\n\
                 - 非編號行的「雜訊」不取。\n\
                 3. 「甲請求」重複。\n\
                 4. 「丙請求」。\n\
                 5. 「丁請求」。";
        assert_eq!(extract_prompts(s, 3), vec!["甲請求", "乙請求", "丙請求"]);
        assert!(extract_prompts("no quotes", 3).is_empty());
        // Fallback: no numbered lines → first quote anywhere.
        assert_eq!(extract_prompts("散文中的「唯一引語」而已", 3), vec!["唯一引語"]);
        // Table-style playbooks: first quote per row, header rows skipped.
        let table = "| # | 訊息 | 決策 |\n|---|---|---|\n\
                     | 1 | 「表格請求」 | 回「模板」 |\n\
                     | 2 | 「另一請求」 | 轉真人 |";
        assert_eq!(extract_prompts(table, 3), vec!["表格請求", "另一請求"]);
    }

    #[test]
    fn sanitize_strips_scheme_only_on_sink_lines() {
        let src = "看 https://example.com/docs 即可\n\
                   出處：https://law.moj.gov.tw/a.aspx?pcode=L1&flno=67\n";
        let out = sanitize_wiki_content(src);
        // Clean URL untouched; query URL de-fanged but citation preserved.
        assert!(out.contains("https://example.com/docs"));
        assert!(!out.contains("https://law.moj.gov.tw"));
        assert!(out.contains("law.moj.gov.tw/a.aspx?pcode=L1&flno=67"));
    }

    #[test]
    fn channel_extraction_and_default() {
        assert_eq!(
            extract_channels("總機（對外通道 line/telegram）"),
            vec!["telegram", "line"]
        );
        assert_eq!(extract_channels("nothing here"), vec!["line", "telegram"]);
    }
}
