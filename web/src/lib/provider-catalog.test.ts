import { describe, it, expect } from 'vitest';
import { PROVIDER_CATALOG, DEFAULT_PROVIDER_ID, findProviderEntry, providerDisplayName } from './provider-catalog';

describe('provider-catalog (WP-A)', () => {
  it('every entry has a non-empty id, label, and keyUrl', () => {
    for (const p of PROVIDER_CATALOG) {
      expect(p.id.length).toBeGreaterThan(0);
      expect(p.label.length).toBeGreaterThan(0);
      expect(p.keyUrl.startsWith('https://')).toBe(true);
    }
  });

  it('ids are unique', () => {
    const ids = PROVIDER_CATALOG.map((p) => p.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  // The gateway's `select_for_provider`/`direct_api_route` route Gemini
  // traffic through "gemini" only — "google" would validate on `accounts.add`
  // (it's in KNOWN_PROVIDER_IDS) but never be selected. Must not appear here.
  it('does not list the "google" alias (a routing dead end)', () => {
    expect(PROVIDER_CATALOG.some((p) => p.id === 'google')).toBe(false);
  });

  it('DEFAULT_PROVIDER_ID is anthropic and matches the gateway default', () => {
    expect(DEFAULT_PROVIDER_ID).toBe('anthropic');
    expect(findProviderEntry(DEFAULT_PROVIDER_ID)).toBeDefined();
  });

  it('findProviderEntry finds a known id and misses an unknown one', () => {
    expect(findProviderEntry('openai')?.label).toBe('OpenAI (GPT)');
    expect(findProviderEntry('not-a-real-provider')).toBeUndefined();
    expect(findProviderEntry(undefined)).toBeUndefined();
  });

  it('providerDisplayName falls back to anthropic when absent, and to the raw id when unknown', () => {
    expect(providerDisplayName(null)).toBe('Claude (Anthropic)');
    expect(providerDisplayName(undefined)).toBe('Claude (Anthropic)');
    expect(providerDisplayName('openai')).toBe('OpenAI (GPT)');
    expect(providerDisplayName('some-future-provider')).toBe('some-future-provider');
  });
});
