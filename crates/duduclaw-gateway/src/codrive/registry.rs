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
//! ## Two kinds of `Cli` action: [`WaitMode`]
//!
//! A registered `Cli` action is one of two structurally different things,
//! and conflating them was a real bug (found live on the CD-4c VM rig, see
//! [`WaitMode`]'s own doc): a **query/one-shot** tool that runs and exits
//! (`touch`, `nmcli`, …) versus a **launcher** for a long-lived GUI process
//! that by design never exits on its own (`chromium --new-window`). The
//! first is judged by its exit status; the second can only be judged by
//! "did it still exist a moment later". [`Exec::Cli`] therefore carries an
//! explicit [`WaitMode`] and [`execute_cli`] has two branches — see
//! [`WaitMode`] for the full reasoning and the detach branch's hard
//! requirements (no `kill_on_drop`, no pipes, liveness grace, explicit
//! reap).
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
//! - A [`WaitMode::Detach`] action keeps only the FIRST
//!   [`DETACH_STDERR_CAPTURE_BYTES`] of its child's stderr, not the last —
//!   so a launcher that prints a long warning preamble and only then fails
//!   can push its real error past the window. Head-not-tail is what the
//!   `ForExit` branch has always done (`truncate_chars(stderr, 200)`) and
//!   it matches the failures actually seen in the field, which are all
//!   startup-time (`Missing X server or $DISPLAY`,
//!   `XDG_RUNTIME_DIR is invalid or not set`); keeping a rolling tail
//!   instead would need an unbounded-until-exit buffer or a ring, neither
//!   of which this has earned yet.

use std::time::Duration;

use serde_json::Value;

use super::script::ApiActionRequest;

/// Per-param string length cap for a `Cli` action's rendered argv — mirrors
/// the spirit of `script::MAX_CONSEQUENTIAL_DESC_CHARS` (a bound, not a
/// silent truncation, since truncating an argv value — e.g. a URL — would
/// change its meaning rather than just its display length).
const MAX_PARAM_CHARS: usize = 2000;
/// Wall-clock ceiling for one [`WaitMode::ForExit`] `Cli` attempt or one
/// `DBus` exec attempt — short, because a registry action stands in for
/// what would otherwise be a handful of GUI clicks; it must fail fast into
/// the C-L1 fallback, not stall the whole script. Deliberately NOT applied
/// to [`WaitMode::Detach`], whose whole contract is "never wait for exit"
/// (its bound is its own `liveness_ms` grace period instead).
const EXEC_TIMEOUT: Duration = Duration::from_secs(10);

/// Liveness grace period used by every production [`WaitMode::Detach`]
/// action (wave-1: both Chromium launches).
///
/// 800ms, at the long end of the "a few hundred ms" band, chosen from the
/// two failure modes this grace actually has to separate:
///
/// - **Too short** ⇒ an immediate, deterministic launch failure (bad flag,
///   no display, profile lock held, GPU/sandbox refusal) is still "alive"
///   when we look, gets reported as success, and the C-L1 fallback never
///   fires — which is the exact class of silent-success bug this whole
///   change exists to remove, just mirrored. The live CD-4c round's own
///   `Missing X server or $DISPLAY` exit is an Ozone-platform-init failure,
///   i.e. one of the *slower* early exits Chromium has (it happens after
///   process/zygote startup), and the target is a modest VM — a 300ms
///   window would be cutting into that margin.
/// - **Too long** ⇒ pure added latency on the success path.
///
/// The asymmetry is what settles it: overshooting costs at most 800ms on a
/// step that stands in for several seconds of GUI clicking, while
/// undershooting costs correctness. It is a per-action field rather than a
/// hard-coded constant inside [`execute_cli`] precisely so a future entry
/// with a genuinely faster or slower launcher can say so.
const DETACH_LIVENESS_MS: u64 = 800;

