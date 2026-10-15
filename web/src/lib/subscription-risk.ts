/**
 * Subscription-login risk gate — shared constants for the dashboard.
 *
 * The gateway refuses `auth.cli_login.start`, `accounts.setup_token_start`
 * and `accounts.add` (type `oauth`) unless the request carries
 * `accept_subscription_risk: true`
 * (`crates/duduclaw-gateway/src/subscription_risk_gate.rs`). The dashboard
 * sends it only after the user ticks `SubscriptionRiskDisclosure`.
 */

/** Anthropic's Claude Code legal-and-compliance page, section "Authentication
 *  and credential use". Mirrors `duduclaw_core::subscription_risk::ANTHROPIC_LEGAL_URL`. */
export const ANTHROPIC_LEGAL_URL = 'https://code.claude.com/docs/en/legal-and-compliance';

/** Mirrors `subscription_risk_gate::ACCEPT_PARAM`. */
export const ACCEPT_SUBSCRIPTION_RISK_PARAM = 'accept_subscription_risk';

/** Mirrors `subscription_risk_gate::NOT_ACCEPTED_CODE`. */
export const SUBSCRIPTION_RISK_NOT_ACCEPTED = 'subscription_risk_not_accepted';

/** `true` when `err` is the gateway's subscription-risk refusal. */
export function isSubscriptionRiskRefusal(err: unknown): boolean {
  return (
    !!err &&
    typeof err === 'object' &&
    'code' in err &&
    (err as { code?: unknown }).code === SUBSCRIPTION_RISK_NOT_ACCEPTED
  );
}
