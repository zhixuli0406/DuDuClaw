//! v1.68.0 — `config.raw.get` / `config.raw.set` (admin): the "設定檔進階編輯"
//! raw TOML editor for `config.toml`, `inference.toml` and one employee's
//! `agent.toml`.
//!
//! * **Masking.** Every value whose key ends in `_enc` or names a secret
//!   (`token`, `secret`, `password`, `api_key`, `widget_key`, `*_key`, …), every
//!   value inside a `headers` / `otlp_headers` / `env` table, and every URL
//!   carrying a password in its userinfo is shown as `«set»`. `[mcp_keys]` is
//!   left out entirely: its *table keys* are the secrets. Formatting and
//!   comments survive (`toml_edit`).
//! * **Restore.** On `set`, a value that is exactly `«set»` takes the stored
//!   value at the same place (array-of-tables entries are matched by their
//!   `id` / `name`, so reordering `[[accounts]]` cannot hand one account the
//!   other's key). `[mcp_keys]` is re-attached from disk.
//! * **Validation.** The result must parse with the typed loader for the file
//!   (`AgentConfig`; `InferenceConfig` + `validate()`; for `config.toml`, which
//!   has no single typed loader at boot, every section validator the gateway
//!   applies at use time: task sandbox, computer-use image, tick sources, team,
//!   takeover, redaction, database sources). Errors carry line and column.
//! * **Write.** Backup `<file>.bak-<unix ts>` (last five kept), locked atomic
//!   write refused when the file changed since it was read, security audit
//!   row `config_raw_edited`, the same hot reloads `system.update_config`
//!   performs, and `restart_required` for the boot-only sections that changed.

#[allow(unused_imports)]
use super::*;

use toml_edit::{DocumentMut, Item, Table as EditTable, Value as EditValue};

use super::config_commit::{RAW_SECRET_MASK, commit_text_locked, content_hash, read_text_or_empty, write_backup};

/// Largest raw file the editor accepts (the real files are a few KB).
const MAX_RAW_BYTES: usize = 1024 * 1024;

/// Tables whose every string value is a credential, whatever the key.
const SECRET_CONTAINERS: &[&str] = &["headers", "otlp_headers", "env"];

/// Tables hidden from the editor because their keys are secrets.
const HIDDEN_SECTIONS: &[&str] = &["mcp_keys"];

/// `config.toml` sections read once at boot (a raw edit needs a restart).
const CONFIG_BOOT_SECTIONS: &[&str] = &[
    "gateway", "server", "telemetry", "logging", "channels", "wiki", "relay", "decision",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawFile {
    Config,
    Inference,
    Agent(String),
}

impl RawFile {
    fn kind(&self) -> &'static str {
        match self {
            RawFile::Config => "config",
            RawFile::Inference => "inference",
            RawFile::Agent(_) => "agent",
        }
    }
}

/// Is `key` the name of a secret value?
pub(crate) fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    k.ends_with("_enc")
        || k == "key"
        || k.ends_with("_key")
        || [
            "token", "secret", "password", "passwd", "api_key", "apikey", "widget_key", "private_key",
            "credential", "service_account_json",
        ]
        .iter()
        .any(|p| k.contains(p))
}

/// `scheme://user:password@host…` — a URL that carries a password.
pub(crate) fn url_has_password(s: &str) -> bool {
    let Some((_, rest)) = s.split_once("://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    authority.rsplit_once('@').is_some_and(|(userinfo, _)| userinfo.contains(':'))
}

fn is_secret_query_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["token", "key", "secret", "password", "sig", "auth"].iter().any(|p| n.contains(p))
}

/// A URL whose query string carries a credential-named parameter.
pub(crate) fn url_has_secret_query(s: &str) -> bool {
    if !s.contains("://") {
        return false;
    }
    let Some((_, q)) = s.split_once('?') else { return false };
    let q = q.split('#').next().unwrap_or("");
    q.split('&').any(|kv| {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        !v.is_empty() && is_secret_query_name(k)
    })
}

/// Display form of a URL: userinfo and credential-named query values
/// replaced by the mask. Not reversible — for read-only views.
pub(crate) fn mask_url_for_display(s: &str) -> String {
    let Some((scheme, rest)) = s.split_once("://") else { return s.to_string() };
    let (before_frag, frag) = match rest.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (rest, None),
    };
    let (main, query) = match before_frag.split_once('?') {
        Some((m, q)) => (m, Some(q)),
        None => (before_frag, None),
    };
    let (authority, path) = match main.find('/') {
        Some(i) => (&main[..i], &main[i..]),
        None => (main, ""),
    };
    let authority = match authority.rsplit_once('@') {
        Some((_, host)) => format!("{RAW_SECRET_MASK}@{host}"),
        None => authority.to_string(),
    };
    let mut out = format!("{scheme}://{authority}{path}");
    if let Some(q) = query {
        let parts: Vec<String> = q
            .split('&')
            .map(|kv| match kv.split_once('=') {
                Some((k, v)) if !v.is_empty() && is_secret_query_name(k) => format!("{k}={RAW_SECRET_MASK}"),
                _ => kv.to_string(),
            })
            .collect();
        out.push('?');
        out.push_str(&parts.join("&"));
    }
    if let Some(f) = frag {
        out.push('#');
        out.push_str(f);
    }
    out
}

fn join(path: &str, key: &str) -> String {
    if path.is_empty() { key.to_string() } else { format!("{path}.{key}") }
}

