//! `duduclaw pack install` (expert pipeline) — source resolution, format dispatch, and the native
//! `expert.toml` importer, plus shared skill/wiki install primitives used by
//! the foreign-format importers ([`super::plugin`], [`super::skill_import`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use console::style;

use duduclaw_core::error::Result;

use super::detect::{self, Format};
use super::topo::{TopoOutcome, topo_sort};
use super::{InstallRecord, PackKind, Report};
use super::{
    ScanVerdict, cfg_err, copy_dir, io_err, manifest, merge_toml, now_iso, safe_zip, scan_external,
};

/// Shared context threaded through every importer.
pub(super) struct InstallCtx {
    pub home: PathBuf,
    pub dry_run: bool,
    pub confirm_profile: Option<String>,
    pub rename: bool,
    /// Explicit operator grant for pack hooks (`--trust-hooks`). Without it
    /// hooks land disabled behind an ApprovalBroker request (fail-closed).
    pub trust_hooks: bool,
    /// Existing agent id to attach the pack's root agents under
    /// (`--attach-under`). Validated to exist BEFORE anything installs
    /// (fail-closed); pack roots then scaffold with `reports_to` set to it.
    pub attach_under: Option<String>,
    /// What to do when this was imported before ([`super::reimport`]).
    pub reimport: super::reimport::ReimportMode,
    /// The earlier import a replace moves away (resolved up front).
    pub replace: Option<super::reimport::ReplaceTarget>,
    /// The gateway's agent cap (`--agent-cap`), applied with the one
    /// resolved from disk ([`super::quota`]).
    pub agent_cap: Option<usize>,
    /// What a real replace moved away and what the new version wrote
    /// ([`InstallCtx::begin_writes`], [`InstallCtx::note_agent`]).
    pub replace_state: std::sync::Mutex<super::reimport::ReplaceState>,
}

/// Options of one pipeline run beyond the source itself.
#[derive(Debug, Clone, Default)]
pub(crate) struct PipelineOpts {
    pub dry_run: bool,
    pub confirm_profile: Option<String>,
    pub rename: bool,
    pub trust_hooks: bool,
    pub attach_under: Option<String>,
    /// Write the machine-readable report here (dashboard upload).
    pub report_json: Option<PathBuf>,
    /// What the front door's zip extraction left out (when it already
    /// extracted the source before handing a directory over).
    pub zip_summary: Option<safe_zip::ExtractSummary>,
    /// `--replace <slug>` / `--as-copy` / `--accept-edits`.
    pub reimport: super::reimport::ReimportMode,
    /// `--agent-cap` (the dashboard passes the gateway's cap).
    pub agent_cap: Option<usize>,
    /// Set by the front door, which already resolved the source and stepped
    /// through wrapper folders: the extraction root it started from. The
    /// source handed over is then used as is (no second unwrap, so the
    /// wrapper depth stays [`detect::MAX_WRAPPER_DEPTH`]).
    pub extract_root: Option<PathBuf>,
    /// Tests only: fail right after the importer wrote everything (the
    /// same point as a failed install-record write).
    #[cfg(test)]
    pub fail_after_dispatch: bool,
    /// Tests only: stop right after the importer wrote, without rolling
    /// back or closing the journal (what a killed installer leaves).
    #[cfg(test)]
    pub abandon_after_dispatch: bool,
}

/// Entry for `duduclaw pack install` once it hands a roster-shaped pack over
/// (default options; the front door itself calls [`cmd_install_with`]).
#[cfg(test)]
pub async fn cmd_install(
    home: &Path,
    source: &str,
    dry_run: bool,
    rename: bool,
    trust_hooks: bool,
    attach_under: Option<String>,
) -> Result<()> {
    let opts = PipelineOpts {
        dry_run,
        rename,
        trust_hooks,
        attach_under,
        ..Default::default()
    };
    super::agent_config::preview::confirmed_install(home, source, opts).await
}

/// Set once the gateway asked this run to stop (see [`listen_for_stop`]).
static STOP_REQUESTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn stop_requested() -> bool {
    STOP_REQUESTED.load(std::sync::atomic::Ordering::SeqCst)
}

/// The failure of a run that was told to stop.
pub(crate) fn interrupted() -> duduclaw_core::error::DuDuClawError {
    super::import_error::import_err("import.timeout", serde_json::json!({ "interrupted": true }), "匯入被要求停止")
}

/// Most of an import is file work with no wait in between, so a stop
/// request cannot cancel it from outside: a task on another worker raises
/// [`STOP_REQUESTED`], the importers stop starting new writes, and the run
/// rolls back as for any failure.
fn listen_for_stop() {
    tokio::spawn(async {
        terminated().await;
        STOP_REQUESTED.store(true, std::sync::atomic::Ordering::SeqCst);
    });
}

/// Resolves when this process is asked to stop (SIGTERM — what the gateway
/// sends at its timeout before it kills). Only for runs the gateway
/// started (it sets the marker nonce); a terminal run keeps the default
/// signal handling. Never on Windows, where the stop is forceful and the
/// journal is recovered by the next import or gateway start instead.
async fn terminated() {
    #[cfg(unix)]
    if std::env::var_os(super::import_error::MARKER_NONCE_ENV).is_some() {
        use tokio::signal::unix::{SignalKind, signal};
        if let Ok(mut s) = signal(SignalKind::terminate()) {
            s.recv().await;
            return;
        }
    }
    std::future::pending::<()>().await
}

/// [`cmd_install`] with the full option set.
///
/// With `report_json`, the machine-readable report is written on success
/// (after the install record) and on failure (with `meta.error`), so the
/// dashboard can always show what happened. The file must be a new `.json`
/// path: an existing file is never overwritten.
pub(crate) async fn cmd_install_with(home: &Path, source: &str, opts: PipelineOpts) -> Result<()> {
    crate::pack_cmd::refuse_in_ai_session(&crate::ai_session_guard::command_markers())?;
    let mut report_file = match &opts.report_json {
        Some(p) => Some(super::report::open_report_file(p)?),
        None => None,
    };
    let mut report = Report::default();
    // A replace stopped half way earlier (killed installer, machine down)
    // is put back before anything else happens.
    let recovered = super::reimport::recover_interrupted(home, super::reimport::RecoverScope::Abandoned).await;
    if !recovered.is_empty() {
        for r in &recovered {
            eprintln!(
                "  {} 上次取代「{}」沒有做完，已還原之前的版本",
                style("↺").yellow(),
                r.slug
            );
        }
        report.detail(
            "recovered",
            serde_json::json!(recovered.iter().map(|r| r.slug.clone()).collect::<Vec<_>>()),
        );
    }
    listen_for_stop();
    let outcome = {
        let run = install_inner(home, source, &opts, &mut report);
        tokio::pin!(run);
        // The stop request first: the importer is only interrupted where it
        // waits (sync file work runs to its next await).
        tokio::select! {
            biased;
            () = terminated() => Err(interrupted()),
            r = &mut run => r,
        }
    };
    // Told to stop part-way through a replace: put the earlier version back
    // from this run's own journal (the importer future is gone by now).
    if outcome.as_ref().err().and_then(super::import_error::failure_of).map(|f| f.params.get("interrupted").is_some())
        == Some(true)
    {
        let mine = super::reimport::recover_interrupted(home, super::reimport::RecoverScope::IncludingOwn).await;
        if !mine.is_empty() {
            eprintln!("  {} 匯入中止，之前的版本已還原", style("↺").yellow());
            report.detail("rolled_back", serde_json::json!({ "interrupted": true }));
        }
    }
    if let Err(e) = &outcome {
        report.meta("error", &super::import_error::plain_message(e));
        report.set_error(super::import_error::error_json(e));
    }
    if let Some(file) = report_file.as_mut() {
        use std::io::Write;
        let body = serde_json::to_string_pretty(&report.to_json(opts.dry_run))
            .map_err(|e| cfg_err(format!("序列化匯入報告失敗：{e}")))?;
        file.write_all(body.as_bytes())
            .map_err(|e| io_err(format!("寫入匯入報告失敗：{e}")))?;
    }
    outcome
}

