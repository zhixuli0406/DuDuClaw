//! WP-comp-shell-ipc (2026-08-22) — shell↔comp window query/control IPC.
//!
//! ## Why this exists (A3's architecture finding)
//! A3's dock integration needed comp to answer "what windows are running"
//! and "switch to this one", but `codrive`'s injection socket
//! (`duduclaw-codrive.sock`) is structurally the wrong channel for that:
//! it is the AGENT's private, token-authenticated seat-injection channel —
//! every command through it is attributed to the agent seat, drives
//! `codrive`'s own freeze/takeover/shadow state machine, and lands in the
//! `codrive` audit trail as an agent action (`codrive/mod.rs`'s module doc
//! has the full state machine). Routing a human's dock click through it
//! would misattribute a human action as an agent action in that trail —
//! exactly the kind of audit-poisoning DESIGN-codrive-desktop-2026-08.md's
//! safety redlines exist to prevent. So this module is a SECOND, entirely
//! separate socket/protocol/audit-trail: `duduclaw-shell.sock` (task
//! brief), reusing only the pure window-matching/focus LOGIC
//! `codrive::window_target`/`DuduclawComp::focus_window` already proved
//! live (WP-A1/WP-CD4a-COMP), never the socket, auth, or audit machinery.
//!
//! ## Trust boundary — same-uid `SO_PEERCRED`, not a bearer token
//! `codrive`'s token exists because an agent CLI subprocess and this
//! compositor are NOT the same trust domain in general (DESIGN §3.3.1's
//! "EIS 界線"). `duduclaw-shell` and `duduclaw-comp`, by contrast, are:
//! on the appliance both run under the SAME kiosk-session system user
//! (`duduclaw-kiosk`, `appliance/mkosi.extra/etc/systemd/system/
//! duduclaw-kiosk.service`), while agent CLI subprocesses run under a
//! DIFFERENT user (`duduclaw`, `duduclaw-gateway.service` — see
//! `appliance/postinst.d/20-users-and-units.sh`'s own useradd comments).
//! That is a real, kernel-enforced boundary a bearer-token file can't
//! improve on and a same-uid `SO_PEERCRED` check (`listener::
//! is_authorized_peer`) uses directly — mirroring the exact pattern
//! `duduclaw-sysd` already established for its own root-daemon socket
//! (`duduclaw-sysd/src/server.rs`'s `handle_connection`), simplified from
//! "an externally configured allowed uid" to "my own uid" since this
//! process and its legitimate caller are always the same user by
//! construction, unlike sysd's caller (a different, non-root user calling
//! into a root daemon). See `listener.rs`'s own doc comment for the exact
//! check and its tests for the "agent cannot reach this socket" proof.
//!
//! ## No freeze gate — this is what a human operating their own desktop
//! looks like
//! `codrive`'s freeze/terminated/takeover state machine governs when the
//! AGENT seat may act — DESIGN §3.1's "人輸入永遠優先". A dock click is
//! human input BY DEFINITION (this socket cannot be reached by anything
//! that isn't already authenticated as the same uid as this kiosk session
//! — see above), so it is never gated behind `codrive.frozen`/
//! `terminated`/`takeover_active` at all: a human can always operate their
//! own desktop, exactly like every other human-input path in this crate
//! (`input.rs`'s real seat, `codrive::human_resume`/`emergency_stop`).
//! This is intentional, not an oversight — gating a human action behind
//! the AGENT's freeze state would have the exact backwards semantics of
//! DESIGN §3.1. It is also not a red-line-3 backdoor for the AGENT to
//! bypass its own freeze: the same-uid auth above means the agent seat's
//! own operator (an agent CLI subprocess) structurally cannot open this
//! socket in the first place, so there is no path from "agent wants to
//! act while frozen" through this module at all.
//!
//! ## One-shot RPC, not a persistent session
//! Unlike `codrive`'s long-lived, stateful, single-connection-at-a-time
//! session (`codrive::listener`'s module doc), this socket is a plain
//! connect → one request line → one response line → close round trip per
//! call (`protocol.rs`'s own module doc has the full reasoning) — the
//! natural shape for a dock that polls `list_windows` on an interval and
//! fires `focus_window` on a click, with no session state to keep in sync
//! across calls.
//!
//! ## Independent audit trail
//! `audit.rs` writes its own JSONL file (`duduclaw-shell-control-audit.
//! jsonl`), never `codrive`'s — a reader who needs to tell "a human
//! switched windows" apart from "the agent did" can do so by WHICH FILE a
//! line is in, not a field inside a shared, disputable file. Only the
//! ACTIONS — `focus_window` and CUR-2's `set_cursor_source` — are audited,
//! mirroring the established "queries aren't audited, actions are"
//! precedent `codrive::listener`'s own `status`/`resume` handling already
//! set — `list_windows` and `get_cursor_source` are read-only and,
//! realistically, polled every few seconds by a live dock / re-read every
//! time a settings page opens, so auditing them would mostly be noise, not
//! evidence.
//!
//! ## CUR-2 (2026-08-22): this socket also carries appearance preferences
//! `get_cursor_source` / `set_cursor_source` let the shell switch the human
//! pointer between the machine's system cursors and the DuDuClaw brand paw
//! **without restarting the compositor**. They live here rather than on
//! `codrive`'s socket for exactly the reason `focus_window` does: choosing
//! how your own pointer looks is a HUMAN act, and attributing it to the
//! agent in the codrive trail would be the audit poisoning this module
//! exists to prevent. The same-uid `SO_PEERCRED` boundary is also precisely
//! the right authority for it — only a process running as this kiosk
//! session's own user may change how that session looks, and an agent CLI
//! subprocess (a DIFFERENT system user) structurally cannot reach the
//! socket at all.
//!
//! ### How the shell calls them
//! ```text
//! -> {"op":"get_cursor_source"}
//! <- {"ok":true,"cursor":{"source":"system","requested":"system",
//!                         "theme":"Adwaita","origin":"default",
//!                         "size":24,"effective_size":24,
//!                         "size_env_pinned":false,"env_pinned":false}}
//!
//! -> {"op":"set_cursor_source","params":{"source":"brand"}}
//! <- {"ok":true,"cursor":{"source":"brand","requested":"brand",
//!                         "theme":"DuDuClaw","origin":"runtime",
//!                         "size":24,"effective_size":24,
//!                         "size_env_pinned":false,"env_pinned":false,
//!                         "persisted":true}}
//!
//! -> {"op":"set_cursor_source","params":{"source":"brnad"}}
//! <- {"ok":false,"error":"invalid_cursor_source"}
//! ```
//! A settings page drives its radio state from `requested` (the user's own
//! choice) and can warn from two honest signals: `source != requested`
//! means the brand theme package is not installed and system cursors are
//! being drawn instead; `env_pinned: true` means an operator pinned
//! `DUDUCLAW_COMP_CURSOR_SOURCE` in comp's spawn environment, so the stored
//! preference will not apply at the next start. Building that page is
//! deliberately NOT part of CUR-2 — the op shape is.
//!
//! ## CUR-3 (2026-08-23): pointer SIZE, on the same socket
//! `set_cursor_size` is the third appearance-preference op, added for the
//! shell's 協助工具 › 指向與點按 page (five segments: 24 / 32 / 48 / 64 / 96).
//! It lives here for the same reason the two above do — pointer size is an
//! accessibility preference belonging to the human at the keyboard, and
//! attributing it to the agent in the codrive trail would be exactly the audit
//! poisoning this module exists to prevent. Live (no restart), persisted, and
//! audited, identical in shape to `set_cursor_source`:
//!
//! ```text
//! -> {"op":"set_cursor_size","params":{"size":32}}
//! <- {"ok":true,"cursor":{…,"size":32,"effective_size":32,"persisted":true}}
//!
//! -> {"op":"set_cursor_size","params":{"size":40}}
//! <- {"ok":false,"error":"invalid_cursor_size"}
//! ```
//!
//! The size axis carries one honest signal the source axis does not need:
//! **`effective_size` may differ from `size`.** An XCursor theme holds a fixed
//! set of image sizes and the nearest is drawn at its own size — nothing is
//! upscaled — so a 96 request against a theme whose largest image is 64 really
//! draws 64 px. That is a correct outcome, not an error, and the reply says so
//! rather than claiming 96. On the appliance's own Adwaita the two are equal at
//! every offered step (Adwaita ships exactly 24/32/48/64/96). See
//! `crate::cursor::theme::CursorThemeStore::effective_size` for the full
//! reasoning, including why upscaling-to-match was rejected.
//!
//! `size_env_pinned` is the size-side twin of `env_pinned`: an operator who set
//! `XCURSOR_SIZE` outranks the stored preference at the next start, and a
//! settings page should say so instead of promising the choice will stick. Note
//! that `XCURSOR_SIZE` accepts any 8–512 value while this op accepts only the
//! five steps — a deliberate operator-vs-UI split documented in
//! `crate::cursor::source`'s own module doc.
//!
//! ## WP-comp-shell-display (2026-08-23): outputs, for the shell's 顯示 page
//! `get_outputs` / `set_output_mode` / `set_output_scale` let a new
//! system-settings app answer "what screens exist, at what resolution/scale"
//! and attempt to change either. Same socket, same trust boundary, same
//! "queries aren't audited, actions are" split as everything else in this
//! module — `get_outputs` is read-only and unaudited, the two `set_*` ops
//! are audited regardless of outcome (this crate is the only process that
//! owns `Output`, so this is structurally the only place these ops can live).
//!
//! ### Wire shape
//! ```text
//! -> {"op":"get_outputs"}
//! <- {"ok":true,"outputs":[
//!      {"name":"Virtual-1","description":"DuDuClaw - duduclaw-comp (winit) - Virtual-1",
//!       "make":"DuDuClaw","model":"duduclaw-comp (winit)",
//!       "width":1920,"height":1080,"refresh_mhz":60000,"scale_pct":100,
//!       "physical_width_mm":0,"physical_height_mm":0,
//!       "modes":[{"width":1920,"height":1080,"refresh_mhz":60000,
//!                 "preferred":true,"current":true}],
//!       "mode_switch_supported":false}
//!    ]}
//!
//! -> {"op":"set_output_mode","params":{"output":"Virtual-1","width":1920,
//!                                       "height":1080,"refresh_mhz":60000}}
//! <- {"ok":false,"error":"mode_switch_unsupported"}
//!
//! -> {"op":"set_output_scale","params":{"output":"Virtual-1","scale_pct":200}}
//! <- {"ok":true,"outputs":[
//!      {"name":"Virtual-1", … ,"scale_pct":200, … }
//!    ]}
//! ```
//! (D4b-3, 2026-08-24: `set_output_scale` now applies for real — see "Scale,
//! real as of D4b-3" below. `set_output_mode` is unchanged and still always
//! refuses — see the section right after this one.)
//! The CD-2 shadow workspace's headless output never appears in `outputs` —
//! same exclusion `state.rs::primary_output` already applies, for the same
//! reason: it is not a screen a human can see.
//!
//! ### `modes` is `Output::modes()`, verbatim — checked, not assumed
//! Read against smithay 0.7.0's actual `src/output.rs`: `Output::
//! change_current_state(Some(mode), …)` and `Output::set_preferred(mode)`
//! both push their argument onto the output's internal `modes` list if it
//! isn't already there. Both `winit_backend.rs::init_winit` and
//! `udev_backend.rs::build_surfaces` call exactly that pair before an output
//! is ever mapped into `space` — so in practice `modes()` is **not** empty
//! on either backend; it holds at least the one mode the output was created
//! with, reported as both `current` and `preferred`. Two things this crate
//! deliberately does NOT do, which is why the list stays that short:
//! - `Output::add_mode` is never called anywhere in this crate, so no
//!   backend ever advertises modes it cannot already produce.
//! - The udev/DRM backend never calls `change_current_state` again after
//!   `build_surfaces` — a real monitor's other EDID modes (`connector::
//!   Info::modes()`, plural, already read by `pick_mode` to choose the ONE
//!   mode a surface is created with) are simply never surfaced here.
//!
//! The one path that can genuinely grow this list at runtime is the winit
//! backend's `WinitEvent::Resized` handler, which calls `change_current_state`
//! again with the host window's new size — each distinct size the nested
//! window has ever been resized to accumulates as its own entry (nothing
//! is ever removed; `Output::delete_mode` is never called either). This is
//! real smithay behaviour, not a bug introduced here, and is reported
//! honestly rather than papered over. `get_outputs` does not synthesize,
//! pad, or deduplicate beyond what `Output::modes()` itself already
//! deduplicates (by exact `Mode` equality) — an empty `modes` array is left
//! as the correct answer for the (currently unreached, but not assumed
//! impossible) case of an output with no current mode at all.
//!
//! ### Why `mode_switch_supported` is always `false`, and `set_output_mode`
//! ### always answers `mode_switch_unsupported`
//! The op parses and validates fully — `unknown_output` for a name that
//! doesn't match any real output, `invalid_mode` for a triple that isn't
//! one of that output's own known modes (`protocol::mode_request_matches`)
//! — before ever reaching the refusal. But actually applying a different
//! mode is out of reach of a contained change to this module on **either**
//! backend, checked by reading both:
//! - **winit** (`winit_backend.rs`): the only handle to the host `winit::
//!   window::Window` lives inside the `move` closure `init_winit` builds
//!   for the event source, captured by value — it is not stored on
//!   `DuduclawComp` or reachable from anywhere `shell_control` can see, and
//!   there is no public accessor to an owned/cloneable handle either
//!   (checked against smithay 0.7.0 source: `WinitGraphicsBackend` holds
//!   `window: Arc<WinitWindow>` PRIVATELY and exposes only `fn window(&self)
//!   -> &WinitWindow`, so the `Arc` itself cannot be cloned out through the
//!   public API).
//!
//!   **Revised finding, D4b-3 (2026-08-24) — this is NOT a hard wall.** A
//!   *mailbox* pattern is genuinely available and was not in scope for this
//!   round: add `DuduclawComp::pending_output_mode_request: Option<(String,
//!   Mode)>`; `shell_control_set_output_mode` sets it (a plain data write,
//!   no window handle needed) instead of refusing; `init_winit`'s own
//!   `WinitEvent::Redraw` arm — which already captures `backend` by move and
//!   therefore already HAS `backend.window()` in scope every frame — polls
//!   and clears it, calling `output.change_current_state(...)` (cheap,
//!   already proven safe: this is exactly what `set_output_scale` below
//!   does) followed by `backend.window().request_inner_size(...)`. This is
//!   the same "the consumer already holds the resource; hand it a mailbox
//!   instead of trying to extract the resource" shape `codrive_sync_mode`/
//!   `codrive_check_watch_idle` already use for other main-thread-only
//!   state. It is explicitly NOT the "on-demand winit" refactor
//!   `winit_backend.rs`'s own doc comment records as attempted and reverted
//!   (that one hoisted the REDRAW SCHEDULING itself out of the closure and
//!   regressed to ~780k skipped frames/5s under CPU measurement — a
//!   different, larger, unrelated change).
//!
//!   **Deferred anyway**, for reasons independent of the mailbox's
//!   feasibility: (1) the host window manager is free to grant a DIFFERENT
//!   size than requested (`request_inner_size` is a request, not a
//!   guarantee), so the synchronous `set_output_mode` reply cannot honestly
//!   report the outcome without either blocking a frame or echoing state it
//!   has not observed yet — the existing `Ok(None) ⇒ shell re-reads`
//!   contract handles this, but it is untested for a resize specifically;
//!   (2) the winit backend only ever runs nested in a host Wayland/X11
//!   session for local development/CI (`BUILD.md`: "The appliance runs the
//!   udev backend") — it has ZERO production value on the duty-machine
//!   appliance this crate ships on, which is the udev backend below; (3) no
//!   test coverage exists yet for a live resize racing the tight unthrottled
//!   winit redraw loop. Left as a scoped, bounded follow-up rather than
//!   implemented under this round's time budget — the point of writing this
//!   down is that "impossible" was the WRONG conclusion the first time this
//!   doc was written, and the corrected one is "possible, low production
//!   value, deferred".
//! - **udev/DRM** (`udev_backend.rs`): the surface's mode is fixed at
//!   `DrmDevice::create_surface(crtc, drm_mode, …)` and baked into the
//!   `GbmBufferedSurface` swapchain's buffer size (`build_surfaces`). A
//!   different mode means a new `DrmSurface` and a new, differently-sized
//!   `GbmBufferedSurface` — i.e. rebuilding the `SurfaceData` this backend's
//!   whole render/vblank/damage-tracking lifecycle is built around, while
//!   `UdevBackendState` (in `CalloopData::udev`) is not even reachable from
//!   `DuduclawComp`, which is all `shell_control`'s handlers ever get. This
//!   is the backend that actually ships (see above) and the surface-rebuild
//!   risk here is real, not a documentation gap — still refused, on purpose,
//!   this round.
//!
//! Given that, `get_outputs`'s `mode_switch_supported` is always `false` —
//! the one thing the task's own instructions call out as unacceptable is a
//! `true` here that a real request would then refuse, so this errs the
//! other way. A future round that actually threads a mode-change request
//! into one backend can flip this per-output without lying about the other.
//!
//! ### Scale, real as of D4b-3 (2026-08-24)
//! `Output::change_current_state` could always set a new `Scale` live —
//! smithay sends the wire update itself (`wl_change_current_state`, gated on
//! `wayland_frontend`). What blocked this op through the previous round was
//! never a missing smithay feature; it was that this crate's own render
//! pipeline did not consistently read the output's scale at all. Checked by
//! grep, not assumed: `cursor/mod.rs`, `codrive/cursor.rs`,
//! `codrive/highlight.rs`, and `codrive/mode_indicator.rs` each hardcoded
//! `Scale::from(1.0)` when building their render elements, while the
//! **only** place that read `Output::current_scale()` was
//! `decor/paint.rs`'s decoration-buffer rendering. Flipping the output's
//! live scale would have made decorations resize while the human/agent
//! cursors, the codrive highlight box, and the screen-edge co-drive
//! indicator stayed physical-pixel-for-logical-pixel — a real desync, not a
//! hypothetical one.
//!
//! Fixed this round by consolidating every one of those four call sites
//! (plus `decor/paint.rs`'s own, pre-existing correct one) onto
//! `render::output_render_scale(&Output) -> Scale<f64>` as the single
//! source of truth — see that function's own doc for why it is a cheap
//! per-call read rather than a cached field. `shell_control_set_output_scale`
//! now: validates (`unknown_output`, `invalid_scale` against the closed
//! `protocol::OUTPUT_SCALE_STEPS` set, unchanged) → applies live via
//! `change_current_state` → re-derives layer-shell/window layout against the
//! output's now-different LOGICAL geometry (`rearrange_layers` +
//! `reapply_window_policy_all`, mirroring what a MODE change already
//! triggers) → persists the choice (`output_prefs`, survives a restart) →
//! echoes the refreshed `outputs` list. This is backend-agnostic: nothing
//! about it touches a `DrmSurface`/`GbmBufferedSurface`, so it applies the
//! same way on udev/DRM as on winit — unlike mode-switching above, scale
//! was never actually blocked by hardware surface lifetime, only by the
//! render pipeline having been built assuming it forever stayed 1.0.
//!
//! **Verified this round only for the two integer steps (100%/200%)** —
//! task brief priority ("整数倍先行"). The three fractional steps
//! (125/150/175%) go through the exact same code path (`Scale::Fractional`
//! uniformly, see `shell_control_set_output_scale`'s own doc) and are
//! believed correct by the same reasoning, but were not independently VM/
//! live-verified: this crate registers `xdg_output` but no
//! `wp_fractional_scale_v1` global (checked: `state.rs`'s global list), so a
//! client that only understands integer `wl_output.scale` sees `ceil()` of
//! the fractional value and may render slightly soft at those three steps
//! specifically — a genuine, disclosed limitation, not a hidden one. See
//! this crate's `BUILD.md` for the live-verification log this round added.
//!
//! ## D2: `set_theme` — comp's own appearance, driven by the shell
//! ```text
//! -> {"op":"set_theme","params":{"theme":"dark"}}
//! <- {"ok":true}
//!
//! -> {"op":"set_theme","params":{"theme":"brnad"}}
//! <- {"ok":false,"error":"invalid_theme"}
//! ```
//! Switches comp's OWN server-side decorations (title bar, border, shadow,
//! Alt-Tab switcher panel — `crate::decor::Palette`) between light and dark,
//! live, no restart. Lives on this socket for the same reason every other
//! appearance preference here does: the shell drives the theme choice from
//! its own `ThemeChoice` (both at its own boot and on every user toggle), and
//! routing that through `codrive`'s agent-injection socket would misattribute
//! a person's own choice to the agent seat in that trail. Audited (an
//! ACTION), never persisted on comp's side — the shell is the durable source
//! of truth and re-announces its value every time IT starts, so comp only
//! ever needs the live value. See `crate::decor::Theme`'s own doc for the
//! full reasoning.
//!
//! ## A1: `take_shell_intents` — the compositor tells the shell something
//! happened
//! ```text
//! -> {"op":"take_shell_intents"}
//! <- {"ok":true,"intents":[]}
//! ```
//! or, after a human presses Super+K anywhere in the session:
//! ```text
//! -> {"op":"take_shell_intents"}
//! <- {"ok":true,"intents":["global_task_bar"]}
//! ```
//! The compositor is the only thing that can see a global keyboard shortcut
//! while an unrelated client holds keyboard focus — the standard wlroots-
//! ecosystem division of labour — so Super+K interception has to live in
//! `input.rs`'s human keyboard filter closure, same structural place (and
//! same "an agent-injected key event structurally cannot reach this closure"
//! guarantee) as Super+Q/Super+Esc/Super+Enter/Alt-Tab. The intercepted press
//! pushes one [`ShellIntent`] onto `DuduclawComp::pending_shell_intents`
//! (`DuduclawComp::push_shell_intent`); this op DRAINS that queue (read and
//! clear in one step, never a re-readable snapshot), which is the shape a
//! ~200ms short-poll loop (the shell's own contract) wants: whatever this
//! call returns is delivered exactly once. `intents` is always present on the
//! wire, even as `[]` — see [`ShellControlResponse::intents`]'s own doc for
//! why that field is NOT skipped the way every other optional field on this
//! envelope is. The queue itself is bounded
//! ([`protocol::MAX_PENDING_SHELL_INTENTS`]) with a drop-oldest-and-warn
//! policy, so a shell that stops polling cannot turn it into an unbounded
//! leak — see `DuduclawComp::push_shell_intent`'s own doc.

