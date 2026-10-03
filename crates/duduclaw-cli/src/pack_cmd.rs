//! `duduclaw pack` — the single front door for every pre-configured bundle of
//! AI employees (T5 / O2, design
//! `commercial/docs/DESIGN-pack-format-unification-2026-09.md`).
//!
//! Before this, "install a ready-made set of agents" had three verbs
//! (`expert install`, `preset install-builtin`, dashboard one-click), three
//! manifest dialects (`expert.toml`, `preset.toml`, `team.toml`) and four
//! places that each re-derived "is this paid content". This module converges
//! the **front door and the data structure**; the installers themselves are
//! untouched, because "the installed result is byte-identical to the old
//! path" is the acceptance condition and moving the internals would turn it
//! into something that has to be re-proved.
//!
//! ```text
//! duduclaw pack install <src>   ┐
//! experts.install RPC           ├─→ install_pack()  ─┬─ Preset   → <home>/presets/<id>/preset.toml
//! experts.install_builtin RPC   │                     └─ Team/Tpl → expert::install::cmd_install
//! experts.install_draft RPC     ┘                                    (safe_zip, DATA scan, hooks
//!                                                                     quarantine, org_store, …)
//! ```
//!
//! The three RPCs reach the CLI by spawning `duduclaw pack install` (see
//! `duduclaw_gateway::handlers::spawn_pack_cli`). `duduclaw expert install`
//! was removed in v1.69.0 and only prints that; the legacy manifest dialects
//! (`expert.toml`, `team.toml`, `preset.toml`, industry directories) are all
//! still read.

use std::path::{Path, PathBuf};

use clap::Subcommand;
use console::style;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_core::pack::{Pack, PackError, PackKind, PackTier};

const UI_LOCALE: &str = "zh-TW";

#[derive(Subcommand)]
pub enum PackCommands {
    /// List packs — installed ones first, then what is available to install
    /// from the built-in catalog and the local preset store.
    List {
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
    },

    /// Read any pack (canonical `pack.toml` or a legacy `expert.toml` /
    /// `team.toml` / `preset.toml` / industry directory) and print the
    /// normalized form, including which dialect it came from.
    Inspect {
        /// Pack directory, `.zip`, `http(s)://…zip`, `registry:<slug>` or
        /// `github:<user>/<repo>`.
        source: String,
        /// Emit machine-readable JSON.
        #[arg(long)]
        json: bool,
        /// Print the canonical `pack.toml` this legacy pack corresponds to
        /// (nothing is written — review it, then save it yourself).
        #[arg(long)]
        emit_canonical: bool,
    },

    /// Install a pack. Routes by declared kind: a job preset lands in the
    /// preset store, a team/industry pack goes through the full expert-pack
    /// security pipeline.
    Install {
        /// Pack directory, `.zip`, `http(s)://…zip`, `registry:<slug>` or
        /// `github:<user>/<repo>`.
        source: String,
        /// Preview the plan without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Team packs: import under a `-imported` suffix on agent-id clashes.
        #[arg(long)]
        rename: bool,
        /// Team packs: explicitly trust and enable the pack's hooks. Without
        /// it hooks land disabled behind an approval request (fail-closed).
        #[arg(long)]
        trust_hooks: bool,
        /// Team packs: attach the pack's root agents under an existing agent.
        #[arg(long)]
        attach_under: Option<String>,
        /// Preset packs: overwrite an existing preset of the same id.
        #[arg(long)]
        force: bool,
    },
}

/// CLI entry point.
pub async fn run(cmd: PackCommands) -> Result<()> {
    let home = crate::duduclaw_home();
    match cmd {
        PackCommands::List { json } => cmd_list(&home, json),
        PackCommands::Inspect {
            source,
            json,
            emit_canonical,
        } => cmd_inspect(&source, json, emit_canonical).await,
        PackCommands::Install {
            source,
            dry_run,
            rename,
            trust_hooks,
            attach_under,
            force,
        } => {
            install_pack(
                &home,
                &source,
                InstallOptions {
                    dry_run,
                    rename,
                    trust_hooks,
                    attach_under,
                    force,
                },
            )
            .await
        }
    }
}

// ───────────────────────────── install ─────────────────────────────

/// Flags that reach one or the other install route. Team-pack flags are
/// ignored by the preset route and vice versa — documented per flag above
/// rather than split into two structs, so one CLI argument set serves both.
#[derive(Debug, Clone, Default)]
pub(crate) struct InstallOptions {
    pub dry_run: bool,
    pub rename: bool,
    pub trust_hooks: bool,
    pub attach_under: Option<String>,
    pub force: bool,
}

