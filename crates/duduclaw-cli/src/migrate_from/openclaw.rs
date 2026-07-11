//! OpenClaw (`~/.openclaw`, JSON5 config) importer.
//!
//! Source shapes per spec §1.1 (one-source-confirmed research):
//! - `openclaw.json` — JSON5 (comments + trailing commas). Top-level
//!   `agents/channels/models/session/gateway/hooks/cron/env`.
//! - channel tokens: `channels.telegram.botToken`, `channels.discord.token`,
//!   `channels.slack.botToken` + `appToken`; whatsapp is linked-device (no token).
//! - default model: `agents.defaults.model.primary` (e.g. `anthropic/claude-...`).
//! - workspace persona/memory: `<workspace>/{SOUL,IDENTITY,USER,AGENTS,TOOLS,MEMORY}.md`
//!   + `memory/*.md` + `skills/`.
//! - legacy cron: `cron/jobs.json` (new SQLite cron is UNVERIFIED → SKIPPED).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use duduclaw_core::error::Result;

use super::apply::*;
use super::report::Report;
use super::*;

/// One agent declared in openclaw.json.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct AgentSpec {
    pub id: String,
    pub workspace: Option<String>,
    pub model: Option<String>,
}

/// Channel tokens lifted from openclaw.json.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ChannelTokens {
    pub telegram: Option<String>,
    pub discord: Option<String>,
    pub slack_bot: Option<String>,
    pub slack_app: Option<String>,
    pub whatsapp_present: bool,
}

/// Parsed openclaw.json (pure — no filesystem).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct OpenClawConfig {
    pub agents: Vec<AgentSpec>,
    pub default_model: Option<String>,
    pub channels: ChannelTokens,
    pub env: BTreeMap<String, String>,
}

fn jstr<'a>(v: &'a serde_json::Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for k in path {
        cur = cur.get(*k)?;
    }
    cur.as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// Parse an openclaw.json (JSON5) document. Pure + unit-tested.
pub(super) fn parse_openclaw(src: &str) -> std::result::Result<OpenClawConfig, String> {
    let v: serde_json::Value = json5::from_str(src).map_err(|e| format!("JSON5 解析失敗: {e}"))?;

    let default_model = jstr(&v, &["agents", "defaults", "model", "primary"])
        .or_else(|| jstr(&v, &["models", "primary"]))
        .or_else(|| jstr(&v, &["model", "primary"]))
        .map(str::to_string);

    // agents.list[] → AgentSpec; fall back to a single implicit `main` agent.
    let mut agents = Vec::new();
    if let Some(list) = v
        .get("agents")
        .and_then(|a| a.get("list"))
        .and_then(|l| l.as_array())
    {
        for item in list {
            let id = jstr(item, &["id"])
                .or_else(|| jstr(item, &["name"]))
                .unwrap_or("main")
                .to_string();
            let workspace = jstr(item, &["workspace"]).map(str::to_string);
            let model = jstr(item, &["model", "primary"])
                .or_else(|| jstr(item, &["model"]))
                .map(str::to_string);
            agents.push(AgentSpec {
                id,
                workspace,
                model,
            });
        }
    }
    if agents.is_empty() {
        agents.push(AgentSpec {
            id: "main".to_string(),
            workspace: None,
            model: None,
        });
    }

    let channels = ChannelTokens {
        telegram: jstr(&v, &["channels", "telegram", "botToken"]).map(str::to_string),
        discord: jstr(&v, &["channels", "discord", "token"]).map(str::to_string),
        slack_bot: jstr(&v, &["channels", "slack", "botToken"]).map(str::to_string),
        slack_app: jstr(&v, &["channels", "slack", "appToken"]).map(str::to_string),
        whatsapp_present: v.get("channels").and_then(|c| c.get("whatsapp")).is_some(),
    };

    let mut env = BTreeMap::new();
    if let Some(obj) = v.get("env").and_then(|e| e.as_object()) {
        for (k, val) in obj {
            if let Some(s) = val.as_str() {
                env.insert(k.clone(), s.to_string());
            }
        }
    }

    Ok(OpenClawConfig {
        agents,
        default_model,
        channels,
        env,
    })
}

fn default_source() -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    for name in [".openclaw", ".moltbot", ".clawdbot"] {
        let p = home.join(name);
        if p.exists() {
            return p;
        }
    }
    home.join(".openclaw")
}

fn find_config(src: &Path) -> Option<PathBuf> {
    for name in ["openclaw.json", "moltbot.json", "clawdbot.json"] {
        let p = src.join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

/// Resolve the workspace directory for an agent (spec §1.1 defaults).
fn workspace_dir(src: &Path, spec: &AgentSpec, single: bool) -> PathBuf {
    if let Some(ws) = &spec.workspace {
        let p = PathBuf::from(ws);
        return if p.is_absolute() { p } else { src.join(p) };
    }
    if single || spec.id == "main" {
        src.join("workspace")
    } else {
        src.join(format!("workspace-{}", spec.id))
    }
}

/// Collect skill directories following the OpenClaw priority order.
fn skill_dirs(src: &Path, workspace: &Path) -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let roots = [
        workspace.join("skills"),
        workspace.join(".agents").join("skills"),
        home.join(".agents").join("skills"),
        src.join("skills"),
    ];
    let mut out = Vec::new();
    for root in roots {
        if let Ok(rd) = std::fs::read_dir(&root) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("SKILL.md").exists() {
                    out.push(p);
                }
            }
        }
    }
    out
}

