//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Per-caller rate limit for `redaction.suggest_pattern` (§13.2: 10/min).
///
/// Reuses `duduclaw_security::rate_limiter::RateLimiter` — the same sliding
/// window `device.power_local` uses — rather than hand-rolling a bucket.
pub(crate) fn suggest_pattern_limiter() -> &'static duduclaw_security::rate_limiter::RateLimiter {
    static LIMITER: std::sync::LazyLock<duduclaw_security::rate_limiter::RateLimiter> =
        std::sync::LazyLock::new(|| {
            duduclaw_security::rate_limiter::RateLimiter::new(
                10,
                std::time::Duration::from_secs(60),
            )
        });
    &LIMITER
}

/// Build the [`duduclaw_redaction::EngineOptions`] a config table resolves to
/// — the identity directory plus the operator's data-source registry.
///
/// Shared by the rule-pack importer so an imported rule is validated against
/// exactly the ambient resources the live manager will hand it.
pub(crate) fn redaction_engine_options(
    table: &toml::Table,
    home_dir: &Path,
) -> Result<duduclaw_redaction::EngineOptions, String> {
    #[derive(serde::Deserialize)]
    struct Wrap {
        #[serde(default)]
        redaction: duduclaw_redaction::RedactionConfig,
    }
    let raw = toml::to_string(table).map_err(|e| format!("serialize config: {e}"))?;
    let cfg = toml::from_str::<Wrap>(&raw)
        .map_err(|e| format!("parse [redaction]: {e}"))?
        .redaction;
    let paths = duduclaw_redaction::ManagerPaths::under_home(home_dir);
    Ok(duduclaw_redaction::EngineOptions {
        identity_people_dir: paths.identity_people_dir,
        data_sources: duduclaw_redaction::resolve_data_sources(&cfg).map_err(|e| e.to_string())?,
        // §13.4: a dry-compile must resolve the model exactly where the live
        // manager would, or an `ai_pii` rule would pass validation here and
        // fail at boot.
        ner_dirs: paths.ner_dirs,
        ner: cfg.ner.clone(),
    })
}