/// **The** pack install front door.
///
/// Resolves `source` once (directory / zip / URL / `registry:` / `github:`
/// all go through the existing, fenced resolver), reads it into a [`Pack`]
/// when it speaks a pack dialect, applies the single tier gate, then routes.
/// A source that is *not* a pack (a Claude Code plugin, a lone Agent Skill)
/// falls through to the expert pipeline unchanged — those importers are the
/// reason the detection there stays fail-closed and is not duplicated here.
pub(crate) async fn install_pack(home: &Path, source: &str, opts: InstallOptions) -> Result<()> {
    let staging = std::env::temp_dir().join(format!("duduclaw-pack-{}", uuid::Uuid::new_v4()));
    let _cleanup = StagingCleanup(staging.clone());

    let dir = crate::expert::resolve_pack_source(source, &staging).await?;
    let root = crate::expert::resolve_pack_root(&dir);

    match load_pack_dir(&root) {
        Ok(pack) => {
            tier_gate(&pack, premium_unlocked())?;
            for problem in pack.lint() {
                eprintln!("  {} {problem}", style("⚠").yellow());
            }
            match pack.kind {
                PackKind::Preset => {
                    install_preset_pack(home, &pack, Some(&root), opts.force, opts.dry_run)
                }
                PackKind::Team | PackKind::Template => {
                    crate::expert::install_via_expert_pipeline(
                        home,
                        &root.to_string_lossy(),
                        opts.dry_run,
                        opts.rename,
                        opts.trust_hooks,
                        opts.attach_under,
                    )
                    .await
                }
            }
        }
        // Not a pack dialect (plugin / single skill / unrecognised): the
        // expert pipeline owns those, including the fail-closed rejection
        // message for an unrecognised layout.
        Err(PackError::NotFound(_)) => {
            crate::expert::install_via_expert_pipeline(
                home,
                &root.to_string_lossy(),
                opts.dry_run,
                opts.rename,
                opts.trust_hooks,
                opts.attach_under,
            )
            .await
        }
        Err(e) => Err(cfg_err(e.to_string())),
    }
}

/// Install a job preset into `<home>/presets/<id>/preset.toml`.
///
/// A legacy `preset.toml` source is copied **verbatim** when `src_dir` is
/// given: the premium department kits carry a regeneration header and per-key
/// comments, and re-serializing through `toml` would silently drop them while
/// the resolved values stayed identical. Everything else (a canonical
/// `pack.toml` preset, or a `Pack` with no source directory) is rendered
/// through the one emitter in `duduclaw_core::pack`.
///
/// Either way the bytes that land are validated first — `load_pack_dir`
/// already ran the preset sanitizer, so an org-authority field (`agent.name`
/// / `reports_to` / `department`) has refused the whole pack before reaching
/// here.
fn install_preset_pack(
    home: &Path,
    pack: &Pack,
    src_dir: Option<&Path>,
    force: bool,
    dry_run: bool,
) -> Result<()> {
    let id = pack.id.trim();
    if !duduclaw_core::is_valid_agent_id(id) {
        return Err(cfg_err(format!(
            "職務組合 id '{}' 非合法 slug",
            id.escape_debug()
        )));
    }
    let verbatim = src_dir
        .map(|d| d.join(duduclaw_core::pack::LEGACY_PRESET_FILE))
        .filter(|p| p.is_file())
        .and_then(|p| std::fs::read_to_string(&p).ok());
    let body = match verbatim {
        Some(text) => text,
        None => match pack.to_legacy_preset_toml() {
            Some(b) => b,
            None => return Err(cfg_err("無法輸出 preset.toml 內容".into())),
        },
    };
    let dest = duduclaw_core::preset::preset_file_path(home, id);
    if dest.is_file() && !force {
        println!(
            "\n  {} 職務組合 {} 已存在，未覆寫（加 --force 覆寫）\n",
            style("–").yellow(),
            style(id).bold()
        );
        return Ok(());
    }
    if dry_run {
        println!(
            "\n  {} 將寫入 {}（--dry-run，未寫入）\n",
            style("ℹ").cyan(),
            dest.display()
        );
        return Ok(());
    }
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| io_err(format!("建立 {} 失敗: {e}", parent.display())))?;
    }
    std::fs::write(&dest, body)
        .map_err(|e| io_err(format!("寫入 {} 失敗: {e}", dest.display())))?;
    println!(
        "\n  {} 已安裝職務組合 {} v{} → {}\n",
        style("✓").green(),
        style(pack.display(UI_LOCALE)).bold(),
        pack.version_or_zero(),
        dest.display()
    );
    println!("  綁定到某位 AI 員工：duduclaw preset bind --agent <id> --preset {id}\n");
    Ok(())
}

// ───────────────────────────── tier gate ─────────────────────────────

/// **The** premium decision. Previously each of `premium_unlocked()` (CLI
/// wizard), `premium_templates_unlocked()` (dashboard), `cmd_preset_
/// install_builtin` and `handle_experts_install_builtin` re-derived "is this
/// paid content" from a directory path; a pack now carries its own tier and
/// exactly one predicate reads it.
///
/// Pure (`unlocked` injected) so the decision is unit-testable without a
/// license file.
pub(crate) fn tier_gate(pack: &Pack, unlocked: bool) -> Result<()> {
    if pack.tier == PackTier::Premium && !unlocked {
        return Err(cfg_err(format!(
            "「{}」屬於付費方案的內建內容，目前的授權未開放（premium_templates）。\
             升級方案後再安裝，或改用免費的包。",
            pack.display(UI_LOCALE)
        )));
    }
    Ok(())
}

fn premium_unlocked() -> bool {
    crate::premium_templates::premium_unlocked()
}