async fn install_inner(home: &Path, source: &str, opts: &PipelineOpts, report: &mut Report) -> Result<()> {
    let dry_run = opts.dry_run;
    // ── 1. Resolve the source to an on-disk directory (download / unzip). ──
    // A throwaway staging dir under the system temp; cleaned at the end.
    let staging = std::env::temp_dir().join(format!("duduclaw-expert-{}", uuid::Uuid::new_v4()));
    let cleanup = Cleanup(staging.clone());

    let resolved = super::source::resolve_source_full(source, &staging).await?;
    let (root, extract_root) = match &opts.extract_root {
        Some(er) => (resolved.dir.clone(), er.clone()),
        None => (detect::resolve_root(&resolved.dir), resolved.dir.clone()),
    };
    let zip_summary = resolved.zip.or(opts.zip_summary.clone());

    // ── 2. Detect format (fail-closed). ──
    let Some(format) = detect::detect(&root) else {
        let contents = detect::list_contents(&root);
        report.meta("unrecognized_contents", &contents.join("\n"));
        // Look for a deeper pack before the staging dir is cleaned up.
        let err = detect::unrecognized_error(&extract_root, &root, contents);
        drop(cleanup);
        return Err(err);
    };

    // ── attach-under: validate the target exists BEFORE anything installs
    //    (fail-closed — a typo'd supervisor must not half-install a pack). ──
    let attach_under = match opts.attach_under.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(id) => {
            if !crate::is_valid_agent_id(id) || !home.join("agents").join(id).join("agent.toml").is_file() {
                drop(cleanup);
                return Err(super::import_error::import_err(
                    "import.attach_under_missing",
                    serde_json::json!({ "id": duduclaw_core::truncate_chars(id, 64) }),
                    format!("--attach-under 目標 '{id}' 不存在（先建立主管或改用既有 agent）"),
                ));
            }
            Some(id.to_string())
        }
    };

    // ── Re-import: resolve what a replace would move away, and refuse a
    //    replace of edited employees without explicit consent. ──
    let replace = super::reimport::resolve_target(home, &opts.reimport)?;
    if let Some(t) = &replace {
        let edits = if dry_run {
            super::reimport::edits_since_import(home, &t.slug, &t.record, &t.agents)
        } else {
            super::reimport::ensure_edits_accepted(home, t)?
        };
        report.detail("replace", super::reimport::replace_detail(t, &edits));
    }

    let ctx = InstallCtx {
        home: home.to_path_buf(),
        dry_run,
        confirm_profile: opts.confirm_profile.clone(),
        rename: opts.rename || opts.reimport == super::reimport::ReimportMode::Copy,
        trust_hooks: opts.trust_hooks,
        attach_under,
        reimport: opts.reimport.clone(),
        replace,
        agent_cap: opts.agent_cap,
        replace_state: Default::default(),
    };
    if let Some(z) = &zip_summary {
        super::report::report_zip_summary(z, report);
    }

    // ── 3. Dispatch. A real replace moves the earlier import to the backup
    //    only once the importer's checks have passed (`begin_writes`), and
    //    moves it back if anything after that fails. ──
    let dispatched = match format {
        Format::Native => install_native(&ctx, &root, report).await,
        Format::ClaudePlugin => super::plugin::install(&ctx, &root, report).await,
        Format::Skill => super::skill_import::install(&ctx, &root, report).await,
        Format::AgentConfig(kind) => super::agent_config::install(&ctx, &root, kind, report).await,
    };
    let state = ctx.take_replace_state();
    let dispatched = match dispatched {
        // Every importer moves the earlier import away before it writes; a
        // real replace that got here without doing so must not record.
        Ok(_) if ctx.replace.is_some() && !dry_run && state.retired.is_none() => Err(
            super::import_error::import_err(
                "import.replace_failed",
                serde_json::json!({ "slug": ctx.replace.as_ref().map(|t| t.slug.clone()) }),
                "取代流程沒有先備份之前的版本，已停止，沒有寫入匯入紀錄",
            ),
        ),
        // Told to stop while the importer worked: roll back like any failure.
        Ok(_) if stop_requested() => Err(interrupted()),
        #[cfg(test)]
        Ok(_) if opts.abandon_after_dispatch => {
            return Err(super::import_error::import_err("import.replace_failed", serde_json::json!({}), "test: abandoned"));
        }
        #[cfg(test)]
        Ok(_) if opts.fail_after_dispatch => Err(super::import_error::import_err(
            "import.replace_failed",
            serde_json::json!({}),
            "test: injected failure after the importer wrote",
        )),
        other => other,
    };
    let record = match dispatched.and_then(|r| {
        if dry_run { Ok(r) } else { super::write_record(home, &r).map(|()| r) }
    }) {
        Ok(r) => r,
        Err(e) => {
            if let Some(log) = &state.retired {
                let n = match super::reimport::rollback(log) {
                    Some(dir) => super::reimport::withdraw_pending(home, &dir, "import_failed").await,
                    None => 0,
                };
                eprintln!(
                    "  {} 之前的版本已還原；新版本已寫入的部分移到 {}{}",
                    style("↺").yellow(),
                    log.backup.join("failed-new").display(),
                    if n > 0 { format!("，並撤回它送出的 {n} 張審核單") } else { String::new() }
                );
                report.detail("rolled_back", serde_json::json!({ "withdrawn_approvals": n }));
            }
            return Err(e);
        }
    };
    let retired = state.retired;
    // The record is written: the replace has happened, its journal goes.
    if let Some(log) = &retired {
        log.close();
    }

    report.render_console(dry_run);
    report.meta("slug", &record.slug);
    report.meta("display_name", &record.display_name);
    report.meta("pack_format", format_label(format));
    report.meta("agents", &record.agents.join(","));

    // ── 4. Install record already persisted above (real runs); keep what
    //    each employee looks like now, so a later replace can tell edits. ──
    if !dry_run {
        if let Err(e) = super::reimport::baseline::write(home, &record.slug, &record.agents) {
            eprintln!("  {} {e}", style("⚠").yellow());
        }
        if let Some(log) = &retired {
            super::reimport::finish(home, log, &record.agents);
            let n = super::reimport::withdraw_pending(home, &log.record_backup(), "import_replaced").await;
            report.detail("replaced", serde_json::json!({ "agents": log.agents, "withdrawn_approvals": n }));
        }
        println!(
            "  {} 已安裝專家包 {} v{} [{}]\n",
            style("✓").green(),
            style(&record.slug).bold(),
            record.version,
            format_label(format)
        );
    } else {
        if let Some(code) = report.to_json(true)["details"]["profile_confirmation"].as_str() {
            println!(
                "  {} 以上為 --dry-run 計畫，未寫入任何檔案；確認內容後移除 --dry-run，並加上 --confirm-profile {code} 即可實際安裝。\n",
                style("ℹ").cyan()
            );
        } else {
            println!(
                "  {} 以上為 --dry-run 計畫，未寫入任何檔案；移除該旗標即實際安裝。\n",
                style("ℹ").cyan()
            );
        }
    }

    drop(cleanup);
    Ok(())
}

