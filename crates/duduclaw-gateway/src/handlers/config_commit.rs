//! v1.68.0 — shared plumbing for the dashboard config writers
//! (`system.update_config`, `tick.sources.*`, `config.raw.*`).
//!
//! Three things live here:
//!
//! 1. **Contract parameter lookup.** The v1.68 RPC contract names every new
//!    parameter after its TOML path (`takeover.enabled`). A caller may send it
//!    nested (`{"takeover": {"enabled": true}}`) or flat
//!    (`{"takeover.enabled": true}`); [`param_at`] accepts both, nested first.
//!    The typed getters reject a present-but-wrong-typed value instead of
//!    silently ignoring it (the pre-1.68 writers ignored it, which is how a
//!    save could "succeed" while writing nothing).
//! 2. **Locked commit.** `config.toml` is also written by the CLI
//!    (`duduclaw onboard`, `mcp_internal_key`), so a dashboard save takes the
//!    same `with_file_lock` advisory lock and refuses to write when the file
//!    changed on disk since it was read (optimistic concurrency): the caller
//!    gets "changed on disk, reload and retry" instead of silently undoing the
//!    other writer's change.
//! 3. **Backups** for the raw editor: `<file>.bak-<unix ts>`, last five kept.

#[allow(unused_imports)]
use super::*;

use sha2::{Digest, Sha256};

/// The literal the raw editor and the v1.68 secret fields show in place of a
/// stored secret. Sending it back unchanged means "keep the stored value".
pub(crate) const RAW_SECRET_MASK: &str = "«set»";

/// Placeholders older dashboard code echoes back for an untouched secret.
pub(crate) fn is_secret_placeholder(v: &str) -> bool {
    v == RAW_SECRET_MASK || v == SECRET_MASK_SET || (!v.is_empty() && v.chars().all(|c| c == '*'))
}

// ── Parameter lookup ────────────────────────────────────────────────────────

/// Find the contract parameter at dotted `path`: nested objects first, then a
/// flat key spelled exactly `path`. `None` when absent (or JSON `null`, which
/// the dashboard uses for "not sent").
pub(crate) fn param_at<'a>(params: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = params;
    let mut nested_ok = true;
    for seg in path.split('.') {
        match cur.get(seg) {
            Some(v) => cur = v,
            None => {
                nested_ok = false;
                break;
            }
        }
    }
    let found = if nested_ok { Some(cur) } else { params.get(path) };
    found.filter(|v| !v.is_null())
}

pub(crate) fn bool_param(params: &Value, path: &str) -> Result<Option<bool>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(format!("{path} must be a boolean")),
    }
}

pub(crate) fn str_param<'a>(params: &'a Value, path: &str) -> Result<Option<&'a str>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.as_str())),
        Some(_) => Err(format!("{path} must be a string")),
    }
}

/// Unsigned integer in `min..=max`.
pub(crate) fn u64_param(params: &Value, path: &str, min: u64, max: u64) -> Result<Option<u64>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(v) => match v.as_u64() {
            Some(n) if (min..=max).contains(&n) => Ok(Some(n)),
            _ => Err(format!("{path} must be an integer between {min} and {max}")),
        },
    }
}

/// Array of strings (each trimmed). Non-string entries are an error.
pub(crate) fn str_array_param(params: &Value, path: &str) -> Result<Option<Vec<String>>, String> {
    match param_at(params, path) {
        None => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| {
                v.as_str()
                    .map(|s| s.trim().to_string())
                    .ok_or_else(|| format!("{path} entries must be strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{path} must be an array of strings")),
    }
}

/// True when the text is non-empty and free of control characters.
pub(crate) fn is_plain_text(s: &str, max_chars: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max_chars && !s.chars().any(char::is_control)
}

// ── TOML table navigation ───────────────────────────────────────────────────

/// Get-or-create the nested table at `path` (e.g. `["container", "sandbox"]`).
pub(crate) fn table_at_mut<'a>(table: &'a mut toml::Table, path: &[&str]) -> Result<&'a mut toml::Table, String> {
    let mut cur = table;
    for (i, seg) in path.iter().enumerate() {
        cur = cur
            .entry(*seg)
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .ok_or_else(|| format!("[{}] in config.toml is not a table", path[..=i].join(".")))?;
    }
    Ok(cur)
}

