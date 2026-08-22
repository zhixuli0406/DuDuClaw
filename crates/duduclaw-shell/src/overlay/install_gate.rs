// Flatpak install confirmation gate — WP-A4-4 (2026-08-22), closing A3's
// own stated debt.
//
// A3 wired `flatpak run` (`crate::apps::launch`) behind a Launcher result
// row's click and left INSTALL unimplemented. Install is a different class
// of action: it downloads and writes hundreds of megabytes from a remote
// the operator may never have looked at, and it changes system state that
// outlives the click. This crate's own operating rules put exactly that
// category behind a human decision ("機器做可逆的事，人做不可逆的事"), so
// there is no code path here that can reach `crate::apps::install` without
// the operator having seen — and confirmed — a sheet naming the app, the
// remote, the download size, and the destination directory.
//
// ── Deliberately pure ───────────────────────────────────────────────────
// No gpui types anywhere in this file, same "testable without a live
// window" discipline `surface.rs` / `notifications_feed.rs` / `lockscreen/
// mod.rs` all state for themselves. `overlay/launcher.rs` renders it; the
// blocking `flatpak` calls (`apps::probe_download_size`, `apps::install`)
// are dispatched from there on a background thread through this crate's
// established `std::thread::spawn` + `mpsc` + `cx.spawn` bridge. This
// struct only ever receives already-settled results.
//
// ── Honesty rules baked into the type ───────────────────────────────────
//   * `SizeProbe` has THREE states, not two — "still asking" and "could not
//     find out" are different things and the sheet says which. There is no
//     variant that carries a made-up number, so a size can only ever be
//     rendered from text `flatpak` itself printed (task brief: "拿得到就顯
//     示，拿不到就誠實留白，不要編數字").
//   * the gate can only be built from a catalog entry that already names a
//     real flatpak ref AND a real remote — it is impossible to open one for
//     something this shell has no way to install (APP-1 made that a
//     property of `CatalogApp`'s type; see the note further down).
//   * `confirm` is the ONLY way to obtain the `(remote, app_id)` pair the
//     caller needs in order to run anything, and it hands it over exactly
//     once, only from `Confirming`. Cancelling drops the whole gate, so
//     "取消＝不執行" is structural, not a convention someone has to
//     remember at the call site.
//   * `apply_install_result(Ok)` reports "已開始安裝", never "已安裝" —
//     `apps::install` forks a child and returns; completion is not
//     something this shell can observe yet.
//
// ── Icon/emoji convention ───────────────────────────────────────────────
// Nothing here introduces a pictogram of its own, before or after ICON-1.
// The sheet renders whatever the catalog entry it was opened for already
// shows in its result row — since ICON-1 (2026-08-22) that is the entry's
// board stroke icon (`crate::icons::catalog_layers`, keyed off `app_id`),
// with its `glyph` character as the fallback. Both come from the entry;
// neither is invented here. This crate still has zero emoji anywhere.
//
// ── APP-1 (2026-08-22): source type changed, behaviour did not ──────────
// `open` used to take a `fake_data::DockApp` — the canned six-entry array
// that also backed the (fake) app list, whose `flatpak_id`/`flatpak_remote`
// were `Option`s that were `None` for five of the six entries. That array
// is gone: the app list is a real enumeration now, and the installable side
// is `apps::catalog::CatalogApp`, where both fields are non-optional
// because an entry that cannot name its ref and its remote is not a catalog
// entry at all (see that module's header comment). `open` therefore no
// longer HAS a "would fail on confirm" case to defend against — the
// guarantee moved from a runtime check into the type. Every other honesty
// rule above is unchanged.

use crate::apps::catalog::CatalogApp;

/// Download size for the pending install. See this file's header comment on
/// why "unknown" is its own state rather than a zero or a blank string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SizeProbe {
    /// `flatpak remote-info --show-size` is still running.
    Probing,
    /// Exactly the text flatpak printed, never reformatted here.
    Known(String),
    /// flatpak is absent, the remote is unreachable, or the output shape
    /// wasn't recognized — the sheet says so in words.
    Unavailable,
}

