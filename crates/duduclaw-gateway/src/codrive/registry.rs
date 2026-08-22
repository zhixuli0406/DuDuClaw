//! CD-4 (WP-CD4a): C-L2 app registry — the second rung of the "型別化優先、
//! GUI 最後" execution ladder (design §3.2). Before a step's ordinary
//! coordinate-based `action` (C-L1) is dispatched over the comp socket, the
//! driver checks whether the step declared an [`super::script::ApiActionRequest`]
//! that a registered third-party app can serve directly via its own native
//! CLI or D-Bus surface — semantic equivalence always wins over clicking
//! around a GUI.
//!
//! ## Decorator shape (UFO²)
//!
//! One [`AppEntry`] binds an `app_id` (plus a few free-text aliases the
//! script's `target_app` field is matched against, whole-word
//! case-insensitive via [`duduclaw_core::word_contains_ci`]) to a fixed set
//! of named [`ActionEntry`]s. Each action carries a `params_schema`
//! (informational — the param names it accepts) and an [`Exec`]: either a
//! `Cli` argv template or a `DBus` method call. [`APP_REGISTRY`] is the
//! whole registry, in-repo and static — no file, no hot reload (a missing
//! entry is simply a MISS, same "absent config ⇒ default behavior" spirit
//! [`super::config::CodriveConfig::from_home`] already uses).
//!
//! ## Wave-1 apps
//!
//! - **Chromium** (`Cli`): `open_window` / `open_url` — CLI-launch only.
//!   Deliberately does NOT touch the DevTools Protocol — that surface
//!   already belongs to this platform's separate browser L1-L5 ladder
//!   (`browser_router.rs`); duplicating it here would be the same
//!   capability implemented twice, not a new one (see DESIGN §3.2's own
//!   "已核定界線" note this module's WP brief carries forward).
//! - **NetworkManager** (`DBus`): `connectivity` / `state` — both
//!   zero-argument, READ-ONLY queries (`CheckConnectivity()` / `state()` on
//!   `org.freedesktop.NetworkManager`, confirmed against the upstream D-Bus
//!   API spec before writing this). Wave-1 deliberately sticks to read-only
//!   actions — an action that actually changed a network connection would
//!   affect whatever host runs the test suite, which a registry-wiring PoC
//!   has no business doing.
//!
//! ## Why the generic `zbus::Proxy`, not the `#[zbus::proxy]` macro
//!
//! Same reasoning `duduclaw-shell/src/oobe/network/nm.rs` already gives for
//! its own (blocking) D-Bus client, copied forward because it applies with
//! equal force here: this crate's own dev loop (macOS) has no D-Bus/
//! NetworkManager to run [`execute_dbus`] against at all, so its
//! correctness rests on the Linux cross-build plus direct reading of the
//! NetworkManager D-Bus spec — never on locally exercising it. Minimizing
//! how much of that correctness depends on trusting a macro's code
//! generation (rather than a plain `Proxy::new` + `.call()` whose types are
//! spelled out at the call site) was judged the safer trade under that
//! constraint. Unlike that module, this one is async-native (the gateway
//! already runs on tokio), so it uses `zbus::Connection`/`zbus::Proxy`
//! directly rather than the `blocking` facade.
//!
//! ## Dispatch outcome contract
//!
//! [`dispatch`] never panics and always resolves to one of three states —
//! [`DispatchOutcome::Miss`] (no registry entry for `(target_app, action)`),
//! [`DispatchOutcome::Executed`] (ran, succeeded), or
//! [`DispatchOutcome::Failed`] (ran, errored) — the caller
//! (`step::try_registry_action`) treats Miss and Failed identically: fall
//! back to the step's own C-L1 coordinate `action`, unchanged. This module
//! never touches [`super::client::CodriveClient`] at all — a successful C-L2
//! dispatch is therefore structurally incapable of sending anything to comp,
//! not just observed-to-not in a particular test run.
//!
//! ## Known gaps (honest, not hidden)
//!
//! - `params_schema` is a declared name list, not full JSON-Schema
//!   validation — a param present with the wrong JSON type still degrades
//!   to a normal `Failed` (missing-param) outcome, never a panic, but it is
//!   not schema-checked ahead of the CLI-render step.
//! - Chromium's binary name is hardcoded to `"chromium"` (resolved via
//!   `PATH`, exactly as the WP brief's `chromium --new-window <url>`
//!   spells it) — no `find_chromium()`-style multi-name locator
//!   (`chromium-browser` / `google-chrome` / …) the way
//!   `files_api::find_soffice` has for LibreOffice. A reasonable follow-up,
//!   out of scope for this wave-1 PoC.
//! - `DBusReply` only has a `U32` variant (exactly what both wave-1 NM
//!   actions return) — not a general D-Bus reply decoder. Extend when a
//!   second reply shape is actually needed.

