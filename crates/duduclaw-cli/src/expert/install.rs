//! `expert install` — source resolution, format dispatch, and the native
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
    pub rename: bool,
    /// Explicit operator grant for pack hooks (`--trust-hooks`). Without it
    /// hooks land disabled behind an ApprovalBroker request (fail-closed).
    pub trust_hooks: bool,
    /// Existing agent id to attach the pack's root agents under
    /// (`--attach-under`). Validated to exist BEFORE anything installs
    /// (fail-closed); pack roots then scaffold with `reports_to` set to it.
    pub attach_under: Option<String>,
}

/// Entry for `duduclaw expert install`.
pub async fn cmd_install(
    home: &Path,
    source: &str,
    dry_run: bool,
    rename: bool,
    trust_hooks: bool,
    attach_under: Option<String>,
) -> Result<()> {
    // ── 1. Resolve the source to an on-disk directory (download / unzip). ──
    // A throwaway staging dir under the system temp; cleaned at the end.
    let staging = std::env::temp_dir().join(format!("duduclaw-expert-{}", uuid::Uuid::new_v4()));
    let cleanup = Cleanup(staging.clone());

    let dir = resolve_source(source, &staging).await?;
    let root = detect::resolve_root(&dir);

    // ── 2. Detect format (fail-closed). ──
    let Some(format) = detect::detect(&root) else {
        let contents = detect::list_contents(&root);
        eprintln!(
            "\n  {} 無法識別的專家包格式（缺 expert.toml / .claude-plugin/plugin.json / SKILL.md）\n",
            style("✗").red().bold()
        );
        eprintln!("  目錄內容:");
        for c in &contents {
            eprintln!("    - {c}");
        }
        eprintln!();
        drop(cleanup);
        return Err(cfg_err("未識別格式，拒絕安裝".into()));
    };

    // ── attach-under: validate the target exists BEFORE anything installs
    //    (fail-closed — a typo'd supervisor must not half-install a pack). ──
    let attach_under = match attach_under.as_deref().map(str::trim) {
        Some("") | None => None,
        Some(id) => {
            if !crate::is_valid_agent_id(id) {
                drop(cleanup);
                return Err(cfg_err(format!("--attach-under '{id}' 非合法 agent id")));
            }
            if !home.join("agents").join(id).join("agent.toml").is_file() {
                drop(cleanup);
                return Err(cfg_err(format!(
                    "--attach-under 目標 '{id}' 不存在（先建立主管或改用既有 agent）"
                )));
            }
            Some(id.to_string())
        }
    };

    let ctx = InstallCtx {
        home: home.to_path_buf(),
        dry_run,
        rename,
        trust_hooks,
        attach_under,
    };
    let mut report = Report::default();

    // ── 3. Dispatch. ──
    let record = match format {
        Format::Native => install_native(&ctx, &root, &mut report).await?,
        Format::ClaudePlugin => super::plugin::install(&ctx, &root, &mut report).await?,
        Format::Skill => super::skill_import::install(&ctx, &root, &mut report).await?,
    };

    report.render_console(dry_run);

    // ── 4. Persist the install record (real runs only). ──
    if !dry_run {
        super::write_record(home, &record)?;
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
    }
}

