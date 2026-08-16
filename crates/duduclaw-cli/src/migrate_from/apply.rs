//! Side-effecting import helpers shared by all platform importers.
//!
//! These are the functions that actually write to disk / SQLite when `--apply`
//! is set (and in dry-run mode simply record the planned outcome). Kept apart
//! from the pure parsers in `mod.rs` so the effectful surface is easy to audit.

use std::path::{Path, PathBuf};

use duduclaw_core::error::{DuDuClawError, Result};
#[allow(unused_imports)]
use duduclaw_core::traits::MemoryEngine as _;
use duduclaw_core::types::{MemoryEntry, MemoryLayer, RuntimeType};
use duduclaw_memory::SqliteMemoryEngine;

use super::report::Report;
use super::*;

fn build_semantic_entry(agent_id: &str, content: &str, platform: Platform) -> MemoryEntry {
    MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        tags: vec![format!("imported-from-{}", platform.as_str())],
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: format!("migrate-from-{}", platform.as_str()),
    }
}

/// Open the shared memory engine (only meaningful in `--apply` mode).
pub(super) fn open_memory(ctx: &Ctx) -> Option<SqliteMemoryEngine> {
    if !ctx.apply {
        return None;
    }
    SqliteMemoryEngine::new(&ctx.home.join("memory.db")).ok()
}

/// Import a set of `facts` as Semantic memory entries for `agent_id`.
/// Reports one roll-up line per source label.
pub(super) async fn import_facts(
    engine: Option<&SqliteMemoryEngine>,
    ctx: &Ctx,
    report: &mut Report,
    agent_id: &str,
    label: &str,
    facts: &[String],
) {
    if facts.is_empty() {
        return;
    }
    if !ctx.apply {
        report.imported("memory", &format!("{label} ({} 筆)", facts.len()));
        return;
    }
    let Some(eng) = engine else {
        report.skipped("memory", label, "開啟 memory.db 失敗");
        return;
    };
    let mut ok = 0usize;
    for f in facts {
        let entry = build_semantic_entry(agent_id, f, ctx.platform);
        // WP1: migrated facts carry the `import` origin (ceiling 0.7).
        let meta = duduclaw_memory::TemporalMeta {
            origin: Some("import".to_string()),
            ..Default::default()
        };
        if eng.store_temporal(agent_id, entry, meta).await.is_ok() {
            ok += 1;
        }
    }
    if ok == facts.len() {
        report.imported("memory", &format!("{label} ({ok} 筆)"));
    } else if ok > 0 {
        report.partial("memory", label, format!("{ok}/{} 筆寫入成功", facts.len()));
    } else {
        report.skipped("memory", label, "全部寫入失敗");
    }
}