mod audit;
// A2 共駕復活 (2026-08-24): the human side of the driving-mode contract —
// `codrive_status` / `codrive_drive`, their action enum, their response
// block, and the two `DuduclawComp` handlers behind them. A separate file
// (with its own `impl DuduclawComp` block, the pattern `codrive/takeover.rs`
// and `codrive/watch.rs` already established) rather than more of this one:
// `mod.rs` is already past the project's 800-line per-file cap.
mod codrive_ops;
mod listener;
mod protocol;

pub(crate) use protocol::{
    ShellControlRequest, ShellControlResponse, ShellIntent, ShellOutputInfo, ShellOutputMode,
    ShellWindowInfo, MAX_PENDING_SHELL_INTENTS, OUTPUT_SCALE_STEPS,
};

use std::{path::PathBuf, sync::Arc};

use smithay::{
    // Aliased: `smithay::output::Scale` (the output-configuration enum
    // `change_current_state` takes) and `smithay::utils::Scale<f64>` (the
    // geometry-conversion factor `render::output_render_scale` returns) are
    // two different types with the same bare name — this file only ever
    // needs the former, but the alias keeps that unambiguous at every call
    // site rather than relying on nobody importing the other one later.
    output::{Output, Scale as OutputScale},
    reexports::calloop::{self, EventLoop},
    utils::SERIAL_COUNTER,
};