/// Read a pack directory and stamp its tier from provenance when the manifest
/// does not state one: anything resolved inside the licensed
/// `templates-premium/` tree is premium content regardless of which dialect
/// it is written in.
pub(crate) fn load_pack_dir(dir: &Path) -> std::result::Result<Pack, PackError> {
    let fallback = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(duduclaw_gateway::premium_templates::label_for_slug)
        .unwrap_or_default();
    let mut pack = duduclaw_core::pack::load_dir(dir, &fallback)?;
    if pack.tier == PackTier::Free && path_is_under_premium_tree(dir) {
        pack.tier = PackTier::Premium;
    }
    Ok(pack)
}

/// Is `dir` inside the resolved premium templates tree? Canonicalized on both
/// sides — an unanchored string `contains` would let `…/templates-premium-evil/`
/// pass (project convention 2).
fn path_is_under_premium_tree(dir: &Path) -> bool {
    let Some(premium) = crate::premium_templates::find_premium_templates_dir() else {
        return false;
    };
    let (Ok(a), Ok(b)) = (std::fs::canonicalize(dir), std::fs::canonicalize(&premium)) else {
        return false;
    };
    a.starts_with(&b)
}

// ───────────────────────────── inspect ─────────────────────────────

async fn cmd_inspect(source: &str, json: bool, emit_canonical: bool) -> Result<()> {
    let staging = std::env::temp_dir().join(format!("duduclaw-pack-{}", uuid::Uuid::new_v4()));
    let _cleanup = StagingCleanup(staging.clone());

    let dir = crate::expert::resolve_pack_source(source, &staging).await?;
    let root = crate::expert::resolve_pack_root(&dir);
    let pack = load_pack_dir(&root).map_err(|e| cfg_err(e.to_string()))?;

    if emit_canonical {
        println!("{}", canonical_toml(&pack));
        return Ok(());
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&pack_json(&pack, false))
                .map_err(|e| cfg_err(format!("序列化失敗: {e}")))?
        );
        return Ok(());
    }

    println!("\n  {}\n", style(pack.display(UI_LOCALE)).bold());
    println!("  id        {}", pack.id);
    println!("  類型      {} ({})", pack.kind.label(), pack.kind.as_str());
    println!("  方案      {}", pack.tier.label());
    println!("  版本      {}", pack.version_or_zero());
    println!(
        "  讀自      {}{}",
        pack.source.as_str(),
        if pack.source.is_legacy() {
            "（舊格式）"
        } else {
            ""
        }
    );
    if !pack.description.trim().is_empty() {
        println!("  說明      {}", pack.description);
    }
    if !pack.category.is_empty() {
        println!("  分類      {}", pack.category);
    }
    if !pack.tags.is_empty() {
        println!("  標籤      {}", pack.tags.join(", "));
    }
    if !pack.channels.is_empty() {
        println!("  建議通道  {}", pack.channels.join(", "));
    }
    if !pack.requires.is_empty() {
        println!(
            "  前置需求  env: {} / bins: {}",
            if pack.requires.env.is_empty() {
                "—".into()
            } else {
                pack.requires.env.join(", ")
            },
            if pack.requires.bins.is_empty() {
                "—".into()
            } else {
                pack.requires.bins.join(", ")
            }
        );
    }

    match pack.kind {
        PackKind::Preset => {
            println!("\n  設定區段（{}）", pack.config.len());
            for key in pack.config.keys() {
                println!("    · [{key}]");
            }
        }
        PackKind::Team => {
            println!("\n  團隊成員（{}）", pack.agents.len());
            for a in &pack.agents {
                let under = if a.reports_to.trim().is_empty() {
                    "（根）".to_string()
                } else {
                    format!("↳ {}", a.reports_to)
                };
                let dept = if a.department.trim().is_empty() {
                    String::new()
                } else {
                    format!(" · 部門 {}", a.department)
                };
                println!(
                    "    · {} [{}/{}]{dept} {under}",
                    a.display_name.as_str(),
                    a.role,
                    a.effective_rank(),
                );
                if !a.overlay.is_empty() {
                    println!("        合規 overlay {} 條", a.overlay.len());
                }
            }
            for h in &pack.humans {
                println!("    · {}（真人保留）", h.title);
            }
        }
        PackKind::Template => {
            println!("\n  單人產業板模（persona 在 SOUL.md，無 roster）");
        }
    }
    if !pack.examples.is_empty() {
        println!("\n  任務示例");
        for e in &pack.examples {
            println!("    · {}", duduclaw_core::truncate_chars(e, 60));
        }
    }
    let lint = pack.lint();
    if !lint.is_empty() {
        println!();
        for l in &lint {
            println!("  {} {l}", style("⚠").yellow());
        }
    }
    println!();
    Ok(())
}

