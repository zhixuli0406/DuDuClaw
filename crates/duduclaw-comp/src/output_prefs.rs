//! WP-comp-shell-display D4b-3: making a live `set_output_scale` change
//! survive a compositor restart.
//!
//! # Why a second file, not `cursor::persist`
//!
//! `cursor::persist`'s own doc argues "one key does not justify one [state
//! file]" for the CURSOR domain specifically — but scale is a different
//! domain (per-OUTPUT, not per-seat), and it is keyed by output NAME rather
//! than being a single scalar, which would make `CursorPrefs` grow a shape
//! nothing about cursors needs. A second small file, same directory
//! (`$XDG_STATE_HOME/duduclaw-comp/`), same on-disk discipline
//! (temp-file-plus-rename, read-modify-write, every failure non-fatal) —
//! see that module's doc for the reasoning behind each of those choices,
//! which this file deliberately does not re-argue.
//!
//! # Keyed by output name, not a single scalar
//!
//! A real machine can have more than one screen, each independently scaled
//! (`set_output_scale`'s own wire shape already addresses one output by
//! name). `winit`'s output is always literally named `"winit"`; `udev`'s are
//! `"{interface}-{interface_id}"` (`udev_backend::build_surfaces`) — stable
//! across boots as long as the same monitor stays on the same connector,
//! which is the expected shape for a fixed-hardware duty machine. A
//! reconnected/renamed output simply has no stored entry and starts at the
//! scale it always started at (100%, `Output::new`'s implicit default) —
//! honest, not a regression.
//!
//! # Failure behaviour
//!
//! Same as `cursor::persist`: every READ failure (missing file, malformed
//! JSON, an entry naming a percentage outside
//! [`crate::shell_control::OUTPUT_SCALE_STEPS`]) degrades to "no
//! stored preference" — never a boot-blocking error. A WRITE failure is
//! reported back to the caller so `shell_control_set_output_scale` can
//! answer honestly rather than silently failing to persist what it just
//! applied live.

use std::{collections::HashMap, path::PathBuf};

use serde::{Deserialize, Serialize};

/// Same subdirectory `cursor::persist::STATE_SUBDIR` uses — one application,
/// one XDG state directory, multiple small files inside it.
pub const STATE_SUBDIR: &str = "duduclaw-comp";
/// The preference file itself. Deliberately not `cursor.json` — a different
/// domain, see this module's doc.
pub const STATE_FILE_NAME: &str = "display.json";

/// On-disk shape: output name -> last-applied scale percentage. `#[serde(
/// default)]` so a file predating this key (there is none yet, but the
/// pattern is copied from `cursor::persist::CursorPrefs` on purpose) still
/// loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DisplayPrefs {
    /// `i64`, not `u32` — the widest thing a hand-edited value could
    /// legally be, so a nonsense number degrades to "no preference for this
    /// output" via [`interpret_scale`] instead of failing the whole
    /// deserialization and taking every OTHER output's entry down with it
    /// (same reasoning `CursorPrefs::size` gives for its own `i64`).
    #[serde(default)]
    pub scale_pct: HashMap<String, i64>,
}

/// Where the preference file lives — byte-identical resolution rule to
/// `cursor::persist::state_path`, just a different final component.
pub fn state_path() -> Option<PathBuf> {
    state_path_from(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Testable core of [`state_path`]. Identical rule to `cursor::persist::
/// state_path_from` — both inputs must be absolute, `/` itself is rejected
/// (a misconfigured service account's HOME) — deliberately re-implemented
/// rather than shared, since the two functions' only common code would be
/// this validation and a shared helper would cost an extra indirection for
/// two ~15-line functions that will not grow a third caller.
pub fn state_path_from(xdg_state_home: Option<&std::ffi::OsStr>, home: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let usable = |v: Option<&std::ffi::OsStr>| -> Option<PathBuf> {
        let p = PathBuf::from(v?);
        if p.is_absolute() && p.as_os_str() != std::ffi::OsStr::new("/") {
            Some(p)
        } else {
            None
        }
    };

    let base = match usable(xdg_state_home) {
        Some(p) => p,
        None => usable(home)?.join(".local").join("state"),
    };
    Some(base.join(STATE_SUBDIR).join(STATE_FILE_NAME))
}

fn read_raw() -> DisplayPrefs {
    let Some(path) = state_path() else {
        return DisplayPrefs::default();
    };
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return DisplayPrefs::default(),
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "output_prefs: could not read the stored display preference — using the default"
            );
            return DisplayPrefs::default();
        }
    };
    parse_raw(&bytes, &path)
}

fn parse_raw(bytes: &[u8], path: &std::path::Path) -> DisplayPrefs {
    match serde_json::from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "output_prefs: stored display preference is not valid JSON — ignoring it"
            );
            DisplayPrefs::default()
        }
    }
}

/// The stored scale percentage for `output_name`, held to the same closed
/// [`crate::shell_control::OUTPUT_SCALE_STEPS`] set the live op
/// accepts — a hand-edited or stale value outside that set degrades to "no
/// preference" rather than being coerced, same "closed set, refuse rather
/// than clamp" policy `cursor::persist::interpret_size` follows.
pub fn load_scale_pct(output_name: &str) -> Option<i64> {
    interpret_scale(&read_raw(), output_name)
}