use crate::{
    codrive::{self, window_target::WindowMatch},
    state::DuduclawComp,
    CalloopData,
};
use audit::ShellControlAuditLog;

/// Cross-thread state shared between the calloop main thread and the
/// shell-control socket thread — today just the audit log (unlike
/// `codrive::CodriveShared`, there is no frozen/terminated/auth-token
/// state here at all, per this module's own doc comment on why: no freeze
/// gate, and auth is a stateless per-connection `SO_PEERCRED` check that
/// needs no shared mutable state to enforce). `pub` (not `pub(crate)`) —
/// mirrors `codrive::CodriveShared`'s own visibility, needed because
/// `DuduclawComp::shell_control` (`state.rs`) exposes this type through a
/// `pub` struct field, same as `DuduclawComp::codrive` already does for
/// `CodriveShared`.
pub struct ShellControlShared {
    audit: Option<ShellControlAuditLog>,
}

impl ShellControlShared {
    fn new(audit: Option<ShellControlAuditLog>) -> Self {
        Self { audit }
    }

    fn disabled() -> Self {
        Self { audit: None }
    }

    /// No-ops (fail-open on *logging*, same repo convention #4 split
    /// `codrive::CodriveShared::record` already documents) when the audit
    /// log failed to open at startup.
    pub(crate) fn record(&self, kind: &'static str, detail: Option<String>) {
        if let Some(audit) = &self.audit {
            audit.record(kind, detail);
        }
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        Self { audit: None }
    }
}

