//! Claude Code plugin importer (`.claude-plugin/plugin.json`) — P0.
//!
//! Mapping (design `commercial/docs/design-expert-pack.md` §2.2):
//!
//! | Claude Code plugin            | DuDuClaw                                  |
//! |-------------------------------|-------------------------------------------|
//! | `plugin.json` name/desc/ver   | pack slug / display / version             |
//! | `agents/*.md` frontmatter     | one agent each                            |
//! | └ `name`                      | agent id (sanitised)                      |
//! | └ `model` (sonnet/opus/…)     | `[model] preferred` (+needs-review flag)  |
//! | └ `tools`                     | `[capabilities] allowed_tools`            |
//! | └ `disallowedTools`           | `[capabilities] denied_tools`             |
//! | └ body                        | `SOUL.md` draft (scanned)                 |
//! | `skills/<name>/SKILL.md`      | global skill (verbatim, scanned)          |
//! | `commands/*.md`               | single skill each                         |
//! | `.mcp.json`                   | merged into each agent's per-agent config |
//! | `hooks/`                      | copied **disabled**, listed for approval  |
//! | lsp/themes/monitors/…         | ignored + reported                        |

use std::path::Path;

use super::install::{InstallCtx, install_skill_global};
use super::{
    InstallRecord, PackKind, Report, ScanVerdict, cfg_err, io_err, map_model, now_iso,
    scan_external, set_capabilities,
};

use duduclaw_core::error::Result;

pub(super) async fn install(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    let plugin_json = dir.join(".claude-plugin").join("plugin.json");
    let raw = std::fs::read_to_string(&plugin_json)
        .map_err(|e| io_err(format!("讀取 plugin.json 失敗: {e}")))?;
    // Claude Code plugin.json is JSON5-flavoured in the wild (comments); parse
    // leniently, falling back to strict JSON.
    let val: serde_json::Value = json5::from_str(&raw)
        .or_else(|_| serde_json::from_str(&raw))
        .map_err(|e| cfg_err(format!("plugin.json 解析失敗: {e}")))?;

    let name = val
        .get("name")
        .and_then(|v| v.as_str())
        .map(|s| crate::sanitize_agent_id(s))
        .filter(|s| crate::is_valid_agent_id(s))
        .ok_or_else(|| cfg_err("plugin.json 缺少合法 name".into()))?;

    if super::read_record(&ctx.home, &name).is_some() {
        return Err(cfg_err(format!(
            "專家包 '{name}' 已安裝（先 `expert remove {name}` 再重裝）"
        )));
    }

    let description = val
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let version = val
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0")
        .to_string();

    let mut record = InstallRecord {
        slug: name.clone(),
        kind: PackKind::ClaudePlugin,
        display_name: if description.is_empty() {
            name.clone()
        } else {
            description.clone()
        },
        version,
        description,
        agents: Vec::new(),
        global_skills: Vec::new(),
        wiki_files: Vec::new(),
        installed_at: now_iso(),
    };

    // Optional plugin `.mcp.json` (merged into every created agent).
    let plugin_mcp = super::read_mcp_servers(&dir.join(".mcp.json"));

    // ── agents/*.md ──
    let created_agents = import_agents(ctx, dir, report, &mut record, &plugin_mcp).await;

    // ── skills/<name>/ ──
    let skills_dir = dir.join("skills");
    if skills_dir.is_dir()
        && let Ok(rd) = std::fs::read_dir(&skills_dir)
    {
        for entry in rd.flatten() {
            if entry.path().is_dir() {
                let sname = entry.file_name().to_string_lossy().into_owned();
                install_skill_global(ctx, &entry.path(), &sname, report, &mut record);
            }
        }
    }

    // ── commands/*.md → single skill each ──
    import_commands(ctx, dir, report, &mut record);

    // ── hooks/ → quarantined; enablement is ApprovalBroker-gated
    //    (--trust-hooks, or approve + `expert hooks <slug>`) ──
    super::hooks::import_hooks(ctx, dir, &name, report).await;

    // ── ignored capabilities ──
    for key in [
        "lspServers",
        "themes",
        "monitors",
        "outputStyles",
        "statusLine",
    ] {
        if val.get(key).is_some() || dir.join(key).exists() {
            report.ignored("unsupported", key, "Claude Code 專屬，DuDuClaw 忽略");
        }
    }

    let _ = created_agents;
    Ok(record)
}

