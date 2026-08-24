//! CD-2 codrive — shadow workspace (headless output + PiP旁觀).
//! DESIGN-codrive-desktop-2026-08.md §3.3.4 ("影子工作區"): "session 級
//! headless output…agent 的視窗在影子 output 上執行；人要看時，影子 output
//! 整幅渲染成人桌面上的一個視窗". Task brief "WP-CD2-shadow" scopes this
//! specifically to headless output + PiP — NOT the full Shadow/Watch/
//! Takeover state machine `mod.rs`'s own module doc already flags as
//! "CD-2+" scope beyond CD-0/CD-1.
//!
//! ## What this file adds
//! 1. A second `smithay::output::Output` ("duduclaw-shadow-0") that is
//!    never bound to any real display backend — `winit_backend.rs` never
//!    calls `render_output(&shadow_output, …)` against the real host
//!    window, only against an offscreen GLES texture (see
//!    `codrive_render_pip` below). Mapped into the SAME `Space` as the
//!    real "winit" output, but at [`SHADOW_ORIGIN`] — far enough away that
//!    `Space`'s own per-output geometry filtering
//!    (`space::space_render_elements` → `Space::render_elements_for_region`,
//!    smithay 0.7.0 `src/desktop/space/mod.rs`) gives the two outputs a
//!    hard, structural, zero-overlap isolation for free: a window mapped
//!    at the shadow origin is never a candidate for the main output's own
//!    render pass, and vice versa. No manual "is this a shadow window,
//!    skip it" filtering needed anywhere else in the crate — this is the
//!    same trick real multi-monitor desktops use for extended-desktop
//!    layouts, repurposed here for workspace isolation instead of physical
//!    monitors.
//! 2. `{"op":"shadow","enable":true|false}` handling
//!    (`DuduclawComp::codrive_set_shadow`) — moves the agent-focused window
//!    to/from the shadow region.
//! 3. PiP rendering (`DuduclawComp::codrive_render_pip`) — a second,
//!    offscreen `render_output` pass targeting a persistent GLES texture
//!    bound via `Offscreen<GlesTexture>`/`Bind<GlesTexture>`, then wrapped
//!    as a `TextureRenderElement` and composited into the MAIN output's
//!    custom-elements slice by `winit_backend.rs` — the same "compositor-
//!    internal render element" convention `cursor.rs`/`highlight.rs`
//!    already established, extended from zero-texture solid-color bars to
//!    a real sampled texture (via smithay's `render_elements!` macro,
//!    `CodriveElement` in `winit_backend.rs`, combining both element
//!    kinds into one enum for `render_output`'s single `custom_elements`
//!    slice).
//!
//! ## Design assumptions this round made (task brief: "文件沒定義的細節做
//! MVP、誠實列假設" — DESIGN §3.3.4 doesn't spell these out)
//! - **Fixed geometry, no dynamic resize**: unlike the real "winit" output
//!   (which tracks `WinitEvent::Resized`), the shadow output's mode and the
//!   PiP thumbnail's size/corner are both fixed constants (task brief
//!   explicitly allows "MVP 可接受固定角落、固定縮放").
//! - **One shadow region, not per-window**: DESIGN §7 R-C2 already ruled
//!   out per-window off-screening as having "無先例" and scoped shadow
//!   workspace to session-level headless output — this file honors that:
//!   every window moved into shadow lands at the exact same
//!   [`SHADOW_ORIGIN`] point (matches the crate's pre-existing
//!   single-window-at-a-time assumption — every brand-new toplevel already
//!   maps to a fixed `(0, 0)` on the main output too, see
//!   `handlers/xdg_shell.rs::new_toplevel`).
//! - **Handback semantics (task brief item 4, DESIGN doesn't define this
//!   exact case)**: "接手＝shadow 視窗搬回主 output 並列印稽核事件" — MVP
//!   reading applied here: both Super+Esc emergency stop (DESIGN §6 red
//!   line 3, a global kill switch — task brief's own example: "急停一樣殺
//!   shadow session") and Super+Enter human resume trigger
//!   [`DuduclawComp::codrive_handback_shadow_if_active`], which
//!   deactivates shadow mode and moves every shadow-region window back to
//!   the main output's `(0, 0)`, auditing the transition either way.
//! - **Freeze scope was deliberately unchanged by the CD-2 shadow round —
//!   now closed by WP-CD2-freeze-scope** (this file's `freeze_bypass_
//!   decision`/`is_freeze_bypass_eligible`, below). DESIGN §3.1 point 2/3:
//!   "並行零干擾" — shadow-targeted commands stay live even while the human's
//!   REAL desktop freezes the shared agent seat, but ONLY the commands
//!   whose actual target is confirmed inside the shadow output; the
//!   `Shadow` toggle op itself never bypasses, in either direction (§3.1
//!   point 3's own worked example: "凍結中不可進入 shadow"). `mod.rs`'s
//!   `handle_agent_inject` calls into this file's decision function instead
//!   of applying its frozen gate uniformly.
//! - **PiP visibility**: the PiP element only renders while
//!   `codrive_shadow_active` is true (task brief allows this — no
//!   DESIGN-mandated "always-on placeholder frame" requirement found).

