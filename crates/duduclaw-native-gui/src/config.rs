// S2 — minimal on-disk settings persistence.
//
// Scope: only the locale choice needs to survive a restart (task item 4,
// "Locale 持久化"). A hand-written two-function reader/writer is enough for
// one scalar key — deliberately NOT a general TOML parser/dependency. If a
// later phase's settings screen needs more keys (theme, window size, ...),
// upgrade to a real `toml` crate dependency then; growing this file by hand
// past a couple of flat `key = "value"` lines would be the wrong call.
//
// Read/write failures are ALWAYS fail-open (task brief: "讀寫失敗一律
// fail-open 不擋啟動") — a missing/corrupt config file or an unwritable
// directory degrades to "show the language picker" / "silently didn't
// save", never a panic or a blocked launch.

use std::io::Write;
use std::path::PathBuf;

use crate::i18n::Locale;

const CONFIG_FILE_NAME: &str = "native-gui.toml";

/// `<duduclaw home>`. Home resolution intentionally mirrors
/// `duduclaw-core::platform::duduclaw_home` (`$DUDUCLAW_HOME` verbatim when
/// set and non-empty, else `$HOME`/`$USERPROFILE` + `/.duduclaw`) — same
/// resolution *logic*, hand-duplicated rather than linked, because this
/// crate deliberately excludes itself from the root workspace to keep
/// gpui's dependency tree away from the gateway build (see this crate's
/// `Cargo.toml` comment); pulling in `duduclaw-core` here would defeat that
/// isolation for the sake of one path function.
///
/// `pub` (WP-C-M2): `sidecar.rs` needs the same home directory for its own
/// pidfile/PATH-augmentation lookups and must resolve it identically — a
/// second hand-rolled copy of this env-var chain would risk drifting out of
/// sync with this one.
pub fn duduclaw_home() -> Option<PathBuf> {
    match std::env::var("DUDUCLAW_HOME") {
        Ok(custom) if !custom.trim().is_empty() => Some(PathBuf::from(custom)),
        _ => {
            let home_dir = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok()?;
            if home_dir.trim().is_empty() {
                return None;
            }
            Some(PathBuf::from(home_dir).join(".duduclaw"))
        }
    }
}

/// `<duduclaw home>/native-gui.toml`.
fn config_path() -> Option<PathBuf> {
    Some(duduclaw_home()?.join(CONFIG_FILE_NAME))
}

/// Read the persisted locale, if any. `None` on any failure whatsoever
/// (unresolvable home dir, missing file, unreadable, malformed, or an
/// unrecognized locale code) — the caller falls back to the language-picker
/// screen exactly as if this were a first launch.
pub fn load_locale() -> Option<Locale> {
    let path = config_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let code = find_scalar_value(&content, "locale")?;
    locale_from_code(&code)
}

/// Persist the chosen locale. Best-effort: logs to stderr and returns on
/// any failure (directory missing/uncreatable, permissions, ...) — losing
/// the "remember my language" nicety is never worth surfacing an error the
/// language-picker screen has no UI to show anyway.
pub fn save_locale(locale: Locale) {
    save_scalar("locale", locale.code());
}

fn locale_from_code(code: &str) -> Option<Locale> {
    Locale::ALL.into_iter().find(|l| l.code() == code)
}

// ── WP-C-M2: gateway connection mode ─────────────────────────────────────
//
// Whether this app should manage a local sidecar gateway (default) or stay
// pointed at a remote one the user picked via `screens::gateway_picker`.
// Stored alongside `locale` in the SAME file — see `upsert_scalar`'s doc
// comment for why every save now reads-modifies-writes the whole key set
// instead of truncating the file (the bug `save_locale` used to have: it
// blindly overwrote the file with only `locale = ...`, which would have
// silently dropped any `gateway_*` keys a prior run had written).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayMode {
    /// Manage (attach to or spawn) a gateway on this machine. Default.
    Local,
    /// Connect to the gateway at `GatewaySelection::remote_url` instead.
    Remote,
}