use std::time::Duration;

use serde_json::Value;

use super::script::ApiActionRequest;

/// Per-param string length cap for a `Cli` action's rendered argv — mirrors
/// the spirit of `script::MAX_CONSEQUENTIAL_DESC_CHARS` (a bound, not a
/// silent truncation, since truncating an argv value — e.g. a URL — would
/// change its meaning rather than just its display length).
const MAX_PARAM_CHARS: usize = 2000;
/// Wall-clock ceiling for one `Cli`/`DBus` exec attempt — short, because a
/// registry action stands in for what would otherwise be a handful of GUI
/// clicks; it must fail fast into the C-L1 fallback, not stall the whole
/// script.
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);

/// One registered app's C-L2 surface.
pub struct AppEntry {
    /// Canonical id, stamped into the audit trail on a hit.
    pub app_id: &'static str,
    /// Additional free-text tokens `CodriveScript::target_app` is also
    /// matched against (whole-word, case-insensitive) — e.g. Chromium is
    /// commonly named "chrome" by callers even though its `app_id` here is
    /// "chromium". `app_id` itself is always an implicit alias.
    pub aliases: &'static [&'static str],
    pub actions: &'static [ActionEntry],
}

/// One named, callable action on an [`AppEntry`] (UFO² "decorator" shape:
/// name + params schema + how to execute it).
pub struct ActionEntry {
    pub name: &'static str,
    /// Declared param names this action's [`Exec::Cli`] template consumes
    /// (informational/documentation surface — see module doc's "known
    /// gaps"). Empty for a zero-param action.
    pub params_schema: &'static [&'static str],
    pub exec: Exec,
}

