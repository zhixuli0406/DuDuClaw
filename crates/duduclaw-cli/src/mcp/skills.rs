use super::*;

/// Pull the `description` field out of a skill's YAML frontmatter without
/// touching the filesystem. Used by `handle_skill_list` after the skill
/// content has already been loaded by `AgentRegistry::load_skills`.
///
/// Tolerant of: missing frontmatter, unterminated frontmatter, missing
/// `description` key, quoted vs. unquoted values. Returns an empty
/// string in any failure mode — UI just shows a blank description rather
/// than skipping the skill.
// Retained as a tolerant fallback parser (still unit-tested). The WP8 display
// path now uses `parse_skill_meta_from_content` for localisation.
#[allow(dead_code)]
pub(crate) fn parse_skill_description_from_content(content: &str) -> String {
    let trimmed = content.trim_start();
    let after = match trimmed.strip_prefix("---") {
        Some(rest) => rest.trim_start_matches(['\r', '\n']),
        None => return String::new(),
    };
    let yaml_end = match after.find("\n---") {
        Some(idx) => idx,
        None => return String::new(),
    };
    let yaml_block = &after[..yaml_end];

    // Find the `description:` line. Manual parse instead of yaml-rs to
    // stay zero-cost on the hot path and tolerate slightly malformed
    // frontmatter (which yaml-rs would reject outright).
    for raw_line in yaml_block.lines() {
        let line = raw_line.trim_start();
        if let Some(rest) = line.strip_prefix("description:") {
            let value = rest.trim();
            // Strip surrounding quotes if present.
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .or_else(|| value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
                .unwrap_or(value);
            return value.to_string();
        }
    }
    String::new()
}

/// Search skill hubs (G5). Default aggregates across all configured hubs
/// with the same weighted scoring the GitHub index uses; `hub` restricts to
/// one hub by exact id. Per-hub failures are reported, never swallowed.
/// T5/O13 — the single skill-search entry point; the `source` argument picks
/// hubs, the learned skill bank, or both.
pub(crate) async fn handle_skill_search(params: &Value, home_dir: &Path) -> Value {
    let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
    if query.is_empty() {
        return serde_json::json!({
            "content": [{"type": "text", "text": "Error: query is required"}],
            "isError": true
        });
    }
    let source = match crate::mcp_alias::resolve_skill_source(params) {
        Ok(s) => s,
        Err(e) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!("Error: {e}")}],
                "isError": true
            });
        }
    };

    let registry = duduclaw_agent::skill_hub::HubRegistry::from_home(home_dir);
    let explicit_hub = params.get("hub").and_then(|v| v.as_str()).map(|s| s.trim());
    if explicit_hub.is_some() && !source.queries_hubs() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: hub is not accepted when source='{}' — the skill bank is not a hub",
                source.as_str()
            )}],
            "isError": true
        });
    }
    if let Some(h) = explicit_hub {
        // Exact-id validation up front so a typo'd hub errors instead of
        // silently returning an empty aggregate.
        if registry.get(h).is_none() {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Error: unknown hub '{h}'. Configured hubs: {}",
                    registry.ids().join(", ")
                )}],
                "isError": true
            });
        }
    }
    // `source = "github"` is exactly "the github hub only" — it is the same
    // index the `hub` parameter already addressed, given its own token so a
    // model that knows the skill lives on GitHub does not have to know that
    // `github` happens to be spelled as a hub id.
    let hub_filter = match source {
        crate::mcp_alias::SkillSource::Github => Some("github"),
        _ => explicit_hub,
    };
    if hub_filter == Some("github") && registry.get("github").is_none() {
        return serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "Error: source='github' but no github hub is configured. Configured hubs: {}",
                registry.ids().join(", ")
            )}],
            "isError": true
        });
    }
    let limit = params
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n.clamp(1, 100) as usize);

    let result = if source.queries_hubs() {
        registry
            .search(home_dir, query, limit.unwrap_or(20), hub_filter)
            .await
    } else {
        duduclaw_agent::skill_hub::AggregatedSearch::default()
    };
    // The learned skill bank. Its store is an in-memory stub today (see
    // `skill_bank_hits`), so this contributes zero rows rather than a
    // fabricated result — an empty source must read as empty, never as
    // "nothing here, so here is something else".
    let bank_hits: Vec<(String, String)> = if source.queries_bank() {
        skill_bank_hits(query, limit.unwrap_or(5))
    } else {
        Vec::new()
    };
    // De-duplicate across sources by skill name: a skill that was learned
    // locally AND exists on a hub should be one row, not two.
    let hub_names: std::collections::HashSet<String> = result
        .hits
        .iter()
        .map(|h| h.entry.name.to_lowercase())
        .collect();
    let bank_hits: Vec<(String, String)> = bank_hits
        .into_iter()
        .filter(|(name, _)| !hub_names.contains(&name.to_lowercase()))
        .collect();

    let mut lines: Vec<String> = Vec::new();
    if result.hits.is_empty() && bank_hits.is_empty() {
        lines.push(format!("No skills found for '{query}'."));
    } else {
        lines.push(format!(
            "Found {} skill(s) for '{query}':\n",
            result.hits.len() + bank_hits.len()
        ));
        for h in &result.hits {
            let s = &h.entry;
            let tags = if s.tags.is_empty() {
                String::new()
            } else {
                format!(" [{}]", s.tags.join(", "))
            };
            // WP2.6: surface trust tier, 60-day installs, and any non-clean
            // source verdict so the caller can judge before installing.
            let mut meta = format!("trust={}", s.trust_tier.as_str());
            if s.install_count > 0 {
                meta.push_str(&format!(", installs={}", s.install_count));
            }
            if let Some(v) = &s.source_verdict {
                if v != "clean" {
                    meta.push_str(&format!(", verdict={v}"));
                }
            }
            lines.push(format!(
                "- **{}** ({}, {}): {}{}",
                s.name, h.hub, meta, s.description, tags
            ));
        }
        for (name, description) in &bank_hits {
            lines.push(format!("- **{name}** (skill-bank): {description}"));
        }
    }
    // Honest degradation: name every hub that failed.
    for (hub, err) in &result.errors {
        lines.push(format!("[unreachable: {hub}: {err}]"));
    }
    // Only say something about the skill bank when the caller explicitly
    // asked for it — otherwise an always-empty stub would add a noise line to
    // every default search.
    if source == crate::mcp_alias::SkillSource::Bank && bank_hits.is_empty() {
        lines.push(
            "(the learned skill bank holds no entries yet — populate it via skill_extract + skill_bank_feedback)"
                .to_string(),
        );
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

/// The learned skill bank's search half, as `(name, description)` rows.
///
/// Honest status: the `SkillBank` store is still an in-memory stub, so this
/// returns zero rows for every query. It is a real function rather than an inline `Vec::new()` so
/// that wiring the store later is a one-body change and the `source="bank"` /
/// `source="all"` routing above is already exercised by tests.
pub(crate) fn skill_bank_hits(_query: &str, _limit: usize) -> Vec<(String, String)> {
    Vec::new()
}

/// WP2.6 §4+§5: report an agent's capability gaps (attachment file types it
/// received but has no matching skill for) plus the template's curated
/// recommended-skills shortlist. Read-only discovery aid.
pub(crate) async fn handle_skill_gaps(params: &Value, home_dir: &Path, default_agent: &str) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let agent_name = if agent_id.is_empty() {
        if default_agent.is_empty() {
            resolve_main_agent_name(home_dir).await
        } else {
            default_agent.to_string()
        }
    } else {
        agent_id.to_string()
    };

    // Installed skill names (global + agent-local), lowercased, as the
    // "already covered" term set for gap analysis.
    let mut installed_terms: Vec<String> = Vec::new();
    // W3-3b (a): `agent_name` defaults to the caller, which may be an `eph-*`
    // role member living under `agents/.ephemeral/<id>/`.
    let gaps_agent_dir = agent_dir_for_id(home_dir, &agent_name);
    for dir in [home_dir.join("skills"), gaps_agent_dir.join("SKILLS")] {
        for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&dir).await {
            installed_terms.push(sk.name.to_lowercase());
        }
    }

    // Extension-based gaps recorded at the attachment chokepoint (§5).
    let records = duduclaw_agent::skill_ext_gap::read_all(home_dir);
    let gaps = duduclaw_agent::skill_ext_gap::aggregate_gaps_for_agent(
        &records,
        &agent_name,
        &installed_terms,
    );

    // Curated per-template recommendations from the agent's own agent.toml (§4).
    let recommended = duduclaw_agent::skill_recommend::read_recommended(&gaps_agent_dir);

    let mut lines: Vec<String> = Vec::new();
    lines.push(format!("Skill recommendations for agent '{agent_name}':\n"));

    lines.push("**Curated (from template)**:".to_string());
    if recommended.is_empty() {
        lines.push("- (none configured — add [skills] recommended to agent.toml)".to_string());
    } else {
        for r in &recommended {
            lines.push(format!("- {}", r.as_ref_str()));
        }
    }

    lines.push("\n**Inferred from received file types**:".to_string());
    if gaps.is_empty() {
        lines.push("- (no capability gaps recorded)".to_string());
    } else {
        for g in &gaps {
            lines.push(format!(
                "- **{}** — seen {}× as [{}] (e.g. {}); try `skill_search {}`",
                g.capability,
                g.count,
                g.exts.join(", "),
                g.sample_filename,
                g.capability
            ));
        }
    }

    serde_json::json!({
        "content": [{"type": "text", "text": lines.join("\n")}]
    })
}