/// One request in flight from the socket thread to the calloop main
/// thread, carrying its own oneshot reply channel — see `listener.rs`'s
/// module doc for why this round trip exists (unlike `codrive`'s
/// fire-and-forget `InjectCmd` channel, every shell-control caller needs
/// the REAL computed answer, not an immediately-knowable ack).
pub(crate) struct ShellControlMsg {
    pub(crate) req: ShellControlRequest,
    /// A7c: which trust tier `listener::classify_peer` assigned this
    /// connection — `Denied` never reaches this struct (the socket thread
    /// answers and closes before ever sending). Threaded through so the
    /// mutating handlers below can label their audit line correctly
    /// instead of the trail silently implying every action was human.
    pub(crate) actor: listener::PeerAuthority,
    pub(crate) reply_tx: std::sync::mpsc::Sender<ShellControlResponse>,
}

/// A7c: optional env var naming a second uid `listener::classify_peer`
/// trusts as `PeerAuthority::Agent`, read once at [`init`]. Unset by
/// default — the Yocto bring-up image needs no config at all today (its
/// gateway runs as root, and `classify_peer`'s root carve-out covers that
/// unconditionally); this exists for the day gateway de-roots (the Debian
/// appliance model, `duduclaw` uid — `appliance/mkosi.extra/etc/systemd/
/// system/duduclaw-gateway.service`'s `User=duduclaw`) and needs an
/// explicit second identity instead.
const SHELL_CONTROL_AGENT_UID_ENV: &str = "DUDUCLAW_SHELL_CONTROL_AGENT_UID";

