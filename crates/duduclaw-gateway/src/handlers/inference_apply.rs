//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Get-or-create a sub-table by `key` on `table`.
pub(crate) fn inf_subtable<'a>(table: &'a mut toml::Table, key: &str) -> Result<&'a mut toml::Table, String> {
    table
        .entry(key)
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| format!("Invalid [{key}] section"))
}

/// Apply scalar `bool`/`i64`/`f64`/`str` fields from a params object to a TOML
/// table, recording changes. `prefix` is used only for the change message.
#[allow(clippy::too_many_arguments)]
pub(crate) fn inf_apply_scalars(
    section: &mut toml::Table,
    src: &serde_json::Map<String, Value>,
    prefix: &str,
    bools: &[&str],
    ints: &[&str],
    floats: &[&str],
    strings: &[&str],
    str_arrays: &[&str],
    changes: &mut Vec<String>,
) -> Result<(), String> {
    for k in bools {
        if let Some(v) = src.get(*k).and_then(|v| v.as_bool()) {
            section.insert((*k).into(), toml::Value::Boolean(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in ints {
        if let Some(v) = src.get(*k).and_then(|v| v.as_i64()) {
            section.insert((*k).into(), toml::Value::Integer(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in floats {
        if let Some(v) = src.get(*k).and_then(|v| v.as_f64()) {
            section.insert((*k).into(), toml::Value::Float(v));
            changes.push(format!("{prefix}.{k} = {v}"));
        }
    }
    for k in strings {
        if let Some(v) = src.get(*k).and_then(|v| v.as_str()) {
            section.insert((*k).into(), toml::Value::String(v.into()));
            changes.push(format!("{prefix}.{k} = \"{v}\""));
        }
    }
    for k in str_arrays {
        if let Some(arr) = src.get(*k).and_then(|v| v.as_array()) {
            let mut out = Vec::with_capacity(arr.len());
            for item in arr {
                let s = item
                    .as_str()
                    .ok_or_else(|| format!("{prefix}.{k} entries must be strings"))?;
                out.push(toml::Value::String(s.into()));
            }
            section.insert((*k).into(), toml::Value::Array(out));
            changes.push(format!("{prefix}.{k} = [{} entries]", arr.len()));
        }
    }
    Ok(())
}

/// Apply an `inference.update` params object onto an inference.toml table.
/// Returns the change list. Validates router thresholds (strong < fast) and
/// generation ranges. Does NOT handle the openai_compat secret — that is done
/// in the async handler so it can call `config_crypto::encrypt_value`.
pub(crate) fn apply_inference_to_table(
    table: &mut toml::Table,
    params: &Value,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();
    let p = params
        .as_object()
        .ok_or_else(|| "params must be an object".to_string())?;

    // ── root scalars (INF.2) ──
    inf_apply_scalars(
        table,
        p,
        "inference",
        &["enabled", "auto_load"],
        &["max_memory_mb"],
        &[],
        &["backend", "models_dir", "default_model"],
        &[],
        &mut changes,
    )?;

    // ── [generation] (INF.3) ──
    if let Some(g) = p.get("generation").and_then(|v| v.as_object()) {
        if let Some(t) = g.get("temperature").and_then(|v| v.as_f64()) {
            if !(0.0..=2.0).contains(&t) {
                return Err("generation.temperature must be 0.0-2.0".into());
            }
        }
        if let Some(tp) = g.get("top_p").and_then(|v| v.as_f64()) {
            if !(0.0..=1.0).contains(&tp) {
                return Err("generation.top_p must be 0.0-1.0".into());
            }
        }
        let section = inf_subtable(table, "generation")?;
        inf_apply_scalars(
            section,
            g,
            "generation",
            &[],
            &["max_tokens", "gpu_layers", "context_size"],
            &["temperature", "top_p"],
            &[],
            &["stop"],
            &mut changes,
        )?;
    }

    // ── [router] (INF.4) — cross-field validation strong < fast ──
    if let Some(r) = p.get("router").and_then(|v| v.as_object()) {
        // Resolve the *effective* thresholds (incoming overrides existing).
        let existing = table.get("router").and_then(|v| v.as_table());
        let fast = r
            .get("fast_threshold")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                existing
                    .and_then(|e| e.get("fast_threshold"))
                    .and_then(|v| v.as_float())
            });
        let strong = r
            .get("strong_threshold")
            .and_then(|v| v.as_f64())
            .or_else(|| {
                existing
                    .and_then(|e| e.get("strong_threshold"))
                    .and_then(|v| v.as_float())
            });
        if let (Some(f), Some(s)) = (fast, strong) {
            if s >= f {
                return Err(format!(
                    "router.strong_threshold ({s}) must be < router.fast_threshold ({f})"
                ));
            }
        }
        for (name, val) in [("fast_threshold", fast), ("strong_threshold", strong)] {
            if let Some(v) = val {
                if !(0.0..=1.0).contains(&v) {
                    return Err(format!("router.{name} must be 0.0-1.0"));
                }
            }
        }
        let section = inf_subtable(table, "router")?;
        inf_apply_scalars(
            section,
            r,
            "router",
            &["enabled"],
            &["max_fast_prompt_tokens"],
            &["fast_threshold", "strong_threshold"],
            &["fast_model", "strong_model"],
            &["cloud_keywords", "fast_keywords"],
            &mut changes,
        )?;
    }

    // ── [openai_compat] non-secret fields (INF.5; api_key handled in caller) ──
    if let Some(oc) = p.get("openai_compat").and_then(|v| v.as_object()) {
        let section = inf_subtable(table, "openai_compat")?;
        inf_apply_scalars(
            section,
            oc,
            "openai_compat",
            &[],
            &[],
            &[],
            &["base_url", "model"],
            &[],
            &mut changes,
        )?;
    }

    // ── Generic pass-through sub-sections (INF.5) ──
    // Each is a flat table of scalars/arrays; apply them generically so new
    // backend fields don't require per-field plumbing. Secrets are not expected
    // in these sections.
    // `exo` / `mlx` / `mistralrs` dropped 2026-09-29 with their backends
    // (`wiki/reports/feature-audit-2026-09-29.md` T1-D2, T3-S4/S5).
    // `llmlingua` / `streaming_llm` dropped 2026-09-29 too (G8): the
    // three-strategy compressor they configured was removed from
    // `duduclaw-inference` in v1.33 — `grep -rn 'llmlingua\|streaming_llm'
    // crates/` returns nothing outside this file, so writing those tables only
    // produced config keys nothing would ever read.
    for sect in &["llamafile", "embedding"] {
        if let Some(obj) = p.get(*sect).and_then(|v| v.as_object()) {
            let section = inf_subtable(table, sect)?;
            for (k, val) in obj {
                let tv = json_to_toml(val)
                    .ok_or_else(|| format!("Unsupported value type for {sect}.{k}"))?;
                section.insert(k.clone(), tv);
            }
            changes.push(format!("{sect} = [updated]"));
        }
    }

    Ok(changes)
}
