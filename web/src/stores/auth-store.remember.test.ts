import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useAuthStore } from './auth-store';
const user = { id: 'remember', email: 'remember@example.com', display_name: 'Remember', role: 'employee', status: 'active' };
const response = (value: unknown, status = 200) => ({ ok: status === 200, status, json: async () => value }) as Response;
beforeEach(() => {
  useAuthStore.setState({ refreshToken: null });
  sessionStorage.clear(); localStorage.clear();
  useAuthStore.getState().logout();
  vi.restoreAllMocks();
});
afterEach(() => {
  useAuthStore.setState({ refreshToken: null });
  sessionStorage.clear(); localStorage.clear();
  useAuthStore.getState().logout();
  vi.unstubAllGlobals();
});
it('exchanges a completed login for an HttpOnly session and persists only a marker', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce(response({ access_token: 'short-access', refresh_token: 'short-refresh' }))
    .mockResolvedValueOnce(response({ access_token: 'remember-access', remembered: true }))
    .mockResolvedValueOnce(response({ user, bindings: [] }));
  vi.stubGlobal('fetch', fetcher);
  await useAuthStore.getState().login(user.email, 'password', true);
  expect(fetcher.mock.calls[1][0]).toBe('/api/session/remember');
  expect(useAuthStore.getState().jwt).toBe('remember-access');
  expect(useAuthStore.getState().refreshToken).toBe('cookie');
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBeNull();
  expect(localStorage.getItem('duduclaw-refresh-token')).toBeNull();
  expect(localStorage.getItem('duduclaw-remembered')).toBe('1');
});
it('waits for MFA completion before creating a remembered session', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce(response({ mfa_required: true, challenge_id: 'challenge' }))
    .mockResolvedValueOnce(response({ access_token: 'remember-access', remembered: true }))
    .mockResolvedValueOnce(response({ user, bindings: [] }));
  vi.stubGlobal('fetch', fetcher);
  await useAuthStore.getState().login(user.email, 'password', true);
  expect(fetcher).toHaveBeenCalledTimes(1);
  expect(localStorage.getItem('duduclaw-remembered')).toBeNull();
  await useAuthStore.getState().establishSession({ access_token: 'mfa-access', refresh_token: 'mfa-refresh' });
  expect(fetcher.mock.calls[1][0]).toBe('/api/session/remember');
});
it('restores a remembered session using cookies without a JavaScript credential', async () => {
  localStorage.setItem('duduclaw-remembered', '1');
  useAuthStore.setState({ initialized: false });
  const fetcher = vi.fn().mockResolvedValueOnce(response({ access_token: 'remember-access', remembered: true }))
    .mockResolvedValueOnce(response({ user, bindings: [] }));
  vi.stubGlobal('fetch', fetcher);
  expect(await useAuthStore.getState().loadFromStorage()).toBe(true);
  expect(fetcher.mock.calls[0][0]).toBe('/api/session/refresh');
  expect(fetcher.mock.calls[0][1]).toEqual(expect.objectContaining({ credentials: 'same-origin', body: '{}', headers: expect.objectContaining({ 'X-DuDuClaw-CSRF': '1' }) }));
});
it('revokes the cookie session on explicit logout', async () => {
  localStorage.setItem('duduclaw-remembered', '1');
  useAuthStore.setState({ refreshToken: 'cookie' });
  const fetcher = vi.fn().mockResolvedValue(response({ ok: true }));
  vi.stubGlobal('fetch', fetcher);
  useAuthStore.getState().logout();
  await vi.waitFor(() => expect(fetcher).toHaveBeenCalledWith('/api/session/logout', expect.objectContaining({ method: 'POST', credentials: 'same-origin', headers: expect.objectContaining({ 'X-DuDuClaw-CSRF': '1' }) })));
  expect(localStorage.getItem('duduclaw-remembered')).toBeNull();
});
