//! The four legacy pack-manifest readers, split verbatim out of `pack.rs`
//! (file-size split only — see [`super`] for the schema, the canonical
//! `pack.toml` reader and the shared TOML helpers this file uses).

use super::*;

// ─────────────────────────── legacy loader 1: preset.toml ───────────────

/// `presets/<id>/preset.toml` → [`PackKind::Preset`].
///
/// Reuses [`crate::preset::parse_preset`] verbatim so the sanitizer, the
/// org-field refusal and the `[preset]`-metadata stripping are literally the
/// same code the live preset resolver runs — a second implementation here
/// would be a place for the two to drift apart.
pub fn parse_legacy_preset(id: &str, text: &str) -> Result<Pack, PackError> {
    let parsed = crate::preset::parse_preset(id, text).map_err(|e| match e {
        crate::preset::PresetError::OrgFieldsRejected(f) => PackError::OrgFieldsRejected(f),
        crate::preset::PresetError::NotFound(what) => PackError::NotFound(what),
        crate::preset::PresetError::Invalid(reason) => PackError::Invalid(reason),
    })?;
    Ok(Pack {
        id: id.trim().to_string(),
        kind: PackKind::Preset,
        tier: PackTier::Free,
        version: parsed.meta.version,
        display_name: label_to_map(&parsed.meta.label),
        description: parsed.meta.description,
        config: parsed.config,
        source: PackSource::LegacyPreset,
        ..Pack::default()
    })
}

// ─────────────────────────── legacy loader 2: expert.toml ───────────────

/// `<slug>/expert.toml` → [`PackKind::Team`].
pub fn parse_legacy_expert(id: &str, text: &str) -> Result<Pack, PackError> {
    let root = parse_toml(text)?;
    let e = root.get("expert").and_then(|v| v.as_table());
    if e.is_none() {
        return Err(PackError::Invalid("缺少 [expert] 區段".into()));
    }

    let declared = str_field(e, "name");
    let id_mismatch = (!declared.trim().is_empty() && declared.trim() != id.trim())
        .then(|| declared.trim().to_string());

    let mut display_name = BTreeMap::new();
    if let Some(dn) = sub_table(e, "display_name") {
        for (k, v) in dn {
            if let Some(s) = v.as_str().filter(|s| !s.trim().is_empty()) {
                display_name.insert(k.clone(), s.trim().to_string());
            }
        }
    }

    let agents = table_array(e, "agents")
        .into_iter()
        .map(|a| PackAgent {
            name: str_field(Some(a), "name"),
            role: str_field(Some(a), "role"),
            display_name: str_field(Some(a), "display_name"),
            reports_to: str_field(Some(a), "reports_to"),
            department: str_field(Some(a), "department"),
            rank: str_field(Some(a), "rank"),
            trigger: str_field(Some(a), "trigger"),
            skills: string_vec(Some(a), "skills"),
            ..PackAgent::default()
        })
        .collect();

    Ok(Pack {
        id: id.trim().to_string(),
        kind: PackKind::Team,
        tier: PackTier::Free,
        version: str_field(e, "version"),
        display_name,
        description: str_field(e, "description"),
        author: str_field(e, "author"),
        license: str_field(e, "license"),
        category: str_field(e, "category"),
        tags: string_vec(e, "tags"),
        requires: PackRequires {
            env: string_vec(sub_table(e, "requires"), "env"),
            bins: string_vec(sub_table(e, "requires"), "bins"),
        },
        prompts: string_vec(sub_table(e, "prompts"), "recommended"),
        channels: string_vec(sub_table(e, "channels"), "suggested"),
        agents,
        source: PackSource::LegacyExpert,
        id_mismatch,
        ..Pack::default()
    })
}

// ─────────────────────────── legacy loader 3: team.toml ─────────────────

