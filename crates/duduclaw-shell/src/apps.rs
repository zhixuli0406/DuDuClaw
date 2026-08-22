// App registry search + launch — WP-A3 (2026-08-22, A-line S5 "殼整合"
// round): the Launcher's typed-search "app" result category and the dock's
// click-to-launch icon both go through this one module rather than each
// re-deriving their own filter/spawn logic.
//
// ── Data source boundary (read this before touching either list) ───────────
// This crate's task brief for this round assumed a live "Flatpak app
// registry" (`flatpak list`, portal-backed metadata) would already exist to
// query. It does not: A2 (`research/native-os-2026-08/flatpak-portal-scope-
// 2026-08.md`) was a container-level INVESTIGATION, not a shipped feature —
// flatpak itself isn't even provisioned into the appliance image yet (that's
// A4, "flatpak 搬 /data", still pending). So `search()` below filters
// `fake_data::DOCK_APPS` — a small hand-authored catalog, honestly labeled
// as such in that const's own doc comment — not a live package-manager
// query. Only ONE entry (`browser`, Chromium) carries a real `flatpak_id`
// with actual launch evidence behind it (A2 §3's container PASS); every
// other entry is a conceptual dock icon lifted from the design board with
// no real app behind it at all, and stays `flatpak_id: None`.
//
// ── Why this is a plain fn module, not an `Entity` ──────────────────────────
// Nothing here needs gpui state — `search` is a pure filter over `'static`
// data (same "gpui-free, independently testable" discipline `surface.rs`
// documents for itself) and `launch` is a fire-and-forget `Command::spawn()`
// call with no result to track yet (no "launching…" UI state this round —
// see `launch`'s own doc comment on the honest limitation that implies).

use crate::fake_data::{self, DockApp};

/// Case-insensitive substring search over `fake_data::DOCK_APPS`'
/// `search_key`/`label` fields — backs the Launcher's "app" result category
/// (D12 §0.8 point 3, `commercial/docs/DESIGN-native-gui-gpui-2026-08.md`:
/// NL delegation is the first result class, app/file/system next). An empty
/// query matches every entry — the Launcher's own pre-typing state, same
/// "browse everything" default a fresh search box uses before narrowing.
pub fn search(query: &str) -> Vec<&'static DockApp> {
    let q = query.trim().to_lowercase();
    fake_data::DOCK_APPS.iter().filter(|app| q.is_empty() || app.search_key.contains(&q) || app.label.to_lowercase().contains(&q)).collect()
}

/// Launches a Flatpak-packaged app by application id (`flatpak run <id>`).
/// Fire-and-forget: `Command::spawn()` returns as soon as the child process
/// is forked — it does not wait for the app to actually start, so this never
/// blocks gpui's render thread and needs no background thread hand-off
/// (unlike `gateway_client`'s blocking HTTP calls, which DO need one — see
/// that module's own header comment for why).
///
/// Callers only wire this behind a click handler when `entry.flatpak_id.
/// is_some()` (`overlay/launcher.rs::app_result_row` / `home/home_dock.rs::
/// dock_app`) — matching this crate's established "only wire what's
/// actually real, leave the rest an honest static stub" convention (see
/// `home/home_dock.rs`'s own header comment on Round 3's dock icons). This
/// fn still no-ops safely (a diag line, never a panic) if handed an entry
/// with no `flatpak_id`, so it's safe to call unconditionally too.
///
/// **Known limitation** (A2 finding): flatpak is not yet provisioned into
/// the appliance image (A4 still pending) — on a dev Mac window, or an
/// appliance build that predates A4, this spawn fails with "no such file or
/// directory". That's an expected, logged outcome, not a bug in this fn;
/// this crate has no toast/notification surface to report a launch failure
/// to the user yet (tracked as debt in the WP-A3 report, not silently
/// dropped).
pub fn launch(entry: &DockApp) {
    let Some(app_id) = entry.flatpak_id else {
        if crate::diag_enabled() {
            eprintln!("[apps] launch requested for '{}' but it has no known flatpak_id (honest no-op)", entry.id);
        }
        return;
    };
    match std::process::Command::new("flatpak").arg("run").arg(app_id).spawn() {
        Ok(_child) => {
            if crate::diag_enabled() {
                eprintln!("[apps] launched {app_id} (dock/launcher id={})", entry.id);
            }
        }
        Err(e) => {
            if crate::diag_enabled() {
                eprintln!("[apps] launch failed for {app_id}: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake_data::VerifiedTier;

    #[test]
    fn empty_query_returns_every_registry_entry() {
        assert_eq!(search("").len(), fake_data::DOCK_APPS.len());
    }

    #[test]
    fn whitespace_only_query_behaves_like_empty() {
        assert_eq!(search("   ").len(), fake_data::DOCK_APPS.len());
    }

    #[test]
    fn search_matches_the_ascii_search_key_case_insensitively() {
        let results = search("CHROM");
        assert!(results.iter().any(|a| a.id == "browser"), "expected 'CHROM' to find the browser entry");
    }

    #[test]
    fn search_also_matches_the_cjk_label() {
        let results = search("信箱");
        assert!(results.iter().any(|a| a.id == "mail"));
    }

    #[test]
    fn search_with_no_match_returns_empty() {
        assert!(search("zzz_no_such_app_in_the_registry").is_empty());
    }

    #[test]
    fn launch_on_an_entry_with_no_flatpak_id_is_a_safe_noop() {
        let entry = fake_data::DOCK_APPS.iter().find(|a| a.flatpak_id.is_none()).expect("at least one honest display-only stub must exist");
        launch(entry); // must not panic — the only assertion this test needs
    }

    #[test]
    fn launch_on_the_one_real_entry_does_not_panic_even_without_flatpak_installed() {
        // Dev-machine reality per this module's own header comment: `flatpak`
        // is very likely absent, and this MUST still be a clean `Err` path,
        // never a panic — `Command::spawn()` returning `Err` is exactly what
        // "flatpak not installed" looks like, so this is real coverage, not
        // a smoke test with a fake precondition.
        let entry = fake_data::DOCK_APPS.iter().find(|a| a.id == "browser").expect("browser entry must exist");
        assert_eq!(entry.verified, VerifiedTier::Works);
        launch(entry);
    }
}