/// Where the gate is in its own little lifecycle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GateStage {
    /// Waiting on the operator. The only stage `confirm` accepts.
    Confirming,
    /// `apps::install` has been spawned; awaiting its (spawn-level) result.
    Installing,
    /// The install process was started successfully. NOT "installed" — see
    /// this file's header comment.
    Started,
    /// The install could not even be started. Carries the honest reason.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InstallGate {
    /// `CatalogApp::id` — the catalog key, used for element ids and to make
    /// sure a late size-probe result is applied to the gate it belongs to
    /// (see `apply_size_probe`).
    pub app_id: &'static str,
    pub label: &'static str,
    pub glyph: &'static str,
    pub flatpak_id: &'static str,
    pub remote: &'static str,
    /// Where the app lands, in the operator's own vocabulary — supplied by
    /// the caller as `apps::INSTALL_DESTINATION_LABEL` (`/data`, the
    /// appliance's data partition). Deliberately NOT the internal
    /// `--installation=data` flag nor the `/data/flatpak` repository
    /// layout: internal implementation detail does not belong in
    /// user-facing copy. Injected rather than read here so this module
    /// stays free of environment/CLI knowledge entirely.
    pub install_root: String,
    pub size: SizeProbe,
    pub stage: GateStage,
}

impl InstallGate {
    /// Opens a gate for a catalog entry. Infallible by construction —
    /// `CatalogApp` cannot exist without a ref and a remote (see this
    /// file's APP-1 note). `install_root` is injected rather than read here
    /// so this module stays free of environment reads too.
    pub(crate) fn open(app: &'static CatalogApp, install_root: String) -> Self {
        Self {
            app_id: app.id,
            label: app.label,
            glyph: app.glyph,
            flatpak_id: app.flatpak_id,
            remote: app.flatpak_remote,
            install_root,
            size: SizeProbe::Probing,
            stage: GateStage::Confirming,
        }
    }

    /// Applies a settled size probe. `app_id` guards against a slow probe
    /// for a PREVIOUS gate landing on the one that replaced it (cancel one
    /// row, immediately open another) — a mismatched result is dropped, not
    /// shown against the wrong app. A probe that arrives after the operator
    /// already confirmed is likewise ignored: the number would be
    /// decoration at that point, and re-rendering it could make a settled
    /// sheet look like it changed its mind.
    pub(crate) fn apply_size_probe(&mut self, app_id: &str, size: Option<String>) {
        if self.app_id != app_id || self.stage != GateStage::Confirming {
            return;
        }
        self.size = match size {
            Some(text) if !text.trim().is_empty() => SizeProbe::Known(text),
            _ => SizeProbe::Unavailable,
        };
    }

    /// The operator pressed the confirm button. Returns the exact pair to
    /// hand `apps::install`, and moves to `Installing` so a second click
    /// (or a double-fire) cannot start a second install. `None` from any
    /// other stage — including a second call from `Installing` — which is
    /// what makes "one confirm, one install" a property of the type rather
    /// than of the click handler.
    pub(crate) fn confirm(&mut self) -> Option<(&'static str, &'static str)> {
        if self.stage != GateStage::Confirming {
            return None;
        }
        self.stage = GateStage::Installing;
        Some((self.remote, self.flatpak_id))
    }

    /// Settles an `apps::install` spawn. `Ok` means the process STARTED.
    pub(crate) fn apply_install_result(&mut self, result: Result<(), String>) {
        if self.stage != GateStage::Installing {
            return;
        }
        self.stage = match result {
            Ok(()) => GateStage::Started,
            Err(reason) => GateStage::Failed(reason),
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::catalog;

    fn browser() -> &'static CatalogApp {
        catalog::INSTALL_CATALOG.iter().find(|a| a.flatpak_id == "org.chromium.Chromium").expect("the one installable catalog entry must exist")
    }

    /// APP-1: there is no longer any way to construct a `CatalogApp`
    /// without a ref and a remote, so the old "refuse an app this shell
    /// cannot install" runtime check has nothing left to refuse — the
    /// guarantee is the type's now. This pins that the catalog itself keeps
    /// holding to it, which is what makes `open` infallible.
    #[test]
    fn every_catalog_entry_a_gate_could_open_names_a_real_ref_and_remote() {
        for entry in catalog::INSTALL_CATALOG {
            let gate = InstallGate::open(entry, "/data".to_string());
            assert!(!gate.flatpak_id.is_empty());
            assert!(!gate.remote.is_empty());
        }
    }

    #[test]
    fn open_carries_every_field_the_sheet_has_to_show() {
        let gate = InstallGate::open(browser(), crate::apps::INSTALL_DESTINATION_LABEL.to_string());
        assert_eq!(gate.label, "瀏覽器");
        assert_eq!(gate.flatpak_id, "org.chromium.Chromium");
        assert_eq!(gate.remote, "flathub");
        assert_eq!(gate.install_root, "/data", "the sheet must name the appliance's data partition, which is where it really lands");
        assert_eq!(gate.size, SizeProbe::Probing, "a freshly opened gate has not asked yet — it must not claim to know");
        assert_eq!(gate.stage, GateStage::Confirming, "a fresh gate offers 安裝/取消 and has run nothing");
    }

    /// Cross-package guard (A4-2/3): the confirm path must hand back the
    /// pair that `apps::install_argv` turns into a `--installation=data`
    /// argv. Asserted end-to-end here so a future refactor that routes
    /// confirm through some other command surface still trips this.
    #[test]
    fn the_confirmed_pair_produces_an_argv_that_targets_the_data_installation() {
        let mut gate = InstallGate::open(browser(), crate::apps::INSTALL_DESTINATION_LABEL.to_string());
        let (remote, app_id) = gate.confirm().expect("a Confirming gate must yield the pair");
        let argv = crate::apps::install_argv(remote, app_id).expect("both values are safe");
        assert!(
            argv.contains(&"--installation=data".to_string()),
            "an install that misses this flag fills the 5 GB root partition — see apps.rs's own section header"
        );
    }

    #[test]
    fn a_size_that_could_not_be_determined_is_unavailable_not_a_number() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.apply_size_probe(gate.app_id, None);
        assert_eq!(gate.size, SizeProbe::Unavailable);

        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.apply_size_probe(gate.app_id, Some("   ".to_string()));
        assert_eq!(gate.size, SizeProbe::Unavailable, "whitespace is not a size");
    }