/// How many bytes of a [`WaitMode::Detach`] child's stderr are kept for the
/// failure detail. 4KiB — comfortably more than the multi-line startup
/// errors this is here to surface (the CD-4c round's `Missing X server or
/// $DISPLAY` and `XDG_RUNTIME_DIR is invalid or not set` are both a single
/// line), small enough that the buffer is irrelevant next to a browser
/// launch, and in any case only the first 200 CHARS of it reach the audit
/// row (same `truncate_chars` budget the `ForExit` branch uses). The drain
/// keeps reading past this cap and discards — the cap bounds MEMORY, never
/// how much is read, because a pipe that stops being read is a hung child
/// (see [`WaitMode`] requirement 2).
const DETACH_STDERR_CAPTURE_BYTES: usize = 4096;

/// Ceiling on how long the detach FAILURE path waits for the bounded stderr
/// drain to finish. On that path the child has already exited, so its write
/// end is closed and the drain reaches EOF almost immediately; this bound
/// exists only for the case where a grandchild inherited the pipe and holds
/// it open — there, reporting the failure with no stderr tail beats
/// stalling the script, so the wait degrades rather than blocks. The
/// still-running SUCCESS path never waits at all.
const DETACH_STDERR_WAIT: Duration = Duration::from_millis(500);

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
    ///
    /// `wait` decides what "this action succeeded" even means for this
    /// program — see [`WaitMode`]; it is a required field (no `Default`)
    /// so adding a registry entry forces an explicit answer instead of
    /// silently inheriting a mode that may be wrong for it.
    Cli { argv_template: &'static [&'static str], wait: WaitMode },
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

