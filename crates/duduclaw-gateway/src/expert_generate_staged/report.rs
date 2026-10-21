//! Draft preview, install pre-check and post-install placement report.
//!
//! Everything here reads the MATERIALIZED pack on disk (the artifact the
//! installer will actually consume), never the model JSON, so what the
//! dashboard shows is what would be installed.
//!
//! The CLI installer skips an item silently from the dashboard's point of
//! view (it only prints a console line): a wiki page or skill blocked by the
//! content scan, a same-name wiki page that already exists (never
//! overwritten), an agent whose id is taken. [`install_precheck`] predicts
//! those before install and [`placement_report`] states what actually landed
//! after, so nothing is skipped without the operator seeing it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::expert_generate::{DraftState, draft_pack_dir};

use super::scan_pack_content;

/// Marker line of a placeholder persona (member whose content failed).
pub const PLACEHOLDER_MARKER: &str = "這份角色說明是暫代版本";

#[derive(Debug, Clone)]
struct ManifestAgent {
    name: String,
    role: String,
    display_name: String,
    reports_to: String,
    skills: Vec<String>,
}

fn manifest_table(pack_dir: &Path) -> toml::Table {
    std::fs::read_to_string(pack_dir.join("expert.toml"))
        .ok()
        .and_then(|raw| raw.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

fn str_of<'a>(t: &'a toml::Table, key: &str) -> &'a str {
    t.get(key).and_then(|v| v.as_str()).unwrap_or_default()
}

fn str_list(v: Option<&toml::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
        .unwrap_or_default()
}

