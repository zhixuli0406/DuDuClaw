// Minimal i18n layer for the shell — Shell-S1 round 3 (OOBE first-step
// language reorder + real i18n, 2026-08-20).
//
// Crate-root, not `oobe/i18n.rs`: `oobe` is only this round's ONE consumer
// (task brief item 4: "Home/overlay 的硬編繁中本輪不動...全殼 i18n 是後續
// 題"), but `home.rs`/`overlay.rs` are SIBLINGS of `oobe` under this same
// crate root, not descendants of it — a later round wiring Home/overlay
// through this same catalog reaches `crate::i18n::*` either way, so putting
// it here now avoids a move later. Mirrors `duduclaw-native-gui/src/i18n/
// mod.rs`'s shape (a `Locale` enum + a flat lookup) but NOT its storage: that
// crate embeds three JSON catalogs (`include_str!` + `serde_json` + a
// `HashMap<String, String>` runtime lookup) because it's a `pub mod` on a
// lib target consumed by exactly one bin with no "no gpui types" constraint
// on the caller. This module is reached from `oobe/mod.rs`, which states
// its own "no gpui types anywhere in THIS file" discipline in its header
// comment — a `HashMap` lookup returning an owned `String`/`SharedString`
// would still be gpui-free, but a plain `match` over a closed `Key` enum
// buys something a runtime map cannot: the THREE per-locale match
// expressions below (`zh_tw`/`en`/`ja_jp`) each have to be EXHAUSTIVE over
// every `Key` variant, so a new key added to the enum without a
// corresponding arm in any one of the three is a compile error, not a
// silent runtime fallback — strictly stronger than the "three-locale key
// set consistency" test the task brief also asks for (which still exists,
// see the `tests` module below, as a second line of defense and to guard
// against copy-paste-empty-string mistakes the compiler can't catch).
//
// `t()` returns `&'static str` (not `String`/`SharedString`) for the same
// reason: every catalog string is a literal, so there is nothing to own —
// call sites that need interpolation use `t1()` instead, which is the only
// function here that allocates.

/// The three languages `oobe::LanguageChoice` maps onto (see that enum's own
/// `to_locale()` — the conversion lives on the OOBE side of this boundary so
/// this module stays independent of any one caller's own selection type,
/// consistent with this file's header comment on why it lives at the crate
/// root rather than under `oobe/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    ZhTw,
    En,
    JaJp,
}