/// Store one SPO-tagged Semantic memory fact with full temporal metadata
/// (WP-9A — `claude-code` memory-shard / session-summary imports; the plain
/// `import_facts` bag-of-strings helper above has no room for a triple).
///
/// Two invariants are enforced here, not left to the caller:
/// - `meta.origin` is unconditionally forced to `"import"` (ceiling 0.7) —
///   even if a caller passed something else, a migrated fact can never claim
///   `agent_derived`/`user_direct` trust (I8 non-malleability, design R7).
/// - `content` is run through the fail-closed injection scanner before any
///   write; a hit is `SKIPPED(security)` and nothing reaches `memory.db`
///   (design §3.4 — "不做「照樣匯入但標記」").
pub(super) async fn store_import_memory(
    engine: Option<&SqliteMemoryEngine>,
    ctx: &Ctx,
    report: &mut Report,
    agent_id: &str,
    label: &str,
    content: &str,
    tags: Vec<String>,
    mut meta: duduclaw_memory::TemporalMeta,
) {
    let scan = duduclaw_security::input_guard::scan_input(
        content,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if scan.blocked {
        report.skipped(
            "memory",
            label,
            format!(
                "security: 偵測到注入風險 (risk {}, 規則 {})",
                scan.risk_score,
                scan.matched_rules.join("/")
            ),
        );
        return;
    }
    if !ctx.apply {
        report.imported("memory", label);
        return;
    }
    let Some(eng) = engine else {
        report.skipped("memory", label, "開啟 memory.db 失敗");
        return;
    };
    let entry = MemoryEntry {
        id: uuid::Uuid::new_v4().to_string(),
        agent_id: agent_id.to_string(),
        content: content.to_string(),
        timestamp: chrono::Utc::now(),
        tags,
        embedding: None,
        layer: MemoryLayer::Semantic,
        importance: 5.0,
        access_count: 0,
        last_accessed: None,
        source_event: format!("migrate-from-{}", ctx.platform.as_str()),
    };
    meta.origin = Some("import".to_string());
    match eng.store_temporal(agent_id, entry, meta).await {
        Ok(_) => report.imported("memory", label),
        Err(e) => report.skipped("memory", label, format!("寫入失敗: {e}")),
    }
}

/// Import defensively-parsed cron jobs into the SQLite cron store.
pub(super) async fn import_cron_jobs(
    ctx: &Ctx,
    report: &mut Report,
    agent_id: &str,
    jobs: &[CronJob],
) {
    if jobs.is_empty() {
        return;
    }
    if !ctx.apply {
        for job in jobs {
            report.imported("cron", &format!("{} ({})", job.name, job.cron));
        }
        return;
    }
    let store = match duduclaw_gateway::cron_store::CronStore::open(&ctx.home) {
        Ok(s) => s,
        Err(e) => {
            for job in jobs {
                report.skipped("cron", &job.name, format!("開啟 cron_store 失敗: {e}"));
            }
            return;
        }
    };
    for job in jobs {
        let row = duduclaw_gateway::cron_store::CronTaskRow::new(
            uuid::Uuid::new_v4().to_string(),
            job.name.clone(),
            agent_id.to_string(),
            job.cron.clone(),
            job.task.clone(),
        );
        match store.insert(&row).await {
            Ok(()) => report.imported("cron", &job.name),
            Err(e) => report.skipped("cron", &job.name, format!("寫入失敗: {e}")),
        }
    }
}

/// Global-config channel key names (flat keys under `[channels]`, matching the
/// gateway's `decrypt_config_field` resolver). Returns
/// `(primary_key, optional_secondary_key)`.
pub(super) fn channel_keys(channel: &str) -> Option<(&'static str, Option<&'static str>)> {
    match channel {
        "telegram" => Some(("telegram_bot_token", None)),
        "discord" => Some(("discord_bot_token", None)),
        "slack" => Some(("slack_bot_token", Some("slack_app_token"))),
        _ => None,
    }
}

fn channel_has_value(channels: &toml::value::Table, base: &str) -> bool {
    let non_empty = |k: &str| {
        channels
            .get(k)
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    non_empty(base) || non_empty(&format!("{base}_enc"))
}

/// Plan/apply a channel token into the `[channels]` sub-table.
///
/// Never overwrites an existing token (→ CONFLICT). Only the encrypted `_enc`
/// field is written — plaintext secrets never touch config.toml.
pub(super) fn plan_channel_token(
    ctx: &Ctx,
    report: &mut Report,
    channels: &mut toml::value::Table,
    channel: &str,
    primary_token: &str,
    secondary_token: Option<&str>,
) {
    let Some((pkey, skey)) = channel_keys(channel) else {
        report.skipped("channel", channel, "v1 尚未支援此通道");
        return;
    };
    if channel_has_value(channels, pkey) {
        report.conflict(
            "channel",
            channel,
            format!("config.toml 已有 {pkey}，不覆蓋"),
        );
        return;
    }
    let masked = mask_token(primary_token);
    if ctx.apply {
        match crate::encrypt_api_key(primary_token, &ctx.home) {
            Some(enc) => {
                channels.insert(format!("{pkey}_enc"), toml::Value::String(enc));
            }
            None => {
                report.skipped("channel", channel, "加密失敗");
                return;
            }
        }
        if let (Some(sk), Some(stok)) = (skey, secondary_token)
            && !stok.is_empty()
            && !channel_has_value(channels, sk)
            && let Some(enc) = crate::encrypt_api_key(stok, &ctx.home)
        {
            channels.insert(format!("{sk}_enc"), toml::Value::String(enc));
        }
    }
    report.imported("channel", &format!("{channel} ({masked})"));
}

/// Plan/apply the Anthropic API key into config.toml `[api]`.
/// Only the encrypted field is written; an existing key is never overwritten.
pub(super) fn plan_api_key(ctx: &Ctx, report: &mut Report, config: &mut toml::value::Table, key: &str) {
    if key.trim().is_empty() {
        return;
    }
    let api = config
        .entry("api")
        .or_insert_with(|| toml::Value::Table(toml::value::Table::new()))
        .as_table_mut();
    let Some(api) = api else {
        report.skipped("api_key", "anthropic", "config.toml [api] 區段格式錯誤");
        return;
    };
    let has = |k: &str| {
        api.get(k)
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false)
    };
    if has("anthropic_api_key") || has("anthropic_api_key_enc") {
        report.conflict(
            "api_key",
            "anthropic",
            "config.toml 已有 anthropic key，不覆蓋",
        );
        return;
    }
    let masked = mask_token(key);
    if ctx.apply {
        match crate::encrypt_api_key(key, &ctx.home) {
            Some(enc) => {
                api.insert("anthropic_api_key_enc".into(), toml::Value::String(enc));
            }
            None => {
                report.skipped("api_key", "anthropic", "加密失敗");
                return;
            }
        }
    }
    report.imported("api_key", &format!("anthropic ({masked})"));
}

/// Read config.toml into a mutable table (empty table if the file is absent).
pub(super) fn read_config_table(home: &Path) -> toml::value::Table {
    let path = home.join("config.toml");
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|c| c.parse::<toml::Table>().ok())
        .unwrap_or_default()
}

