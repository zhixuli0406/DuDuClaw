//! CUR-2: making a live cursor-source switch survive a compositor restart.
//!
//! # Why this file exists at all
//!
//! CUR-1 shipped [`crate::cursor::source::CURSOR_SOURCE_ENV`] and argued (in
//! that module's doc) that an env var was enough because "the durable value
//! lives wherever the session launcher that spawns comp keeps its
//! environment". CUR-2's brief rejects that for the user-facing case: a
//! person toggling 外觀 → 指標 in the shell must not have their compositor
//! restarted, and must not have to hand-edit `/etc/duduclaw/kiosk.env`. So
//! the preference needs somewhere of its own to live.
//!
//! # Where, and why not somewhere else
//!
//! `$XDG_STATE_HOME/duduclaw-comp/cursor.json`, falling back to
//! `$HOME/.local/state/duduclaw-comp/cursor.json` — the XDG Base Directory
//! Specification's own home for "state data that should persist between
//! (application) restarts, but that is not important or portable enough to
//! the user that it should be stored in `$XDG_DATA_HOME`". A cursor
//! preference is exactly that.
//!
//! Rejected alternatives, and the reason each lost:
//!
//! * **`$XDG_RUNTIME_DIR`** — where this crate's socket and audit log live.
//!   It is a tmpfs that systemd removes when the unit stops
//!   (`RuntimeDirectory=duduclaw-kiosk`,
//!   `appliance/mkosi.extra/etc/systemd/system/duduclaw-kiosk.service`), so a
//!   preference stored there would survive exactly as long as the process
//!   that set it. That defeats the whole point.
//! * **`/etc/duduclaw/kiosk.env`** — root-owned on the appliance; the kiosk
//!   session runs unprivileged and cannot write it. It stays the OPERATOR's
//!   channel (see the priority order below), which is a different thing from
//!   a user preference.
//! * **A new comp config file** — CUR-1's brief explicitly forbade growing
//!   "一整套設定系統", and one key does not justify one.
//!
//! On the appliance this resolves to `/data/duduclaw-kiosk/.local/state/…`:
//! `$HOME` comes from the `duduclaw-kiosk` user's passwd entry
//! (`useradd -d /data/duduclaw-kiosk`, `appliance/postinst.d/
//! 20-users-and-units.sh`), the directory is created and chowned at first
//! boot (`duduclaw-firstboot-provision.sh`), and `/data` is the persistent
//! partition. `ProtectHome=yes` on the kiosk unit does not touch it — that
//! directive covers `/home`, `/root` and `/run/user`, none of which is where
//! this user's home lives.
//!
//! # Priority: env var beats this file, always
//!
//! ```text
//! DUDUCLAW_COMP_CURSOR_SOURCE   (operator, explicit)   highest
//! cursor.json                   (user preference)
//! CursorSource::System          (default)              lowest
//! ```
//!
//! An operator who pins the value in the spawn environment is making a
//! deliberate, machine-level statement (debugging, a locked-down deployment);
//! a preference file must not silently override it. The `set_cursor_source`
//! op still applies live in that case and still writes the file — it simply
//! reports `env_pinned: true` so a settings UI can say "this will revert
//! next restart" instead of lying.
//!
//! # Failure behaviour
//!
//! Every failure here is non-fatal and degrades to "no stored preference":
//! unreadable file, malformed JSON, unknown value, no `$HOME` at all. The
//! cursor is not something worth refusing to boot over. A WRITE failure, by
//! contrast, is reported back to the caller (`persisted: false` in the
//! response) — silently pretending a preference was saved is exactly the
//! kind of false success this repo's conventions forbid.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::source::CursorSource;

/// Directory under the XDG state home. Named after the binary, per the XDG
/// convention that each application owns one subdirectory.
pub const STATE_SUBDIR: &str = "duduclaw-comp";
/// The preference file itself.
pub const STATE_FILE_NAME: &str = "cursor.json";

/// On-disk shape. JSON (rather than a bare `brand\n` line) purely so a later
/// cursor preference — size, a theme name — can be added without inventing a
/// second file or a second parser; `#[serde(default)]` means an older file
/// missing a newer key still loads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CursorPrefs {
    /// `"system"` / `"brand"`. Stored as a string, not as the enum's serde
    /// representation, so the file stays hand-editable and so an
    /// unrecognised value degrades to "no preference" rather than to a parse
    /// error that would discard the whole file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// Where the preference file lives, or `None` when this process has no home
/// directory to put it in.
///
/// Pure except for the two `std::env::var` reads; the resolution rule itself
/// is [`state_path_from`], which is what the tests drive.
pub fn state_path() -> Option<PathBuf> {
    state_path_from(
        std::env::var_os("XDG_STATE_HOME").as_deref(),
        std::env::var_os("HOME").as_deref(),
    )
}

/// Testable core of [`state_path`].
///
/// Both inputs must be ABSOLUTE to be used — the XDG spec says a relative
/// value "should be considered invalid and the application should fall back
/// to the default", and honouring a relative one would make the file land
/// wherever comp happened to be started from.
pub fn state_path_from(
    xdg_state_home: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
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

/// Reads the stored preference. `None` for "there isn't one" AND for every
/// failure mode — see this module's doc on why that is deliberate.
pub fn load() -> Option<CursorSource> {
    let path = state_path()?;
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "cursor: could not read the stored cursor preference — using the default"
            );
            return None;
        }
    };
    parse(&bytes, &path)
}