/// How a registered action is actually carried out. `Copy` (every field is
/// a `'static` reference or a small `Copy` enum) so `dispatch_in` can match
/// on `action.exec` by value through the shared `&ActionEntry` it holds,
/// instead of fighting double-reference match ergonomics through `&Exec`.
#[derive(Clone, Copy)]
pub enum Exec {
    /// Spawn `argv_template[0]` with `argv_template[1..]` as literal
    /// arguments — each element passed individually via
    /// `tokio::process::Command::arg` (never through a shell; coding
    /// convention: no string-concatenated shell invocation anywhere in this
    /// codebase). A `{param_name}` element is substituted from the
    /// request's `params` object at dispatch time (see [`render_argv`]);
    /// every other element is sent byte-for-byte as written here.
    Cli { argv_template: &'static [&'static str] },
    /// Call a D-Bus method with zero arguments and decode its reply per
    /// `reply`. Linux-only ([`execute_dbus`] is `cfg(target_os = "linux")`;
    /// every other target's [`dispatch`] treats every `DBus` action as an
    /// honest `Failed` — "not supported on this platform" — which falls
    /// back to C-L1 exactly like any other exec failure).
    DBus {
        bus: DBusBus,
        /// The service's well-known bus name (D-Bus `destination`) — the
        /// terse WP brief's decorator shape names only `{bus, path, iface,
        /// method}`, but the D-Bus wire protocol itself requires a
        /// destination to route the call; for both wave-1 actions this
        /// happens to equal `iface` (NetworkManager's own convention of a
        /// service name matching its main interface name), spelled out
        /// explicitly here rather than silently reusing `iface` for two
        /// different roles.
        destination: &'static str,
        path: &'static str,
        iface: &'static str,
        method: &'static str,
        reply: DBusReply,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum DBusBus {
    System,
    #[allow(dead_code)] // no wave-1 action uses the session bus yet
    Session,
}

/// How to decode a `DBus` action's zero-argument method reply. Deliberately
/// narrow (see module doc's "known gaps") — extend as new actions need a
/// different reply shape.
#[derive(Debug, Clone, Copy)]
pub enum DBusReply {
    U32,
}

// ── Wave-1 registry ──────────────────────────────────────────────────────

const CHROMIUM: AppEntry = AppEntry {
    app_id: "chromium",
    aliases: &["chrome", "google-chrome", "google chrome"],
    actions: &[
        ActionEntry {
            name: "open_window",
            params_schema: &[],
            exec: Exec::Cli { argv_template: &["chromium", "--new-window"] },
        },
        ActionEntry {
            name: "open_url",
            params_schema: &["url"],
            exec: Exec::Cli { argv_template: &["chromium", "--new-window", "{url}"] },
        },
    ],
};

const NETWORK_MANAGER: AppEntry = AppEntry {
    app_id: "networkmanager",
    aliases: &["network-manager", "network manager", "nm"],
    actions: &[
        ActionEntry {
            name: "connectivity",
            params_schema: &[],
            // org.freedesktop.NetworkManager.CheckConnectivity() -> (u connectivity)
            // — re-checks and returns NMConnectivityState. Confirmed against
            // the upstream NetworkManager D-Bus API reference before wiring
            // this in (see module doc).
            exec: Exec::DBus {
                bus: DBusBus::System,
                destination: "org.freedesktop.NetworkManager",
                path: "/org/freedesktop/NetworkManager",
                iface: "org.freedesktop.NetworkManager",
                method: "CheckConnectivity",
                reply: DBusReply::U32,
            },
        },
        ActionEntry {
            name: "state",
            params_schema: &[],
            // org.freedesktop.NetworkManager.state() -> (u state) — the
            // legacy zero-arg method mirroring the `State` property, kept
            // for compatibility by NetworkManager itself; confirmed present
            // against the upstream spec (see module doc).
            exec: Exec::DBus {
                bus: DBusBus::System,
                destination: "org.freedesktop.NetworkManager",
                path: "/org/freedesktop/NetworkManager",
                iface: "org.freedesktop.NetworkManager",
                method: "state",
                reply: DBusReply::U32,
            },
        },
    ],
};

/// The production registry. `cfg(test)` appends a harmless, filesystem-only
/// fixture app ([`TEST_FIXTURE`]) so the fake-comp integration suite
/// (`tests_registry.rs`) can exercise the FULL `run_script` → `step` →
/// `registry::dispatch` path end to end — including the "a hit sends zero
/// comp events" claim — without depending on Chromium or a live
/// NetworkManager/D-Bus session being present in whatever environment
/// `cargo test` happens to run in. The production binary never sees this
/// extra entry; `TEST_FIXTURE` is defined `#[cfg(test)]` itself, so it does
/// not exist in a release build even as dead code.
#[cfg(not(test))]
pub static APP_REGISTRY: &[AppEntry] = &[CHROMIUM, NETWORK_MANAGER];

#[cfg(test)]
pub static APP_REGISTRY: &[AppEntry] = &[CHROMIUM, NETWORK_MANAGER, TEST_FIXTURE];

/// Test-only fixture app — see [`APP_REGISTRY`]'s `cfg(test)` doc. `touch`
/// exists at this path on every unix `cargo test` runs on in this repo
/// (macOS dev loop + Linux CI); the whole fixture (and every test that
/// exercises it) is itself declared `#[cfg(all(test, unix))]` at the module
/// level in `tests_registry.rs`, matching `tests.rs`'s own unix-only
/// fake-comp harness gating.
#[cfg(test)]
const TEST_FIXTURE: AppEntry = AppEntry {
    app_id: "codrive-test-fixture",
    aliases: &[],
    actions: &[ActionEntry {
        name: "touch_file",
        params_schema: &["path"],
        exec: Exec::Cli { argv_template: &["/usr/bin/touch", "{path}"] },
    }],
};

// ── Dispatch ─────────────────────────────────────────────────────────────

/// Outcome of one [`dispatch`] call. See module doc's "dispatch outcome
/// contract".
#[derive(Debug)]
pub enum DispatchOutcome {
    Executed { detail: String },
    Miss,
    Failed { detail: String },
}

/// Look up and run `req` against `target_app` in the production
/// [`APP_REGISTRY`]. Thin wrapper over [`dispatch_in`] so production call
/// sites never have to name the registry explicitly.
pub async fn dispatch(target_app: &str, req: &ApiActionRequest) -> DispatchOutcome {
    dispatch_in(APP_REGISTRY, target_app, req).await
}

/// Core dispatch, parameterized over the registry so it's directly
/// unit-testable with a small ad-hoc `AppEntry` slice (see `tests_registry.rs`)
/// independent of whichever apps happen to be in [`APP_REGISTRY`].
async fn dispatch_in(registry: &[AppEntry], target_app: &str, req: &ApiActionRequest) -> DispatchOutcome {
    let Some(action) = find_action(registry, target_app, &req.action) else {
        return DispatchOutcome::Miss;
    };
    let result = match action.exec {
        Exec::Cli { argv_template } => execute_cli(argv_template, &req.params).await,
        Exec::DBus { bus, destination, path, iface, method, reply } => {
            execute_dbus(bus, destination, path, iface, method, reply).await
        }
    };
    match result {
        Ok(detail) => DispatchOutcome::Executed { detail },
        Err(detail) => DispatchOutcome::Failed { detail },
    }
}

fn find_action<'a>(registry: &'a [AppEntry], target_app: &str, action_name: &str) -> Option<&'a ActionEntry> {
    let app = registry.iter().find(|app| {
        duduclaw_core::word_contains_ci(target_app, app.app_id)
            || app.aliases.iter().any(|alias| duduclaw_core::word_contains_ci(target_app, alias))
    })?;
    app.actions.iter().find(|a| a.name == action_name)
}