/// What [`execute_cli`] waits for before deciding an [`Exec::Cli`] action
/// succeeded.
///
/// ## Why this exists
///
/// C-L2 originally had one rule — spawn, wait for exit, success iff exit
/// status 0 — which is correct for a one-shot tool and *structurally
/// unable to ever report success* for a launcher. Both production `Cli`
/// entries in this registry are the second kind (`chromium --new-window`),
/// so, before this split, a working Chromium launch and a wedged child
/// were the same observation from the call site: both ran out
/// [`EXEC_TIMEOUT`], both audited `exec_failed_fallback`, both degraded to
/// C-L1 — and `kill_on_drop` then killed the window the successful one had
/// just opened. Confirmed live on the CD-4c VM rig (see
/// `live_tests::live_bridge_cd4c_c_l2_chromium_real_dispatch`); the mode is
/// a property of the *program being run*, not of the caller, so it is
/// declared per action alongside the argv it applies to.
///
/// ## `Detach`'s hard requirements
///
/// Detaching correctly is more than "don't await the child" — each of the
/// following is load-bearing, and dropping any one of them re-introduces a
/// different bug:
///
/// 1. **No `kill_on_drop`.** With it, the child dies the moment the
///    [`tokio::process::Child`] handle drops — i.e. immediately after a
///    successful launch. This is what made a real Chromium window vanish.
/// 2. **A pipe must never stop being read.** A chatty child (Chromium is
///    very chatty on stderr) fills the ~64KB pipe buffer and then blocks
///    forever on its next write — a hang caused entirely by our own
///    bookkeeping. `stdout` is therefore `Stdio::null()` outright: it is
///    the bulk of the volume and carries no diagnostic value here.
///    `stderr` IS piped, because the only two real C-L2 launch bugs found
///    in the field so far were both identified from its text and an exit
///    status alone would have said nothing — but it is piped **only**
///    together with [`drain_stderr_bounded`], spawned immediately, which
///    keeps reading to EOF for the child's whole life and merely stops
///    *retaining* past [`DETACH_STDERR_CAPTURE_BYTES`]. Bounding the
///    buffer is not the same as bounding the reading; conflating the two
///    is exactly how this requirement gets violated.
/// 3. **A liveness grace period, not pure fire-and-forget.** A program
///    that rejects its arguments and exits non-zero in 40ms would
///    otherwise be reported as a success and the C-L1 fallback would never
///    fire — the same silent-success failure as before, just inverted. So
///    the child is given `liveness_ms` and then polled once
///    (non-blocking): still running ⇒ success; already exited ⇒ its exit
///    status decides, exactly like `ForExit`. See [`DETACH_LIVENESS_MS`]
///    for how the production value was picked.
/// 4. **An explicit reap.** A surviving child must not become a zombie
///    when it eventually does exit, so [`execute_cli`] hands the `Child`
///    to a detached `tokio::spawn` whose only job is to `.wait()` on it.
///    (Tokio's own orphan queue would reap a dropped `Child` too, but only
///    on unix and only as an implementation detail of `Drop` — an explicit
///    owner is cheap, portable, and readable, and it keeps the child's
///    lifetime tied to the runtime rather than to a `Drop` side effect.)
#[derive(Debug, Clone, Copy)]
pub enum WaitMode {
    /// Run to completion: success iff the child exits 0 within
    /// [`EXEC_TIMEOUT`]. Correct for one-shot tools that terminate on
    /// their own (`touch`, a CLI query, …) and the default for any new
    /// entry unless it is specifically a launcher.
    ForExit,
    /// Launch and let go: success iff the child is still alive after
    /// `liveness_ms`, or exited 0 within it. Correct for long-lived GUI
    /// processes that never exit on their own. See this enum's doc for the
    /// four requirements this branch has to satisfy.
    Detach { liveness_ms: u64 },
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

// WP-CD4c VM live-fire fix (2026-08-22, minimal/surgical): `--ozone-platform=
// wayland` added to both actions below. Found live on the appliance VM —
// `duduclaw-comp`'s target environment ships no XWayland session for
// Chromium's default X11 Ozone backend to attach to (`Xwayland` the binary
// is present, but nothing starts/manages it as an X server here), so every
// `open_window`/`open_url` call registry-hit-but-exec-failed 100% of the
// time (`ERROR:ui/ozone/platform/x11/ozone_platform_x11.cc:257] Missing X
// server or $DISPLAY`), silently degrading to the C-L1 fallback on every
// single call. Verified live as the actual fix, not a guess: same VM, same
// non-root kiosk user, `chromium --new-window --ozone-platform=wayland
// about:blank` runs to completion (no X11/DISPLAY error) against comp's
// native xdg-shell Wayland socket — `--no-sandbox` is NOT needed for this
// (that was a separate, unrelated failure specific to testing as root,
// which production's non-root `duduclaw` service user never hits).
const CHROMIUM: AppEntry = AppEntry {
    app_id: "chromium",
    aliases: &["chrome", "google-chrome", "google chrome"],
    actions: &[
        // Both actions are `WaitMode::Detach`: `chromium --new-window` is a
        // launcher for a GUI process that stays up until the user closes
        // it, so "exited 0" is not a thing it will ever do on the success
        // path (see `WaitMode`). Everything else about these two entries is
        // unchanged.
        ActionEntry {
            name: "open_window",
            params_schema: &[],
            exec: Exec::Cli {
                argv_template: &["chromium", "--ozone-platform=wayland", "--new-window"],
                wait: WaitMode::Detach { liveness_ms: DETACH_LIVENESS_MS },
            },
        },
        ActionEntry {
            name: "open_url",
            params_schema: &["url"],
            exec: Exec::Cli {
                argv_template: &["chromium", "--ozone-platform=wayland", "--new-window", "{url}"],
                wait: WaitMode::Detach { liveness_ms: DETACH_LIVENESS_MS },
            },
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
        // `WaitMode::ForExit` is not incidental here: both callers of this
        // fixture (`dispatch_cli_hit_executes_for_real_and_touches_the_file`
        // below, and `tests_registry::registry_hit_step_executes_without_
        // touching_comp`) assert the touched file EXISTS the instant
        // `dispatch` returns, which is only guaranteed because this action
        // waits for `touch` to exit.
        exec: Exec::Cli { argv_template: &["/usr/bin/touch", "{path}"], wait: WaitMode::ForExit },
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
        Exec::Cli { argv_template, wait } => execute_cli(argv_template, wait, &req.params).await,
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

async fn execute_cli(
    argv_template: &'static [&'static str],
    wait: WaitMode,
    params: &Value,
) -> Result<String, String> {
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
    // WP-CD4c VM live-fire fix (2026-08-22, minimal/surgical): layered on
    // top of the allowlist above, never added to it — `AGENT_CLI_ENV_
    // ALLOWLIST` (`duduclaw-core/spawn_env.rs`) is shared by every AI-CLI
    // (claude/codex/gemini) subprocess spawn and deliberately excludes
    // display/runtime vars (an agent CLI never needs a display). C-L2's
    // `execute_cli` is the one call site in this whole allowlist's user
    // base that spawns an actual GUI client (wave-1: Chromium), and without
    // these three the child can't find a display at all — found live: every
    // `open_window`/`open_url` call registry-hit-but-exec-failed 100% of
    // the time (`XDG_RUNTIME_DIR is invalid or not set`, then, once that
    // was passed through, `Missing X server or $DISPLAY` — this crate's own
    // `--ozone-platform=wayland` fix above handles the latter), silently
    // degrading to the C-L1 fallback on every single real call. `.env()`
    // after `apply_agent_cli_env_allowlist` is the documented "explicit env
    // wins" layering pattern this module's own doc comment already uses for
    // rotator-resolved credentials — passed through only when actually set
    // (never forces `DISPLAY` into existence for a pure-Wayland target),
    // and scoped to exactly this call site so no other allowlist consumer
    // gains a wider env surface.
    for var in ["XDG_RUNTIME_DIR", "WAYLAND_DISPLAY", "DISPLAY"] {
        if let Ok(v) = std::env::var(var) {
            if !v.is_empty() {
                cmd.env(var, v);
            }
        }
    }
    cmd.stdin(std::process::Stdio::null());

    match wait {
        WaitMode::ForExit => {
            cmd.stdout(std::process::Stdio::piped());
            cmd.stderr(std::process::Stdio::piped());
            // If EXEC_TIMEOUT elapses, the `tokio::time::timeout` future is
            // simply dropped — without this, a child that outlived the
            // timeout would keep running as an orphan. Correct for THIS
            // branch only: a `Detach` action's child is supposed to outlive
            // the call, which is exactly why that branch must not set it.
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
        WaitMode::Detach { liveness_ms } => {
            // Each of these three lines is load-bearing — see `WaitMode`'s
            // doc. `stdout` is discarded outright (bulk volume, zero
            // diagnostic value); `stderr` is piped ONLY because the drain
            // task below is spawned in the same breath and never stops
            // reading it. NO kill_on_drop: the whole point of this branch
            // is that the child outlives this call.
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::piped());
            cmd.kill_on_drop(false);

            let mut child = cmd.spawn().map_err(|e| format!("spawn failed for '{program}': {e}"))?;
            let pid = child.id();
            // Spawned BEFORE the grace sleep, not after: the child is
            // already writing, and a pipe left unread even for the grace
            // period is a pipe that can fill. Dropping this `JoinHandle`
            // (the success path does) detaches the task rather than
            // aborting it, so the draining outlives this call exactly as
            // the child does.
            let stderr_drain = child.stderr.take().map(|pipe| tokio::spawn(drain_stderr_bounded(pipe)));

            tokio::time::sleep(Duration::from_millis(liveness_ms)).await;

            match child.try_wait() {
                // Already gone, and it failed — a launcher that rejected its
                // own arguments (bad flag, no display, profile lock, …).
                // Reported so the caller degrades to C-L1 exactly as a
                // `ForExit` failure would, and with the same stderr tail
                // budget, because an exit status on its own has never been
                // enough to diagnose one of these (both real CD-4c launch
                // bugs were identified from this text).
                Ok(Some(status)) if !status.success() => {
                    let stderr_tail = collect_detach_stderr(stderr_drain).await;
                    Err(format!(
                        "'{program}' exited with {status} within the {liveness_ms}ms launch grace: {stderr_tail}"
                    ))
                }
                // Already gone, cleanly. Legitimate for a launcher that
                // hands its request to an already-running instance — which
                // is precisely what `chromium --new-window` does when a
                // Chromium is already up on that profile. An honest
                // success, worded so the audit row does not claim a process
                // is still running when it is not.
                Ok(Some(status)) => Ok(format!(
                    "cli exec ok: {program} launched and exited {status} within the {liveness_ms}ms launch grace"
                )),
                // Still running after the grace period — the success case a
                // GUI launcher is judged by. The wording deliberately never
                // says "exited": nothing has.
                Ok(None) => {
                    reap_detached(child, &program);
                    Ok(format!(
                        "cli exec ok: {program} launched{}, still running after {liveness_ms}ms (detached)",
                        pid.map(|p| format!(" (pid {p})")).unwrap_or_default()
                    ))
                }
                // `try_wait` itself errored (a waitpid-level failure). Rare,
                // and not something to guess about — the child may well be
                // running fine — so it is still reaped, and reported as an
                // honest failure rather than a claimed success.
                Err(e) => {
                    reap_detached(child, &program);
                    Err(format!("'{program}' launched but its status could not be polled: {e}"))
                }
            }
        }
    }
}

/// [`WaitMode::Detach`] requirement 4: hand a still-running detached child
/// to a background task whose only job is to `.wait()` on it, so that when
/// it eventually does exit it is reaped instead of lingering as a zombie in
/// the gateway's process table. The task holds nothing but the `Child`
/// handle and ends when the child does.
/// [`WaitMode::Detach`] requirement 2's other half: read a detached child's
/// stderr to EOF for the child's entire life — so the pipe can never fill
/// and wedge it — while RETAINING only the first
/// [`DETACH_STDERR_CAPTURE_BYTES`]. Reading and retaining are deliberately
/// separate operations here: the cap bounds memory, never the reading.
async fn drain_stderr_bounded(mut pipe: tokio::process::ChildStderr) -> Vec<u8> {
    use tokio::io::AsyncReadExt;

    let mut kept: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) => break, // EOF — every writer closed its end
            Ok(n) => {
                if kept.len() < DETACH_STDERR_CAPTURE_BYTES {
                    let room = DETACH_STDERR_CAPTURE_BYTES - kept.len();
                    kept.extend_from_slice(&buf[..n.min(room)]);
                }
                // Past the cap the loop CONTINUES and simply throws the
                // bytes away. Breaking out here instead would stop draining
                // a still-open pipe and re-create the exact 64KB wedge this
                // function exists to prevent — the one-line version of
                // requirement 2 going wrong.
            }
            // A read error means draining is over regardless; whatever was
            // captured before it is still worth reporting.
            Err(_) => break,
        }
    }
    kept
}