/// Every translatable string OOBE's step screens, shared widgets (bottom-nav
/// Back/Skip/Continue/Get-Started), and validation error message need (task
/// brief item 2). Deliberately does NOT cover `oobe::fake_data`'s Wi-Fi SSIDs
/// or the account-field placeholder hint ("Louis"/dots) — those are literal
/// stand-in DATA (a network's own name, an example account name), the same
/// category the task brief's own item 4 exempts for Home/overlay, not OOBE
/// chrome; see `oobe/fake_data.rs`'s header comment for this same call
/// spelled out where that data lives. Nor does it cover the
/// `LanguageAccessibility` step's own top caption (see `oobe::steps::
/// language`'s header comment) or either language's native name (`oobe::
/// LanguageChoice::label()` — never translated, same policy `duduclaw-
/// native-gui/src/i18n/mod.rs`'s `Locale::native_name()` documents: a reader
/// of any of the three languages must recognize their own option before
/// picking one).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    NavBack,
    NavSkip,
    NavContinue,
    NavGetStarted,

    InputDetectionTitle,
    InputDetectionSubtitle,
    InputDetectionKeyboard,
    InputDetectionMouse,
    InputDetectionDetected,

    LanguageAccessibilityEntry,
    /// ICON-3 (2026-08-23): these two are no longer the row's PRIMARY
    /// affordance — the board replaced the "展開 ▼"/"收合 ▲" text with a
    /// `go-next`/`go-down` chevron. They stay in the catalog, and stay
    /// translated, as that chevron's `icon_or_glyph` text fallback (the
    /// arrow inside each string is exactly what makes them a usable
    /// fallback), so a missing asset degrades to the pre-ICON-3 wording
    /// rather than to a blank hole.
    LanguageAccessibilityCollapse,
    LanguageAccessibilityExpand,
    /// ICON-3: reworded. It used to name three specific options
    /// (旁白／放大鏡／對比度) because it was the ONLY thing the expanded
    /// panel showed; the panel now lists the board's five real categories
    /// above it, so this line's job is narrower — it says, honestly, that
    /// none of the five is adjustable yet.
    LanguageAccessibilityPlaceholder,
    /// ICON-3 (2026-08-23): the five accessibility categories
    /// `OOBE-ProgressAndIcons.dc.html` draws inside the expanded panel.
    /// Purely informational rows (no toggle, no chevron, no click target),
    /// exactly as the board draws them — see `steps::language`'s own header
    /// comment on why they are NOT interactive.
    LanguageA11ySeeingLabel,
    LanguageA11ySeeingDesc,
    LanguageA11yHearingLabel,
    LanguageA11yHearingDesc,
    LanguageA11yTypingLabel,
    LanguageA11yTypingDesc,
    LanguageA11yPointingLabel,
    LanguageA11yPointingDesc,
    LanguageA11yZoomLabel,
    LanguageA11yZoomDesc,

    CommonSelected,

    NetworkTitle,
    NetworkSubtitlePrompt,
    NetworkConnectedTo,
    NetworkSecuredBadge,
    NetworkConnectedBadge,
    /// Shell-S3 (2026-08-21, real Wi-Fi backend — see `oobe::network`'s own
    /// header comment) — the `NeverScanned` scan state's prompt, before the
    /// operator has clicked "掃描 Wi-Fi" even once.
    NetworkNeverScanned,
    NetworkScanButton,
    /// Same click target as `NetworkScanButton`, different label for
    /// re-scanning from `Failed`/`Loaded`/empty-list states — see
    /// `steps::network`'s `kick_off_scan`, shared by every caller.
    NetworkRescanButton,
    NetworkScanningStatus,
    NetworkScanFailedStatus,
    NetworkScanEmptyStatus,
    /// Task brief: "在 UI 誠實標示（不可假裝連線成功）" — shown whenever the
    /// CURRENT scan/connect calls used the demo backend, see
    /// `oobe::network::NetBackendKind`'s own doc comment for every
    /// situation that covers (not only Linux/NetworkManager-unreachable).
    NetworkDemoModeNotice,
    NetworkPskLabel,
    NetworkConnectButton,
    NetworkConnectingButton,
    /// Inline status line inside the connect panel while `Connecting` — as
    /// opposed to `NetworkConnectingButton`, which is the BUTTON's own
    /// label while busy; both render at once.
    NetworkConnectingStatus,
    NetworkCancelButton,
    /// Client-side pre-check mirroring the real WPA-PSK passphrase length
    /// rule (8–63 characters) — see `oobe::NetConnectFailureKind::
    /// PasswordTooShort`'s own doc comment.
    NetworkPskLengthError,
    NetworkWrongPasswordError,
    NetworkConnectUnreachableError,
    /// D4a §5.4-2 (2026-08-23): the wired/portal-only "已透過網路線連線"
    /// status line — see `OobeUiState::wired_online`'s own doc comment.
    NetworkWiredConnected,
    /// D4a §5.3 (2026-08-23): the remaining eight of the gateway's own
    /// nine-code Wi-Fi failure classification — `WrongPassword` reuses the
    /// pre-existing `NetworkWrongPasswordError` key above, so only eight new
    /// keys are needed here. See `network::WifiFailureCode`'s own doc
    /// comment for the full nine-code list and
    /// `network_ui::NetConnectFailureKind::from_code` for the mapping.
    NetworkNotFoundError,
    NetworkOutOfRangeError,
    NetworkNoAdapterError,
    NetworkDriverMissingError,
    /// `{}` = the SSID the operator was joining.
    NetworkNoIpError,
    /// `{}` = the SSID (or, for a wired/portal-only connection, empty —
    /// `steps::network::portal_notice` falls back to the empty string when
    /// no SSID is known). Doubles as both the CONNECT-failure `portal` code
    /// message and the standalone captive-portal banner (D4a §6) — same
    /// wording either way, since both describe the identical situation
    /// ("connected, but a browser login is required").
    NetworkPortalNotice,
    /// The button next to `NetworkPortalNotice` — D4a section 6's "開啟登入
    /// 頁" affordance. Only rendered when the gateway actually reported a
    /// `portal_url`; with no URL there is nothing honest to open, so the
    /// notice stands alone rather than offering a button that does nothing.
    NetworkPortalOpenButton,
    NetworkBackendUnavailableError,
    NetworkUnsupportedSecurityError,
    /// D4a §5.4 (2026-08-23): shown in `failed_panel` (a Wi-Fi SCAN
    /// failure, not a connect failure) specifically when the current
    /// backend is `NetBackendKind::Unavailable` — a distinct, more specific
    /// line than the generic `NetworkScanFailedStatus` retry message.
    NetworkUnavailableHint,
    /// D4a-7 (2026-08-31, QEMU wired-only OOBE deadlock): shown when the
    /// operator clicks Continue on the `Network` step while `can_advance_
    /// with_wired` is still false — see `OobeUiState::net_continue_blocked`'s
    /// own doc comment for the field this reads, and `steps::network::
    /// continue_blocked_notice` for the one call site. Names BOTH escape
    /// routes (pick a Wi-Fi row, or plug in a cable) since this fires for
    /// EITHER a wireless-only or a wired-only machine — the step has no way
    /// to know which the operator has in front of them.
    NetworkContinueBlockedNotice,
    /// Same trigger as `NetworkContinueBlockedNotice` above, shown instead
    /// of it while the background recheck THAT SAME blocked click kicked
    /// off (`steps::network::handle_blocked_continue`) is still in flight —
    /// deliberately worded around "network status", not "Wi-Fi", since this
    /// recheck is fetching `NetworkBackend::network_status()` (wired
    /// awareness), not just the Wi-Fi scan list `NetworkScanningStatus`
    /// already covers.
    NetworkStatusCheckingNotice,

    UpdateTitle,
    UpdateChecking,
    UpdateUpToDate,

    AccountTitle,
    AccountSubtitle,
    AccountNameLabel,
    AccountPasswordLabel,
    AccountValidationError,
    AccountCreateButton,
    AccountCreatedButton,
    /// Shell-S2 round 1 (real `/api/first-run/claim` RPC wiring) — see
    /// `oobe::claim`'s own header comment. Button label while a claim
    /// request is in flight.
    AccountCreatingButton,
    /// Client-side pre-validation mirroring the gateway's own `< 8 chars`
    /// rule (`handle_first_run_claim`), shown before ever dispatching a
    /// request — and also the message shown if the gateway rejects the
    /// password anyway (`ClaimError::RejectedTooShort`).
    AccountPasswordTooShortError,
    /// Shown when `GET /api/first-run/status` already reports
    /// `claimable: false` (or the claim itself raced and lost, 409) — this
    /// device already has an administrator account, informational rather
    /// than an error.
    AccountAlreadyClaimedInfo,
    /// Shown for any claim failure that isn't a rejected password —
    /// connection refused, timeout, unexpected HTTP status, malformed
    /// response — collapsed to one retryable message (see
    /// `oobe::AccountClaimFailureKind`'s own doc comment for why).
    AccountUnreachableError,

    /// Installer-settings-integration WP3 (2026-08-29): the LIVE
    /// installer's own `Network` step (`live_install::steps::network`) — a
    /// pure SSID/passphrase COLLECTION form, not OOBE's own `Network` step
    /// above (which scans/connects for real). Deliberately named `LiveWifi*`
    /// rather than reusing the `Network*` keys above, even though both
    /// screens show an SSID + password: the two steps have unrelated status
    /// vocabularies (this one never scans, never connects, never shows a
    /// signal bar) and sharing a key prefix would blur that distinction in
    /// the catalog itself.
    LiveWifiTitle,
    LiveWifiSubtitle,
    LiveWifiSsidLabel,
    LiveWifiPskLabel,
    /// The step's "this is optional" hint line — see `steps::network`'s own
    /// header comment for why an empty submission is a legal outcome here,
    /// unlike `Account`'s `AccountValidationError` just above.
    LiveWifiOptionalHint,
    /// `NetworkError::SsidMissingWithPsk` — a passphrase was typed but the
    /// SSID field was left empty.
    LiveWifiErrSsidMissing,
    /// `NetworkError::SsidTooLong` — the typed SSID exceeds the 802.11
    /// 32-byte limit.
    LiveWifiErrSsidTooLong,
    /// `NetworkError::PskLengthInvalid` — the typed passphrase is non-empty
    /// but outside the WPA-PSK 8..=63 character range.
    LiveWifiErrPskLength,

    RuntimeAuthTitle,
    RuntimeAuthSubtitle,
    RuntimeAuthAuthorized,
    RuntimeAuthAuthorizeNow,
    RuntimeAuthDeferLater,

    PrivacyTitle,
    PrivacySubtitle,
    PrivacyUsageStatsLabel,
    PrivacyUsageStatsDesc,
    PrivacyErrorReportsLabel,
    PrivacyErrorReportsDesc,
    PrivacyPersonalizationLabel,
    PrivacyPersonalizationDesc,
    PrivacyMarketingLabel,
    PrivacyMarketingDesc,

    TemplatesTitle,
    TemplatesSubtitle,
    TemplatesExpressTitle,
    TemplatesExpressDesc,
    TemplatesExpressApply,
    TemplatesExpressApplied,
    TemplatesCustomHint,
    TemplatesSkip,

    ThemeTitle,
    ThemeSubtitle,
    ThemeLight,
    ThemeDark,

    FinishTitle,
    FinishSubtitle,
    FinishSummaryLanguage,
    FinishSummaryNetwork,
    FinishSummaryAccount,
    FinishSummaryRuntime,
    FinishSummaryPrivacy,
    FinishSummaryTemplates,
    FinishRuntimeAuthorized,
    FinishRuntimeDeferred,
    FinishRuntimeNotSet,
    FinishNetworkNotConnected,
    FinishAccountCreated,
    FinishAccountNotCreated,
    FinishPrivacyAllOff,
    FinishPrivacyOnCount,
    FinishTemplatesExpress,
    FinishTemplatesCustom,
    FinishTemplatesSkipped,
    FinishTemplatesNotChosen,

    /// Shell-S4 (2026-08-22, WP-S4-notif): the Notifications overlay's
    /// approval-card feed hitting the REAL gateway (`gateway_client`,
    /// `overlay::notifications_feed`) introduces genuinely NEW process
    /// chrome — a connectivity banner, a loading state, an empty state, a
    /// two-step confirm/decide flow — that didn't exist when Home/overlay
    /// was declared out of scope for i18n (see this file's own header
    /// comment on that boundary). These seven keys are i18n'd for the same
    /// reason `NetworkDemoModeNotice`/`AccountUnreachableError` above are:
    /// honest-state messaging is process chrome, not per-item design
    /// CONTENT — the approval cards' own `summary`/`agent_id`/detail text
    /// (server-generated prose) stays a plain zh-TW literal in
    /// `notifications_feed.rs`, same as every other piece of Home/overlay
    /// DATA. "取消" reuses the existing `NetworkCancelButton` key rather
    /// than adding an eighth — it's exactly the same generic action.
    NotifOfflineBanner,
    NotifLoadingLabel,
    NotifEmptyLabel,
    NotifConfirmButton,
    NotifDecidingLabel,
    NotifDecideFailedLabel,
    NotifRetryButton,

    /// D6 (2026-08-23) — the third-party app notification section of the same
    /// panel (`crate::notifyd`). Same boundary rule the block above states:
    /// this is process CHROME (a section heading, a button, the honest
    /// daemon-status banners, relative timestamps), so it is i18n'd; the
    /// notification CONTENT itself (`app_name`/`summary`/`body`, which
    /// arrives from an arbitrary third-party application) is DATA and is
    /// rendered verbatim — translating an app's own message would be both
    /// impossible and wrong.
    NotifAppSectionLabel,
    NotifDismissButton,
    NotifClearAllButton,
    /// Honest daemon status. `NotifDaemonNameTakenBanner` is deliberately
    /// NOT phrased as an error: another daemon owning the name means those
    /// notifications are being shown SOMEWHERE, just not here.
    NotifDaemonNameTakenBanner,
    NotifDaemonFailedBanner,
    NotifDaemonUnsupportedBanner,
    /// "+N 則" on a card the flood guard merged others onto.
    NotifMergedCount,
    NotifAgeJustNow,
    NotifAgeMinutes,
    NotifAgeHours,
    NotifAgeDays,

    /// A4 (2026-08-24) — the same panel's "進行中任務" section (real
    /// `tasks.list(status="in_progress")` rows, see `overlay::
    /// notifications_tasks`'s own header comment). Same boundary rule as
    /// `NotifAppSectionLabel` above: this heading is process CHROME, the
    /// task `title`/`assigned_to` it lists is server-generated DATA and
    /// stays a plain literal.
    NotifTaskSectionLabel,

    /// Shell-S4-lock lockscreen chrome — see `lockscreen/render.rs`'s header.
    LockAwaySummaryTitle,
    LockPendingCountLabel,
    /// WP-lock-pw (2026-08-22): wording changed from a direct "解鎖"
    /// (unlock) claim to "輸入密碼" (enter password) — the surface no
    /// longer unlocks on any key/click, only reveals the password prompt,
    /// see `lockscreen/render.rs`'s own header comment for the reversal.
    LockUnlockHint,
    /// WP-lock-pw: the password field's own placeholder text.
    LockPasswordPlaceholder,
    /// `gateway_client::LoginError::InvalidCredentials` (401) — a real
    /// password mismatch against the fixed `admin@local` account.
    LockPasswordWrongError,
    /// Every OTHER verify failure (offline, timeout, unexpected HTTP
    /// status, malformed response, the gateway's own rate limit) — one
    /// honest fail-safe message, see `lockscreen/render.rs`'s own header
    /// comment on why this surface stays LOCKED rather than degrading.
    LockOfflineError,
    /// Shown while a verify request is in flight.
    LockVerifyingLabel,
    /// Shown once this surface's own client-side throttle
    /// (`lockscreen::FAILS_BEFORE_THROTTLE`/`THROTTLE_DURATION`) is active —
    /// a submit is still accepted once the cooldown elapses, this is
    /// informational, not a hard lockout.
    LockThrottledLabel,

    /// ICON-3 (2026-08-23): the lockscreen's bottom-centre power button and
    /// its two-item menu. Both actions are irreversible from this surface's
    /// point of view (the machine goes away), so each takes a second
    /// confirmation — the same two-step shape `overlay/notifications.rs`
    /// already uses for approve/reject, applied to the one control on this
    /// surface that can end the session.
    LockPowerRestart,
    LockPowerShutdown,
    LockPowerConfirmRestart,
    LockPowerConfirmShutdown,
    LockPowerSending,
    /// Every transport-level failure — no session, unreachable gateway,
    /// timeout, a rejection this client has no specific handling for. One
    /// honest message, the same collapse `LockOfflineError` already makes
    /// for the unlock path.
    LockPowerFailed,
    /// The gateway answered, and answered that it does not know this method
    /// — an older gateway build. Distinct from `LockPowerFailed` because
    /// the operator's response differs: nothing is going to fix itself by
    /// retrying.
    LockPowerUnsupported,

    /// ICON-3 (2026-08-23): the pointer-settings surface
    /// (`overlay/pointer_settings.rs`) — entirely new process chrome with
    /// no zh-TW-literal neighbours of its own, so it routes through this
    /// catalog exactly like the `Launcher*`/`InstallGate*` keys above. Its
    /// ENTRY row inside ControlCenter reads from these same keys via
    /// `t(Locale::ZhTw, ..)`, the same shape `lockscreen/render.rs` already
    /// uses, so the two places that name this screen can never disagree.
    PointerTitle,
    PointerSubtitle,
    PointerEntryDesc,
    PointerSectionAccessibility,
    PointerShapeLabel,
    PointerSizeLabel,
    PointerSourceSystem,
    PointerSourceSystemDesc,
    PointerSourceBrand,
    PointerSourceBrandDesc,
    PointerSizeDefault,
    PointerSizeMedium,
    PointerSizeLarge,
    PointerSizeExtraLarge,
    PointerSizeLargest,
    /// GNOME's own `cc-cursor-size-page` help line, translated — the one
    /// piece of copy on this screen with a verbatim upstream precedent.
    PointerZoomHint,
    PointerLoading,
    /// No compositor to talk to at all: `$XDG_RUNTIME_DIR` unset, or no
    /// `duduclaw-shell.sock`. The ordinary case on a dev Mac, and the
    /// honest one on a Linux box whose compositor is down.
    PointerUnavailable,
    /// The compositor is there and answered, but does not implement
    /// `set_cursor_size` (or reported no `size` at all) — an older
    /// `duduclaw-comp`. Only the SIZE half degrades; shape still works.
    PointerSizeUnsupported,
    PointerApplyFailed,
    /// The compositor is drawing a DIFFERENT cursor theme from the one that
    /// was chosen, because the chosen one is not installed
    /// (`CursorState::theme_missing`). The radio keeps showing the choice;
    /// this line explains why the pointer does not match it.
    PointerThemeMissing,
    /// An operator pinned the cursor style and/or size in the compositor's
    /// spawn environment (`DUDUCLAW_COMP_CURSOR_SOURCE` / `XCURSOR_SIZE`),
    /// which outranks anything stored from this screen at the next start.
    /// Without this line the page would quietly promise a choice that does
    /// not survive a reboot.
    PointerEnvPinned,

    /// WP-A3 (2026-08-22): the Launcher's live search box going from a
    /// static predisplay to real typing (`overlay/launcher.rs`) introduces
    /// genuinely new process chrome — same "honest-state messaging is
    /// chrome, not per-item DESIGN content" boundary the `Notif*` keys
    /// above already establish (see that block's own comment). The five
    /// `VerifiedTier*` keys below are the D8 "DuDuClaw Verified" badge
    /// labels (`fake_data::VerifiedTier`) — also new chrome, no design-board
    /// precedent (neither `Launcher.dc.html` nor any other board shows a
    /// Verified badge).
    LauncherSearchPlaceholder,
    LauncherNoAppResults,
    /// APP-1 (2026-08-22): the app list stopped being a canned array and
    /// became a real enumeration of this machine, which introduces three
    /// genuinely new honest states an empty list can be in — and they are
    /// three different facts, so they get three different sentences rather
    /// than one shrug (see `apps::feed::AppsEmptyState`). Same "honest-state
    /// messaging is process chrome, not per-item design CONTENT" boundary
    /// the `Notif*`/`Launcher*` keys above already draw. None of them leaks
    /// an internal detail (no `flatpak`, no `--installation=data`, no XDG
    /// path) — the operator is told what is true, in their own vocabulary.
    LauncherAppsScanning,
    LauncherAppsNoneInstalled,
    LauncherAppsReadFailed,
    /// Section heading for the Launcher's installable-catalog rows
    /// (`apps::catalog`) — new chrome with no design-board precedent (the
    /// boards only ever showed 交辦/應用程式/檔案).
    LauncherSectionInstallable,
    /// WP-A4-4 (2026-08-22): the flatpak install confirmation gate
    /// (`overlay::install_gate`). All new chrome with no design-board
    /// precedent — the boards never show an install flow — so it routes
    /// through this catalog like the `Launcher*`/`Notif*` keys above,
    /// rather than being hardcoded like `fake_data`'s registry DATA.
    /// `InstallGateSizeUnknown` deliberately says "無法取得" rather than a
    /// dash or a zero: the sheet has to be honest that the number could not
    /// be determined, not silently blank (see that module's header comment).
    LauncherInstallButton,
    InstallGateTitle,
    InstallGateNotice,
    InstallGateSourceLabel,
    InstallGateSizeLabel,
    InstallGateLocationLabel,
    InstallGateSizeProbing,
    InstallGateSizeUnknown,
    InstallGateConfirmButton,
    InstallGateInstallingLabel,
    InstallGateStartedLabel,
    InstallGateFailedLabel,
    VerifiedTierVerified,
    VerifiedTierWorks,
    VerifiedTierPartial,
    VerifiedTierUnsupported,
    VerifiedTierUnrated,

    /// A1 result-loopback (2026-08-24): the Launcher's "Enter 交辦" submit
    /// path (`overlay::launcher::try_submit_delegate`) failing BEFORE a
    /// task was ever created — new chrome, same catalog as every other
    /// `Launcher*`/`Notif*` honest-state string above. Two distinct
    /// failures get two distinct sentences (5.誠實回報: no generic "出錯
    ///了" that could mean anything): no agent this session can delegate
    /// to at all, versus the submit RPC itself failing (offline gateway,
    /// rejected request, etc.).
    LauncherDelegateNoAgent,
    LauncherDelegateSubmitFailed,
    /// The failure card's title (`overlay::launcher::
    /// post_submit_failure_card`) — separate from the two sentences above
    /// so either can be reused as the card BODY under one shared heading.
    LauncherDelegateSubmitFailedTitle,

    /// A1 result-loopback (2026-08-24): `main.rs::post_task_result_card`'s
    /// three terminal-state card summaries (a single `{}` placeholder for
    /// the task's own title, `t1`), the honest fallback when the gateway
    /// gave no `result_summary`/`judge_feedback`/`pause_reason` at all, and
    /// the two decision buttons a `needs_human` card offers
    /// (`gateway_client::decide_goal_task`, `action: "retry"|"abort"` — the
    /// SAME verbs the dashboard's needs_human board sends, see
    /// `overlay/notifications_apps.rs`'s own doc comment on why only these
    /// two ride a notification card). User-facing, zero internal
    /// vocabulary: no "goal_mode", "needs_human", or task id ever appears.
    TaskResultDoneSummary,
    TaskResultFailedSummary,
    TaskResultNeedsHumanSummary,
    TaskResultNoDetail,
    TaskResultRetryButton,
    TaskResultAbortButton,
    /// Posted (also through `post_system`, `system_task: None`) when a
    /// retry/abort click's own `tasks.goal_decide` call fails — the
    /// original `needs_human` card is deliberately left in place on this
    /// path (`overlay/notifications_apps.rs::apply_decide_outcome`) so the
    /// operator can just press it again, but silently doing nothing beyond
    /// that would still be exactly the kind of hidden failure 5.誠實回報
    /// forbids, hence this second card.
    TaskResultDecideFailedTitle,
    TaskResultDecideFailed,
}

