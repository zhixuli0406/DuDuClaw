// Split out of `i18n/mod.rs` — WP-lock-pw (2026-08-22). Pure line-count
// housekeeping, zero behavior/coverage change: `i18n.rs` (a flat file) was
// already at 799 lines before this round's lockscreen-password keys, and
// this crate's own "200-400 typical, 800 max" file-size convention (see
// e.g. `oobe/mod.rs`'s own header comment on why `ui_state.rs`/
// `network_ui.rs`/`selections.rs` were split out of it the same way) left
// no room to add five new keys in place. `i18n.rs` became `i18n/mod.rs`
// (Rust treats `mod i18n;` identically either way, so `main.rs`'s own
// declaration needed no change) and this file holds exactly what used to be
// its `#[cfg(test)] mod tests { ... }` block, now declared as `mod tests;`
// instead — same mechanical move, not a rewrite.

use super::*;

// Hand-maintained, same discipline `OobeStep::ALL`/`PrivacyToggle::ALL`
// establish in `oobe/mod.rs` — scoped to `#[cfg(test)]` rather than a
// `pub const Key::ALL` because nothing in PRODUCTION code needs to
// iterate every key (each render call site already knows exactly which
// key it wants), and an unused `pub` const on a bin-only crate (no lib
// target — see `Cargo.toml`) still trips `dead_code` outside test
// builds. The REAL drift guard is `zh_tw`/`en`/`ja_jp` each being an
// exhaustive `match` over `Key` (a variant missing an arm in any one of
// the three is a compile error) — this list only has to stay in sync
// for the completeness/non-empty test below to actually cover
// everything, not for correctness of `t()` itself.
const ALL_KEYS: &[Key] = &[
    Key::NavBack,
    Key::NavSkip,
    Key::NavContinue,
    Key::NavGetStarted,
    Key::InputDetectionTitle,
    Key::InputDetectionSubtitle,
    Key::InputDetectionKeyboard,
    Key::InputDetectionMouse,
    Key::InputDetectionDetected,
    Key::LanguageAccessibilityEntry,
    Key::LanguageAccessibilityCollapse,
    Key::LanguageAccessibilityExpand,
    Key::LanguageAccessibilityPlaceholder,
    Key::LanguageA11ySeeingLabel,
    Key::LanguageA11ySeeingDesc,
    Key::LanguageA11yHearingLabel,
    Key::LanguageA11yHearingDesc,
    Key::LanguageA11yTypingLabel,
    Key::LanguageA11yTypingDesc,
    Key::LanguageA11yPointingLabel,
    Key::LanguageA11yPointingDesc,
    Key::LanguageA11yZoomLabel,
    Key::LanguageA11yZoomDesc,
    Key::CommonSelected,
    Key::NetworkTitle,
    Key::NetworkSubtitlePrompt,
    Key::NetworkConnectedTo,
    Key::NetworkSecuredBadge,
    Key::NetworkConnectedBadge,
    Key::NetworkNeverScanned,
    Key::NetworkScanButton,
    Key::NetworkRescanButton,
    Key::NetworkScanningStatus,
    Key::NetworkScanFailedStatus,
    Key::NetworkScanEmptyStatus,
    Key::NetworkDemoModeNotice,
    Key::NetworkPskLabel,
    Key::NetworkConnectButton,
    Key::NetworkConnectingButton,
    Key::NetworkConnectingStatus,
    Key::NetworkCancelButton,
    Key::NetworkPskLengthError,
    Key::NetworkWrongPasswordError,
    Key::NetworkConnectUnreachableError,
    Key::NetworkWiredConnected,
    Key::NetworkNotFoundError,
    Key::NetworkOutOfRangeError,
    Key::NetworkNoAdapterError,
    Key::NetworkDriverMissingError,
    Key::NetworkNoIpError,
    Key::NetworkPortalNotice,
    Key::NetworkPortalOpenButton,
    Key::NetworkBackendUnavailableError,
    Key::NetworkUnsupportedSecurityError,
    Key::NetworkUnavailableHint,
    Key::UpdateTitle,
    Key::UpdateChecking,
    Key::UpdateUpToDate,
    Key::AccountTitle,
    Key::AccountSubtitle,
    Key::AccountNameLabel,
    Key::AccountPasswordLabel,
    Key::AccountValidationError,
    Key::AccountCreateButton,
    Key::AccountCreatedButton,
    Key::AccountCreatingButton,
    Key::AccountPasswordTooShortError,
    Key::AccountAlreadyClaimedInfo,
    Key::AccountUnreachableError,
    Key::RuntimeAuthTitle,
    Key::RuntimeAuthSubtitle,
    Key::RuntimeAuthAuthorized,
    Key::RuntimeAuthAuthorizeNow,
    Key::RuntimeAuthDeferLater,
    Key::PrivacyTitle,
    Key::PrivacySubtitle,
    Key::PrivacyUsageStatsLabel,
    Key::PrivacyUsageStatsDesc,
    Key::PrivacyErrorReportsLabel,
    Key::PrivacyErrorReportsDesc,
    Key::PrivacyPersonalizationLabel,
    Key::PrivacyPersonalizationDesc,
    Key::PrivacyMarketingLabel,
    Key::PrivacyMarketingDesc,
    Key::TemplatesTitle,
    Key::TemplatesSubtitle,
    Key::TemplatesExpressTitle,
    Key::TemplatesExpressDesc,
    Key::TemplatesExpressApply,
    Key::TemplatesExpressApplied,
    Key::TemplatesCustomHint,
    Key::TemplatesSkip,
    Key::ThemeTitle,
    Key::ThemeSubtitle,
    Key::ThemeLight,
    Key::ThemeDark,
    Key::FinishTitle,
    Key::FinishSubtitle,
    Key::FinishSummaryLanguage,
    Key::FinishSummaryNetwork,
    Key::FinishSummaryAccount,
    Key::FinishSummaryRuntime,
    Key::FinishSummaryPrivacy,
    Key::FinishSummaryTemplates,
    Key::FinishRuntimeAuthorized,
    Key::FinishRuntimeDeferred,
    Key::FinishRuntimeNotSet,
    Key::FinishNetworkNotConnected,
    Key::FinishAccountCreated,
    Key::FinishAccountNotCreated,
    Key::FinishPrivacyAllOff,
    Key::FinishPrivacyOnCount,
    Key::FinishTemplatesExpress,
    Key::FinishTemplatesCustom,
    Key::FinishTemplatesSkipped,
    Key::FinishTemplatesNotChosen,
    Key::NotifOfflineBanner,
    Key::NotifLoadingLabel,
    Key::NotifEmptyLabel,
    Key::NotifConfirmButton,
    Key::NotifDecidingLabel,
    Key::NotifDecideFailedLabel,
    Key::NotifRetryButton,
    Key::NotifAppSectionLabel,
    Key::NotifDismissButton,
    Key::NotifClearAllButton,
    Key::NotifDaemonNameTakenBanner,
    Key::NotifDaemonFailedBanner,
    Key::NotifDaemonUnsupportedBanner,
    Key::NotifMergedCount,
    Key::NotifAgeJustNow,
    Key::NotifAgeMinutes,
    Key::NotifAgeHours,
    Key::NotifAgeDays,
    Key::LockAwaySummaryTitle,
    Key::LockPendingCountLabel,
    Key::LockUnlockHint,
    Key::LockPasswordPlaceholder,
    Key::LockPasswordWrongError,
    Key::LockOfflineError,
    Key::LockVerifyingLabel,
    Key::LockThrottledLabel,
    Key::LockPowerRestart,
    Key::LockPowerShutdown,
    Key::LockPowerConfirmRestart,
    Key::LockPowerConfirmShutdown,
    Key::LockPowerSending,
    Key::LockPowerFailed,
    Key::LockPowerUnsupported,
    Key::PointerTitle,
    Key::PointerSubtitle,
    Key::PointerEntryDesc,
    Key::PointerSectionAccessibility,
    Key::PointerShapeLabel,
    Key::PointerSizeLabel,
    Key::PointerSourceSystem,
    Key::PointerSourceSystemDesc,
    Key::PointerSourceBrand,
    Key::PointerSourceBrandDesc,
    Key::PointerSizeDefault,
    Key::PointerSizeMedium,
    Key::PointerSizeLarge,
    Key::PointerSizeExtraLarge,
    Key::PointerSizeLargest,
    Key::PointerZoomHint,
    Key::PointerLoading,
    Key::PointerUnavailable,
    Key::PointerSizeUnsupported,
    Key::PointerApplyFailed,
    Key::PointerThemeMissing,
    Key::PointerEnvPinned,
    Key::LauncherSearchPlaceholder,
    Key::LauncherNoAppResults,
    Key::LauncherAppsScanning,
    Key::LauncherAppsNoneInstalled,
    Key::LauncherAppsReadFailed,
    Key::LauncherSectionInstallable,
    Key::LauncherInstallButton,
    Key::InstallGateTitle,
    Key::InstallGateNotice,
    Key::InstallGateSourceLabel,
    Key::InstallGateSizeLabel,
    Key::InstallGateLocationLabel,
    Key::InstallGateSizeProbing,
    Key::InstallGateSizeUnknown,
    Key::InstallGateConfirmButton,
    Key::InstallGateInstallingLabel,
    Key::InstallGateStartedLabel,
    Key::InstallGateFailedLabel,
    Key::VerifiedTierVerified,
    Key::VerifiedTierWorks,
    Key::VerifiedTierPartial,
    Key::VerifiedTierUnsupported,
    Key::VerifiedTierUnrated,
];

