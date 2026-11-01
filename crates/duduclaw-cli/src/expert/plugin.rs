//! Claude Code plugin importer (`.claude-plugin/plugin.json`) — P0.
//!
//! Mapping (design `commercial/docs/specs/design-expert-pack.md` §2.2):
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
//! | `.mcp.json`                   | reported only; never imported             |
//! | `hooks/`                      | copied **disabled**, listed for approval  |
//! | lsp/themes/monitors/…         | ignored + reported                        |

use std::collections::{BTreeMap, BTreeSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use super::agent_config::ir::{AgentDraft, Doc, ImportBundle};
use super::install::InstallCtx;
use super::{InstallRecord, PackKind, Report, ScanVerdict, cfg_err, map_model, now_iso,
    scan_external, set_capabilities};
use duduclaw_core::error::Result;
use serde_json::{Value, json};

type Files = BTreeMap<String, Vec<u8>>;

struct PlannedAgent {
    scaffold: crate::AgentScaffold,
    profile: AgentDraft,
    files: Files,
}

struct PluginPlan {
    slug: String,
    description: String,
    version: String,
    agents: Vec<PlannedAgent>,
    skills: BTreeMap<String, Files>,
    hooks: Files,
    trust_hooks: bool,
}

impl PluginPlan {
    fn bundle(&self) -> ImportBundle {
        ImportBundle {
            format_code: "claude-plugin",
            agents: self.agents.iter().map(|a| a.profile.clone()).collect(),
            ..Default::default()
        }
    }

    fn effects(&self) -> Value {
        let agents: Vec<Value> = self.agents.iter().map(|a| json!({
            "id": a.scaffold.name, "name": a.scaffold.display_name,
            "parent": a.scaffold.reports_to, "trigger": a.scaffold.trigger,
            "model": a.profile.model, "tools": a.profile.tools,
            "disallowed_tools": a.profile.disallowed_tools, "settings": a.profile.settings,
            "files": file_hashes(&format!("agents/{}", a.scaffold.name), &a.files),
        })).collect();
        let skills: Vec<Value> = self.skills.iter().map(|(name, files)| json!({
            "name": name, "files": file_hashes(&format!("skills/{name}"), files),
        })).collect();
        json!({"schema": "claude-plugin-install/v1", "slug": self.slug,
            "description": self.description, "version": self.version,
            "agents": agents, "skills": skills,
            "hooks": {"trust": self.trust_hooks,
                "files": file_hashes(&format!("experts/{}/hooks-disabled", self.slug), &self.hooks),
                "enabled_files": if self.trust_hooks {
                    file_hashes(&format!("experts/{}/hooks", self.slug), &self.hooks)
                } else { Vec::new() },
                "approval": !self.trust_hooks && !self.hooks.is_empty()}})
    }
}

fn file_hashes(prefix: &str, files: &Files) -> Vec<Value> {
    files.iter().map(|(path, bytes)| json!({
        "path": format!("{prefix}/{path}"),
        "sha256": super::registry::sha256_hex(bytes),
    })).collect()
}

pub(super) async fn install(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    // The plan owns every input byte. Confirmation never reopens the source.
    let plan = plan_plugin(ctx, dir, report)?;
    let effects = super::agent_config::preview::bind_context(ctx, plan.effects())?;
    super::agent_config::preview::require_plan_confirmation(
        &plan.bundle(), effects, ctx.dry_run, ctx.confirm_profile.as_deref(), report,
    )?;
    ctx.ensure_quota(plan.agents.len()).await?;
    ctx.begin_writes()?;
    if !ctx.dry_run { refuse_existing_hooks(ctx, &plan, false)?; }
    let mut record = InstallRecord {
        slug: plan.slug.clone(), kind: PackKind::ClaudePlugin,
        display_name: if plan.description.is_empty() { plan.slug.clone() } else { plan.description.clone() },
        version: plan.version.clone(), description: plan.description.clone(),
        agents: Vec::new(), global_skills: Vec::new(), wiki_files: Vec::new(),
        agent_skills: Vec::new(), agent_ids: Default::default(),
        installed_at: now_iso(), source_format: String::new(),
    };
    for agent in &plan.agents {
        if ctx.stopping() { return Err(super::install::interrupted()); }
        let id = &agent.scaffold.name;
        if !ctx.dry_run {
            ctx.note_agent(id);
            crate::scaffold_planned_agent_dir(&ctx.home, &agent.scaffold, &agent.files).await?;
        }
        report.imported("agent", id);
        record.agents.push(id.clone());
    }
    for (name, files) in &plan.skills {
        if ctx.stopping() { return Err(super::install::interrupted()); }
        if !ctx.dry_run {
            let dest = ctx.home.join("skills").join(name);
            ctx.note_path(&dest);
            write_new_tree(&dest, files)?;
        }
        report.imported("skill", name);
        record.global_skills.push(name.clone());
    }
    if !plan.hooks.is_empty() {
        if ctx.stopping() { return Err(super::install::interrupted()); }
        // The lifecycle only consumes this private copy of the confirmed bytes.
        // Approval ids and audit timestamps are allocated during application.
        let stage = tempfile::tempdir().map_err(|e| cfg_err(format!("建立 hooks 暫存失敗：{e}")))?;
        write_new_tree(&stage.path().join("hooks"), &plan.hooks)?;
        super::hooks::import_hooks(ctx, stage.path(), &plan.slug, report).await;
    }
    Ok(record)
}

fn plan_plugin(ctx: &InstallCtx, dir: &Path, report: &mut Report) -> Result<PluginPlan> {
    let mut total = 0;
    if !optional_dir(dir)? || !optional_dir(&dir.join(".claude-plugin"))? {
        return Err(cfg_err("缺少外掛來源目錄".into()));
    }
    let raw = read_file(&dir.join(".claude-plugin/plugin.json"), &mut total)?;
    let raw = std::str::from_utf8(&raw).map_err(|_| cfg_err("plugin.json 必須是 UTF-8".into()))?;
    let val: Value = json5::from_str(raw).or_else(|_| serde_json::from_str(raw))
        .map_err(|e| super::import_error::bad_content(format!("plugin.json 解析失敗: {e}")))?;
    let name = val.get("name").and_then(Value::as_str).map(crate::sanitize_agent_id)
        .filter(|s| crate::is_valid_agent_id(s))
        .ok_or_else(|| super::import_error::bad_content("plugin.json 缺少合法 name".into()))?;
    let mut plan = PluginPlan {
        slug: ctx.claim_slug(&name)?,
        description: val.get("description").and_then(Value::as_str).unwrap_or("").into(),
        version: val.get("version").and_then(Value::as_str).unwrap_or("0.0.0").into(),
        agents: Vec::new(), skills: BTreeMap::new(), hooks: Files::new(), trust_hooks: ctx.trust_hooks,
    };
    let mut book = ctx.name_book();
    let mut ids = BTreeSet::new();
    for path in entries(&dir.join("agents"))? {
        if path.extension().and_then(|v| v.to_str()) != Some("md") { continue; }
        let raw = read_file(&path, &mut total)?;
        let raw = std::str::from_utf8(&raw).map_err(|_| cfg_err("外掛員工定義必須是 UTF-8".into()))?;
        let (fm, body) = checked_frontmatter(raw)?;
        let raw_name = fm.as_ref().and_then(|v| v.get("name")).and_then(|v| v.as_str())
            .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("agent"));
        let base = crate::sanitize_agent_id(raw_name);
        if !crate::is_valid_agent_id(&base) || duduclaw_core::is_reserved_agent_id(&base) {
            return Err(cfg_err("外掛員工代號無效".into()));
        }
        let mut id = base.clone();
        if ctx.agent_taken(&id) || ids.contains(&id) || book.is_taken(&id) {
            if !ctx.rename { return Err(cfg_err(format!("員工代號衝突：{id}"))); }
            id = crate::sanitize_agent_id(&format!("{base}-imported"));
            if !crate::is_valid_agent_id(&id) || ctx.agent_taken(&id) || ids.contains(&id) || book.is_taken(&id) {
                return Err(cfg_err(format!("重新命名後仍衝突：{id}")));
            }
        }
        ids.insert(id.clone());
        let description = fm.as_ref().and_then(|v| v.get("description")).and_then(|v| v.as_str()).unwrap_or("");
        let wanted = if description.is_empty() { raw_name.to_string() } else { duduclaw_core::truncate_chars(description, 40) };
        let display = [wanted.clone(), format!("{wanted}（{id}）"), format!("{wanted}-{id}")]
            .into_iter().find(|s| duduclaw_core::agent_names::norm(s) == duduclaw_core::agent_names::norm(&id) || !book.is_taken(s))
            .ok_or_else(|| cfg_err("外掛員工名稱衝突，請重新命名".into()))?;
        book.reserve(&id);
        book.reserve(&display);
        let model_raw = fm.as_ref().and_then(|v| v.get("model"));
        if model_raw.is_some_and(|v| !v.is_string()) { return Err(cfg_err("model 必須是字串".into())); }
        let (model, review) = model_raw.and_then(|v| v.as_str()).map(map_model).unwrap_or((None, false));
        if review { report.warning("agent-model", &id, "非 Claude 模型，請確認模型設定"); }
        let allowed = checked_tools(fm.as_ref().and_then(|v| v.get("tools")))?;
        let denied = checked_tools(fm.as_ref().and_then(|v| v.get("disallowedTools")))?;
        let scaffold = crate::AgentScaffold {
            name: id.clone(), display_name: display.clone(), role: "worker".into(),
            reports_to: ctx.attach_under.clone().unwrap_or_default(), icon: "🐾".into(),
            trigger: format!("@{id}"), provider: duduclaw_core::types::RuntimeType::Claude,
            model_preferred: model.clone(), soul_body: Some(body.clone()),
        };
        let mut files = crate::scaffold_agent_files(&scaffold)?;
        let config = files.get("agent.toml").ok_or_else(|| cfg_err("缺少員工設定".into()))?;
        let mut settings: toml::Table = std::str::from_utf8(config)
            .map_err(|e| cfg_err(e.to_string()))?.parse().map_err(|e| cfg_err(format!("員工設定格式不正確：{e}")))?;
        set_capabilities(&mut settings, &allowed, &denied);
        let _: duduclaw_core::types::AgentConfig = toml::Value::Table(settings.clone()).try_into()
            .map_err(|e| cfg_err(format!("員工設定型別不正確：{e}")))?;
        files.insert("agent.toml".into(), toml::to_string_pretty(&settings)
            .map_err(|e| cfg_err(e.to_string()))?.into_bytes());
        let model = settings.get("model").and_then(|v| v.get("preferred"))
            .and_then(toml::Value::as_str).map(str::to_string);
        let profile = AgentDraft {
            ref_id: id.clone(), id_hint: id, display_name: display, description: description.into(),
            soul: Some(Doc::new("plugin-agent", body)), settings, model,
            tools: Some(allowed), disallowed_tools: denied,
            reports_to: Some(scaffold.reports_to.clone()), ..Default::default()
        };
        plan.agents.push(PlannedAgent { scaffold, profile, files });
    }
    for path in entries(&dir.join("skills"))? {
        let name = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| cfg_err("技能名稱必須是 UTF-8".into()))?;
        if !duduclaw_agent::skill_loader::is_safe_skill_name(name) { return Err(cfg_err("非安全技能名稱".into())); }
        let mut files = Files::new();
        read_tree(&path, &path, &mut files, &mut total, 0)?;
        let body = files.get("SKILL.md").ok_or_else(|| cfg_err("技能缺少 SKILL.md".into()))?;
        scan_text(body)?;
        if ctx.home.join("skills").join(name).exists() {
            report.conflict("skill", name, "全域已有同名技能，不覆蓋");
        } else {
            plan.skills.insert(name.into(), files);
        }
    }
    for path in entries(&dir.join("commands"))? {
        if path.extension().and_then(|v| v.to_str()) != Some("md") { continue; }
        let raw = read_file(&path, &mut total)?;
        let raw = std::str::from_utf8(&raw).map_err(|_| cfg_err("command 必須是 UTF-8".into()))?;
        let (fm, body) = checked_frontmatter(raw)?;
        let raw_name = fm.as_ref().and_then(|v| v.get("name")).and_then(|v| v.as_str())
            .unwrap_or_else(|| path.file_stem().and_then(|s| s.to_str()).unwrap_or("command"));
        let name = crate::sanitize_agent_id(raw_name);
        if !duduclaw_agent::skill_loader::is_safe_skill_name(&name) { return Err(cfg_err("command 技能名稱無效".into())); }
        let desc = fm.as_ref().and_then(|v| v.get("description")).and_then(|v| v.as_str()).unwrap_or("");
        let skill = format!("---\nname: {name}\ndescription: {desc}\n---\n\n{body}\n").into_bytes();
        scan_text(&skill)?;
        if ctx.home.join("skills").join(&name).exists() || plan.skills.contains_key(&name) {
            report.conflict("command", &name, "全域已有同名技能，不覆蓋");
        } else {
            plan.skills.insert(name, [("SKILL.md".into(), skill)].into_iter().collect());
        }
    }
    let hooks = dir.join("hooks");
    if optional_dir(&hooks)? { read_tree(&hooks, &hooks, &mut plan.hooks, &mut total, 0)?; }
    if let Some(servers) = super::read_mcp_servers(&dir.join(".mcp.json")) {
        for name in servers.keys() { report.skipped("mcp-servers", name, "匯入不新增 MCP 伺服器，請由管理者另行設定"); }
    }
    for key in ["lspServers", "themes", "monitors", "outputStyles", "statusLine"] {
        if val.get(key).is_some() || dir.join(key).exists() {
            report.ignored("unsupported", key, "Claude Code 專屬，DuDuClaw 忽略");
        }
    }
    refuse_existing_hooks(ctx, &plan, true)?;
    Ok(plan)
}