// ── Install-class approval gate (WP5, stdio-path parity) ────────────────────
//
// The WS/PolicyKernel path (`mcp_dispatch.rs`) already blocks high-risk tool
// calls on the `ApprovalBroker`. The stdio direct path bypassed it, so an
// agent could self-install a hub skill (scan-gated only) with no human in the
// loop — the "agent 自己接工具誰授權?→ 這沒做" gap. These helpers give the
// stdio path the same admin-approval gate for install/attach-class tools.
//
// Fail-closed throughout: broker unavailable, request failure, denial, or TTL
// expiry all resolve to a DENY — never a silent install.

/// Install a skill from a hub — always through the fail-closed scan gate, and
/// (for non-admin callers) through the admin-approval gate.
pub(crate) async fn handle_skill_hub_install(
    params: &Value,
    home_dir: &Path,
    default_agent: &str,
    caller_is_admin: bool,
) -> Value {
    let hub = params.get("hub").and_then(|v| v.as_str()).unwrap_or("");
    let skill_name = params
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let owner = params
        .get("owner")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());
    let scope = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global");

    if hub.is_empty() || skill_name.is_empty() {
        return mcp_error("hub and skill_name are required");
    }
    // Path-safety first: slug/owner land in URL paths and file names.
    if !is_safe_path_component(skill_name) {
        return mcp_error("invalid skill_name (alphanumeric, hyphens, underscores only)");
    }
    if let Some(o) = owner {
        if !is_safe_path_component(o) {
            return mcp_error("invalid owner (alphanumeric, hyphens, underscores only)");
        }
    }
    // WP2.3: `department:<dept>` installs the skill into the shared department
    // layer (`~/.duduclaw/shared/skills/departments/<dept>/`) so only agents in
    // that department load it. The department name is DATA — validated against
    // the `is_valid_department` allowlist here and again at the loader sink.
    if scope != "global" {
        if let Some(dept) = scope.strip_prefix("department:") {
            if !duduclaw_core::is_valid_department(dept) {
                return mcp_error(
                    "invalid department scope (1..=64 bytes, no path separators / whitespace / control chars)",
                );
            }
        } else if !is_safe_path_component(scope) {
            return mcp_error(
                "invalid scope (use 'global', 'department:<name>', or a valid agent id)",
            );
        }
    }

    use duduclaw_gateway::skill_lifecycle::hub_install;

    // ── Phase 1: fetch + security scan (fail-closed) ────────────────────────
    // High-risk / unknown hub / absent content DENY here and never reach the
    // approval queue — approval is only for scanned-and-passed skills.
    let gated = match hub_install::fetch_and_gate(home_dir, hub, skill_name, owner).await {
        Ok(g) => g,
        Err(e) => return mcp_error(&e),
    };

    // ── Phase 2: admin approval (non-admin install-class or explicit) ───────
    let summary = format!(
        "安裝技能「{skill_name}」（來源 hub：{hub}，範圍：{scope}，掃描風險：{}，{} 項發現）",
        gated.risk_level, gated.findings
    );
    let payload = serde_json::json!({
        "tool": "skill_hub_install",
        "hub": hub,
        "skill_name": skill_name,
        "owner": owner,
        "scope": scope,
    });
    match gate_install_approval(
        home_dir,
        default_agent,
        "skill_hub_install",
        &summary,
        payload,
        caller_is_admin,
    )
    .await
    {
        InstallApprovalOutcome::Proceed => {}
        InstallApprovalOutcome::Denied(msg) => return mcp_error(&msg),
    }

    // ── Phase 3: write the gated skill into the loader root ─────────────────
    match hub_install::install_gated(home_dir, &gated, scope).await {
        Ok(report) => mcp_text(&format!(
            "Skill '{}' installed from hub '{}' into scope '{}' (scan: risk {}, {} finding(s)).",
            report.skill_name, report.hub, report.scope, report.risk_level, report.findings
        )),
        Err(e) => mcp_error(&e),
    }
}

