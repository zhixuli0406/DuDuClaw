import type { ProtectedProviderInput } from '@/lib/api';

export const PROVIDER_PROTOCOLS = ['chat_completions', 'responses', 'anthropic', 'gemini'] as const;

/** Never use server messages: even availability copy belongs to this feature. */
export function providerErrorCode(error: unknown): string {
  const code = error && typeof error === 'object' && 'code' in error ? error.code : null;
  return typeof code === 'string' && /^[a-z_]{1,64}$/.test(code) ? code : 'unknown';
}

export function providerErrorKey(code: string): string {
  switch (code) {
    case 'busy': case 'unconfirmed': case 'not_allocated': case 'home_entry_unsafe':
    case 'storage_unavailable': case 'invalid_provider': case 'not_available':
      return `providers.error.${code}`;
    case 'unreachable': case 'service_unavailable': return 'providers.error.unreachable';
    case 'step_up_required': case 'session_expired': return 'providers.error.step_up_required';
    default: return 'providers.error.generic';
  }
}

const unsafe = /[\p{Cc}"\\]/u;
export function validProviders(providers: ProtectedProviderInput[]): boolean {
  if (providers.length < 1 || providers.length > 32) return false;
  const names = new Set<string>();
  return providers.every((provider) => {
    if (!provider.name.trim() || Array.from(provider.name).length > 128 || unsafe.test(provider.name)
      || !provider.api_key.trim() || unsafe.test(provider.api_key) || unsafe.test(provider.base_url)
      || names.has(provider.name)) return false;
    names.add(provider.name);
    try {
      const url = new URL(provider.base_url);
      return provider.base_url.startsWith('https://') && url.protocol === 'https:' && !!url.hostname
        && !url.username && !url.password
        && !provider.base_url.includes('?') && !provider.base_url.includes('#');
    } catch { return false; }
  });
}