/// Render a [`Pack`] as the canonical `pack.toml` it corresponds to. Used by
/// `--emit-canonical`: a migration aid, printed for human review, never
/// written to disk by this command (the premium content tree's compliance
/// overlays are human-reviewed and no machine rewrites them — design §1).
fn canonical_toml(pack: &Pack) -> String {
    let mut p = toml::value::Table::new();
    let s = |v: &str| toml::Value::String(v.to_string());
    let arr =
        |v: &[String]| toml::Value::Array(v.iter().cloned().map(toml::Value::String).collect());

    p.insert(
        "schema".into(),
        toml::Value::Integer(duduclaw_core::pack::PACK_SCHEMA),
    );
    p.insert("id".into(), s(&pack.id));
    p.insert("kind".into(), s(pack.kind.as_str()));
    p.insert("tier".into(), s(pack.tier.as_str()));
    p.insert("version".into(), s(pack.version_or_zero()));
    p.insert("description".into(), s(&pack.description));
    if !pack.author.is_empty() {
        p.insert("author".into(), s(&pack.author));
    }
    if !pack.license.is_empty() {
        p.insert("license".into(), s(&pack.license));
    }
    if !pack.category.is_empty() {
        p.insert("category".into(), s(&pack.category));
    }
    if !pack.tags.is_empty() {
        p.insert("tags".into(), arr(&pack.tags));
    }
    if !pack.examples.is_empty() {
        p.insert("examples".into(), arr(&pack.examples));
    }
    if !pack.display_name.is_empty() {
        let mut dn = toml::value::Table::new();
        for (k, v) in &pack.display_name {
            dn.insert(k.clone(), s(v));
        }
        p.insert("display_name".into(), toml::Value::Table(dn));
    }
    if !pack.requires.is_empty() {
        let mut r = toml::value::Table::new();
        r.insert("env".into(), arr(&pack.requires.env));
        r.insert("bins".into(), arr(&pack.requires.bins));
        p.insert("requires".into(), toml::Value::Table(r));
    }
    if !pack.prompts.is_empty() {
        let mut t = toml::value::Table::new();
        t.insert("recommended".into(), arr(&pack.prompts));
        p.insert("prompts".into(), toml::Value::Table(t));
    }
    if !pack.channels.is_empty() {
        let mut t = toml::value::Table::new();
        t.insert("suggested".into(), arr(&pack.channels));
        p.insert("channels".into(), toml::Value::Table(t));
    }
    if !pack.config.is_empty() {
        p.insert("config".into(), toml::Value::Table(pack.config.clone()));
    }
    if !pack.agents.is_empty() {
        let agents: Vec<toml::Value> = pack
            .agents
            .iter()
            .map(|a| {
                let mut t = toml::value::Table::new();
                t.insert("name".into(), s(&a.name));
                t.insert("role".into(), s(&a.role));
                t.insert("display_name".into(), s(&a.display_name));
                t.insert("reports_to".into(), s(&a.reports_to));
                if !a.department.is_empty() {
                    t.insert("department".into(), s(&a.department));
                }
                if !a.rank.is_empty() {
                    t.insert("rank".into(), s(&a.rank));
                }
                if !a.trigger.is_empty() {
                    t.insert("trigger".into(), s(&a.trigger));
                }
                if !a.kit.is_empty() {
                    t.insert("kit".into(), s(&a.kit));
                }
                if !a.summary.is_empty() {
                    t.insert("summary".into(), s(&a.summary));
                }
                if !a.skills.is_empty() {
                    t.insert("skills".into(), arr(&a.skills));
                }
                if !a.overlay.is_empty() {
                    t.insert("overlay".into(), arr(&a.overlay));
                }
                if !a.preset.is_empty() {
                    t.insert("preset".into(), s(&a.preset));
                }
                toml::Value::Table(t)
            })
            .collect();
        p.insert("agents".into(), toml::Value::Array(agents));
    }
    if !pack.humans.is_empty() {
        let humans: Vec<toml::Value> = pack
            .humans
            .iter()
            .map(|h| {
                let mut t = toml::value::Table::new();
                t.insert("title".into(), s(&h.title));
                t.insert("summary".into(), s(&h.summary));
                toml::Value::Table(t)
            })
            .collect();
        p.insert("humans".into(), toml::Value::Array(humans));
    }
    if !pack.excluded.is_empty() {
        let ex: Vec<toml::Value> = pack
            .excluded
            .iter()
            .map(|x| {
                let mut t = toml::value::Table::new();
                t.insert("kit".into(), s(&x.kit));
                t.insert("reason".into(), s(&x.reason));
                toml::Value::Table(t)
            })
            .collect();
        p.insert("excluded".into(), toml::Value::Array(ex));
    }
    if !pack.eval_suites.is_empty() {
        let mut t = toml::value::Table::new();
        for (k, v) in &pack.eval_suites {
            t.insert(k.clone(), s(v));
        }
        p.insert("eval_suites".into(), toml::Value::Table(t));
    }
    if !pack.autopilot_rules.is_empty() {
        p.insert(
            "autopilot_rules".into(),
            toml::Value::Array(
                pack.autopilot_rules
                    .iter()
                    .cloned()
                    .map(toml::Value::Table)
                    .collect(),
            ),
        );
    }

    let mut root = toml::value::Table::new();
    root.insert("pack".into(), toml::Value::Table(p));
    toml::to_string_pretty(&toml::Value::Table(root)).unwrap_or_default()
}

fn pack_json(pack: &Pack, installed: bool) -> serde_json::Value {
    serde_json::json!({
        "id": pack.id,
        "kind": pack.kind.as_str(),
        "tier": pack.tier.as_str(),
        "version": pack.version_or_zero(),
        "display_name": pack.display(UI_LOCALE),
        "description": pack.description,
        "category": pack.category,
        "tags": pack.tags,
        "examples": pack.examples,
        "source_format": pack.source.as_str(),
        "legacy_format": pack.source.is_legacy(),
        "installed": installed,
        "agents": pack.agents.iter().map(|a| serde_json::json!({
            "name": a.name,
            "role": a.role,
            "display_name": a.display_name,
            "reports_to": a.reports_to,
            "department": a.department,
            "rank": a.effective_rank(),
            "overlay_rules": a.overlay.len(),
        })).collect::<Vec<_>>(),
        "config_sections": pack.config.keys().cloned().collect::<Vec<_>>(),
        "lint": pack.lint(),
    })
}

