//! Subscription-login risk notice — one wording for every terminal and log
//! surface (`duduclaw onboard`, `duduclaw doctor`, the account rotator's
//! host-login adoption WARN and the gateway's refusal message).
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

/// One-line zh-TW notice for terminal output and logs. Pair it with
/// [`ANTHROPIC_LEGAL_URL`].
pub const SUBSCRIPTION_RISK_NOTICE_ZH: &str = "Anthropic 的規範不允許第三方產品經手 Claude 訂閱帳號的憑證，\
     或用訂閱額度替其他人轉送請求，違反時可能不經通知就被限制；要長期穩定使用，請改用 Claude Console 的 API 金鑰。";

/// The notice and the source link on one line.
pub fn subscription_risk_line_zh() -> String {
    format!("{SUBSCRIPTION_RISK_NOTICE_ZH} 詳情：{ANTHROPIC_LEGAL_URL}")
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