/// Testable core of [`load`]'s parsing half.
fn parse(bytes: &[u8], path: &std::path::Path) -> Option<CursorSource> {
    let prefs: CursorPrefs = match serde_json::from_slice(bytes) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %path.display(),
                "cursor: stored cursor preference is not valid JSON — ignoring it"
            );
            return None;
        }
    };
    let raw = prefs.source?;
    match CursorSource::parse_strict(&raw) {
        Some(s) => Some(s),
        None => {
            tracing::warn!(
                value = %raw,
                path = %path.display(),
                "cursor: stored cursor preference names an unknown source — ignoring it"
            );
            None
        }
    }
}

/// Writes the preference, creating the directory chain if needed.
///
/// Temp-file-plus-rename, so a crash mid-write leaves the previous value
/// intact rather than a truncated file. No cross-process advisory lock:
/// `duduclaw-comp` is the only writer of this path for its whole lifetime
/// (the same single-writer-process reasoning `shell_control::audit` gives
/// for using a plain `Mutex`), and the shell reaches the value through the
/// socket, never by writing the file itself.
pub fn store(source: CursorSource) -> Result<PathBuf, String> {
    let path = state_path().ok_or_else(|| {
        "no XDG_STATE_HOME and no absolute HOME — nowhere to persist the cursor preference"
            .to_string()
    })?;
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;

    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let prefs = CursorPrefs {
        source: Some(source.as_str().to_string()),
    };
    let mut body =
        serde_json::to_vec(&prefs).map_err(|e| format!("serialize cursor preference: {e}"))?;
    body.push(b'\n');

    // `.tmp` next to the target (not in /tmp) so the rename below is
    // guaranteed to be same-filesystem and therefore atomic.
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
        let p = state_path_from(Some(OsStr::new("/var/lib/x")), Some(OsStr::new("/home/u")))
            .expect("should resolve");
        assert_eq!(p, PathBuf::from("/var/lib/x/duduclaw-comp/cursor.json"));
    }

    #[test]
    fn falls_back_to_home_local_state() {
        let p = state_path_from(None, Some(OsStr::new("/data/duduclaw-kiosk")))
            .expect("should resolve");
        assert_eq!(
            p,
            PathBuf::from("/data/duduclaw-kiosk/.local/state/duduclaw-comp/cursor.json")
        );
    }

    #[test]
    fn relative_or_empty_values_are_not_honoured() {
        // A relative XDG_STATE_HOME must fall through to HOME, not put the
        // file wherever comp was started from.
        let p = state_path_from(Some(OsStr::new("relative/state")), Some(OsStr::new("/home/u")))
            .expect("should fall through to HOME");
        assert_eq!(p, PathBuf::from("/home/u/.local/state/duduclaw-comp/cursor.json"));

        assert!(state_path_from(Some(OsStr::new("")), Some(OsStr::new(""))).is_none());
        // `/` as a home directory is what a misconfigured service account
        // looks like; writing `/.local/state` there would need root.
        assert!(state_path_from(None, Some(OsStr::new("/"))).is_none());
        assert!(state_path_from(None, None).is_none());
    }

    #[test]
    fn parses_a_stored_brand_preference() {
        let p = std::path::Path::new("/nonexistent/cursor.json");
        assert_eq!(parse(br#"{"source":"brand"}"#, p), Some(CursorSource::Brand));
        assert_eq!(parse(br#"{"source":"system"}"#, p), Some(CursorSource::System));
        // Hand-edited with different casing / whitespace — `parse_strict`
        // trims and case-folds, so this is still honoured.
        assert_eq!(parse(br#"{"source":" Brand "}"#, p), Some(CursorSource::Brand));
    }

    #[test]
    fn malformed_or_unknown_content_degrades_to_no_preference() {
        let p = std::path::Path::new("/nonexistent/cursor.json");
        assert_eq!(parse(b"not json at all", p), None);
        assert_eq!(parse(b"{}", p), None);
        assert_eq!(parse(br#"{"source":"claw"}"#, p), None);
        assert_eq!(parse(br#"{"source":""}"#, p), None);
        // A key from some future version must not discard the whole file.
        assert_eq!(
            parse(br#"{"source":"brand","size":48}"#, p),
            Some(CursorSource::Brand)
        );
    }

    #[test]
    fn store_then_load_round_trips_through_a_real_file() {
        // Drives the real `store`/`load` pair by pointing XDG_STATE_HOME at a
        // temp dir. `set_var` is `unsafe` since Rust 1.82 and is only sound
        // when nothing else is reading the environment concurrently — this
        // test is the only one in the crate that touches XDG_STATE_HOME, and
        // it restores the previous value before returning.
        let dir = std::env::temp_dir().join(format!(
            "duduclaw-comp-cursor-persist-{}-{}",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let previous = std::env::var_os("XDG_STATE_HOME");
        // SAFETY: single-threaded test body; restored below.
        unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };

        assert_eq!(load(), None, "a fresh state dir has no preference");

        let written = store(CursorSource::Brand).expect("store must succeed");
        assert_eq!(written, dir.join("duduclaw-comp/cursor.json"));
        assert_eq!(load(), Some(CursorSource::Brand));

        store(CursorSource::System).expect("overwrite must succeed");
        assert_eq!(load(), Some(CursorSource::System));

        // No `.tmp` litter left behind by a successful write.
        assert!(!dir.join("duduclaw-comp/cursor.json.tmp").exists());

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