/// Look up `key` in `locale`'s catalog. Never falls through to a "missing
/// key" placeholder the way `duduclaw-native-gui/src/i18n/mod.rs`'s `t()`
/// does — there is nothing to fall through TO: each of `zh_tw`/`en`/`ja_jp`
/// below is a total function over `Key` (every arm required, checked at
/// compile time), so `en`/`ja_jp` are honest full translations, not an
/// English-fallback catalog with holes.
pub fn t(locale: Locale, key: Key) -> &'static str {
    match locale {
        Locale::ZhTw => zh_tw(key),
        Locale::En => en(key),
        Locale::JaJp => ja_jp(key),
    }
}

/// `t()` with a single `{}` placeholder substituted — the only shape any
/// OOBE string needs (an SSID, a count, a template id), so a single
/// unnamed placeholder (vs. native-gui's `t1`'s named `{param}` form) is
/// enough; the substituted VALUE's position is still locale-controlled (the
/// catalog string decides where `{}` sits, e.g. English "Connected to {}"
/// vs. Japanese "{} に接続済み" — prefix vs. suffix), which is the whole
/// reason this isn't just a `format!("{prefix}{value}")` call at each site.
pub fn t1(locale: Locale, key: Key, value: &str) -> String {
    t(locale, key).replacen("{}", value, 1)
}

