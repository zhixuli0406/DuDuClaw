// Installable-app catalog — APP-1 (2026-08-22).
//
// ── What this replaced, and why it is not the same thing ────────────────
// Until this round the shell's only app list was `fake_data::DOCK_APPS`:
// six hand-authored entries lifted from a design board (信箱/文件/瀏覽器/
// 圖片/訊息/行事曆), five of which had no real app behind them at all. It
// was labelled honestly in its own doc comment but it was still the thing
// the Launcher and the dock actually rendered — so on a real appliance the
// operator was reading a menu of software that was not there. It is GONE:
// the app list is now a real enumeration (`apps::installed`), and nothing
// anywhere falls back to canned entries when that enumeration is empty or
// fails.
//
// What survives here is the ONE thing in that array that was never
// fictional and that a real enumeration cannot express: a curated list of
// apps this shell knows how to INSTALL. "Installed" and "installable" are
// different claims — an inventory answers "what is here", a catalog answers
// "what could be fetched, from where" — and the install confirmation gate
// (`overlay::install_gate`, WP-A4-4) needs the second one: a real flatpak
// ref plus the real remote it comes from. Neither is derivable from a scan
// of a machine that does not have the app yet.
//
// ── The bar for an entry ────────────────────────────────────────────────
// Every field here is a checkable claim about a real thing, not a design
// sketch. An entry needs a real flatpak application id, a real remote it is
// actually published on, and a `verified` tier backed by evidence. Today
// exactly one entry clears that bar (Chromium — A2's own container PASS,
// cited below); the five design-board icons that had no app behind them are
// deleted rather than demoted, because a catalog of things that cannot be
// installed is the same lie in a different section.
//
// The Launcher renders this as its own clearly-separated 「可安裝」section,
// filtered to entries that are NOT already installed, so it can never be
// mistaken for the inventory above it (`overlay/launcher.rs`).

use super::VerifiedTier;

/// One app this shell knows how to fetch. Every field is `&'static str`
/// (not `Option`) — an entry that cannot name its ref and its remote does
/// not belong here at all, which is a stronger guarantee than the old
/// `Option<&str>` pair that `InstallGate::open` had to defend against at
/// runtime.
pub(crate) struct CatalogApp {
    /// Element id / catalog key.
    pub id: &'static str,
    /// Single-character FALLBACK for the icon slot. Since ICON-1
    /// (2026-08-22) a catalog entry with board artwork renders that instead
    /// (`crate::icons::catalog_layers`, keyed off `id`); this is what shows
    /// if that lookup finds nothing.
    pub glyph: &'static str,
    pub label: &'static str,
    /// Lower-case ASCII search aliases, space separated. The Launcher's
    /// search box has no CJK IME composition (see `overlay/launcher.rs`'s
    /// own header comment), so an ASCII alias is the only way a
    /// CJK-labelled entry is findable by typing.
    pub search_key: &'static str,
    pub flatpak_id: &'static str,
    pub flatpak_remote: &'static str,
    pub verified: VerifiedTier,
}

pub(crate) const INSTALL_CATALOG: &[CatalogApp] = &[CatalogApp {
    id: "catalog-chromium",
    glyph: "網",
    label: "瀏覽器",
    search_key: "browser chromium chrome web 瀏覽器",
    // A2 investigation (`research/native-os-2026-08/flatpak-portal-scope-
    // 2026-08.md` §3): a real container-level PASS — zero portal backend,
    // the window still mapped into duduclaw-comp's space and closed
    // cleanly. The only app in this crate with real launch evidence.
    flatpak_id: "org.chromium.Chromium",
    // Where A2's own PASS pulled this exact ref from — recorded, not
    // assumed. Deliberately per-entry rather than a crate-wide "flathub"
    // default: "which remote does this come from" is data, and defaulting
    // it would be inventing a fact about every future entry.
    flatpak_remote: "flathub",
    verified: VerifiedTier::Works,
}];