fn format_label(f: Format) -> &'static str {
    match f {
        Format::Native => "expert.toml",
        Format::ClaudePlugin => "claude-plugin",
        Format::Skill => "agent-skill",
        Format::AgentConfig(k) => k.code(),
    }
}

// ─────────────────────────── Native importer ───────────────────────────

async fn install_native(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    let m = manifest::read(dir).map_err(super::import_error::bad_content)?;
    let e = &m.expert;

    if e.name.trim().is_empty() || !crate::is_valid_agent_id(&e.name) {
        return Err(super::import_error::bad_content(format!(
            "expert.name '{}' 非合法 slug",
            e.name.escape_debug()
        )));
    }
    let mut profiles = BTreeMap::new();
    for agent in &e.agents {
        if !crate::is_valid_agent_id(&agent.name) || duduclaw_core::is_reserved_agent_id(&agent.name) {
            return Err(cfg_err("專家包員工代號無效，不能用於路徑".into()));
        }
        let root = dir.join("agents").join(&agent.name);
        let read_doc = |file: &str| -> Result<Option<super::agent_config::ir::Doc>> {
            let path = root.join(file);
            match std::fs::symlink_metadata(&path) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(cfg_err(format!("無法讀取專家包區段 {file}：{e}"))),
                Ok(meta) if !meta.is_file() => return Err(cfg_err(format!("專家包區段 {file} 不是一般檔案"))),
                Ok(_) => {}
            }
            let text = super::agent_config::md::read_text(&path)
                .ok_or_else(|| cfg_err(format!("無法讀取專家包區段 {file}")))?;
            Ok(Some(super::agent_config::ir::Doc::new(file, text)))
        };
        let settings = match read_doc("agent.partial.toml")? {
            Some(doc) => toml::from_str(&doc.text).map_err(|_| cfg_err("專家包設定格式不正確".into()))?,
            None => toml::Table::new(),
        };
        super::agent_config::preview::reject_mcp_settings(&settings)?;
        profiles.insert(agent.name.clone(), super::agent_config::ir::AgentDraft {
            id_hint: agent.name.clone(), display_name: if agent.display_name.is_empty() { agent.name.clone() } else { agent.display_name.clone() },
            description: format!("role: {}\nreports_to: {}\ndepartment: {}\nrank: {}\ntrigger: {}\nskills: {}",
                agent.role, agent.reports_to, agent.department, agent.rank, agent.trigger, agent.skills.join("\n")),
            soul: read_doc("soul.md")?, contract: read_doc("CONTRACT.toml")?,
            settings, ..Default::default()
        });
    }
    for profile in profiles.values_mut() {
        super::agent_config::credentials::sanitize_draft(profile)?;
    }
    if profiles.len() != e.agents.len() {
        return Err(cfg_err("專家包含重複員工代號".into()));
    }
    let slug = ctx.claim_slug(&e.name)?;
    let mut record = InstallRecord {
        slug: slug.clone(),
        kind: PackKind::Native,
        display_name: e.display(super::UI_LOCALE).to_string(),
        version: e.version.clone(),
        description: e.description.clone(),
        agents: Vec::new(),
        global_skills: Vec::new(),
        wiki_files: Vec::new(),
        agent_skills: Vec::new(),
        agent_ids: Default::default(),
        installed_at: now_iso(),
        source_format: String::new(),
    };

    // ── requires: doctor-style, warn only ──
    for env_var in &e.requires.env {
        if std::env::var(env_var).map(|v| v.is_empty()).unwrap_or(true) {
            report.warning("requires-env", env_var, "未設定（安裝不受阻）");
        }
    }
    for bin in &e.requires.bins {
        if which_on_path(bin).is_none() {
            report.warning("requires-bin", bin, "不在 PATH（安裝不受阻）");
        }
    }

    // ── prompts / channels: informational ──
    for p in &e.prompts.recommended {
        report.ignored("prompt", &truncate(p, 48), "建議提示語（供 UI 呈現）");
    }
    if !e.channels.suggested.is_empty() {
        report.ignored("channels", &e.channels.suggested.join(", "), "建議通路");
    }

    // ── agents in topological (parents-first) order ──
    let nodes: Vec<(String, Option<String>)> = e
        .agents
        .iter()
        .map(|a| {
            let rt = a.reports_to.trim();
            (
                a.name.clone(),
                if rt.is_empty() {
                    None
                } else {
                    Some(rt.to_string())
                },
            )
        })
        .collect();
    let order = match topo_sort(&nodes) {
        TopoOutcome::Sorted(o) => o,
        TopoOutcome::Cycle(stuck) => {
            report.warning(
                "agents",
                "reports_to",
                format!("偵測到循環 ({})，以扁平階層匯入", stuck.join("/")),
            );
            e.agents.iter().map(|a| a.name.clone()).collect()
        }
    };

    // One id decision for the whole team: when any member's id is taken,
    // every member gets the same suffix, so the team stays recognisable and
    // `reports_to` keeps pointing inside it (instead of skipping members and
    // leaving their subordinates top-level).
    let suffix = team_id_suffix(ctx, &e.agents, report);
    // Names (ids, display names, triggers) current employees answer to:
    // channel routing calls `@display` and `@id`, so no new member may
    // share one.
    let mut book = ctx.name_book();
    let mut final_ids = std::collections::BTreeSet::new();
    for agent in &e.agents {
        let id = crate::sanitize_agent_id(&format!("{}{suffix}", agent.name));
        if !final_ids.insert(id.clone()) { return Err(cfg_err("解析後的員工代號重複".into())); }
        book.reserve(&id);
    }

    // in-pack name → final (possibly renamed) agent id.
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let by_name: BTreeMap<&str, &manifest::ExpertAgent> =
        e.agents.iter().map(|a| (a.name.as_str(), a)).collect();

    let mut agents = Vec::new();
    for name in &order {
        let agent = by_name.get(name.as_str())
            .ok_or_else(|| cfg_err("找不到計畫中的員工".into()))?;
        let parent_final = if agent.reports_to.trim().is_empty() {
            ctx.attach_under.clone().unwrap_or_default()
        } else {
            id_map.get(agent.reports_to.trim()).cloned().unwrap_or_default()
        };
        let placement = Placement { reports_to: &parent_final, suffix: &suffix };
        let planned = plan_native_agent(ctx, dir, agent, &profiles[&agent.name], &placement, &mut book, report)?;
        id_map.insert(name.clone(), planned.scaffold.name.clone());
        agents.push(planned);
    }
    record.agent_ids = id_map;
    let mut wiki = snapshot_native_tree(&dir.join("wiki"))?;
    wiki.retain(|rel, bytes| {
        if ctx.home.join("shared/wiki").join(rel).exists() {
            report.conflict("wiki", rel, "已存在同名頁面，不覆蓋");
            return false;
        }
        let Ok(content) = std::str::from_utf8(bytes) else {
            report.skipped("wiki", rel, "不是 UTF-8 文字");
            return false;
        };
        if let ScanVerdict::Blocked(reason) = scan_external(content) {
            report.skipped("wiki", rel, format!("security: {reason}"));
            return false;
        }
        true
    });
    let hooks = snapshot_native_tree(&dir.join("hooks"))?;
    let planned_bytes = agents.iter().flat_map(|a| a.files.values())
        .chain(wiki.values()).chain(hooks.values())
        .fold(0u64, |total, bytes| total.saturating_add(bytes.len() as u64));
    if planned_bytes > safe_zip::MAX_UNPACK_BYTES {
        return Err(cfg_err("安裝計畫檔案超過上限".into()));
    }
    let bundle = super::agent_config::ir::ImportBundle {
        format_code: "native-pack", agents: agents.iter().map(|a| a.profile.clone()).collect(), ..Default::default()
    };
    let effects = serde_json::json!({
        "slug": slug, "display_name": record.display_name, "version": record.version,
        "description": record.description, "agent_ids": record.agent_ids,
        "agents": agents.iter().map(|a| serde_json::json!({
            "id": a.scaffold.name, "department": a.department,
            "files": native_file_hashes(&format!("agents/{}", a.scaffold.name), &a.files),
            "skills": a.skills,
        })).collect::<Vec<_>>(),
        "wiki_files": native_file_hashes("shared/wiki", &wiki),
        "hooks": {"files": native_file_hashes(&format!("experts/{slug}/hooks-disabled"), &hooks),
            "enabled_files": if ctx.trust_hooks { native_file_hashes(&format!("experts/{slug}/hooks"), &hooks) } else { Vec::new() },
            "trust_hooks": ctx.trust_hooks},
    });
    let effects = super::agent_config::preview::bind_context(ctx, effects)?;
    super::agent_config::preview::require_plan_confirmation(
        &bundle, effects, ctx.dry_run, ctx.confirm_profile.as_deref(), report,
    )?;
    ctx.ensure_quota(agents.len()).await?;
    ctx.begin_writes()?;
    if !ctx.dry_run && !hooks.is_empty() {
        for dest in [super::hooks::disabled_dir(&ctx.home, &slug), super::hooks::enabled_dir(&ctx.home, &slug)] {
            match std::fs::symlink_metadata(dest) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(cfg_err("Hooks 安裝目的地已存在或無法檢查，未寫入".into())),
            }
        }
    }
    for planned in agents {
        if ctx.stopping() { return Err(interrupted()); }
        let id = &planned.scaffold.name;
        if !ctx.dry_run {
            ctx.note_agent(id);
            crate::scaffold_planned_agent_dir(&ctx.home, &planned.scaffold, &planned.files).await?;
            duduclaw_core::org_store::upsert(&ctx.home, id,
                duduclaw_core::OrgEntry::new(&planned.scaffold.reports_to, &planned.department))
                .map_err(|e| cfg_err(format!("組織資料寫入失敗：{e}")))?;
            if !planned.department.is_empty() {
                std::fs::create_dir_all(ctx.home.join("shared/wiki")
                    .join(duduclaw_core::DEPARTMENTS_NAMESPACE).join(&planned.department))
                    .map_err(|e| cfg_err(format!("部門 wiki 目錄建立失敗：{e}")))?;
            }
        }
        report.imported("agent", id);
        record.agents.push(id.clone());
        record.agent_skills.extend(planned.skills.iter().map(|skill| format!("{id}/{skill}")));
    }
    for (rel, bytes) in wiki {
        if !ctx.dry_run {
            let dest = ctx.home.join("shared/wiki").join(&rel);
            ctx.note_path(&dest);
            write_planned_file(&ctx.home, &format!("shared/wiki/{rel}"), &bytes)?;
        }
        report.imported("wiki", &rel);
        record.wiki_files.push(rel);
    }
    // The hooks lifecycle consumes only the reviewed snapshot. Approval ids and
    // timestamps are execution records; hook bytes and enablement are bound above.
    if !hooks.is_empty() {
        let stage = tempfile::tempdir().map_err(|e| cfg_err(e.to_string()))?;
        for (rel, bytes) in hooks {
            write_planned_file(stage.path(), &format!("hooks/{rel}"), &bytes)?;
        }
        super::hooks::import_hooks(ctx, stage.path(), &slug, report).await;
    }

    Ok(record)
}