/// Substitute `{param_name}` argv-template elements from `params` (must be a
/// JSON object with string values for every referenced name); every other
/// element is passed through byte-for-byte. Every substituted value is
/// still handed to `tokio::process::Command::arg` individually by the
/// caller — this function only decides WHAT string goes in each argv slot,
/// never how it reaches the child process (coding convention: argv, never a
/// shell).
fn render_argv(template: &[&'static str], params: &Value) -> Result<Vec<String>, String> {
    let obj = params.as_object();
    let mut out = Vec::with_capacity(template.len());
    for tok in template {
        match tok.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(key) => {
                let val = obj
                    .and_then(|o| o.get(key))
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| format!("missing or non-string param '{key}'"))?;
                if val.chars().count() > MAX_PARAM_CHARS {
                    return Err(format!("param '{key}' exceeds {MAX_PARAM_CHARS} chars"));
                }
                if val.contains('\0') {
                    return Err(format!("param '{key}' contains a NUL byte"));
                }
                out.push(val.to_string());
            }
            None => out.push((*tok).to_string()),
        }
    }
    Ok(out)
}

async fn execute_cli(argv_template: &'static [&'static str], params: &Value) -> Result<String, String> {
    if argv_template.is_empty() {
        // Defensive, not load-bearing: every real registry entry has a
        // non-empty template — see this module's own const definitions.
        return Err("empty argv_template".to_string());
    }
    let argv = render_argv(argv_template, params)?;
    let program = argv[0].clone();

    let mut cmd = tokio::process::Command::new(&program);
    cmd.args(&argv[1..]);
    duduclaw_core::apply_agent_cli_env_allowlist(&mut cmd);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    // If EXEC_TIMEOUT elapses, the `tokio::time::timeout` future is simply
    // dropped — without this, the child (e.g. a GUI app that doesn't exit
    // on its own) would keep running as an orphan.
    cmd.kill_on_drop(true);

    let output = tokio::time::timeout(EXEC_TIMEOUT, cmd.output())
        .await
        .map_err(|_| format!("timed out after {EXEC_TIMEOUT:?}"))?
        .map_err(|e| format!("spawn failed for '{program}': {e}"))?;

    if !output.status.success() {
        let stderr_tail = duduclaw_core::truncate_chars(&String::from_utf8_lossy(&output.stderr), 200);
        return Err(format!("'{program}' exited with {}: {stderr_tail}", output.status));
    }
    Ok(format!("cli exec ok: {program} exited {}", output.status))
}