/// Dry-compile a candidate `config.toml` table: resolve the whole rule-spec
/// list exactly the way `RedactionManager::open` does and build the engine.
/// Errors carry the compiler's own text so the editor can show it verbatim.
pub(crate) fn dry_compile_redaction_table(table: &toml::Table, home_dir: &Path) -> Result<(), String> {
    #[derive(serde::Deserialize)]
    struct Wrap {
        #[serde(default)]
        redaction: duduclaw_redaction::RedactionConfig,
    }
    let raw = toml::to_string(table).map_err(|e| format!("serialize config: {e}"))?;
    let cfg = toml::from_str::<Wrap>(&raw)
        .map_err(|e| format!("parse [redaction]: {e}"))?
        .redaction;
    cfg.validate().map_err(|e| e.to_string())?;
    let paths = duduclaw_redaction::ManagerPaths::under_home(home_dir);
    let specs = duduclaw_redaction::resolve_rule_specs(&cfg, &paths).map_err(|e| e.to_string())?;
    // The data-source registry is resolved the same way `RedactionManager::open`
    // does, so a `db_field` rule pointing at a source this config does not
    // define (or a source whose record paths do not parse) fails HERE rather
    // than at the next boot.
    let data_sources = duduclaw_redaction::resolve_data_sources(&cfg).map_err(|e| e.to_string())?;
    duduclaw_redaction::RuleEngine::from_specs_with(
        specs,
        &duduclaw_redaction::EngineOptions {
            identity_people_dir: paths.identity_people_dir.clone(),
            data_sources,
            ner_dirs: paths.ner_dirs.clone(),
            ner: cfg.ner.clone(),
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

/// Wire shape of one data source for `redaction.get` (§13.5).
pub(crate) fn data_source_wire(name: &str, def: &duduclaw_redaction::DataSourceDef, builtin: bool) -> Value {
    json!({
        "name": name,
        "label": if def.label.trim().is_empty() { name } else { def.label.as_str() },
        "builtin": builtin,
        "tools": def.tools,
        "table_arg": def.table_arg,
        "table": def.table,
        "table_result": def.table_result,
        "record_paths": def.record_paths,
        "free_form_names": def.free_form_names,
        "key_alias": def.key_alias,
    })
}

/// The `data_sources` array for `redaction.get`: the built-ins first
/// (`builtin: true`, not editable), then the operator's
/// `[redaction.data_sources.*]` entries in name order.
///
/// A built-in shows the simple form its bindings can actually prove — Odoo
/// mixes per-tool tables and aliases, so its `table` / `table_arg` come back
/// `null` rather than as a summary that reads like fact. A config entry that
/// does not deserialise is skipped (there is nothing to render); one that
/// deserialises but will not load is shown verbatim, so the operator can see
/// and fix the entry that is blocking the boot.
pub(crate) fn redaction_data_sources(table: &toml::Table) -> Vec<Value> {
    let mut out: Vec<Value> = duduclaw_redaction::builtin_sources()
        .iter()
        .map(|s| data_source_wire(&s.name, &s.simple_form(), true))
        .collect();

    let Some(defs) = table
        .get("redaction")
        .and_then(|v| v.as_table())
        .and_then(|r| r.get("data_sources"))
        .and_then(|v| v.as_table())
    else {
        return out;
    };
    for (name, body) in defs {
        // A config entry may not shadow a built-in (the loader refuses it);
        // the built-in row above is the one that takes effect.
        if duduclaw_redaction::is_builtin_source(name) {
            continue;
        }
        let Ok(def) = body.clone().try_into::<duduclaw_redaction::DataSourceDef>() else {
            continue;
        };
        let shown = def
            .clone()
            .into_source(name)
            .map(|s| s.simple_form())
            .unwrap_or(def);
        out.push(data_source_wire(name, &shown, false));
    }
    out
}

/// Rule ids whose `db_field` source resolves to `name`, in id order.
///
/// Resolution matches the crate's (`source` wins, `connector` is its
/// deprecated alias, neither ⇒ `odoo`) so a rule written either way still
/// pins its source against deletion.
pub(crate) fn data_source_referenced_by(red: &toml::Table, name: &str) -> Vec<String> {
    let Some(rules) = red.get("rules").and_then(|v| v.as_table()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = Vec::new();
    for (id, body) in rules {
        let Some(body) = body.as_table() else {
            continue;
        };
        if body.get("type").and_then(|v| v.as_str()) != Some("db_field") {
            continue;
        }
        let source = body
            .get("source")
            .and_then(|v| v.as_str())
            .or_else(|| body.get("connector").and_then(|v| v.as_str()))
            .unwrap_or(duduclaw_redaction::rules::db_field::DEFAULT_SOURCE);
        if source == name {
            out.push(id.clone());
        }
    }
    out.sort();
    out
}

/// Apply the `data_sources` upsert-merge onto `[redaction.data_sources.*]`
/// (§13.5).
///
/// Same semantics as `field_rules`: `null` removes that name, an absent name
/// is untouched, and everything is validated BEFORE the table is mutated.
/// Two refusals are specific to sources: a built-in name is never writable
/// (shadowing `odoo` with a half-specified copy would quietly unbind columns
/// that used to be masked), and a source still named by a `db_field` rule
/// cannot be deleted — the rule would stop compiling at the next boot.
pub(crate) fn apply_data_sources_to_table(
    red: &mut toml::Table,
    data_sources: &serde_json::Map<String, Value>,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    // ── Validate everything first (no partial writes) ──
    let mut planned: Vec<(String, Option<toml::Value>)> = Vec::new();
    for (name, body) in data_sources {
        let name_trim = name.trim();
        if !duduclaw_redaction::is_valid_data_source_name(name_trim) {
            return Err(format!(
                "Invalid data_sources name '{name_trim}'. Must match ^[a-z][a-z0-9_-]{{0,63}}$"
            ));
        }
        if duduclaw_redaction::is_builtin_source(name_trim) {
            return Err(format!(
                "'{name_trim}' is a built-in data source — it cannot be edited or removed."
            ));
        }
        if body.is_null() {
            let refs = data_source_referenced_by(red, name_trim);
            if !refs.is_empty() {
                return Err(format!(
                    "data source '{name_trim}' is still referenced by rule {} — change or remove the rule first",
                    refs.join(", ")
                ));
            }
            planned.push((name_trim.to_string(), None));
            continue;
        }
        if !body.is_object() {
            return Err(format!(
                "data_sources.{name_trim} must be an object or null"
            ));
        }
        let def: duduclaw_redaction::DataSourceDef = serde_json::from_value(body.clone())
            .map_err(|e| format!("data_sources.{name_trim} is not a valid definition: {e}"))?;
        // Surface the precise reason here (empty tools / table XOR / bad
        // record path) instead of leaving it to the whole-config dry compile.
        def.clone()
            .into_source(name_trim)
            .map_err(|e| e.to_string())?;
        let toml_body = toml::Value::try_from(&def)
            .map_err(|e| format!("data_sources.{name_trim} cannot be serialised: {e}"))?;
        planned.push((name_trim.to_string(), Some(toml_body)));
    }

    // ── Commit ──
    for (name, body) in planned {
        match body {
            None => {
                if let Some(srcs) = red.get_mut("data_sources").and_then(|v| v.as_table_mut()) {
                    srcs.remove(&name);
                }
                changes.push(format!("redaction.data_sources.{name} removed"));
            }
            Some(body) => {
                let tools = body
                    .as_table()
                    .and_then(|t| t.get("tools"))
                    .and_then(|v| v.as_array())
                    .map(|a| a.len())
                    .unwrap_or(0);
                let srcs = red
                    .entry("data_sources")
                    .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
                    .as_table_mut()
                    .ok_or_else(|| "Invalid [redaction.data_sources] section".to_string())?;
                srcs.insert(name.clone(), body);
                changes.push(format!(
                    "redaction.data_sources.{name} = {{ tools = {tools} }}"
                ));
            }
        }
    }
    Ok(())
}

/// Profile catalogue for the dashboard's field picker: every built-in profile
/// plus any custom profile at `<home>/redaction/profiles/*.toml`, each with
/// its rule count and the PII categories (fields) it covers.
pub(crate) fn redaction_available_profiles(home_dir: &std::path::Path) -> Vec<Value> {
    fn summary(name: &str, p: &duduclaw_redaction::config::Profile, builtin: bool) -> Value {
        // Not the declared `category` fields: a `ner` rule declares one
        // placeholder and emits one category per label, so the profile's own
        // accessor is the only thing that knows what `ai_pii` actually covers.
        let cats: Vec<String> = p.categories();
        json!({
            "name": name,
            "description": p.meta.description,
            "builtin": builtin,
            // §13.2: the explicit inverse of `builtin` — a custom profile is
            // the only kind `redaction.profiles.remove` may delete, and the
            // card needs to know that without re-deriving the rule.
            "custom": !builtin,
            "label": p.meta.name,
            "rule_count": p.rules.len(),
            "categories": cats,
            // §13.4: true for any profile holding a `type = "ner"` rule, so
            // the card shows the download button by capability rather than by
            // hard-coding the name `ai_pii` (an imported rule pack with a
            // `ner` rule gets the same treatment).
            "requires_model": p.requires_model(),
        })
    }

    let mut out = Vec::new();
    let mut names: Vec<&str> = duduclaw_redaction::profiles::builtin_profiles()
        .keys()
        .copied()
        .collect();
    names.sort_unstable();
    for name in names {
        if let Ok(Some(p)) = duduclaw_redaction::profiles::load_builtin(name) {
            out.push(summary(name, &p, true));
        }
    }
    // Custom profiles — same directory the manager resolves at boot.
    let custom_dir = home_dir.join("redaction").join("profiles");
    if let Ok(entries) = std::fs::read_dir(&custom_dir) {
        let mut files: Vec<_> = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("toml"))
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            match duduclaw_redaction::config::Profile::from_path(&path) {
                Ok(p) => out.push(summary(name, &p, false)),
                Err(e) => {
                    warn!(profile = %name, error = %e, "custom redaction profile unreadable — skipped from catalogue")
                }
            }
        }
    }
    out
}

/// Parse a config.toml `[redaction]` section into the `redaction.get`
/// response shape.
// ── RFC-23 §14.4: `[redaction] data_file_guard` (WP-F2) ─────────────────────
//
// Deliberately two small standalone functions rather than fields threaded
// through `redaction_table_to_response` / `apply_redaction_to_table`: the
// field is owned by a different work package from the data-source editor that
// shares those two functions, and keeping the edit surface disjoint is what
// lets both land without stepping on each other.

/// Read `[redaction] data_file_guard` for the wire response.
///
/// Absent / non-string / unrecognized ⇒ the default `"on"`, matching
/// [`crate::redaction_proxy::data_file_guard_mode`] — the dashboard must show
/// the value the spawn sites will actually use, not the literal file content.
pub(crate) fn redaction_data_file_guard(table: &toml::Table) -> String {
    let raw = table
        .get("redaction")
        .and_then(|r| r.as_table())
        .and_then(|r| r.get("data_file_guard"))
        .and_then(|v| v.as_str())
        .unwrap_or(crate::redaction_proxy::DATA_FILE_GUARD_DEFAULT)
        .trim()
        .to_ascii_lowercase();
    if crate::redaction_proxy::DATA_FILE_GUARD_MODES.contains(&raw.as_str()) {
        raw
    } else {
        crate::redaction_proxy::DATA_FILE_GUARD_DEFAULT.to_string()
    }
}

/// Apply a `data_file_guard` field from a `redaction.update` payload.
///
/// Three-value validated: an unrecognized string is an ERROR here (unlike the
/// read path's fail-safe default) because a dashboard that silently turned
/// the operator's `"readonly"` typo into `"on"` would be lying about what it
/// saved. Absent ⇒ no change, no entry in `changes`.
pub(crate) fn apply_data_file_guard_to_table(
    table: &mut toml::Table,
    params: &Value,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    let Some(raw) = params.get("data_file_guard") else {
        return Ok(());
    };
    let mode = raw
        .as_str()
        .ok_or_else(|| "data_file_guard must be a string".to_string())?
        .trim()
        .to_ascii_lowercase();
    if !crate::redaction_proxy::DATA_FILE_GUARD_MODES.contains(&mode.as_str()) {
        return Err(format!(
            "data_file_guard must be one of {}",
            crate::redaction_proxy::DATA_FILE_GUARD_MODES.join(" / ")
        ));
    }
    let red = table
        .entry("redaction")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| "Invalid [redaction] section".to_string())?;
    red.insert("data_file_guard".into(), toml::Value::String(mode.clone()));
    changes.push(format!("redaction.data_file_guard = {mode}"));
    Ok(())
}