fn zh_tw(key: Key) -> &'static str {
    match key {
        Key::NavBack => "返回",
        Key::NavSkip => "略過",
        Key::NavContinue => "繼續",
        Key::NavGetStarted => "開始使用",

        Key::InputDetectionTitle => "歡迎使用 DuDuClaw OS",
        Key::InputDetectionSubtitle => "正在偵測已連接的輸入裝置…",
        Key::InputDetectionKeyboard => "鍵盤",
        Key::InputDetectionMouse => "滑鼠",
        Key::InputDetectionDetected => "已偵測",

        Key::LanguageAccessibilityEntry => "輔助使用設定",
        Key::LanguageAccessibilityCollapse => "收合 ▲",
        Key::LanguageAccessibilityExpand => "展開 ▼",
        Key::LanguageAccessibilityPlaceholder => "以上選項尚未開放調整，之後會在設定中提供",
        Key::LanguageA11ySeeingLabel => "視覺",
        Key::LanguageA11ySeeingDesc => "放大文字、高對比、朗讀畫面",
        Key::LanguageA11yHearingLabel => "聽覺",
        Key::LanguageA11yHearingDesc => "單聲道、視覺提示音",
        Key::LanguageA11yTypingLabel => "打字",
        Key::LanguageA11yTypingDesc => "螢幕鍵盤、相黏鍵、慢速鍵",
        Key::LanguageA11yPointingLabel => "指向與點按",
        Key::LanguageA11yPointingDesc => "指標大小與造型、滑鼠鍵",
        Key::LanguageA11yZoomLabel => "縮放",
        Key::LanguageA11yZoomDesc => "畫面放大鏡",

        Key::CommonSelected => "已選擇",

        Key::NetworkTitle => "連接網路",
        Key::NetworkSubtitlePrompt => "選擇一個 Wi-Fi 網路以繼續",
        Key::NetworkConnectedTo => "已連線：{}",
        Key::NetworkSecuredBadge => "需密碼",
        Key::NetworkConnectedBadge => "已連線",
        Key::NetworkNeverScanned => "尚未掃描 Wi-Fi 網路",
        Key::NetworkScanButton => "掃描 Wi-Fi",
        Key::NetworkRescanButton => "重新整理",
        Key::NetworkScanningStatus => "正在掃描 Wi-Fi…",
        Key::NetworkScanFailedStatus => "掃描失敗，請重試",
        Key::NetworkScanEmptyStatus => "找不到任何 Wi-Fi 網路",
        Key::NetworkDemoModeNotice => "示範模式：目前顯示的是範例 Wi-Fi 清單，並非真實掃描結果",
        Key::NetworkPskLabel => "Wi-Fi 密碼",
        Key::NetworkConnectButton => "連線",
        Key::NetworkConnectingButton => "連線中…",
        Key::NetworkConnectingStatus => "正在連線…",
        Key::NetworkCancelButton => "取消",
        Key::NetworkPskLengthError => "密碼長度需為 8–63 個字元",
        Key::NetworkWrongPasswordError => "密碼錯誤，請重新輸入",
        Key::NetworkConnectUnreachableError => "無法連線，請稍後重試",
        Key::NetworkWiredConnected => "已透過網路線連線",
        Key::NetworkNotFoundError => "找不到這個網路，可能已離開範圍",
        Key::NetworkOutOfRangeError => "訊號太弱連不上，請靠近路由器再試",
        Key::NetworkNoAdapterError => "這台機器沒有偵測到 Wi-Fi 硬體，請改用網路線",
        Key::NetworkDriverMissingError => "Wi-Fi 硬體無法啟動（缺少驅動韌體），請改用網路線並回報型號",
        Key::NetworkNoIpError => "已連上 {}，但沒有取得網路位址（可能是路由器 DHCP 問題）",
        Key::NetworkPortalNotice => "已連上 {}，需要在瀏覽器完成登入",
        Key::NetworkPortalOpenButton => "開啟登入頁",
        Key::NetworkBackendUnavailableError => "網路服務未啟動，請重新開機或聯絡支援",
        Key::NetworkUnsupportedSecurityError => "這個網路使用過舊的加密方式（WEP），系統不支援",
        Key::NetworkUnavailableHint => "找不到網路服務，請改用網路線繼續設定，或重新開機後再試一次",
        Key::NetworkContinueBlockedNotice => "尚未偵測到網路連線——選擇 Wi-Fi，或接上網路線後再按一次繼續",
        Key::NetworkStatusCheckingNotice => "正在確認網路狀態…",

        Key::UpdateTitle => "系統更新",
        Key::UpdateChecking => "正在檢查更新…",
        Key::UpdateUpToDate => "已是最新版本",

        Key::AccountTitle => "建立操作者帳號",
        Key::AccountSubtitle => "這是唯一必要的身分設定步驟",
        Key::AccountNameLabel => "操作者名稱",
        Key::AccountPasswordLabel => "密碼",
        Key::AccountValidationError => "請輸入操作者名稱與密碼",
        Key::AccountCreateButton => "建立帳號",
        Key::AccountCreatedButton => "已建立帳號",
        Key::AccountCreatingButton => "建立中…",
        Key::AccountPasswordTooShortError => "密碼至少需要 8 個字元",
        Key::AccountAlreadyClaimedInfo => "此裝置已完成初始設定，沿用既有的管理者帳號。",
        Key::AccountUnreachableError => "無法連線到本機服務，請稍後重試。",

        Key::LiveWifiTitle => "設定 Wi-Fi",
        Key::LiveWifiSubtitle => "裝完首次開機會自動連線，留空可跳過",
        Key::LiveWifiSsidLabel => "網路名稱（SSID）",
        Key::LiveWifiPskLabel => "Wi-Fi 密碼",
        Key::LiveWifiOptionalHint => "此步可留空：有線網路或稍後在設定中連線",
        Key::LiveWifiErrSsidMissing => "輸入了密碼但沒有網路名稱",
        Key::LiveWifiErrSsidTooLong => "網路名稱過長",
        Key::LiveWifiErrPskLength => "Wi-Fi 密碼長度須為 8–63 字元",

        Key::RuntimeAuthTitle => "AI Runtime 授權",
        Key::RuntimeAuthSubtitle => "這台機器的 AI runtime 需要授權才能開始工作",
        Key::RuntimeAuthAuthorized => "已授權，可以繼續",
        Key::RuntimeAuthAuthorizeNow => "立即設定",
        Key::RuntimeAuthDeferLater => "稍後再說",

        Key::PrivacyTitle => "隱私與遙測",
        Key::PrivacySubtitle => "以下選項預設全部關閉，可以隨時在設定中調整",
        Key::PrivacyUsageStatsLabel => "使用統計",
        Key::PrivacyUsageStatsDesc => "協助改善 DuDuClaw OS 的使用體驗",
        Key::PrivacyErrorReportsLabel => "錯誤回報",
        Key::PrivacyErrorReportsDesc => "當機時自動傳送診斷資料",
        Key::PrivacyPersonalizationLabel => "個人化建議",
        Key::PrivacyPersonalizationDesc => "依使用習慣調整介面與建議",
        Key::PrivacyMarketingLabel => "行銷與活動通知",
        Key::PrivacyMarketingDesc => "接收新功能與活動的相關通知",

        Key::TemplatesTitle => "挑選產業板模",
        Key::TemplatesSubtitle => "可以先跳過，之後隨時在管理面加入",
        Key::TemplatesExpressTitle => "快速開始",
        Key::TemplatesExpressDesc => "一鍵套用預設 AI 團隊組合",
        Key::TemplatesExpressApply => "套用 Express",
        Key::TemplatesExpressApplied => "已選擇 Express",
        Key::TemplatesCustomHint => "或自行挑選產業板模",
        Key::TemplatesSkip => "略過，稍後再設定",

        Key::ThemeTitle => "選擇外觀",
        Key::ThemeSubtitle => "亮色或暗色，之後可以在設定中變更",
        Key::ThemeLight => "亮色",
        Key::ThemeDark => "暗色",

        Key::FinishTitle => "設定完成",
        Key::FinishSubtitle => "即將進入 DuDuClaw 桌面",
        Key::FinishSummaryLanguage => "語言",
        Key::FinishSummaryNetwork => "網路",
        Key::FinishSummaryAccount => "帳號",
        Key::FinishSummaryRuntime => "AI Runtime",
        Key::FinishSummaryPrivacy => "隱私",
        Key::FinishSummaryTemplates => "板模",
        Key::FinishRuntimeAuthorized => "已授權",
        Key::FinishRuntimeDeferred => "已延後",
        Key::FinishRuntimeNotSet => "未設定",
        Key::FinishNetworkNotConnected => "未連線",
        Key::FinishAccountCreated => "已建立",
        Key::FinishAccountNotCreated => "未建立",
        Key::FinishPrivacyAllOff => "全部關閉",
        Key::FinishPrivacyOnCount => "{} 項已開啟",
        Key::FinishTemplatesExpress => "快速開始 Express",
        Key::FinishTemplatesCustom => "自訂：{}",
        Key::FinishTemplatesSkipped => "已略過",
        Key::FinishTemplatesNotChosen => "尚未選擇",

        Key::NotifOfflineBanner => "無法連線到本機服務，請稍後再試",
        Key::NotifLoadingLabel => "正在載入審批項目…",
        Key::NotifEmptyLabel => "目前沒有待你核准的項目",
        Key::NotifConfirmButton => "確定",
        Key::NotifDecidingLabel => "處理中…",
        Key::NotifDecideFailedLabel => "送出失敗，請重試",
        Key::NotifRetryButton => "重試",

        Key::NotifAppSectionLabel => "應用程式通知",
        Key::NotifDismissButton => "關閉",
        Key::NotifClearAllButton => "全部清除",
        Key::NotifDaemonNameTakenBanner => "系統通知由另一個服務接手，應用程式通知不會顯示在這裡",
        Key::NotifDaemonFailedBanner => "無法接收應用程式通知：{}",
        Key::NotifDaemonUnsupportedBanner => "此平台不支援應用程式通知",
        Key::NotifMergedCount => "另有 {} 則",
        Key::NotifAgeJustNow => "剛剛",
        Key::NotifAgeMinutes => "{} 分鐘前",
        Key::NotifAgeHours => "{} 小時前",
        Key::NotifAgeDays => "{} 天前",
        Key::NotifTaskSectionLabel => "進行中任務",

        Key::LockAwaySummaryTitle => "你離開的 {}",
        Key::LockPendingCountLabel => "{} 件等你決定",
        Key::LockUnlockHint => "按任意鍵或點擊以輸入密碼",
        Key::LockPasswordPlaceholder => "輸入密碼解鎖",
        Key::LockPasswordWrongError => "密碼錯誤，請重新輸入",
        Key::LockOfflineError => "本機服務未回應，請稍後再試",
        Key::LockVerifyingLabel => "驗證中…",
        Key::LockThrottledLabel => "嘗試次數過多，請稍候再試",

        Key::LockPowerRestart => "重新啟動",
        Key::LockPowerShutdown => "關機",
        Key::LockPowerConfirmRestart => "確定要重新啟動這台機器嗎？",
        Key::LockPowerConfirmShutdown => "確定要關閉這台機器嗎？",
        Key::LockPowerSending => "正在送出…",
        Key::LockPowerFailed => "本機服務未回應，指令沒有送出",
        Key::LockPowerUnsupported => "這台機器目前不支援從鎖定畫面執行這個動作",

        Key::PointerTitle => "指向與點按",
        Key::PointerSubtitle => "協助工具 · 指標的大小與造型",
        Key::PointerEntryDesc => "指標大小與造型",
        Key::PointerSectionAccessibility => "協助工具",
        Key::PointerShapeLabel => "造型",
        Key::PointerSizeLabel => "大小",
        Key::PointerSourceSystem => "系統標準",
        Key::PointerSourceSystemDesc => "預設",
        Key::PointerSourceBrand => "嘟嘟爪印",
        Key::PointerSourceBrandDesc => "只換箭頭與抓取手勢，其餘沿用系統形狀",
        Key::PointerSizeDefault => "預設",
        Key::PointerSizeMedium => "中",
        Key::PointerSizeLarge => "大",
        Key::PointerSizeExtraLarge => "特大",
        Key::PointerSizeLargest => "最大",
        Key::PointerZoomHint => "指標大小可以和「縮放」搭配使用，讓指標更容易看見。",
        Key::PointerLoading => "正在讀取指標設定…",
        Key::PointerUnavailable => "找不到桌面服務，這台機器上目前無法調整指標。",
        Key::PointerSizeUnsupported => "這台機器的桌面服務還不支援調整指標大小。",
        Key::PointerApplyFailed => "無法套用這個設定，請稍後再試。",
        Key::PointerThemeMissing => "這台機器沒有安裝選定的指標圖案，目前畫的是系統指標。",
        Key::PointerEnvPinned => "這台機器的指標設定由開機環境固定，這裡的選擇下次開機不會生效。",

        Key::LauncherSearchPlaceholder => "輸入以搜尋…",
        Key::LauncherNoAppResults => "沒有符合的 app",
        Key::LauncherAppsScanning => "正在尋找這台機器上的應用程式…",
        Key::LauncherAppsNoneInstalled => "這台機器上還沒有可以開啟的應用程式",
        Key::LauncherAppsReadFailed => "讀不到這台機器上的應用程式清單",
        Key::LauncherSectionInstallable => "可安裝",
        Key::LauncherInstallButton => "安裝",
        Key::InstallGateTitle => "要安裝這個 app 嗎？",
        Key::InstallGateNotice => "安裝會下載檔案並改變這台機器的內容。確定前不會執行任何動作。",
        Key::InstallGateSourceLabel => "來源",
        Key::InstallGateSizeLabel => "下載大小",
        Key::InstallGateLocationLabel => "安裝位置",
        Key::InstallGateSizeProbing => "查詢中…",
        Key::InstallGateSizeUnknown => "無法取得",
        Key::InstallGateConfirmButton => "開始安裝",
        Key::InstallGateInstallingLabel => "正在開始…",
        Key::InstallGateStartedLabel => "已開始安裝，完成後會出現在 app 清單",
        Key::InstallGateFailedLabel => "無法開始安裝",
        Key::VerifiedTierVerified => "已驗證",
        Key::VerifiedTierWorks => "可用",
        Key::VerifiedTierPartial => "部分支援",
        Key::VerifiedTierUnsupported => "不支援",
        Key::VerifiedTierUnrated => "未評級",

        Key::LauncherDelegateNoAgent => "找不到可交辦的 AI 員工，請先在儀表板建立一個。",
        Key::LauncherDelegateSubmitFailed => "交辦沒有送出，請稍後再試一次。",
        Key::LauncherDelegateSubmitFailedTitle => "交辦沒有送出",

        Key::TaskResultDoneSummary => "「{}」已完成",
        Key::TaskResultFailedSummary => "「{}」失敗",
        Key::TaskResultNeedsHumanSummary => "「{}」需要你確認",
        Key::TaskResultNoDetail => "沒有附上摘要內容。",
        Key::TaskResultRetryButton => "重試",
        Key::TaskResultAbortButton => "放棄",
        Key::TaskResultDecideFailedTitle => "決定沒有送出",
        Key::TaskResultDecideFailed => "沒有送出成功，請稍後再試一次。",
    }
}