/// Where one agent lands: its final supervisor and the team-wide id suffix.
struct Placement<'a> {
    reports_to: &'a str,
    suffix: &'a str,
}

/// The id suffix applied to EVERY member of the pack: `""` when no member's
/// id is taken; otherwise `-imported` (with `--rename`) or the first free
/// `-2`, `-3`, … for which no member collides. Reported once.
fn team_id_suffix(
    ctx: &InstallCtx,
    agents: &[manifest::ExpertAgent],
    report: &mut Report,
) -> String {
    let bases: Vec<String> = agents
        .iter()
        .map(|a| crate::sanitize_agent_id(&a.name))
        .filter(|b| crate::is_valid_agent_id(b))
        .collect();
    let book = ctx.name_book();
    let free = |suffix: &str| {
        bases.iter().all(|b| {
            let id = crate::sanitize_agent_id(&format!("{b}{suffix}"));
            crate::is_valid_agent_id(&id) && !ctx.agent_taken(&id) && !book.is_taken(&id)
        })
    };
    if free("") {
        return String::new();
    }
    let candidates = (if ctx.rename { vec!["-imported".to_string()] } else { Vec::new() })
        .into_iter()
        .chain((2..100).map(|n| format!("-{n}")));
    for suffix in candidates {
        if free(&suffix) {
            report.warning(
                "agents",
                "id",
                format!("有成員代號已被使用，整組代號改加「{suffix}」"),
            );
            return suffix;
        }
    }
    String::new() // nothing free: per-member conflicts are reported below
}

struct NativeAgentPlan {
    scaffold: crate::AgentScaffold,
    profile: super::agent_config::ir::AgentDraft,
    files: BTreeMap<String, Vec<u8>>,
    department: String,
    skills: Vec<String>,
}

