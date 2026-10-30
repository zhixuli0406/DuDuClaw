//! Claude Code subagents (`.claude/agents/*.md`, `agents/*.md`) and GitHub
//! Copilot custom agents (`*.agent.md`, legacy `*.chatmode.md`,
//! `.github/agents/*.md`) — one parser, since both are "frontmatter + body
//! = system prompt". One file becomes one employee.
//!
//! | frontmatter                        | DuDuClaw                               |
//! |------------------------------------|----------------------------------------|
//! | `name` (Copilot: optional)         | employee id / display name             |
//! | `description`                      | IDENTITY.md + report                   |
//! | body                               | INSTRUCTIONS.md                        |
//! | `model`                            | `[model] preferred` (Claude ids only)  |
//! | `tools` / `disallowedTools`        | tool lists — narrowing only            |
//! | `mcpServers` / `mcp-servers`       | report only, never imported            |
//! | `hooks`, `permissionMode`, `handoffs`, `skills`, … | reported, not applied  |

use std::path::Path;

use super::super::detect::{ConfigKind, subagent_files};
use super::super::ir::{AgentDraft, Doc, ImportBundle};
use super::super::md;
use super::{bundle, dir_label};

/// Frontmatter keys with no DuDuClaw equivalent: reported, never applied.
const IGNORED_KEYS: &[&str] = &[
    "permissionMode",
    "hooks",
    "skills",
    "memory",
    "maxTurns",
    "background",
    "isolation",
    "effort",
    "initialPrompt",
    "handoffs",
    "target",
    "disable-model-invocation",
    "user-invocable",
    "omitClaudeMd",
    "autoCompactWindow",
];

pub fn read(root: &Path) -> Result<ImportBundle, String> {
    let mut b = bundle(ConfigKind::Subagents);
    for path in subagent_files(root) {
        match read_one(root, &path) {
            Ok(a) => b.agents.push(a),
            Err(e) => b.warnings.push(e),
        }
    }
    if b.agents.is_empty() {
        return Err("找不到可匯入的子代理定義".into());
    }
    b.display_name = if b.agents.len() == 1 {
        b.agents[0].display_name.clone()
    } else {
        dir_label(root)
    };
    b.slug_hint = if b.agents.len() == 1 {
        b.agents[0].id_hint.clone()
    } else {
        dir_label(root)
    };
    Ok(b)
}

fn read_one(root: &Path, path: &Path) -> Result<AgentDraft, String> {
    let rel = md::rel(root, path);
    let text = md::read_text(path).ok_or_else(|| format!("{rel} 無法以 UTF-8 讀取，略過"))?;
    let (Some(fm), body) = md::split_frontmatter(&text) else {
        return Err(format!("{rel} 沒有 frontmatter，略過"));
    };
    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("agent")
        .trim_end_matches(".md")
        .trim_end_matches(".agent")
        .trim_end_matches(".chatmode")
        .to_string();
    let name = fm.get("name").unwrap_or(&stem).to_string();
    let description = fm.get("description").unwrap_or("").to_string();

    let mut a = AgentDraft {
        ref_id: stem.clone(),
        id_hint: name.clone(),
        display_name: name.clone(),
        description: description.clone(),
        model: fm.get("model").map(str::to_string),
        tools: fm.list("tools"),
        disallowed_tools: fm.list("disallowedTools").unwrap_or_default(),
        ..Default::default()
    };
    if !body.trim().is_empty() {
        a.instructions = Some(Doc::new(rel.clone(), body));
    }
    if !description.is_empty() {
        a.identity = Some(Doc::new(
            rel.clone(),
            format!("# {name}\n\n{description}\n"),
        ));
    }
    for key in ["mcpServers", "mcp-servers"] {
        let Some(v) = fm.yaml.as_ref().and_then(|y| y.get(key)) else {
            continue;
        };
        match serde_json::to_value(v) {
            Ok(serde_json::Value::Object(map)) => a.mcp_servers.extend(map),
            // A list of server *names* refers to servers defined elsewhere.
            _ => a.ignored_fields.push((
                key.into(),
                "只列了伺服器名稱，請在 DuDuClaw 另行設定".into(),
            )),
        }
    }
    for key in IGNORED_KEYS {
        if let Some(v) = fm.get(key).map(str::to_string).or_else(|| {
            fm.yaml
                .as_ref()
                .and_then(|y| y.get(*key))
                .map(|_| "（已設定）".to_string())
        }) {
            a.ignored_fields
                .push(((*key).into(), duduclaw_core::truncate_chars(&v, 60)));
        }
    }
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    }

    #[test]
    fn claude_and_copilot_files_share_one_parser() {
        let d = tempfile::tempdir().unwrap();
        let r = d.path();
        write(
            &r.join(".claude/agents/reviewer.md"),
            "---\nname: code-reviewer\ndescription: Reviews diffs\ntools: Read, Grep\nmodel: sonnet\npermissionMode: bypassPermissions\nmcpServers:\n  github:\n    command: npx\n    env:\n      GITHUB_TOKEN: ghp_secret\n---\nYou review code.\n",
        );
        write(
            &r.join("planner.agent.md"),
            "---\ndescription: 規劃任務\ntools: ['search', 'fetch']\nhandoffs:\n  - label: x\n---\n先列出步驟。\n",
        );
        let b = read(r).unwrap();
        assert_eq!(b.agents.len(), 2);
        let rev = b
            .agents
            .iter()
            .find(|a| a.id_hint == "code-reviewer")
            .unwrap();
        assert_eq!(
            rev.tools.as_deref(),
            Some(&["Read".to_string(), "Grep".to_string()][..])
        );
        assert_eq!(rev.model.as_deref(), Some("sonnet"));
        assert_eq!(
            rev.instructions.as_ref().unwrap().text.trim(),
            "You review code."
        );
        assert!(rev.mcp_servers.contains_key("github"));
        assert!(
            rev.ignored_fields
                .iter()
                .any(|(k, _)| k == "permissionMode")
        );
        let plan = b.agents.iter().find(|a| a.ref_id == "planner").unwrap();
        assert_eq!(plan.id_hint, "planner");
        assert_eq!(plan.tools.as_ref().unwrap().len(), 2);
        assert!(plan.ignored_fields.iter().any(|(k, _)| k == "handoffs"));
    }
}