/// Await a detached child's bounded stderr drain and render it into the
/// failure detail, under [`DETACH_STDERR_WAIT`]. Every degradation path
/// yields an explicit placeholder rather than an empty string, so a reader
/// can tell "the child printed nothing" apart from "we could not read it" —
/// the same honesty rule the rest of this module's outcome strings follow.
async fn collect_detach_stderr(drain: Option<tokio::task::JoinHandle<Vec<u8>>>) -> String {
    let Some(handle) = drain else {
        return "<stderr unavailable>".to_string();
    };
    match tokio::time::timeout(DETACH_STDERR_WAIT, handle).await {
        Ok(Ok(bytes)) if bytes.is_empty() => "<no stderr output>".to_string(),
        // `from_utf8_lossy` FIRST: the capture is cut at a byte cap and can
        // land mid-codepoint, so the bytes are not guaranteed valid UTF-8.
        // `truncate_chars` then bounds it by CODEPOINT — never a raw byte
        // slice (coding convention 1) — on the same 200 budget the
        // `ForExit` branch uses, so both failure details read alike.
        Ok(Ok(bytes)) => duduclaw_core::truncate_chars(&String::from_utf8_lossy(&bytes), 200),
        Ok(Err(e)) => format!("<stderr drain failed: {e}>"),
        Err(_) => format!("<stderr still open after {DETACH_STDERR_WAIT:?}>"),
    }
}