/// Resolve every destination and render every employee file before confirmation.
fn plan_native_agent(
    ctx: &InstallCtx,
    pack_dir: &Path,
    agent: &manifest::ExpertAgent,
    profile: &super::agent_config::ir::AgentDraft,
    placement: &Placement<'_>,
    book: &mut duduclaw_core::agent_names::NameBook,
    report: &mut Report,
) -> Result<NativeAgentPlan> {
    let reports_to_final = placement.reports_to;
    let base = crate::sanitize_agent_id(&agent.name);
    if !crate::is_valid_agent_id(&base) {
        report.skipped("agent", &agent.name, "無法轉為合法 agent id");
        return Err(cfg_err("員工代號無效或已被使用，請重新預覽".into()));
    }
    let final_id = crate::sanitize_agent_id(&format!("{base}{}", placement.suffix));
    if !crate::is_valid_agent_id(&final_id) || ctx.agent_taken(&final_id) {
        report.conflict("agent", &final_id, "代號已被使用，這位沒有安裝");
        return Err(cfg_err("員工代號無效或已被使用，請重新預覽".into()));
    }

    let soul_body = profile.soul.as_ref().map(|doc| doc.text.clone());

    let wanted_display = if agent.display_name.trim().is_empty() {
        agent.name.clone()
    } else {
        agent.display_name.clone()
    };
    // `@display` and `@id` both route: never reuse a name another employee
    // answers to.
    let display = [wanted_display.clone(), format!("{wanted_display}（{final_id}）"),
        format!("{wanted_display}-{final_id}"), final_id.clone()]
        .into_iter().find(|name| duduclaw_core::agent_names::norm(name) == duduclaw_core::agent_names::norm(&final_id) || !book.is_taken(name))
        .ok_or_else(|| cfg_err("員工名稱衝突，請重新預覽".into()))?;
    let renamed = display != wanted_display;
    book.reserve(&display);
    book.reserve(&final_id);
    if renamed {
        report.warning(
            "agent-name",
            &final_id,
            format!("「{wanted_display}」已被其他員工使用，改叫「{display}」"),
        );
    }
    // Normalize to the canonical role string — the roster may use pack-side
    // names (`front_desk`) or free-form casing; writing an unparseable role
    // into agent.toml would brick the agent at registry load. Unknown → worker
    // (fail-safe, reported) rather than a verbatim passthrough.
    let role = match agent.role.trim() {
        "" => "worker".to_string(),
        raw => match raw.parse::<duduclaw_core::types::AgentRole>() {
            Ok(r) => r.as_str().to_string(),
            Err(_) => {
                report.warning(
                    "agent-role",
                    &base,
                    format!("未知 role '{}'，改用 worker", raw.escape_debug()),
                );
                "worker".to_string()
            }
        },
    };
    let wanted_trigger = if agent.trigger.trim().is_empty() || renamed {
        format!("@{display}")
    } else {
        agent.trigger.clone()
    };
    let same_as_display =
        duduclaw_core::agent_names::norm(&wanted_trigger) == duduclaw_core::agent_names::norm(&display);
    let trigger = if !same_as_display && book.is_taken(&wanted_trigger) {
        report.warning(
            "agent-trigger",
            &final_id,
            format!("觸發詞「{wanted_trigger}」已被其他員工使用，改用「@{display}」"),
        );
        format!("@{display}")
    } else {
        wanted_trigger
    };
    book.reserve(&trigger);

    super::agent_config::preview::validate_section_limits(profile)?;
    let mut scaffold = crate::AgentScaffold {
        name: final_id.clone(), display_name: display, role,
        reports_to: reports_to_final.to_string(), icon: "🐾".into(), trigger,
        provider: duduclaw_core::types::RuntimeType::Claude,
        model_preferred: None, soul_body,
    };
    let mut files = crate::scaffold_agent_files(&scaffold)?;
    let mut settings: toml::Table = std::str::from_utf8(&files["agent.toml"])
        .map_err(|e| cfg_err(e.to_string()))?.parse().map_err(|e| cfg_err(format!("員工設定格式不正確：{e}")))?;
    let department = agent.department.trim();
    if !department.is_empty() && duduclaw_core::is_valid_department(department) {
        settings.get_mut("agent").and_then(toml::Value::as_table_mut).unwrap()
            .insert("department".into(), toml::Value::String(department.into()));
    } else if !department.is_empty() {
        report.warning("agent-department", &final_id, "非合法部門名，略過");
    }
    merge_toml(&mut settings, &profile.settings);
    if let Some(capabilities) = settings.get("capabilities") {
        let table = capabilities.as_table().ok_or_else(|| cfg_err("工具設定不是表格".into()))?;
        for key in ["allowed_tools", "denied_tools"] {
            if let Some(value) = table.get(key) {
                let list = value.as_array().ok_or_else(|| cfg_err(format!("{key} 必須是字串清單")))?;
                if list.iter().any(|tool| tool.as_str().is_none_or(|name| name.trim().is_empty())) {
                    return Err(cfg_err(format!("{key} 含無效工具名稱")));
                }
            }
        }
    }
    let config: duduclaw_core::types::AgentConfig = toml::Value::Table(settings.clone()).try_into()
        .map_err(|e| cfg_err(format!("員工設定型別不正確：{e}")))?;
    // A partial cannot rename a resolved destination or bypass name collision checks.
    if config.agent.name != final_id || config.agent.display_name != scaffold.display_name
        || config.agent.trigger != scaffold.trigger {
        return Err(cfg_err("專家包設定不能覆寫已解析的員工代號、名稱或呼叫詞".into()));
    }
    scaffold.reports_to = config.agent.reports_to.clone();
    let department = settings.get("agent").and_then(toml::Value::as_table)
        .and_then(|a| a.get("department")).and_then(toml::Value::as_str).unwrap_or("").trim().to_string();
    if !department.is_empty() && !duduclaw_core::is_valid_department(&department) {
        return Err(cfg_err("專家包設定的部門名稱無效".into()));
    }
    files.insert("agent.toml".into(), toml::to_string_pretty(&settings)
        .map_err(|e| cfg_err(e.to_string()))?.into_bytes());
    let mut preview = profile.clone();
    preview.id_hint = final_id.clone();
    preview.display_name = config.agent.display_name;
    preview.reports_to = Some(scaffold.reports_to.clone());
    preview.model = Some(config.model.preferred);
    preview.tools = Some(config.capabilities.allowed_tools);
    preview.disallowed_tools = config.capabilities.denied_tools;
    preview.settings = settings;
    if let Some(doc) = &profile.contract {
        if let ScanVerdict::Blocked(reason) = scan_external(&doc.text) {
            return Err(cfg_err(format!("契約未通過內容安全檢查：{reason}")));
        }
        let table: toml::Table = doc.text.parse().map_err(|e| cfg_err(format!("契約格式不正確：{e}")))?;
        if table.keys().any(|key| key != "boundaries") || table.get("boundaries")
            .and_then(toml::Value::as_table).map(|b| b.keys().any(|key|
                !matches!(key.as_str(), "must_not" | "must_always" | "max_tool_calls_per_turn"))).unwrap_or(false) {
            return Err(cfg_err("契約包含未知欄位".into()));
        }
        let parsed: duduclaw_agent::contract::Contract = toml::from_str(&doc.text)
            .map_err(|e| cfg_err(format!("契約格式不正確：{e}")))?;
        let sanitized = duduclaw_agent::contract_sanitize::sanitize_boundaries(
            &parsed.boundaries, profile.soul.as_ref().map(|d| d.text.as_str()),
        );
        for (rule, why) in &sanitized.dropped {
            report.warning("contract", &final_id, format!("略過紅線「{}」（{}）", truncate(rule, 40), why.zh()));
        }
        let contract = duduclaw_agent::contract::Contract { boundaries: sanitized.boundaries };
        if !contract.boundaries.must_not.is_empty() || !contract.boundaries.must_always.is_empty() {
            let body = format!("# Installed from an expert pack.\n{}", toml::to_string_pretty(&contract)
                .map_err(|e| cfg_err(e.to_string()))?);
            files.insert("CONTRACT.toml".into(), body.as_bytes().to_vec());
            preview.contract = Some(super::agent_config::ir::Doc::new("CONTRACT.toml", body));
        } else {
            preview.contract = None;
        }
    }
    let mut skills = Vec::new();
    for skill in &agent.skills {
        if !duduclaw_agent::skill_loader::is_safe_skill_name(skill) {
            return Err(cfg_err("非安全技能名稱".into()));
        }
        let tree = snapshot_native_tree(&pack_dir.join("skills").join(skill))?;
        let Some(body) = tree.get("SKILL.md") else {
            report.ignored("skill", skill, "非包內技能（假設已安裝）");
            continue;
        };
        let body = std::str::from_utf8(body).map_err(|e| cfg_err(e.to_string()))?;
        if let ScanVerdict::Blocked(reason) = scan_external(body) {
            return Err(cfg_err(format!("技能未通過內容安全檢查：{reason}")));
        }
        for (rel, bytes) in tree {
            files.insert(format!("SKILLS/{skill}/{rel}"), bytes);
        }
        report.imported("skill", &format!("{skill} → {final_id}"));
        skills.push(skill.clone());
    }
    Ok(NativeAgentPlan { scaffold, profile: preview, files, department, skills })
}