fn refuse_existing_hooks(ctx: &InstallCtx, plan: &PluginPlan, before_replace: bool) -> Result<()> {
    if plan.hooks.is_empty() || (before_replace && ctx.replace.as_ref().is_some_and(|r| r.slug == plan.slug)) {
        return Ok(());
    }
    for name in ["hooks", "hooks-disabled", "hooks-state.json"] {
        match std::fs::symlink_metadata(ctx.home.join("experts").join(&plan.slug).join(name)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            _ => return Err(cfg_err("hooks 目的地已存在或無法確認，請重新預覽".into())),
        }
    }
    Ok(())
}

fn checked_frontmatter(raw: &str) -> Result<(Option<serde_yaml::Value>, String)> {
    let parsed = parse_md_frontmatter(raw);
    if raw.trim_start_matches('\u{feff}').starts_with("---") && parsed.0.is_none() {
        return Err(cfg_err("外掛 frontmatter 格式不正確".into()));
    }
    Ok(parsed)
}

fn checked_tools(value: Option<&serde_yaml::Value>) -> Result<Vec<String>> {
    let Some(value) = value else { return Ok(Vec::new()); };
    if !value.is_string() && !value.as_sequence().is_some_and(|v| v.iter().all(|t| t.is_string())) {
        return Err(cfg_err("工具限制必須是字串或字串清單".into()));
    }
    Ok(parse_tool_list(value))
}