/// Starts the shell-control socket listener + audit log and wires its
/// request channel into the event loop. Called from `DuduclawComp::new`,
/// mirroring `codrive::init`'s own call shape — but this needs no
/// `SeatState`/`DisplayHandle` at all (no new `wl_seat` is created; every
/// op here acts on the EXISTING human seat, `DuduclawComp::seat`).
pub fn init(event_loop: &mut EventLoop<CalloopData>) -> Arc<ShellControlShared> {
    let Some(runtime_dir) = std::env::var_os("XDG_RUNTIME_DIR") else {
        tracing::error!(
            "shell_control: XDG_RUNTIME_DIR is not set — the shell-control socket is disabled \
             for this run"
        );
        return Arc::new(ShellControlShared::disabled());
    };

    let sock_path = PathBuf::from(&runtime_dir).join(protocol::SOCKET_FILE_NAME);
    let audit_path = PathBuf::from(&runtime_dir).join(protocol::AUDIT_FILE_NAME);

    let audit = match ShellControlAuditLog::open(&audit_path) {
        Ok(a) => Some(a),
        Err(e) => {
            tracing::error!(error = %e, path = %audit_path.display(), "shell_control: failed to open audit log — continuing without one");
            None
        }
    };

    let shared = Arc::new(ShellControlShared::new(audit));

    // This process's own uid — the entire auth policy for the ORIGINAL
    // (Human) trust tier (see this module's doc comment for why "same uid
    // as me" is sufficient and correct here, unlike `codrive`'s bearer
    // token).
    // SAFETY: `getuid()` has no preconditions and cannot fail.
    let own_uid = unsafe { libc::getuid() };

    // A7c: the forward-compat second identity (`listener::PeerAuthority::
    // Agent`) for the day gateway de-roots to a real non-zero uid — see
    // `listener::classify_peer`'s doc comment for the full reasoning,
    // including why root (uid 0) needs no config at all. Fail-closed on a
    // missing or unparseable value: `None` means "no second identity is
    // trusted", never "trust everyone" — coding convention #4.
    let agent_uid = match std::env::var(SHELL_CONTROL_AGENT_UID_ENV) {
        Ok(raw) => match raw.trim().parse::<u32>() {
            Ok(uid) => Some(uid),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    value = %raw,
                    "shell_control: {SHELL_CONTROL_AGENT_UID_ENV} is set but not a valid uid — \
                     ignoring (fail-closed: no second identity is trusted this run)"
                );
                None
            }
        },
        Err(_) => None,
    };

    let (tx, rx) = calloop::channel::channel::<ShellControlMsg>();

    if let Err(e) = listener::spawn(sock_path, own_uid, agent_uid, Arc::clone(&shared), tx) {
        tracing::error!(error = %e, "shell_control: failed to start the shell-control socket listener — dock/window queries will fail this run");
    }

    event_loop
        .handle()
        .insert_source(rx, |event, _, data: &mut CalloopData| {
            if let calloop::channel::Event::Msg(msg) = event {
                let resp = data.state.handle_shell_control_request(msg.req, msg.actor);
                // Best-effort: if the socket thread already gave up
                // waiting (`listener::MAIN_THREAD_REPLY_TIMEOUT` elapsed)
                // the receiver is gone and this send simply fails — no
                // panic, no retry, matching `codrive::CodriveShared::
                // push_event`'s own "courtesy, not guaranteed delivery"
                // posture for a dropped peer.
                let _ = msg.reply_tx.send(resp);
            }
        })
        .expect("shell_control: failed to insert the request channel into the event loop");

    shared
}

impl DuduclawComp {
    /// Executes one already-validated (by `listener::validate`) shell-
    /// control request on the calloop main thread — the only thread
    /// allowed to touch `self.space`/`self.seat`, same reasoning as
    /// `codrive::handle_agent_inject`. No freeze/terminated check here at
    /// all — see this module's own doc comment for why that is correct,
    /// not an oversight.
    pub(crate) fn handle_shell_control_request(
        &mut self,
        req: ShellControlRequest,
        actor: listener::PeerAuthority,
    ) -> ShellControlResponse {
        match req {
            ShellControlRequest::ListWindows => ShellControlResponse::windows(self.shell_control_list_windows()),
            ShellControlRequest::FocusWindow { query } => self.shell_control_focus_window(query),
            ShellControlRequest::GetCursorSource => ShellControlResponse::cursor(self.cursor_source_info()),
            ShellControlRequest::SetCursorSource { source } => {
                self.shell_control_set_cursor_source(&source, actor)
            }
            ShellControlRequest::SetCursorSize { size } => self.shell_control_set_cursor_size(size, actor),
            ShellControlRequest::GetOutputs => ShellControlResponse::outputs(self.shell_control_get_outputs()),
            ShellControlRequest::SetOutputMode { output, width, height, refresh_mhz } => {
                self.shell_control_set_output_mode(&output, width, height, refresh_mhz)
            }
            ShellControlRequest::SetOutputScale { output, scale_pct } => {
                self.shell_control_set_output_scale(&output, scale_pct, actor)
            }
            ShellControlRequest::SetTheme { theme } => self.shell_control_set_theme(&theme, actor),
            ShellControlRequest::TakeShellIntents => self.shell_control_take_shell_intents(),
            // A2: both handlers live in `codrive_ops.rs` — see that module's
            // doc for the trust boundary and the two actions' semantics.
            // Human-only (never `agent_allowed`), so `actor` is always
            // `Human` here — no need to thread it further.
            ShellControlRequest::CodriveStatus => self.shell_control_codrive_status(),
            ShellControlRequest::CodriveDrive { action } => self.shell_control_codrive_drive(&action),
            ShellControlRequest::SetSessionLocked { locked } => {
                self.shell_control_set_session_locked(locked)
            }
        }
    }

    /// D9-bug3/D9-bug4: record the shell's lock state and re-settle everything
    /// that depends on it.
    ///
    /// Audited unconditionally, unlike the cursor/theme setters which only
    /// note whether they changed anything: this is the compositor's record of
    /// when the machine was locked and unlocked, and an unlock that comp
    /// already believed had happened is exactly the line a reader of that
    /// trail would most want present. `changed` is still reported so a
    /// re-announcement is distinguishable from a real transition.
    fn shell_control_set_session_locked(&mut self, locked: bool) -> ShellControlResponse {
        let changed = self.set_session_locked(locked);
        self.shell_control
            .record("set_session_locked", Some(format!("locked={locked} changed={changed}")));
        ShellControlResponse::ok()
    }