/// Persist a config.toml table (atomic temp + rename).
pub(super) fn write_config_table(home: &Path, table: &toml::value::Table) -> Result<()> {
    let path = home.join("config.toml");
    let tmp = home.join("config.toml.migrate.tmp");
    let content = toml::to_string_pretty(table)
        .map_err(|e| DuDuClawError::Config(format!("序列化 config.toml 失敗: {e}")))?;
    std::fs::create_dir_all(home).ok();
    std::fs::write(&tmp, content)
        .map_err(|e| DuDuClawError::Io(std::io::Error::other(format!("寫入暫存檔失敗: {e}"))))?;
    std::fs::rename(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        DuDuClawError::Io(std::io::Error::other(format!("覆寫 config.toml 失敗: {e}")))
    })?;
    Ok(())
}

/// Scaffold an imported agent directory, honouring conflict/rename policy.
/// Returns the final agent id (also in dry-run, so downstream items can be
/// planned against it).
#[allow(clippy::too_many_arguments)]
pub(super) async fn scaffold_agent(
    ctx: &Ctx,
    report: &mut Report,
    desired_id: &str,
    display_name: &str,
    role: &str,
    reports_to: &str,
    model_preferred: Option<String>,
    soul_body: Option<String>,
) -> Option<String> {
    let base = sanitize_agent_id(desired_id);
    if !crate::is_valid_agent_id(&base) {
        report.skipped("agent", desired_id, "無法轉為合法的 agent id");
        return None;
    }

    let mut final_id = base.clone();
    if ctx.home.join("agents").join(&final_id).exists() {
        if ctx.rename {
            final_id = sanitize_agent_id(&format!("{base}-imported"));
            if !crate::is_valid_agent_id(&final_id)
                || ctx.home.join("agents").join(&final_id).exists()
            {
                report.conflict("agent", &base, "重新命名後仍與既有 agent 衝突");
                return None;
            }
        } else {
            report.conflict(
                "agent",
                &base,
                "同名 agent 已存在（加 --rename 以 -imported 後綴匯入）",
            );
            return None;
        }
    }

    if !ctx.apply {
        report.imported("agent", &final_id);
        return Some(final_id);
    }

    let scaffold = crate::AgentScaffold {
        name: final_id.clone(),
        display_name: display_name.to_string(),
        role: role.to_string(),
        reports_to: reports_to.to_string(),
        icon: "🤖".to_string(),
        trigger: format!("@{display_name}"),
        provider: RuntimeType::Claude,
        model_preferred,
        soul_body,
    };
    match crate::scaffold_agent_dir(&ctx.home, &scaffold).await {
        Ok(_) => {
            report.imported("agent", &final_id);
            Some(final_id)
        }
        Err(e) => {
            report.skipped("agent", &final_id, format!("建立失敗: {e}"));
            None
        }
    }
}

/// Copy + install skill directories after running each SKILL.md through the
/// prompt-injection scanner. A flagged skill is NOT installed (SKIPPED-security).
pub(super) fn install_skills(ctx: &Ctx, report: &mut Report, agent_id: &str, skill_dirs: &[PathBuf]) {
    for sd in skill_dirs {
        let name = sd
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "unknown-skill".to_string());
        let skill_md = sd.join("SKILL.md");
        let content = match std::fs::read_to_string(&skill_md) {
            Ok(c) => c,
            Err(_) => {
                report.skipped("skill", &name, "找不到或無法讀取 SKILL.md");
                continue;
            }
        };

        // Fail-closed injection scan (duduclaw-security, 6 rule categories).
        let scan = duduclaw_security::input_guard::scan_input(
            &content,
            duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
        );
        if scan.blocked {
            report.skipped(
                "skill",
                &name,
                format!(
                    "security: 偵測到注入風險 (risk {}, 規則 {})",
                    scan.risk_score,
                    scan.matched_rules.join("/")
                ),
            );
            continue;
        }

        if !ctx.apply {
            report.imported("skill", &name);
            continue;
        }
        let dest = ctx
            .home
            .join("agents")
            .join(agent_id)
            .join("SKILLS")
            .join(&name);
        match copy_dir_recursive(sd, &dest) {
            Ok(()) => report.imported("skill", &name),
            Err(e) => report.skipped("skill", &name, format!("複製失敗: {e}")),
        }
    }
}

