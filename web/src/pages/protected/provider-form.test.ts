import { describe, expect, it } from 'vitest';
import { PROVIDER_PROTOCOLS, validProviders } from './provider-form';
const provider = { name: 'Example', base_url: 'https://api.example.com/v1', api_key: 'key' };

describe('model provider form validation', () => {
  it('offers exactly the four server serde protocols', () => {
    expect(PROVIDER_PROTOCOLS).toEqual(['chat_completions', 'responses', 'anthropic', 'gemini']);
  });
  it('accepts HTTPS paths and Unicode names, and bounds rows to 1–32', () => {
    expect(validProviders([{ ...provider, name: '模型服務' }])).toBe(true);
    expect(validProviders([])).toBe(false);
    expect(validProviders(Array.from({ length: 32 }, (_, i) => ({ ...provider, name: `provider-${i}` })))).toBe(true);
    expect(validProviders(Array.from({ length: 33 }, (_, i) => ({ ...provider, name: `provider-${i}` })))).toBe(false);
  });
  it.each(['http://example.com', 'https://user:pw@example.com', 'https://example.com?', 'https://example.com#',
    'https://example.com?q=secret', 'https://example.com/#fragment', 'invalid', 'https://example.com/\\evil'])('refuses unsafe endpoint %s', (base_url) => {
    expect(validProviders([{ ...provider, base_url }])).toBe(false);
  });
  it.each(['', ' ', 'x'.repeat(129), 'with"quote', 'with\\slash', 'with\nnewline', 'with\u007fcontrol'])('refuses an invalid name without echoing it', (name) => {
    expect(validProviders([{ ...provider, name }])).toBe(false);
  });
  it('refuses duplicate names and empty or unescapable keys', () => {
    expect(validProviders([provider, { ...provider }])).toBe(false);
    for (const api_key of ['', ' ', 'key"quote', 'key\\slash', 'key\u0000control']) {
      expect(validProviders([{ ...provider, api_key }])).toBe(false);
    }
  });
});