use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            damage::OutputDamageTracker,
            element::{solid::SolidColorRenderElement, texture::TextureRenderElement, Id, Kind},
            gles::{GlesRenderer, GlesTexture},
            Bind, Offscreen, Renderer,
        },
    },
    output::{Mode, Output, PhysicalProperties, Subpixel},
    reexports::wayland_server::DisplayHandle,
    utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform},
};

use crate::state::DuduclawComp;

use super::protocol::InjectCmd;

/// Logical-space origin of the shadow workspace's headless output. See this
/// file's module doc point 1 for why this location (not `(0, 0)`, not
/// anywhere near the real output) is what makes the isolation structural
/// rather than filtered-by-hand. `pub` — `handlers/xdg_shell.rs::
/// new_toplevel` reads this directly to route brand-new toplevels created
/// while shadow mode is already active straight to the shadow region.
pub const SHADOW_ORIGIN: (i32, i32) = (0, 100_000);

/// Fixed logical/physical size for the shadow output. The SHADOW output's
/// own scale specifically stays 1.0 forever: `shell_control_find_output`
/// (`shell_control/mod.rs`) explicitly excludes `DuduclawComp::
/// shadow_output` from every `get_outputs`/`set_output_scale` lookup, so
/// there is no wire path that can ever change it — logical and physical
/// pixels coincide throughout this file regardless of what scale a REAL
/// output is running at (WP-comp-shell-display D4b-3 made that a live,
/// per-output choice — see `render::output_render_scale`'s doc).
const SHADOW_SIZE: (i32, i32) = (1280, 800);

/// PiP thumbnail's fixed size on the main output, same 8:5 aspect ratio as
/// [`SHADOW_SIZE`] (1280:800 == 240:150) so the preview isn't stretched.
const PIP_SIZE: (i32, i32) = (240, 150);

/// Fixed corner margin in pixels (task brief: "固定角落").
const PIP_MARGIN: i32 = 16;

/// Creates the shadow workspace's `Output` object and registers it as a
/// `wl_output` global. Does NOT map it into `state.space` — the caller
/// (`DuduclawComp::new`) does that once it also owns `&mut Space`.
///
/// Registered as a real global (not withheld from the protocol) because
/// that matches the three prior-art headless-output implementations
/// DESIGN §2/§3.3.4 cites (wlroots headless backend, `kwin --virtual`,
/// xdg-desktop-portal's virtual monitor) — a headless output is still a
/// first-class `wl_output` in all of them. Nothing here advertises it to
/// ordinary human-launched clients as a preferred/only output; this
/// compositor's own window-placement logic (`codrive_set_shadow`,
/// `handlers/xdg_shell.rs::new_toplevel`) is what keeps it agent-only in
/// practice.
pub fn create_shadow_output(dh: &DisplayHandle) -> Output {
    let mode = Mode {
        size: SHADOW_SIZE.into(),
        refresh: 60_000,
    };
    let output = Output::new(
        "duduclaw-shadow-0".to_string(),
        PhysicalProperties {
            size: (0, 0).into(),
            subpixel: Subpixel::Unknown,
            make: "DuDuClaw".into(),
            model: "duduclaw-comp (shadow)".into(),
        },
    );
    let _global = output.create_global::<DuduclawComp>(dh);
    output.change_current_state(Some(mode), Some(Transform::Normal), None, Some(SHADOW_ORIGIN.into()));
    output.set_preferred(mode);
    output
}