impl GatewayMode {
    fn code(self) -> &'static str {
        match self {
            GatewayMode::Local => "local",
            GatewayMode::Remote => "remote",
        }
    }

    fn from_code(code: &str) -> GatewayMode {
        // Fail-safe: anything unrecognized (corrupt file, future value this
        // build doesn't know about) degrades to the always-safe default
        // rather than refusing to start.
        match code {
            "remote" => GatewayMode::Remote,
            _ => GatewayMode::Local,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySelection {
    pub mode: GatewayMode,
    /// Set when `mode == Remote` (the URL the user picked). May still be
    /// `None` under `Remote` if the file was hand-edited into an
    /// inconsistent state — callers must treat that as "no usable remote"
    /// and fall back to local, never panic on it.
    pub remote_url: Option<String>,
}

/// Read the persisted gateway selection, if any. `None` on any failure
/// (unresolvable home dir, missing file, unreadable, no `gateway_mode` key
/// at all) — callers fall back to `GatewayMode::Local` exactly as if this
/// were a first launch, same fail-open contract as [`load_locale`].
pub fn load_gateway_selection() -> Option<GatewaySelection> {
    let path = config_path()?;
    let content = std::fs::read_to_string(path).ok()?;
    let mode = GatewayMode::from_code(&find_scalar_value(&content, "gateway_mode")?);
    let remote_url = find_scalar_value(&content, "gateway_remote_url");
    Some(GatewaySelection { mode, remote_url })
}

/// Persist a gateway selection (`screens::gateway_picker`'s "連線"/"切換到
/// 本機" actions). `Local` clears any stored `gateway_remote_url` so a later
/// stale read can't resurrect an abandoned remote target; `Remote` requires
/// a non-empty URL (the caller is expected to have already validated it —
/// this function does not re-validate, it only persists).
pub fn save_gateway_selection(sel: &GatewaySelection) {
    let mut kvs = read_all_scalars();
    upsert(&mut kvs, "gateway_mode", sel.mode.code());
    match (&sel.mode, &sel.remote_url) {
        (GatewayMode::Remote, Some(url)) if !url.trim().is_empty() => {
            upsert(&mut kvs, "gateway_remote_url", url);
        }
        _ => remove(&mut kvs, "gateway_remote_url"),
    }
    write_all_scalars(&kvs);
}

/// Read every `key = "value"` line in the config file (empty on any
/// failure — missing file, unresolvable home dir, ...), preserving file
/// order so a rewrite doesn't needlessly reorder keys a human might be
/// reading.
fn read_all_scalars() -> Vec<(String, String)> {
    let Some(path) = config_path() else { return Vec::new() };
    let Ok(content) = std::fs::read_to_string(&path) else { return Vec::new() };
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else { continue };
        let v = v.trim();
        let v = v.strip_prefix('"').unwrap_or(v);
        let v = v.strip_suffix('"').unwrap_or(v);
        out.push((k.trim().to_string(), v.to_string()));
    }
    out
}

fn upsert(kvs: &mut Vec<(String, String)>, key: &str, value: &str) {
    if let Some(entry) = kvs.iter_mut().find(|(k, _)| k == key) {
        entry.1 = value.to_string();
    } else {
        kvs.push((key.to_string(), value.to_string()));
    }
}

fn remove(kvs: &mut Vec<(String, String)>, key: &str) {
    kvs.retain(|(k, _)| k != key);
}

/// Write the full key set back out, one `key = "value"` line each.
/// Best-effort: logs to stderr and returns on any failure, same fail-open
/// contract every other write in this module follows.
fn write_all_scalars(kvs: &[(String, String)]) {
    let Some(path) = config_path() else {
        eprintln!(
            "[config] could not resolve a home directory (no $DUDUCLAW_HOME/$HOME/$USERPROFILE) \
             — setting will not persist"
        );
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("[config] could not create {}: {e}", dir.display());
            return;
        }
    }
    let mut content = String::new();
    for (k, v) in kvs {
        content.push_str(k);
        content.push_str(" = \"");
        content.push_str(v);
        content.push_str("\"\n");
    }
    let write_result = std::fs::File::create(&path).and_then(|mut f| f.write_all(content.as_bytes()));
    if let Err(e) = write_result {
        eprintln!("[config] could not write {}: {e}", path.display());
    }
}