fn interpret_scale(prefs: &DisplayPrefs, output_name: &str) -> Option<i64> {
    let raw = *prefs.scale_pct.get(output_name)?;
    if crate::shell_control::OUTPUT_SCALE_STEPS.contains(&raw) {
        Some(raw)
    } else {
        tracing::warn!(
            output = output_name,
            value = raw,
            "output_prefs: stored scale is not one of the offered steps — ignoring it"
        );
        None
    }
}

/// Writes `output_name`'s scale, preserving every other output's entry
/// already in the file (same read-modify-write discipline `cursor::persist::
/// store`/`store_size` established, for the same data-loss reason).
pub fn store_scale_pct(output_name: &str, scale_pct: i64) -> Result<PathBuf, String> {
    let mut prefs = read_raw();
    prefs.scale_pct.insert(output_name.to_string(), scale_pct);
    write_raw(&prefs)
}

fn write_raw(prefs: &DisplayPrefs) -> Result<PathBuf, String> {
    let path = state_path().ok_or_else(|| {
        "no XDG_STATE_HOME and no absolute HOME — nowhere to persist the display preference".to_string()
    })?;
    let dir = path.parent().ok_or_else(|| format!("{} has no parent directory", path.display()))?;

    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let mut body = serde_json::to_vec(prefs).map_err(|e| format!("serialize display preference: {e}"))?;
    body.push(b'\n');

    // `.tmp` next to the target, same atomicity reasoning as
    // `cursor::persist::write_raw`.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, &body).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    if let Err(e) = std::fs::rename(&tmp, &path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("rename into {}: {e}", path.display()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn prefers_xdg_state_home_when_absolute() {
        let p = state_path_from(Some(OsStr::new("/var/lib/x")), Some(OsStr::new("/home/u"))).expect("should resolve");
        assert_eq!(p, PathBuf::from("/var/lib/x/duduclaw-comp/display.json"));
    }

    #[test]
    fn falls_back_to_home_local_state() {
        let p = state_path_from(None, Some(OsStr::new("/data/duduclaw-kiosk"))).expect("should resolve");
        assert_eq!(p, PathBuf::from("/data/duduclaw-kiosk/.local/state/duduclaw-comp/display.json"));
    }

    #[test]
    fn relative_or_empty_values_are_not_honoured() {
        // A relative XDG_STATE_HOME must fall through to HOME, not put the
        // file wherever comp was started from.
        let p = state_path_from(Some(OsStr::new("relative/state")), Some(OsStr::new("/home/u")))
            .expect("should fall through to HOME");
        assert_eq!(p, PathBuf::from("/home/u/.local/state/duduclaw-comp/display.json"));

        assert!(state_path_from(Some(OsStr::new("")), Some(OsStr::new(""))).is_none());
        assert!(state_path_from(None, Some(OsStr::new("/"))).is_none());
        assert!(state_path_from(None, None).is_none());
    }

    fn parse(bytes: &[u8], p: &std::path::Path, output: &str) -> Option<i64> {
        interpret_scale(&parse_raw(bytes, p), output)
    }

    #[test]
    fn parses_a_stored_scale_for_the_named_output() {
        let p = std::path::Path::new("/nonexistent/display.json");
        let bytes = br#"{"scale_pct":{"Virtual-1":200}}"#;
        assert_eq!(parse(bytes, p, "Virtual-1"), Some(200));
        assert_eq!(parse(bytes, p, "HDMI-A-1"), None, "no entry for a different output");
    }

    #[test]
    fn a_value_outside_the_offered_steps_degrades_to_no_preference() {
        let p = std::path::Path::new("/nonexistent/display.json");
        for bad in [0, -1, 99, 101, 201, 999] {
            let bytes = format!(r#"{{"scale_pct":{{"Virtual-1":{bad}}}}}"#).into_bytes();
            assert_eq!(parse(&bytes, p, "Virtual-1"), None, "{bad} must be ignored");
        }
    }

    #[test]
    fn malformed_content_degrades_to_no_preference() {
        let p = std::path::Path::new("/nonexistent/display.json");
        assert_eq!(parse(b"not json", p, "Virtual-1"), None);
        assert_eq!(parse(b"{}", p, "Virtual-1"), None);
    }

    #[test]
    fn store_then_load_round_trips_through_a_real_file() {
        let dir = std::env::temp_dir().join(format!("duduclaw-comp-output-prefs-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&dir);
        let previous = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: single-threaded test body; restored below. Same pattern
        // `cursor::persist`'s own round-trip test uses.
        unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };

        assert_eq!(load_scale_pct("Virtual-1"), None, "a fresh state dir has no preference");

        let written = store_scale_pct("Virtual-1", 200).expect("store must succeed");
        assert_eq!(written, dir.join("duduclaw-comp/display.json"));
        assert_eq!(load_scale_pct("Virtual-1"), Some(200));

        // A second output's entry must not evict the first.
        store_scale_pct("HDMI-A-1", 150).expect("store must succeed");
        assert_eq!(load_scale_pct("Virtual-1"), Some(200), "the first output's entry must survive");
        assert_eq!(load_scale_pct("HDMI-A-1"), Some(150));

        // Overwriting one output's own entry works.
        store_scale_pct("Virtual-1", 100).expect("overwrite must succeed");
        assert_eq!(load_scale_pct("Virtual-1"), Some(100));
        assert_eq!(load_scale_pct("HDMI-A-1"), Some(150), "unaffected");

        assert!(!dir.join("duduclaw-comp/display.json.tmp").exists());

        // SAFETY: same as above.
        unsafe {
            match previous {
                Some(v) => std::env::set_var("XDG_STATE_HOME", v),
                None => std::env::remove_var("XDG_STATE_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
