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

/// `inference.toml` `backend` values the inference engine can start. The
/// in-process `llama_cpp` / `mistral_rs` backends were removed on 2026-09-29;
/// those strings still deserialize (`duduclaw_inference::BackendType`) but fail
/// at init, so they are refused on write.
pub(crate) const SUPPORTED_INFERENCE_BACKENDS: &[&str] = &["openai_compat"];

/// What `inference.update` does with the root `backend` key.
#[derive(Debug, PartialEq)]
pub(crate) enum BackendWrite {
    /// Absent, or the stored value echoed back unchanged.
    Unchanged,
    /// Empty string: remove the key so the engine picks its default.
    Clear,
    Set(String),
}

/// Validate an incoming `backend`. Supported values and `""` are accepted.
/// A removed value is refused with a message naming the supported one —
/// except when it equals what is already stored: the dashboard echoes the
/// loaded value on every save, and an existing file must keep loading and
/// saving until the user picks a new backend.
pub(crate) fn inf_validate_backend(
    table: &toml::Table,
    p: &serde_json::Map<String, Value>,
) -> Result<BackendWrite, String> {
    let Some(raw) = p.get("backend") else {
        return Ok(BackendWrite::Unchanged);
    };
    if raw.is_null() {
        return Ok(BackendWrite::Unchanged);
    }
    let v = raw
        .as_str()
        .ok_or_else(|| "inference.backend must be a string".to_string())?
        .trim();
    if v.is_empty() {
        return Ok(BackendWrite::Clear);
    }
    if table.get("backend").and_then(|b| b.as_str()) == Some(v) {
        return Ok(BackendWrite::Unchanged);
    }
    if SUPPORTED_INFERENCE_BACKENDS.iter().any(|b| *b == v) {
        return Ok(BackendWrite::Set(v.to_string()));
    }
    Err(format!(
        "inference.backend '{v}' is not supported: the in-process llama_cpp / mistral_rs \
         backends were removed; use \"{}\" (a local OpenAI-compatible server such as \
         llama-server, Ollama or vLLM) or leave it empty",
        SUPPORTED_INFERENCE_BACKENDS.join("\", \"")
    ))
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
    // `backend` is validated before anything is written so a refused value
    // leaves the table untouched.
    let backend_write = inf_validate_backend(table, p)?;
    inf_apply_scalars(
        table,
        p,
        "inference",
        &["enabled", "auto_load"],
        // `max_memory_mb` was removed in v1.68 (no reader).
        &[],
        &[],
        &["models_dir", "default_model"],
        &[],
        &mut changes,
    )?;
    match backend_write {
        BackendWrite::Unchanged => {}
        BackendWrite::Clear => {
            if table.remove("backend").is_some() {
                changes.push("inference.backend = (auto)".into());
            }
        }
        BackendWrite::Set(v) => {
            changes.push(format!("inference.backend = \"{v}\""));
            table.insert("backend".into(), toml::Value::String(v));
        }
    }

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
            // v1.68: logprob capture for the UCCI calibrated cascade.
            // `gpu_layers` / `context_size` were removed (only the deleted
            // llama.cpp backend read them; llamafile has its own copies).
            &["capture_logprobs", "capture_top_logprobs"],
            &["max_tokens"],
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
        if let Some(v) = r.get("ucci_shadow_max_inflight") {
            if !v.as_u64().is_some_and(|n| (1..=16).contains(&n)) {
                return Err("router.ucci_shadow_max_inflight must be an integer 1-16".into());
            }
        }
        for k in ["local_tools", "ucci_shadow_strong", "ucci_drop_stop_token"] {
            if r.get(k).is_some_and(|v| !v.is_boolean() && !v.is_null()) {
                return Err(format!("router.{k} must be a boolean"));
            }
        }
        let section = inf_subtable(table, "router")?;
        inf_apply_scalars(
            section,
            r,
            "router",
            &["enabled", "local_tools", "ucci_shadow_strong", "ucci_drop_stop_token"],
            &["max_fast_prompt_tokens", "ucci_shadow_max_inflight"],
            &["fast_threshold", "strong_threshold"],
            &["fast_model", "strong_model"],
            &["cloud_keywords", "fast_keywords"],
            &mut changes,
        )?;
        // UCCI router / observation file paths (relative to the DuDuClaw
        // home or absolute). Empty string removes the key = feature off.
        for k in ["ucci_fast_router", "ucci_strong_router", "ucci_observations"] {
            match r.get(k) {
                None | Some(Value::Null) => {}
                Some(Value::String(p)) => {
                    let p = p.trim();
                    if p.is_empty() {
                        section.remove(k);
                        changes.push(format!("router.{k} cleared"));
                    } else if p.len() > 4096 || p.chars().any(char::is_control) {
                        return Err(format!("router.{k} must be a file path"));
                    } else {
                        section.insert(k.into(), toml::Value::String(p.into()));
                        changes.push(format!("router.{k} = \"{p}\""));
                    }
                }
                Some(_) => return Err(format!("router.{k} must be a string")),
            }
        }
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

    // ── [llamafile] (typed since v1.68; the generic pass-through could only
    // edit keys that already existed in the file). `[embedding]` had no
    // reader and is no longer accepted. ──
    if let Some(lf) = p.get("llamafile").and_then(|v| v.as_object()) {
        apply_llamafile(table, lf, &mut changes)?;
    }

    Ok(changes)
}