impl DuduclawComp {
    /// `{"op":"shadow","enable":true|false}` (this file's module doc).
    /// Runs on the calloop main thread only, reached via
    /// `codrive::handle_agent_inject`'s `InjectCmd::Shadow` arm — the same
    /// thread that owns `self.space`.
    pub fn codrive_set_shadow(&mut self, enable: bool) {
        if self.codrive_shadow_active == enable {
            // Idempotent re-assertion (e.g. a script defensively sending
            // "shadow on" twice) — audited so it's not a silent drop, but
            // no window actually moves.
            self.codrive.record(
                if enable { "shadow_enabled" } else { "shadow_disabled" },
                Some("shadow"),
                None,
                None,
                Some("no-op — already in the requested state".into()),
            );
            return;
        }

        self.codrive_shadow_active = enable;
        // WP-CD2-freeze-scope: keep `CodriveShared::shadow_active` (the
        // socket thread's optimistic-pre-check mirror, see mod.rs) in
        // lockstep with this main-thread-only field — every write to one
        // goes through this function or `codrive_handback_shadow_if_active`
        // below, never directly.
        self.codrive.shadow_active.store(enable, std::sync::atomic::Ordering::SeqCst);

        if enable {
            self.move_agent_focused_window_to(SHADOW_ORIGIN);
            self.codrive.record("shadow_enabled", Some("shadow"), None, None, None);
        } else {
            let moved = self.move_shadow_windows_to((0, 0));
            self.codrive.record(
                "shadow_disabled",
                Some("shadow"),
                None,
                None,
                Some(format!("{moved} window(s) moved to the main output")),
            );
        }
    }

    /// Shared handback path (task brief item 4, no stricter DESIGN rule
    /// found for this exact case — see this file's module doc "Handback
    /// semantics"). Called from both `emergency_stop` (Super+Esc) and
    /// `human_resume` (Super+Enter), unconditionally (not gated on whether
    /// the seat itself was frozen) — a shadow session is a separate piece
    /// of state from the freeze flag, so both entry points must check
    /// `codrive_shadow_active` themselves rather than piggy-backing on
    /// their caller's own frozen/no-op branch. No-op (no audit line) if no
    /// shadow session was active.
    pub fn codrive_handback_shadow_if_active(&mut self, reason: &'static str) {
        if !self.codrive_shadow_active {
            return;
        }
        self.codrive_shadow_active = false;
        // WP-CD2-freeze-scope: see `codrive_set_shadow`'s matching store.
        self.codrive.shadow_active.store(false, std::sync::atomic::Ordering::SeqCst);
        let moved = self.move_shadow_windows_to((0, 0));
        self.codrive.record(
            "shadow_disabled",
            Some("shadow"),
            None,
            None,
            Some(format!("handback ({reason}) — {moved} window(s) moved to the main output")),
        );
    }