const ALL_LOCALES: [Locale; 3] = [Locale::ZhTw, Locale::En, Locale::JaJp];

#[test]
fn all_keys_has_the_expected_count_and_no_duplicates() {
    // A hardcoded count, same style `all_has_ten_steps_in_declared_
    // order` (`oobe/mod.rs`) pins `OobeStep::ALL.len() == 10` — catches
    // `ALL_KEYS` silently drifting out of sync with a newly added `Key`
    // variant (the compiler won't catch THAT half; only the per-locale
    // match arms are compiler-enforced).
    // 194 as of D6 (2026-08-23), which added the eleven
    // `org.freedesktop.Notifications` panel keys (`NotifAppSectionLabel` …
    // `NotifAgeDays`); 183 before that, from D4a §6's
    // `NetworkPortalOpenButton`.
    assert_eq!(ALL_KEYS.len(), 194);
    let mut seen = std::collections::HashSet::new();
    for key in ALL_KEYS {
        assert!(seen.insert(key), "duplicate key in ALL_KEYS: {key:?}");
    }
}

#[test]
fn every_key_has_a_non_empty_translation_in_all_three_locales() {
    // The three-locale key-set-consistency check the task brief asks
    // for. Weaker than the compile-time exhaustiveness `zh_tw`/`en`/
    // `ja_jp` already give (see this module's own header comment) —
    // this additionally catches an arm that compiles but was left an
    // empty string by mistake.
    for key in ALL_KEYS {
        for locale in ALL_LOCALES {
            let s = t(locale, *key);
            assert!(!s.is_empty(), "{locale:?}/{key:?} is empty");
        }
    }
}

#[test]
fn t1_substitutes_the_placeholder_in_every_locale() {
    for locale in ALL_LOCALES {
        let out = t1(locale, Key::NetworkConnectedTo, "DuDu-Office");
        assert!(out.contains("DuDu-Office"), "{locale:?}: {out}");
        assert!(!out.contains("{}"), "{locale:?}: placeholder left unsubstituted: {out}");
    }
}

#[test]
fn t1_only_substitutes_the_first_placeholder_occurrence() {
    // `replacen(.., 1)` — every current catalog string has at most one
    // `{}`, but this pins the contract explicitly rather than leaving it
    // implicit, so a future key with two `{}`s fails loudly here instead
    // of silently double-substituting.
    assert_eq!(t1(Locale::En, Key::FinishTemplatesCustom, "{}"), "Custom: {}");
}

#[test]
fn zh_tw_is_the_default_reading_before_any_pick() {
    // §B-1 row 1: "語言（zh-TW 預設高亮）" — `oobe::LanguageChoice`'s own
    // `#[default]` variant is `ZhTw`, so the very first OOBE frame (no
    // click yet) must already read correctly through THIS catalog.
    assert_eq!(t(Locale::ZhTw, Key::NavContinue), "繼續");
}
