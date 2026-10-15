//! Subscription-login risk notice — one wording for every terminal and log
//! surface (`duduclaw onboard`, `duduclaw doctor`, the account rotator's
//! host-login adoption WARN and the gateway's refusal message).
//!
//! [`vendor_policy`] picks the notice and the terms URL for the vendor of a
//! login (Anthropic, OpenAI, Google Gemini CLI / Antigravity, xAI, GitHub),
//! so a Codex or Gemini refusal never quotes Anthropic's policy. Every
//! sentence is backed by the vendor page named next to its URL constant; where
//! DuDuClaw found no published position, the notice says so.
//!
//! The dashboard carries its own localized copy (`subscriptionRisk.*` in
//! `web/src/i18n/*.json`); keep the two saying the same thing.
//!
//! Source (fetched 2026-10-16): Anthropic, "Legal and compliance" for
//! Claude Code, section "Authentication and credential use". It says
//! third-party developers may not offer Claude.ai login in their own
//! applications or route requests through Free/Pro/Max plan credentials on
//! behalf of their users, may not collect, store or intermediate Claude.ai
//! credentials or session tokens, and that Anthropic may enforce this without
//! prior notice. The notice deliberately says "not permitted by policy", never
//! "blocked": `claude -p` with an OAuth token still works technically
//! (CHANGELOG v1.66.0).

/// Anthropic's Claude Code legal-and-compliance page (the primary source).
pub const ANTHROPIC_LEGAL_URL: &str = "https://code.claude.com/docs/en/legal-and-compliance";

/// Google's Gemini CLI "Terms of Service and Privacy Notice" page
/// (geminicli.com, published from `google-gemini/gemini-cli`
/// `docs/resources/tos-privacy.md`; fetched 2026-10-16). It says directly
/// accessing the services powering Gemini CLI with third-party software,
/// tools or services through Gemini CLI OAuth violates the applicable terms
/// and may be grounds for suspension or termination of the account.
pub const GEMINI_CLI_TERMS_URL: &str = "https://geminicli.com/docs/resources/tos-privacy/";

/// Google Antigravity terms (fetched 2026-10-16). Section 6: using third
/// party software, tools or services to access the Service (e.g. through
/// Antigravity OAuth) is a breach, and may be grounds for suspension or
/// termination of the Antigravity and/or Gemini CLI accounts.
pub const ANTIGRAVITY_TERMS_URL: &str = "https://antigravity.google/terms";

/// OpenAI Terms of Use. The page answers 403 to scripted clients, so its text
/// was not fetched here; the notice only says the terms apply and that OpenAI's
/// position on third-party products driving a ChatGPT login is unclear.
pub const OPENAI_TERMS_URL: &str = "https://openai.com/policies/terms-of-use/";

/// xAI consumer Terms of Service (covers Grok subscriptions). Same 403 caveat
/// as [`OPENAI_TERMS_URL`]; the notice makes no claim about its content.
pub const XAI_TERMS_URL: &str = "https://x.ai/legal/terms-of-service";

/// GitHub terms for additional products (has a GitHub Copilot section).
pub const GITHUB_COPILOT_TERMS_URL: &str =
    "https://docs.github.com/en/site-policy/github-terms/github-terms-for-additional-products-and-features";

/// One-line zh-TW notice for terminal output and logs. Pair it with
/// [`ANTHROPIC_LEGAL_URL`].
pub const SUBSCRIPTION_RISK_NOTICE_ZH: &str = "Anthropic 的規範不允許第三方產品經手 Claude 訂閱帳號的憑證，\
     或用訂閱額度替其他人轉送請求，違反時可能不經通知就被限制；要長期穩定使用，請改用 Claude Console 的 API 金鑰。";

/// The notice and the source link on one line.
pub fn subscription_risk_line_zh() -> String {
    format!("{SUBSCRIPTION_RISK_NOTICE_ZH} 詳情：{ANTHROPIC_LEGAL_URL}")
}

/// The vendor terms that apply to one subscription login: a zh-TW notice for
/// the gateway refusal and terminal output, and the vendor's own terms page
/// (`None` when DuDuClaw has no page to point at).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VendorPolicy {
    /// Stable vendor key (`anthropic`, `openai`, `google-gemini-cli`,
    /// `google-antigravity`, `xai`, `github`, `other`).
    pub vendor: &'static str,
    pub notice_zh: &'static str,
    pub url: Option<&'static str>,
}

impl VendorPolicy {
    /// Notice plus link on one line.
    pub fn line_zh(&self) -> String {
        match self.url {
            Some(url) => format!("{} 詳情：{url}", self.notice_zh),
            None => self.notice_zh.to_string(),
        }
    }
}

const ANTHROPIC_POLICY: VendorPolicy = VendorPolicy {
    vendor: "anthropic",
    notice_zh: SUBSCRIPTION_RISK_NOTICE_ZH,
    url: Some(ANTHROPIC_LEGAL_URL),
};

