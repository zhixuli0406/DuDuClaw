// OOBE disk persistence + boot-entry resolution — split out of
// `oobe/mod.rs` (WP-OOBE-split, 2026-08-21, see `state.rs`'s own header
// comment for the full four-way split this round makes). Carries the
// atomic-write state file (`state_path`/`load_state`/`save_state` — task
// brief: "寫入用 temp+rename 原子寫", "斷電續步") and the pure boot-time
// decision functions built on top of it (`resolve_boot_flow`/`boot_theme`)
// that `main.rs` calls once at window-open time. The persisted DATA SHAPE
// itself (`OobeState`/`OobeSelections` and the value enums they carry)
// lives in `selections.rs`, imported here rather than duplicated.

use std::path::PathBuf;

use super::selections::{OobeState, ThemeChoice};
use super::state::{OobeFlow, OobeStep};

const STATE_SUBDIR: &str = "shell";
const STATE_FILE_NAME: &str = "oobe_state.json";

/// Home resolution intentionally mirrors `duduclaw-core::platform::
/// duduclaw_home` (`$DUDUCLAW_HOME` verbatim when set and non-empty, else
/// `$HOME`/`$USERPROFILE` + `/.duduclaw`) — hand-duplicated, not linked,
/// same reasoning `duduclaw-native-gui/src/config.rs`'s own `config_path()`
/// doc comment gives for why THAT crate doesn't pull in `duduclaw-core` for
/// one path function: this crate deliberately excludes itself from the root
/// workspace to keep gpui's dependency tree away from the gateway build.
fn duduclaw_home() -> Option<PathBuf> {
    match std::env::var("DUDUCLAW_HOME") {
        Ok(custom) if !custom.trim().is_empty() => Some(PathBuf::from(custom)),
        _ => {
            let home_dir = std::env::var("HOME").or_else(|_| std::env::var("USERPROFILE")).ok()?;
            if home_dir.trim().is_empty() {
                return None;
            }
            Some(PathBuf::from(home_dir).join(".duduclaw"))
        }
    }
}

/// `<duduclaw home>/shell/oobe_state.json` (task brief: "落
/// `$DUDUCLAW_HOME`... `shell/` 下").
pub fn state_path() -> Option<PathBuf> {
    duduclaw_home().map(|home| home.join(STATE_SUBDIR).join(STATE_FILE_NAME))
}

/// `None`/unreadable/corrupt file all degrade to `OobeState::default()` —
/// same fail-open contract `duduclaw-native-gui/src/config.rs`'s
/// `load_locale` establishes ("a missing/corrupt config file... degrades to
/// ... exactly as if this were a first launch").
pub fn load_state() -> OobeState {
    state_path().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|content| serde_json::from_str(&content).ok()).unwrap_or_default()
}

/// Best-effort atomic write (temp file + rename, task brief: "寫入用
/// temp+rename 原子寫") — survives a mid-write power loss on a kiosk device
/// without corrupting the state file (the whole point of the "斷電續步"
/// requirement: a torn/partial JSON write must never brick the next boot's
/// `load_state()`). Any failure logs to stderr and returns; losing the
/// "resume at the same step" nicety is never worth blocking the OOBE UI
/// thread on a disk error.
pub fn save_state(state: &OobeState) {
    let Some(path) = state_path() else {
        eprintln!("[oobe] could not resolve a home directory (no $DUDUCLAW_HOME/$HOME/$USERPROFILE) — state will not persist");
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!("[oobe] could not create {}: {e}", dir.display());
        return;
    }
    let content = match serde_json::to_string_pretty(state) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[oobe] could not serialize state: {e}");
            return;
        }
    };
    let tmp_path = dir.join(format!("{STATE_FILE_NAME}.tmp"));
    if let Err(e) = std::fs::write(&tmp_path, content) {
        eprintln!("[oobe] could not write temp state file {}: {e}", tmp_path.display());
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, &path) {
        eprintln!("[oobe] could not rename temp state file into place: {e}");
    }
}

// ── Boot-entry resolution ─────────────────────────────────────────────