    /// CUR-3: change the human pointer's size live, then persist the choice.
    ///
    /// Same apply-first-persist-second ordering, and the same reasoning, as
    /// [`Self::shell_control_set_cursor_source`] directly below: the switch is
    /// what the caller asked for and it cannot fail, while writing the
    /// preference file can. A write failure degrades to "live now, gone after
    /// a restart" and is reported as `persisted: false` + `persist_error`,
    /// never swallowed.
    ///
    /// `size` has already been through `listener::validate`, so
    /// `cursor_size_from_wire` here cannot fail; it is re-parsed rather than
    /// passed as a `u32` because the wire type is `i64` and the parse is the
    /// boundary. A validation gap therefore lands on a real error response,
    /// not a panic and not an unvalidated write.
    fn shell_control_set_cursor_size(
        &mut self,
        size: i64,
        actor: listener::PeerAuthority,
    ) -> ShellControlResponse {
        let Some(px) = crate::cursor::source::cursor_size_from_wire(size) else {
            tracing::error!(
                "shell_control: set_cursor_size reached the main thread with a value \
                 listener::validate should have refused — refusing here too"
            );
            self.shell_control.record(
                "set_cursor_size_failed",
                Some(format!("actor={} error=invalid_cursor_size", actor.audit_label())),
            );
            return ShellControlResponse::err("invalid_cursor_size");
        };

        let changed = self.set_cursor_size(px);

        let (persisted, persist_error) = match crate::cursor::persist::store_size(px) {
            Ok(path) => {
                tracing::debug!(path = %path.display(), "cursor: size preference stored");
                (true, None)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cursor: the size switch is live but could not be persisted — it will \
                     revert at the next compositor start"
                );
                (false, Some(e))
            }
        };

        let mut info = self.cursor_source_info();
        info.persisted = Some(persisted);
        info.persist_error = persist_error.clone();

        // Audited: an ACTION with a user-visible effect. `effective` is in the
        // line on purpose — "the user asked for 96 and the theme drew 64" is a
        // support answer that must be recoverable from the trail, not only
        // from a live socket query. `actor` (A7c) is first in the line so a
        // reader can tell a human's own preference apart from an agent-driven
        // one without cross-referencing a second file.
        self.shell_control.record(
            "set_cursor_size",
            Some(format!(
                "actor={} size={px} effective_size={} theme={:?} changed={changed} persisted={persisted}{}",
                actor.audit_label(),
                info.effective_size,
                info.theme,
                match &persist_error {
                    Some(e) => format!(" persist_error={e:?}"),
                    None => String::new(),
                }
            )),
        );

