//! Hermes (`~/.hermes`, NousResearch/hermes-agent, Python) importer.
//!
//! Source shapes per spec §1.2:
//! - `config.yaml` (YAML) + secret `.env` + `auth.json`.
//! - profiles: `profiles/<name>/` recursive; `active_profile` plain text.
//!   v1 imports only the active profile (others → SKIPPED with `--source` hint).
//! - channel tokens (in `.env`): `TELEGRAM_BOT_TOKEN`, `DISCORD_*`,
//!   `SLACK_BOT_TOKEN`/`SLACK_APP_TOKEN`, `EMAIL_*` (email unsupported in v1).
//! - default model: `config.yaml` `model.default`.
//! - persona: `SOUL.md`; memory: `memories/MEMORY.md` + `memories/USER.md`.
//! - sessions: `state.db` → archived verbatim.
//! - cron: `cron/jobs.json` (defensive parse).

use std::path::PathBuf;

use duduclaw_core::error::Result;

use super::apply::*;
use super::report::Report;
use super::*;

/// Parsed hermes `config.yaml` (pure — no filesystem).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct HermesConfig {
    pub model_default: Option<String>,
}

/// Parse a hermes `config.yaml`. Pure + unit-tested.
pub(super) fn parse_hermes_config(yaml: &str) -> std::result::Result<HermesConfig, String> {
    let v: serde_yaml::Value =
        serde_yaml::from_str(yaml).map_err(|e| format!("YAML 解析失敗: {e}"))?;
    let model_default = v
        .get("model")
        .and_then(|m| {
            m.get("default")
                .and_then(|d| d.as_str())
                .or_else(|| m.as_str())
        })
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    Ok(HermesConfig { model_default })
}

fn default_hermes() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".hermes")
}