// ───────────────────────────── list ─────────────────────────────

fn cmd_list(home: &Path, json: bool) -> Result<()> {
    let installed = crate::expert::list_records(home);
    let unlocked = premium_unlocked();

    // Local preset store.
    let mut local_presets = Vec::new();
    for id in duduclaw_core::preset::list_presets(home) {
        if let Ok(p) = load_pack_dir(&duduclaw_core::preset::preset_dir(home, &id)) {
            local_presets.push(p);
        }
    }

    // Built-in catalog (premium tree): standalone experts + converted teams,
    // the authored team playbooks, department presets, industry packs.
    //
    // `experts/` is walked FIRST and ids are deduped: the same team exists
    // twice on disk — `teams/<i>-team/team.toml` (authored source) and
    // `experts/<i>-team/expert.toml` (the committed `convert-teams` output).
    // They are one pack to a user, and the converted copy is the one that
    // carries a version and matches an install record's slug, so it wins.
    let mut available: Vec<Pack> = Vec::new();
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    if let Some(premium) = crate::premium_templates::find_premium_templates_dir() {
        let mut push = |p: Pack, out: &mut Vec<Pack>| {
            if seen.insert(p.id.clone()) {
                out.push(p);
            }
        };
        for sub in ["experts", "teams", "presets"] {
            for dir in duduclaw_core::pack::list_dir(&premium.join(sub)) {
                if let Ok(p) = load_pack_dir(&dir) {
                    push(p, &mut available);
                }
            }
        }
        for dir in duduclaw_core::pack::list_dir(&premium) {
            // `<industry>-pro/` siblings only — the three catalog subdirs are
            // enumerated above and `_departments` / `_roles` are kits, not packs.
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            if name.ends_with("-pro")
                && let Ok(p) = load_pack_dir(&dir)
            {
                push(p, &mut available);
            }
        }
    }
    available.sort_by(|a, b| (a.kind, a.id.clone()).cmp(&(b.kind, b.id.clone())));

    if json {
        let installed_json: Vec<serde_json::Value> = installed
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.slug,
                    "kind": PackKind::Team.as_str(),
                    "version": r.version,
                    "display_name": if r.display_name.is_empty() { r.slug.clone() } else { r.display_name.clone() },
                    "description": r.description,
                    "installed": true,
                    "agents_installed": r.agents.len(),
                    "source_format": r.kind.label(),
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "premium_unlocked": unlocked,
                "installed": installed_json,
                "presets": local_presets.iter().map(|p| pack_json(p, true)).collect::<Vec<_>>(),
                "available": available.iter().map(|p| pack_json(p, false)).collect::<Vec<_>>(),
            }))
            .map_err(|e| cfg_err(format!("序列化失敗: {e}")))?
        );
        return Ok(());
    }

    println!("\n  {}\n", style("已安裝的團隊包").bold());
    if installed.is_empty() {
        println!("    （無）用 `duduclaw pack install <path|zip|url>` 安裝。");
    }
    for r in &installed {
        let name = if r.display_name.is_empty() {
            r.slug.clone()
        } else {
            format!("{} ({})", r.display_name, r.slug)
        };
        println!(
            "    {} {}  v{}  · 成員 {} · 技能 {} · wiki {}",
            style("●").cyan(),
            style(name).bold(),
            r.version,
            r.agents.len(),
            r.global_skills.len(),
            r.wiki_files.len()
        );
    }

    println!("\n  {}\n", style("本機職務組合（preset）").bold());
    if local_presets.is_empty() {
        println!(
            "    （無）用 `duduclaw preset install-builtin` 或 `duduclaw pack install` 安裝。"
        );
    }
    for p in &local_presets {
        println!(
            "    {} {}  v{}  ({})",
            style("●").magenta(),
            style(p.display(UI_LOCALE)).bold(),
            p.version_or_zero(),
            p.id
        );
    }

    println!("\n  {}\n", style("可安裝的內建包").bold());
    if available.is_empty() {
        println!("    （無）本機沒有內建包目錄。");
    }
    let installed_ids: std::collections::BTreeSet<&str> =
        installed.iter().map(|r| r.slug.as_str()).collect();
    for p in &available {
        let mark = if p.tier == PackTier::Premium && !unlocked {
            style("🔒").dim().to_string()
        } else if installed_ids.contains(p.id.as_str()) {
            style("✓").green().to_string()
        } else {
            style("○").dim().to_string()
        };
        println!(
            "    {mark} {}  [{}·{}]  {}",
            style(p.display(UI_LOCALE)).bold(),
            p.kind.label(),
            p.tier.label(),
            style(&p.id).dim()
        );
    }
    if !unlocked && available.iter().any(|p| p.tier == PackTier::Premium) {
        println!(
            "\n  {} 標示 🔒 的內建包屬於付費方案；升級後即可一鍵安裝。",
            style("ℹ").cyan()
        );
    }
    println!();
    Ok(())
}

// ───────────────────────────── plumbing ─────────────────────────────