/// Report curator lifecycle state; optionally force a pass.
pub(crate) async fn handle_skill_curator_status(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::custom_skills::{CurationStatus, CustomSkillStore};
    use duduclaw_gateway::skill_lifecycle::curator;

    let store = match CustomSkillStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return mcp_error(&format!("curation store: {e}")),
    };

    let force_run = params.get("run").and_then(|v| v.as_bool()).unwrap_or(false);
    let cfg = curator::CuratorConfig::load_from_home(home_dir);
    let mut pass_summary = String::new();
    if force_run {
        match curator::run_pass(home_dir, &store, &cfg, chrono::Utc::now()).await {
            Ok(report) => {
                pass_summary = format!(
                    "Pass executed: {} newly stale, {} archived, {} reactivated, {} error(s).\n\n",
                    report.newly_stale.len(),
                    report.newly_archived.len(),
                    report.reactivated.len(),
                    report.errors.len()
                );
            }
            Err(e) => return mcp_error(&format!("curator pass failed: {e}")),
        }
    }

    let rows = match store.curation_list().await {
        Ok(r) => r,
        Err(e) => return mcp_error(&e),
    };

    let mut stale = Vec::new();
    let mut archived = Vec::new();
    let mut unmanaged = Vec::new();
    let mut pinned = Vec::new();
    for r in &rows {
        let key = format!("{} [{}]", r.skill_name, r.scope);
        if r.pinned {
            pinned.push(key.clone());
        }
        match r.status {
            CurationStatus::Stale => stale.push(key),
            CurationStatus::Archived => archived.push(key),
            // Nested layouts the curator can't manage — tracked, never
            // archived (flagged once by the pass).
            CurationStatus::Unmanaged => unmanaged.push(key),
            CurationStatus::Active => {}
        }
    }

    fn block(title: &str, items: &[String]) -> String {
        if items.is_empty() {
            format!("**{title}**: (none)\n")
        } else {
            format!(
                "**{title}** ({}):\n{}\n",
                items.len(),
                items.iter().map(|i| format!("- {i}\n")).collect::<String>()
            )
        }
    }

    let text = format!(
        "{}Curator status — {} tracked skill(s) (enabled: {}, stale ≥ {}d, archive ≥ {}d)\n\n{}{}{}{}",
        pass_summary,
        rows.len(),
        cfg.enabled,
        cfg.stale_days,
        cfg.archive_days,
        block("Stale", &stale),
        block("Archived (recoverable via skill_pin)", &archived),
        block(
            "Unmanaged layout (tracked only, never auto-archived)",
            &unmanaged
        ),
        block("Pinned", &pinned),
    );
    mcp_text(&text)
}