/// Vendor terms for a runtime id (`claude`, `codex`, `gemini`, `antigravity`,
/// `grok`) or an `[[accounts]] provider` value (`anthropic`, `openai`,
/// `gemini`/`google`, `xai`, `github`). An empty id means Anthropic (the
/// default provider of an `[[accounts]]` entry). Anything else gets a
/// vendor-neutral notice with no link: DuDuClaw does not claim to know that
/// vendor's terms.
pub fn vendor_policy(id: &str) -> VendorPolicy {
    match id.trim().to_ascii_lowercase().as_str() {
        "" | "claude" | "anthropic" => ANTHROPIC_POLICY,
        "codex" | "openai" => VendorPolicy {
            vendor: "openai",
            notice_zh: "OpenAI 對第三方產品代為操作 ChatGPT 訂閱登入（Codex）的立場不明；\
                 訂閱帳號仍受 OpenAI 使用條款約束，帳號被限制的風險由你自行承擔。要長期穩定使用，請改用 OpenAI API 金鑰。",
            url: Some(OPENAI_TERMS_URL),
        },
        "gemini" | "google" => VendorPolicy {
            vendor: "google-gemini-cli",
            notice_zh: "Google 的 Gemini CLI 條款寫明：用第三方軟體、工具或服務透過 Gemini CLI 的 OAuth \
                 直接存取其背後的服務屬於違規，可能導致帳號被停權或終止。DuDuClaw 執行的是原版 Gemini CLI，\
                 但是否落在這條範圍內由 Google 判斷。要長期穩定使用，請改用 Gemini API 金鑰。",
            url: Some(GEMINI_CLI_TERMS_URL),
        },
        "antigravity" | "agy" => VendorPolicy {
            vendor: "google-antigravity",
            notice_zh: "Google 的 Antigravity 條款寫明：用第三方軟體、工具或服務存取該服務（例如透過 Antigravity OAuth）\
                 屬於違約，可能導致 Antigravity 和 Gemini CLI 帳號被停權或終止。DuDuClaw 執行的是原版 agy，\
                 但是否落在這條範圍內由 Google 判斷。遠端或容器部署請改用 API 金鑰模式。",
            url: Some(ANTIGRAVITY_TERMS_URL),
        },
        "grok" | "xai" => VendorPolicy {
            vendor: "xai",
            notice_zh: "xAI 對第三方產品代為操作 SuperGrok／X Premium+ 訂閱的立場不明；\
                 訂閱帳號仍受 xAI 服務條款約束，帳號被限制的風險由你自行承擔。",
            url: Some(XAI_TERMS_URL),
        },
        "github" | "copilot" => VendorPolicy {
            vendor: "github",
            notice_zh: "這個登入會把 GitHub Copilot 的訂閱憑證存進 DuDuClaw，並用 Copilot 編輯器整合使用的公開 OAuth client id \
                 （不是 GitHub 發給 DuDuClaw 的）換取 Copilot token；用量計在你的 Copilot 訂閱，受 GitHub 條款約束，\
                 帳號被限制的風險由你自行承擔。",
            url: Some(GITHUB_COPILOT_TERMS_URL),
        },
        _ => VendorPolicy {
            vendor: "other",
            notice_zh: "這會讓 DuDuClaw 保存並使用這家供應商的訂閱憑證。各家條款可能限制第三方產品使用訂閱帳號，\
                 帳號被限制的風險由你自行承擔；請先閱讀該供應商的條款，能用 API 金鑰就優先用 API 金鑰。",
            url: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_carries_the_primary_source_link() {
        let line = subscription_risk_line_zh();
        assert!(line.contains(ANTHROPIC_LEGAL_URL));
        assert!(line.starts_with(SUBSCRIPTION_RISK_NOTICE_ZH));
    }

    #[test]
    fn every_login_runtime_gets_its_own_vendor_terms() {
        assert_eq!(vendor_policy("claude").url, Some(ANTHROPIC_LEGAL_URL));
        assert_eq!(vendor_policy("").vendor, "anthropic");
        assert_eq!(vendor_policy("codex").url, Some(OPENAI_TERMS_URL));
        assert_eq!(vendor_policy("openai").vendor, "openai");
        assert_eq!(vendor_policy("gemini").url, Some(GEMINI_CLI_TERMS_URL));
        assert_eq!(vendor_policy("antigravity").url, Some(ANTIGRAVITY_TERMS_URL));
        assert_eq!(vendor_policy("grok").url, Some(XAI_TERMS_URL));
        assert_eq!(vendor_policy("xai").vendor, "xai");
        assert_eq!(vendor_policy("github").url, Some(GITHUB_COPILOT_TERMS_URL));
        let other = vendor_policy("deepseek");
        assert_eq!(other.vendor, "other");
        assert_eq!(other.url, None);
        assert_eq!(other.line_zh(), other.notice_zh);
        // Non-Anthropic notices never borrow Anthropic's wording or link.
        for id in ["codex", "gemini", "antigravity", "grok", "github", "deepseek"] {
            let p = vendor_policy(id);
            assert!(!p.notice_zh.contains("Anthropic"), "{id}: {}", p.notice_zh);
            assert!(!p.line_zh().contains(ANTHROPIC_LEGAL_URL), "{id}");
        }
    }

    /// Only claims with a cited vendor page: no dated "blocked since" story.
    #[test]
    fn google_notices_cite_terms_not_an_unsourced_block() {
        for id in ["gemini", "antigravity"] {
            let n = vendor_policy(id).notice_zh;
            for banned in ["封鎖", "2026-03", "伺服器端"] {
                assert!(!n.contains(banned), "{id} notice must not say {banned:?}");
            }
            assert!(n.contains("條款"), "{id}: {n}");
        }
    }

    /// The notice states policy, not a technical block (owner decision
    /// 2026-10-16: OAuth `claude -p` still works, so "blocked" would be false).
    #[test]
    fn notice_never_claims_a_technical_block() {
        for banned in ["封鎖", "blocked", "伺服器端"] {
            assert!(
                !SUBSCRIPTION_RISK_NOTICE_ZH.contains(banned),
                "notice must not say {banned:?}"
            );
        }
    }
}
