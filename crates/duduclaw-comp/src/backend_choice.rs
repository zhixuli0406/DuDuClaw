//! A4-1: runtime (NOT compile-time) backend selection.
//!
//! One binary carries both backends and decides at startup which one owns
//! the display:
//!
//! - **winit** — nested inside an existing Wayland/X11 session. This is the
//!   development/CI path (`BUILD.md`'s container rounds run
//!   `weston --backend=headless-backend.so` → `duduclaw-comp` → `foot`), and
//!   it is what `WAYLAND_DISPLAY`/`DISPLAY` being set actually means: "there
//!   is already a compositor/X server here, don't try to take the KMS
//!   master lease away from it".
//! - **udev/DRM** — real hardware ownership via libseat + DRM/KMS + libinput
//!   (`udev_backend.rs`). This is the appliance path, replacing the
//!   `cage → duduclaw-comp(winit)` double stack documented in `BUILD.md`.
//!
//! `DUDUCLAW_COMP_BACKEND=winit|udev` forces either one; an unrecognised
//! value is a hard error rather than a silent fallback (a typo'd override
//! that quietly did the opposite of what the operator asked is exactly the
//! kind of "sounds like it worked" failure this repo's conventions forbid).
//!
//! Everything here is a pure function of three strings so the decision is
//! unit-testable without a display server, a seat, or a GPU — the same
//! constraint every other decision helper in this crate is written under
//! (see `input.rs::is_system_gesture_tail`).

/// Which display backend owns this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Nested inside a host Wayland/X11 session (`winit_backend.rs`).
    Winit,
    /// Direct hardware ownership: libseat + DRM/KMS + libinput
    /// (`udev_backend.rs`).
    Udev,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Winit => "winit",
            BackendKind::Udev => "udev",
        }
    }
}

/// Environment variable that forces a backend regardless of what the
/// session environment looks like.
pub const BACKEND_ENV: &str = "DUDUCLAW_COMP_BACKEND";

/// Pure selection rule. `force` is `$DUDUCLAW_COMP_BACKEND`,
/// `wayland_display` is `$WAYLAND_DISPLAY`, `x11_display` is `$DISPLAY`.
///
/// Precedence:
/// 1. An explicit `force` value wins over everything (including a
///    contradictory session environment — that is the whole point of the
///    override: `DUDUCLAW_COMP_BACKEND=udev` from inside a terminal on
///    another compositor is how you deliberately switch to a free VT).
/// 2. Otherwise a non-empty `WAYLAND_DISPLAY` **or** `DISPLAY` means there
///    is a host session to nest inside → winit.
/// 3. Otherwise → udev/DRM.
///
/// Empty-string env vars are treated as unset: `WAYLAND_DISPLAY=` is what
/// you get from a systemd unit that declares `Environment=WAYLAND_DISPLAY=`
/// without a value, and it must not be read as "there is a host session".
pub fn choose_backend(
    force: Option<&str>,
    wayland_display: Option<&str>,
    x11_display: Option<&str>,
) -> Result<BackendKind, String> {
    if let Some(raw) = force {
        let v = raw.trim();
        if !v.is_empty() {
            return match v.to_ascii_lowercase().as_str() {
                "winit" | "nested" => Ok(BackendKind::Winit),
                // `drm` and `kms` accepted as aliases because that is what
                // the same thing is called in every other compositor's
                // docs; the canonical spelling stays `udev`.
                "udev" | "drm" | "kms" => Ok(BackendKind::Udev),
                other => Err(format!(
                    "{BACKEND_ENV}={other:?} is not a known backend (expected \"winit\" or \"udev\")"
                )),
            };
        }
    }

    let nested = [wayland_display, x11_display]
        .into_iter()
        .flatten()
        .any(|v| !v.trim().is_empty());

    Ok(if nested {
        BackendKind::Winit
    } else {
        BackendKind::Udev
    })
}

/// Reads the three environment variables and applies [`choose_backend`].
pub fn choose_backend_from_env() -> Result<BackendKind, String> {
    let force = std::env::var(BACKEND_ENV).ok();
    let wl = std::env::var("WAYLAND_DISPLAY").ok();
    let x11 = std::env::var("DISPLAY").ok();
    choose_backend(force.as_deref(), wl.as_deref(), x11.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_session_env_means_real_hardware() {
        assert_eq!(choose_backend(None, None, None).unwrap(), BackendKind::Udev);
    }

    #[test]
    fn a_wayland_session_means_nested_winit() {
        assert_eq!(
            choose_backend(None, Some("wayland-0"), None).unwrap(),
            BackendKind::Winit
        );
    }

    #[test]
    fn an_x11_session_means_nested_winit() {
        assert_eq!(choose_backend(None, None, Some(":0")).unwrap(), BackendKind::Winit);
    }

    #[test]
    fn empty_env_values_are_treated_as_unset() {
        // A systemd `Environment=WAYLAND_DISPLAY=` (or an `export
        // WAYLAND_DISPLAY=` left over in a profile script) must not be
        // mistaken for "a host compositor is running here".
        assert_eq!(
            choose_backend(None, Some(""), Some("   ")).unwrap(),
            BackendKind::Udev
        );
    }

    #[test]
    fn force_wins_over_a_live_session() {
        assert_eq!(
            choose_backend(Some("udev"), Some("wayland-0"), Some(":0")).unwrap(),
            BackendKind::Udev
        );
    }

    #[test]
    fn force_winit_wins_over_a_bare_tty() {
        assert_eq!(choose_backend(Some("winit"), None, None).unwrap(), BackendKind::Winit);
    }

    #[test]
    fn force_accepts_documented_aliases_case_insensitively() {
        for v in ["UDEV", "drm", "KMS", "Udev"] {
            assert_eq!(choose_backend(Some(v), None, None).unwrap(), BackendKind::Udev, "{v}");
        }
        for v in ["WINIT", "Nested"] {
            assert_eq!(choose_backend(Some(v), None, None).unwrap(), BackendKind::Winit, "{v}");
        }
    }

    #[test]
    fn an_empty_force_value_falls_through_to_autodetection() {
        assert_eq!(choose_backend(Some(""), Some("wayland-0"), None).unwrap(), BackendKind::Winit);
        assert_eq!(choose_backend(Some("  "), None, None).unwrap(), BackendKind::Udev);
    }

    #[test]
    fn a_typoed_force_value_is_a_hard_error_not_a_silent_fallback() {
        let err = choose_backend(Some("udv"), None, None).unwrap_err();
        assert!(err.contains("udv"), "error must quote the offending value: {err}");
        assert!(err.contains(BACKEND_ENV), "error must name the env var: {err}");
    }

    #[test]
    fn backend_kind_names_are_the_documented_spellings() {
        assert_eq!(BackendKind::Winit.as_str(), "winit");
        assert_eq!(BackendKind::Udev.as_str(), "udev");
    }
}