/// Value at dotted `path`, if every segment exists.
pub(crate) fn toml_at<'a>(table: &'a toml::Table, path: &str) -> Option<&'a toml::Value> {
    let mut segs = path.split('.');
    let mut cur = table.get(segs.next()?)?;
    for seg in segs {
        cur = cur.as_table()?.get(seg)?;
    }
    Some(cur)
}

/// The value at `path` as JSON, `null` when absent. For audit before/after.
pub(crate) fn toml_at_json(table: &toml::Table, path: &str) -> Value {
    toml_at(table, path)
        .and_then(|v| serde_json::to_value(v).ok())
        .unwrap_or(Value::Null)
}

// ── Locked commit ───────────────────────────────────────────────────────────

/// Create `path` fresh with owner-only permissions (0600 on Unix) and write
/// `content`. Config files hold encrypted secrets and the keys of the raw
/// editor's placeholders, so no temp or backup copy is ever world-readable,
/// not even for the moment between create and chmod.
pub(crate) fn write_owner_only(path: &Path, content: &str) -> std::io::Result<()> {
    use std::io::Write;
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(content.as_bytes())?;
    f.sync_all()
}

/// Hex SHA-256 of a file's bytes; the empty string for an absent file.
pub(crate) fn content_hash(content: &str) -> String {
    hex::encode(Sha256::digest(content.as_bytes()))
}

/// Read `path` as text: `Ok("")` when absent, `Err` on any other IO error.
pub(crate) fn read_text_or_empty(path: &Path) -> Result<String, String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(format!("failed to read {}: {e}", path.display())),
    }
}

/// Atomically replace `path` with `new_content`, under the cross-process
/// advisory lock, but only when the file still hashes to `expected_hash`
/// (the hash of what the caller read). Synchronous; call it through
/// `spawn_blocking`.
pub(crate) fn commit_text_locked(path: &Path, expected_hash: &str, new_content: &str) -> Result<(), String> {
    commit_rendered_locked(path, expected_hash, |_| Ok(new_content.to_string()))
}

/// Commit `table` with [`commit_text_locked`]. The bytes written are the
/// file as it is on disk (verified against `expected_hash` under the lock)
/// with only the keys that differ from `table` edited in place — see
/// [`render_preserving`] — so the operator's comments, blank lines and key
/// order survive a dashboard save.
pub(crate) async fn commit_table_locked(path: &Path, expected_hash: String, table: &toml::Table) -> Result<(), String> {
    let path = path.to_path_buf();
    let table = table.clone();
    tokio::task::spawn_blocking(move || {
        commit_rendered_locked(&path, &expected_hash, |current| render_preserving(current, &table))
    })
    .await
    .map_err(|e| format!("config write task failed: {e}"))?
}

/// [`commit_text_locked`] with the new content computed from the current
/// file text while the lock is held (and after the hash compare), so the
/// render and the write see the same bytes.
pub(crate) fn commit_rendered_locked(
    path: &Path,
    expected_hash: &str,
    render: impl FnOnce(&str) -> Result<String, String>,
) -> Result<(), String> {
    let outcome = duduclaw_core::with_file_lock(path, || {
        let current = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Ok(Err(format!("failed to re-read {}: {e}", path.display()))),
        };
        if content_hash(&current) != expected_hash {
            return Ok(Err(changed_on_disk(path)));
        }
        let new_content = match render(&current) {
            Ok(c) => c,
            Err(e) => return Ok(Err(e)),
        };
        replace_owner_only(path, &new_content)?;
        Ok(Ok(()))
    });
    match outcome {
        Ok(inner) => inner,
        Err(e) => Err(format!("failed to write {}: {e}", path.display())),
    }
}

fn changed_on_disk(path: &Path) -> String {
    format!(
        "{} changed on disk while this edit was open (another writer saved first) — reload and try again",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("config file")
    )
}