    /// Moves the window currently focused by the AGENT seat's keyboard (if
    /// any) to `dest`. Used when `{"op":"shadow","enable":true}` arrives
    /// with a window already mapped (e.g. `foot` launched before the
    /// `shadow` op, matching CD-0/CD-1's own live-run scripts) — a window
    /// created AFTER shadow mode is already active is instead routed
    /// directly by `handlers/xdg_shell.rs::new_toplevel`.
    fn move_agent_focused_window_to(&mut self, dest: (i32, i32)) {
        let Some(surface) = self.agent_seat.get_keyboard().and_then(|k| k.current_focus()) else {
            return;
        };
        let Some(window) = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &surface))
            .cloned()
        else {
            return;
        };
        self.space.map_element(window, dest, false);
        self.codrive.record("shadow_window_moved", Some("shadow"), None, None, Some("to_shadow".into()));
    }

    /// Moves every window currently mapped at [`SHADOW_ORIGIN`] to `dest`,
    /// returning how many moved. MVP note: every moved window lands at the
    /// exact same `dest` point — no multi-window tiling logic (matches
    /// this whole crate's pre-existing single-window-at-a-time assumption;
    /// see this file's module doc).
    fn move_shadow_windows_to(&mut self, dest: (i32, i32)) -> usize {
        let origin: Point<i32, Logical> = SHADOW_ORIGIN.into();
        let moving: Vec<_> = self
            .space
            .elements()
            .filter(|w| self.space.element_location(w) == Some(origin))
            .cloned()
            .collect();
        let count = moving.len();
        for window in moving {
            self.space.map_element(window, dest, false);
        }
        if count > 0 {
            self.codrive.record(
                "shadow_window_moved",
                Some("shadow"),
                None,
                None,
                Some(format!("to_main x{count}")),
            );
        }
        count
    }

    /// Offscreen-renders the shadow output's current content into a
    /// persistent GLES texture, then wraps it as a `TextureRenderElement`
    /// positioned at a fixed bottom-right corner of the MAIN output
    /// (DESIGN §3.3.4: "人要看時，影子 output 整幅渲染成人桌面上的一個視
    /// 窗…比 RDP loopback 乾淨"). Returns `None` while no shadow session is
    /// active (see module doc "PiP visibility") or if any GLES step fails
    /// (fail-open on the PREVIEW only — never blocks or drops the
    /// underlying agent session; matches `audit.rs`'s "fail-open on
    /// logging, fail-closed on authorization" split, this is the
    /// visualization-layer equivalent).
    ///
    /// `pip_texture` is caller-owned (`winit_backend.rs`'s `init_winit`
    /// closure captures, alongside the pre-existing `damage_tracker`/
    /// `output` locals) and reused across frames — allocated lazily on
    /// first use rather than every redraw, since this crate's winit
    /// backend redraws in a tight, unthrottled loop (BUILD.md's "Honest
    /// stub" notes on the base spike) and a fresh GL texture every frame
    /// would be needless churn for a texture whose size never changes.
    pub fn codrive_render_pip(
        &mut self,
        renderer: &mut GlesRenderer,
        pip_texture: &mut Option<GlesTexture>,
        pip_damage_tracker: &mut OutputDamageTracker,
        main_output_size: Size<i32, Physical>,
    ) -> Option<TextureRenderElement<GlesTexture>> {
        if !self.codrive_shadow_active {
            return None;
        }

        if pip_texture.is_none() {
            let size: Size<i32, Buffer> = SHADOW_SIZE.into();
            match renderer.create_buffer(Fourcc::Abgr8888, size) {
                Ok(tex) => *pip_texture = Some(tex),
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        "codrive: failed to allocate the shadow-workspace PiP texture — skipping this frame's PiP"
                    );
                    return None;
                }
            }
        }
        let texture = pip_texture.as_mut().expect("just ensured Some above");

        let mut target = match renderer.bind(texture) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    "codrive: failed to bind the shadow-workspace PiP texture as a render target — skipping this frame's PiP"
                );
                return None;
            }
        };

        let render_result = smithay::desktop::space::render_output::<_, SolidColorRenderElement, _, _>(
            &self.shadow_output,
            renderer,
            &mut target,
            1.0,
            0,
            [&self.space],
            &[],
            pip_damage_tracker,
            [0.05, 0.05, 0.05, 1.0],
        );
        // Release the exclusive borrow of `*pip_texture` this render pass
        // held (`target` borrows `texture`, which reborrows `*pip_texture`)
        // before reading the texture back out below.
        drop(target);

        if let Err(e) = render_result {
            tracing::warn!(
                error = ?e,
                "codrive: failed to render the shadow output into the PiP texture — skipping this frame's PiP"
            );
            return None;
        }

        let texture = pip_texture.clone().expect("just rendered into it above");
        let pip_logical: Size<i32, Logical> = PIP_SIZE.into();
        // Full native texture extent as the sample source (`src`), a
        // smaller `size` as the destination — this pair is what makes
        // `TextureRenderElement` actually downscale the whole 1280x800
        // shadow frame into a 240x150 thumbnail rather than cropping its
        // top-left corner (see this function's own reasoning, checked
        // against `TextureRenderElement::{logical_size, src, scale}` in
        // the vendored smithay 0.7.0 source before writing this — a
        // `size`-only call with no explicit `src` would default `src` to
        // `size` itself, silently turning this into a crop, not a scale).
        let full_src = Rectangle::<f64, Logical>::from_size(Size::from((SHADOW_SIZE.0 as f64, SHADOW_SIZE.1 as f64)));

        let loc_x = (main_output_size.w - PIP_SIZE.0 - PIP_MARGIN).max(0);
        let loc_y = (main_output_size.h - PIP_SIZE.1 - PIP_MARGIN).max(0);

        Some(TextureRenderElement::from_static_texture(
            Id::new(),
            renderer.context_id(),
            (loc_x as f64, loc_y as f64),
            texture,
            1,
            Transform::Normal,
            None,
            Some(full_src),
            Some(pip_logical),
            None,
            Kind::Unspecified,
        ))
    }
}