fn en(key: Key) -> &'static str {
    match key {
        Key::NavBack => "Back",
        Key::NavSkip => "Skip",
        Key::NavContinue => "Continue",
        Key::NavGetStarted => "Get started",

        Key::InputDetectionTitle => "Welcome to DuDuClaw OS",
        Key::InputDetectionSubtitle => "Detecting connected input devices…",
        Key::InputDetectionKeyboard => "Keyboard",
        Key::InputDetectionMouse => "Mouse",
        Key::InputDetectionDetected => "Detected",

        Key::LanguageAccessibilityEntry => "Accessibility",
        Key::LanguageAccessibilityCollapse => "Collapse ▲",
        Key::LanguageAccessibilityExpand => "Expand ▼",
        Key::LanguageAccessibilityPlaceholder => "None of these can be adjusted yet. They'll arrive in Settings in a future update.",
        Key::LanguageA11ySeeingLabel => "Seeing",
        Key::LanguageA11ySeeingDesc => "Larger text, high contrast, screen reader",
        Key::LanguageA11yHearingLabel => "Hearing",
        Key::LanguageA11yHearingDesc => "Mono audio, visual alerts",
        Key::LanguageA11yTypingLabel => "Typing",
        Key::LanguageA11yTypingDesc => "On-screen keyboard, sticky keys, slow keys",
        Key::LanguageA11yPointingLabel => "Pointing & clicking",
        Key::LanguageA11yPointingDesc => "Pointer size and style, mouse keys",
        Key::LanguageA11yZoomLabel => "Zoom",
        Key::LanguageA11yZoomDesc => "Screen magnifier",

        Key::CommonSelected => "Selected",

        Key::NetworkTitle => "Connect to a network",
        Key::NetworkSubtitlePrompt => "Choose a Wi-Fi network to continue",
        Key::NetworkConnectedTo => "Connected to {}",
        Key::NetworkSecuredBadge => "Password required",
        Key::NetworkConnectedBadge => "Connected",
        Key::NetworkNeverScanned => "Wi-Fi networks haven't been scanned yet",
        Key::NetworkScanButton => "Scan for Wi-Fi",
        Key::NetworkRescanButton => "Refresh",
        Key::NetworkScanningStatus => "Scanning for Wi-Fi networks…",
        Key::NetworkScanFailedStatus => "Scan failed, please try again",
        Key::NetworkScanEmptyStatus => "No Wi-Fi networks found",
        Key::NetworkDemoModeNotice => "Demo mode: the Wi-Fi list shown is example data, not a real scan",
        Key::NetworkPskLabel => "Wi-Fi password",
        Key::NetworkConnectButton => "Connect",
        Key::NetworkConnectingButton => "Connecting…",
        Key::NetworkConnectingStatus => "Connecting…",
        Key::NetworkCancelButton => "Cancel",
        Key::NetworkPskLengthError => "Password must be 8–63 characters",
        Key::NetworkWrongPasswordError => "Incorrect password, please try again",
        Key::NetworkConnectUnreachableError => "Couldn't connect, please try again",
        Key::NetworkWiredConnected => "Connected over a wired network",
        Key::NetworkNotFoundError => "Couldn't find this network — it may be out of range",
        Key::NetworkOutOfRangeError => "Signal too weak to connect. Move closer to the router and try again",
        Key::NetworkNoAdapterError => "No Wi-Fi hardware detected on this machine. Please use a wired connection instead",
        Key::NetworkDriverMissingError => "The Wi-Fi hardware couldn't start (missing driver firmware). Please use a wired connection and report the model",
        Key::NetworkNoIpError => "Connected to {}, but didn't get a network address (the router's DHCP may be having trouble)",
        Key::NetworkPortalNotice => "Connected to {} — sign in through a browser to finish",
        Key::NetworkPortalOpenButton => "Open the sign-in page",
        Key::NetworkBackendUnavailableError => "The network service isn't running. Please restart, or contact support",
        Key::NetworkUnsupportedSecurityError => "This network uses an outdated encryption method (WEP), which isn't supported",
        Key::NetworkUnavailableHint => "Couldn't reach the network service. Please use a wired connection to continue setup, or restart and try again",
        Key::NetworkContinueBlockedNotice => "No network connection detected yet — choose a Wi-Fi network, or plug in a cable and press Continue again",
        Key::NetworkStatusCheckingNotice => "Checking network status…",

        Key::UpdateTitle => "System update",
        Key::UpdateChecking => "Checking for updates…",
        Key::UpdateUpToDate => "You're up to date",

        Key::AccountTitle => "Create an operator account",
        Key::AccountSubtitle => "This is the only required identity step",
        Key::AccountNameLabel => "Operator name",
        Key::AccountPasswordLabel => "Password",
        Key::AccountValidationError => "Enter an operator name and password",
        Key::AccountCreateButton => "Create account",
        Key::AccountCreatedButton => "Account created",
        Key::AccountCreatingButton => "Creating…",
        Key::AccountPasswordTooShortError => "Password must be at least 8 characters",
        Key::AccountAlreadyClaimedInfo => "This device has already been set up. Using the existing administrator account.",
        Key::AccountUnreachableError => "Couldn't reach the local service. Please try again shortly.",

        Key::LiveWifiTitle => "Set up Wi-Fi",
        Key::LiveWifiSubtitle => "The target system connects automatically on first boot. Leave blank to skip.",
        Key::LiveWifiSsidLabel => "Network name (SSID)",
        Key::LiveWifiPskLabel => "Wi-Fi password",
        Key::LiveWifiOptionalHint => "This step is optional: use a wired connection, or connect later from Settings",
        Key::LiveWifiErrSsidMissing => "A password was entered, but no network name",
        Key::LiveWifiErrSsidTooLong => "The network name is too long",
        Key::LiveWifiErrPskLength => "The Wi-Fi password must be 8–63 characters",

        Key::RuntimeAuthTitle => "Authorize AI runtime",
        Key::RuntimeAuthSubtitle => "This device's AI runtime needs authorization before it can start working",
        Key::RuntimeAuthAuthorized => "Authorized. Ready to continue.",
        Key::RuntimeAuthAuthorizeNow => "Set up now",
        Key::RuntimeAuthDeferLater => "Set up later",

        Key::PrivacyTitle => "Privacy & diagnostics",
        Key::PrivacySubtitle => "These are all off by default. Change them anytime in Settings.",
        Key::PrivacyUsageStatsLabel => "Usage statistics",
        Key::PrivacyUsageStatsDesc => "Help improve the DuDuClaw OS experience",
        Key::PrivacyErrorReportsLabel => "Error reports",
        Key::PrivacyErrorReportsDesc => "Automatically send diagnostic data when a crash occurs",
        Key::PrivacyPersonalizationLabel => "Personalization",
        Key::PrivacyPersonalizationDesc => "Tailor the interface and suggestions to how you work",
        Key::PrivacyMarketingLabel => "Marketing & announcements",
        Key::PrivacyMarketingDesc => "Get notified about new features and events",

        Key::TemplatesTitle => "Choose an industry template",
        Key::TemplatesSubtitle => "You can skip this and add one later from the console",
        Key::TemplatesExpressTitle => "Quick start",
        Key::TemplatesExpressDesc => "Apply the default AI team lineup with one click",
        Key::TemplatesExpressApply => "Apply Express",
        Key::TemplatesExpressApplied => "Express applied",
        Key::TemplatesCustomHint => "Or pick an industry template yourself",
        Key::TemplatesSkip => "Skip for now",

        Key::ThemeTitle => "Choose your appearance",
        Key::ThemeSubtitle => "Light or dark — change this anytime in Settings",
        Key::ThemeLight => "Light",
        Key::ThemeDark => "Dark",

        Key::FinishTitle => "Setup complete",
        Key::FinishSubtitle => "Taking you to your DuDuClaw desktop",
        Key::FinishSummaryLanguage => "Language",
        Key::FinishSummaryNetwork => "Network",
        Key::FinishSummaryAccount => "Account",
        Key::FinishSummaryRuntime => "AI runtime",
        Key::FinishSummaryPrivacy => "Privacy",
        Key::FinishSummaryTemplates => "Template",
        Key::FinishRuntimeAuthorized => "Authorized",
        Key::FinishRuntimeDeferred => "Deferred",
        Key::FinishRuntimeNotSet => "Not set",
        Key::FinishNetworkNotConnected => "Not connected",
        Key::FinishAccountCreated => "Created",
        Key::FinishAccountNotCreated => "Not created",
        Key::FinishPrivacyAllOff => "All off",
        Key::FinishPrivacyOnCount => "{} enabled",
        Key::FinishTemplatesExpress => "Quick start (Express)",
        Key::FinishTemplatesCustom => "Custom: {}",
        Key::FinishTemplatesSkipped => "Skipped",
        Key::FinishTemplatesNotChosen => "Not chosen",

        Key::NotifOfflineBanner => "Couldn't reach the local service. Please try again shortly.",
        Key::NotifLoadingLabel => "Loading approvals…",
        Key::NotifEmptyLabel => "Nothing waiting on your approval right now",
        Key::NotifConfirmButton => "Confirm",
        Key::NotifDecidingLabel => "Processing…",
        Key::NotifDecideFailedLabel => "Failed to submit. Please retry.",
        Key::NotifRetryButton => "Retry",

        Key::NotifAppSectionLabel => "App notifications",
        Key::NotifDismissButton => "Dismiss",
        Key::NotifClearAllButton => "Clear all",
        Key::NotifDaemonNameTakenBanner => "Another service is handling system notifications, so app notifications won't appear here",
        Key::NotifDaemonFailedBanner => "Can't receive app notifications: {}",
        Key::NotifDaemonUnsupportedBanner => "App notifications aren't supported on this platform",
        Key::NotifMergedCount => "{} more",
        Key::NotifAgeJustNow => "Just now",
        Key::NotifAgeMinutes => "{} min ago",
        Key::NotifAgeHours => "{} h ago",
        Key::NotifAgeDays => "{} d ago",
        Key::NotifTaskSectionLabel => "In progress",

        Key::LockAwaySummaryTitle => "Away for {}",
        Key::LockPendingCountLabel => "{} awaiting your decision",
        Key::LockUnlockHint => "Press any key or click to enter your password",
        Key::LockPasswordPlaceholder => "Enter password to unlock",
        Key::LockPasswordWrongError => "Incorrect password, please try again",
        Key::LockOfflineError => "Local service isn't responding. Please try again shortly.",
        Key::LockVerifyingLabel => "Verifying…",
        Key::LockThrottledLabel => "Too many attempts. Please wait a moment.",

        Key::LockPowerRestart => "Restart",
        Key::LockPowerShutdown => "Shut down",
        Key::LockPowerConfirmRestart => "Restart this machine?",
        Key::LockPowerConfirmShutdown => "Shut down this machine?",
        Key::LockPowerSending => "Sending…",
        Key::LockPowerFailed => "The local service didn't respond; nothing was sent.",
        Key::LockPowerUnsupported => "This machine can't do that from the lock screen.",

        Key::PointerTitle => "Pointing & clicking",
        Key::PointerSubtitle => "Accessibility · pointer size and style",
        Key::PointerEntryDesc => "Pointer size and style",
        Key::PointerSectionAccessibility => "Accessibility",
        Key::PointerShapeLabel => "Style",
        Key::PointerSizeLabel => "Size",
        Key::PointerSourceSystem => "System standard",
        Key::PointerSourceSystemDesc => "Default",
        Key::PointerSourceBrand => "DuDu paw",
        Key::PointerSourceBrandDesc => "Replaces the arrow and grab shapes only; everything else stays as the system draws it",
        Key::PointerSizeDefault => "Default",
        Key::PointerSizeMedium => "Medium",
        Key::PointerSizeLarge => "Large",
        Key::PointerSizeExtraLarge => "Extra large",
        Key::PointerSizeLargest => "Largest",
        Key::PointerZoomHint => "Pointer size can be combined with Zoom to make it easier to see the pointer.",
        Key::PointerLoading => "Reading pointer settings…",
        Key::PointerUnavailable => "The desktop service isn't reachable, so the pointer can't be changed on this machine.",
        Key::PointerSizeUnsupported => "This machine's desktop service can't change the pointer size yet.",
        Key::PointerApplyFailed => "Couldn't apply that setting. Please try again shortly.",
        Key::PointerThemeMissing => "The chosen pointer artwork isn't installed on this machine, so system pointers are being drawn.",
        Key::PointerEnvPinned => "This machine's pointer settings are pinned at startup, so a choice made here won't apply after a restart.",

        Key::LauncherSearchPlaceholder => "Type to search…",
        Key::LauncherNoAppResults => "No matching apps",
        Key::LauncherAppsScanning => "Looking for the apps on this machine\u{2026}",
        Key::LauncherAppsNoneInstalled => "There are no apps on this machine yet",
        Key::LauncherAppsReadFailed => "Couldn't read the list of apps on this machine",
        Key::LauncherSectionInstallable => "Available to install",
        Key::LauncherInstallButton => "Install",
        Key::InstallGateTitle => "Install this app?",
        Key::InstallGateNotice => "Installing downloads files and changes what is on this machine. Nothing runs until you confirm.",
        Key::InstallGateSourceLabel => "Source",
        Key::InstallGateSizeLabel => "Download size",
        Key::InstallGateLocationLabel => "Installs to",
        Key::InstallGateSizeProbing => "Checking\u{2026}",
        Key::InstallGateSizeUnknown => "Could not determine",
        Key::InstallGateConfirmButton => "Start install",
        Key::InstallGateInstallingLabel => "Starting\u{2026}",
        Key::InstallGateStartedLabel => "Install started; the app appears in the list when it finishes",
        Key::InstallGateFailedLabel => "Could not start the install",
        Key::VerifiedTierVerified => "Verified",
        Key::VerifiedTierWorks => "Works",
        Key::VerifiedTierPartial => "Partial",
        Key::VerifiedTierUnsupported => "Unsupported",
        Key::VerifiedTierUnrated => "Unrated",

        Key::LauncherDelegateNoAgent => "No agent is reachable to delegate to — set one up on the dashboard first.",
        Key::LauncherDelegateSubmitFailed => "The delegation wasn't sent — please try again in a moment.",
        Key::LauncherDelegateSubmitFailedTitle => "Delegation not sent",

        Key::TaskResultDoneSummary => "\u{201c}{}\u{201d} is done",
        Key::TaskResultFailedSummary => "\u{201c}{}\u{201d} failed",
        Key::TaskResultNeedsHumanSummary => "\u{201c}{}\u{201d} needs your input",
        Key::TaskResultNoDetail => "No summary was attached.",
        Key::TaskResultRetryButton => "Retry",
        Key::TaskResultAbortButton => "Abort",
        Key::TaskResultDecideFailedTitle => "That decision wasn't sent",
        Key::TaskResultDecideFailed => "That didn't go through — please try again in a moment.",
    }
}

