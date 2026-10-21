//! OpenClaw-style persona workspaces (also SoulSpec / OpenGAP, which reuse
//! the same file names) and the OpenClaw state directory that lists several
//! of them.
//!
//! | file                         | slot                                   |
//! |------------------------------|----------------------------------------|
//! | `SOUL.md`                    | SOUL.md                                |
//! | `IDENTITY.md`                | IDENTITY.md + name / emoji             |
//! | `AGENTS.md` (> CLAUDE/GEMINI)| INSTRUCTIONS.md                        |
//! | `USER.md`                    | wiki「服務對象」                        |
//! | `TOOLS.md` (retired)         | wiki「工具備註」                        |
//! | `HEARTBEAT.md` (retired)     | wiki「定期檢查清單」                    |
//! | `BOOT.md`                    | wiki knowledge page (not executed)     |
//! | `MEMORY.md`, `memory/*.md`   | memory facts (low trust, lineage)      |
//! | `skills/`, `.agents/skills/` | skills                                 |
//! | `BOOTSTRAP.md`               | skipped (one-time ritual)              |
//! | anything else                | archived under `imported/`             |

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::detect::ConfigKind;
use super::super::ir::{AgentDraft, Doc, ImportBundle};
use super::super::md;
use super::{bundle, dir_label, leftovers, pick_instructions, skill_dirs};

/// A single workspace directory as one agent.
pub fn read_single(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::Workspace);
    let mut secrets = Vec::new();
    let mut hidden = Vec::new();
    let agent = read_workspace(root, root, &dir_label(root), &mut secrets, &mut hidden);
    b.display_name = agent.display_name.clone();
    b.slug_hint = agent.id_hint.clone();
    b.agents.push(agent);
    note_secrets_hidden(&mut b, &secrets, &hidden);
    Ok(b)
}

/// Read one workspace into an [`AgentDraft`]. `pub` so `duduclaw migrate
/// from openclaw` routes the same files through the same slots.
pub fn read_workspace(
    ws: &Path,
    import_root: &Path,
    ref_id: &str,
    secrets: &mut Vec<String>,
    hidden: &mut Vec<String>,
) -> AgentDraft {
    let mut consumed: BTreeSet<PathBuf> = BTreeSet::new();
    let take = |name: &str, consumed: &mut BTreeSet<PathBuf>| -> Option<Doc> {
        let p = ws.join(name);
        if !p.is_file() {
            return None;
        }
        consumed.insert(p.clone());
        md::read_text(&p).map(|t| Doc::new(md::rel(import_root, &p), t))
    };

    let mut a = AgentDraft {
        ref_id: ref_id.to_string(),
        id_hint: ref_id.to_string(),
        ..Default::default()
    };
    a.soul = take("SOUL.md", &mut consumed);
    a.identity = take("IDENTITY.md", &mut consumed);
    a.user_profile = take("USER.md", &mut consumed);
    a.tools_notes = take("TOOLS.md", &mut consumed);
    a.heartbeat = take("HEARTBEAT.md", &mut consumed);
    if let Some(boot) = take("BOOT.md", &mut consumed) {
        a.knowledge.push(boot);
    }
    if ws.join("BOOTSTRAP.md").is_file() {
        consumed.insert(ws.join("BOOTSTRAP.md"));
        a.skipped.push((
            md::rel(import_root, &ws.join("BOOTSTRAP.md")),
            "首次啟動用的一次性設定，不匯入".into(),
        ));
    }
    if let Some(mem) = take("MEMORY.md", &mut consumed) {
        a.memory.push(mem);
    }
    let mem_dir = ws.join("memory");
    if mem_dir.is_dir() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&mem_dir)
            .map(|rd| rd.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.sort();
        for f in files {
            let is_md = f.extension().and_then(|e| e.to_str()) == Some("md");
            if is_md && let Some(t) = md::read_text(&f) {
                consumed.insert(f.clone());
                a.memory.push(Doc::new(md::rel(import_root, &f), t));
            }
        }
    }

    let (instr, skipped) = pick_instructions(ws, import_root, &mut consumed);
    a.instructions = instr;
    a.skipped.extend(skipped);

    for root in [ws.join("skills"), ws.join(".agents").join("skills")] {
        if root.is_dir() {
            consumed.insert(root.clone());
            a.skills.extend(skill_dirs(&root, import_root));
        }
    }

    // Name / emoji: IDENTITY.md wins, else the SOUL.md heading, else the ref.
    let ident = a
        .identity
        .as_ref()
        .map(|d| md::parse_identity(&d.text))
        .unwrap_or_default();
    a.display_name = ident
        .name
        .clone()
        .or_else(|| a.soul.as_ref().and_then(|d| first_heading(&d.text)))
        .unwrap_or_else(|| ref_id.to_string());
    a.emoji = ident.emoji.clone();
    a.description = ident.vibe.unwrap_or_default();
    if let Some(name) = &ident.name {
        a.id_hint = name.clone();
    }

    a.unmapped = leftovers(ws, import_root, &consumed, secrets, hidden);
    a
}