// ── WP-CD2-freeze-scope: which frozen-seat commands may bypass the
//    freeze because they're confined to the shadow output ──────────────
//
// DESIGN §3.1 point 3: the freeze gate protects the SHARED main desktop
// the instant a human touches it — it was never meant to also pause a
// shadow session running in parallel on a headless output the human can't
// see. `mod.rs`'s `handle_agent_inject` calls `is_freeze_bypass_eligible`
// in place of applying its frozen gate unconditionally.
//
// The actual policy (`freeze_bypass_decision`) is a PURE function — no
// `&DuduclawComp` — specifically so it's unit-testable without
// constructing a full compositor state (`EventLoop`+`Display`+
// `DuduclawComp::new`), which this crate has never done in a unit test;
// see BUILD.md's "Honest stub" sections for why live/container
// verification, not unit tests, is this crate's usual tool for anything
// touching real seat/space state. `is_freeze_bypass_eligible` is the thin,
// untested wrapper that extracts live agent-seat facts from a real `comp`.

/// Whether logical-space point `(x, y)` falls within the shadow output's
/// bounds ([`SHADOW_ORIGIN`] .. `SHADOW_ORIGIN + SHADOW_SIZE`, half-open).
/// A `Move`/`Highlight` coordinate landing here is what turns "shadow mode
/// happens to be on" into "this op is actually confined to the shadow
/// output" — the distinction WP-CD2-freeze-scope's invariant (c) requires.
fn point_in_shadow_bounds(x: f64, y: f64) -> bool {
    let (ox, oy) = SHADOW_ORIGIN;
    let (sw, sh) = SHADOW_SIZE;
    x >= ox as f64 && x < (ox + sw) as f64 && y >= oy as f64 && y < (oy + sh) as f64
}

/// Whether the WHOLE rectangle `(x, y, w, h)` — not just its origin corner
/// — sits inside the shadow output's bounds. Used for `highlight`: a box
/// that started inside shadow bounds but extended out into the main
/// output's space would still be a visible leak onto the frozen desktop.
fn rect_in_shadow_bounds(x: f64, y: f64, w: f64, h: f64) -> bool {
    point_in_shadow_bounds(x, y) && point_in_shadow_bounds(x + w, y + h)
}

