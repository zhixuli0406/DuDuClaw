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
    crate::pack_cmd::refuse_in_ai_session(&crate::ai_session_guard::markers())?;
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
        println!(
            "  {} 以上為 --dry-run 計畫，未寫入任何檔案；移除該旗標即實際安裝。\n",
            style("ℹ").cyan()
        );
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
            if !path.exists() { return Ok(None); }
            let text = super::agent_config::md::read_text(&path)
                .ok_or_else(|| cfg_err(format!("無法讀取專家包區段 {file}")))?;
            Ok(Some(super::agent_config::ir::Doc::new(file, text)))
        };
        let settings = match read_doc("agent.partial.toml")? {
            Some(doc) => toml::from_str(&doc.text).map_err(|e| cfg_err(format!("專家包設定格式不正確：{e}")))?,
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
    let bundle = super::agent_config::ir::ImportBundle {
        format_code: "native-pack", agents: profiles.values().cloned().collect(), ..Default::default()
    };
    super::agent_config::preview::require_confirmation(&bundle, ctx.dry_run, ctx.confirm_profile.as_deref(), report)?;
    let slug = ctx.claim_slug(&e.name)?;
    let adding = e
        .agents
        .iter()
        .filter(|a| crate::is_valid_agent_id(&crate::sanitize_agent_id(&a.name)))
        .count();
    ctx.ensure_quota(adding).await?;
    ctx.begin_writes()?;

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

    // in-pack name → final (possibly renamed) agent id.
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let by_name: BTreeMap<&str, &manifest::ExpertAgent> =
        e.agents.iter().map(|a| (a.name.as_str(), a)).collect();

    for name in &order {
        if ctx.stopping() {
            break;
        }
        let Some(agent) = by_name.get(name.as_str()) else {
            continue;
        };
        let parent_final = if agent.reports_to.trim().is_empty() {
            // Pack root — attach under the operator-chosen supervisor (already
            // validated to exist), or stay a root as before.
            ctx.attach_under.clone().unwrap_or_default()
        } else {
            id_map
                .get(agent.reports_to.trim())
                .cloned()
                .unwrap_or_default()
        };
        let placement = Placement {
            reports_to: &parent_final,
            suffix: &suffix,
        };
        if let Some(final_id) =
            install_one_agent(ctx, dir, agent, &profiles[&agent.name], &placement, &mut book, report, &mut record).await?
        {
            id_map.insert(name.clone(), final_id);
        } else if e.agents.iter().any(|a| a.reports_to.trim() == name.as_str()) {
            report.warning(
                "agents",
                name,
                "這位沒有安裝，原本向他回報的成員會變成最上層",
            );
        }
    }
    record.agent_ids = id_map.clone();

    // ── wiki ──
    let wiki_dir = dir.join("wiki");
    if wiki_dir.is_dir() {
        install_wiki_tree(ctx, &wiki_dir, report, &mut record);
    }

    // ── hooks/ → quarantined; enablement is ApprovalBroker-gated ──
    super::hooks::import_hooks(ctx, dir, &slug, report).await;

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
    let free = |suffix: &str| {
        bases.iter().all(|b| {
            let id = crate::sanitize_agent_id(&format!("{b}{suffix}"));
            crate::is_valid_agent_id(&id) && !ctx.agent_taken(&id)
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

/// Scaffold one native pack agent (soul + partial merge + skills).
async fn install_one_agent(
    ctx: &InstallCtx,
    pack_dir: &Path,
    agent: &manifest::ExpertAgent,
    profile: &super::agent_config::ir::AgentDraft,
    placement: &Placement<'_>,
    book: &mut duduclaw_core::agent_names::NameBook,
    report: &mut Report,
    record: &mut InstallRecord,
) -> Result<Option<String>> {
    let reports_to_final = placement.reports_to;
    let base = crate::sanitize_agent_id(&agent.name);
    if !crate::is_valid_agent_id(&base) {
        report.skipped("agent", &agent.name, "無法轉為合法 agent id");
        return Ok(None);
    }
    let final_id = crate::sanitize_agent_id(&format!("{base}{}", placement.suffix));
    if !crate::is_valid_agent_id(&final_id) || ctx.agent_taken(&final_id) {
        report.conflict("agent", &final_id, "代號已被使用，這位沒有安裝");
        return Ok(None);
    }

    let soul_body = profile.soul.as_ref().map(|doc| doc.text.clone());

    let wanted_display = if agent.display_name.trim().is_empty() {
        agent.name.clone()
    } else {
        agent.display_name.clone()
    };
    // `@display` and `@id` both route: never reuse a name another employee
    // answers to.
    let (display, renamed) = book.unique_display(&wanted_display, &final_id);
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

    if ctx.dry_run {
        report.imported("agent", &final_id);
        if !agent.department.trim().is_empty() {
            report.imported(
                "agent-department",
                &format!("{final_id} → {}", agent.department.trim()),
            );
        }
        // Report referenced skills in plan mode too.
        plan_agent_skills(pack_dir, agent, report);
        if pack_dir
            .join("agents")
            .join(&agent.name)
            .join("CONTRACT.toml")
            .is_file()
        {
            report.imported("contract", &final_id);
        }
        return Ok(Some(final_id));
    }

    let scaffold = crate::AgentScaffold {
        name: final_id.clone(),
        display_name: display,
        role,
        reports_to: reports_to_final.to_string(),
        icon: "🐾".to_string(),
        trigger,
        provider: duduclaw_core::types::RuntimeType::Claude,
        model_preferred: None,
        soul_body,
    };
    ctx.note_agent(&final_id);
    crate::scaffold_agent_dir(&ctx.home, &scaffold).await?;
    report.imported("agent", &final_id);
    record.agents.push(final_id.clone());

    // ── Org placement: `[agent] department` + its shared-wiki space. ──
    // Invalid names warn-and-skip (the rest of the agent still installs);
    // `expert pack` already rejects them at authoring time.
    let department = agent.department.trim();
    if !department.is_empty() {
        if duduclaw_core::is_valid_department(department) {
            let dept = department.to_string();
            super::patch_agent_toml(&ctx.home, &final_id, |t| {
                if let Some(a) = t.get_mut("agent").and_then(|v| v.as_table_mut()) {
                    a.insert("department".into(), toml::Value::String(dept.clone()));
                }
            })?;
            let wiki_dir = ctx
                .home
                .join("shared")
                .join("wiki")
                .join(duduclaw_core::DEPARTMENTS_NAMESPACE)
                .join(department);
            if let Err(e) = std::fs::create_dir_all(&wiki_dir) {
                report.warning(
                    "agent-department",
                    &final_id,
                    format!("部門 wiki 目錄建立失敗: {e}"),
                );
            }
            report.imported("agent-department", &format!("{final_id} → {department}"));
        } else {
            report.warning(
                "agent-department",
                &final_id,
                format!(
                    "department '{}' 非合法部門名，略過",
                    department.escape_debug()
                ),
            );
        }
    }

    if !profile.settings.is_empty() {
        super::patch_agent_toml(&ctx.home, &final_id, |table| merge_toml(table, &profile.settings))?;
        report.imported("agent-settings", &final_id);
    }

    // WP22 T1 — re-record the authoritative org placement from the finished
    // `agent.toml`. `scaffold_agent_dir` already stored `reports_to`, but two
    // later steps can still move this agent: the `[agent] department` patch
    // above, and the `agent.partial.toml` overlay (which is *not* section-
    // filtered on the install side, so a pack may legitimately carry org
    // fields). Adopting the final mirror once, here, is the only way the store
    // and the file agree at the end of a pack install — otherwise `duduclaw
    // doctor` would report drift on every freshly installed pack.
    if let Some(entry) = duduclaw_core::org_store::read_mirror(
        &ctx.home.join("agents").join(&final_id).join("agent.toml"),
    ) {
        if let Err(e) = duduclaw_core::org_store::upsert(&ctx.home, &final_id, entry) {
            report.warning(
                "agent-department",
                &final_id,
                format!("組織資料寫入失敗: {e}"),
            );
        }
    }

    let pack_mcp = pack_dir.join("agents").join(&agent.name).join(".mcp.json");
    if let Some(servers) = super::read_mcp_servers(&pack_mcp) {
        for name in servers.keys() {
            report.skipped("mcp-servers", name, "匯入不新增 MCP 伺服器，請由管理者另行設定");
        }
    }

    // Install referenced skills into this agent's SKILLS/.
    install_agent_skills(ctx, pack_dir, agent, &final_id, report, record).await;

    // CONTRACT.toml (behavioural red lines).
    install_agent_contract(ctx, profile, &final_id, report);

    Ok(Some(final_id))
}

/// Install a pack agent's `CONTRACT.toml`: scanned like any pack text, parsed
/// with the runtime parser, and re-rendered so only `[boundaries]` lands
/// (unknown sections are dropped rather than carried). A freshly scaffolded
/// agent has no contract, so an existing file is a conflict, never
/// overwritten.
fn install_agent_contract(
    ctx: &InstallCtx,
    profile: &super::agent_config::ir::AgentDraft,
    final_id: &str,
    report: &mut Report,
) {
    let Some(doc) = &profile.contract else {
        return; // no contract in the pack — nothing to do
    };
    let raw = &doc.text;
    if let ScanVerdict::Blocked(reason) = scan_external(&raw) {
        report.skipped("contract", final_id, format!("security: {reason}"));
        return;
    }
    let parsed: duduclaw_agent::contract::Contract = match toml::from_str(&raw) {
        Ok(c) => c,
        Err(e) => {
            report.skipped("contract", final_id, format!("解析失敗: {e}"));
            return;
        }
    };
    // Same sanitizer as AI-generated teams: `must_not` also blocks channel
    // replies verbatim, so wildcard / keyword rules and rules that repeat the
    // employee's own handoff wording are dropped — and reported, not silent.
    let soul = profile.soul.as_ref().map(|doc| doc.text.as_str());
    let sanitized =
        duduclaw_agent::contract_sanitize::sanitize_boundaries(&parsed.boundaries, soul);
    if !sanitized.dropped.is_empty() {
        let list: Vec<String> = sanitized
            .dropped
            .iter()
            .map(|(rule, why)| format!("「{}」（{}）", truncate(rule, 40), why.zh()))
            .collect();
        report.warning(
            "contract",
            final_id,
            format!("略過 {} 條紅線：{}", list.len(), list.join("、")),
        );
    }
    let contract = duduclaw_agent::contract::Contract {
        boundaries: sanitized.boundaries,
    };
    if contract.boundaries.must_not.is_empty() && contract.boundaries.must_always.is_empty() {
        report.skipped("contract", final_id, "整理後沒有可用的紅線");
        return;
    }
    let body = match toml::to_string_pretty(&contract) {
        Ok(b) => b,
        Err(e) => {
            report.skipped("contract", final_id, format!("序列化失敗: {e}"));
            return;
        }
    };
    let dest = ctx
        .home
        .join("agents")
        .join(final_id)
        .join("CONTRACT.toml");
    if dest.exists() {
        report.conflict("contract", final_id, "已有 CONTRACT.toml，不覆蓋");
        return;
    }
    match std::fs::write(&dest, format!("# Installed from an expert pack.\n{body}")) {
        Ok(()) => report.imported("contract", final_id),
        Err(e) => report.skipped("contract", final_id, format!("寫入失敗: {e}")),
    }
}

/// Copy each referenced pack skill into `<home>/agents/<id>/SKILLS/<name>`
/// after scanning it. External (non-pack) references are noted, not fetched.
async fn install_agent_skills(
    ctx: &InstallCtx,
    pack_dir: &Path,
    agent: &manifest::ExpertAgent,
    final_id: &str,
    report: &mut Report,
    record: &mut InstallRecord,
) {
    for skill in &agent.skills {
        let src = pack_dir.join("skills").join(skill);
        if !src.join("SKILL.md").is_file() {
            report.ignored("skill", skill, "非包內技能（假設已安裝）");
            continue;
        }
        if !duduclaw_agent::skill_loader::is_safe_skill_name(skill) {
            report.skipped("skill", skill, "非安全技能名稱");
            continue;
        }
        let content = std::fs::read_to_string(src.join("SKILL.md")).unwrap_or_default();
        if let ScanVerdict::Blocked(reason) = scan_external(&content) {
            report.skipped("skill", skill, format!("security: {reason}"));
            continue;
        }
        let dest = ctx
            .home
            .join("agents")
            .join(final_id)
            .join("SKILLS")
            .join(skill);
        match copy_dir(&src, &dest) {
            Ok(()) => {
                report.imported("skill", &format!("{skill} → {final_id}"));
                record.agent_skills.push(format!("{final_id}/{skill}"));
            }
            Err(err) => report.skipped("skill", skill, format!("複製失敗: {err}")),
        }
    }
}

/// Dry-run reporting of referenced skills.
fn plan_agent_skills(pack_dir: &Path, agent: &manifest::ExpertAgent, report: &mut Report) {
    for skill in &agent.skills {
        if pack_dir
            .join("skills")
            .join(skill)
            .join("SKILL.md")
            .is_file()
        {
            report.imported("skill", &format!("{skill} → {}", agent.name));
        } else {
            report.ignored("skill", skill, "非包內技能（假設已安裝）");
        }
    }
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

/// Install a wiki tree into `<home>/shared/wiki/<rel>`, scanning each page.
/// Never overwrites an existing page (→ CONFLICT). Records written paths.
pub(super) fn install_wiki_tree(
    ctx: &InstallCtx,
    wiki_src: &Path,
    report: &mut Report,
    record: &mut InstallRecord,
) {
    let mut files = Vec::new();
    collect_rel_files(wiki_src, wiki_src, &mut files);
    let wiki_root = ctx.home.join("shared").join("wiki");
    for rel in files {
        if ctx.stopping() {
            break;
        }
        let src = wiki_src.join(&rel);
        let rel_str = rel.to_string_lossy().replace('\\', "/");
        let content = std::fs::read_to_string(&src).unwrap_or_default();
        if let ScanVerdict::Blocked(reason) = scan_external(&content) {
            report.skipped("wiki", &rel_str, format!("security: {reason}"));
            continue;
        }
        let dest = wiki_root.join(&rel);
        if dest.exists() {
            report.conflict("wiki", &rel_str, "已存在同名頁面，不覆蓋");
            continue;
        }
        if ctx.dry_run {
            report.imported("wiki", &rel_str);
            continue;
        }
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        ctx.note_path(&dest);
        match std::fs::copy(&src, &dest) {
            Ok(_) => {
                report.imported("wiki", &rel_str);
                record.wiki_files.push(rel_str);
            }
            Err(err) => report.skipped("wiki", &rel_str, format!("複製失敗: {err}")),
        }
    }
}

/// Collect regular files under `dir` as paths relative to `base` (skips links).
fn collect_rel_files(base: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for entry in rd.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() {
                continue;
            }
            let path = entry.path();
            if ft.is_dir() {
                collect_rel_files(base, &path, out);
            } else if ft.is_file()
                && let Ok(rel) = path.strip_prefix(base)
            {
                out.push(rel.to_path_buf());
            }
        }
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