/// `# Title` of a Markdown document, ignoring file-name headings
/// (`# SOUL.md - …` keeps only the part after the dash).
fn first_heading(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| l.starts_with("# "))?;
    let title = line.trim_start_matches("# ").trim();
    let title = title
        .split_once(" - ")
        .map(|(_, t)| t)
        .unwrap_or(title)
        .trim();
    let lower = title.to_ascii_lowercase();
    if title.is_empty() || lower.ends_with(".md") || lower == "soul" {
        None
    } else {
        Some(title.to_string())
    }
}

/// OpenClaw state dir: `openclaw.json` (JSON5) names the agents and their
/// workspaces (`agents.list[]` or the `agents.entries` map). Only
/// workspaces inside the import root are read — an absolute path from the
/// source machine is never followed (an uploaded zip must not make the
/// importer read arbitrary local folders). Channel tokens, `env` and API
/// keys in the config are never imported here.
pub fn read_state_dir(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::OpenClawState);
    let cfg_path = ["openclaw.json", "moltbot.json", "clawdbot.json"]
        .iter()
        .map(|f| root.join(f))
        .find(|p| p.is_file())
        .ok_or_else(|| "找不到 openclaw.json".to_string())?;
    let raw = md::read_text(&cfg_path).ok_or_else(|| "無法讀取 openclaw.json".to_string())?;
    let cfg: serde_json::Value =
        json5::from_str(&raw).map_err(|e| format!("openclaw.json 不是合法的 JSON5：{e}"))?;

    let specs = agent_specs(&cfg);
    let default_model = jstr(&cfg, &["agents", "defaults", "model", "primary"])
        .or_else(|| jstr(&cfg, &["agents", "defaults", "model"]));
    let default_ws = jstr(&cfg, &["agents", "defaults", "workspace"]);
    let single = specs.len() <= 1;
    let root_canon = root
        .canonicalize()
        .map_err(|e| format!("無法解析匯入目錄：{e}"))?;

    let mut secrets = Vec::new();
    let mut hidden = Vec::new();
    let mut used_ws: BTreeSet<PathBuf> = BTreeSet::new();
    for spec in &specs {
        let rel_ws = spec
            .workspace
            .clone()
            .or_else(|| if single { default_ws.clone() } else { None })
            .unwrap_or_else(|| {
                if single || spec.id == "main" {
                    "workspace".into()
                } else {
                    format!("workspace-{}", spec.id)
                }
            });
        let ws = root.join(&rel_ws);
        let inside = ws
            .canonicalize()
            .map(|c| c.starts_with(&root_canon))
            .unwrap_or(false);
        if !inside || !ws.is_dir() {
            b.warnings.push(format!(
                "AI 員工「{}」的工作區 {} 不在匯入的資料夾內，已略過",
                spec.id, rel_ws
            ));
            continue;
        }
        used_ws.insert(ws.clone());
        if let Ok(c) = ws.canonicalize() {
            used_ws.insert(c);
        }
        let mut agent = read_workspace(&ws, root, &spec.id, &mut secrets, &mut hidden);
        if let Some(name) = &spec.name {
            agent.display_name = name.clone();
        }
        agent.model = spec.model.clone().or_else(|| default_model.clone());
        b.agents.push(agent);
    }
    if b.agents.is_empty() {
        return Err("openclaw.json 裡沒有可匯入的工作區".into());
    }

    b.display_name = b.agents[0].display_name.clone();
    b.slug_hint = if b.agents.len() == 1 {
        b.agents[0].id_hint.clone()
    } else {
        "openclaw-team".into()
    };
    b.warnings.push(
        "openclaw.json 內的通道金鑰、環境變數與 API 金鑰不會匯入；要連同通道一起搬，請改用「duduclaw migrate from openclaw」。"
            .into(),
    );
    let mut consumed: BTreeSet<PathBuf> = used_ws;
    consumed.insert(cfg_path);
    for skip in ["agents", "cron", "sessions", "logs"] {
        if root.join(skip).is_dir() {
            consumed.insert(root.join(skip));
            b.warnings
                .push(format!("{skip}/（對話紀錄與排程）不在設定檔匯入範圍"));
        }
    }
    b.unmapped = leftovers(root, root, &consumed, &mut secrets, &mut hidden);
    note_secrets_hidden(&mut b, &secrets, &hidden);
    Ok(b)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Spec {
    id: String,
    name: Option<String>,
    workspace: Option<String>,
    model: Option<String>,
}