        ShellControlResponse::cursor(info)
    }

    /// CUR-2: switch the human pointer's artwork source live, then persist
    /// the choice.
    ///
    /// Ordering matters and is deliberate: **apply first, persist second.**
    /// The switch is what the caller asked for and it cannot fail; writing
    /// the preference file can (read-only home, full disk, a service account
    /// with no `$HOME`). Persisting first would mean a failed write blocked a
    /// switch that would otherwise have worked. Doing it this way, a write
    /// failure degrades to "live now, gone after a restart" — reported
    /// honestly as `persisted: false` + `persist_error`, never swallowed.
    ///
    /// `source` has already been through `listener::validate`, so
    /// `parse_strict` here cannot fail; it is re-parsed rather than passed as
    /// an enum because the wire type is a string and the parse is the
    /// boundary. The `unreachable`-style fallback is written as a real error
    /// response, not a panic — a validation gap must not take the compositor
    /// down.
    fn shell_control_set_cursor_source(
        &mut self,
        source: &str,
        actor: listener::PeerAuthority,
    ) -> ShellControlResponse {
        let Some(requested) = crate::cursor::source::CursorSource::parse_strict(source) else {
            tracing::error!(
                "shell_control: set_cursor_source reached the main thread with a value \
                 listener::validate should have refused — refusing here too"
            );
            self.shell_control.record(
                "set_cursor_source_failed",
                Some(format!("actor={} error=invalid_cursor_source", actor.audit_label())),
            );
            return ShellControlResponse::err("invalid_cursor_source");
        };

        let changed = self.set_cursor_source(requested);

        let (persisted, persist_error) = match crate::cursor::persist::store(requested) {
            Ok(path) => {
                tracing::debug!(path = %path.display(), "cursor: preference stored");
                (true, None)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "cursor: the source switch is live but could not be persisted — it will \
                     revert at the next compositor start"
                );
                (false, Some(e))
            }
        };

        let mut info = self.cursor_source_info();
        info.persisted = Some(persisted);
        info.persist_error = persist_error.clone();

        // Audited: this is an ACTION with a real, user-visible effect, unlike
        // `list_windows`/`get_cursor_source`. The detail records the outcome
        // (including the fail-safe's effective value and a persistence
        // failure), not just the request — "what did the machine actually do"
        // is what an audit line is for. `actor` (A7c) leads the line for the
        // same reason `set_cursor_size` above records it.
        self.shell_control.record(
            "set_cursor_source",
            Some(format!(
                "actor={} requested={requested} effective={} theme={:?} changed={changed} persisted={persisted}{}",
                actor.audit_label(),
                info.source,
                info.theme,
                match &persist_error {
                    Some(e) => format!(" persist_error={e:?}"),
                    None => String::new(),
                }
            )),
        );

        ShellControlResponse::cursor(info)
    }

    /// Read-only — never audited, see this module's own doc comment.
    /// `focused` reports the HUMAN seat's (`self.seat`) current keyboard
    /// focus, never the agent seat's: this is a human-facing surface (a
    /// dock), so "which window is focused" must answer the human's own
    /// question.
    fn shell_control_list_windows(&self) -> Vec<ShellWindowInfo> {
        let focused_surface = self.seat.get_keyboard().and_then(|k| k.current_focus());
        // WM-3: minimized windows are included (`DuduclawComp::all_windows`).
        // A dock that cannot see them cannot bring them back, and this
        // compositor has no other task bar — see `crate::minimize`'s module doc
        // on the recoverability invariant.
        self.all_windows()
            .iter()
            .map(|w| {
                let (app_id, title) = codrive::window_target::window_identity(w);
                let focused = focused_surface.as_ref() == Some(w.toplevel().unwrap().wl_surface());
                ShellWindowInfo {
                    app_id,
                    title,
                    focused,
                    minimized: self.is_minimized(w),
                }
            })
            .collect()
    }

    /// Reuses `codrive::window_target::find_target_window` — the EXACT
    /// same matching policy (exact app_id first, title-prefix fallback,
    /// lowest-z-order tie-break) `codrive`'s own `activate_window` op
    /// already proved live (BUILD.md's "WP-CD4a-COMP" section) — but
    /// applies the result to the HUMAN seat (`self.seat`, never
    /// `self.agent_seat`) and audits to THIS module's own log, not
    /// `codrive`'s (see this module's own doc comment for both reasons).
    fn shell_control_focus_window(&mut self, query: String) -> ShellControlResponse {
        // WM-3: resolved against mapped AND minimized windows. `activate` on a
        // minimized window means restore-and-raise — the op's semantics are
        // unchanged ("bring this window to the front"), it is the set of
        // windows that can honestly answer it that grew.
        let candidates = self.all_windows();
        match codrive::window_target::find_target_in(&candidates, &query) {
            Some((window, matched)) => {
                let restored = self.is_minimized(&window);
                if restored {
                    self.unminimize_window(&window);
                }
                let seat = self.seat.clone();
                let serial = SERIAL_COUNTER.next_serial();
                self.focus_window(&seat, Some(&window), serial);

                let (resp, detail) = match &matched {
                    WindowMatch::AppId(id) => (
                        ShellControlResponse::focused_by_app_id(id.clone()),
                        format!("query={query:?} matched_app_id={id:?}"),
                    ),
                    WindowMatch::TitlePrefix(title) => (
                        ShellControlResponse::focused_by_title_prefix(title.clone()),
                        format!("query={query:?} matched_via=title_prefix matched_title={title:?}"),
                    ),
                };
                // WM-3: `restored` is in the audit detail because "the dock
                // un-minimized a window" and "the dock raised an already-visible
                // window" are different user actions with the same op name, and
                // the trail has to be able to tell them apart.
                let detail = format!("{detail} restored={restored}");
                tracing::info!(query = %query, detail = %detail, restored, "shell_control: focus_window — window focused");
                self.shell_control.record("focus_window", Some(detail));
                resp
            }
            None => {
                tracing::info!(query = %query, "shell_control: focus_window — no toplevel matched by app_id or title prefix");
                self.shell_control.record("focus_window_failed", Some(format!("query={query:?}")));
                ShellControlResponse::err("not_found")
            }
        }
    }

    /// WP-comp-shell-display: every real output (CD-2's shadow workspace
    /// excluded — see this module's own doc). Read-only, never audited —
    /// same "queries aren't audited, actions are" rule as `list_windows`/
    /// `get_cursor_source`.
    fn shell_control_get_outputs(&self) -> Vec<ShellOutputInfo> {
        self.space
            .outputs()
            .filter(|o| *o != &self.shadow_output)
            .filter_map(shell_output_info)
            .collect()
    }

    /// WP-comp-shell-display: an output matching `name` among the real
    /// (non-shadow) outputs — exact equality, never a substring/prefix
    /// match (coding convention #2: no unanchored matching for a routing
    /// decision).
    fn shell_control_find_output(&self, name: &str) -> Option<&Output> {
        self.space.outputs().find(|o| *o != &self.shadow_output && o.name() == name)
    }

    /// WP-comp-shell-display: `set_output_mode` — validates fully
    /// (`unknown_output` / `invalid_mode`) but never actually switches a
    /// mode on this build. See this module's own doc for the concrete,
    /// checked-not-assumed reason on each backend. Audited regardless of
    /// outcome: this is an ACTION (an attempted mutation), like
    /// `set_cursor_source`/`set_cursor_size`/`focus_window` above, even
    /// though every path today ends in a refusal — the trail is what will
    /// tell a later round how often this is actually requested.
    fn shell_control_set_output_mode(
        &mut self,
        output_name: &str,
        width: i64,
        height: i64,
        refresh_mhz: i64,
    ) -> ShellControlResponse {
        let Some(output) = self.shell_control_find_output(output_name) else {
            self.shell_control.record(
                "set_output_mode_failed",
                Some(format!("output={output_name:?} error=unknown_output")),
            );
            return ShellControlResponse::err("unknown_output");
        };

        let modes = output.modes();
        let detail = format!("output={output_name:?} width={width} height={height} refresh_mhz={refresh_mhz}");

        if !protocol::mode_request_matches(width, height, refresh_mhz, &modes) {
            self.shell_control.record("set_output_mode_failed", Some(format!("{detail} error=invalid_mode")));
            return ShellControlResponse::err("invalid_mode");
        }

        // A real, known mode of a real output — and still refused. See this
        // module's own doc ("Why `mode_switch_supported` is always `false`")
        // for exactly why neither backend can apply this from here today.
        self.shell_control.record(
            "set_output_mode_failed",
            Some(format!("{detail} error=mode_switch_unsupported")),
        );
        ShellControlResponse::err("mode_switch_unsupported")
    }

    /// WP-comp-shell-display D4b-3: `set_output_scale` — validates fully
    /// (`unknown_output` / `invalid_scale`) and now actually applies the
    /// change. See this module's own doc ("Scale, real as of D4b-3") for why
    /// this was safe to turn on: `Output::change_current_state` could always
    /// set a live `Scale` (smithay sends the wire update itself); what
    /// blocked this op was that four render-element builders across
    /// `cursor/`, `codrive/cursor.rs`, `codrive/highlight.rs`, and
    /// `codrive/mode_indicator.rs` hardcoded `Scale::from(1.0)` instead of
    /// reading the output's real one — fixed this round, consolidated onto
    /// `render::output_render_scale` as the single source every one of them
    /// now reads. Audited regardless of outcome, same reasoning as
    /// `shell_control_set_output_mode` above.
    fn shell_control_set_output_scale(
        &mut self,
        output_name: &str,
        scale_pct: i64,
        actor: listener::PeerAuthority,
    ) -> ShellControlResponse {
        let Some(output) = self.shell_control_find_output(output_name).cloned() else {
            self.shell_control.record(
                "set_output_scale_failed",
                Some(format!(
                    "actor={} output={output_name:?} error=unknown_output",
                    actor.audit_label()
                )),
            );
            return ShellControlResponse::err("unknown_output");
        };

        if !protocol::OUTPUT_SCALE_STEPS.contains(&scale_pct) {
            // Reached the main thread with a value `listener::validate`
            // should have refused — same defensive re-check shape as
            // `shell_control_set_cursor_size`'s re-parse of the size.
            tracing::error!(
                "shell_control: set_output_scale reached the main thread with an \
                 out-of-set value — refusing here too"
            );
            self.shell_control.record(
                "set_output_scale_failed",
                Some(format!(
                    "actor={} output={output_name:?} scale_pct={scale_pct} error=invalid_scale",
                    actor.audit_label()
                )),
            );
            return ShellControlResponse::err("invalid_scale");
        }

        // `Scale::Fractional` uniformly for every step, integer (100/200) or
        // not (125/150/175) — `fractional_scale()` round-trips either way,
        // and this crate's own render math only ever reads the fractional
        // value (`render::output_render_scale`), never the enum variant. A
        // client that only understands integer `wl_output.scale` sees
        // `Scale::integer_scale()` (`ceil()` of this value) via smithay's own
        // wire translation — the standard Wayland fallback, not something
        // this crate has to implement itself.
        let new_scale = OutputScale::Fractional(scale_pct as f64 / 100.0);
        output.change_current_state(None, None, Some(new_scale), None);

        // WM-1/WM-3: layer-shell surfaces and windows are laid out against
        // the output's LOGICAL geometry (`Space::output_geometry`), which
        // just changed even though the physical mode did not — logical size
        // = physical mode size / scale, so a 100% -> 200% change HALVES it.
        // Mirrors exactly what `winit_backend.rs`'s `WinitEvent::Resized`
        // handler already does after a MODE change; a scale change shrinks
        // or grows that same logical work area via a different divisor, and
        // needs the identical re-layout so the dock/panel and every mapped
        // window get a fresh `configure` at the new size instead of
        // overflowing (or under-filling) the screen until something else
        // happens to trigger a relayout.
        self.rearrange_layers();
        self.reapply_window_policy_all();

        let (persisted, persist_error) = match crate::output_prefs::store_scale_pct(output_name, scale_pct) {
            Ok(path) => {
                tracing::debug!(path = %path.display(), output = output_name, scale_pct, "display: scale preference stored");
                (true, None)
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    output = output_name,
                    "display: the scale switch is live but could not be persisted — it will \
                     revert at the next compositor start"
                );
                (false, Some(e))
            }
        };

        // Apply-first-persist-second, same ordering (and the same reasoning)
        // as `shell_control_set_cursor_source`: the switch itself cannot
        // fail, writing the preference file can, and a write failure must
        // never take back a live change that already succeeded. `actor`
        // (A7c) leads the line — this is the op "把字放大" actually drives.
        self.shell_control.record(
            "set_output_scale",
            Some(format!(
                "actor={} output={output_name:?} scale_pct={scale_pct} persisted={persisted}{}",
                actor.audit_label(),
                match &persist_error {
                    Some(e) => format!(" persist_error={e:?}"),
                    None => String::new(),
                }
            )),
        );

        // Echo the refreshed output list rather than a bare `{"ok":true}` —
        // `comp_client::set_output_scale`'s own doc already anticipated this
        // shape (`Result<Option<Vec<CompOutput>>, _>`), and it saves the
        // settings page a second round trip to see its own change take
        // effect (`scale_pct`/`modes[].current` etc. all reflect the output
        // exactly as it stands after this call, not a locally-guessed copy).
        ShellControlResponse::outputs(self.shell_control_get_outputs())
    }

    /// D2: switch comp's own decoration appearance live, then audit the
    /// outcome. `theme` has already been through `listener::validate`, so
    /// `Theme::parse_strict` here cannot fail; it is re-parsed rather than
    /// passed as an enum for the same reason `shell_control_set_cursor_source`
    /// re-parses `source` — the wire type is a string and the parse is the
    /// boundary, so a validation gap lands on a real error response, not a
    /// panic. Unlike the cursor `set_*` ops there is nothing to persist (see
    /// `crate::decor::Theme`'s doc), and the success reply is deliberately the
    /// bare `{"ok":true}` shape (`ShellControlResponse::ok`) — the shell's own
    /// client only reads `ok`.
    fn shell_control_set_theme(
        &mut self,
        theme: &str,
        actor: listener::PeerAuthority,
    ) -> ShellControlResponse {
        let Some(requested) = crate::decor::Theme::parse_strict(theme) else {
            tracing::error!(
                "shell_control: set_theme reached the main thread with a value \
                 listener::validate should have refused — refusing here too"
            );
            self.shell_control.record(
                "set_theme_failed",
                Some(format!("actor={} error=invalid_theme", actor.audit_label())),
            );
            return ShellControlResponse::err("invalid_theme");
        };

        let changed = self.set_theme(requested);

        // Audited: an ACTION with a real, user-visible effect, same as
        // `set_cursor_source`/`set_cursor_size` above. `actor` (A7c) leads
        // the line for the same reason.
        self.shell_control.record(
            "set_theme",
            Some(format!("actor={} requested={} changed={changed}", actor.audit_label(), requested.as_str())),
        );

        ShellControlResponse::ok()
    }

    /// A1: drains [`DuduclawComp::pending_shell_intents`] — read-and-clear,
    /// never a re-readable snapshot, matching the op's own "the shell pulls
    /// each gesture exactly once" contract.
    ///
    /// Only audited when the drain is non-empty — see
    /// [`ShellControlRequest::TakeShellIntents`]'s own doc for why an empty
    /// poll (the overwhelming majority, at a ~200ms poll interval) must NOT
    /// leave a line, while a genuine drained gesture still does.
    fn shell_control_take_shell_intents(&mut self) -> ShellControlResponse {
        let drained: Vec<ShellIntent> = self.pending_shell_intents.drain(..).collect();
        if !drained.is_empty() {
            let names: Vec<&str> = drained.iter().map(|i| i.as_str()).collect();
            self.shell_control.record("take_shell_intents", Some(format!("intents={names:?}")));
        }
        ShellControlResponse::intents(drained.iter().map(|i| i.as_str().to_string()).collect())
    }

    /// A1: pushes one [`ShellIntent`] onto the pending queue, enforcing
    /// [`MAX_PENDING_SHELL_INTENTS`] by dropping the OLDEST entry rather than
    /// refusing the new one — a global hotkey the human just pressed is the
    /// freshest signal and the one most likely to still matter; a queue this
    /// deep only ever fills up when the shell has stopped polling entirely
    /// (crashed, restarting), and in that case every queued entry is already
    /// stale.
    ///
    /// Called from `input.rs`'s human keyboard filter closure ONLY — see that
    /// call site's own comment for why an agent-injected key event can never
    /// reach this method (the codrive agent seat's keyboard filter is a
    /// separate, unconditional-forward closure that never routes through
    /// `process_input_event` at all).
    pub(crate) fn push_shell_intent(&mut self, intent: ShellIntent) {
        if self.pending_shell_intents.len() >= MAX_PENDING_SHELL_INTENTS {
            let dropped = self.pending_shell_intents.pop_front();
            tracing::warn!(
                dropped = ?dropped.map(ShellIntent::as_str),
                pushed = intent.as_str(),
                cap = MAX_PENDING_SHELL_INTENTS,
                "shell_control: pending shell-intent queue full — the shell has stopped polling \
                 take_shell_intents; dropping the OLDEST entry to make room"
            );
        }
        self.pending_shell_intents.push_back(intent);
    }
}