/// Write `content` to `<path>.toml.tmp` (owner-only) and rename it over
/// `path`. The caller holds the lock.
fn replace_owner_only(path: &Path, content: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    write_owner_only(&tmp, content)?;
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

// ── Format-preserving render ────────────────────────────────────────────────

/// Render `new_table` as an edit of `original_text`: every key whose value is
/// unchanged keeps its exact bytes (comments above it, trailing comment,
/// blank lines, position, inline-table / array formatting); a changed scalar
/// keeps its line's decoration (the trailing comment survives); a removed key
/// goes with its own line; a new key is appended at the end of its section;
/// a new section is created after the last table of its parent (for a
/// top-level section, the end of the file).
///
/// `Err` when `original_text` is not valid TOML: callers must never replace
/// a file they could not parse. An absent / blank original is rendered with
/// `toml::to_string_pretty`, as before. If the edited document would not
/// read back as exactly `new_table` (a bug here), the full re-serialisation
/// is used instead, which is always correct, only not format-preserving.
pub(crate) fn render_preserving(original_text: &str, new_table: &toml::Table) -> Result<String, String> {
    let pretty = || toml::to_string_pretty(new_table).map_err(|e| format!("failed to serialize config: {e}"));
    if original_text.trim().is_empty() {
        return pretty();
    }
    let unparsable = |e: &dyn std::fmt::Display| {
        format!("config file is not valid TOML ({e}) — refusing to overwrite it (fix it in the raw editor or a terminal first)")
    };
    let old: toml::Table = original_text.parse().map_err(|e: toml::de::Error| unparsable(&e))?;
    let mut doc: toml_edit::DocumentMut =
        original_text.parse().map_err(|e: toml_edit::TomlError| unparsable(&e))?;
    sync_table(doc.as_table_mut(), &old, new_table, false);
    let rendered = doc.to_string();
    match rendered.parse::<toml::Table>() {
        Ok(back) if &back == new_table => Ok(rendered),
        _ => {
            tracing::warn!("format-preserving config render diverged; writing a full re-serialisation instead");
            pretty()
        }
    }
}

/// Bring `dst` (whose current content is `old`) to `new`, touching only the
/// keys that differ. `inline` is true inside an inline table, where every
/// child must be a plain value.
fn sync_table(dst: &mut dyn toml_edit::TableLike, old: &toml::Table, new: &toml::Table, inline: bool) {
    let removed: Vec<&String> = old.keys().filter(|k| !new.contains_key(*k)).collect();
    for key in removed {
        dst.remove(key);
    }
    for (key, nv) in new {
        match (old.get(key), dst.get_mut(key)) {
            (Some(ov), Some(_)) if ov == nv => {}
            (Some(ov), Some(item)) => sync_item(item, ov, nv, inline),
            _ if inline => {
                let value = append_inline_value(dst, to_value(nv));
                dst.insert(key, toml_edit::Item::Value(value));
            }
            _ => {
                dst.insert(key, fresh_item(nv, inline));
            }
        }
    }
}

/// An appended inline-table entry takes over the closing whitespace of what
/// was the last entry, so `{ a = 1 }` grows to `{ a = 1, b = 2 }` and not
/// `{ a = 1 , b = 2 }`.
fn append_inline_value(dst: &mut dyn toml_edit::TableLike, mut value: toml_edit::Value) -> toml_edit::Value {
    let closing = dst.iter_mut().last().and_then(|(_, item)| {
        let last = item.as_value_mut()?;
        let suffix = last.decor().suffix().cloned();
        last.decor_mut().set_suffix("");
        suffix
    });
    value.decor_mut().set_prefix(" ");
    if let Some(suffix) = closing {
        value.decor_mut().set_suffix(suffix);
    }
    value
}

fn sync_item(item: &mut toml_edit::Item, old: &toml::Value, new: &toml::Value, inline: bool) {
    use toml_edit::{Item, Value as EV};
    match (item, old, new) {
        (Item::Table(t), toml::Value::Table(o), toml::Value::Table(n)) => sync_table(t, o, n, false),
        (Item::Value(EV::InlineTable(t)), toml::Value::Table(o), toml::Value::Table(n)) => sync_table(t, o, n, true),
        (Item::ArrayOfTables(a), toml::Value::Array(o), toml::Value::Array(n)) if all_tables(n) && all_tables(o) => {
            sync_array_of_tables(a, o, n)
        }
        (Item::Value(v), _, _) if inline || !needs_block(new) => {
            let mut replacement = to_value(new);
            *replacement.decor_mut() = v.decor().clone();
            *v = replacement;
        }
        (item, _, _) => *item = fresh_item(new, inline),
    }
}

/// Same length: edit element by element. Otherwise keep every old element
/// that survives unchanged (in order, so a removal keeps its neighbours'
/// bytes) and create the rest fresh.
fn sync_array_of_tables(dst: &mut toml_edit::ArrayOfTables, old: &[toml::Value], new: &[toml::Value]) {
    if old.len() == new.len() && dst.len() == old.len() {
        for (i, (o, n)) in old.iter().zip(new).enumerate() {
            if let (Some(t), Some(ot), Some(nt)) = (dst.get_mut(i), o.as_table(), n.as_table())
                && ot != nt
            {
                sync_table(t, ot, nt, false);
            }
        }
        return;
    }
    let kept: Vec<toml_edit::Table> = dst.iter().cloned().collect();
    let mut rebuilt = toml_edit::ArrayOfTables::new();
    let mut next = 0;
    for n in new {
        let found = (next..old.len().min(kept.len())).find(|&k| &old[k] == n);
        match (found, n.as_table()) {
            (Some(k), _) => {
                rebuilt.push(kept[k].clone());
                next = k + 1;
            }
            (None, Some(nt)) => rebuilt.push(fresh_table(nt)),
            (None, None) => {}
        }
    }
    *dst = rebuilt;
}

fn all_tables(items: &[toml::Value]) -> bool {
    !items.is_empty() && items.iter().all(toml::Value::is_table)
}

/// Whether `v` must be written as a `[section]` / `[[array]]` block rather
/// than a plain value (what `toml::to_string_pretty` would do).
fn needs_block(v: &toml::Value) -> bool {
    match v {
        toml::Value::Table(_) => true,
        toml::Value::Array(a) => all_tables(a),
        _ => false,
    }
}

fn fresh_item(v: &toml::Value, inline: bool) -> toml_edit::Item {
    match v {
        toml::Value::Table(t) if !inline => toml_edit::Item::Table(fresh_table(t)),
        toml::Value::Array(a) if !inline && all_tables(a) => {
            let mut aot = toml_edit::ArrayOfTables::new();
            for t in a.iter().filter_map(toml::Value::as_table) {
                aot.push(fresh_table(t));
            }
            toml_edit::Item::ArrayOfTables(aot)
        }
        _ => toml_edit::Item::Value(to_value(v)),
    }
}

/// A new `[section]`: implicit, so a section holding only sub-sections gets
/// no empty header of its own (as `toml::to_string_pretty` writes it).
fn fresh_table(t: &toml::Table) -> toml_edit::Table {
    let mut out = toml_edit::Table::new();
    out.set_implicit(true);
    for (k, v) in t {
        out.insert(k, fresh_item(v, false));
    }
    out
}

fn to_value(v: &toml::Value) -> toml_edit::Value {
    match v {
        toml::Value::String(s) => s.as_str().into(),
        toml::Value::Integer(i) => (*i).into(),
        toml::Value::Float(f) => (*f).into(),
        toml::Value::Boolean(b) => (*b).into(),
        toml::Value::Datetime(d) => match d.to_string().parse::<toml_edit::Datetime>() {
            Ok(dt) => dt.into(),
            // Unreachable for a datetime toml itself parsed; the read-back
            // check in `render_preserving` catches it either way.
            Err(_) => d.to_string().into(),
        },
        toml::Value::Array(a) => toml_edit::Value::Array(a.iter().map(to_value).collect()),
        toml::Value::Table(t) => {
            let mut out = toml_edit::InlineTable::new();
            for (k, v) in t {
                out.insert(k, to_value(v));
            }
            toml_edit::Value::InlineTable(out)
        }
    }
}

// ── Backups ─────────────────────────────────────────────────────────────────

/// How many `<file>.bak-<ts>` copies the raw editor keeps per file.
pub(crate) const RAW_BACKUPS_KEPT: usize = 5;

/// Copy `content` to `<path>.bak-<unix ts>` and delete all but the newest
/// [`RAW_BACKUPS_KEPT`] backups of that file. Returns the backup path.
/// A same-second collision gets a `-<n>` suffix rather than overwriting.
pub(crate) fn write_backup(path: &Path, content: &str, now_unix: i64) -> Result<PathBuf, String> {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return Err("config path has no file name".into());
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let mut backup = dir.join(format!("{name}.bak-{now_unix}"));
    let mut n = 1;
    while backup.exists() {
        backup = dir.join(format!("{name}.bak-{now_unix}-{n}"));
        n += 1;
    }
    // Backups hold the same secrets as the file itself.
    write_owner_only(&backup, content).map_err(|e| format!("failed to write backup: {e}"))?;
    prune_backups(dir, name, RAW_BACKUPS_KEPT);
    Ok(backup)
}

/// Delete all but the newest `keep` backups of `name` in `dir`. Order is by
/// the numeric timestamp in the name (then the collision suffix), so a
/// touched mtime can never make an old backup look new.
pub(crate) fn prune_backups(dir: &Path, name: &str, keep: usize) {
    let prefix = format!("{name}.bak-");
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let mut found: Vec<((i64, u32), PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let file = e.file_name().to_str()?.to_string();
            let rest = file.strip_prefix(&prefix)?;
            let mut parts = rest.splitn(2, '-');
            let ts = parts.next()?.parse::<i64>().ok()?;
            let seq = match parts.next() {
                Some(s) => s.parse::<u32>().ok()?,
                None => 0,
            };
            Some(((ts, seq), e.path()))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, p) in found.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn param_at_accepts_nested_and_flat() {
        let nested = json!({ "takeover": { "enabled": true } });
        let flat = json!({ "takeover.enabled": false });
        assert_eq!(bool_param(&nested, "takeover.enabled").unwrap(), Some(true));
        assert_eq!(bool_param(&flat, "takeover.enabled").unwrap(), Some(false));
        assert_eq!(bool_param(&json!({}), "takeover.enabled").unwrap(), None);
        assert_eq!(bool_param(&json!({"takeover": {"enabled": null}}), "takeover.enabled").unwrap(), None);
    }

    #[test]
    fn typed_getters_reject_wrong_types() {
        assert!(bool_param(&json!({"a": {"b": "true"}}), "a.b").is_err());
        assert!(u64_param(&json!({"a": -1}), "a", 0, 5).is_err());
        assert!(u64_param(&json!({"a": 9}), "a", 0, 5).is_err());
        assert_eq!(u64_param(&json!({"a": 5}), "a", 0, 5).unwrap(), Some(5));
        assert!(str_array_param(&json!({"a": [1]}), "a").is_err());
    }

    #[test]
    fn commit_refuses_when_file_changed_underneath() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "a = 1\n").unwrap();
        let stale = content_hash("a = 0\n");
        let err = commit_text_locked(&path, &stale, "a = 2\n").unwrap_err();
        assert!(err.contains("changed on disk"), "{err}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
        let fresh = content_hash("a = 1\n");
        commit_text_locked(&path, &fresh, "a = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 2\n");
    }

    #[test]
    fn commit_creates_an_absent_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        commit_text_locked(&path, &content_hash(""), "x = true\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "x = true\n");
    }

    #[cfg(unix)]
    #[test]
    fn commits_and_backups_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        commit_text_locked(&path, &content_hash(""), "x = 1\n").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let b = write_backup(&path, "x = 1\n", 1).unwrap();
        assert_eq!(std::fs::metadata(&b).unwrap().permissions().mode() & 0o777, 0o600);
    }

    const FIXTURE: &str = r#"# DuDuClaw config, hand edited