async fn import_agents(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
    record: &mut InstallRecord,
    plugin_mcp: &Option<serde_json::Map<String, serde_json::Value>>,
) -> usize {
    let agents_dir = dir.join("agents");
    let Ok(rd) = std::fs::read_dir(&agents_dir) else {
        return 0;
    };
    let mut entries: Vec<_> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("md"))
        .collect();
    entries.sort();

    let mut count = 0;
    for path in entries {
        let raw = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (fm, body) = parse_md_frontmatter(&raw);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("agent")
            .to_string();
        let raw_name = fm
            .as_ref()
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .unwrap_or(stem);
        let agent_id = crate::sanitize_agent_id(&raw_name);
        if !crate::is_valid_agent_id(&agent_id) {
            report.skipped("agent", &raw_name, "無法轉為合法 agent id");
            continue;
        }

        // Persona body (DATA) → scan.
        if let ScanVerdict::Blocked(reason) = scan_external(&body) {
            report.skipped("agent", &agent_id, format!("security: {reason}"));
            continue;
        }

        // Conflict resolution.
        let mut final_id = agent_id.clone();
        if ctx.home.join("agents").join(&final_id).exists() {
            if ctx.rename {
                final_id = crate::sanitize_agent_id(&format!("{agent_id}-imported"));
                if !crate::is_valid_agent_id(&final_id)
                    || ctx.home.join("agents").join(&final_id).exists()
                {
                    report.conflict("agent", &agent_id, "重新命名後仍衝突");
                    continue;
                }
            } else {
                report.conflict("agent", &agent_id, "同名 agent 已存在（--rename）");
                continue;
            }
        }

        // model / tools mapping.
        let (model_pref, needs_review) = fm
            .as_ref()
            .and_then(|v| v.get("model"))
            .and_then(|v| v.as_str())
            .map(map_model)
            .unwrap_or((None, false));
        if needs_review {
            report.warning(
                "agent-model",
                &final_id,
                format!(
                    "model '{}' 非 Claude，已保留待人工確認",
                    model_pref.clone().unwrap_or_default()
                ),
            );
        }
        let allowed = fm
            .as_ref()
            .and_then(|v| v.get("tools"))
            .map(parse_tool_list)
            .unwrap_or_default();
        let denied = fm
            .as_ref()
            .and_then(|v| v.get("disallowedTools"))
            .map(parse_tool_list)
            .unwrap_or_default();

        let display = fm
            .as_ref()
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str())
            .map(|s| duduclaw_core::truncate_chars(s, 40))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| raw_name.clone());

        if ctx.dry_run {
            report.imported("agent", &final_id);
            count += 1;
            continue;
        }

        let scaffold = crate::AgentScaffold {
            name: final_id.clone(),
            display_name: display,
            role: "worker".to_string(),
            reports_to: ctx.attach_under.clone().unwrap_or_default(),
            icon: "🐾".to_string(),
            trigger: format!("@{final_id}"),
            provider: duduclaw_core::types::RuntimeType::Claude,
            model_preferred: model_pref,
            soul_body: Some(body),
        };
        if let Err(e) = crate::scaffold_agent_dir(&ctx.home, &scaffold).await {
            report.skipped("agent", &final_id, format!("建立失敗: {e}"));
            continue;
        }
        report.imported("agent", &final_id);
        record.agents.push(final_id.clone());
        count += 1;

        // Capabilities patch.
        if !allowed.is_empty() || !denied.is_empty() {
            let _ = super::patch_agent_toml(&ctx.home, &final_id, |t| {
                set_capabilities(t, &allowed, &denied)
            });
            report.imported("agent-tools", &final_id);
        }

        // Merge plugin MCP servers into this agent's per-agent .mcp.json
        // (shared helper; duduclaw entry is never overwritten).
        if let Some(servers) = plugin_mcp
            && !servers.is_empty()
            && let Ok(written) = super::merge_agent_mcp(&ctx.home, &final_id, servers)
            && written > 0
        {
            report.imported("mcp-servers", &format!("{written} → {final_id}"));
        }
    }
    count
}