/// WP-comp-shell-display: converts one real `Output` into its
/// `get_outputs` wire row. `None` only when the output has no
/// `current_mode()` yet — both backends always call `change_current_state`
/// with a mode before mapping the output into `space` (this module's own
/// doc traces exactly where), so this is not reachable in practice; skipping
/// rather than reporting fabricated `0×0` data is the honest choice if it
/// ever is.
fn shell_output_info(output: &Output) -> Option<ShellOutputInfo> {
    let current = output.current_mode()?;
    let preferred = output.preferred_mode();
    let physical = output.physical_properties();

    let modes = output
        .modes()
        .into_iter()
        .map(|m| ShellOutputMode {
            width: m.size.w as i64,
            height: m.size.h as i64,
            refresh_mhz: m.refresh as i64,
            preferred: Some(m) == preferred,
            current: m == current,
        })
        .collect();

    Some(ShellOutputInfo {
        name: output.name(),
        description: output.description(),
        make: physical.make,
        model: physical.model,
        width: current.size.w as i64,
        height: current.size.h as i64,
        refresh_mhz: current.refresh as i64,
        // Never a float on the wire — same reasoning `SetCursorSize` gives
        // for using `i64` throughout this socket.
        scale_pct: (output.current_scale().fractional_scale() * 100.0).round() as i64,
        physical_width_mm: physical.size.w as i64,
        physical_height_mm: physical.size.h as i64,
        modes,
        // Always false — see this module's own doc for the concrete,
        // checked-not-assumed reason on each backend.
        mode_switch_supported: false,
    })
}