/// Keys `[llamafile]` accepts (see `duduclaw_inference::llamafile::LlamafileConfig`).
const LLAMAFILE_KEYS: &[&str] = &[
    "enabled", "dir", "default_file", "port", "host", "gpu_layers", "context_size", "extra_args",
];

/// Typed write of `[llamafile]`. The values reach a subprocess argv (no
/// shell), so each one is bounded.
pub(crate) fn apply_llamafile(
    table: &mut toml::Table,
    lf: &serde_json::Map<String, Value>,
    changes: &mut Vec<String>,
) -> Result<(), String> {
    if let Some(k) = lf.keys().find(|k| !LLAMAFILE_KEYS.contains(&k.as_str())) {
        return Err(format!("unknown llamafile key `{k}`"));
    }
    let text = |k: &str, v: &Value| -> Result<String, String> {
        let s = v.as_str().ok_or_else(|| format!("llamafile.{k} must be a string"))?.trim();
        if s.len() > 4096 || s.chars().any(char::is_control) {
            return Err(format!("llamafile.{k} contains invalid characters"));
        }
        Ok(s.to_string())
    };
    // Validate everything before touching the table.
    let mut writes: Vec<(&str, Option<toml::Value>)> = Vec::new();
    for (k, v) in lf {
        let k = LLAMAFILE_KEYS.iter().find(|x| **x == k.as_str()).copied().unwrap_or("");
        // `null` (and an empty string for the text fields / `extra_args`)
        // removes the key, so a saved value can be cleared from the form.
        let clears = v.is_null()
            || (matches!(k, "dir" | "host" | "default_file" | "extra_args")
                && v.as_str().is_some_and(|s| s.trim().is_empty()))
            || (k == "extra_args" && v.as_array().is_some_and(|a| a.is_empty()));
        if clears {
            writes.push((k, None));
            continue;
        }
        let tv = match k {
            "enabled" => Some(toml::Value::Boolean(v.as_bool().ok_or("llamafile.enabled must be a boolean")?)),
            "dir" => Some(toml::Value::String(text(k, v)?)),
            "default_file" => {
                let s = text(k, v)?;
                if s.contains('/') || s.contains('\\') || s == ".." {
                    return Err("llamafile.default_file must be a file name, not a path".into());
                }
                (!s.is_empty()).then_some(toml::Value::String(s))
            }
            "host" => {
                let s = text(k, v)?;
                if s != "localhost" && s.parse::<std::net::IpAddr>().is_err() {
                    return Err("llamafile.host must be an IP address or localhost".into());
                }
                Some(toml::Value::String(s))
            }
            "port" => {
                let n = v.as_u64().filter(|n| (1..=65535).contains(n)).ok_or("llamafile.port must be 1-65535")?;
                Some(toml::Value::Integer(n as i64))
            }
            "gpu_layers" => {
                let n = v.as_i64().filter(|n| (-1..=10_000).contains(n)).ok_or("llamafile.gpu_layers must be -1 (all) to 10000")?;
                Some(toml::Value::Integer(n))
            }
            "context_size" => {
                let n = v
                    .as_u64()
                    .filter(|n| (256..=1_048_576).contains(n))
                    .ok_or("llamafile.context_size must be 256-1048576")?;
                Some(toml::Value::Integer(n as i64))
            }
            "extra_args" => {
                let arr = v.as_array().ok_or("llamafile.extra_args must be an array of strings")?;
                if arr.len() > 32 {
                    return Err("llamafile.extra_args supports at most 32 entries".into());
                }
                let mut out = Vec::with_capacity(arr.len());
                for a in arr {
                    let s = a.as_str().ok_or("llamafile.extra_args must be an array of strings")?;
                    if s.len() > 256 || s.contains('\0') {
                        return Err("llamafile.extra_args entries must be at most 256 bytes".into());
                    }
                    out.push(toml::Value::String(s.to_string()));
                }
                Some(toml::Value::Array(out))
            }
            _ => unreachable!("filtered against LLAMAFILE_KEYS above"),
        };
        writes.push((k, tv));
    }
    let section = inf_subtable(table, "llamafile")?;
    for (k, tv) in writes {
        match tv {
            Some(v) => {
                changes.push(format!("llamafile.{k} updated"));
                section.insert(k.into(), v);
            }
            None => {
                section.remove(k);
                changes.push(format!("llamafile.{k} cleared"));
            }
        }
    }
    Ok(())
}