fn reap_detached(mut child: tokio::process::Child, program: &str) {
    let program = program.to_string();
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => tracing::debug!(program = %program, status = %status, "codrive C-L2 detached child exited"),
            Err(e) => tracing::debug!(program = %program, error = %e, "codrive C-L2 detached child wait failed"),
        }
    });
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

    // ── registry contents: the actual argv + WaitMode, pinned ────────────
    //
    // The `find_action(...).is_some()` tests above only prove an action of
    // that NAME is registered — they say nothing about what it would
    // actually run, and the `render_argv_*` tests below feed hand-written
    // templates rather than the registry's own. That left the production
    // consts with zero test coverage of their contents: two separate live-
    // fire fixes to `CHROMIUM`'s argv (`--ozone-platform=wayland`) and to
    // how its exec waits landed without a single test going red either
    // before or after. These pin both.

    fn cli_exec_of(target_app: &str, action: &str) -> (&'static [&'static str], WaitMode) {
        let entry = find_action(APP_REGISTRY, target_app, action)
            .unwrap_or_else(|| panic!("{target_app}/{action} must be registered"));
        match entry.exec {
            Exec::Cli { argv_template, wait } => (argv_template, wait),
            _ => panic!("{target_app}/{action} must be a Cli action"),
        }
    }

    #[test]
    fn chromium_open_window_argv_and_wait_mode_are_pinned() {
        let (argv, wait) = cli_exec_of("chromium", "open_window");
        assert_eq!(
            argv,
            ["chromium", "--ozone-platform=wayland", "--new-window"].as_slice(),
            "`--ozone-platform=wayland` is a CD-4c live-fire fix: comp's target has no XWayland \
             for Chromium's default X11 Ozone backend, so dropping it breaks every launch"
        );
        assert!(
            matches!(wait, WaitMode::Detach { .. }),
            "chromium is a GUI launcher that never exits on its own — ForExit here means a \
             successful launch can only ever be reported as a timeout failure: {wait:?}"
        );
    }

    #[test]
    fn chromium_open_url_argv_and_wait_mode_are_pinned() {
        let (argv, wait) = cli_exec_of("chromium", "open_url");
        assert_eq!(argv, ["chromium", "--ozone-platform=wayland", "--new-window", "{url}"].as_slice());
        assert!(matches!(wait, WaitMode::Detach { .. }), "{wait:?}");
        let entry = find_action(APP_REGISTRY, "chromium", "open_url").unwrap();
        assert_eq!(entry.params_schema, ["url"].as_slice(), "the {{url}} placeholder and the schema must agree");
    }

    #[test]
    fn detach_liveness_is_a_real_grace_period_not_zero() {
        // A zero/near-zero grace turns Detach back into fire-and-forget,
        // which reports an instantly-failing launch as a success and stops
        // the C-L1 fallback from ever firing.
        for action in ["open_window", "open_url"] {
            match cli_exec_of("chromium", action).1 {
                WaitMode::Detach { liveness_ms } => {
                    assert!(liveness_ms >= 100, "{action}: grace too short to observe an early exit");
                    assert!(liveness_ms <= 3000, "{action}: grace is pure added latency on the success path");
                }
                other => panic!("{action}: expected Detach, got {other:?}"),
            }
        }
    }

    #[test]
    fn networkmanager_actions_stay_dbus_and_read_only() {
        // The Detach split is a `Cli` concern only: D-Bus is request/
        // response, so its wait-for-reply semantics were already right and
        // must not drift into this change.
        for action in ["connectivity", "state"] {
            let entry = find_action(APP_REGISTRY, "networkmanager", action).unwrap();
            match entry.exec {
                Exec::DBus { destination, method, .. } => {
                    assert_eq!(destination, "org.freedesktop.NetworkManager");
                    assert!(entry.params_schema.is_empty(), "{action} is a zero-arg read-only query");
                    assert!(!method.is_empty());
                }
                _ => panic!("{action} must stay a DBus action"),
            }
        }
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
        // spawning it from an automated test would pop a visible GUI window
        // and — now that those entries are `WaitMode::Detach` — leave it
        // running after the test returns, exactly what an automated suite
        // must never do. A local ad-hoc registry pointed at a path that
        // cannot possibly exist proves the same "spawn error degrades to
        // Failed, never a panic" contract hermetically.
        const FIXTURE: AppEntry = AppEntry {
            app_id: "definitely-nonexistent-app",
            aliases: &[],
            actions: &[ActionEntry {
                name: "noop",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &["/definitely/not/a/real/binary-xyz123"],
                    wait: WaitMode::ForExit,
                },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "definitely-nonexistent-app", &req("noop", Value::Null)).await;
        assert!(matches!(outcome, DispatchOutcome::Failed { .. }), "{outcome:?}");
    }

    // ── dispatch_in — WaitMode::Detach behavior (unix) ───────────────────
    //
    // These use `/bin/sh` rather than a bespoke helper binary because a
    // detach test needs a child that (a) outlives the call and (b) proves
    // afterwards that it was never killed — a marker file written after a
    // delay is the only observable that shows both, and nothing in coreutils
    // does "sleep, then touch". The `{path}` param is passed POSITIONALLY
    // (`sh -c '<static script>' sh <path>` → `"$1"`), never concatenated
    // into the script text, so this fixture models the argv-safe shape and
    // not a shell-injection one.

    #[cfg(unix)]
    fn detach_survivor_fixture() -> AppEntry {
        AppEntry {
            app_id: "codrive-detach-survivor",
            aliases: &[],
            actions: &[ActionEntry {
                name: "late_touch",
                params_schema: &["path"],
                exec: Exec::Cli {
                    argv_template: &["/bin/sh", "-c", "sleep 2; touch \"$1\"", "sh", "{path}"],
                    wait: WaitMode::Detach { liveness_ms: 200 },
                },
            }],
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_returns_long_before_the_child_exits() {
        let dir = std::env::temp_dir().join(format!("codrive-detach-fast-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("marker").to_string_lossy().to_string();

        let started = std::time::Instant::now();
        let outcome = dispatch_in(
            &[detach_survivor_fixture()],
            "codrive-detach-survivor",
            &req("late_touch", serde_json::json!({"path": target})),
        )
        .await;
        let elapsed = started.elapsed();

        match outcome {
            DispatchOutcome::Executed { detail } => {
                assert!(
                    detail.contains("still running"),
                    "a live detached child's audit detail must say so, never claim it exited: {detail}"
                );
                assert!(!detail.contains("exited"), "must not claim an exit that never happened: {detail}");
            }
            other => panic!("a long-running detached child must be Executed, got {other:?}"),
        }
        // The child sleeps 2s; the grace is 200ms. Under the OLD
        // wait-for-exit semantics this call could not have returned in
        // under 2s (and the real chromium entries could not have returned
        // in under EXEC_TIMEOUT at all).
        assert!(elapsed < Duration::from_secs(1), "detach must not wait for exit (took {elapsed:?})");
        assert!(
            !std::path::Path::new(&target).exists(),
            "sanity: the child must still be mid-sleep when dispatch returns"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_does_not_kill_the_child_it_reported_as_running() {
        // The regression this pins: `kill_on_drop(true)` on the detach path
        // would kill the child the moment the `Child` handle dropped — i.e.
        // right after reporting success — so the marker would never appear.
        let dir = std::env::temp_dir().join(format!("codrive-detach-alive-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("marker").to_string_lossy().to_string();

        let outcome = dispatch_in(
            &[detach_survivor_fixture()],
            "codrive-detach-survivor",
            &req("late_touch", serde_json::json!({"path": target.clone()})),
        )
        .await;
        assert!(matches!(outcome, DispatchOutcome::Executed { .. }), "{outcome:?}");

        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while !std::path::Path::new(&target).exists() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the detached child never wrote its marker — it was killed after dispatch returned"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_still_fails_when_the_child_exits_nonzero_within_the_grace() {
        // The other half of the contract: a launcher that rejects its own
        // arguments and dies immediately must NOT be reported as a success
        // just because we stopped waiting for exit — otherwise the C-L1
        // fallback would never fire again.
        const FIXTURE: AppEntry = AppEntry {
            app_id: "codrive-detach-instafail",
            aliases: &[],
            actions: &[ActionEntry {
                name: "die",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &["/bin/sh", "-c", "exit 3"],
                    wait: WaitMode::Detach { liveness_ms: 400 },
                },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "codrive-detach-instafail", &req("die", Value::Null)).await;
        match outcome {
            DispatchOutcome::Failed { detail } => {
                assert!(detail.contains("launch grace"), "detail should name the grace window: {detail}");
            }
            other => panic!("an instantly-failing detached child must be Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_clean_early_exit_is_an_honest_success() {
        // A launcher handing off to an already-running instance exits 0
        // straight away (`chromium --new-window` against a live profile
        // does exactly this) — a real success, and the detail must say
        // "exited", not "still running".
        const FIXTURE: AppEntry = AppEntry {
            app_id: "codrive-detach-handoff",
            aliases: &[],
            actions: &[ActionEntry {
                name: "handoff",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &["/bin/sh", "-c", "exit 0"],
                    wait: WaitMode::Detach { liveness_ms: 400 },
                },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "codrive-detach-handoff", &req("handoff", Value::Null)).await;
        match outcome {
            DispatchOutcome::Executed { detail } => {
                assert!(detail.contains("exited"), "{detail}");
                assert!(!detail.contains("still running"), "must not claim a process that is gone: {detail}");
            }
            other => panic!("a clean early exit must be Executed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_failure_detail_carries_the_child_stderr() {
        // The whole reason stderr is piped on this branch: both real C-L2
        // launch bugs found on the CD-4c rig (`Missing X server or
        // $DISPLAY`, `XDG_RUNTIME_DIR is invalid or not set`) were
        // identified from this text, and an exit status alone says nothing.
        const FIXTURE: AppEntry = AppEntry {
            app_id: "codrive-detach-noisy-fail",
            aliases: &[],
            actions: &[ActionEntry {
                name: "die_loudly",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &["/bin/sh", "-c", "echo codrive-stderr-marker-7f3a 1>&2; exit 4"],
                    wait: WaitMode::Detach { liveness_ms: 400 },
                },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "codrive-detach-noisy-fail", &req("die_loudly", Value::Null)).await;
        match outcome {
            DispatchOutcome::Failed { detail } => {
                assert!(
                    detail.contains("codrive-stderr-marker-7f3a"),
                    "the child's stderr must reach the failure detail: {detail}"
                );
                assert!(detail.contains("launch grace"), "{detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_stderr_past_the_cap_does_not_wedge_the_child() {
        // ~260KB of stderr, several times the ~64KB pipe buffer. If the
        // drain stopped reading at `DETACH_STDERR_CAPTURE_BYTES` instead of
        // merely stopping RETAINING there, this child would block forever
        // on a full pipe, never exit, and be reported as a live "Executed"
        // launch — so the `Failed` assertion below is the regression pin
        // for requirement 2, not just a smoke test.
        const FIXTURE: AppEntry = AppEntry {
            app_id: "codrive-detach-firehose",
            aliases: &[],
            actions: &[ActionEntry {
                name: "flood_then_die",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &[
                        "/bin/sh",
                        "-c",
                        "i=0; while [ $i -lt 4000 ]; do \
                         echo 'codrive-flood-................................................'  1>&2; \
                         i=$((i+1)); done; echo codrive-stderr-marker-tail 1>&2; exit 5",
                    ],
                    wait: WaitMode::Detach { liveness_ms: 2000 },
                },
            }],
        };
        let started = std::time::Instant::now();
        let outcome = dispatch_in(&[FIXTURE], "codrive-detach-firehose", &req("flood_then_die", Value::Null)).await;
        let elapsed = started.elapsed();

        match outcome {
            DispatchOutcome::Failed { detail } => {
                // Bounded by `truncate_chars(.., 200)`, so the 260KB never
                // reaches the audit row — and the marker printed LAST is
                // past the retained head, which is the documented
                // head-not-tail trade-off, not a bug.
                assert!(detail.chars().count() < 600, "the detail must stay bounded: {} chars", detail.chars().count());
                assert!(detail.contains("codrive-flood-"), "the retained head must survive: {detail}");
            }
            other => panic!("a flooding child must still exit and be judged by its status, got {other:?}"),
        }
        assert!(elapsed < Duration::from_secs(8), "draining must not stall the dispatch (took {elapsed:?})");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detach_spawn_error_is_failed_not_panic() {
        const FIXTURE: AppEntry = AppEntry {
            app_id: "codrive-detach-missing-binary",
            aliases: &[],
            actions: &[ActionEntry {
                name: "noop",
                params_schema: &[],
                exec: Exec::Cli {
                    argv_template: &["/definitely/not/a/real/binary-xyz123"],
                    wait: WaitMode::Detach { liveness_ms: 200 },
                },
            }],
        };
        let outcome = dispatch_in(&[FIXTURE], "codrive-detach-missing-binary", &req("noop", Value::Null)).await;
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