/// `teams/<industry>-team/team.toml` → [`PackKind::Team`].
///
/// Roster shape matches what `duduclaw expert convert-teams` produces today,
/// **field for field** — that converter's `expert.toml` is what the built-in
/// catalog actually installs, so any divergence here would mean "the pack you
/// inspect" and "the pack you install" disagree. Specifically:
///
/// - the front desk is the single pack root (`reports_to` empty), rank
///   `manager`, trigger `@<display>`, and carries **no** `department` — the
///   converter deliberately omits it (the dashboard `templates.create_agent`
///   staging path stamps `industry` instead; that is a different flow and is
///   not what this loader mirrors);
/// - every worker reports to the front desk, rank `staff`, trigger falling
///   back to its `name`, department = explicit override else
///   [`crate::org::department_for_kit`];
/// - a blank `display_name` falls back to `name` on both.
///
/// The front desk's dispatch skill is **not** derived here: the converter
/// mines it out of `TEAM.md`, which is a sibling file this loader never
/// reads. That is missing input, not a lossy read of `team.toml`.
///
/// `schema` is deliberately **not** carried into [`Pack`] — `Pack` has its own
/// schema (the canonical format's) and two answers to "what schema is this"
/// is how formats rot. The `team.toml` schema check stays with its own
/// reader (`premium_templates::load_team_manifest`).
pub fn parse_legacy_team(id: &str, text: &str) -> Result<Pack, PackError> {
    let root = parse_toml(text)?;
    let t = Some(&root);

    let Some(fd) = sub_table(t, "front_desk") else {
        return Err(PackError::Invalid("缺少 [front_desk] 區段".into()));
    };
    let fd_name = str_field(Some(fd), "name");
    if fd_name.trim().is_empty() {
        return Err(PackError::Invalid("front_desk 缺少 name".into()));
    }

    let industry = str_field(t, "industry");
    let label = str_field(t, "label");
    let source_pack = str_field(t, "pack");

    let or_name = |raw: String, name: &str| {
        if raw.trim().is_empty() {
            name.to_string()
        } else {
            raw
        }
    };

    let fd_display = or_name(str_field(Some(fd), "display_name"), &fd_name);
    let mut agents = vec![PackAgent {
        name: fd_name.clone(),
        role: "front_desk".to_string(),
        display_name: fd_display.clone(),
        reports_to: String::new(),
        department: String::new(),
        rank: "manager".to_string(),
        trigger: format!("@{fd_display}"),
        summary: str_field(Some(fd), "summary"),
        ..PackAgent::default()
    }];

    for w in table_array(t, "workers") {
        let kit = str_field(Some(w), "kit");
        let name = str_field(Some(w), "name");
        let declared_dept = str_field(Some(w), "department");
        let department = if declared_dept.trim().is_empty() {
            org::department_for_kit(&kit)
                .unwrap_or_default()
                .to_string()
        } else {
            declared_dept.trim().to_string()
        };
        agents.push(PackAgent {
            display_name: or_name(str_field(Some(w), "display_name"), &name),
            trigger: or_name(str_field(Some(w), "trigger"), &name),
            name,
            role: "worker".to_string(),
            reports_to: fd_name.clone(),
            department,
            rank: "staff".to_string(),
            summary: str_field(Some(w), "summary"),
            overlay: string_vec(Some(w), "overlay"),
            kit,
            ..PackAgent::default()
        });
    }

    let humans = table_array(t, "humans")
        .into_iter()
        .map(|h| PackHumanRole {
            title: str_field(Some(h), "title"),
            summary: str_field(Some(h), "summary"),
        })
        .collect();

    let excluded = table_array(t, "excluded")
        .into_iter()
        .map(|x| PackExcludedKit {
            kit: str_field(Some(x), "kit"),
            reason: str_field(Some(x), "reason"),
        })
        .collect();

    let mut tags = Vec::new();
    if !industry.trim().is_empty() {
        tags.push(industry.trim().to_string());
    }
    if !source_pack.trim().is_empty() {
        tags.push(source_pack.trim().to_string());
    }

    Ok(Pack {
        id: id.trim().to_string(),
        kind: PackKind::Team,
        // Team playbooks live only in the gitignored premium tree.
        tier: PackTier::Premium,
        version: String::new(),
        display_name: label_to_map(&label),
        description: label,
        tags,
        examples: string_vec(t, "examples"),
        agents,
        humans,
        excluded,
        source: PackSource::LegacyTeam,
        ..Pack::default()
    })
}

// ──────────────────── legacy loader 4: `<industry>-pro/` ────────────────

/// A premium industry pack directory (`SOUL.md` + optional `template.toml`)
/// → [`PackKind::Template`]. `fallback_label` lets the caller pass the
/// `premium_templates::label_for_slug` table without this crate having to
/// know it.
pub fn parse_legacy_industry(
    id: &str,
    template_toml: Option<&str>,
    fallback_label: &str,
) -> Result<Pack, PackError> {
    let mut label = String::new();
    if let Some(text) = template_toml {
        let root = parse_toml(text)?;
        let raw = str_field(Some(&root), "label");
        let trimmed = raw.trim();
        // Fail-closed on a hostile/broken manifest the same way
        // `premium_templates::label_from_manifest` does: the fallback wins
        // rather than a control-character or 500-char "label" reaching a menu.
        if !trimmed.is_empty()
            && trimmed.chars().count() <= 80
            && !trimmed.chars().any(char::is_control)
        {
            label = trimmed.to_string();
        }
    }
    if label.is_empty() {
        label = fallback_label.trim().to_string();
    }
    Ok(Pack {
        id: id.trim().to_string(),
        kind: PackKind::Template,
        tier: PackTier::Premium,
        display_name: label_to_map(&label),
        description: label,
        source: PackSource::LegacyIndustry,
        ..Pack::default()
    })
}
