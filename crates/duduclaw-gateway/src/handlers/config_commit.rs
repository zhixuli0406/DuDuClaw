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
    let outcome = duduclaw_core::with_file_lock(path, || {
        let current = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Ok(Err(format!("failed to re-read {}: {e}", path.display()))),
        };
        if content_hash(&current) != expected_hash {
            return Ok(Err(format!(
                "{} changed on disk while this edit was open (another writer saved first) — reload and try again",
                path.file_name().and_then(|n| n.to_str()).unwrap_or("config file")
            )));
        }
        let tmp = path.with_extension("toml.tmp");
        write_owner_only(&tmp, new_content)?;
        if let Err(e) = std::fs::rename(&tmp, path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        Ok(Ok(()))
    });
    match outcome {
        Ok(inner) => inner,
        Err(e) => Err(format!("failed to write {}: {e}", path.display())),
    }
}

/// Serialize `table` and commit it with [`commit_text_locked`].
pub(crate) async fn commit_table_locked(path: &Path, expected_hash: String, table: &toml::Table) -> Result<(), String> {
    let content = toml::to_string_pretty(table).map_err(|e| format!("failed to serialize config: {e}"))?;
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || commit_text_locked(&path, &expected_hash, &content))
        .await
        .map_err(|e| format!("config write task failed: {e}"))?
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
