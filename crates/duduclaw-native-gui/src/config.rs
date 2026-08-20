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

/// `<duduclaw home>/native-gui.toml`. Home resolution intentionally mirrors
/// `duduclaw-core::platform::duduclaw_home` (`$DUDUCLAW_HOME` verbatim when
/// set and non-empty, else `$HOME`/`$USERPROFILE` + `/.duduclaw`) — same
/// resolution *logic*, hand-duplicated rather than linked, because this
/// crate deliberately excludes itself from the root workspace to keep
/// gpui's dependency tree away from the gateway build (see this crate's
/// `Cargo.toml` comment); pulling in `duduclaw-core` here would defeat that
/// isolation for the sake of one path function.
fn config_path() -> Option<PathBuf> {
    let home = match std::env::var("DUDUCLAW_HOME") {
        Ok(custom) if !custom.trim().is_empty() => PathBuf::from(custom),
        _ => {
            let home_dir = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok()?;
            if home_dir.trim().is_empty() {
                return None;
            }
            PathBuf::from(home_dir).join(".duduclaw")
        }
    };
    Some(home.join(CONFIG_FILE_NAME))
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
    let Some(path) = config_path() else {
        eprintln!(
            "[config] could not resolve a home directory (no $DUDUCLAW_HOME/$HOME/$USERPROFILE) \
             — locale choice will not persist"
        );
        return;
    };
    if let Some(dir) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(dir) {
            eprintln!("[config] could not create {}: {e}", dir.display());
            return;
        }
    }
    let content = format!("locale = \"{}\"\n", locale.code());
    let write_result = std::fs::File::create(&path).and_then(|mut f| f.write_all(content.as_bytes()));
    if let Err(e) = write_result {
        eprintln!("[config] could not write {}: {e}", path.display());
    }
}

fn locale_from_code(code: &str) -> Option<Locale> {
    Locale::ALL.into_iter().find(|l| l.code() == code)
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
        // SAFETY: `cargo test` runs each test in its own thread, but env
        // vars are process-global — this test only reads back its own
        // write within the same statement, and always restores the prior
        // value, so it doesn't leak state to other tests observing
        // `DUDUCLAW_HOME` (none currently do).
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
}