/// Identity of an array-of-tables entry: `id=…` / `name=…`, if present.
fn table_identity(t: &dyn toml_edit::TableLike) -> Option<String> {
    ["id", "name"]
        .iter()
        .find_map(|k| t.get(k).and_then(|i| i.as_str()).map(|v| format!("{k}={v}")))
}

fn masked_string(old: &EditValue) -> EditValue {
    let mut v = EditValue::from(RAW_SECRET_MASK);
    *v.decor_mut() = old.decor().clone();
    v
}

// ── Masking ─────────────────────────────────────────────────────────────────

fn mask_value(v: &mut EditValue, key: &str, path: &str, force: bool, masked: &mut Vec<String>) {
    let secret = force || is_secret_key(key);
    match v {
        EditValue::String(s) => {
            let text = s.value().clone();
            if !text.is_empty() && (secret || url_has_password(&text) || url_has_secret_query(&text)) {
                *v = masked_string(v);
                masked.push(path.to_string());
            }
        }
        EditValue::Array(arr) => {
            for (i, e) in arr.iter_mut().enumerate() {
                mask_value(e, key, &format!("{path}[{i}]"), secret, masked);
            }
        }
        EditValue::InlineTable(t) => {
            let container = secret || SECRET_CONTAINERS.contains(&key);
            for (k, e) in t.iter_mut() {
                let k = k.get().to_string();
                mask_value(e, &k, &join(path, &k), container, masked);
            }
        }
        _ => {}
    }
}

fn mask_table(t: &mut EditTable, path: &str, force: bool, masked: &mut Vec<String>) {
    for (k, item) in t.iter_mut() {
        let k = k.get().to_string();
        mask_item(item, &k, &join(path, &k), force, masked);
    }
}

fn mask_item(item: &mut Item, key: &str, path: &str, force: bool, masked: &mut Vec<String>) {
    // Table names are not judged by `is_secret_key` (`[secret_manager]`
    // would otherwise hide its addresses); each key inside is judged itself.
    let container = force || SECRET_CONTAINERS.contains(&key);
    match item {
        Item::Value(v) => mask_value(v, key, path, force, masked),
        Item::Table(t) => mask_table(t, path, container, masked),
        Item::ArrayOfTables(a) => {
            for (i, t) in a.iter_mut().enumerate() {
                let seg = table_identity(t).unwrap_or_else(|| i.to_string());
                mask_table(t, &format!("{path}[{seg}]"), container, masked);
            }
        }
        Item::None => {}
    }
}

/// Mask `text` for display. Returns the masked text and the masked paths.
pub(crate) fn mask_raw_toml(text: &str) -> Result<(String, Vec<String>, Vec<String>), String> {
    let mut doc: DocumentMut = text.parse().map_err(|e: toml_edit::TomlError| parse_error(text, &e))?;
    let mut hidden = Vec::new();
    for s in HIDDEN_SECTIONS {
        if doc.remove(s).is_some() {
            hidden.push((*s).to_string());
        }
    }
    let mut masked = Vec::new();
    mask_table(doc.as_table_mut(), "", false, &mut masked);
    Ok((doc.to_string(), masked, hidden))
}

// ── Restore ─────────────────────────────────────────────────────────────────

/// Keys naming where a secret in the same table is sent. A kept («set»)
/// secret is refused when one of these changed in the same edit — otherwise
/// the stored secret would go to a host the editor just chose.
pub(crate) fn is_destination_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    matches!(k.as_str(), "url" | "host" | "addr" | "base_url" | "endpoint" | "server" | "api_base" | "hostname")
        || k.ends_with("_url")
        || k.ends_with("_addr")
        || k.ends_with("_host")
        || k.ends_with("_endpoint")
}

/// What a restore pass learned: every secret value it put back (so error
/// messages can be scrubbed of them).
#[derive(Default)]
pub(crate) struct RestoreCtx {
    pub restored: Vec<String>,
}

impl RestoreCtx {
    /// Replace every restored secret in `msg` with the mask.
    pub(crate) fn scrub(&self, msg: &str) -> String {
        let mut out = msg.to_string();
        for s in &self.restored {
            if s.len() >= 4 {
                out = out.replace(s.as_str(), RAW_SECRET_MASK);
            }
        }
        out
    }
}

fn plain(item: Option<&Item>) -> Option<String> {
    item.and_then(|i| i.as_value()).map(|v| {
        let mut v = v.clone();
        v.decor_mut().clear();
        v.to_string()
    })
}

/// The first destination key whose value differs between the new and the
/// stored table (`None` when every destination key is unchanged).
fn changed_destination(new: &dyn toml_edit::TableLike, old: Option<&dyn toml_edit::TableLike>) -> Option<String> {
    let mut keys: Vec<String> = new.iter().map(|(k, _)| k.to_string()).collect();
    if let Some(o) = old {
        keys.extend(o.iter().map(|(k, _)| k.to_string()));
    }
    keys.into_iter().filter(|k| is_destination_key(k)).find(|k| {
        // A masked destination (a URL carrying a password) is itself being
        // restored, not changed.
        if new.get(k).and_then(|i| i.as_str()) == Some(RAW_SECRET_MASK) {
            return false;
        }
        let n = plain(new.get(k));
        let o = plain(old.and_then(|o| o.get(k)));
        // Clearing the destination sends nothing anywhere.
        n.is_some() && n != o
    })
}