fn ja_jp(key: Key) -> &'static str {
    match key {
        Key::NavBack => "戻る",
        Key::NavSkip => "スキップ",
        Key::NavContinue => "続ける",
        Key::NavGetStarted => "使ってみる",

        Key::InputDetectionTitle => "DuDuClaw OS へようこそ",
        Key::InputDetectionSubtitle => "接続されている入力デバイスを検出しています…",
        Key::InputDetectionKeyboard => "キーボード",
        Key::InputDetectionMouse => "マウス",
        Key::InputDetectionDetected => "検出済み",

        Key::LanguageAccessibilityEntry => "アクセシビリティ設定",
        Key::LanguageAccessibilityCollapse => "閉じる ▲",
        Key::LanguageAccessibilityExpand => "開く ▼",
        Key::LanguageAccessibilityPlaceholder => "上記の項目はまだ変更できません。今後のアップデートで設定から対応予定です。",
        Key::LanguageA11ySeeingLabel => "視覚",
        Key::LanguageA11ySeeingDesc => "文字の拡大、ハイコントラスト、画面の読み上げ",
        Key::LanguageA11yHearingLabel => "聴覚",
        Key::LanguageA11yHearingDesc => "モノラル音声、視覚的な通知音",
        Key::LanguageA11yTypingLabel => "入力",
        Key::LanguageA11yTypingDesc => "スクリーンキーボード、固定キー、スローキー",
        Key::LanguageA11yPointingLabel => "ポインタ操作",
        Key::LanguageA11yPointingDesc => "ポインタの大きさと形状、マウスキー",
        Key::LanguageA11yZoomLabel => "ズーム",
        Key::LanguageA11yZoomDesc => "画面の拡大鏡",

        Key::CommonSelected => "選択済み",

        Key::NetworkTitle => "ネットワークに接続",
        Key::NetworkSubtitlePrompt => "続けるには Wi-Fi ネットワークを選択してください",
        Key::NetworkConnectedTo => "{} に接続済み",
        Key::NetworkSecuredBadge => "パスワードが必要",
        Key::NetworkConnectedBadge => "接続済み",
        Key::NetworkNeverScanned => "まだ Wi-Fi ネットワークをスキャンしていません",
        Key::NetworkScanButton => "Wi-Fi をスキャン",
        Key::NetworkRescanButton => "更新",
        Key::NetworkScanningStatus => "Wi-Fi ネットワークをスキャン中…",
        Key::NetworkScanFailedStatus => "スキャンに失敗しました。もう一度お試しください",
        Key::NetworkScanEmptyStatus => "Wi-Fi ネットワークが見つかりません",
        Key::NetworkDemoModeNotice => "デモモード：表示されている Wi-Fi 一覧はサンプルデータで、実際のスキャン結果ではありません",
        Key::NetworkPskLabel => "Wi-Fi パスワード",
        Key::NetworkConnectButton => "接続",
        Key::NetworkConnectingButton => "接続中…",
        Key::NetworkConnectingStatus => "接続中…",
        Key::NetworkCancelButton => "キャンセル",
        Key::NetworkPskLengthError => "パスワードは8〜63文字で入力してください",
        Key::NetworkWrongPasswordError => "パスワードが正しくありません。もう一度お試しください",
        Key::NetworkConnectUnreachableError => "接続できませんでした。もう一度お試しください",
        Key::NetworkWiredConnected => "有線ネットワークで接続済み",
        Key::NetworkNotFoundError => "このネットワークが見つかりません。電波の届く範囲外かもしれません",
        Key::NetworkOutOfRangeError => "電波が弱くて接続できません。ルーターに近づいてもう一度お試しください",
        Key::NetworkNoAdapterError => "この端末に Wi-Fi ハードウェアが検出されませんでした。有線接続をご利用ください",
        Key::NetworkDriverMissingError => "Wi-Fi ハードウェアを起動できません（ドライバーファームウェアがありません）。有線接続をご利用のうえ、機種名をご報告ください",
        Key::NetworkNoIpError => "{} に接続しましたが、ネットワークアドレスを取得できませんでした（ルーターの DHCP に問題がある可能性があります）",
        Key::NetworkPortalNotice => "{} に接続しました。ブラウザでログインを完了してください",
        Key::NetworkPortalOpenButton => "ログインページを開く",
        Key::NetworkBackendUnavailableError => "ネットワークサービスが起動していません。再起動するか、サポートにご連絡ください",
        Key::NetworkUnsupportedSecurityError => "このネットワークは古い暗号化方式（WEP）を使用しており、対応していません",
        Key::NetworkUnavailableHint => "ネットワークサービスに接続できません。有線接続でセットアップを続けるか、再起動してもう一度お試しください",
        Key::NetworkContinueBlockedNotice => "ネットワーク接続がまだ検出されていません。Wi-Fi を選択するか、ケーブルを接続してからもう一度「続ける」を押してください",
        Key::NetworkStatusCheckingNotice => "ネットワークの状態を確認しています…",

        Key::UpdateTitle => "システムアップデート",
        Key::UpdateChecking => "アップデートを確認しています…",
        Key::UpdateUpToDate => "最新の状態です",

        Key::AccountTitle => "オペレーターアカウントを作成",
        Key::AccountSubtitle => "必須の設定はこのステップのみです",
        Key::AccountNameLabel => "オペレーター名",
        Key::AccountPasswordLabel => "パスワード",
        Key::AccountValidationError => "オペレーター名とパスワードを入力してください",
        Key::AccountCreateButton => "アカウントを作成",
        Key::AccountCreatedButton => "作成済み",
        Key::AccountCreatingButton => "作成中…",
        Key::AccountPasswordTooShortError => "パスワードは8文字以上で入力してください",
        Key::AccountAlreadyClaimedInfo => "この端末はすでにセットアップ済みです。既存の管理者アカウントを使用します。",
        Key::AccountUnreachableError => "ローカルサービスに接続できません。しばらくしてから再試行してください。",

        Key::LiveWifiTitle => "Wi-Fi を設定",
        Key::LiveWifiSubtitle => "インストール完了後、初回起動時に自動接続します。空欄のままでスキップできます。",
        Key::LiveWifiSsidLabel => "ネットワーク名（SSID）",
        Key::LiveWifiPskLabel => "Wi-Fi パスワード",
        Key::LiveWifiOptionalHint => "このステップは省略できます：有線接続を使うか、後で設定から接続してください",
        Key::LiveWifiErrSsidMissing => "パスワードは入力されましたが、ネットワーク名がありません",
        Key::LiveWifiErrSsidTooLong => "ネットワーク名が長すぎます",
        Key::LiveWifiErrPskLength => "Wi-Fi パスワードは8〜63文字で入力してください",

        Key::RuntimeAuthTitle => "AI ランタイムの認証",
        Key::RuntimeAuthSubtitle => "この端末の AI ランタイムを使い始めるには認証が必要です",
        Key::RuntimeAuthAuthorized => "認証済み。続行できます。",
        Key::RuntimeAuthAuthorizeNow => "今すぐ設定",
        Key::RuntimeAuthDeferLater => "あとで設定",

        Key::PrivacyTitle => "プライバシーと診断",
        Key::PrivacySubtitle => "以下はすべて初期設定でオフです。設定からいつでも変更できます。",
        Key::PrivacyUsageStatsLabel => "使用状況の統計",
        Key::PrivacyUsageStatsDesc => "DuDuClaw OS の使い勝手向上に役立てます",
        Key::PrivacyErrorReportsLabel => "エラーレポート",
        Key::PrivacyErrorReportsDesc => "クラッシュ時に診断データを自動送信します",
        Key::PrivacyPersonalizationLabel => "パーソナライズ",
        Key::PrivacyPersonalizationDesc => "使い方に合わせて画面や提案を調整します",
        Key::PrivacyMarketingLabel => "お知らせとキャンペーン情報",
        Key::PrivacyMarketingDesc => "新機能やイベントの情報をお届けします",

        Key::TemplatesTitle => "業種テンプレートを選択",
        Key::TemplatesSubtitle => "スキップして、あとから管理画面で追加することもできます",
        Key::TemplatesExpressTitle => "クイックスタート",
        Key::TemplatesExpressDesc => "デフォルトの AI チーム構成をワンクリックで適用",
        Key::TemplatesExpressApply => "Express を適用",
        Key::TemplatesExpressApplied => "Express を適用済み",
        Key::TemplatesCustomHint => "または、業種テンプレートを自分で選ぶ",
        Key::TemplatesSkip => "今はスキップ",

        Key::ThemeTitle => "外観を選択",
        Key::ThemeSubtitle => "ライトまたはダーク。設定からいつでも変更できます",
        Key::ThemeLight => "ライト",
        Key::ThemeDark => "ダーク",

        Key::FinishTitle => "セットアップ完了",
        Key::FinishSubtitle => "まもなく DuDuClaw のデスクトップに移動します",
        Key::FinishSummaryLanguage => "言語",
        Key::FinishSummaryNetwork => "ネットワーク",
        Key::FinishSummaryAccount => "アカウント",
        Key::FinishSummaryRuntime => "AI ランタイム",
        Key::FinishSummaryPrivacy => "プライバシー",
        Key::FinishSummaryTemplates => "テンプレート",
        Key::FinishRuntimeAuthorized => "認証済み",
        Key::FinishRuntimeDeferred => "あとで設定",
        Key::FinishRuntimeNotSet => "未設定",
        Key::FinishNetworkNotConnected => "未接続",
        Key::FinishAccountCreated => "作成済み",
        Key::FinishAccountNotCreated => "未作成",
        Key::FinishPrivacyAllOff => "すべてオフ",
        Key::FinishPrivacyOnCount => "{} 件有効",
        Key::FinishTemplatesExpress => "クイックスタート（Express）",
        Key::FinishTemplatesCustom => "カスタム：{}",
        Key::FinishTemplatesSkipped => "スキップ済み",
        Key::FinishTemplatesNotChosen => "未選択",

        Key::NotifOfflineBanner => "ローカルサービスに接続できません。しばらくしてから再試行してください。",
        Key::NotifLoadingLabel => "承認項目を読み込み中…",
        Key::NotifEmptyLabel => "現在、承認待ちの項目はありません",
        Key::NotifConfirmButton => "確定",
        Key::NotifDecidingLabel => "処理中…",
        Key::NotifDecideFailedLabel => "送信に失敗しました。再試行してください。",
        Key::NotifRetryButton => "再試行",

        Key::NotifAppSectionLabel => "アプリの通知",
        Key::NotifDismissButton => "閉じる",
        Key::NotifClearAllButton => "すべて消去",
        Key::NotifDaemonNameTakenBanner => "システム通知は別のサービスが処理しているため、アプリの通知はここには表示されません",
        Key::NotifDaemonFailedBanner => "アプリの通知を受信できません：{}",
        Key::NotifDaemonUnsupportedBanner => "このプラットフォームではアプリの通知に対応していません",
        Key::NotifMergedCount => "他 {} 件",
        Key::NotifAgeJustNow => "たった今",
        Key::NotifAgeMinutes => "{} 分前",
        Key::NotifAgeHours => "{} 時間前",
        Key::NotifAgeDays => "{} 日前",
        Key::NotifTaskSectionLabel => "進行中のタスク",

        Key::LockAwaySummaryTitle => "離席していた時間：{}",
        Key::LockPendingCountLabel => "{} 件があなたの判断を待っています",
        Key::LockUnlockHint => "任意のキーを押すかクリックしてパスワードを入力",
        Key::LockPasswordPlaceholder => "パスワードを入力してロック解除",
        Key::LockPasswordWrongError => "パスワードが正しくありません。もう一度お試しください",
        Key::LockOfflineError => "ローカルサービスが応答していません。しばらくしてから再試行してください。",
        Key::LockVerifyingLabel => "確認中…",
        Key::LockThrottledLabel => "試行回数が多すぎます。少し待ってから再試行してください。",

        Key::LockPowerRestart => "再起動",
        Key::LockPowerShutdown => "シャットダウン",
        Key::LockPowerConfirmRestart => "この端末を再起動しますか？",
        Key::LockPowerConfirmShutdown => "この端末をシャットダウンしますか？",
        Key::LockPowerSending => "送信中…",
        Key::LockPowerFailed => "ローカルサービスが応答しないため、指示は送信されていません。",
        Key::LockPowerUnsupported => "この端末はロック画面からこの操作を実行できません。",

        Key::PointerTitle => "ポインタ操作",
        Key::PointerSubtitle => "アクセシビリティ · ポインタの大きさと形状",
        Key::PointerEntryDesc => "ポインタの大きさと形状",
        Key::PointerSectionAccessibility => "アクセシビリティ",
        Key::PointerShapeLabel => "形状",
        Key::PointerSizeLabel => "大きさ",
        Key::PointerSourceSystem => "システム標準",
        Key::PointerSourceSystemDesc => "デフォルト",
        Key::PointerSourceBrand => "ドゥドゥの肉球",
        Key::PointerSourceBrandDesc => "矢印とつかむ形だけを差し替え、その他はシステムの形状のままです",
        Key::PointerSizeDefault => "デフォルト",
        Key::PointerSizeMedium => "中",
        Key::PointerSizeLarge => "大",
        Key::PointerSizeExtraLarge => "特大",
        Key::PointerSizeLargest => "最大",
        Key::PointerZoomHint => "ポインタの大きさはズームと組み合わせると、より見つけやすくなります。",
        Key::PointerLoading => "ポインタ設定を読み込んでいます…",
        Key::PointerUnavailable => "デスクトップサービスに接続できないため、この端末ではポインタを変更できません。",
        Key::PointerSizeUnsupported => "この端末のデスクトップサービスはまだポインタの大きさを変更できません。",
        Key::PointerApplyFailed => "設定を適用できませんでした。しばらくしてから再試行してください。",
        Key::PointerThemeMissing => "選択したポインタの画像がこの端末にインストールされていないため、システムのポインタを表示しています。",
        Key::PointerEnvPinned => "この端末のポインタ設定は起動時に固定されているため、ここでの選択は次回起動時には反映されません。",

        Key::LauncherSearchPlaceholder => "入力して検索…",
        Key::LauncherNoAppResults => "該当するアプリがありません",
        Key::LauncherAppsScanning => "この端末のアプリを探しています…",
        Key::LauncherAppsNoneInstalled => "この端末にはまだ開けるアプリがありません",
        Key::LauncherAppsReadFailed => "この端末のアプリ一覧を読み取れませんでした",
        Key::LauncherSectionInstallable => "インストール可能",
        Key::LauncherInstallButton => "インストール",
        Key::InstallGateTitle => "このアプリをインストールしますか？",
        Key::InstallGateNotice => "インストールはファイルをダウンロードし、この端末の内容を変更します。確定するまで何も実行されません。",
        Key::InstallGateSourceLabel => "提供元",
        Key::InstallGateSizeLabel => "ダウンロードサイズ",
        Key::InstallGateLocationLabel => "インストール先",
        Key::InstallGateSizeProbing => "確認中…",
        Key::InstallGateSizeUnknown => "取得できません",
        Key::InstallGateConfirmButton => "インストール開始",
        Key::InstallGateInstallingLabel => "開始しています…",
        Key::InstallGateStartedLabel => "インストールを開始しました。完了するとアプリ一覧に表示されます",
        Key::InstallGateFailedLabel => "インストールを開始できませんでした",
        Key::VerifiedTierVerified => "検証済み",
        Key::VerifiedTierWorks => "動作確認済み",
        Key::VerifiedTierPartial => "一部対応",
        Key::VerifiedTierUnsupported => "非対応",
        Key::VerifiedTierUnrated => "未評価",

        Key::LauncherDelegateNoAgent => "委任できる AI スタッフが見つかりません。ダッシュボードで作成してください。",
        Key::LauncherDelegateSubmitFailed => "委任を送信できませんでした。しばらくしてからもう一度お試しください。",
        Key::LauncherDelegateSubmitFailedTitle => "委任を送信できませんでした",

        Key::TaskResultDoneSummary => "「{}」が完了しました",
        Key::TaskResultFailedSummary => "「{}」が失敗しました",
        Key::TaskResultNeedsHumanSummary => "「{}」の確認が必要です",
        Key::TaskResultNoDetail => "要約は添付されていません。",
        Key::TaskResultRetryButton => "再試行",
        Key::TaskResultAbortButton => "中止",
        Key::TaskResultDecideFailedTitle => "決定を送信できませんでした",
        Key::TaskResultDecideFailed => "送信できませんでした。しばらくしてからもう一度お試しください。",
    }
}

#[cfg(test)]
mod tests;