/// Case-insensitive substring search over the catalog, same shape
/// `apps::search` uses for the installed list. An empty query matches
/// everything (the Launcher's pre-typing browse state).
pub(crate) fn search(query: &str) -> Vec<&'static CatalogApp> {
    let q = query.trim().to_lowercase();
    INSTALL_CATALOG.iter().filter(|app| q.is_empty() || app.search_key.contains(&q) || app.label.to_lowercase().contains(&q)).collect()
}

/// The D8 "DuDuClaw Verified" compatibility tier for an app id, if this
/// crate has one on file. `Unrated` for everything else — the honest state
/// for an app nobody has evaluated, never a guessed tier (see
/// `VerifiedTier`'s own doc comment in `apps.rs`).
///
/// Matched EXACTLY (case-insensitively), never by prefix or substring:
/// `org.chromium.Chromium` must not lend its rating to
/// `org.chromium.Chromium.Fork` (this crate's coding convention 2).
pub(crate) fn verified_tier(app_id: &str) -> VerifiedTier {
    if app_id.is_empty() {
        return VerifiedTier::Unrated;
    }
    INSTALL_CATALOG.iter().find(|entry| entry.flatpak_id.eq_ignore_ascii_case(app_id)).map(|entry| entry.verified).unwrap_or(VerifiedTier::Unrated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_entry_names_a_real_ref_a_real_remote_and_a_rated_tier() {
        // The bar this file's header comment sets, enforced. An entry that
        // cannot be installed, or that carries a guessed rating, is not a
        // catalog entry — it is the fake data this round removed.
        assert!(!INSTALL_CATALOG.is_empty());
        for entry in INSTALL_CATALOG {
            assert!(!entry.id.is_empty());
            assert!(!entry.label.is_empty());
            assert!(!entry.glyph.is_empty());
            assert!(!entry.search_key.is_empty());
            assert!(crate::apps::flatpak_list::is_plausible_app_id(entry.flatpak_id), "{} has an implausible flatpak id", entry.id);
            assert!(!entry.flatpak_remote.is_empty());
            assert!(!entry.flatpak_remote.starts_with('-'), "a flag-shaped remote would be refused by apps::install anyway");
            assert_ne!(entry.verified, VerifiedTier::Unrated, "{} must not be published with a rating nobody made", entry.id);
        }
    }

    #[test]
    fn catalog_ids_are_unique() {
        let mut ids: Vec<&str> = INSTALL_CATALOG.iter().map(|a| a.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), INSTALL_CATALOG.len());
    }

    #[test]
    fn search_matches_the_ascii_alias_and_the_cjk_label() {
        assert!(search("CHROM").iter().any(|a| a.id == "catalog-chromium"));
        assert!(search("瀏覽").iter().any(|a| a.id == "catalog-chromium"));
        assert_eq!(search("").len(), INSTALL_CATALOG.len(), "an empty query browses everything");
        assert!(search("zzz_no_such_app").is_empty());
    }

    #[test]
    fn verified_tier_is_an_exact_lookup_not_a_prefix_one() {
        assert_eq!(verified_tier("org.chromium.Chromium"), VerifiedTier::Works);
        assert_eq!(verified_tier("ORG.CHROMIUM.CHROMIUM"), VerifiedTier::Works);
        assert_eq!(verified_tier("org.chromium.Chromium.Fork"), VerifiedTier::Unrated, "a rating must never leak to a different app");
        assert_eq!(verified_tier("org.chromium"), VerifiedTier::Unrated);
        assert_eq!(verified_tier("firefox"), VerifiedTier::Unrated);
        assert_eq!(verified_tier(""), VerifiedTier::Unrated);
    }

    /// The catalog is about INSTALLING, so every entry has to survive the
    /// argv builders that actually run — asserted end-to-end rather than by
    /// eyeballing the constants.
    #[test]
    fn every_entry_produces_a_real_install_argv_targeting_the_data_installation() {
        for entry in INSTALL_CATALOG {
            let argv = crate::apps::install_argv(entry.flatpak_remote, entry.flatpak_id).expect("catalog values must be safe CLI args");
            assert!(argv.contains(&"--installation=data".to_string()), "{} would install onto the root partition", entry.id);
            assert!(crate::apps::remote_info_argv(entry.flatpak_remote, entry.flatpak_id).is_some());
        }
    }
}