fn manifest_agents(expert: Option<&toml::Table>) -> Vec<ManifestAgent> {
    expert
        .and_then(|t| t.get("agents"))
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_table())
                .map(|t| ManifestAgent {
                    name: str_of(t, "name").to_string(),
                    role: str_of(t, "role").to_string(),
                    display_name: str_of(t, "display_name").to_string(),
                    reports_to: str_of(t, "reports_to").to_string(),
                    skills: str_list(t.get("skills")),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Relative paths (forward slashes) of every `.md` under `<pack>/wiki`.
fn wiki_files(pack_dir: &Path) -> Vec<String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            let path = entry.path();
            if ft.is_dir() {
                walk(base, &path, out);
            } else if ft.is_file()
                && path.extension().and_then(|e| e.to_str()) == Some("md")
                && let Ok(rel) = path.strip_prefix(base)
            {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    let base = pack_dir.join("wiki");
    let mut out = Vec::new();
    walk(&base, &base, &mut out);
    out.sort();
    out
}

fn first_heading(content: &str) -> Option<String> {
    content
        .lines()
        .find_map(|l| l.strip_prefix("# ").map(|t| t.trim().to_string()))
}

fn skill_description(pack_dir: &Path, skill: &str) -> String {
    let content = std::fs::read_to_string(pack_dir.join("skills").join(skill).join("SKILL.md"))
        .unwrap_or_default();
    duduclaw_agent::skill_loader::parse_skill_meta_from_content(&content, skill).description
}

/// Same parser as the runtime (`duduclaw_agent::contract`); a missing or
/// unparsable file reads as empty, exactly as the runtime treats it.
fn read_contract(pack_dir: &Path, agent: &str) -> duduclaw_agent::contract::Contract {
    std::fs::read_to_string(pack_dir.join("agents").join(agent).join("CONTRACT.toml"))
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

fn agent_preview(pack_dir: &Path, a: &ManifestAgent) -> Value {
    let soul =
        std::fs::read_to_string(pack_dir.join("agents").join(&a.name).join("soul.md")).unwrap_or_default();
    let contract = read_contract(pack_dir, &a.name);
    let skills: Vec<Value> = a
        .skills
        .iter()
        .map(|s| json!({ "name": s, "description": skill_description(pack_dir, s) }))
        .collect();
    json!({
        "name": a.name,
        "role": a.role,
        "display_name": a.display_name,
        "reports_to": a.reports_to,
        "soul_excerpt": duduclaw_core::truncate_chars(soul.trim(), 200),
        "soul_chars": soul.chars().count(),
        "placeholder": soul.contains(PLACEHOLDER_MARKER),
        "skills": skills,
        "red_lines": contract.boundaries.must_not,
        "must_always": contract.boundaries.must_always,
    })
}

fn wiki_pages(pack_dir: &Path) -> Vec<Value> {
    wiki_files(pack_dir)
        .into_iter()
        .map(|rel| {
            let content = std::fs::read_to_string(pack_dir.join("wiki").join(&rel)).unwrap_or_default();
            let title = first_heading(&content).unwrap_or_else(|| rel.clone());
            json!({ "file": rel, "title": title, "chars": content.chars().count() })
        })
        .collect()
}

/// Build the preview payload the UI shows before install.
pub fn draft_preview_json(home: &Path, state: &DraftState) -> Value {
    let Ok(pack_dir) = draft_pack_dir(home, &state.draft_id) else {
        return Value::Null;
    };
    let manifest = manifest_table(&pack_dir);
    let expert = manifest.get("expert").and_then(|v| v.as_table());
    let get = |key: &str| expert.map(|t| str_of(t, key).to_string()).unwrap_or_default();
    let sub_list = |table: &str, key: &str| {
        str_list(
            expert
                .and_then(|t| t.get(table))
                .and_then(|v| v.as_table())
                .and_then(|t| t.get(key)),
        )
    };
    let display_name = expert
        .and_then(|t| t.get("display_name"))
        .and_then(|v| v.as_table())
        .map(|t| str_of(t, "zh-TW").to_string())
        .unwrap_or_default();
    let agents = manifest_agents(expert);
    let pages = wiki_pages(&pack_dir);
    let mut skills: Vec<String> = agents.iter().flat_map(|a| a.skills.clone()).collect();
    skills.sort();
    skills.dedup();
    let wiki_titles: Vec<Value> = pages.iter().map(|p| p["title"].clone()).collect();

    json!({
        "slug": get("name"),
        "display_name": display_name,
        "description": get("description"),
        "version": get("version"),
        "prompts": sub_list("prompts", "recommended"),
        "channels": sub_list("channels", "suggested"),
        "agents": agents.iter().map(|a| agent_preview(&pack_dir, a)).collect::<Vec<_>>(),
        "skills": skills,
        "wiki_titles": wiki_titles,
        "wiki_pages": pages,
        "notes": state.notes,
        "install_warnings": install_precheck(home, &pack_dir),
    })
}

fn warning(kind: &str, name: &str, reason: &str, detail: &str) -> Value {
    json!({ "kind": kind, "name": name, "reason": reason, "detail": detail })
}

/// Predict what the installer will skip: content-scan blocks, taken agent
/// ids, and wiki pages whose target already exists (never overwritten).
pub fn install_precheck(home: &Path, pack_dir: &Path) -> Vec<Value> {
    let manifest = manifest_table(pack_dir);
    let agents = manifest_agents(manifest.get("expert").and_then(|v| v.as_table()));
    let mut out = Vec::new();
    for a in &agents {
        if home.join("agents").join(&a.name).exists() {
            out.push(warning("agent", &a.name, "conflict", "已有同名 AI 員工，這位不會安裝"));
        }
        let agent_dir = pack_dir.join("agents").join(&a.name);
        for (file, kind) in [("soul.md", "agent"), ("CONTRACT.toml", "contract")] {
            let body = std::fs::read_to_string(agent_dir.join(file)).unwrap_or_default();
            if let Some(reason) = scan_pack_content(&body) {
                out.push(warning(kind, &a.name, "blocked", &reason));
            }
        }
    }
    let skill_dirs: BTreeSet<String> = agents.iter().flat_map(|a| a.skills.clone()).collect();
    for skill in &skill_dirs {
        let body = std::fs::read_to_string(pack_dir.join("skills").join(skill).join("SKILL.md"))
            .unwrap_or_default();
        if let Some(reason) = scan_pack_content(&body) {
            out.push(warning("skill", skill, "blocked", &reason));
        }
    }
    for rel in wiki_files(pack_dir) {
        if home.join("shared").join("wiki").join(&rel).exists() {
            out.push(warning("wiki", &rel, "conflict", "知識庫已有同名頁面，不會覆蓋"));
            continue;
        }
        let body = std::fs::read_to_string(pack_dir.join("wiki").join(&rel)).unwrap_or_default();
        if let Some(reason) = scan_pack_content(&body) {
            out.push(warning("wiki", &rel, "blocked", &reason));
        }
    }
    out
}

/// Wiki targets that already exist BEFORE install (snapshot for
/// [`placement_report`]: a page missing after install that was already
/// there is a conflict, not a scan block).
pub fn existing_wiki_targets(home: &Path, pack_dir: &Path) -> BTreeSet<String> {
    wiki_files(pack_dir)
        .into_iter()
        .filter(|rel| home.join("shared").join("wiki").join(rel).exists())
        .collect()
}

/// What actually landed, read back from `<home>` after the install
/// subprocess finished. `installed_agents` / `installed_wiki` come from the
/// install record the CLI wrote.
pub fn placement_report(
    home: &Path,
    pack_dir: &Path,
    installed_agents: &[String],
    installed_wiki: &[String],
    preexisting_wiki: &BTreeSet<String>,
) -> Value {
    let manifest = manifest_table(pack_dir);
    let agents = manifest_agents(manifest.get("expert").and_then(|v| v.as_table()));
    let agent_rows: Vec<Value> = agents
        .iter()
        .map(|a| {
            let installed = installed_agents.iter().any(|x| x == &a.name);
            let home_agent: PathBuf = home.join("agents").join(&a.name);
            let skills: Vec<Value> = a
                .skills
                .iter()
                .map(|s| {
                    let ok = installed && home_agent.join("SKILLS").join(s).join("SKILL.md").is_file();
                    json!({ "name": s, "installed": ok })
                })
                .collect();
            let wants_contract = pack_dir.join("agents").join(&a.name).join("CONTRACT.toml").is_file();
            json!({
                "name": a.name,
                "display_name": a.display_name,
                "installed": installed,
                "skills": skills,
                "contract": if wants_contract {
                    json!(installed && home_agent.join("CONTRACT.toml").is_file())
                } else {
                    Value::Null
                },
            })
        })
        .collect();
    let wiki_rows: Vec<Value> = wiki_files(pack_dir)
        .into_iter()
        .map(|rel| {
            let status = if installed_wiki.iter().any(|w| w == &rel) {
                "installed".to_string()
            } else if preexisting_wiki.contains(&rel) {
                "conflict".to_string()
            } else {
                let body =
                    std::fs::read_to_string(pack_dir.join("wiki").join(&rel)).unwrap_or_default();
                match scan_pack_content(&body) {
                    Some(_) => "blocked".to_string(),
                    None => "skipped".to_string(),
                }
            };
            json!({ "file": rel, "status": status })
        })
        .collect();
    let problems = agent_rows.iter().any(|a| {
        a["installed"] != json!(true)
            || a["contract"] == json!(false)
            || a["skills"]
                .as_array()
                .is_some_and(|s| s.iter().any(|x| x["installed"] != json!(true)))
    }) || wiki_rows.iter().any(|w| w["status"] != json!("installed"));
    json!({ "agents": agent_rows, "wiki": wiki_rows, "complete": !problems })
}
