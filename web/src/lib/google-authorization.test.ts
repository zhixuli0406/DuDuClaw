import { afterEach, expect, it, vi } from 'vitest';
import { useAuthStore } from '@/stores/auth-store';
import { googleAuthorization, googleAuthorizationReturn } from './google-authorization';
const ticket = 'a'.repeat(64);
afterEach(() => { vi.unstubAllGlobals(); useAuthStore.setState({ jwt: null } as never); });
it('accepts only the fixed same-origin landing route, never an open redirect', () => {
  expect(googleAuthorizationReturn(`/me/google/authorize#ticket=${ticket}`)).toBe(`/me/google/authorize#ticket=${ticket}`);
  for (const value of [`https://evil.test/me/google/authorize#ticket=${ticket}`, `//evil.test/#ticket=${ticket}`,
    `/me/google/authorize?redirect=https://evil.test#ticket=${ticket}`, `/me/google/authorize#ticket=${ticket}&user=bob`,
    '/me/google/authorize#ticket=short']) expect(googleAuthorizationReturn(value)).toBeUndefined();
});
it('previews with GET and a session bearer; the ticket is not in the URL', async () => {
  useAuthStore.setState({ jwt: 'session-jwt' } as never);
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ agent: 'bot' }) });
  vi.stubGlobal('fetch', fetch);
  await googleAuthorization.preview(ticket);
  expect(fetch).toHaveBeenCalledWith('/api/me/google/authorization-request', expect.objectContaining({
    method: 'GET', headers: { Authorization: 'Bearer session-jwt', 'X-Google-Authorization-Ticket': ticket },
    body: undefined, cache: 'no-store', referrerPolicy: 'no-referrer', credentials: 'omit',
  }));
});
it('only explicit confirmation POSTs both disclosures without trusting a member id', async () => {
  const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ completed: true }) });
  vi.stubGlobal('fetch', fetch);
  await googleAuthorization.confirm(ticket);
  expect(fetch).toHaveBeenCalledWith('/api/me/google/authorization-request', expect.objectContaining({ method: 'POST',
    body: JSON.stringify({ ticket, acknowledge_google_data: true, confirm_shared_access: true }) }));
});
it('preserves the gateway-provided sub-path in preview and confirmation requests', async () => {
  const meta = document.createElement('meta');
  meta.name = 'duduclaw-base'; meta.content = '/team'; document.head.append(meta);
  try {
    const fetch = vi.fn().mockResolvedValue({ ok: true, json: async () => ({}) });
    vi.stubGlobal('fetch', fetch);
    await googleAuthorization.preview(ticket);
    await googleAuthorization.confirm(ticket);
    expect(fetch.mock.calls.map(call => call[0])).toEqual([
      '/team/api/me/google/authorization-request', '/team/api/me/google/authorization-request',
    ]);
  } finally { meta.remove(); }
});
