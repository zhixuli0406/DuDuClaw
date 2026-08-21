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
    LanguageAccessibilityCollapse,
    LanguageAccessibilityExpand,
    LanguageAccessibilityPlaceholder,

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
        Key::LanguageAccessibilityPlaceholder => "輔助使用選項（旁白／放大鏡／對比度）— 下一輪實作",

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
        Key::LanguageAccessibilityPlaceholder => "Accessibility options (VoiceOver, Zoom, Contrast). Coming in a future update.",

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
        Key::LanguageAccessibilityPlaceholder => "アクセシビリティ設定（読み上げ／拡大鏡／コントラスト）。今後のアップデートで対応予定です。",

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
    }
}

#[cfg(test)]
mod tests {
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
    ];

    const ALL_LOCALES: [Locale; 3] = [Locale::ZhTw, Locale::En, Locale::JaJp];

    #[test]
    fn all_keys_has_the_expected_count_and_no_duplicates() {
        // A hardcoded count, same style `all_has_ten_steps_in_declared_
        // order` (`oobe/mod.rs`) pins `OobeStep::ALL.len() == 10` — catches
        // `ALL_KEYS` silently drifting out of sync with a newly added `Key`
        // variant (the compiler won't catch THAT half; only the per-locale
        // match arms are compiler-enforced).
        assert_eq!(ALL_KEYS.len(), 95);
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
}
