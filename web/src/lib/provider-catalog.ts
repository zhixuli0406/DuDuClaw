/**
 * WP-A (TODO-ai-runtimes-2026-09.md §3 WP-A) — dashboard-side metadata for
 * every LLM provider `accounts.add` accepts.
 *
 * The canonical list of accepted ids lives server-side
 * (`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`); this module is NOT
 * generated from it (no build-time bridge from Rust consts to TS exists in
 * this workspace), so `PROVIDER_CATALOG`'s `id` column below must be kept in
 * sync by hand. A gap here is not silent, though — `AddAccountDialog` sends
 * whatever id the user picked, and the gateway rejects anything not in its
 * own list with a clear error a future maintainer will trip over quickly.
 *
 * `"google"` is intentionally NOT listed: it is a `provider_env_key_names`
 * env-var-alias for `"gemini"` (both resolve `GEMINI_API_KEY`/`GOOGLE_API_KEY`),
 * but `select_for_provider`/`direct_api_route` on the gateway always route
 * Gemini traffic through the id `"gemini"` — an account saved with
 * `provider = "google"` would validate fine but then never be selected by
 * anything, a confusing dead end. Exposing only the id that is actually
 * live keeps the picker from setting that trap.
 */
export interface ProviderCatalogEntry {
  /** Must be one of `duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`. */
  readonly id: string;
  /** Product name — proper noun, shown verbatim (not translated). */
  readonly label: string;
  /** Example key shape, when the vendor's format is well-known/stable
   *  enough to state as fact. `null` ⇒ render the generic i18n placeholder
   *  instead of guessing a prefix that might be wrong. */
  readonly keyPlaceholder: string | null;
  /** Vendor's own API-key management page, opened in a new tab from the
   *  "get a key" link next to the key field. */
  readonly keyUrl: string;
}

export const PROVIDER_CATALOG: readonly ProviderCatalogEntry[] = [
  {
    id: 'anthropic',
    label: 'Claude (Anthropic)',
    keyPlaceholder: 'sk-ant-...',
    keyUrl: 'https://console.anthropic.com/settings/keys',
  },
  {
    id: 'openai',
    label: 'OpenAI (GPT)',
    keyPlaceholder: 'sk-...',
    keyUrl: 'https://platform.openai.com/api-keys',
  },
  {
    id: 'gemini',
    label: 'Google Gemini',
    keyPlaceholder: 'AIza...',
    keyUrl: 'https://aistudio.google.com/apikey',
  },
  {
    id: 'deepseek',
    label: 'DeepSeek',
    keyPlaceholder: 'sk-...',
    keyUrl: 'https://platform.deepseek.com/api_keys',
  },
  {
    id: 'xai',
    label: 'xAI (Grok)',
    keyPlaceholder: 'xai-...',
    keyUrl: 'https://console.x.ai/team/default/api-keys',
  },
  {
    id: 'groq',
    label: 'Groq',
    keyPlaceholder: 'gsk_...',
    keyUrl: 'https://console.groq.com/keys',
  },
  {
    id: 'openrouter',
    label: 'OpenRouter',
    keyPlaceholder: 'sk-or-...',
    keyUrl: 'https://openrouter.ai/settings/keys',
  },
  {
    id: 'mistral',
    label: 'Mistral',
    keyPlaceholder: null,
    keyUrl: 'https://console.mistral.ai/api-keys/',
  },
  {
    id: 'together',
    label: 'Together AI',
    keyPlaceholder: null,
    keyUrl: 'https://api.together.ai/settings/api-keys',
  },
  {
    id: 'minimax',
    label: 'MiniMax',
    keyPlaceholder: null,
    keyUrl: 'https://platform.minimax.io',
  },
  {
    id: 'qwen',
    label: 'Qwen (DashScope)',
    keyPlaceholder: null,
    keyUrl: 'https://dashscope-intl.console.aliyun.com/apiKey',
  },
];

/** The default provider `AddAccountDialog` pre-selects — matches the
 *  gateway's own `accounts.add` default so an unmodified form round-trips
 *  identically to the pre-WP-A "no provider param at all" behavior. */
export const DEFAULT_PROVIDER_ID = 'anthropic';

export function findProviderEntry(id: string | undefined | null): ProviderCatalogEntry | undefined {
  return PROVIDER_CATALOG.find((p) => p.id === id);
}

/** Display label for an account's `provider` field — falls back to the raw
 *  id (rather than hiding it) for a provider this catalogue doesn't know
 *  about yet, e.g. one added directly through the RPC before the dashboard
 *  catalogue caught up. */
export function providerDisplayName(id: string | undefined | null): string {
  if (!id) return PROVIDER_CATALOG[0].label; // absent ⇒ pre-WP-A default (anthropic)
  return findProviderEntry(id)?.label ?? id;
}