fn scan_text(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| cfg_err("技能定義必須是 UTF-8".into()))?;
    if let ScanVerdict::Blocked(reason) = scan_external(text) {
        return Err(cfg_err(format!("技能未通過安全掃描：{reason}")));
    }
    Ok(())
}

fn optional_dir(path: &Path) -> Result<bool> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Ok(meta) if meta.is_dir() => Ok(true),
        Ok(_) => Err(cfg_err("外掛來源目錄不得是連結或特殊檔案".into())),
        Err(e) => Err(cfg_err(format!("無法讀取外掛目錄：{e}"))),
    }
}

fn entries(path: &Path) -> Result<Vec<PathBuf>> {
    if !optional_dir(path)? { return Ok(Vec::new()); }
    let mut out = std::fs::read_dir(path).map_err(|e| cfg_err(e.to_string()))?
        .map(|entry| entry.map(|e| e.path())).collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| cfg_err(e.to_string()))?;
    out.sort();
    Ok(out)
}

fn read_file(path: &Path, total: &mut u64) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| cfg_err(e.to_string()))?;
    if !meta.is_file() { return Err(cfg_err("外掛來源不得是連結或特殊檔案".into())); }
    let remaining = super::safe_zip::MAX_UNPACK_BYTES.saturating_sub(*total);
    if meta.len() > remaining { return Err(cfg_err("外掛內容超過大小限制".into())); }
    let mut bytes = Vec::new();
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| cfg_err(e.to_string()))?;
    if !file.metadata().map_err(|e| cfg_err(e.to_string()))?.is_file() {
        return Err(cfg_err("外掛來源必須是一般檔案".into()));
    }
    file.take(remaining + 1).read_to_end(&mut bytes).map_err(|e| cfg_err(e.to_string()))?;
    *total = total.saturating_add(bytes.len() as u64);
    if *total > super::safe_zip::MAX_UNPACK_BYTES { return Err(cfg_err("外掛內容超過大小限制".into())); }
    Ok(bytes)
}