/// WP-CD2-freeze-scope's actual policy: given the three live facts a
/// freeze-bypass decision depends on, is `cmd` allowed through a freeze?
/// Fail-closed on every axis — not just "is shadow active", but "is THIS
/// op's own target confirmed inside it". The `Shadow` toggle itself NEVER
/// bypasses, in either direction: DESIGN's own worked example is "凍結中不
/// 可進入 shadow" (agent can't escape a freeze by entering shadow), and by
/// the same fail-closed reading, disabling shadow — which moves a window
/// onto the main output, exactly what a freeze must prevent — is not an
/// agent-initiated escape hatch either; only the human-triggered handback
/// path (`codrive_handback_shadow_if_active`, called from `emergency_stop`/
/// `human_resume`, never from this decision) may do that while frozen.
pub(super) fn freeze_bypass_decision(
    shadow_active: bool,
    cmd: &InjectCmd,
    agent_pointer_pos: Option<(f64, f64)>,
    agent_keyboard_focus_in_shadow: bool,
) -> bool {
    if !shadow_active {
        return false;
    }
    match cmd {
        InjectCmd::Shadow { .. } => false,
        InjectCmd::Move { x, y } => point_in_shadow_bounds(*x, *y),
        InjectCmd::Button { .. } => agent_pointer_pos.is_some_and(|(x, y)| point_in_shadow_bounds(x, y)),
        InjectCmd::Key { .. } | InjectCmd::KeyName { .. } | InjectCmd::Text { .. } => agent_keyboard_focus_in_shadow,
        InjectCmd::Highlight { x, y, w, h, .. } => rect_in_shadow_bounds(*x, *y, *w, *h),
        // Never actually reach `handle_agent_inject` (`listener.rs`
        // answers them synchronously) — listed explicitly, not via a `_`
        // wildcard, so a future new `InjectCmd` variant fails this match at
        // compile time instead of silently inheriting a bypass by default.
        // WP-CD4b-fix (B3): `WindowGeometry` joins this group for the same
        // reason — the socket thread answers it over its own oneshot query
        // bridge, so it never reaches `handle_agent_inject`. `false` here
        // is not a denial of the query (nothing consults this function for
        // it); it is the honest "this is not an injection that could be
        // confined to the shadow output" answer, and it keeps the match
        // exhaustive-without-wildcard so the next new variant still has to
        // be thought about.
        InjectCmd::Resume | InjectCmd::Status | InjectCmd::RotateToken | InjectCmd::WindowGeometry { .. } => false,
        // CD-3: never bypass-eligible, same reasoning as `Shadow` above —
        // `TakeOver` is itself a control-plane transition (not something
        // "confined to the shadow output" even applies to), and `Watch`'s
        // toggle shouldn't be smugglable through a freeze either. See
        // `takeover.rs`'s module doc for why an active takeover ALSO forces
        // every OTHER op's bypass to `false` — that's `mod.rs`'s job
        // (`!self.codrive_takeover_active` gates the call into this
        // function at all), not this match's.
        InjectCmd::TakeOver { .. } | InjectCmd::Watch { .. } => false,
        // WP-CD4a-COMP: `activate_window` is denied outright while frozen,
        // same fail-closed reasoning as `Shadow`/`TakeOver`/`Watch` above —
        // it's a focus-changing control action over the whole `space`, not
        // something "confined to the shadow output" naturally applies to
        // (unlike `Move`/`Button`/`Highlight`, it carries no target
        // coordinate to confirm against `SHADOW_ORIGIN`'s bounds at all).
        // The task brief for this op is explicit: "凍結/takeover 中拒絕
        // （照既有 op 閘）" — reuse the standard frozen gate, no new bypass
        // carve-out.
        InjectCmd::ActivateWindow { .. } => false,
    }
}

/// Whether the agent seat's CURRENT keyboard focus surface belongs to a
/// window mapped at exactly [`SHADOW_ORIGIN`] — the same lookup
/// `move_agent_focused_window_to` already does, reused read-only here. No
/// focus at all, or focus on a window not at the shadow origin, both
/// return `false` (fail-closed) — deliberately never falls back to
/// "shadow is active, so assume yes."
fn agent_keyboard_focus_is_shadowed(comp: &DuduclawComp) -> bool {
    let Some(surface) = comp.agent_seat.get_keyboard().and_then(|k| k.current_focus()) else {
        return false;
    };
    let origin: Point<i32, Logical> = SHADOW_ORIGIN.into();
    comp.space
        .elements()
        .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == &surface))
        .is_some_and(|w| comp.space.element_location(w) == Some(origin))
}