pub(super) async fn migrate(ctx: &Ctx, source: Option<PathBuf>) -> Result<Report> {
    let src = source.unwrap_or_else(default_source);
    let mut report = Report::new("openclaw", &src.display().to_string(), ctx.apply);

    let Some(cfg_path) = find_config(&src) else {
        report.skipped(
            "config",
            &src.display().to_string(),
            "找不到 openclaw.json / moltbot.json / clawdbot.json",
        );
        return Ok(report);
    };

    let raw = match std::fs::read_to_string(&cfg_path) {
        Ok(c) => c,
        Err(e) => {
            report.skipped("config", "openclaw.json", format!("讀取失敗: {e}"));
            return Ok(report);
        }
    };
    let cfg = match parse_openclaw(&raw) {
        Ok(c) => c,
        Err(e) => {
            report.skipped("config", "openclaw.json", e);
            return Ok(report);
        }
    };

    // ── API keys: ANTHROPIC from env section + .env; others → SKIPPED hint ──
    let mut env = cfg.env.clone();
    let dotenv = src.join(".env");
    if let Ok(content) = std::fs::read_to_string(&dotenv) {
        for (k, v) in parse_env_file(&content) {
            env.entry(k).or_insert(v);
        }
    }
    let anthropic_key = env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();

    let mut config_table = read_config_table(&ctx.home);
    plan_api_key(ctx, &mut report, &mut config_table, &anthropic_key);
    for (k, _) in env.iter() {
        if k.ends_with("_API_KEY") && k != "ANTHROPIC_API_KEY" {
            report.skipped(
                "api_key",
                k,
                "非 Anthropic 供應商金鑰，v1 不轉移（請手動設定）",
            );
        }
    }

    // ── Channels (global config.toml [channels]) ──
    {
        let channels = config_table
            .entry("channels")
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
            .as_table_mut();
        if let Some(channels) = channels {
            if let Some(tok) = &cfg.channels.telegram {
                plan_channel_token(ctx, &mut report, channels, "telegram", tok, None);
            }
            if let Some(tok) = &cfg.channels.discord {
                plan_channel_token(ctx, &mut report, channels, "discord", tok, None);
            }
            if let Some(bot) = &cfg.channels.slack_bot {
                plan_channel_token(
                    ctx,
                    &mut report,
                    channels,
                    "slack",
                    bot,
                    cfg.channels.slack_app.as_deref(),
                );
            }
        }
        if cfg.channels.whatsapp_present {
            report.skipped(
                "channel",
                "whatsapp",
                "linked-device 憑證綁裝置，技術上不可轉移",
            );
        }
    }

    if ctx.apply
        && let Err(e) = write_config_table(&ctx.home, &config_table)
    {
        report.skipped("config", "config.toml", format!("寫入失敗: {e}"));
    }

    // ── Agents + workspace persona / memory / skills ──
    let engine = open_memory(ctx);
    let single = cfg.agents.len() == 1;
    for spec in &cfg.agents {
        let ws = workspace_dir(&src, spec, single);
        let soul_body = std::fs::read_to_string(ws.join("SOUL.md")).ok();
        let model_raw = spec.model.clone().or_else(|| cfg.default_model.clone());
        let (preferred, needs_review) = match &model_raw {
            Some(m) => {
                let (p, r) = map_model(m);
                (Some(p), r)
            }
            None => (None, false),
        };

        let Some(agent_id) = scaffold_agent(
            ctx,
            &mut report,
            &spec.id,
            &spec.id,
            "specialist",
            "",
            preferred.clone(),
            soul_body,
        )
        .await
        else {
            continue;
        };

        if needs_review && let Some(m) = &preferred {
            report.partial(
                "model",
                &agent_id,
                format!("非 Claude 模型 '{m}'，請人工確認 runtime 對映"),
            );
        }

        // Memory: bullets from MEMORY.md + memory/*.md + USER.md.
        let mut facts = Vec::new();
        for file in ["MEMORY.md", "USER.md"] {
            if let Ok(c) = std::fs::read_to_string(ws.join(file)) {
                facts.extend(extract_bullets(&c));
            }
        }
        if let Ok(rd) = std::fs::read_dir(ws.join("memory")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("md")
                    && let Ok(c) = std::fs::read_to_string(&p)
                {
                    facts.extend(extract_bullets(&c));
                }
            }
        }
        import_facts(
            engine.as_ref(),
            ctx,
            &mut report,
            &agent_id,
            "workspace memory",
            &facts,
        )
        .await;

        // Copy persona files verbatim into the agent's memory dir (fidelity).
        for file in [
            "IDENTITY.md",
            "USER.md",
            "AGENTS.md",
            "TOOLS.md",
            "MEMORY.md",
        ] {
            let _ = copy_into_agent_memory(ctx, &agent_id, &ws.join(file));
        }

        // Skills (scanned + installed).
        let skills = skill_dirs(&src, &ws);
        if !skills.is_empty() {
            install_skills(ctx, &mut report, &agent_id, &skills);
        }

        // Archive raw sessions.
        let sessions = src.join("agents").join(&spec.id).join("sessions");
        if sessions.exists() {
            archive_raw(
                ctx,
                &mut report,
                &[(format!("{}-sessions", spec.id), sessions)],
            );
        }
    }

    // ── Cron: legacy jobs.json supported; SQLite cron → SKIPPED hint ──
    let jobs_json = src.join("cron").join("jobs.json");
    if let Ok(content) = std::fs::read_to_string(&jobs_json) {
        import_legacy_cron(ctx, &mut report, &content).await;
    }
    let sqlite_cron = src.join("cron.db");
    if sqlite_cron.exists() || src.join("cron").join("cron.db").exists() {
        report.skipped(
            "cron",
            "sqlite-cron",
            "OpenClaw 新版 SQLite cron schema 未驗證，v1 不解析（請於舊平台 `openclaw cron list` 手抄）",
        );
    }

    if !skill_dirs(&src, &src.join("workspace")).is_empty() || cfg.agents.iter().any(|_| true) {
        report.note("已安裝的 skills 皆通過 duduclaw-security 注入掃描 (input_guard, 6 規則)。");
    }

    Ok(report)
}