fn read_tree(base: &Path, dir: &Path, files: &mut Files, total: &mut u64, depth: usize) -> Result<()> {
    if depth > 32 || files.len() > 10_000 { return Err(cfg_err("外掛檔案數或目錄深度超過限制".into())); }
    for path in entries(dir)? {
        let meta = std::fs::symlink_metadata(&path).map_err(|e| cfg_err(e.to_string()))?;
        if meta.is_dir() {
            read_tree(base, &path, files, total, depth + 1)?;
        } else {
            let relative = path.strip_prefix(base).map_err(|e| cfg_err(e.to_string()))?;
            let mut parts = Vec::new();
            for part in relative.components() {
                let std::path::Component::Normal(part) = part else {
                    return Err(cfg_err("外掛路徑無效".into()));
                };
                let part = part.to_str().ok_or_else(|| cfg_err("外掛路徑必須是 UTF-8".into()))?;
                if part.contains('\\') { return Err(cfg_err("外掛路徑不可包含反斜線".into())); }
                parts.push(part);
            }
            let rel = parts.join("/");
            if files.len() >= 10_000 { return Err(cfg_err("外掛檔案數超過限制".into())); }
            files.insert(rel, read_file(&path, total)?);
        }
    }
    Ok(())
}

fn write_new_tree(dest: &Path, files: &Files) -> Result<()> {
    if let Some(parent) = dest.parent() { std::fs::create_dir_all(parent).map_err(|e| cfg_err(e.to_string()))?; }
    std::fs::create_dir(dest).map_err(|e| cfg_err(format!("目的地已存在或無法建立：{e}")))?;
    for (rel, bytes) in files {
        let path = dest.join(rel);
        if let Some(parent) = path.parent() { std::fs::create_dir_all(parent).map_err(|e| cfg_err(e.to_string()))?; }
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)
            .map_err(|e| cfg_err(e.to_string()))?;
        file.write_all(bytes).map_err(|e| cfg_err(e.to_string()))?;
    }
    Ok(())
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

    fn ctx(home: &Path) -> InstallCtx {
        InstallCtx {
            home: home.to_path_buf(), dry_run: true, confirm_profile: None,
            rename: false, trust_hooks: false, attach_under: None,
            reimport: super::super::reimport::ReimportMode::Fresh, replace: None,
            agent_cap: None, replace_state: Default::default(),
        }
    }

    fn source(dir: &Path, frontmatter: &str) {
        std::fs::create_dir_all(dir.join(".claude-plugin")).unwrap();
        std::fs::create_dir_all(dir.join("agents")).unwrap();
        std::fs::write(dir.join(".claude-plugin/plugin.json"), r#"{"name":"review-plugin"}"#).unwrap();
        std::fs::write(dir.join("agents/helper.md"), format!("---\nname: helper\n{frontmatter}\n---\nHelp with the task.")).unwrap();
    }

    fn plan_digest(ctx: &InstallCtx, plan: &PluginPlan) -> String {
        let mut report = Report::default();
        super::super::agent_config::preview::require_plan_confirmation(
            &plan.bundle(), super::super::agent_config::preview::bind_context(ctx, plan.effects()).unwrap(),
            true, None, &mut report,
        ).unwrap();
        report.to_json(true)["details"]["profile_confirmation"].as_str().unwrap().into()
    }

    #[test]
    fn plugin_confirmation_binds_model_tools_denials_and_parent() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut ctx = ctx(home.path());
        source(src.path(), "model: sonnet\ntools: Read\ndisallowedTools: Bash");
        let original = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
        let digest = plan_digest(&ctx, &original);
        for fields in [
            "model: opus\ntools: Read\ndisallowedTools: Bash",
            "model: sonnet\ntools: Read, Bash\ndisallowedTools: Bash",
            "model: sonnet\ntools: Read\ndisallowedTools: []",
        ] {
            source(src.path(), fields);
            let changed = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
            assert_ne!(digest, plan_digest(&ctx, &changed));
        }
        source(src.path(), "model: sonnet\ntools: Read\ndisallowedTools: Bash");
        ctx.attach_under = Some("supervisor".into());
        assert_ne!(digest, plan_digest(&ctx, &plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap()));
        let config = std::str::from_utf8(&original.agents[0].files["agent.toml"]).unwrap();
        let table: toml::Table = config.parse().unwrap();
        assert_eq!(table["capabilities"]["denied_tools"][0].as_str(), Some("Bash"));
    }

    #[test]
    fn plugin_confirmation_binds_resolved_collisions_and_skill_bytes() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut ctx = ctx(home.path());
        ctx.rename = true;
        source(src.path(), "model: sonnet");
        std::fs::create_dir_all(src.path().join("skills/guide")).unwrap();
        std::fs::write(src.path().join("skills/guide/SKILL.md"), "# Guide\nRead the guide.").unwrap();
        let original = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
        let digest = plan_digest(&ctx, &original);
        std::fs::create_dir_all(home.path().join("agents/helper")).unwrap();
        let collision = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
        assert_eq!(collision.agents[0].profile.id_hint, "helper-imported");
        assert_ne!(digest, plan_digest(&ctx, &collision));
        let before_skill = plan_digest(&ctx, &collision);
        std::fs::write(src.path().join("skills/guide/SKILL.md"), "# Guide\nCheck the guide twice.").unwrap();
        let changed = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
        assert_ne!(before_skill, plan_digest(&ctx, &changed));
        let dest = home.path().join("snapshot");
        write_new_tree(&dest, &original.skills["guide"]).unwrap();
        assert_eq!(std::fs::read_to_string(dest.join("SKILL.md")).unwrap(), "# Guide\nRead the guide.");
    }

    #[test]
    fn plugin_rejects_malformed_tool_restrictions() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        for field in ["tools: 42", "disallowedTools: [Bash, 42]", "tools: [Read"] {
            source(src.path(), field);
            assert!(plan_plugin(&ctx(home.path()), src.path(), &mut Report::default()).is_err());
        }
    }


    #[tokio::test]
    async fn old_plugin_confirmation_cannot_remove_a_bash_denial() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut ctx = ctx(home.path());
        source(src.path(), "disallowedTools: Bash");
        let plan = plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap();
        ctx.confirm_profile = Some(plan_digest(&ctx, &plan));
        ctx.dry_run = false;
        super::super::agent_config::preview::require_plan_confirmation(
            &plan.bundle(), super::super::agent_config::preview::bind_context(&ctx, plan.effects()).unwrap(),
            false, ctx.confirm_profile.as_deref(), &mut Report::default(),
        ).unwrap();
        source(src.path(), "disallowedTools: []");
        let error = install(&ctx, src.path(), &mut Report::default()).await.unwrap_err();
        assert!(error.to_string().contains("重新預覽"), "{error}");
        assert!(!home.path().join("agents/helper").exists());
    }

    #[test]
    fn plugin_confirmation_binds_commands_hooks_and_hook_grants() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut ctx = ctx(home.path());
        source(src.path(), "");
        std::fs::create_dir_all(src.path().join("commands")).unwrap();
        std::fs::create_dir_all(src.path().join("hooks")).unwrap();
        std::fs::write(src.path().join("commands/help.md"), "Explain this task.").unwrap();
        std::fs::write(src.path().join("hooks/hooks.json"), "{}").unwrap();
        let initial = plan_digest(&ctx, &plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap());
        std::fs::write(src.path().join("commands/help.md"), "Explain the completed task.").unwrap();
        let command = plan_digest(&ctx, &plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap());
        assert_ne!(initial, command);
        std::fs::write(src.path().join("hooks/hooks.json"), "{\"hooks\": []}").unwrap();
        let hooks = plan_digest(&ctx, &plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap());
        assert_ne!(command, hooks);
        ctx.trust_hooks = true;
        assert_ne!(hooks, plan_digest(&ctx, &plan_plugin(&ctx, src.path(), &mut Report::default()).unwrap()));
    }


    #[cfg(unix)]
    #[test]
    fn plugin_rejects_links_and_ambiguous_attachment_paths() {
        let home = tempfile::tempdir().unwrap();
        let src = tempfile::tempdir().unwrap();
        source(src.path(), "");
        let skill = src.path().join("skills/guide");
        std::fs::create_dir_all(&skill).unwrap();
        std::fs::write(skill.join("SKILL.md"), "Read the guide.").unwrap();
        std::os::unix::fs::symlink("SKILL.md", skill.join("link.md")).unwrap();
        assert!(plan_plugin(&ctx(home.path()), src.path(), &mut Report::default()).is_err());
        std::fs::remove_file(skill.join("link.md")).unwrap();
        std::fs::write(skill.join("..\\escape"), "data").unwrap();
        assert!(plan_plugin(&ctx(home.path()), src.path(), &mut Report::default()).is_err());
    }

}