/// `agents.list[]` (`{id|name, workspace, model}`) or the `agents.entries`
/// map (`{<id>: {workspace, model, name}}`); one implicit `main` otherwise.
fn agent_specs(cfg: &serde_json::Value) -> Vec<Spec> {
    let one = |id: String, v: &serde_json::Value| Spec {
        name: jstr(v, &["name"]).filter(|n| *n != id),
        workspace: jstr(v, &["workspace"]),
        model: jstr(v, &["model", "primary"]).or_else(|| jstr(v, &["model"])),
        id,
    };
    let agents = cfg.get("agents");
    let mut out = Vec::new();
    if let Some(list) = agents
        .and_then(|a| a.get("list"))
        .and_then(|l| l.as_array())
    {
        for item in list {
            let id = jstr(item, &["id"])
                .or_else(|| jstr(item, &["name"]))
                .unwrap_or_else(|| "main".into());
            out.push(one(id, item));
        }
    }
    if let Some(map) = agents
        .and_then(|a| a.get("entries"))
        .and_then(|m| m.as_object())
    {
        for (id, v) in map {
            if !out.iter().any(|s| &s.id == id) {
                out.push(one(id.clone(), v));
            }
        }
    }
    if out.is_empty() {
        out.push(Spec {
            id: "main".into(),
            name: None,
            workspace: None,
            model: None,
        });
    }
    out
}

fn jstr(v: &serde_json::Value, path: &[&str]) -> Option<String> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub(super) fn note_secrets_hidden(b: &mut ImportBundle, secrets: &[String], hidden: &[String]) {
    if !secrets.is_empty() {
        b.warnings.push(format!(
            "以下檔案含金鑰或登入資訊，不會匯入，請到 DuDuClaw 重新設定：{}",
            secrets.join("、")
        ));
    }
    if !hidden.is_empty() {
        b.warnings
            .push(format!("已略過隱藏資料夾：{}", hidden.join("、")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn workspace_maps_every_persona_file() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path();
        write(&ws.join("SOUL.md"), "# 小助理\n親切");
        write(&ws.join("IDENTITY.md"), "- Name: Molty\n- Emoji: 🦞\n");
        write(&ws.join("AGENTS.md"), "先讀 USER.md");
        write(&ws.join("CLAUDE.md"), "@AGENTS.md");
        write(&ws.join("USER.md"), "- 叫我 Sam");
        write(&ws.join("TOOLS.md"), "- 印表機在二樓");
        write(&ws.join("HEARTBEAT.md"), "- 每天看信箱");
        write(&ws.join("BOOTSTRAP.md"), "first run");
        write(&ws.join("MEMORY.md"), "- 客戶在台北");
        write(&ws.join("memory/2026-10-01.md"), "- 週一開會");
        write(
            &ws.join("skills/greet/SKILL.md"),
            "---\nname: greet\ndescription: hi\n---\n",
        );
        write(&ws.join(".env"), "OPENAI_API_KEY=sk-x");
        write(&ws.join("notes/plan.txt"), "x");
        let b = read_single(ws).unwrap();
        let a = &b.agents[0];
        assert_eq!(a.display_name, "Molty");
        assert_eq!(a.emoji.as_deref(), Some("🦞"));
        assert_eq!(a.instructions.as_ref().unwrap().text, "先讀 USER.md");
        assert!(a.skipped.iter().any(|(s, _)| s == "CLAUDE.md"));
        assert!(a.skipped.iter().any(|(s, _)| s == "BOOTSTRAP.md"));
        assert!(a.user_profile.is_some() && a.tools_notes.is_some() && a.heartbeat.is_some());
        assert_eq!(a.memory.len(), 2);
        assert_eq!(a.skills.len(), 1);
        assert_eq!(a.unmapped.len(), 1, "{:?}", a.unmapped);
        assert_eq!(a.unmapped[0].rel, "notes/plan.txt");
        assert!(
            b.warnings.iter().any(|w| w.contains(".env")),
            "{:?}",
            b.warnings
        );
    }

    #[test]
    fn state_dir_reads_entries_map_and_refuses_outside_paths() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(
            &root.join("openclaw.json"),
            r#"{ agents: { entries: {
                 sales: { workspace: "ws-sales", model: "anthropic/claude-haiku-4-5" },
                 ops: { workspace: "/etc" },
               } },
               channels: { telegram: { botToken: "123:abc" } } }"#,
        );
        write(&root.join("ws-sales/SOUL.md"), "# 業務\n");
        let b = read_state_dir(root).unwrap();
        assert_eq!(b.agents.len(), 1);
        assert_eq!(b.agents[0].ref_id, "sales");
        assert_eq!(
            b.agents[0].model.as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
        assert!(
            b.warnings.iter().any(|w| w.contains("ops")),
            "{:?}",
            b.warnings
        );
        assert!(b.warnings.iter().any(|w| w.contains("通道金鑰")));
    }

    #[test]
    fn state_dir_list_form_and_default_workspace_names() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path();
        write(
            &root.join("openclaw.json"),
            r#"{ agents: { list: [ { id: "main" }, { id: "support" } ] } }"#,
        );
        write(&root.join("workspace/SOUL.md"), "main");
        write(&root.join("workspace-support/SOUL.md"), "support");
        let b = read_state_dir(root).unwrap();
        let ids: Vec<&str> = b.agents.iter().map(|a| a.ref_id.as_str()).collect();
        assert_eq!(ids, vec!["main", "support"]);
        assert_eq!(b.slug_hint, "openclaw-team");
    }
}