/// Convert `commands/*.md` into single global skills (Agent Skills shape).
fn import_commands(ctx: &InstallCtx, dir: &Path, report: &mut Report, record: &mut InstallRecord) {
    let cmd_dir = dir.join("commands");
    let Ok(rd) = std::fs::read_dir(&cmd_dir) else {
        return;
    };
    for entry in rd.flatten() {
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("md") {
            continue;
        }
        let raw = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let (fm, body) = parse_md_frontmatter(&raw);
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("command")
            .to_string();
        let name = fm
            .as_ref()
            .and_then(|v| v.get("name"))
            .and_then(|v| v.as_str())
            .map(crate::sanitize_agent_id)
            .filter(|s| duduclaw_agent::skill_loader::is_safe_skill_name(s))
            .unwrap_or_else(|| crate::sanitize_agent_id(&stem));
        if !duduclaw_agent::skill_loader::is_safe_skill_name(&name) {
            report.skipped("command", &stem, "無法轉為安全技能名稱");
            continue;
        }
        let desc = fm
            .as_ref()
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        // Assemble a spec-shaped SKILL.md.
        let skill_md = format!("---\nname: {name}\ndescription: {desc}\n---\n\n{body}\n");
        if let ScanVerdict::Blocked(reason) = scan_external(&skill_md) {
            report.skipped("command", &name, format!("security: {reason}"));
            continue;
        }
        let dest_dir = ctx.home.join("skills").join(&name);
        if dest_dir.exists() {
            report.conflict("command", &name, "全域已有同名技能，不覆蓋");
            continue;
        }
        if ctx.dry_run {
            report.imported("command", &name);
            continue;
        }
        if std::fs::create_dir_all(&dest_dir)
            .and_then(|_| std::fs::write(dest_dir.join("SKILL.md"), &skill_md))
            .is_ok()
        {
            report.imported("command", &name);
            record.global_skills.push(name);
        } else {
            report.skipped("command", &name, "寫入失敗");
        }
    }
}

// ─────────────────────────── frontmatter helpers ───────────────────────────

/// Split a Markdown doc into `(Some(yaml), body)` when it opens with a `---`
/// fence and has a closing `---`; otherwise `(None, whole)`.
fn parse_md_frontmatter(content: &str) -> (Option<serde_yaml::Value>, String) {
    let normalized = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = normalized.lines();
    if lines.next().map(str::trim_end) != Some("---") {
        return (None, content.to_string());
    }
    let mut yaml_lines: Vec<&str> = Vec::new();
    let mut body_lines: Vec<&str> = Vec::new();
    let mut in_body = false;
    for line in lines {
        if !in_body && line.trim_end() == "---" {
            in_body = true;
            continue;
        }
        if in_body {
            body_lines.push(line);
        } else {
            yaml_lines.push(line);
        }
    }
    if !in_body {
        return (None, content.to_string());
    }
    let yaml = serde_yaml::from_str::<serde_yaml::Value>(&yaml_lines.join("\n")).ok();
    (yaml, body_lines.join("\n").trim_start().to_string())
}

/// A Claude Code `tools` / `disallowedTools` field is either a comma-separated
/// string ("Read, Write, Bash") or a YAML list. Normalise to `Vec<String>`.
fn parse_tool_list(v: &serde_yaml::Value) -> Vec<String> {
    match v {
        serde_yaml::Value::String(s) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|x| x.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frontmatter_split() {
        let (fm, body) =
            parse_md_frontmatter("---\nname: helper\nmodel: sonnet\n---\n\nHello world");
        let fm = fm.unwrap();
        assert_eq!(fm.get("name").unwrap().as_str(), Some("helper"));
        assert_eq!(body, "Hello world");
    }

    #[test]
    fn frontmatter_absent() {
        let (fm, body) = parse_md_frontmatter("no frontmatter here");
        assert!(fm.is_none());
        assert_eq!(body, "no frontmatter here");
    }

    #[test]
    fn tool_list_string_and_seq() {
        let s = serde_yaml::Value::String("Read, Write , Bash".into());
        assert_eq!(parse_tool_list(&s), vec!["Read", "Write", "Bash"]);
        let seq: serde_yaml::Value = serde_yaml::from_str("- Read\n- Bash\n").unwrap();
        assert_eq!(parse_tool_list(&seq), vec!["Read", "Bash"]);
    }
}