/// Pin / unpin a skill (pin exempts from stale+archive; pinning an archived
/// skill restores it).
pub(crate) async fn handle_skill_pin(params: &Value, home_dir: &Path) -> Value {
    use duduclaw_gateway::custom_skills::CustomSkillStore;
    use duduclaw_gateway::skill_lifecycle::curator;

    let skill_name = params
        .get("skill_name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let scope_raw = params
        .get("scope")
        .and_then(|v| v.as_str())
        .unwrap_or("global");
    let pinned = params
        .get("pinned")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    if skill_name.is_empty() {
        return mcp_error("skill_name is required");
    }
    if !is_safe_path_component(skill_name) {
        return mcp_error("invalid skill_name (alphanumeric, hyphens, underscores only)");
    }
    let scope = if scope_raw == "global" {
        "global".to_string()
    } else {
        if !is_safe_path_component(scope_raw) {
            return mcp_error("invalid scope (use 'global' or a valid agent id)");
        }
        format!("agent:{scope_raw}")
    };

    let store = match CustomSkillStore::open(home_dir) {
        Ok(s) => s,
        Err(e) => return mcp_error(&format!("curation store: {e}")),
    };
    match curator::set_pin(home_dir, &store, skill_name, &scope, pinned).await {
        Ok(msg) => mcp_text(&msg),
        Err(e) => mcp_error(&e),
    }
}

/// List all skills installed for a specific agent, including global skills.
pub(crate) async fn handle_skill_list(params: &Value, home_dir: &Path) -> Value {
    let agent_id = params
        .get("agent_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_name = if agent_id.is_empty() {
        resolve_main_agent_name(home_dir).await
    } else {
        agent_id.to_string()
    };

    // Collect global skills from ~/.duduclaw/skills/.
    //
    // 2026-05-11: switched from flat `read_dir` to the recursive
    // `AgentRegistry::load_skills` so the Anthropic Skills spec
    // (`<skill-name>/SKILL.md`) is honoured alongside the legacy flat
    // `<skill>.md` layout. The only thing we need on top of `load_skills`
    // is the per-skill `description` for display — re-parse it from the
    // already-loaded content, no second `read_dir` round-trip.
    let global_skills_dir = home_dir.join("skills");
    let mut global_skills = Vec::new();
    let mut global_names = std::collections::HashSet::new();

    for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&global_skills_dir).await {
        // WP8: show the localised (zh-TW default) name/description so
        // non-English-reading employees can tell what a skill does; the
        // registry key (sk.name) stays the machine identity for override dedup.
        let meta =
            duduclaw_agent::skill_loader::parse_skill_meta_from_content(&sk.content, &sk.name);
        let locale = duduclaw_agent::skill_loader::DEFAULT_SKILL_LOCALE;
        global_names.insert(sk.name.clone());
        global_skills.push(format!(
            "- {}: {} (global)",
            meta.display_name(locale),
            meta.display_description(locale)
        ));
    }

    // Collect agent-local skills from ~/.duduclaw/agents/<agent>/SKILLS/
    let skills_dir = home_dir.join("agents").join(&agent_name).join("SKILLS");
    let mut agent_skills = Vec::new();

    for sk in duduclaw_agent::registry::AgentRegistry::load_skills(&skills_dir).await {
        let meta =
            duduclaw_agent::skill_loader::parse_skill_meta_from_content(&sk.content, &sk.name);
        let locale = duduclaw_agent::skill_loader::DEFAULT_SKILL_LOCALE;
        let suffix = if global_names.contains(&sk.name) {
            " (override)"
        } else {
            ""
        };
        agent_skills.push(format!(
            "- {}: {}{}",
            meta.display_name(locale),
            meta.display_description(locale),
            suffix
        ));
    }

    // Remove global skills that are overridden by agent-local
    let agent_local_names: std::collections::HashSet<String> = agent_skills
        .iter()
        .filter_map(|s| {
            s.strip_prefix("- ")
                .and_then(|s| s.split(':').next())
                .map(String::from)
        })
        .collect();
    global_skills.retain(|s| {
        let name = s
            .strip_prefix("- ")
            .and_then(|s| s.split(':').next())
            .unwrap_or("");
        !agent_local_names.contains(name)
    });

    let total = global_skills.len() + agent_skills.len();
    if total == 0 {
        serde_json::json!({
            "content": [{"type": "text", "text": format!(
                "No skills installed for agent '{agent_name}'."
            )}]
        })
    } else {
        let mut parts = Vec::new();
        if !global_skills.is_empty() {
            parts.push(format!(
                "**Global skills** ({}):\n{}",
                global_skills.len(),
                global_skills.join("\n")
            ));
        }
        if !agent_skills.is_empty() {
            parts.push(format!(
                "**Agent '{}' skills** ({}):\n{}",
                agent_name,
                agent_skills.len(),
                agent_skills.join("\n")
            ));
        }
        let text = format!("Total {} skill(s):\n\n{}", total, parts.join("\n\n"));
        serde_json::json!({
            "content": [{"type": "text", "text": text}]
        })
    }
}
