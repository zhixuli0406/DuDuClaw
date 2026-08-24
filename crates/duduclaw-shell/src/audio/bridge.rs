// The one gpui-touching file in `crate::audio` — D5 (2026-08-24).
//
// ── Why it moved here from `overlay::controlcenter` ─────────────────────
// Shell-S4 put `kick_off_audio_call` inside ControlCenter because
// ControlCenter was the only surface that touched audio. D5 gives 系統設定 ›
// 聲音 real controls over the same backend, so there are now two callers and
// the choice is between exposing a private fn from an overlay module or
// giving the audio module its own bridge. This is the bridge. Behaviour is
// unchanged from the version that lived in ControlCenter — same background
// thread, same `std::sync::mpsc` handoff, same 20ms `cx.spawn` poll, same
// in-flight guard checked FIRST.
//
// ── The discipline this file deliberately breaks, and why that is fine ──
// `mod.rs`'s header comment holds the audio module to "no gpui anywhere in
// the state/backend layer", so that `AudioUiState` and the backends stay
// constructible and testable without a window. That still holds: this file
// contains NO state and NO backend logic, only the thread/timer plumbing
// that carries one backend call to one view update. It is the audio twin of
// `settings::spawn_rpc`, which is exactly the same kind of module-local
// bridge for that directory's twelve call sites.
//
// ── Why not just reuse `settings::spawn_rpc` ────────────────────────────
// Because the audio calls have a guard `spawn_rpc` has no concept of. A fast
// slider drag fires mouse-move ticks faster than one `wpctl` round-trip
// settles, so a bare "spawn a thread per event" bridge would queue dozens of
// subprocess spawns behind one drag. `AudioUiState.in_flight` collapses them
// (see that field's own doc comment), and the guard has to live in the
// dispatch path, not at the call sites, or one forgetful call site
// reintroduces the pile-up.

use gpui::Context;

use super::{AudioBackend, AudioError, AudioUiState, VolumeState};
use crate::ShellView;

/// Dispatches the eager FIRST read, at most once per process, from a render
/// pass. Every surface that displays a volume calls this while rendering;
/// the `probe_started` flag (not `backend_kind`) is what makes a repaint
/// cheap — see that field's own doc comment for why a failing backend must
/// not re-dispatch forever.
///
/// This is deliberately a render-time side effect, which this crate normally
/// avoids (`oobe::steps::network`'s "I/O is always click-triggered" note).
/// The settings app already made the same exception for the same reason
/// (`settings::*_page::ensure_loaded`, all called from `render`): a control
/// that shows a number cannot wait for the operator to interact before it
/// learns what the number is — before D5, ControlCenter's answer to that was
/// to display a seeded 62% until the first drag, which is precisely the
/// invented value this round removed. Idempotence, not abstinence, is what
/// keeps a render-time read safe.
pub(crate) fn ensure_volume_probed(cx: &mut Context<ShellView>) {
    cx.spawn(async move |weak, cx| {
        let _ = weak.update(cx, |view: &mut ShellView, cx: &mut Context<ShellView>| {
            if !claim_probe(&mut view.audio_ui) {
                return;
            }
            kick_off_audio_call(view, cx, None, |backend| backend.get_volume());
        });
    })
    .detach();
}

/// Dispatches ONE `AudioBackend` call on a background thread and bridges its
/// result back to `ShellView` — the same background-thread ->
/// `std::sync::mpsc` -> `cx.spawn` poll-loop pattern `oobe::steps::network`'s
/// `kick_off_scan`/`kick_off_connect` established (see that file's own header
/// comment for why: no `reqwest`/`tokio` in this crate, and gpui's main
/// thread must never block on real I/O — a subprocess spawn, here, rather
/// than a network call).
///
/// Shared by every verb that settles into `AudioUiState` (`set_volume` from
/// ControlCenter's drag, `toggle_mute` from its speaker glyph and from 聲音's
/// mute row, `get_volume` from the eager probe and 聲音's 重新整理) rather
/// than duplicated per call site, since they differ only in which method the
/// closure invokes.
///
/// The in-flight guard is checked HERE, before `AudioUiState::begin` even
/// runs — this is the authoritative guard `AudioUiState.in_flight`'s own doc
/// comment refers to, not the cosmetic `busy` gate a renderer may also apply.
pub(crate) fn kick_off_audio_call(
    view: &mut ShellView,
    cx: &mut Context<ShellView>,
    optimistic_pct: Option<u8>,
    call: impl FnOnce(&dyn AudioBackend) -> Result<VolumeState, AudioError> + Send + 'static,
) {
    if view.audio_ui.in_flight {
        return;
    }
    view.audio_ui.begin(optimistic_pct);
    cx.notify();

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let (backend, kind) = super::select_backend();
        let result = call(backend.as_ref());
        // A send failure means the view is gone (window closed mid-call);
        // dropping the result is correct and must not panic the thread.
        let _ = tx.send((kind, result));
    });

    // 20ms poll interval, shorter than `steps::network`'s 50ms — a volume
    // backend call is a local sub-10ms round-trip, not real network I/O, so
    // polling more often keeps a drag feeling responsive without adding
    // meaningful busy-work (this timer only runs while ONE call is
    // in-flight, never continuously).
    cx.spawn(async move |weak, cx| loop {
        match rx.try_recv() {
            Ok((kind, result)) => {
                let _ = weak.update(cx, |view: &mut ShellView, cx: &mut Context<ShellView>| {
                    view.audio_ui.settle(kind, result);
                    cx.notify();
                });
                break;
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
        }
        cx.background_executor().timer(std::time::Duration::from_millis(20)).await;
    })
    .detach();
}

/// The pure half of [`ensure_volume_probed`], so the "at most once" contract
/// is testable without a window (this crate has no headless UI harness —
/// the gap `surface.rs`'s own header comment documents).
///
/// Returns whether THIS call is the one that should dispatch the read, and
/// marks the state as probed when it is.
fn claim_probe(state: &mut AudioUiState) -> bool {
    if state.probe_started {
        return false;
    }
    state.probe_started = true;
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::AudioBackendKind;

    #[test]
    fn the_first_render_claims_the_probe_and_later_ones_do_not() {
        let mut state = AudioUiState::default();
        assert!(claim_probe(&mut state), "the first render must dispatch the read");
        assert!(!claim_probe(&mut state), "a repaint must not re-dispatch");
        assert!(!claim_probe(&mut state));
    }

    /// A backend that keeps failing must NOT cause a fresh subprocess spawn
    /// on every frame — `backend_kind` being set is not what stops the
    /// re-dispatch, `probe_started` is.
    #[test]
    fn a_failed_probe_is_not_retried_every_frame() {
        let mut state = AudioUiState::default();
        assert!(claim_probe(&mut state));
        state.settle(AudioBackendKind::Unavailable, Err(AudioError::Unavailable("no pipewire".to_string())));
        assert!(!claim_probe(&mut state), "a failed first read must not turn every repaint into a spawn");
    }
}