/// Resolve `source` to a directory containing the pack. Handles a plain
/// directory, a local `.zip`, and an `http(s)` URL to a `.zip`.
async fn resolve_source(source: &str, staging: &Path) -> Result<PathBuf> {
    // WP2.5: BRAT-style side door — `github:user/repo[@branch]` installs the
    // repo's default-branch archive directly. Unregistered and unreviewed;
    // say so honestly, then let the normal fences (zip fence, content scan,
    // hook quarantine) do their job.
    if let Some(spec) = source.strip_prefix("github:") {
        let (url, label) = super::registry::github_archive_url(spec.trim())?;
        println!("  ⚠ 側門安裝 {label}：未經 registry 驗證，來源風險自負（掃描與 hooks 隔離照常生效）");
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        download_zip(&url, &zip_path).await?;
        let unpack = staging.join("unpacked");
        safe_zip::extract_to(&zip_path, &unpack)?;
        return Ok(descend_single_dir(unpack));
    }
    // WP2.2 R2/R3: `registry:<slug>` — resolve via the pack registry with
    // client-side sha256 (always) + minisign (code lane) verification.
    if let Some(slug) = source.strip_prefix("registry:") {
        let (_entry, archive) = super::registry::fetch_verified_archive(slug.trim()).await?;
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        std::fs::write(&zip_path, &archive).map_err(|e| io_err(format!("寫入下載檔失敗: {e}")))?;
        let unpack = staging.join("unpacked");
        safe_zip::extract_to(&zip_path, &unpack)?;
        return Ok(unpack);
    }
    if source.starts_with("http://") || source.starts_with("https://") {
        std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
        let zip_path = staging.join("download.zip");
        download_zip(source, &zip_path).await?;
        let unpack = staging.join("unpacked");
        safe_zip::extract_to(&zip_path, &unpack)?;
        return Ok(unpack);
    }

    let path = PathBuf::from(source);
    if !path.exists() {
        return Err(cfg_err(format!("來源不存在: {source}")));
    }
    if path.is_dir() {
        return Ok(path);
    }
    // A file → must be a zip.
    let is_zip = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("zip"))
        .unwrap_or(false);
    if !is_zip {
        return Err(cfg_err(format!("來源檔不是目錄也不是 .zip: {source}")));
    }
    std::fs::create_dir_all(staging).map_err(|e| io_err(format!("建立暫存目錄失敗: {e}")))?;
    let unpack = staging.join("unpacked");
    safe_zip::extract_to(&path, &unpack)?;
    Ok(unpack)
}


