// The "there is no audio here, and I will not pretend otherwise" backend —
// D5 (2026-08-24).
//
// ── Why a backend at all, rather than making `select_backend` fallible ───
// Every caller in this crate takes a `(Box<dyn AudioBackend>,
// AudioBackendKind)` pair and runs one verb on a background thread. Turning
// `select_backend()` into a `Result` would push a second failure shape into
// every one of those call sites (a selection error AND a call error), for a
// case the UI already has to render anyway. A backend whose every verb
// returns the real probe failure keeps ONE error path, and keeps the error
// TEXT — `UnavailableAudioBackend` carries the exact reason `wpctl` gave, so
// a support session reading the journal sees "failed to spawn wpctl: No such
// file or directory" rather than a generic shrug this module invented.
//
// ── Why this exists instead of falling back to `FakeAudioBackend` ────────
// Because PipeWire is in the image now (appliance/mkosi.conf, D5 wave). Up
// to Shell-S4 a Linux probe failure meant "this image has no audio stack at
// all", and a demo slider was a defensible placeholder. On a duty box whose
// image DOES ship PipeWire, the same failure means something is wrong with
// THIS MACHINE — a wedged wireplumber, a missing sink, a session that never
// started the daemon — and a slider that moves and changes nothing would
// hide exactly the fault the operator needs to see. See `mod.rs`'s header
// comment (D5 section) for the full argument, and
// `settings::sound_page`'s own header comment for the identical reasoning
// applied one layer up.
//
// Nothing here spawns a process, touches a socket, or sleeps: the decision
// was already made once by `select_backend()`, and re-probing per call would
// turn every slider drag into a fresh subprocess spawn against a binary
// known not to work.

use super::{AudioBackend, AudioError, OutputDevice, VolumeState};

pub(crate) struct UnavailableAudioBackend {
    /// The reason the probe gave, verbatim. Cloned into every error rather
    /// than moved, because one selection can serve many calls.
    reason: String,
}

impl UnavailableAudioBackend {
    pub(crate) fn new(reason: String) -> Self {
        Self { reason }
    }

    fn err<T>(&self) -> Result<T, AudioError> {
        Err(AudioError::Unavailable(self.reason.clone()))
    }
}

impl AudioBackend for UnavailableAudioBackend {
    fn get_volume(&self) -> Result<VolumeState, AudioError> {
        self.err()
    }

    fn set_volume(&self, _pct: u8) -> Result<VolumeState, AudioError> {
        self.err()
    }

    fn toggle_mute(&self) -> Result<VolumeState, AudioError> {
        self.err()
    }

    /// An `Err`, NOT `Ok(vec![])`. The difference is the whole point of the
    /// trait returning a `Result<Vec<_>>`: "I asked and this machine has no
    /// sinks" is a fact the settings page states plainly, while "I could not
    /// ask" is a fault it reports. Collapsing the two here would make a
    /// broken audio stack look like a machine with no speakers.
    fn list_outputs(&self) -> Result<Vec<OutputDevice>, AudioError> {
        self.err()
    }

    fn set_default_output(&self, _id: u32) -> Result<Vec<OutputDevice>, AudioError> {
        self.err()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> UnavailableAudioBackend {
        UnavailableAudioBackend::new("failed to spawn wpctl: No such file or directory".to_string())
    }

    /// Every verb must fail. A backend that answered even one of them
    /// "successfully" would let a caller believe audio partially works.
    #[test]
    fn every_verb_fails() {
        let b = backend();
        assert!(b.get_volume().is_err());
        assert!(b.set_volume(50).is_err());
        assert!(b.toggle_mute().is_err());
        assert!(b.list_outputs().is_err());
        assert!(b.set_default_output(42).is_err());
    }

    /// The probe's own words survive to the error, so the journal names the
    /// real fault instead of a generic message invented here.
    #[test]
    fn the_probe_reason_is_carried_verbatim_into_every_error() {
        let b = backend();
        for e in [
            b.get_volume().unwrap_err(),
            b.set_volume(50).unwrap_err(),
            b.toggle_mute().unwrap_err(),
            b.list_outputs().unwrap_err(),
            b.set_default_output(1).unwrap_err(),
        ] {
            let AudioError::Unavailable(reason) = e;
            assert_eq!(reason, "failed to spawn wpctl: No such file or directory");
        }
    }

    /// Listing must not degrade into "successfully found nothing" — see
    /// `list_outputs`' own doc comment.
    #[test]
    fn listing_reports_a_failure_not_an_empty_machine() {
        assert!(backend().list_outputs().is_err(), "an empty Ok would read as 'this box has no speakers'");
    }
}