/// Read-modify-write a single scalar key, preserving every other key
/// already in the file. [`save_locale`]'s original implementation truncated
/// the whole file to just `locale = "..."` — harmless while locale was the
/// only persisted setting, but would have silently dropped `gateway_*` keys
/// the moment both existed side by side. Every scalar save in this module
/// now goes through this one path.
fn save_scalar(key: &str, value: &str) {
    let mut kvs = read_all_scalars();
    upsert(&mut kvs, key, value);
    write_all_scalars(&kvs);
}

/// Extract `key = "value"` from a flat, hand-written-TOML-shaped file —
/// handles exactly the shape [`save_locale`] writes (one `key = "quoted
/// value"` per line; blank lines and `#` comments tolerated) and nothing
/// more. NOT a TOML parser: no tables, no multi-line strings, no escape
/// sequences beyond what a locale code (`zh-TW` / `en` / `ja-JP`) ever
/// needs.
fn find_scalar_value(content: &str, key: &str) -> Option<String> {
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() != key {
            continue;
        }
        let v = v.trim();
        let v = v.strip_prefix('"').unwrap_or(v);
        let v = v.strip_suffix('"').unwrap_or(v);
        return Some(v.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `cargo test` runs every `#[test]` in this binary across a thread
    /// pool by default, but `DUDUCLAW_HOME` (like any env var) is process-
    /// global — two tests concurrently mutating it (each snapshot-then-
    /// restore, matching a DIFFERENT temp dir) can interleave and either
    /// observe the other's directory mid-test or clobber the restore.
    /// Every test below that touches `DUDUCLAW_HOME` acquires this lock
    /// FIRST and holds it for the entire snapshot/mutate/restore window, so
    /// at most one such test runs its env-touching section at a time
    /// (`.unwrap_or_else(|p| p.into_inner())` recovers from a prior test
    /// panicking mid-section — poisoning must not cascade into every
    /// subsequent test in this module failing too).
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn parses_the_exact_shape_save_locale_writes() {
        assert_eq!(
            find_scalar_value("locale = \"zh-TW\"\n", "locale"),
            Some("zh-TW".to_string())
        );
    }

    #[test]
    fn tolerates_comments_and_blank_lines() {
        let content = "# native gui settings\n\nlocale = \"en\"\n";
        assert_eq!(find_scalar_value(content, "locale"), Some("en".to_string()));
    }

    #[test]
    fn missing_key_is_none() {
        assert_eq!(find_scalar_value("other = 1\n", "locale"), None);
    }

    #[test]
    fn line_without_equals_sign_is_skipped_not_fatal() {
        // Regression guard: an earlier draft used `line.split_once('=')?`
        // inside the loop, which would bail the WHOLE function (not just
        // skip the line) the first time it hit a line with no `=` — e.g. a
        // stray comment-less blank-ish line. Any key after such a line
        // would become permanently unreadable.
        let content = "not a valid line at all\nlocale = \"ja-JP\"\n";
        assert_eq!(find_scalar_value(content, "locale"), Some("ja-JP".to_string()));
    }

    #[test]
    fn unrecognized_locale_code_is_none() {
        assert_eq!(locale_from_code("fr-FR"), None);
        assert_eq!(locale_from_code("ja-JP"), Some(Locale::JaJp));
    }

    #[test]
    fn config_path_honors_duduclaw_home_override() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", "/tmp/duduclaw-native-gui-test-home") };
        let path = config_path();
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_HOME", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_HOME") },
        }
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/tmp/duduclaw-native-gui-test-home/native-gui.toml"
            ))
        );
    }

    #[test]
    fn gateway_mode_from_code_round_trips_and_fails_safe() {
        assert_eq!(GatewayMode::from_code("remote"), GatewayMode::Remote);
        assert_eq!(GatewayMode::from_code("local"), GatewayMode::Local);
        // Corrupt/future-unknown value degrades to the always-safe default
        // rather than refusing to start.
        assert_eq!(GatewayMode::from_code("garbage"), GatewayMode::Local);
        assert_eq!(GatewayMode::from_code(""), GatewayMode::Local);
    }

    /// Runs a closure with `DUDUCLAW_HOME` pointed at a fresh, empty temp
    /// directory, then restores the prior value — same snapshot/restore
    /// shape `config_path_honors_duduclaw_home_override` already
    /// established, factored out because the tests below need it twice.
    fn with_temp_home<R>(f: impl FnOnce(&std::path::Path) -> R) -> R {
        // Held for the WHOLE snapshot/mutate-env/run-closure/restore
        // window, not just the mutation instant — see `ENV_LOCK`'s own doc
        // comment for why a narrower critical section wouldn't be safe
        // here (the closure itself reads `DUDUCLAW_HOME`, transitively,
        // via `save_locale`/`save_gateway_selection`/etc.).
        let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

        let dir = std::env::temp_dir().join(format!(
            "duduclaw-native-gui-config-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &dir) };
        let result = f(&dir);
        match prev {
            Some(v) => unsafe { std::env::set_var("DUDUCLAW_HOME", v) },
            None => unsafe { std::env::remove_var("DUDUCLAW_HOME") },
        }
        let _ = std::fs::remove_dir_all(&dir);
        result
    }

    #[test]
    fn save_locale_then_save_gateway_selection_preserves_both_keys() {
        // Regression guard for the bug `save_scalar`'s doc comment
        // describes: `save_locale`'s original implementation truncated the
        // whole file, so writing a gateway selection afterward would have
        // silently dropped the persisted locale (and vice versa).
        with_temp_home(|_| {
            save_locale(Locale::JaJp);
            save_gateway_selection(&GatewaySelection {
                mode: GatewayMode::Remote,
                remote_url: Some("http://192.168.1.5:18789".to_string()),
            });
            assert_eq!(load_locale(), Some(Locale::JaJp));
            assert_eq!(
                load_gateway_selection(),
                Some(GatewaySelection {
                    mode: GatewayMode::Remote,
                    remote_url: Some("http://192.168.1.5:18789".to_string()),
                })
            );

            // Now save the locale again (a language-picker re-visit) — the
            // gateway selection written above must still be intact.
            save_locale(Locale::En);
            assert_eq!(load_locale(), Some(Locale::En));
            assert_eq!(
                load_gateway_selection().map(|s| s.mode),
                Some(GatewayMode::Remote)
            );
        });
    }

    #[test]
    fn switching_back_to_local_clears_stored_remote_url() {
        with_temp_home(|_| {
            save_gateway_selection(&GatewaySelection {
                mode: GatewayMode::Remote,
                remote_url: Some("http://10.0.0.9:18789".to_string()),
            });
            save_gateway_selection(&GatewaySelection { mode: GatewayMode::Local, remote_url: None });
            let sel = load_gateway_selection().unwrap();
            assert_eq!(sel.mode, GatewayMode::Local);
            assert_eq!(sel.remote_url, None, "stale remote URL must not survive a switch back to local");
        });
    }

    #[test]
    fn load_gateway_selection_missing_file_is_none() {
        with_temp_home(|_| {
            assert_eq!(load_gateway_selection(), None);
        });
    }
}