/// GitHub source archives wrap everything in a `<repo>-<branch>/` top dir —
/// when the extraction root has exactly one directory and no manifest of its
/// own, descend into it so format detection sees the real pack root.
fn descend_single_dir(root: PathBuf) -> PathBuf {
    if root.join("expert.toml").is_file() || root.join(".claude-plugin").is_dir() {
        return root;
    }
    let entries: Vec<_> = std::fs::read_dir(&root)
        .map(|it| it.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    let dirs: Vec<_> = entries.iter().filter(|p| p.is_dir()).collect();
    if dirs.len() == 1 && entries.len() == 1 {
        return dirs[0].clone();
    }
    root
}

/// Download a URL to `dest`, capped at [`safe_zip::MAX_UNPACK_BYTES`].
async fn download_zip(url: &str, dest: &Path) -> Result<()> {
    let resp = reqwest::get(url)
        .await
        .map_err(|e| io_err(format!("下載失敗: {e}")))?;
    if !resp.status().is_success() {
        return Err(io_err(format!("下載失敗 HTTP {}", resp.status())));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| io_err(format!("讀取回應失敗: {e}")))?;
    if bytes.len() as u64 > safe_zip::MAX_UNPACK_BYTES {
        return Err(cfg_err(format!(
            "下載內容過大（{} bytes > 上限 {}）",
            bytes.len(),
            safe_zip::MAX_UNPACK_BYTES
        )));
    }
    std::fs::write(dest, &bytes).map_err(|e| io_err(format!("寫入下載檔失敗: {e}")))?;
    Ok(())
}

// ─────────────────────────── Native importer ───────────────────────────

async fn install_native(
    ctx: &InstallCtx,
    dir: &Path,
    report: &mut Report,
) -> Result<InstallRecord> {
    let m = manifest::read(dir).map_err(cfg_err)?;
    let e = &m.expert;

    if e.name.trim().is_empty() || !crate::is_valid_agent_id(&e.name) {
        return Err(cfg_err(format!(
            "expert.name '{}' 非合法 slug",
            e.name.escape_debug()
        )));
    }
    if super::read_record(&ctx.home, &e.name).is_some() {
        return Err(cfg_err(format!(
            "專家包 '{}' 已安裝（先 `expert remove {}` 再重裝）",
            e.name, e.name
        )));
    }

    let mut record = InstallRecord {
        slug: e.name.clone(),
        kind: PackKind::Native,
        display_name: e.display(super::UI_LOCALE).to_string(),
        version: e.version.clone(),
        description: e.description.clone(),
        agents: Vec::new(),
        global_skills: Vec::new(),
        wiki_files: Vec::new(),
        installed_at: now_iso(),
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

    // in-pack name → final (possibly renamed) agent id.
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let by_name: BTreeMap<&str, &manifest::ExpertAgent> =
        e.agents.iter().map(|a| (a.name.as_str(), a)).collect();

    for name in &order {
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
        if let Some(final_id) =
            install_one_agent(ctx, dir, agent, &parent_final, report, &mut record).await?
        {
            id_map.insert(name.clone(), final_id);
        }
    }

    // ── wiki ──
    let wiki_dir = dir.join("wiki");
    if wiki_dir.is_dir() {
        install_wiki_tree(ctx, &wiki_dir, report, &mut record);
    }

    // ── hooks/ → quarantined; enablement is ApprovalBroker-gated ──
    super::hooks::import_hooks(ctx, dir, &e.name, report).await;

    Ok(record)
}

/// Scaffold one native pack agent (soul + partial merge + skills).
async fn install_one_agent(
    ctx: &InstallCtx,
    pack_dir: &Path,
    agent: &manifest::ExpertAgent,
    reports_to_final: &str,
    report: &mut Report,
    record: &mut InstallRecord,
) -> Result<Option<String>> {
    let base = crate::sanitize_agent_id(&agent.name);
    if !crate::is_valid_agent_id(&base) {
        report.skipped("agent", &agent.name, "無法轉為合法 agent id");
        return Ok(None);
    }

    // Conflict / rename resolution.
    let mut final_id = base.clone();
    if ctx.home.join("agents").join(&final_id).exists() {
        if ctx.rename {
            final_id = crate::sanitize_agent_id(&format!("{base}-imported"));
            if !crate::is_valid_agent_id(&final_id)
                || ctx.home.join("agents").join(&final_id).exists()
            {
                report.conflict("agent", &base, "重新命名後仍衝突");
                return Ok(None);
            }
        } else {
            report.conflict(
                "agent",
                &base,
                "同名 agent 已存在（--rename 以 -imported 匯入）",
            );
            return Ok(None);
        }
    }

    // Persona (DATA) → scan.
    let soul_path = pack_dir.join("agents").join(&agent.name).join("soul.md");
    let soul_body = match std::fs::read_to_string(&soul_path) {
        Ok(s) => {
            if let ScanVerdict::Blocked(reason) = scan_external(&s) {
                report.skipped("agent", &base, format!("security: {reason}"));
                return Ok(None);
            }
            Some(s)
        }
        Err(_) => None, // no soul → scaffold default persona
    };

    let display = if agent.display_name.trim().is_empty() {
        agent.name.clone()
    } else {
        agent.display_name.clone()
    };
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
    let trigger = if agent.trigger.trim().is_empty() {
        format!("@{display}")
    } else {
        agent.trigger.clone()
    };

    if ctx.dry_run {
        report.imported("agent", &final_id);
        if !agent.department.trim().is_empty() {
            report.imported("agent-department", &format!("{final_id} → {}", agent.department.trim()));
        }
        // Report referenced skills in plan mode too.
        plan_agent_skills(pack_dir, agent, report);
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
                report.warning("agent-department", &final_id, format!("部門 wiki 目錄建立失敗: {e}"));
            }
            report.imported("agent-department", &format!("{final_id} → {department}"));
        } else {
            report.warning(
                "agent-department",
                &final_id,
                format!("department '{}' 非合法部門名，略過", department.escape_debug()),
            );
        }
    }

    // Merge agent.partial.toml onto the scaffolded agent.toml.
    let partial_path = pack_dir
        .join("agents")
        .join(&agent.name)
        .join("agent.partial.toml");
    if let Ok(partial_src) = std::fs::read_to_string(&partial_path) {
        match partial_src.parse::<toml::Table>() {
            Ok(overlay) => {
                super::patch_agent_toml(&ctx.home, &final_id, |t| merge_toml(t, &overlay))?;
                report.imported("agent-settings", &final_id);
            }
            Err(err) => {
                report.warning(
                    "agent-settings",
                    &final_id,
                    format!("partial 解析失敗: {err}"),
                );
            }
        }
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
            report.warning("agent-department", &final_id, format!("組織資料寫入失敗: {e}"));
        }
    }

    // Merge the pack's per-agent .mcp.json servers into the scaffolded file.
    // The scaffold already wrote `.mcp.json` with the wired `duduclaw` server;
    // pack-declared servers (e.g. a Photoshop MCP) are folded in alongside it
    // (duduclaw entry is never overwritten). Without this the pack's servers
    // would be silently dropped.
    let pack_mcp = pack_dir.join("agents").join(&agent.name).join(".mcp.json");
    if let Some(servers) = super::read_mcp_servers(&pack_mcp)
        && !servers.is_empty()
    {
        match super::merge_agent_mcp(&ctx.home, &final_id, &servers) {
            Ok(written) if written > 0 => {
                report.imported("mcp-servers", &format!("{written} → {final_id}"));
            }
            Ok(_) => {}
            Err(err) => report.warning("mcp-servers", &final_id, format!("併入失敗: {err}")),
        }
    }

    // Install referenced skills into this agent's SKILLS/.
    install_agent_skills(ctx, pack_dir, agent, &final_id, report).await;

    Ok(Some(final_id))
}