pub(super) async fn migrate(ctx: &Ctx, source: Option<PathBuf>) -> Result<Report> {
    let base = source.clone().unwrap_or_else(default_hermes);
    let mut report = Report::new("hermes", &base.display().to_string(), ctx.apply);

    if !base.exists() {
        report.skipped("config", &base.display().to_string(), "來源目錄不存在");
        return Ok(report);
    }

    // ── Resolve active profile root (only when no explicit --source) ──
    let mut profile_root = base.clone();
    if source.is_none() {
        let active = std::fs::read_to_string(base.join("active_profile"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(active_name) = &active {
            let pr = base.join("profiles").join(active_name);
            if pr.exists() {
                profile_root = pr;
            }
        }
        if let Ok(rd) = std::fs::read_dir(base.join("profiles")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if !p.is_dir() {
                    continue;
                }
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if active.as_deref() != Some(name.as_str()) {
                    report.skipped(
                        "profile",
                        &name,
                        "非 active profile，v1 不轉（用 `--source <profile 目錄>` 逐一轉移）",
                    );
                }
            }
        }
    }

    // ── config.yaml → model ──
    let model_default = match std::fs::read_to_string(profile_root.join("config.yaml")) {
        Ok(y) => match parse_hermes_config(&y) {
            Ok(c) => c.model_default,
            Err(e) => {
                report.skipped("config", "config.yaml", e);
                None
            }
        },
        Err(_) => None,
    };

    // ── .env (secrets): prefer profile-local, fall back to base ──
    let env = {
        let mut merged = BTreeMap::new();
        for candidate in [profile_root.join(".env"), base.join(".env")] {
            if let Ok(content) = std::fs::read_to_string(&candidate) {
                for (k, v) in parse_env_file(&content) {
                    merged.entry(k).or_insert(v);
                }
            }
        }
        merged
    };

    let anthropic_key = env.get("ANTHROPIC_API_KEY").cloned().unwrap_or_default();

    let mut config_table = read_config_table(&ctx.home);
    plan_api_key(ctx, &mut report, &mut config_table, &anthropic_key);
    for k in env.keys() {
        if k.ends_with("_API_KEY") && k != "ANTHROPIC_API_KEY" {
            report.skipped(
                "api_key",
                k,
                "非 Anthropic 供應商金鑰，v1 不轉移（請手動設定）",
            );
        }
    }

    // ── Channels from .env ──
    {
        let channels = config_table
            .entry("channels")
            .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
            .as_table_mut();
        if let Some(channels) = channels {
            if let Some(tok) = env.get("TELEGRAM_BOT_TOKEN") {
                plan_channel_token(ctx, &mut report, channels, "telegram", tok, None);
            }
            if let Some(tok) = env
                .get("DISCORD_BOT_TOKEN")
                .or_else(|| env.get("DISCORD_TOKEN"))
            {
                plan_channel_token(ctx, &mut report, channels, "discord", tok, None);
            }
            if let Some(bot) = env.get("SLACK_BOT_TOKEN") {
                plan_channel_token(
                    ctx,
                    &mut report,
                    channels,
                    "slack",
                    bot,
                    env.get("SLACK_APP_TOKEN").map(String::as_str),
                );
            }
        }
        if env.keys().any(|k| k.starts_with("EMAIL_")) {
            report.skipped("channel", "email", "v1 尚未支援 email 通道");
        }
    }

    if ctx.apply
        && let Err(e) = write_config_table(&ctx.home, &config_table)
    {
        report.skipped("config", "config.toml", format!("寫入失敗: {e}"));
    }

    // ── Single agent (Hermes is a single-agent platform) ──
    let soul_body = std::fs::read_to_string(profile_root.join("SOUL.md")).ok();
    let (preferred, needs_review) = match &model_default {
        Some(m) => {
            let (p, r) = map_model(m);
            (Some(p), r)
        }
        None => (None, false),
    };

    let engine = open_memory(ctx);
    if let Some(agent_id) = scaffold_agent(
        ctx,
        &mut report,
        "hermes",
        "Hermes",
        "specialist",
        "",
        preferred.clone(),
        soul_body,
    )
    .await
    {
        if needs_review && let Some(m) = &preferred {
            report.partial(
                "model",
                &agent_id,
                format!("非 Claude 模型 '{m}'，請人工確認 runtime 對映"),
            );
        }

        // Memory: memories/MEMORY.md + memories/USER.md bullets.
        let mut facts = Vec::new();
        for file in ["MEMORY.md", "USER.md"] {
            if let Ok(c) = std::fs::read_to_string(profile_root.join("memories").join(file)) {
                facts.extend(extract_bullets(&c));
            }
        }
        import_facts(
            engine.as_ref(),
            ctx,
            &mut report,
            &agent_id,
            "memories",
            &facts,
        )
        .await;
        for file in ["MEMORY.md", "USER.md"] {
            let _ =
                copy_into_agent_memory(ctx, &agent_id, &profile_root.join("memories").join(file));
        }

        // Skills.
        let mut skills = Vec::new();
        if let Ok(rd) = std::fs::read_dir(profile_root.join("skills")) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() && p.join("SKILL.md").exists() {
                    skills.push(p);
                }
            }
        }
        if !skills.is_empty() {
            install_skills(ctx, &mut report, &agent_id, &skills);
            report
                .note("已安裝的 skills 皆通過 duduclaw-security 注入掃描 (input_guard, 6 規則)。");
        }

        // Cron: cron/jobs.json defensive parse.
        if let Ok(content) = std::fs::read_to_string(profile_root.join("cron").join("jobs.json")) {
            import_cron_file(ctx, &mut report, &agent_id, &content).await;
        }

        // Archive raw sessions (state.db).
        let state_db = profile_root.join("state.db");
        if state_db.exists() {
            archive_raw(ctx, &mut report, &[("state.db".to_string(), state_db)]);
        }
    }

    Ok(report)
}

async fn import_cron_file(ctx: &Ctx, report: &mut Report, agent_id: &str, content: &str) {
    let parsed: serde_json::Value = match json5::from_str(content) {
        Ok(v) => v,
        Err(e) => {
            report.skipped("cron", "jobs.json", format!("解析失敗: {e}"));
            return;
        }
    };
    let arr = parsed
        .as_array()
        .cloned()
        .or_else(|| parsed.get("jobs").and_then(|j| j.as_array()).cloned())
        .unwrap_or_default();
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
    import_cron_jobs(ctx, report, agent_id, &jobs).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hermes_model_default() {
        let yaml = "model:\n  default: anthropic/claude-opus-4.6\n  temperature: 0.7\nother: 1\n";
        let cfg = parse_hermes_config(yaml).unwrap();
        assert_eq!(
            cfg.model_default.as_deref(),
            Some("anthropic/claude-opus-4.6")
        );
    }

    #[test]
    fn parse_hermes_model_scalar_form() {
        // `model: "..."` (scalar) also handled.
        let yaml = "model: anthropic/claude-sonnet-4-6\n";
        let cfg = parse_hermes_config(yaml).unwrap();
        assert_eq!(
            cfg.model_default.as_deref(),
            Some("anthropic/claude-sonnet-4-6")
        );
    }

    #[test]
    fn parse_hermes_missing_model_is_none() {
        let cfg = parse_hermes_config("gateway:\n  port: 8080\n").unwrap();
        assert!(cfg.model_default.is_none());
    }

    #[test]
    fn parse_hermes_rejects_bad_yaml() {
        assert!(parse_hermes_config("key: [unbalanced").is_err());
    }
}
