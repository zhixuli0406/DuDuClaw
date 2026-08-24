// Q1 (2026-08-24): the shipping gate for this crate's debug affordances.
//
// ── The hole this closes ────────────────────────────────────────────────
// Every debug/diagnostic override in this crate used to be gated on nothing
// but its own environment variable. That is fine on a developer's machine
// and not fine on a duty appliance, because the appliance's kiosk launcher
// sources `/etc/duduclaw/kiosk.env` with `set -a`
// (`appliance/mkosi.extra/usr/local/sbin/duduclaw-kiosk-launch.sh`) into the
// whole session tree, on a read-write root. So ONE file write turned into,
// among other things:
//
//     DUDUCLAW_SHELL_LOCK_NO_PASSWORD=1   # the lock screen stops asking
//     DUDUCLAW_SHELL_DIAG=1               # every keystroke goes to journald
//
// — persistent across reboots, invisible in the UI, and needing no image
// rebuild and no `APPLIANCE_DEBUG`. `APPLIANCE_DEBUG` itself is NOT the
// problem here: it only ever sets a root password at bake time
// (`appliance/build.sh`), and a shipping bake leaves root locked.
//
// ── The gate ────────────────────────────────────────────────────────────
// A Cargo feature, **off by default**, so it is compiled in rather than
// configured — a file write cannot forge it, and neither can an env var.
// Off-by-default is the load-bearing half: forgetting the flag produces a
// SAFE binary, whereas an on-by-default feature that ships must be actively
// turned off by whoever remembers to.
//
//     cargo build                                  # ship: affordances gone
//     cargo build --features debug-affordances     # dev loop: affordances on
//     cargo test  --features debug-affordances     # both, see below
//
// `cargo test` passes in BOTH configurations. Tests that assert an
// affordance actually works are `#[cfg(feature = "debug-affordances")]`;
// tests that assert the gate refuses are unconditional and read
// [`debug_affordances_available`] rather than assuming either answer, so
// neither configuration silently skips its own half.
//
// ── What is deliberately NOT gated ──────────────────────────────────────
// Operational configuration is not a debug affordance and stays live in
// every build: `DUDUCLAW_SHELL_LOCK_PRIVACY`, `DUDUCLAW_SHELL_LOCK_IDLE_MINS`,
// `DUDUCLAW_SHELL_NO_LAYER_SHELL`, `DUDUCLAW_SHELL_GATEWAY_URL`,
// `XDG_RUNTIME_DIR`. Two of those carry real risk and are recorded honestly
// in this round's report rather than changed here without a decision:
// `LOCK_IDLE_MINS=0` disables auto-lock, and `GATEWAY_URL` points the
// password check itself at an arbitrary host.

/// Whether this binary was built with the debug affordances compiled in.
///
/// `const fn` on purpose: every call site is a compile-time constant, so the
/// dead branch is optimized out and the env var it reads is not even present
/// in the shipping binary's strings.
pub(crate) const fn debug_affordances_available() -> bool {
    cfg!(feature = "debug-affordances")
}

/// Reads `name` **only** when debug affordances are compiled in.
///
/// The single choke point every gated override goes through, so "is this one
/// gated?" is answered by whether it calls this function rather than by
/// reading each call site's own conditional. Returns `None` in a shipping
/// build regardless of what the environment says.
pub(crate) fn debug_env(name: &str) -> Option<String> {
    if !debug_affordances_available() {
        return None;
    }
    std::env::var(name).ok()
}

/// `debug_env(name)` narrowed to the `=1` convention most of this crate's
/// switches use.
pub(crate) fn debug_env_is_one(name: &str) -> bool {
    debug_env(name).is_some_and(|v| v.trim() == "1")
}

/// One line on stderr at boot when a shipping-unsafe build is running, so
/// "why is this machine behaving oddly" has an answer in the journal.
///
/// Deliberately unconditional output (not `diag`-gated): the entire point is
/// that a build with the affordances compiled in must announce itself even
/// when no diagnostic switch is set.
pub(crate) fn announce_build_flavour() {
    if debug_affordances_available() {
        eprintln!(
            "[shipping] WARNING: built with --features debug-affordances — \
             DUDUCLAW_SHELL_DIAG / _DEBUG_SURFACE / _SKIP_OOBE / _FAKE_* / \
             _LOCK_NO_PASSWORD are LIVE in this binary. Not for shipping."
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the gate rather than asserting a fixed answer — this file's
    /// header comment explains why both configurations must have a real
    /// assertion instead of one of them skipping.
    #[test]
    fn the_gate_agrees_with_the_cargo_feature() {
        assert_eq!(debug_affordances_available(), cfg!(feature = "debug-affordances"));
    }

    #[test]
    fn a_shipping_build_reads_no_debug_env_var_at_all() {
        // Uses a name nothing else in this crate touches, so this cannot
        // race another test's own env handling.
        const NAME: &str = "DUDUCLAW_SHELL_SHIPPING_GATE_SELFTEST";
        unsafe {
            std::env::set_var(NAME, "1");
        }
        let seen = debug_env(NAME);
        let one = debug_env_is_one(NAME);
        unsafe {
            std::env::remove_var(NAME);
        }
        if debug_affordances_available() {
            assert_eq!(seen.as_deref(), Some("1"));
            assert!(one);
        } else {
            assert_eq!(seen, None, "a shipping build must not observe debug env vars");
            assert!(!one);
        }
    }
}