/// Copy each referenced pack skill into `<home>/agents/<id>/SKILLS/<name>`
/// after scanning it. External (non-pack) references are noted, not fetched.
async fn install_agent_skills(
    ctx: &InstallCtx,
    pack_dir: &Path,
    agent: &manifest::ExpertAgent,
    final_id: &str,
    report: &mut Report,
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
            Ok(()) => report.imported("skill", &format!("{skill} → {final_id}")),
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
            "",                                       // empty file
            "[agent]\nname = \"a\"\n",                // unrelated section only
            "[model]\n",                              // section, no key
            "[model]\npreferred = 42\n",              // wrong type
            "model = \"scalar\"\n",                   // wrong-typed section
            "[capabilities]\nallowed_tools = \"Bash\"\n", // non-array
            "not toml [[[",                           // malformed file
        ] {
            let (model, tools, disallowed) = fm(body);
            if body.contains("preferred = 42") || !body.contains("preferred") {
                assert!(model.is_none(), "model omitted for {body:?}");
            }
            assert!(tools.is_empty(), "tools omitted for {body:?}");
            assert!(disallowed.is_empty(), "disallowedTools omitted for {body:?}");
        }
    }

    #[test]
    fn default_direction_frontmatter_filters_mixed_arrays_instead_of_dropping_them() {
        let (model, tools, disallowed) = fm(
            "[model]\npreferred = \"claude-sonnet-4-6\"\n\
             [capabilities]\n\
             allowed_tools = [\"Bash\", 7, \"Read\"]\n\
             denied_tools = [\"WebFetch\"]\n",
        );
        assert_eq!(model.as_deref(), Some("claude-sonnet-4-6"));
        assert_eq!(tools, vec!["Bash".to_string(), "Read".to_string()]);
        assert_eq!(disallowed, vec!["WebFetch".to_string()]);
    }
}