fn native_file_hashes(prefix: &str, files: &BTreeMap<String, Vec<u8>>) -> Vec<serde_json::Value> {
    files.iter().map(|(rel, bytes)| serde_json::json!({
        "path": format!("{prefix}/{rel}"), "sha256": super::registry::sha256_hex(bytes),
    })).collect()
}

/// Bound, regular-file-only snapshots. No source path is consulted after review.
fn snapshot_native_tree(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    fn visit(base: &Path, path: &Path, out: &mut BTreeMap<String, Vec<u8>>, total: &mut u64) -> Result<()> {
        let meta = std::fs::symlink_metadata(path).map_err(|e| cfg_err(e.to_string()))?;
        if meta.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|e| cfg_err(e.to_string()))? {
                let entry = entry.map_err(|e| cfg_err(e.to_string()))?;
                let name = entry.file_name();
                let name = name.to_str().ok_or_else(|| cfg_err("來源檔案名稱不是 UTF-8".into()))?;
                if name.contains('\\') {
                    return Err(cfg_err("來源檔案名稱含不安全的分隔符號".into()));
                }
                visit(base, &entry.path(), out, total)?;
            }
        } else if meta.is_file() {
            *total = total.saturating_add(meta.len());
            if *total > safe_zip::MAX_UNPACK_BYTES || out.len() >= 10_000 {
                return Err(cfg_err("安裝計畫檔案超過上限".into()));
            }
            use std::io::Read;
            let mut bytes = Vec::new();
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let file = options.open(path).map_err(|e| cfg_err(e.to_string()))?;
            if !file.metadata().map_err(|e| cfg_err(e.to_string()))?.is_file() {
                return Err(cfg_err("來源不是一般檔案".into()));
            }
            file.take(meta.len().saturating_add(1)).read_to_end(&mut bytes).map_err(|e| cfg_err(e.to_string()))?;
            if bytes.len() as u64 != meta.len() { return Err(cfg_err("來源檔案讀取期間變更".into())); }
            let bytes = super::agent_config::credentials::mask_bytes(bytes)?;
            let relative = path.strip_prefix(base).map_err(|e| cfg_err(e.to_string()))?;
            let mut parts = Vec::new();
            for part in relative.components() {
                let std::path::Component::Normal(name) = part else {
                    return Err(cfg_err("來源檔案名稱無效".into()));
                };
                let name = name.to_str().ok_or_else(|| cfg_err("來源檔案名稱不是 UTF-8".into()))?;
                if name.contains('\\') { return Err(cfg_err("來源檔案名稱含不安全的分隔符號".into())); }
                parts.push(name);
            }
            let rel = parts.join("/");
            if out.insert(rel, bytes).is_some() { return Err(cfg_err("來源檔案路徑重複".into())); }
        } else {
            return Err(cfg_err("安裝來源含捷徑或特殊檔案".into()));
        }
        Ok(())
    }
    let mut out = BTreeMap::new();
    match std::fs::symlink_metadata(root) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(cfg_err(e.to_string())),
        Ok(meta) if !meta.is_dir() => return Err(cfg_err("安裝來源不是一般目錄".into())),
        Ok(_) => {}
    }
    visit(root, root, &mut out, &mut 0)?;
    Ok(out)
}

pub(super) fn write_planned_file(root: &Path, rel: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    if rel.contains('\\') || rel.split('/').any(|part| part.is_empty() || matches!(part, "." | ".."))
        || Path::new(rel).components().any(|part| !matches!(part, std::path::Component::Normal(_))) {
        return Err(cfg_err("無效安裝路徑".into()));
    }
    let root_meta = std::fs::symlink_metadata(root).map_err(|e| cfg_err(e.to_string()))?;
    if !root_meta.is_dir() || root_meta.file_type().is_symlink() {
        return Err(cfg_err("安裝根目錄不是一般目錄".into()));
    }
    let mut dest = root.to_path_buf();
    let mut parts = rel.split('/').peekable();
    while let Some(part) = parts.next() {
        dest.push(part);
        if parts.peek().is_none() { break; }
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) if meta.is_dir() && !meta.file_type().is_symlink() => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&dest).map_err(|e| cfg_err(format!("無法建立安裝目錄：{e}")))?;
            }
            _ => return Err(cfg_err("安裝路徑含捷徑、特殊檔案或無法檢查的目錄".into())),
        }
    }
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&dest)
        .map_err(|e| cfg_err(format!("安裝目的地已變更或無法寫入：{e}")))?;
    file.write_all(bytes).map_err(|e| cfg_err(e.to_string()))
}

// ─────────────────── Shared skill / wiki install primitives ───────────────────

/// Install a skill directory into the global `<home>/skills/<name>`. Used by
/// the plugin & single-skill importers (their skills are pack-level, not
/// per-agent-referenced). Records the name for removal only when this install
/// actually created it (a pre-existing skill is left untouched → CONFLICT).
pub(super) fn install_skill_global(
    ctx: &InstallCtx,
    src: &Path,
    name: &str,
    report: &mut Report,
    record: &mut InstallRecord,
) {
    let skill_md = src.join("SKILL.md");
    let content = match std::fs::read_to_string(&skill_md) {
        Ok(c) => c,
        Err(_) => {
            report.skipped("skill", name, "找不到 SKILL.md");
            return;
        }
    };
    if !duduclaw_agent::skill_loader::is_safe_skill_name(name) {
        report.skipped("skill", name, "非安全技能名稱");
        return;
    }
    if let ScanVerdict::Blocked(reason) = scan_external(&content) {
        report.skipped("skill", name, format!("security: {reason}"));
        return;
    }
    let dest = ctx.home.join("skills").join(name);
    if dest.exists() {
        report.conflict("skill", name, "全域已有同名技能，不覆蓋");
        return;
    }
    if ctx.dry_run {
        report.imported("skill", name);
        return;
    }
    ctx.note_path(&dest);
    match copy_dir(src, &dest) {
        Ok(()) => {
            report.imported("skill", name);
            record.global_skills.push(name.to_string());
        }
        Err(err) => report.skipped("skill", name, format!("複製失敗: {err}")),
    }
}