/// Resolves what the shell should show at boot — `Some(flow)` to open OOBE
/// immediately in that state, `None` to go straight to Home. Pure function
/// over already-read env values + the persisted state (never reads env or
/// disk itself), so the priority rules (task brief: "兩者都設時 FORCE
/// 優先") are unit-testable without touching real env vars or disk — see
/// `main.rs` for where the env vars actually get read.
///
/// Priority: `DUDUCLAW_SHELL_FORCE_OOBE=1` > `DUDUCLAW_SHELL_SKIP_OOBE=1` >
/// a recognized `DUDUCLAW_SHELL_DEBUG_OOBE_STEP` > the persisted state's own
/// `completed` flag. FORCE and DEBUG_STEP both ignore `persisted.completed`
/// (that's the entire point of a debug/force override); DEBUG_STEP also
/// ignores `persisted.current_step` (jumps straight to the requested step)
/// but keeps `persisted.selections` so earlier fake choices are still
/// visible when jumping ahead.
pub fn resolve_boot_flow(force: Option<&str>, skip: Option<&str>, debug_step: Option<&str>, persisted: OobeState) -> Option<OobeFlow> {
    if force.is_some_and(|v| v == "1") {
        return Some(OobeFlow::from_state(persisted));
    }
    if skip.is_some_and(|v| v == "1") {
        return None;
    }
    if let Some(raw) = debug_step {
        if let Some(step) = OobeStep::from_debug_env(raw) {
            let mut state = persisted;
            state.completed = false;
            state.current_step = step;
            return Some(OobeFlow::from_state(state));
        }
    }
    if persisted.completed {
        None
    } else {
        Some(OobeFlow::from_state(persisted))
    }
}

/// The boot-time theme Home should render in — Shell-S1 (2026-08-20,
/// Home/overlay dark theme). Deliberately SEPARATE from `resolve_boot_flow`
/// above: that function only returns `Some(flow)` when OOBE ITSELF should
/// render, which on the most common boot path (a returning operator who
/// already finished OOBE) is `None` — reading the theme choice back OUT of
/// that `Option` would silently lose it on exactly the path real users hit
/// every day. This reads straight off the persisted state instead, before
/// `resolve_boot_flow` ever gets to decide Home-vs-OOBE, so it answers the
/// SAME regardless of that decision — `main()` calls this first (`ThemeChoice`
/// is `Copy`, so reading the field doesn't need to clone `persisted` before
/// moving it into `resolve_boot_flow` next). Default (no persisted choice,
/// e.g. a fresh install) is `ThemeChoice::Light` — `OobeSelections::
/// default()`'s own `#[default]`, same value `OobeStep::Theme`'s own doc
/// comment already documents as this crate's honest "no choice made yet"
/// baseline.
pub fn boot_theme(persisted: &OobeState) -> ThemeChoice {
    persisted.selections.theme
}