# Gateway section comment
[gateway]
port = 18789 # trailing on port
bind = "127.0.0.1"
# auth_token = "commented-out"

[general]
default_agent = "dudu"   # who answers
log_level = "info"

# the mailbox
[mail]
# the switch
enabled = false # off for now
# who reads mail
default_agent = "dudu"
# drop folder
dropfolder_enabled = true

[[accounts]]
id = "a"
api_key_enc = "ZW5jcnlwdGVk/base64=="

[[accounts]]
id = "b"
priority = 2

[tick]
enabled = false

[[tick.sources]]
id = "s1"
kind = "http_poll"
url = "https://example.com"
headers = { "X-Key" = "v" }

[channels.line]
channel_token_enc = "abc=="

[[channels.line.accounts]]
name = "oa1"

# trailing comment at end
"#;

    fn edited(f: impl FnOnce(&mut toml::Table)) -> String {
        let mut t: toml::Table = FIXTURE.parse().unwrap();
        f(&mut t);
        let out = render_preserving(FIXTURE, &t).unwrap();
        assert_eq!(out.parse::<toml::Table>().unwrap(), t, "{out}");
        out
    }

    fn mail(t: &mut toml::Table) -> &mut toml::Table {
        t.get_mut("mail").unwrap().as_table_mut().unwrap()
    }

    #[test]
    fn unchanged_table_renders_byte_identical() {
        assert_eq!(edited(|_| {}), FIXTURE);
    }

    #[test]
    fn changing_one_key_changes_only_its_value() {
        let out = edited(|t| {
            mail(t).insert("enabled".into(), toml::Value::Boolean(true));
        });
        assert_eq!(out, FIXTURE.replace("enabled = false # off for now", "enabled = true # off for now"));
    }

    #[test]
    fn new_key_lands_at_the_end_of_its_section() {
        let out = edited(|t| {
            mail(t).insert("auto_trigger".into(), toml::Value::Boolean(false));
        });
        assert_eq!(
            out,
            FIXTURE.replace("dropfolder_enabled = true\n", "dropfolder_enabled = true\nauto_trigger = false\n")
        );
    }

    #[test]
    fn new_section_goes_to_the_end_without_losing_comments() {
        let out = edited(|t| {
            let mut night = toml::Table::new();
            night.insert("llm_enabled".into(), toml::Value::Boolean(true));
            t.insert("night".into(), toml::Value::Table(night));
        });
        let body = FIXTURE.trim_end_matches("\n# trailing comment at end\n");
        assert!(out.starts_with(body), "{out}");
        assert!(out.contains("[night]\nllm_enabled = true\n"), "{out}");
        assert!(out.ends_with("# trailing comment at end\n"), "{out}");
    }

    #[test]
    fn removing_a_key_keeps_the_neighbours_comments() {
        let out = edited(|t| {
            mail(t).remove("default_agent");
        });
        assert_eq!(out, FIXTURE.replace("# who reads mail\ndefault_agent = \"dudu\"\n", ""));
    }

    #[test]
    fn array_of_tables_append_and_remove_keep_other_elements() {
        let appended = edited(|t| {
            let accounts = t.get_mut("accounts").unwrap().as_array_mut().unwrap();
            let mut c = toml::Table::new();
            c.insert("id".into(), toml::Value::String("c".into()));
            accounts.push(toml::Value::Table(c));
        });
        assert_eq!(
            appended,
            FIXTURE.replace("priority = 2\n", "priority = 2\n\n[[accounts]]\nid = \"c\"\n")
        );
        let removed = edited(|t| {
            t.get_mut("accounts").unwrap().as_array_mut().unwrap().remove(0);
        });
        assert_eq!(
            removed,
            FIXTURE.replace("[[accounts]]\nid = \"a\"\napi_key_enc = \"ZW5jcnlwdGVk/base64==\"\n\n", "")
        );
        // Editing inside one element of an array of tables.
        let tweaked = edited(|t| {
            let src = t["tick"]["sources"][0].clone();
            let mut src = src.as_table().unwrap().clone();
            src.insert("url".into(), toml::Value::String("https://example.org".into()));
            t.get_mut("tick").unwrap().as_table_mut().unwrap().insert(
                "sources".into(),
                toml::Value::Array(vec![toml::Value::Table(src)]),
            );
        });
        assert_eq!(tweaked, FIXTURE.replace("https://example.com", "https://example.org"));
    }

    #[test]
    fn inline_table_edit_keeps_its_inline_form() {
        let out = edited(|t| {
            let src = t.get_mut("tick").unwrap().get_mut("sources").unwrap().as_array_mut().unwrap()[0]
                .as_table_mut()
                .unwrap();
            src.get_mut("headers").unwrap().as_table_mut().unwrap().insert("X-Other".into(), toml::Value::String("w".into()));
        });
        assert!(out.contains(r#"headers = { "X-Key" = "v", X-Other = "w" }"#), "{out}");
    }

    #[test]
    fn unparsable_original_is_refused_and_blank_is_pretty() {
        let t: toml::Table = "a = 1".parse().unwrap();
        assert!(render_preserving("[broken", &t).unwrap_err().contains("not valid TOML"));
        assert_eq!(render_preserving("", &t).unwrap(), "a = 1\n");
    }

    #[test]
    fn commit_table_locked_preserves_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, FIXTURE).unwrap();
        let mut t: toml::Table = FIXTURE.parse().unwrap();
        mail(&mut t).insert("enabled".into(), toml::Value::Boolean(true));
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(commit_table_locked(&path, content_hash(FIXTURE), &t)).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            FIXTURE.replace("enabled = false # off for now", "enabled = true # off for now")
        );
    }

    #[test]
    fn backups_keep_the_newest_five() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for ts in 100..108 {
            write_backup(&path, &format!("v = {ts}\n"), ts).unwrap();
        }
        // Same-second collision gets a suffix and counts as newer.
        write_backup(&path, "v = dup\n", 107).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![
                "config.toml.bak-104",
                "config.toml.bak-105",
                "config.toml.bak-106",
                "config.toml.bak-107",
                "config.toml.bak-107-1",
            ]
        );
    }
}