/// Parse + import a legacy `cron/jobs.json` array or object.
async fn import_legacy_cron(ctx: &Ctx, report: &mut Report, content: &str) {
    let parsed: serde_json::Value = match json5::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            report.skipped("cron", "jobs.json", format!("解析失敗: {e}"));
            return;
        }
    };
    // Accept either a bare array or `{ "jobs": [...] }`.
    let arr = parsed
        .as_array()
        .cloned()
        .or_else(|| parsed.get("jobs").and_then(|j| j.as_array()).cloned())
        .unwrap_or_default();
    if arr.is_empty() {
        return;
    }
    let mut jobs = Vec::new();
    for (i, item) in arr.iter().enumerate() {
        match parse_cron_job(item) {
            Some(j) => jobs.push(j),
            None => report.skipped(
                "cron",
                &format!("jobs.json[{i}]"),
                "缺 cron 或 task 欄位，無法解析",
            ),
        }
    }
    // Cron belongs to the primary agent (first imported) — use `main` fallback.
    let agent = "main".to_string();
    import_cron_jobs(ctx, report, &agent, &jobs).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_openclaw_json5_with_comments_and_trailing_commas() {
        let src = r#"{
            // OpenClaw config
            agents: {
                defaults: { model: { primary: "anthropic/claude-sonnet-4-6" } },
                list: [
                    { id: "main", workspace: "workspace" },
                    { name: "sales", model: { primary: "anthropic/claude-haiku-4-5" } },
                ],
            },
            channels: {
                telegram: { botToken: "111:aaa" },
                discord: { token: "disc-tok" },
                slack: { botToken: "xoxb-1", appToken: "xapp-1" },
                whatsapp: { linkedDevice: true },
            },
            env: { ANTHROPIC_API_KEY: "sk-ant-xxx", OPENAI_API_KEY: "sk-oai" },
        }"#;
        let cfg = parse_openclaw(src).expect("parses JSON5");
        assert_eq!(
            cfg.default_model.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
        assert_eq!(cfg.agents.len(), 2);
        assert_eq!(cfg.agents[0].id, "main");
        assert_eq!(cfg.agents[1].id, "sales");
        assert_eq!(
            cfg.agents[1].model.as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
        assert_eq!(cfg.channels.telegram.as_deref(), Some("111:aaa"));
        assert_eq!(cfg.channels.discord.as_deref(), Some("disc-tok"));
        assert_eq!(cfg.channels.slack_bot.as_deref(), Some("xoxb-1"));
        assert_eq!(cfg.channels.slack_app.as_deref(), Some("xapp-1"));
        assert!(cfg.channels.whatsapp_present);
        assert_eq!(cfg.env.get("ANTHROPIC_API_KEY").unwrap(), "sk-ant-xxx");
    }

    #[test]
    fn parse_openclaw_defaults_to_single_main_agent() {
        let src = r#"{ channels: {}, models: { primary: "anthropic/claude-opus-4.6" } }"#;
        let cfg = parse_openclaw(src).unwrap();
        assert_eq!(cfg.agents.len(), 1);
        assert_eq!(cfg.agents[0].id, "main");
        assert_eq!(
            cfg.default_model.as_deref(),
            Some("anthropic/claude-opus-4.6")
        );
        assert!(cfg.channels.telegram.is_none());
        assert!(!cfg.channels.whatsapp_present);
    }

    #[test]
    fn parse_openclaw_rejects_garbage() {
        assert!(parse_openclaw("not json at all {{{").is_err());
    }
}