    #[test]
    fn a_known_size_is_kept_verbatim() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.apply_size_probe(gate.app_id, Some("123.4 MB".to_string()));
        assert_eq!(gate.size, SizeProbe::Known("123.4 MB".to_string()));
    }

    #[test]
    fn a_size_probe_for_a_different_app_is_dropped_not_shown_against_this_one() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.apply_size_probe("some-other-app", Some("999 GB".to_string()));
        assert_eq!(gate.size, SizeProbe::Probing, "a stale probe must never label THIS app with another app's size");
    }

    /// The load-bearing test for this whole feature: nothing can obtain the
    /// install command without an explicit confirm.
    #[test]
    fn confirm_is_the_only_source_of_an_install_command() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        assert_eq!(gate.confirm(), Some(("flathub", "org.chromium.Chromium")));
        assert_eq!(gate.stage, GateStage::Installing, "the buttons must be gone once it is running — `install_sheet` renders on this");
    }

    #[test]
    fn a_second_confirm_cannot_start_a_second_install() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        assert!(gate.confirm().is_some());
        assert_eq!(gate.confirm(), None, "a double click must not fork flatpak twice");
        gate.apply_install_result(Ok(()));
        assert_eq!(gate.confirm(), None, "and neither must a click on a finished sheet");
    }

    /// Cancel is modelled as "drop the gate" at the call site
    /// (`overlay_ui.install_gate = None`) — this test pins the property
    /// that makes that safe: a gate that was never confirmed never produced
    /// a command, so dropping it can not leave an install running.
    #[test]
    fn cancelling_before_confirm_means_nothing_was_ever_handed_out() {
        let gate = InstallGate::open(browser(), "/x".to_string());
        assert_eq!(gate.stage, GateStage::Confirming);
        drop(gate); // what `install_gate = None` does
                    // Nothing to assert beyond the type: `confirm` is `&mut self` and
                    // was never called, so no `(remote, id)` pair exists anywhere.
    }

    #[test]
    fn a_failed_spawn_is_reported_honestly_not_as_a_success() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.confirm();
        gate.apply_install_result(Err("No such file or directory (os error 2)".to_string()));
        assert_eq!(gate.stage, GateStage::Failed("No such file or directory (os error 2)".to_string()));
    }

    #[test]
    fn a_successful_spawn_reports_started_which_is_not_the_same_as_installed() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.confirm();
        gate.apply_install_result(Ok(()));
        assert_eq!(gate.stage, GateStage::Started);
    }

    #[test]
    fn an_install_result_arriving_out_of_order_is_ignored() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.apply_install_result(Ok(()));
        assert_eq!(gate.stage, GateStage::Confirming, "nothing was started, so nothing can have finished");
    }

    #[test]
    fn a_size_probe_landing_after_confirm_does_not_disturb_the_settled_sheet() {
        let mut gate = InstallGate::open(browser(), "/x".to_string());
        gate.confirm();
        gate.apply_size_probe(gate.app_id, Some("123.4 MB".to_string()));
        assert_eq!(gate.size, SizeProbe::Probing);
        assert_eq!(gate.stage, GateStage::Installing);
    }
}