/// Thin wrapper: extracts live agent-seat facts from `comp` and defers to
/// [`freeze_bypass_decision`] for the actual policy. Called from
/// `handle_agent_inject` (`mod.rs`) — the only real caller.
pub(super) fn is_freeze_bypass_eligible(comp: &DuduclawComp, cmd: &InjectCmd) -> bool {
    let agent_pointer_pos = comp.agent_seat.get_pointer().map(|p| {
        let loc = p.current_location();
        (loc.x, loc.y)
    });
    freeze_bypass_decision(
        comp.codrive_shadow_active,
        cmd,
        agent_pointer_pos,
        agent_keyboard_focus_is_shadowed(comp),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shadow_origin_and_size_share_an_aspect_ratio_with_pip_size() {
        // Not a hard protocol requirement, but a stretched preview would be
        // an obvious visual bug — pin the 8:5 ratio both constants share so
        // a future edit to one alone fails a test instead of silently
        // distorting the PiP.
        let shadow_ratio = SHADOW_SIZE.0 as f64 / SHADOW_SIZE.1 as f64;
        let pip_ratio = PIP_SIZE.0 as f64 / PIP_SIZE.1 as f64;
        assert!((shadow_ratio - pip_ratio).abs() < 1e-9);
    }

    #[test]
    fn shadow_origin_is_far_from_any_realistic_main_output_rect() {
        // Structural isolation depends on this margin exceeding any
        // plausible main-output height — guard against a future edit
        // shrinking it back into overlap range by accident.
        assert!(SHADOW_ORIGIN.1 >= 10_000);
    }

    // ── WP-CD2-freeze-scope: `freeze_bypass_decision` / bounds helpers ──

    /// A point safely inside the shadow output (an arbitrary offset from
    /// its origin, well short of `SHADOW_SIZE`'s edge).
    fn inside_shadow() -> (f64, f64) {
        (100.0, SHADOW_ORIGIN.1 as f64 + 100.0)
    }

    /// A point at a typical main-output coordinate — nowhere near the
    /// shadow region's `y >= 100_000` bound.
    fn on_main_output() -> (f64, f64) {
        (100.0, 100.0)
    }

    #[test]
    fn point_in_shadow_bounds_accepts_the_origin_corner_and_interior() {
        let (ox, oy) = SHADOW_ORIGIN;
        assert!(point_in_shadow_bounds(ox as f64, oy as f64), "the origin corner itself is inside (half-open lower bound)");
        let inside = inside_shadow();
        assert!(point_in_shadow_bounds(inside.0, inside.1));
    }

    #[test]
    fn point_in_shadow_bounds_rejects_the_far_edge_and_main_output() {
        let (ox, oy) = SHADOW_ORIGIN;
        let (sw, sh) = SHADOW_SIZE;
        assert!(
            !point_in_shadow_bounds((ox + sw) as f64, (oy + sh) as f64),
            "origin + size is exactly OUT of bounds (half-open upper bound)"
        );
        assert!(!point_in_shadow_bounds(ox as f64 + 10.0, (oy - 1) as f64), "one unit above the shadow region's y-origin");
        let main = on_main_output();
        assert!(!point_in_shadow_bounds(main.0, main.1), "an ordinary main-output coordinate must never read as shadow-scoped");
    }

    #[test]
    fn rect_in_shadow_bounds_requires_the_whole_rectangle_inside() {
        let (ox, oy) = SHADOW_ORIGIN;
        assert!(rect_in_shadow_bounds(ox as f64 + 10.0, oy as f64 + 10.0, 50.0, 50.0), "small rect fully inside");
        // Starts inside but its far corner (x+w, y+h) crosses the shadow
        // output's right edge — must NOT count as confined (this is
        // exactly the "highlight box leaking onto the main desktop" case
        // invariant (c) exists to prevent).
        let (sw, _sh) = SHADOW_SIZE;
        assert!(!rect_in_shadow_bounds(ox as f64 + (sw as f64) - 10.0, oy as f64 + 10.0, 50.0, 50.0));
    }

    #[test]
    fn freeze_bypass_decision_shadow_inactive_never_bypasses() {
        // Invariant (d): with no shadow session active, every op stays
        // denied regardless of its target — the pre-existing, non-shadow
        // frozen behavior this WP must leave byte-identical.
        let inside = inside_shadow();
        for cmd in [
            InjectCmd::Move { x: inside.0, y: inside.1 },
            InjectCmd::Button { btn: "left".into(), state: "press".into() },
            InjectCmd::Text { s: "x".into() },
            InjectCmd::Shadow { enable: true },
        ] {
            assert!(!freeze_bypass_decision(false, &cmd, Some(inside), true));
        }
    }

    #[test]
    fn freeze_bypass_decision_shadow_toggle_never_bypasses_either_direction() {
        // Invariant (a): "凍結中不可進入 shadow" — an agent cannot use the
        // toggle op to escape (or, by the same conservative reading,
        // re-enter) a freeze, even though a shadow session IS active.
        assert!(!freeze_bypass_decision(true, &InjectCmd::Shadow { enable: true }, None, true));
        assert!(!freeze_bypass_decision(true, &InjectCmd::Shadow { enable: false }, None, true));
    }

    #[test]
    fn freeze_bypass_decision_move_follows_target_coordinate() {
        // Invariant (c): a `move` bypasses only when ITS OWN coordinate is
        // confirmed inside the shadow output — not merely because shadow
        // mode happens to be on.
        let inside = inside_shadow();
        let main = on_main_output();
        assert!(freeze_bypass_decision(true, &InjectCmd::Move { x: inside.0, y: inside.1 }, None, false));
        assert!(!freeze_bypass_decision(true, &InjectCmd::Move { x: main.0, y: main.1 }, None, false));
    }

    #[test]
    fn freeze_bypass_decision_button_follows_live_pointer_position() {
        let cmd = InjectCmd::Button { btn: "left".into(), state: "press".into() };
        assert!(freeze_bypass_decision(true, &cmd, Some(inside_shadow()), false));
        assert!(!freeze_bypass_decision(true, &cmd, Some(on_main_output()), false));
        assert!(!freeze_bypass_decision(true, &cmd, None, false), "no known pointer position must fail closed");
    }

    #[test]
    fn freeze_bypass_decision_key_and_text_follow_keyboard_focus_flag() {
        for cmd in [
            InjectCmd::Key { keycode: 38, state: "press".into() },
            InjectCmd::KeyName { name: "enter".into(), state: "press".into() },
            InjectCmd::Text { s: "hello".into() },
        ] {
            assert!(freeze_bypass_decision(true, &cmd, None, true), "{cmd:?}: focus confirmed in shadow");
            assert!(!freeze_bypass_decision(true, &cmd, None, false), "{cmd:?}: focus NOT confirmed in shadow must fail closed");
        }
    }

    #[test]
    fn freeze_bypass_decision_highlight_follows_whole_rect() {
        let (ox, oy) = SHADOW_ORIGIN;
        let inside = InjectCmd::Highlight { x: ox as f64 + 10.0, y: oy as f64 + 10.0, w: 50.0, h: 50.0, ms: None };
        let leaking = InjectCmd::Highlight { x: ox as f64 - 10.0, y: oy as f64 + 10.0, w: 50.0, h: 50.0, ms: None };
        assert!(freeze_bypass_decision(true, &inside, None, false));
        assert!(!freeze_bypass_decision(true, &leaking, None, false));
    }

    #[test]
    fn freeze_bypass_decision_control_ops_never_bypass() {
        // These never actually reach `handle_agent_inject` in practice
        // (`listener.rs` answers them synchronously) — asserted anyway so
        // the match in `freeze_bypass_decision` stays fail-closed by
        // construction, not by accident.
        for cmd in [InjectCmd::Resume, InjectCmd::Status, InjectCmd::RotateToken] {
            assert!(!freeze_bypass_decision(true, &cmd, Some(inside_shadow()), true));
        }
    }

    #[test]
    fn freeze_bypass_decision_take_over_and_watch_never_bypass() {
        // CD-3: same fail-closed reasoning as the Shadow toggle — these are
        // control-plane transitions, not something a "confined to the
        // shadow output" check even applies to.
        let take_over = InjectCmd::TakeOver { reason: "login".to_string() };
        let watch = InjectCmd::Watch { enable: true };
        assert!(!freeze_bypass_decision(true, &take_over, Some(inside_shadow()), true));
        assert!(!freeze_bypass_decision(true, &watch, Some(inside_shadow()), true));
    }

    /// WP-CD4a-COMP: `activate_window` must never bypass a freeze, even with
    /// a shadow session active — this is the pure half of the "凍結中拒" test
    /// requirement; `codrive/tests_listener.rs`'s
    /// `frozen_denies_activate_window_before_reaching_channel` covers the
    /// socket-thread end-to-end half.
    #[test]
    fn freeze_bypass_decision_activate_window_never_bypasses() {
        let cmd = InjectCmd::ActivateWindow { app_id: "foot-A".to_string() };
        assert!(!freeze_bypass_decision(true, &cmd, Some(inside_shadow()), true));
        assert!(!freeze_bypass_decision(false, &cmd, Some(inside_shadow()), true));
    }
}