struct StagingCleanup(PathBuf);
impl Drop for StagingCleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn io_err(msg: String) -> DuDuClawError {
    DuDuClawError::Io(std::io::Error::other(msg))
}
fn cfg_err(msg: String) -> DuDuClawError {
    DuDuClawError::Config(msg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_core::pack::{PackSource, parse_legacy_expert, parse_legacy_team, parse_pack};

    const TEAM: &str = r#"
industry = "clinic"
pack = "clinic-pro"
label = "醫美／牙醫診所"

[front_desk]
name = "clinic-assistant"
display_name = "診所前台助理"

[[workers]]
kit = "billing-admin"
name = "clinic-billing"
display_name = "帳務請款助理"
overlay = ["不得做任何醫療診斷"]
"#;

    const EXPERT: &str = r#"
[expert]
name = "cad-drafter"
version = "1.0.0"
description = "CAD 製圖"

[[expert.agents]]
name = "cad-drafter"
role = "worker"
"#;

    #[test]
    fn tier_gate_blocks_premium_only_when_locked() {
        let premium = parse_legacy_team("clinic-team", TEAM).unwrap();
        assert_eq!(premium.tier, PackTier::Premium);
        assert!(tier_gate(&premium, true).is_ok(), "unlocked → installs");
        let err = tier_gate(&premium, false).unwrap_err();
        assert!(
            err.to_string().contains("付費方案"),
            "locked premium must be refused: {err}"
        );

        let free = parse_legacy_expert("cad-drafter", EXPERT).unwrap();
        assert_eq!(free.tier, PackTier::Free);
        assert!(
            tier_gate(&free, false).is_ok(),
            "a free pack never consults the license"
        );
    }

    #[test]
    fn tier_gate_refuses_a_pack_whose_tier_cannot_be_read() {
        // `PackTier::parse` fails closed to Premium — an unreadable tier must
        // not accidentally unlock paid content.
        let src = r#"
[pack]
schema = 1
kind = "team"
tier = "gratis"
version = "1.0.0"
label = "x"

[[pack.agents]]
name = "a"
"#;
        let p = parse_pack("x", src).unwrap();
        assert_eq!(p.tier, PackTier::Premium);
        assert!(tier_gate(&p, false).is_err());
    }

    #[test]
    fn canonical_emit_round_trips_through_the_loader() {
        for (id, pack) in [
            (
                "clinic-team",
                parse_legacy_team("clinic-team", TEAM).unwrap(),
            ),
            (
                "cad-drafter",
                parse_legacy_expert("cad-drafter", EXPERT).unwrap(),
            ),
        ] {
            let emitted = canonical_toml(&pack);
            let back = parse_pack(id, &emitted)
                .unwrap_or_else(|e| panic!("{id} canonical emit must re-parse: {e}"));
            assert_eq!(back.kind, pack.kind, "{id}");
            assert_eq!(back.tier, pack.tier, "{id}");
            assert_eq!(back.display(UI_LOCALE), pack.display(UI_LOCALE), "{id}");
            assert_eq!(back.agents.len(), pack.agents.len(), "{id}");
            for (b, p) in back.agents.iter().zip(pack.agents.iter()) {
                assert_eq!(b.name, p.name, "{id}");
                assert_eq!(b.role, p.role, "{id}");
                assert_eq!(b.reports_to, p.reports_to, "{id}");
                assert_eq!(b.department, p.department, "{id}");
                assert_eq!(b.rank, p.rank, "{id}");
                assert_eq!(b.trigger, p.trigger, "{id}");
                assert_eq!(b.overlay, p.overlay, "{id}");
            }
            assert_eq!(back.source, PackSource::Canonical, "{id}");
        }
    }

    #[test]
    fn premium_tree_containment_is_anchored_not_substring() {
        // Without a resolvable premium tree the check is simply false; the
        // point pinned here is that it never reduces to a string `contains`.
        let tmp = tempfile::tempdir().unwrap();
        let evil = tmp.path().join("templates-premium-evil");
        std::fs::create_dir_all(&evil).unwrap();
        assert!(!path_is_under_premium_tree(&evil));
    }

    #[test]
    fn preset_install_writes_the_preset_store_and_honours_force() {
        let home = tempfile::tempdir().unwrap();
        let src = r#"
[pack]
schema = 1
kind = "preset"
tier = "free"
version = "1.0.0"
label = "帳務請款助理"
description = "d"

[pack.config.model]
preferred = "claude-haiku-4-5"
"#;
        let pack = parse_pack("billing-admin", src).unwrap();
        let dest = duduclaw_core::preset::preset_file_path(home.path(), "billing-admin");

        install_preset_pack(home.path(), &pack, None, false, true).unwrap();
        assert!(!dest.exists(), "--dry-run writes nothing");

        install_preset_pack(home.path(), &pack, None, false, false).unwrap();
        assert!(dest.is_file());
        // The live preset reader must accept what we wrote.
        let loaded = duduclaw_core::preset::load_preset(home.path(), "billing-admin").unwrap();
        assert_eq!(loaded.meta.label, "帳務請款助理");

        std::fs::write(&dest, "# hand edited\n[preset]\nversion=\"9\"\n").unwrap();
        install_preset_pack(home.path(), &pack, None, false, false).unwrap();
        assert!(
            std::fs::read_to_string(&dest)
                .unwrap()
                .contains("hand edited"),
            "an existing preset is never clobbered without --force"
        );
        install_preset_pack(home.path(), &pack, None, true, false).unwrap();
        assert!(
            !std::fs::read_to_string(&dest)
                .unwrap()
                .contains("hand edited")
        );
    }

    // ── Provenance: premium-tree content is premium tier ──
    //
    // The roster-level golden comparison against the committed
    // `convert-teams` output lives with the loaders it exercises
    // (`duduclaw_core::pack::tests::builtin_team_packs_match_convert_teams_output`);
    // what is cli-specific, and pinned here, is the provenance stamp that
    // feeds `tier_gate`: a pack read from inside the licensed tree is premium
    // even when its own dialect declares no tier.

    fn premium_tree() -> Option<PathBuf> {
        crate::premium_templates::find_premium_templates_dir()
    }

    #[test]
    fn premium_tree_content_is_stamped_premium_whatever_dialect_it_uses() {
        let Some(premium) = premium_tree() else {
            eprintln!(
                "SKIP premium_tree_content_is_stamped_premium: templates-premium/ not resolvable \
                 (set DUDUCLAW_PREMIUM_TEMPLATES to run this locally)"
            );
            return;
        };
        let mut checked = 0usize;
        // An `expert.toml` pack declares no tier at all — provenance is the
        // only thing that can classify it, and without this stamp an
        // unlicensed `duduclaw pack install ./templates-premium/experts/<slug>`
        // would install paid content.
        for slug in ["cad-drafter", "marketing-designer", "clinic-team"] {
            let dir = premium.join("experts").join(slug);
            if !dir.join("expert.toml").is_file() {
                eprintln!("SKIP {slug}: not present");
                continue;
            }
            let p = load_pack_dir(&dir).unwrap_or_else(|e| panic!("{slug}: {e}"));
            assert_eq!(p.tier, PackTier::Premium, "{slug} is premium-tree content");
            assert!(tier_gate(&p, false).is_err(), "{slug} refused when locked");
            assert!(tier_gate(&p, true).is_ok(), "{slug} installs when unlocked");
            checked += 1;
        }
        assert!(checked >= 2, "expected at least two premium packs, saw {checked}");
    }

    #[tokio::test]
    async fn install_pack_copies_a_legacy_preset_verbatim() {
        let tmp = tempfile::tempdir().unwrap();
        // The pack id is the DIRECTORY name, so it has to be a valid slug —
        // a bare `tempdir()` (`.tmpXXXXXX`) is deliberately refused.
        let src = tmp.path().join("billing-admin");
        std::fs::create_dir_all(&src).unwrap();
        let body = "# 由部門 kit 機械轉換而來 —— 重新產生：cargo test …\n\n\
                    [preset]\nversion = \"1.0.0\"\nlabel = \"帳務請款助理\"\n\
                    description = \"共用部門職務組合\"\n\n\
                    [model]\n# 這行註解必須活下來\npreferred = \"claude-haiku-4-5\"\n";
        std::fs::write(src.join("preset.toml"), body).unwrap();

        let home = tempfile::tempdir().unwrap();
        install_pack(
            home.path(),
            src.to_str().unwrap(),
            InstallOptions::default(),
        )
        .await
        .expect("legacy preset installs");

        let dest = duduclaw_core::preset::preset_file_path(home.path(), "billing-admin");
        let written = std::fs::read_to_string(&dest).unwrap();
        assert_eq!(
            written, body,
            "a legacy preset.toml is copied byte-for-byte; re-serializing would \
             silently drop the premium kits' regeneration header and per-key comments"
        );
    }

    // ── Byte-identical install parity ──
    //
    // The acceptance condition for this convergence is "the installed result
    // matches the old path exactly". The old path is `expert::install::
    // cmd_install`, which still exists untouched and is still exercised
    // directly by the 15 tests in `expert::tests`. This test installs the same
    // pack both ways into two isolated homes and diffs the produced trees, so
    // "byte-identical" is measured, not asserted in a comment.

    fn write_fixture(dir: &Path) {
        let w = |rel: &str, body: &str| {
            let p = dir.join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "expert.toml",
            r#"
[expert]
name = "parity-demo"
description = "安裝路徑一致性測試包"
version = "1.0.0"

[[expert.agents]]
name = "front"
role = "front_desk"
display_name = "櫃檯"
department = "客服"

[[expert.agents]]
name = "helper"
role = "worker"
reports_to = "front"
"#,
        );
        w("agents/front/soul.md", "# 櫃檯\n\n我負責接待與分派。\n");
        w(
            "agents/front/agent.partial.toml",
            "[model]\npreferred = \"claude-haiku-4-5\"\n",
        );
        w("agents/helper/soul.md", "# 助理\n\n我負責整理資料。\n");
        w("wiki/policies/demo.md", "# 政策\n\n一切照規矩來。\n");
    }

    /// Every regular file under `root`, as (relative path, bytes), sorted.
    /// `experts/` is excluded: its `install.json` records a wall-clock
    /// `installed_at`, the one field that cannot be equal across two runs.
    fn snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
        fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for e in rd.flatten() {
                let p = e.path();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_symlink() {
                    continue;
                }
                if ft.is_dir() {
                    walk(base, &p, out);
                } else if ft.is_file()
                    && let Ok(rel) = p.strip_prefix(base)
                {
                    let rel = rel.to_string_lossy().replace('\\', "/");
                    if rel.starts_with("experts/") {
                        continue;
                    }
                    out.push((rel, std::fs::read(&p).unwrap_or_default()));
                }
            }
        }
        let mut out = Vec::new();
        walk(root, root, &mut out);
        out.sort();
        out
    }

    #[tokio::test]
    async fn install_pack_produces_the_same_tree_as_the_legacy_expert_pipeline() {
        let tmp = tempfile::tempdir().unwrap();
        // Named so the directory slug matches `expert.name` (a mismatch would
        // only warn, but it would be noise unrelated to what this test pins).
        let pack = tmp.path().join("parity-demo");
        std::fs::create_dir_all(&pack).unwrap();
        write_fixture(&pack);
        let src = pack.to_str().unwrap().to_string();

        let old_home = tempfile::tempdir().unwrap();
        crate::expert::install_via_expert_pipeline(
            old_home.path(),
            &src,
            false,
            false,
            false,
            None,
        )
        .await
        .expect("legacy pipeline installs");

        let new_home = tempfile::tempdir().unwrap();
        install_pack(new_home.path(), &src, InstallOptions::default())
            .await
            .expect("unified front door installs");

        let a = snapshot(old_home.path());
        let b = snapshot(new_home.path());

        let names_a: Vec<&str> = a.iter().map(|(p, _)| p.as_str()).collect();
        let names_b: Vec<&str> = b.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names_a, names_b, "the two routes must write the same files");
        assert!(
            names_a.iter().any(|p| p.starts_with("agents/front/")),
            "sanity: the pack actually installed something: {names_a:?}"
        );
        for ((pa, ba), (_, bb)) in a.iter().zip(b.iter()) {
            assert_eq!(
                String::from_utf8_lossy(ba),
                String::from_utf8_lossy(bb),
                "content differs for {pa}"
            );
        }
    }

    // ── CLI surface ──
    //
    // `Cli::try_parse_from` is routed through `run_on_big_stack`: clap's giant
    // command enum blows the 2 MiB default test stack in debug builds.

    #[test]
    fn pack_subcommands_parse_and_expert_install_is_removed() {
        crate::test_support::run_on_big_stack(pack_subcommands_parse_body);
    }

    fn pack_subcommands_parse_body() {
        use clap::Parser;

        let cli = crate::Cli::try_parse_from(["duduclaw", "pack", "list", "--json"])
            .expect("pack list parses");
        assert!(matches!(
            cli.command,
            crate::Commands::Maintenance(crate::MaintenanceCommands::Pack {
                command: PackCommands::List { json: true }
            })
        ));

        let cli = crate::Cli::try_parse_from(["duduclaw", "pack", "inspect", "./x", "--json"])
            .expect("pack inspect parses");
        match cli.command {
            crate::Commands::Maintenance(crate::MaintenanceCommands::Pack {
                command:
                    PackCommands::Inspect {
                        source,
                        json,
                        emit_canonical,
                    },
            }) => {
                assert_eq!(source, "./x");
                assert!(json);
                assert!(!emit_canonical);
            }
            _ => panic!("pack inspect parsed into the wrong command"),
        }

        let cli = crate::Cli::try_parse_from([
            "duduclaw",
            "pack",
            "install",
            "./x",
            "--dry-run",
            "--force",
        ])
        .expect("pack install parses");
        match cli.command {
            crate::Commands::Maintenance(crate::MaintenanceCommands::Pack {
                command:
                    PackCommands::Install {
                        source,
                        dry_run,
                        force,
                        ..
                    },
            }) => {
                assert_eq!(source, "./x");
                assert!(dry_run);
                assert!(force);
            }
            _ => panic!("pack install parsed into the wrong command"),
        }

        // The old verb parses only to be refused with a message naming
        // `pack install` (v1.69.0); `expert list` is a real verb, not an alias.
        let cli = crate::Cli::try_parse_from(["duduclaw", "expert", "install", "./x", "--dry-run"])
            .expect("expert install still parses so it can explain itself");
        match cli.command {
            crate::Commands::Maintenance(crate::MaintenanceCommands::Expert {
                command: crate::expert::ExpertCommands::Install(_),
            }) => {}
            _ => panic!("expert install must map to the removed-verb stub"),
        }
        let msg = crate::removed_spelling::RemovedSpelling::ExpertInstall.message();
        assert!(msg.contains("v1.69.0") && msg.contains("duduclaw pack install"), "{msg}");
        let cli = crate::Cli::try_parse_from(["duduclaw", "expert", "list", "--json"])
            .expect("expert list stays");
        assert!(matches!(
            cli.command,
            crate::Commands::Maintenance(crate::MaintenanceCommands::Expert {
                command: crate::expert::ExpertCommands::List { json: true }
            })
        ));
    }

    #[test]
    fn preset_install_refuses_an_unsafe_id() {
        let home = tempfile::tempdir().unwrap();
        let mut pack = parse_legacy_expert("cad-drafter", EXPERT).unwrap();
        pack.kind = PackKind::Preset;
        pack.id = "../escape".into();
        let err = install_preset_pack(home.path(), &pack, None, true, false).unwrap_err();
        assert!(err.to_string().contains("非合法 slug"), "{err}");
    }
}