// ─────────────────────────── export ───────────────────────────

/// `expert export <slug> --format claude-plugin` (P0 subset).
pub async fn cmd_export(home: &Path, slug: &str, format: &str, out: Option<&Path>) -> Result<()> {
    if format != "claude-plugin" {
        return Err(cfg_err(format!(
            "尚未支援匯出格式 '{format}'（目前僅 claude-plugin）"
        )));
    }
    let Some(rec) = super::read_record(home, slug) else {
        return Err(cfg_err(format!(
            "找不到已安裝的專家包 '{slug}'（用 `duduclaw expert list` 查看）"
        )));
    };

    let default_out = PathBuf::from(format!("{slug}-claude-plugin"));
    let out_dir = out.unwrap_or(&default_out);
    std::fs::create_dir_all(out_dir.join(".claude-plugin"))
        .map_err(|e| io_err(format!("建立輸出目錄失敗: {e}")))?;
    let agents_out = out_dir.join("agents");
    std::fs::create_dir_all(&agents_out)
        .map_err(|e| io_err(format!("建立 agents 目錄失敗: {e}")))?;

    // plugin.json — superset DuDuClaw fields go under `x-duduclaw` (official
    // Claude Code guarantees unknown keys are ignored).
    let plugin_json = serde_json::json!({
        "name": rec.slug,
        "description": rec.display_name,
        "version": if rec.version.is_empty() { "0.0.0" } else { &rec.version },
        "x-duduclaw": {
            "kind": rec.kind,
            "agents": rec.agents,
            "exportedAt": now_iso(),
        }
    });
    std::fs::write(
        out_dir.join(".claude-plugin").join("plugin.json"),
        serde_json::to_string_pretty(&plugin_json)
            .map_err(|e| cfg_err(format!("序列化 plugin.json 失敗: {e}")))?,
    )
    .map_err(|e| io_err(format!("寫入 plugin.json 失敗: {e}")))?;

    // Each installed agent → agents/<id>.md with frontmatter + SOUL body.
    // Aggregate every agent's non-duduclaw MCP servers into one plugin-level
    // `.mcp.json` (the Claude Code plugin convention); the wired `duduclaw`
    // server is stripped on the way out (it is DuDuClaw-internal).
    let mut exported = 0usize;
    let mut plugin_servers = serde_json::Map::new();
    for agent_id in &rec.agents {
        let agent_dir = home.join("agents").join(agent_id);
        let soul = std::fs::read_to_string(agent_dir.join("SOUL.md")).unwrap_or_default();
        let sections = duduclaw_core::agent_toml::load(&agent_dir);
        let (model, tools, disallowed) = extract_agent_frontmatter(&sections);
        let mut fm = String::from("---\n");
        fm.push_str(&format!("name: {agent_id}\n"));
        if let Some(m) = model {
            fm.push_str(&format!("model: {m}\n"));
        }
        if !tools.is_empty() {
            fm.push_str(&format!("tools: {}\n", tools.join(", ")));
        }
        if !disallowed.is_empty() {
            fm.push_str(&format!("disallowedTools: {}\n", disallowed.join(", ")));
        }
        fm.push_str("---\n\n");
        fm.push_str(&soul);
        std::fs::write(agents_out.join(format!("{agent_id}.md")), fm)
            .map_err(|e| io_err(format!("寫入 agent 匯出失敗: {e}")))?;
        exported += 1;

        if let Some(servers) = super::read_mcp_servers(&agent_dir.join(".mcp.json")) {
            for (k, v) in servers {
                if k != "duduclaw" {
                    plugin_servers.entry(k).or_insert(v);
                }
            }
        }
    }

    if !plugin_servers.is_empty() {
        let mcp_doc = serde_json::json!({ "mcpServers": plugin_servers });
        std::fs::write(
            out_dir.join(".mcp.json"),
            serde_json::to_string_pretty(&mcp_doc)
                .map_err(|e| cfg_err(format!("序列化 .mcp.json 失敗: {e}")))?,
        )
        .map_err(|e| io_err(format!("寫入 .mcp.json 失敗: {e}")))?;
    }

    println!(
        "\n  {} 已匯出 {} 個 agent 為 claude-plugin → {}\n",
        style("✓").green(),
        exported,
        out_dir.display()
    );
    Ok(())
}

/// Pull `[model] preferred` + `[capabilities] allowed/denied_tools` back out
/// of an agent.toml for plugin-format frontmatter.
///
/// Goes through the shared typed parse point
/// ([`duduclaw_core::agent_toml`]) rather than a hand-rolled `toml::Value`
/// walk. Missing directions are unchanged: an absent/unreadable/malformed
/// file, an absent section, or a wrong-typed value all yield `None` / an
/// empty list, and a mixed array still keeps only its string elements.
fn extract_agent_frontmatter(
    sections: &duduclaw_core::agent_toml::AgentTomlSections,
) -> (Option<String>, Vec<String>, Vec<String>) {
    (
        sections.model.preferred.clone(),
        sections.capabilities.allowed_tools.clone(),
        sections.capabilities.denied_tools.clone(),
    )
}

// ─────────────────────────── small utils ───────────────────────────

fn which_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
        #[cfg(windows)]
        {
            let exe = dir.join(format!("{bin}.exe"));
            if exe.is_file() {
                return Some(exe);
            }
        }
    }
    None
}

fn truncate(s: &str, max_chars: usize) -> String {
    duduclaw_core::truncate_chars(s, max_chars)
}

/// RAII cleanup of the staging temp dir.
struct Cleanup(PathBuf);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// R5: `extract_agent_frontmatter`'s directions, pinned.
///
/// This reader feeds Claude Code plugin frontmatter, and every field is
/// fail-*open* (absent ⇒ the key is simply omitted from the frontmatter):
/// a missing model line or an empty tools line is a benign export, whereas
/// refusing to export would strand the whole plugin bundle over one typo.
///
/// The mixed-array case is the one worth pinning: the raw walk used
/// `filter_map(as_str)` and kept the good elements, so `allowed_tools` /
/// `denied_tools` are read through `lenient::string_vec` rather than a strict
/// `Vec<String>` — which also means a stray element no longer fails
/// `AgentConfig` outright and deletes the agent from the registry.
#[cfg(test)]
mod frontmatter_direction_tests {
    use super::extract_agent_frontmatter;

    fn fm(body: &str) -> (Option<String>, Vec<String>, Vec<String>) {
        extract_agent_frontmatter(&duduclaw_core::agent_toml::parse(body))
    }