/// Archive verbatim source files/dirs into `imported/<platform>/raw/<label>`.
pub(super) fn archive_raw(ctx: &Ctx, report: &mut Report, entries: &[(String, PathBuf)]) {
    for (label, src) in entries {
        if !src.exists() {
            continue;
        }
        if !ctx.apply {
            report.imported("raw", label);
            continue;
        }
        let dest = ctx.raw_dir().join(label);
        let res = if src.is_dir() {
            copy_dir_recursive(src, &dest)
        } else {
            std::fs::create_dir_all(ctx.raw_dir()).and_then(|_| std::fs::copy(src, &dest).map(|_| ()))
        };
        match res {
            Ok(()) => report.imported("raw", label),
            Err(e) => report.skipped("raw", label, format!("歸檔失敗: {e}")),
        }
    }
}

/// Copy `src` file(s) into the agent's `memory/` dir for fidelity.
pub(super) fn copy_into_agent_memory(ctx: &Ctx, agent_id: &str, src: &Path) -> std::io::Result<()> {
    if !ctx.apply || !src.exists() {
        return Ok(());
    }
    let name = src
        .file_name()
        .map(|n| n.to_owned())
        .unwrap_or_else(|| std::ffi::OsString::from("imported.md"));
    let dest_dir = ctx.home.join("agents").join(agent_id).join("memory");
    std::fs::create_dir_all(&dest_dir)?;
    std::fs::copy(src, dest_dir.join(name)).map(|_| ())
}

/// Recursively copy a directory tree.
pub(super) fn copy_dir_recursive(src: &Path, dest: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let target = dest.join(entry.file_name());
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else {
            std::fs::copy(&path, &target)?;
        }
    }
    Ok(())
}

/// A wiki page to write into an agent's `imported/<platform>/...` namespace
/// (WP-9A — `claude_code.rs`'s CLAUDE.md → wiki mapping; the module had no
/// wiki-writing helper before this).
pub(super) struct ImportedWikiPage {
    /// Path relative to the agent's `wiki/` dir, e.g.
    /// `imported/claude-code/global/CLAUDE.md`. Must end in `.md`.
    pub rel_path: String,
    pub title: String,
    /// Already includes the DATA-not-instructions banner — this function
    /// does not add one, so callers must (mirrors `store_import_memory`,
    /// which leaves content shaping to the caller too).
    pub body: String,
    pub tags: Vec<String>,
    pub sources: Vec<String>,
}

/// Write one imported wiki page: `layer: context` (never enters the system
/// prompt injection budget — `WikiLayer::auto_inject()` only admits
/// Identity/Core), `trust: 0.3` (same ceiling as the `import` memory origin),
/// `author: "import"`. Injection-scanned first (fail-closed, same discipline
/// as `store_import_memory`); dry-run only records the plan.
pub(super) fn import_wiki_page(ctx: &Ctx, report: &mut Report, agent_id: &str, page: &ImportedWikiPage) {
    let scan = duduclaw_security::input_guard::scan_input(
        &page.body,
        duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
    );
    if scan.blocked {
        report.skipped(
            "wiki",
            &page.rel_path,
            format!(
                "security: 偵測到注入風險 (risk {}, 規則 {})",
                scan.risk_score,
                scan.matched_rules.join("/")
            ),
        );
        return;
    }
    if !ctx.apply {
        report.imported("wiki", &page.rel_path);
        return;
    }
    let wiki_dir = ctx.home.join("agents").join(agent_id).join("wiki");
    let store = duduclaw_memory::WikiStore::new(wiki_dir);
    if let Err(e) = store.ensure_scaffold() {
        report.skipped("wiki", &page.rel_path, format!("初始化 wiki 失敗: {e}"));
        return;
    }
    let now = chrono::Utc::now();
    let wp = duduclaw_memory::WikiPage {
        path: page.rel_path.clone(),
        title: page.title.clone(),
        created: now,
        updated: now,
        tags: page.tags.clone(),
        related: Vec::new(),
        sources: page.sources.clone(),
        author: Some("import".to_string()),
        layer: duduclaw_memory::WikiLayer::Context,
        trust: 0.3,
        source_type: duduclaw_memory::SourceType::Unknown,
        last_verified: None,
        citation_count: 0,
        error_signal_count: 0,
        success_signal_count: 0,
        do_not_inject: false,
        body: page.body.clone(),
    };
    let content = duduclaw_memory::serialize_page(&wp);
    match store.write_page_with_author(&page.rel_path, &content, "import") {
        Ok(()) => report.imported("wiki", &page.rel_path),
        Err(e) => report.skipped("wiki", &page.rel_path, format!("寫入失敗: {e}")),
    }
}