#[cfg(target_os = "linux")]
async fn execute_dbus(
    bus: DBusBus,
    destination: &'static str,
    path: &'static str,
    iface: &'static str,
    method: &'static str,
    reply: DBusReply,
) -> Result<String, String> {
    let connection = match bus {
        DBusBus::System => zbus::Connection::system().await,
        DBusBus::Session => zbus::Connection::session().await,
    }
    .map_err(|e| format!("dbus connect failed: {e}"))?;

    let proxy = zbus::Proxy::new(&connection, destination, path, iface)
        .await
        .map_err(|e| format!("dbus proxy build failed: {e}"))?;

    match reply {
        DBusReply::U32 => {
            let value: u32 = proxy
                .call(method, &())
                .await
                .map_err(|e| format!("dbus call {iface}.{method} failed: {e}"))?;
            Ok(format!("dbus exec ok: {iface}.{method} -> {value}"))
        }
    }
}

/// Every other target: this feature only ever runs on the Linux appliance
/// image (comp itself is Linux-only — see `client.rs`'s own non-unix stub),
/// so a `DBus` action here is an honest, immediate "not supported on this
/// platform" — never a silent no-op — which the caller treats exactly like
/// any other exec failure and falls back to the step's C-L1 coordinate path.
#[cfg(not(target_os = "linux"))]
async fn execute_dbus(
    _bus: DBusBus,
    _destination: &'static str,
    _path: &'static str,
    iface: &'static str,
    method: &'static str,
    _reply: DBusReply,
) -> Result<String, String> {
    Err(format!(
        "codrive C-L2 D-Bus actions ({iface}.{method}) are only supported on the Linux appliance image"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(action: &str, params: Value) -> ApiActionRequest {
        ApiActionRequest { action: action.to_string(), params }
    }

    // ── find_action / aliasing ──────────────────────────────────────────

    #[test]
    fn finds_by_canonical_app_id() {
        let entry = find_action(APP_REGISTRY, "chromium", "open_window");
        assert!(entry.is_some());
    }

    #[test]
    fn finds_by_alias_whole_word() {
        let entry = find_action(APP_REGISTRY, "chrome", "open_window");
        assert!(entry.is_some(), "the 'chrome' alias must resolve to the chromium entry");
    }

    #[test]
    fn alias_match_is_whole_word_not_substring() {
        // "chromebook" contains "chrome" as a substring but not as a whole
        // word — word_contains_ci must not false-positive here.
        assert!(find_action(APP_REGISTRY, "chromebook-launcher", "open_window").is_none());
    }

    #[test]
    fn unknown_app_is_miss() {
        assert!(find_action(APP_REGISTRY, "some-random-app", "open_window").is_none());
    }

    #[test]
    fn unknown_action_on_known_app_is_miss() {
        assert!(find_action(APP_REGISTRY, "chromium", "close_all_tabs").is_none());
    }

    #[test]
    fn networkmanager_read_only_actions_present() {
        assert!(find_action(APP_REGISTRY, "networkmanager", "connectivity").is_some());
        assert!(find_action(APP_REGISTRY, "networkmanager", "state").is_some());
        assert!(find_action(APP_REGISTRY, "nm", "state").is_some(), "the 'nm' alias must resolve");
    }

    // ── render_argv ──────────────────────────────────────────────────────

    #[test]
    fn render_argv_no_placeholders() {
        let argv = render_argv(&["chromium", "--new-window"], &Value::Null).unwrap();
        assert_eq!(argv, vec!["chromium", "--new-window"]);
    }

    #[test]
    fn render_argv_substitutes_placeholder() {
        let argv = render_argv(
            &["chromium", "--new-window", "{url}"],
            &serde_json::json!({"url": "https://example.com"}),
        )
        .unwrap();
        assert_eq!(argv, vec!["chromium", "--new-window", "https://example.com"]);
    }

    #[test]
    fn render_argv_missing_param_errs() {
        let err = render_argv(&["chromium", "--new-window", "{url}"], &Value::Null).unwrap_err();
        assert!(err.contains("url"));
    }

    #[test]
    fn render_argv_non_string_param_errs() {
        let err = render_argv(
            &["chromium", "--new-window", "{url}"],
            &serde_json::json!({"url": 123}),
        )
        .unwrap_err();
        assert!(err.contains("url"));
    }

    #[test]
    fn render_argv_never_shells_out_special_chars() {
        // The whole point: a value containing shell metacharacters is
        // passed through as ONE literal argv element, never interpreted.
        let argv = render_argv(
            &["chromium", "--new-window", "{url}"],
            &serde_json::json!({"url": "https://x.test/?a=1;rm -rf ~ && echo pwned"}),
        )
        .unwrap();
        assert_eq!(argv[2], "https://x.test/?a=1;rm -rf ~ && echo pwned");
    }

    #[test]
    fn render_argv_oversized_param_rejected() {
        let big = "a".repeat(MAX_PARAM_CHARS + 1);
        let err = render_argv(&["chromium", "{url}"], &serde_json::json!({"url": big})).unwrap_err();
        assert!(err.contains("exceeds"));
    }

    // ── dispatch_in — Miss ───────────────────────────────────────────────

    #[tokio::test]
    async fn dispatch_miss_for_unregistered_app() {
        let outcome = dispatch_in(APP_REGISTRY, "totally-unknown-app", &req("anything", Value::Null)).await;
        assert!(matches!(outcome, DispatchOutcome::Miss));
    }

    #[tokio::test]
    async fn dispatch_miss_for_unregistered_action() {
        let outcome = dispatch_in(APP_REGISTRY, "chromium", &req("does_not_exist", Value::Null)).await;
        assert!(matches!(outcome, DispatchOutcome::Miss));
    }

    // ── dispatch_in — Cli exec, real subprocess (unix; see TEST_FIXTURE) ──

    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_cli_hit_executes_for_real_and_touches_the_file() {
        let dir = std::env::temp_dir().join(format!("codrive-registry-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("touched").to_string_lossy().to_string();
        assert!(!std::path::Path::new(&target).exists());

        let outcome = dispatch_in(
            APP_REGISTRY,
            "codrive-test-fixture",
            &req("touch_file", serde_json::json!({"path": target})),
        )
        .await;

        assert!(matches!(outcome, DispatchOutcome::Executed { .. }), "{outcome:?}");
        assert!(std::path::Path::new(&target).exists(), "the touch side effect must be real, not simulated");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_cli_missing_param_is_failed_not_panic() {
        let outcome = dispatch_in(APP_REGISTRY, "codrive-test-fixture", &req("touch_file", Value::Null)).await;
        assert!(matches!(outcome, DispatchOutcome::Failed { .. }), "{outcome:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_cli_nonexistent_binary_is_failed_not_panic() {
        // Deliberately NOT the real `chromium` registry entry: on a machine
        // that happens to have a real Chromium/Chrome on PATH, actually
        // spawning it from an automated test would pop a visible GUI
        // window (or worse, hang until `EXEC_TIMEOUT` if the browser
        // process doesn't exit on its own) — exactly what an automated
        // suite must never risk. A local ad-hoc registry pointed at a path
        // that cannot possibly exist proves the same "spawn error degrades
        // to Failed, never a panic" contract hermetically.
        const FIXTURE: AppEntry = AppEntry {
            app_id: "definitely-nonexistent-app",
            aliases: &[],
            actions: &[ActionEntry {
                name: "noop",
                params_schema: &[],
                exec: Exec::Cli { argv_template: &["/definitely/not/a/real/binary-xyz123"] },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "definitely-nonexistent-app", &req("noop", Value::Null)).await;
        assert!(matches!(outcome, DispatchOutcome::Failed { .. }), "{outcome:?}");
    }

    // ── DBus exec, non-Linux honest refusal ─────────────────────────────

    #[cfg(not(target_os = "linux"))]
    #[tokio::test]
    async fn dispatch_dbus_on_non_linux_is_honest_failed_not_silent_noop() {
        let outcome = dispatch_in(APP_REGISTRY, "networkmanager", &req("state", Value::Null)).await;
        match outcome {
            DispatchOutcome::Failed { detail } => {
                assert!(detail.contains("Linux"), "detail should explain the platform gap: {detail}");
            }
            other => panic!("expected Failed on non-Linux, got {other:?}"),
        }
    }
}