    #[test]
    fn default_direction_frontmatter_omits_what_it_cannot_read() {
        for body in [
            "",                                           // empty file
            "[agent]\nname = \"a\"\n",                    // unrelated section only
            "[model]\n",                                  // section, no key
            "[model]\npreferred = 42\n",                  // wrong type
            "model = \"scalar\"\n",                       // wrong-typed section
            "[capabilities]\nallowed_tools = \"Bash\"\n", // non-array
            "not toml [[[",                               // malformed file
        ] {
            let (model, tools, disallowed) = fm(body);
            if body.contains("preferred = 42") || !body.contains("preferred") {
                assert!(model.is_none(), "model omitted for {body:?}");
            }
            assert!(tools.is_empty(), "tools omitted for {body:?}");
            assert!(
                disallowed.is_empty(),
                "disallowedTools omitted for {body:?}"
            );
        }
    }

    #[test]
    fn default_direction_frontmatter_filters_mixed_arrays_instead_of_dropping_them() {
        let (model, tools, disallowed) = fm("[model]\npreferred = \"claude-sonnet-4-6\"\n\
             [capabilities]\n\
             allowed_tools = [\"Bash\", 7, \"Read\"]\n\
             denied_tools = [\"WebFetch\"]\n");
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(tools, vec!["Bash".to_string(), "Read".to_string()]);
        assert_eq!(disallowed, vec!["WebFetch".to_string()]);
    }
}

#[cfg(test)]
mod native_plan_tests {
    use super::*;

    fn ctx(home: &Path) -> InstallCtx {
        InstallCtx {
            home: home.to_path_buf(), dry_run: true, confirm_profile: None,
            rename: false, trust_hooks: false, attach_under: None,
            reimport: Default::default(), replace: None, agent_cap: None,
            replace_state: Default::default(),
        }
    }

    fn source(root: &Path) {
        std::fs::create_dir_all(root.join("agents/helper")).unwrap();
        std::fs::create_dir_all(root.join("skills/review")).unwrap();
        std::fs::write(root.join("expert.toml"),
            "[expert]\nname = \"plan-test\"\n[[expert.agents]]\nname = \"helper\"\nskills = [\"review\"]\n").unwrap();
        std::fs::write(root.join("agents/helper/soul.md"), "Helpful employee.").unwrap();
        std::fs::write(root.join("agents/helper/agent.partial.toml"),
            "[capabilities]\ndenied_tools = [\"Bash\"]\n").unwrap();
        std::fs::write(root.join("skills/review/SKILL.md"), "Review the supplied material.").unwrap();
    }

    async fn preview(ctx: &InstallCtx, root: &Path) -> serde_json::Value {
        let mut report = Report::default();
        install_native(ctx, root, &mut report).await.unwrap();
        report.to_json(true)["details"].clone()
    }

    // Not run in this security-fix session: the owner prohibited builds/tests.
    #[tokio::test]
    async fn native_confirmation_rejects_changed_tools_skill_and_collision_plan() {
        for change in ["tools", "skill", "collision", "parent", "model"] {
            let home = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            source(root.path());
            let mut context = ctx(home.path());
            let old = preview(&context, root.path()).await;
            match change {
                "tools" => std::fs::write(root.path().join("agents/helper/agent.partial.toml"),
                    "[capabilities]\ndenied_tools = []\n").unwrap(),
                "skill" => std::fs::write(root.path().join("skills/review/SKILL.md"),
                    "Review the amended material.").unwrap(),
                "collision" => {
                    std::fs::create_dir_all(home.path().join("agents/helper")).unwrap();
                    std::fs::write(home.path().join("agents/helper/agent.toml"),
                        "[agent]\nname = \"helper\"\n").unwrap();
                }
                "parent" => context.attach_under = Some("supervisor".into()),
                _ => std::fs::write(root.path().join("agents/helper/agent.partial.toml"),
                    "[model]\npreferred = \"claude-opus-4-5\"\n").unwrap(),
            }
            let new = preview(&context, root.path()).await;
            assert_ne!(old["profile_confirmation"], new["profile_confirmation"], "{change}");
            context.dry_run = false;
            context.confirm_profile = old["profile_confirmation"].as_str().map(str::to_string);
            assert!(install_native(&context, root.path(), &mut Report::default()).await.is_err(), "{change}");
            assert!(!home.path().join("agents/helper/SOUL.md").exists(), "{change}");
            assert!(!home.path().join("agents/helper-2").exists(), "{change}");
        }
    }

    #[tokio::test]
    async fn native_confirmation_writes_exact_reviewed_files() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        source(root.path());
        let mut context = ctx(home.path());
        let reviewed = preview(&context, root.path()).await;
        context.dry_run = false;
        context.confirm_profile = reviewed["profile_confirmation"].as_str().map(str::to_string);
        install_native(&context, root.path(), &mut Report::default()).await.unwrap();
        for file in reviewed["profile_install_plan"]["agents"][0]["files"].as_array().unwrap() {
            let bytes = std::fs::read(home.path().join(file["path"].as_str().unwrap())).unwrap();
            assert_eq!(super::super::registry::sha256_hex(&bytes), file["sha256"].as_str().unwrap());
        }
        assert_eq!(reviewed["profile_preview"][0]["disallowed_tools"], serde_json::json!(["Bash"]));
    }

    #[tokio::test]
    async fn native_pack_import_rejects_unknown_contract_fields() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        source(root.path());
        std::fs::write(root.path().join("agents/helper/CONTRACT.toml"),
            "[boundaries]\nmust_always = [\"State uncertainty\"]\nunknown = true\n").unwrap();
        assert!(install_native(&ctx(home.path()), root.path(), &mut Report::default()).await.is_err());
        assert!(!home.path().join("agents").exists());
    }

    #[cfg(unix)]
    #[test]
    fn native_plan_refuses_linked_skill_files() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(other.path(), root.path().join("SKILL.md")).unwrap();
        assert!(snapshot_native_tree(root.path()).is_err());
    }

    // NOT RUN: builds and tests were prohibited during this security fix.
    #[cfg(unix)]
    #[test]
    fn native_snapshot_refuses_backslash_traversal_files_and_directories() {
        for directory in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("..\\..\\outside.md");
            if directory { std::fs::create_dir(path).unwrap(); }
            else { std::fs::write(path, "outside").unwrap(); }
            assert!(snapshot_native_tree(root.path()).is_err());
        }
    }

    // NOT RUN: builds and tests were prohibited during this security fix.
    #[test]
    fn native_writer_refuses_non_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        for path in ["../outside.md", "..\\outside.md", "/outside.md", "wiki/../../outside.md", "wiki/./page.md"] {
            assert!(write_planned_file(root.path(), path, b"outside").is_err(), "{path}");
        }
        assert!(!root.path().join("wiki").exists());
    }

    // NOT RUN: builds and tests were prohibited during this security fix.
    #[cfg(unix)]
    #[test]
    fn native_writer_refuses_symlinked_parent_directories() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("shared")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("shared/wiki")).unwrap();
        assert!(write_planned_file(root.path(), "shared/wiki/outside.md", b"outside").is_err());
        assert!(!outside.path().join("outside.md").exists());
    }
}
