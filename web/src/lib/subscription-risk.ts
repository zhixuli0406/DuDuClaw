/**
 * Subscription-login risk gate — shared constants for the dashboard.
 *
 * The gateway refuses `auth.cli_login.start`, `accounts.setup_token_start`,
 * `accounts.add` (type `oauth`) and a `config.raw.set` that adds an oauth
 * account unless the request carries
 * `accept_subscription_risk: true`
 * (`crates/duduclaw-gateway/src/subscription_risk_gate.rs`). The dashboard
 * sends it only after the user ticks `SubscriptionRiskDisclosure`.
 */

/** Anthropic's Claude Code legal-and-compliance page, section "Authentication
 *  and credential use". Mirrors `duduclaw_core::subscription_risk::ANTHROPIC_LEGAL_URL`. */
export const ANTHROPIC_LEGAL_URL = 'https://code.claude.com/docs/en/legal-and-compliance';

/** Vendor whose terms apply to a subscription login. Mirrors the `vendor`
 *  keys of `duduclaw_core::subscription_risk::vendor_policy`. */
export type SubscriptionVendor = 'anthropic' | 'openai' | 'gemini' | 'xai' | 'other';

/** Each vendor's own terms page (`null`: no page DuDuClaw can point at).
 *  Mirrors the `*_URL` constants in `duduclaw_core::subscription_risk`. */
export const VENDOR_TERMS_URL: Record<SubscriptionVendor, string | null> = {
  anthropic: ANTHROPIC_LEGAL_URL,
  // Sign in with ChatGPT terms (2026-09-29): the program that governs ChatGPT
  // plan use in third-party apps. Mirrors `OPENAI_SIWC_TERMS_URL`.
  openai: 'https://openai.com/policies/sign-in-with-chatgpt-terms/',
  gemini: 'https://geminicli.com/docs/resources/tos-privacy/',
  xai: 'https://x.ai/legal/terms-of-service',
  other: null,
};

/** Vendor for a CLI runtime id (`claude`, `codex`, `gemini`,
 *  `grok`) or an `[[accounts]]` provider id. Empty = Anthropic (the default
 *  provider of an account). */
export function subscriptionVendorFor(id: string | null | undefined): SubscriptionVendor {
  switch ((id ?? '').trim().toLowerCase()) {
    case '':
    case 'claude':
    case 'anthropic':
      return 'anthropic';
    case 'codex':
    case 'openai':
      return 'openai';
    case 'gemini':
    case 'google':
      return 'gemini';
    case 'grok':
    case 'xai':
      return 'xai';
    default:
      return 'other';
  }
}

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