/// The operator's display name for the LOCKSCREEN's identity row — ICON-3
/// (2026-08-23). Same "read it off the persisted state at boot, before
/// `resolve_boot_flow` consumes it" shape `boot_theme` above establishes,
/// and for the same reason: the lockscreen only ever renders on the boot
/// path where OOBE resolved to `None`, so pulling this out of that `Option`
/// would lose it exactly where it is needed.
///
/// Whitespace-only and empty values come back as `None`, not as a blank
/// name — the lockscreen's identity row falls back to `avatar-default` plus
/// no name text in that case, which is honest ("this machine has no name on
/// file") rather than rendering an empty pill.
///
/// This is the SAME local-display-only field `OobeSelections::operator_name`
/// documents: the gateway's `admin@local` account has no profile name of its
/// own to ask for, so what OOBE typed is the only name that exists. It is
/// never used for authentication — `gateway_client::verify_password` sends
/// only the password.
pub fn boot_operator_name(persisted: &OobeState) -> Option<String> {
    persisted.selections.operator_name.as_deref().map(str::trim).filter(|name| !name.is_empty()).map(str::to_string)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::oobe::selections::{LanguageChoice, OobeSelections, TemplateChoice};

    // `std::env::set_var`/`remove_var` are `unsafe` (stdlib requires it
    // regardless of edition on this toolchain) and process-global — these
    // tests serialize via ENV_LOCK and always restore the prior value, same
    // discipline `duduclaw-core/src/platform.rs`'s own `home_tests` module
    // establishes for its `DUDUCLAW_HOME`-mutating tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn save_then_load_round_trips_the_full_state() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let mut flow = OobeFlow::new();
        flow.set_language(LanguageChoice::En);
        flow.next();
        flow.set_network("DuDu-Office", true);
        flow.next();
        save_state(flow.state());

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Network);
        assert_eq!(loaded.selections.language, LanguageChoice::En);
        assert!(loaded.selections.network_connected);
        assert_eq!(loaded.selections.network_ssid.as_deref(), Some("DuDu-Office"));
        assert!(!loaded.completed);
    }

    #[test]
    fn load_state_with_no_file_is_the_default() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-empty-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }

        assert_eq!(loaded, OobeState::default());
    }

    // ── round 2: forward-compat hardening (task brief §C) ────────────

    #[test]
    fn oobe_selections_round_trips_every_new_field() {
        let mut selections = OobeSelections {
            runtime_authorized: true,
            privacy_usage_stats: true,
            privacy_marketing: true,
            theme: ThemeChoice::Dark,
            operator_name: Some("Louis".to_string()),
            ..OobeSelections::default()
        };
        selections.template_choice = Some(TemplateChoice::Custom("retail".to_string()));

        let json = serde_json::to_string(&selections).expect("serialize");
        let back: OobeSelections = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, selections);
    }

    #[test]
    fn load_state_tolerates_missing_new_fields_from_an_older_schema() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-oldschema-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        // Hand-written JSON shaped like an OLDER binary's `oobe_state.json`
        // — has `current_step`/`completed`/the five `selections` keys round
        // 1 shipped with, but predates every field round 2 adds
        // (`runtime_authorized`, the four `privacy_*` toggles). Task brief:
        // "舊檔缺新欄位時逐欄回預設而非整檔 fail-open 重置到步 0" — this must
        // NOT collapse to `OobeState::default()` (step 0); it must load
        // with `current_step` preserved and only the missing fields
        // defaulted.
        let old_schema = r#"{
            "completed": false,
            "current_step": "privacy",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": true,
                "runtime_deferred": false,
                "template_choice": null
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Privacy, "must NOT fall back to step 0 just because new fields are missing");
        assert!(!loaded.completed);
        assert!(loaded.selections.network_connected);
        assert_eq!(loaded.selections.network_ssid.as_deref(), Some("DuDu-Office"));
        assert!(loaded.selections.account_created);
        // Every field this round adds must default in, not error the whole
        // file out.
        assert!(!loaded.selections.runtime_authorized);
        assert!(!loaded.selections.privacy_usage_stats);
        assert!(!loaded.selections.privacy_error_reports);
        assert!(!loaded.selections.privacy_personalization);
        assert!(!loaded.selections.privacy_marketing);
    }

    #[test]
    fn load_state_tolerates_an_older_schema_missing_the_theme_field() {
        // A state file written by a binary from BEFORE the `Theme` step
        // existed — this round's own version of the same forward-compat
        // contract `load_state_tolerates_missing_new_fields_from_an_older_
        // schema` above already proves for round 2's fields: "舊檔無此欄不
        // 得重置流程". `current_step` names the step by STRING (`"templates"`),
        // never an array position, so this loads correctly even though
        // `Theme`'s insertion shifted `Finish` from index 8 to index 9 (see
        // `slug_and_from_debug_env_round_trip_for_every_step` for the
        // general guarantee this relies on).
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-pretheme-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{
            "completed": false,
            "current_step": "templates",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": true,
                "runtime_deferred": false,
                "runtime_authorized": true,
                "privacy_usage_stats": false,
                "privacy_error_reports": false,
                "privacy_personalization": false,
                "privacy_marketing": false,
                "template_choice": null
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::Templates, "must NOT fall back to step 0 just because `theme` is missing");
        assert!(!loaded.completed);
        assert!(loaded.selections.runtime_authorized);
        assert_eq!(loaded.selections.theme, ThemeChoice::Light, "a missing `theme` key must default in, not error the whole file out");
    }

    #[test]
    fn load_state_tolerates_an_older_schema_missing_the_operator_name_field() {
        // Same forward-compat contract as the `theme` test just above,
        // pinned for Shell-S2 round 1's own new field: a state file written
        // before `operator_name` existed must still load, with the missing
        // key defaulting to `None` rather than erroring the whole file out.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-preopname-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{
            "completed": false,
            "current_step": "account-create",
            "selections": {
                "language": "zh-tw",
                "network_connected": true,
                "network_ssid": "DuDu-Office",
                "account_created": false,
                "runtime_deferred": false,
                "theme": "light"
            }
        }"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::AccountCreate, "must NOT fall back to step 0 just because `operator_name` is missing");
        assert!(!loaded.completed);
        assert_eq!(loaded.selections.operator_name, None, "a missing `operator_name` key must default to None, not error the whole file out");
    }

    #[test]
    fn load_state_tolerates_an_old_file_whose_current_step_was_the_prior_first_step() {
        // Task brief item 1's own explicit compat requirement: a state file
        // written by a binary from BEFORE this round's reorder (§B-1's
        // literal row order, `InputDetection` at index 0) has to keep
        // loading correctly under the NEW order. `#[serde(rename_all =
        // "kebab-case")]` on `OobeStep` serializes/deserializes by VARIANT
        // NAME ("input-detection"), never by array position — so this was
        // never actually at risk from the reorder itself, but the task
        // brief asks for it proven, not assumed, and a future contributor
        // reading this test file should be able to see the reorder didn't
        // silently break resume — see `oobe/mod.rs`'s header comment for
        // the reorder rationale.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-oldfirststep-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let old_schema = r#"{"completed":false,"current_step":"input-detection","selections":{}}"#;
        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), old_schema).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded.current_step, OobeStep::InputDetection, "must still deserialize to InputDetection by name");
        assert_eq!(loaded.current_step.index(), 1, "and InputDetection now lands at the new order's second position (index 1)");
        assert!(!loaded.completed);

        // The boot-entry resolver must resume there too, not silently reset
        // to the new step 0.
        let flow = resolve_boot_flow(None, None, None, loaded).expect("not completed, so OOBE must reopen");
        assert_eq!(flow.current(), OobeStep::InputDetection);
    }

    #[test]
    fn load_state_still_fails_open_on_corrupt_json() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-corrupt-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), "{ not valid json at all").unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded, OobeState::default(), "corrupt JSON must still fail-open to the full default");
    }

    #[test]
    fn load_state_falls_back_to_default_on_an_unrecognized_current_step() {
        // Task brief: "current_step 未知值（未來新步）→ 整檔回預設可接受" —
        // a NEWER binary's step name this OLDER binary doesn't know is a
        // genuine enum-variant deserialize failure (not a missing-field
        // case `#[serde(default)]` can paper over), so the whole file
        // legitimately fails to parse and `load_state()`'s own fail-open
        // lands on the FULL default, current_step included.
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("duduclaw-shell-oobe-test-unknownstep-{}", std::process::id()));
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", &tmp) };

        let dir = tmp.join("shell");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("oobe_state.json"), r#"{"completed":false,"current_step":"some-future-step","selections":{}}"#).unwrap();

        let loaded = load_state();

        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(loaded, OobeState::default());
    }

    #[test]
    fn state_path_lands_under_the_shell_subdirectory() {
        let _g = ENV_LOCK.lock().unwrap();
        let prev = std::env::var("DUDUCLAW_HOME").ok();
        unsafe { std::env::set_var("DUDUCLAW_HOME", "/tmp/duduclaw-shell-oobe-path-test") };
        let path = state_path();
        unsafe {
            match prev {
                Some(v) => std::env::set_var("DUDUCLAW_HOME", v),
                None => std::env::remove_var("DUDUCLAW_HOME"),
            }
        }
        assert_eq!(path, Some(PathBuf::from("/tmp/duduclaw-shell-oobe-path-test/shell/oobe_state.json")));
    }

    // ── resolve_boot_flow ─────────────────────────────────────────────

    #[test]
    fn boot_flow_defaults_to_oobe_when_state_is_not_completed() {
        let flow = resolve_boot_flow(None, None, None, OobeState::default());
        assert!(flow.is_some());
        assert_eq!(flow.unwrap().current(), OobeStep::LanguageAccessibility);
    }

    #[test]
    fn boot_flow_goes_to_home_when_state_is_completed() {
        let state = OobeState { completed: true, ..OobeState::default() };
        assert!(resolve_boot_flow(None, None, None, state).is_none());
    }

    #[test]
    fn force_oobe_wins_even_when_persisted_state_is_completed() {
        let state = OobeState { completed: true, ..OobeState::default() };
        let flow = resolve_boot_flow(Some("1"), None, None, state);
        assert!(flow.is_some());
    }

    #[test]
    fn force_oobe_beats_skip_oobe_when_both_are_set() {
        // Task brief: "兩者都設時 FORCE 優先".
        let flow = resolve_boot_flow(Some("1"), Some("1"), None, OobeState::default());
        assert!(flow.is_some());
    }

    #[test]
    fn skip_oobe_wins_over_an_incomplete_persisted_state() {
        let flow = resolve_boot_flow(None, Some("1"), None, OobeState::default());
        assert!(flow.is_none());
    }

    #[test]
    fn debug_step_opens_oobe_directly_at_that_step_even_if_completed() {
        let state = OobeState { completed: true, current_step: OobeStep::Finish, ..OobeState::default() };
        let flow = resolve_boot_flow(None, None, Some("network"), state);
        let flow = flow.expect("debug step must force OOBE open");
        assert_eq!(flow.current(), OobeStep::Network);
        assert!(!flow.completed());
    }

    #[test]
    fn debug_step_with_unrecognized_value_falls_back_to_normal_resolution() {
        let state = OobeState { completed: true, ..OobeState::default() };
        let flow = resolve_boot_flow(None, None, Some("bogus"), state);
        assert!(flow.is_none(), "unrecognized debug step should not override a completed state");
    }

    #[test]
    fn skip_oobe_beats_an_unrecognized_debug_step() {
        // Priority ordering: FORCE > SKIP > DEBUG_STEP > persisted state.
        let flow = resolve_boot_flow(None, Some("1"), Some("network"), OobeState::default());
        assert!(flow.is_none());
    }

    #[test]
    fn debug_step_preserves_earlier_persisted_selections() {
        let mut state = OobeState::default();
        state.selections.language = LanguageChoice::En;
        let flow = resolve_boot_flow(None, None, Some("finish"), state).unwrap();
        assert_eq!(flow.current(), OobeStep::Finish);
        assert_eq!(flow.selections().language, LanguageChoice::En);
    }

    // ── boot_theme — Shell-S1 (Home/overlay dark theme) ────────

    #[test]
    fn boot_theme_defaults_to_light_on_a_fresh_install() {
        assert_eq!(boot_theme(&OobeState::default()), ThemeChoice::Light);
    }

    #[test]
    fn boot_theme_reads_the_persisted_selection_even_when_oobe_is_skipped() {
        // The whole point of splitting `boot_theme` out of
        // `resolve_boot_flow` (see that function's own doc comment): a
        // returning operator's boot path resolves `resolve_boot_flow` to
        // `None` (OOBE doesn't render), but Home still needs the theme they
        // picked. Both are asserted here from the SAME state value to prove
        // neither reading interferes with the other.
        let mut state = OobeState { completed: true, ..OobeState::default() };
        state.selections.theme = ThemeChoice::Dark;
        assert_eq!(boot_theme(&state), ThemeChoice::Dark);
        assert_eq!(resolve_boot_flow(None, None, None, state), None, "a completed state must still resolve to Home");
    }

    #[test]
    fn boot_theme_reads_the_persisted_selection_when_oobe_is_still_in_progress() {
        // The other boot path: OOBE itself resolves to `Some`, but a
        // mid-flow operator may already have visited the Theme step and
        // backed out (or the state was seeded pre-completion) — `boot_theme`
        // must not depend on `resolve_boot_flow`'s own `Option` shape at all.
        let mut state = OobeState::default();
        state.selections.theme = ThemeChoice::Dark;
        assert_eq!(boot_theme(&state), ThemeChoice::Dark);
        assert!(resolve_boot_flow(None, None, None, state).is_some());
    }
}