fn restore_value(
    v: &mut EditValue,
    old: Option<&Item>,
    path: &str,
    dest_changed: Option<&str>,
    ctx: &mut RestoreCtx,
) -> Result<(), String> {
    match v {
        EditValue::String(s) if s.value() == RAW_SECRET_MASK => {
            if let Some(dest) = dest_changed {
                return Err(format!(
                    "`{path}` keeps its stored secret («set») but `{dest}` changed in the same edit — re-enter the secret for the new destination"
                ));
            }
            match old.and_then(|o| o.as_value()) {
                Some(prev @ EditValue::String(ps)) => {
                    ctx.restored.push(ps.value().clone());
                    let decor = v.decor().clone();
                    *v = prev.clone();
                    *v.decor_mut() = decor;
                    Ok(())
                }
                _ => Err(format!(
                    "`{path}` is «set» but nothing is stored there to keep — type the real value"
                )),
            }
        }
        EditValue::Array(arr) => {
            let old_arr = old.and_then(|o| o.as_array());
            for (i, e) in arr.iter_mut().enumerate() {
                // Tables inside an inline array have no identity: never
                // restore a secret into one by position.
                let prev = if matches!(e, EditValue::InlineTable(_)) {
                    None
                } else {
                    old_arr.and_then(|a| a.get(i)).map(|x| Item::Value(x.clone()))
                };
                restore_value(e, prev.as_ref(), &format!("{path}[{i}]"), dest_changed, ctx)?;
            }
            Ok(())
        }
        EditValue::InlineTable(t) => {
            let old_t = old.and_then(|o| o.as_table_like());
            let own = changed_destination(t, old_t);
            let dest = dest_changed.map(str::to_string).or(own);
            for (k, e) in t.iter_mut() {
                let k = k.get().to_string();
                restore_value(e, old_t.and_then(|ot| ot.get(&k)), &join(path, &k), dest.as_deref(), ctx)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn restore_table(
    t: &mut EditTable,
    old: Option<&dyn toml_edit::TableLike>,
    path: &str,
    dest_changed: Option<&str>,
    ctx: &mut RestoreCtx,
) -> Result<(), String> {
    let own = changed_destination(t, old);
    let dest = dest_changed.map(str::to_string).or(own);
    for (k, item) in t.iter_mut() {
        let k = k.get().to_string();
        restore_item(item, old.and_then(|o| o.get(&k)), &join(path, &k), dest.as_deref(), ctx)?;
    }
    Ok(())
}

fn restore_item(
    item: &mut Item,
    old: Option<&Item>,
    path: &str,
    dest_changed: Option<&str>,
    ctx: &mut RestoreCtx,
) -> Result<(), String> {
    match item {
        Item::Value(v) => restore_value(v, old, path, dest_changed, ctx),
        Item::Table(t) => restore_table(t, old.and_then(|o| o.as_table_like()), path, dest_changed, ctx),
        Item::ArrayOfTables(a) => {
            let old_arr = old.and_then(|o| o.as_array_of_tables());
            let mut seen = std::collections::HashSet::new();
            for t in a.iter() {
                if let Some(id) = table_identity(t)
                    && !seen.insert(id.clone())
                {
                    return Err(format!("`{path}` has two entries with {id}"));
                }
            }
            for (i, t) in a.iter_mut().enumerate() {
                let ident = table_identity(t);
                // Entries are matched by id/name only — never by position.
                let prev = match (&ident, old_arr) {
                    (Some(id), Some(oa)) => {
                        let mut hits = oa.iter().filter(|ot| table_identity(*ot).as_deref() == Some(id.as_str()));
                        let first = hits.next();
                        if hits.next().is_some() {
                            return Err(format!("the stored `{path}` has two entries with {id}; fix it by re-entering their secrets"));
                        }
                        first
                    }
                    _ => None,
                };
                let seg = ident.clone().unwrap_or_else(|| i.to_string());
                restore_table(t, prev.map(|p| p as &dyn toml_edit::TableLike), &format!("{path}[{seg}]"), dest_changed, ctx)
                    .map_err(|e| {
                        if ident.is_none() && e.contains("nothing is stored") {
                            format!("`{path}[{i}]` has no `id` or `name`, so a «set» value cannot be matched to a stored one — type the real value")
                        } else {
                            e
                        }
                    })?;
            }
            Ok(())
        }
        Item::None => Ok(()),
    }
}

/// Turn the submitted (masked) text back into the text to write: placeholders
/// take their stored values and hidden sections come back from disk.
#[cfg(test)]
pub(crate) fn restore_raw_toml(submitted: &str, stored: &str) -> Result<String, String> {
    restore_raw_toml_ctx(submitted, stored, &mut RestoreCtx::default())
}

pub(crate) fn restore_raw_toml_ctx(submitted: &str, stored: &str, ctx: &mut RestoreCtx) -> Result<String, String> {
    let mut doc: DocumentMut = submitted.parse().map_err(|e: toml_edit::TomlError| parse_error(submitted, &e))?;
    let old: DocumentMut = if stored.trim().is_empty() {
        DocumentMut::new()
    } else {
        stored.parse().map_err(|e: toml_edit::TomlError| {
            // Line/column only: the stored text holds real secrets.
            let p = parse_error(stored, &e);
            format!("the stored file does not parse ({p}) — fix it from the terminal")
        })?
    };
    for s in HIDDEN_SECTIONS {
        if doc.get(s).is_some() {
            return Err(format!("[{s}] is managed on its own page and cannot be edited here"));
        }
    }
    restore_table(doc.as_table_mut(), Some(old.as_table() as &dyn toml_edit::TableLike), "", None, ctx)?;
    for s in HIDDEN_SECTIONS {
        if let Some(item) = old.get(s) {
            doc.insert(s, item.clone());
        }
    }
    Ok(doc.to_string())
}

/// `TOML parse error at line L, column C: …`.
fn parse_error(text: &str, e: &toml_edit::TomlError) -> String {
    let (line, col) = e
        .span()
        .map(|span| {
            let before = &text[..span.start.min(text.len())];
            let line = before.matches('\n').count() + 1;
            let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
            (line, col)
        })
        .unwrap_or((0, 0));
    format!("TOML parse error at line {line}, column {col}: {}", e.message())
}

/// An agent's `[permissions]` edited here is an operator decision: add the
/// v1.68 marker so the boot migration never resets it.
fn mark_permissions_if_edited(new_text: &str, stored: &str) -> Result<String, String> {
    let mut doc: DocumentMut = new_text.parse().map_err(|e: toml_edit::TomlError| parse_error(new_text, &e))?;
    let old_perms = stored
        .parse::<toml::Table>()
        .ok()
        .and_then(|t| t.get("permissions").cloned());
    let new_perms = new_text.parse::<toml::Table>().ok().and_then(|t| t.get("permissions").cloned());
    let key = super::agents_update_v168::PERMISSIONS_MARKER_KEY;
    if new_perms.is_some() && new_perms != old_perms {
        if let Some(p) = doc.get_mut("permissions").and_then(|i| i.as_table_like_mut())
            && !p.contains_key(key)
        {
            p.insert(key, toml_edit::value(super::agents_update_v168::PERMISSIONS_MARKER_VALUE));
        }
    }
    Ok(doc.to_string())
}

// ── Typed validation ────────────────────────────────────────────────────────

/// Validate a `config.toml` with the section validators the gateway itself
/// applies when it reads each section.
pub(crate) fn validate_config_table(table: &toml::Table) -> Result<(), String> {
    crate::task_sandbox::settings::parse(table).map_err(|e| format!("[container.sandbox]: {e}"))?;
    if table.contains_key("computer_use") {
        crate::computer_use_image::parse(table).map_err(|e| format!("[computer_use]: {e}"))?;
    }
    if let Some(tick) = table.get("tick") {
        let tick = tick.as_table().ok_or("[tick] must be a table")?;
        if let Some(sources) = tick.get("sources") {
            let arr = sources.as_array().ok_or("[[tick.sources]] must be an array of tables")?;
            let mut ids = std::collections::HashSet::new();
            for entry in arr {
                let entry = entry.as_table().ok_or("[[tick.sources]] entries must be tables")?;
                let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("?");
                if !ids.insert(id.to_string()) {
                    return Err(format!("[[tick.sources]] duplicate id `{id}`"));
                }
                super::tick_sources_rpc::validate_source_entry(entry, tick)
                    .map_err(|e| format!("[[tick.sources]] `{id}`: {e}"))?;
            }
        }
    }
    if let Some(team) = table.get("team") {
        team.clone()
            .try_into::<duduclaw_core::types::TeamConfig>()
            .map_err(|e| format!("[team]: {}", e.message()))?;
    }
    if let Some(t) = table.get("takeover") {
        t.clone()
            .try_into::<duduclaw_core::takeover_state::TakeoverConfig>()
            .map_err(|e| format!("[takeover]: {}", e.message()))?;
    }
    if let Some(r) = table.get("redaction") {
        r.clone()
            .try_into::<duduclaw_redaction::RedactionConfig>()
            .map_err(|e| format!("[redaction]: {}", e.message()))?;
    }
    if let Some(sm) = table.get("secret_manager") {
        sm.clone()
            .try_into::<duduclaw_security::secret_manager::SecretManagerConfig>()
            .map_err(|e| format!("[secret_manager]: {}", e.message()))?;
    }
    let db = duduclaw_db::parse_db_sources(table);
    if let Some(err) = db.errors.first() {
        return Err(format!("[db_sources.{}]: {}", err.name, err.message));
    }
    Ok(())
}

/// ` (line L, column C)` for a toml deserialisation error, or "".
fn de_location(text: &str, e: &toml::de::Error) -> String {
    e.span()
        .map(|span| {
            let before = &text[..span.start.min(text.len())];
            format!(
                " (line {}, column {})",
                before.matches('\n').count() + 1,
                before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1
            )
        })
        .unwrap_or_default()
}

/// Validate `text` as the given file kind. Returns the parsed table.
pub(crate) fn validate_raw(file: &RawFile, text: &str) -> Result<toml::Table, String> {
    let table: toml::Table = text.parse().map_err(|e: toml::de::Error| {
        let (line, col) = e
            .span()
            .map(|span| {
                let before = &text[..span.start.min(text.len())];
                (
                    before.matches('\n').count() + 1,
                    before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1,
                )
            })
            .unwrap_or((0, 0));
        format!("TOML parse error at line {line}, column {col}: {}", e.message())
    })?;
    match file {
        RawFile::Config => {
            super::config_schema::check_config_types(text, &table)?;
            validate_config_table(&table)?;
        }
        RawFile::Inference => {
            let cfg: duduclaw_inference::config::InferenceConfig = toml::Value::Table(table.clone())
                .try_into()
                .map_err(|e| format!("inference.toml does not match the inference config: {}", e.message()))?;
            cfg.validate().map_err(|e| e.to_string())?;
        }
        RawFile::Agent(_) => {
            // `e.message()` + location only: the error's Display quotes the
            // offending source line, which may hold a restored secret.
            toml::from_str::<duduclaw_core::types::AgentConfig>(text).map_err(|e| {
                format!(
                    "agent.toml does not match the employee config{}: {}",
                    de_location(text, &e),
                    e.message()
                )
            })?;
        }
    }
    Ok(table)
}

/// Keys that name a command, a binary or an outbound path. Changing any of
/// them is audited as `config_protected_key_changed`; the two operator-only
/// executables are refused here outright (terminal only).
const RAW_REFUSED_KEYS: &[&str] = &["dispatch.judge_command", "evolution.eval_binary"];
const RAW_CONFIG_SENSITIVE_KEYS: &[&str] = &[
    "container.sandbox.when_unavailable",
    "container.sandbox.script_when_unavailable",
    "tick.allow_command_sources",
    "voice.stt_command",
];
const RAW_INFERENCE_SENSITIVE_KEYS: &[&str] = &[
    "llamafile.dir",
    "llamafile.default_file",
    "llamafile.extra_args",
    "llamafile.host",
    "router.ucci_observations",
];

/// `(key, before, after)` for every sensitive key that changed; `Err` for a
/// change to a key the raw editor may not touch.
pub(crate) fn sensitive_key_changes(
    file: &RawFile,
    before: &toml::Table,
    after: &toml::Table,
) -> Result<Vec<(String, Value, Value)>, String> {
    use super::config_commit::toml_at_json;
    let mut out = Vec::new();
    let mut check = |key: &str, out: &mut Vec<(String, Value, Value)>| {
        let (b, a) = (toml_at_json(before, key), toml_at_json(after, key));
        if b != a {
            out.push((key.to_string(), b, a));
        }
    };
    match file {
        RawFile::Config => {
            for key in RAW_REFUSED_KEYS {
                if toml_at_json(before, key) != toml_at_json(after, key) {
                    return Err(format!(
                        "`{key}` names an executable and can only be changed from the operator terminal (edit config.toml directly)"
                    ));
                }
            }
            for key in RAW_CONFIG_SENSITIVE_KEYS {
                check(key, &mut out);
            }
            // Tick `command` argv, matched by source id.
            let commands = |t: &toml::Table| -> std::collections::BTreeMap<String, Value> {
                super::config_commit::toml_at(t, "tick.sources")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|e| e.as_table())
                            .filter_map(|e| {
                                let id = e.get("id")?.as_str()?.to_string();
                                let cmd = e.get("command").and_then(|c| serde_json::to_value(c).ok())?;
                                Some((id, cmd))
                            })
                            .collect()
                    })
                    .unwrap_or_default()
            };
            let (bc, ac) = (commands(before), commands(after));
            let mut ids: Vec<&String> = bc.keys().chain(ac.keys()).collect();
            ids.sort();
            ids.dedup();
            for id in ids {
                let (b, a) = (bc.get(id).cloned().unwrap_or(Value::Null), ac.get(id).cloned().unwrap_or(Value::Null));
                if b != a {
                    out.push((format!("tick.sources[{id}].command"), b, a));
                }
            }
        }
        RawFile::Inference => {
            for key in RAW_INFERENCE_SENSITIVE_KEYS {
                check(key, &mut out);
            }
        }
        RawFile::Agent(_) => {}
    }
    Ok(out)
}

/// Top-level sections whose content differs.
pub(crate) fn changed_sections(before: &toml::Table, after: &toml::Table) -> Vec<String> {
    let mut keys: Vec<&String> = before.keys().chain(after.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter().filter(|k| before.get(*k) != after.get(*k)).cloned().collect()
}

fn key_changed(before: &toml::Table, after: &toml::Table, path: &str) -> bool {
    super::config_commit::toml_at(before, path) != super::config_commit::toml_at(after, path)
}

/// Protected sections/keys per file kind.
pub(crate) fn protected_changed(file: &RawFile, before: &toml::Table, after: &toml::Table, sections: &[String]) -> bool {
    match file {
        RawFile::Config => sections.iter().any(|s| s == "delegation" || s == "acp"),
        RawFile::Inference => false,
        RawFile::Agent(_) => {
            sections.iter().any(|s| s == "agent" || s == "capabilities")
                || ["container.sandbox_enabled", "container.network_access", "permissions.can_modify_own_soul"]
                    .iter()
                    .any(|p| key_changed(before, after, p))
        }
    }
}

impl MethodHandler {
    /// Resolve `file` to a path. `agent:<id>` must name an existing employee
    /// directory directly under `<home>/agents` (no symlinks, no escape).
    pub(crate) fn resolve_raw_file(&self, file: &str) -> Result<(RawFile, PathBuf), String> {
        match file {
            "config" => Ok((RawFile::Config, self.home_dir.join("config.toml"))),
            "inference" => Ok((RawFile::Inference, self.home_dir.join("inference.toml"))),
            other => {
                let Some(id) = other.strip_prefix("agent:") else {
                    return Err("file must be \"config\", \"inference\" or \"agent:<id>\"".into());
                };
                if !duduclaw_core::is_valid_agent_id(id) || id.starts_with(['.', '_']) {
                    return Err(format!("invalid agent id `{id}`"));
                }
                let agents = self.home_dir.join("agents");
                let dir = agents.join(id);
                let path = dir.join("agent.toml");
                for p in [&dir, &path] {
                    match std::fs::symlink_metadata(p) {
                        Ok(m) if m.file_type().is_symlink() => {
                            return Err(format!("agent `{id}` is a symlink and cannot be edited here"));
                        }
                        Ok(_) => {}
                        Err(_) => return Err(format!("agent `{id}` not found")),
                    }
                }
                let canon_agents = std::fs::canonicalize(&agents).map_err(|_| "agents directory not found".to_string())?;
                let canon = std::fs::canonicalize(&path).map_err(|_| format!("agent `{id}` not found"))?;
                if canon.parent() != Some(canon_agents.join(id).as_path()) {
                    return Err(format!("agent `{id}` resolves outside the agents directory"));
                }
                Ok((RawFile::Agent(id.to_string()), path))
            }
        }
    }

    /// `config.raw.get { file }` → `{ file, exists, content, masked[], hidden_sections[], hash }`.
    pub(crate) async fn handle_config_raw_get(&self, params: Value) -> WsFrame {
        let file = params.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let (_kind, path) = match self.resolve_raw_file(file) {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let text = match read_text_or_empty(&path) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let (content, masked, hidden) = match mask_raw_toml(&text) {
            Ok(r) => r,
            Err(e) => {
                // Never fall back to the unmasked text.
                return WsFrame::error_response("", &format!("{file} does not parse, cannot display it safely: {e}"));
            }
        };
        WsFrame::ok_response(
            "",
            json!({
                "file": file,
                "exists": path.exists(),
                "content": content,
                "masked": masked,
                "hidden_sections": hidden,
                // Send back as `base_hash` so a concurrent edit is detected.
                "hash": content_hash(&text),
            }),
        )
    }

    /// `config.raw.set { file, content, base_hash? }`.
    pub(crate) async fn handle_config_raw_set(&self, params: Value, ctx: &UserContext) -> WsFrame {
        match self.config_raw_set_inner(&params, ctx).await {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    async fn config_raw_set_inner(&self, params: &Value, ctx: &UserContext) -> Result<Value, String> {
        let file = params.get("file").and_then(|v| v.as_str()).unwrap_or("");
        let submitted = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("`content` (the TOML text) is required")?;
        if submitted.len() > MAX_RAW_BYTES {
            return Err(format!("content is larger than {MAX_RAW_BYTES} bytes"));
        }
        let (kind, path) = self.resolve_raw_file(file)?;
        let stored = read_text_or_empty(&path)?;
        let stored_hash = content_hash(&stored);
        // `base_hash` (from `config.raw.get`) is required: an edit made
        // against a file that changed since it was opened is refused.
        let base = params
            .get("base_hash")
            .and_then(|v| v.as_str())
            .ok_or("`base_hash` (the `hash` returned by config.raw.get) is required")?;
        if base != stored_hash {
            return Err(format!("{file} changed since it was opened — reload and try again"));
        }
        let mut rctx = RestoreCtx::default();
        let mut new_text = restore_raw_toml_ctx(submitted, &stored, &mut rctx)?;
        if matches!(kind, RawFile::Agent(_)) {
            new_text = mark_permissions_if_edited(&new_text, &stored).map_err(|e| rctx.scrub(&e))?;
        }
        // Every validation message is scrubbed of restored secrets.
        let after = validate_raw(&kind, &new_text).map_err(|e| rctx.scrub(&e))?;
        let before: toml::Table = stored.parse().unwrap_or_default();
        let sensitive = sensitive_key_changes(&kind, &before, &after)?;
        if let RawFile::Agent(id) = &kind {
            let name_before = super::config_commit::toml_at(&before, "agent.name");
            if name_before.is_some() && name_before != super::config_commit::toml_at(&after, "agent.name") {
                return Err(format!("[agent] name cannot be changed here (it is the employee id `{id}`)"));
            }
        }
        let sections = changed_sections(&before, &after);
        // A submission whose parsed content equals the stored file is a true
        // no-op (the hidden [mcp_keys] re-attached at the end, or a pure
        // whitespace/comment difference, does not count): no write, no
        // backup, no audit row, the same hash back.
        if sections.is_empty() && !stored.trim().is_empty() {
            return Ok(json!({
                "success": true,
                "unchanged": true,
                "changed_sections": [],
                "protected_changed": false,
                "restart_required": [],
                "hot_reloaded": [],
                "backup": null,
                "hash": stored_hash,
            }));
        }
        let protected = protected_changed(&kind, &before, &after, &sections);

        let backup = if stored.is_empty() {
            None
        } else {
            Some(write_backup(&path, &stored, chrono::Utc::now().timestamp())?)
        };
        {
            let (p, h, t) = (path.clone(), stored_hash.clone(), new_text.clone());
            tokio::task::spawn_blocking(move || commit_text_locked(&p, &h, &t))
                .await
                .map_err(|e| format!("write task failed: {e}"))??;
        }

        let backup_name = backup
            .as_ref()
            .and_then(|b| b.file_name())
            .and_then(|n| n.to_str())
            .map(str::to_string);
        crate::security_autopilot::audit_and_emit(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "config_raw_edited",
                match &kind {
                    RawFile::Agent(id) => id.as_str(),
                    _ => ctx.user_id.as_str(),
                },
                if protected {
                    duduclaw_security::audit::Severity::Warning
                } else {
                    duduclaw_security::audit::Severity::Info
                },
                json!({
                    "file": kind.kind(),
                    "agent_id": match &kind { RawFile::Agent(id) => Value::String(id.clone()), _ => Value::Null },
                    "changed_sections": sections,
                    "protected_changed": protected,
                    "backup": backup_name,
                    "user_id": ctx.user_id,
                }),
            ),
        );
        for (key, b, a) in &sensitive {
            crate::security_autopilot::audit_and_emit(
                &self.home_dir,
                &duduclaw_security::audit::AuditEvent::new(
                    "config_protected_key_changed",
                    ctx.user_id.as_str(),
                    duduclaw_security::audit::Severity::Warning,
                    json!({ "key": key, "before": b, "after": a, "file": kind.kind(), "user_id": ctx.user_id, "source": "config.raw.set" }),
                ),
            );
        }
        crate::security_autopilot::emit_config_changed();

        let (hot_reloaded, restart_required) = self.apply_raw_reloads(&kind, &before, &after, &sections).await;
        info!(file = kind.kind(), ?sections, ?restart_required, "config.raw.set completed");
        Ok(json!({
            "success": true,
            "unchanged": false,
            "hash": content_hash(&new_text),
            "changed_sections": sections,
            "protected_changed": protected,
            "backup": backup_name,
            "hot_reloaded": hot_reloaded,
            "restart_required": restart_required,
        }))
    }

    /// Apply what can be applied live after a raw write; report the rest.
    async fn apply_raw_reloads(
        &self,
        kind: &RawFile,
        before: &toml::Table,
        after: &toml::Table,
        sections: &[String],
    ) -> (Vec<String>, Vec<String>) {
        let mut hot: Vec<String> = Vec::new();
        let mut restart: Vec<String> = Vec::new();
        let has = |s: &str| sections.iter().any(|x| x == s);
        match kind {
            RawFile::Config => {
                for s in CONFIG_BOOT_SECTIONS {
                    if has(s) {
                        restart.push((*s).to_string());
                    }
                }
                if key_changed(before, after, "general.name") {
                    restart.push("general.name".into());
                }
                if key_changed(before, after, "general.log_level") {
                    let level = super::config_commit::toml_at(after, "general.log_level").and_then(|v| v.as_str());
                    match level.map(crate::log::apply_log_level) {
                        Some(Ok(crate::log::LogLevelApply::Applied)) => hot.push("general.log_level".into()),
                        _ => restart.push("general.log_level".into()),
                    }
                }
                if key_changed(before, after, "rotation.health_check_interval_seconds") {
                    restart.push("rotation.health_check_interval_seconds".into());
                }
                if key_changed(before, after, "goal_loop.resume_on_restart") {
                    restart.push("goal_loop.resume_on_restart".into());
                }
                if has("rotation") || has("accounts") || has("account_loading") || has("api") {
                    crate::claude_runner::invalidate_rotator_cache().await;
                    hot.push("rotation".into());
                }
                if has("dispatch") || has("task_forward_model") {
                    self.respawn_dispatch_engine().await;
                    self.respawn_goal_loop_driver().await;
                    hot.push("dispatch".into());
                } else if has("goal_loop") {
                    self.respawn_goal_loop_driver().await;
                    hot.push("goal_loop".into());
                }
                if has("topology_evolution") {
                    self.respawn_topology_driver().await;
                    hot.push("topology_evolution".into());
                }
                if has("tick") {
                    if self.respawn_tick_sources().await.is_some() {
                        hot.push("tick".into());
                    } else {
                        restart.push("tick".into());
                    }
                }
                if has("redaction") {
                    let (applied, _) = self.apply_redaction_hot_reload(after).await;
                    if applied {
                        hot.push("redaction".into());
                    } else {
                        restart.push("redaction".into());
                    }
                }
            }
            RawFile::Inference => {
                crate::claude_runner::reset_inference_engine().await;
                hot.push("inference".into());
            }
            RawFile::Agent(id) => {
                if has("agent") {
                    self.sync_org_after_raw_edit(id, before, after);
                }
                let rescanned = self.registry.write().await.scan().await.is_ok();
                if rescanned {
                    hot.push("registry".into());
                } else {
                    restart.push("registry".into());
                }
                if key_changed(before, after, "heartbeat.max_concurrent_runs") {
                    restart.push("heartbeat.max_concurrent_runs".into());
                }
                if has("channels") {
                    let changed: Vec<&str> = ["discord", "telegram", "slack"]
                        .into_iter()
                        .filter(|c| key_changed(before, after, &format!("channels.{c}")))
                        .collect();
                    if !changed.is_empty() {
                        self.hot_restart_agent_channels(&changed, id).await;
                        hot.push("channels".into());
                    }
                }
                if rescanned && (has("os_watch") || has("capabilities")) {
                    self.hot_reload_os_watcher(id).await;
                }
            }
        }
        restart.sort();
        restart.dedup();
        (hot, restart)
    }

    /// A raw edit of `[agent] reports_to` / `department` moves the employee in
    /// the org authority too (same rule as `agents.update`: only when changed).
    fn sync_org_after_raw_edit(&self, id: &str, before: &toml::Table, after: &toml::Table) {
        if !key_changed(before, after, "agent.reports_to") && !key_changed(before, after, "agent.department") {
            return;
        }
        let get = |p: &str| {
            super::config_commit::toml_at(after, p)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let entry = duduclaw_core::OrgEntry::new(get("agent.reports_to"), get("agent.department"));
        if let Err(e) = duduclaw_core::org_store::upsert(&self.home_dir, id, entry) {
            warn!(agent = %id, error = %e, "org.toml upsert failed after config.raw.set");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"# operator comment
[gateway]
auth_token_enc = "c2VjcmV0"
bind = "127.0.0.1"

[webchat]
widget_key = "0123456789abcdef"

[db_sources.crm]
url = "postgres://app:hunter2@db.local/crm"

[[accounts]]
id = "a1"
api_key_enc = "AAAA"

[[accounts]]
id = "a2"
api_key_enc = "BBBB"

[[tick.sources]]
id = "feed"
kind = "http_poll"
url = "https://example.com/x"
headers = { Authorization = "Bearer xyz" }

[mcp_keys."ddc_live_secret"]
client_id = "x"
"#;

    #[test]
    fn mask_hides_every_secret_and_keeps_comments() {
        let (masked, paths, hidden) = mask_raw_toml(SAMPLE).unwrap();
        for secret in ["c2VjcmV0", "0123456789abcdef", "hunter2", "AAAA", "BBBB", "Bearer xyz", "ddc_live_secret"] {
            assert!(!masked.contains(secret), "{secret} leaked:\n{masked}");
        }
        assert!(masked.contains("# operator comment"));
        assert!(masked.contains("bind = \"127.0.0.1\""));
        assert_eq!(hidden, vec!["mcp_keys".to_string()]);
        assert!(paths.contains(&"accounts[id=a2].api_key_enc".to_string()), "{paths:?}");
    }

    #[test]
    fn round_trip_restores_secrets_even_when_entries_are_reordered() {
        let (masked, _, _) = mask_raw_toml(SAMPLE).unwrap();
        // Swap the two [[accounts]] blocks and change bind.
        let a1 = "[[accounts]]\nid = \"a1\"\napi_key_enc = \"«set»\"\n";
        let a2 = "[[accounts]]\nid = \"a2\"\napi_key_enc = \"«set»\"\n";
        let edited = masked
            .replace(a1, "TMP_A1")
            .replace(a2, a1)
            .replace("TMP_A1", a2)
            .replace("127.0.0.1", "0.0.0.0");
        let restored = restore_raw_toml(&edited, SAMPLE).unwrap();
        let t: toml::Table = restored.parse().unwrap();
        let accounts = t["accounts"].as_array().unwrap();
        assert_eq!(accounts[0]["id"].as_str(), Some("a2"));
        assert_eq!(accounts[0]["api_key_enc"].as_str(), Some("BBBB"));
        assert_eq!(accounts[1]["api_key_enc"].as_str(), Some("AAAA"));
        assert_eq!(t["gateway"]["bind"].as_str(), Some("0.0.0.0"));
        assert_eq!(t["gateway"]["auth_token_enc"].as_str(), Some("c2VjcmV0"));
        assert_eq!(t["db_sources"]["crm"]["url"].as_str(), Some("postgres://app:hunter2@db.local/crm"));
        assert!(t["mcp_keys"].as_table().unwrap().contains_key("ddc_live_secret"));
        assert!(restored.contains("# operator comment"));
    }

    #[test]
    fn placeholder_without_stored_value_is_refused() {
        let err = restore_raw_toml("[gateway]\nnew_token = \"«set»\"\n", SAMPLE).unwrap_err();
        assert!(err.contains("gateway.new_token"), "{err}");
    }

    #[test]
    fn hidden_section_cannot_be_submitted() {
        assert!(restore_raw_toml("[mcp_keys.\"x\"]\nclient_id = \"y\"\n", SAMPLE).is_err());
    }

    #[test]
    fn parse_errors_carry_line_and_column() {
        let err = mask_raw_toml("a = 1\nb = = 2\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
        let err = validate_raw(&RawFile::Config, "a = 1\nb = \n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }

    #[test]
    fn config_validation_uses_the_section_validators() {
        assert!(validate_raw(&RawFile::Config, "[container.sandbox]\nbogus = 1\n").is_err());
        assert!(validate_raw(&RawFile::Config, "[[tick.sources]]\nid = \"x\"\nkind = \"command\"\ncommand = [\"/bin/true\"]\n").is_err());
        assert!(validate_raw(&RawFile::Config, "[takeover]\nduration_minutes = \"long\"\n").is_err());
        assert!(validate_raw(&RawFile::Config, "[general]\nname = \"box\"\n").is_ok());
    }

    #[test]
    fn inference_validation_is_typed() {
        assert!(validate_raw(&RawFile::Inference, "enabled = \"yes\"\n").is_err());
        assert!(validate_raw(&RawFile::Inference, "enabled = true\n[router]\nenabled = true\nfast_threshold = 0.2\nstrong_threshold = 0.5\n").is_err());
        assert!(validate_raw(&RawFile::Inference, "enabled = false\n[embedding]\nanything = 1\n").is_ok(), "unknown sections stay tolerated");
    }

    #[test]
    fn secret_key_detection() {
        for k in ["bot_token_enc", "api_key", "app_secret", "widget_key", "encoding_aes_key", "key", "oauth_token"] {
            assert!(is_secret_key(k), "{k}");
        }
        for k in ["bind", "default_agent", "allowed_origins", "url", "id"] {
            assert!(!is_secret_key(k), "{k}");
        }
        assert!(url_has_password("postgres://u:p@h/db"));
        assert!(!url_has_password("https://user@h/x"));
        assert!(!url_has_password("https://h/x?a=b@c"));
    }

    #[test]
    fn changed_and_protected_sections() {
        let b: toml::Table = toml::from_str("[acp]\ntrusted = false\n[general]\nname = \"a\"\n").unwrap();
        let a: toml::Table = toml::from_str("[acp]\ntrusted = true\n[general]\nname = \"a\"\n").unwrap();
        let s = changed_sections(&b, &a);
        assert_eq!(s, vec!["acp".to_string()]);
        assert!(protected_changed(&RawFile::Config, &b, &a, &s));
    }
}
