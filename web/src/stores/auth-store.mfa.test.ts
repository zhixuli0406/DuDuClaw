import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { useAuthStore } from './auth-store';
import { mfaApi } from '@/lib/mfa-api';

const user = { id: 'u', email: 'u@example.com', display_name: 'U', role: 'employee' as const, status: 'active' };
const response = (body: unknown) => ({ ok: true, status: 200, json: async () => body }) as Response;
beforeEach(() => { useAuthStore.getState().logout(); vi.restoreAllMocks(); sessionStorage.clear(); localStorage.clear(); });
afterEach(() => { useAuthStore.getState().logout(); vi.unstubAllGlobals(); });

it.each(['password', 'otp'])('preserves legacy unenrolled %s login', async mode => {
  const fetcher = vi.fn().mockResolvedValueOnce(response({ access_token: 'jwt', refresh_token: 'refresh', user }))
    .mockResolvedValueOnce(response({ user, bindings: [] }));
  vi.stubGlobal('fetch', fetcher);
  const result = mode === 'password' ? await useAuthStore.getState().login(user.email, 'pw') : await useAuthStore.getState().otpVerify('otp', '123456');
  expect(result).toBeUndefined(); expect(useAuthStore.getState().isAuthenticated).toBe(true);
  expect(fetcher).toHaveBeenCalledTimes(2); expect(sessionStorage.getItem('duduclaw-refresh-token')).toBe('refresh');
});
it.each([
  { mfa_required: true, challenge_id: 'challenge' },
  { mfa_enrollment_required: true, enrollment_token: 'enroll', challenge_ref: 'reference' },
])('returns challenge without storing it or authenticating: %j', async challenge => {
  const storage = vi.spyOn(Storage.prototype, 'setItem');
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response(challenge)));
  expect(await useAuthStore.getState().login(user.email, 'pw')).toEqual(challenge);
  expect(useAuthStore.getState().isAuthenticated).toBe(false); expect(storage).not.toHaveBeenCalled();
  expect(JSON.stringify(useAuthStore.getState())).not.toContain('challenge');
  expect(await useAuthStore.getState().otpVerify('otp', '123456')).toEqual(challenge);
  expect(storage).not.toHaveBeenCalled();
});
it('never persists unexpected secret/recovery fields in the final session response', async () => {
  const data = { access_token: 'jwt', refresh_token: 'refresh', secret: 'SECRET', recovery_codes: ['RECOVERY'] };
  vi.stubGlobal('fetch', vi.fn().mockResolvedValue(response({ user, bindings: [] })));
  const writes = vi.spyOn(Storage.prototype, 'setItem');
  await useAuthStore.getState().establishSession(data);
  expect(writes.mock.calls).toEqual([['duduclaw-refresh-token', 'refresh']]);
  expect(JSON.stringify(useAuthStore.getState())).not.toContain('SECRET');
  expect(JSON.stringify(useAuthStore.getState())).not.toContain('RECOVERY');
});
it('sends exactly one factor field and disables HTTP caching', async () => {
  const fetcher = vi.fn().mockResolvedValue(response({ access_token: 'jwt', refresh_token: 'refresh' }));
  vi.stubGlobal('fetch', fetcher);
  await mfaApi.verify('challenge', 'RECOVERY', true);
  expect(fetcher).toHaveBeenCalledWith('/api/mfa/verify', expect.objectContaining({ body: JSON.stringify({ challenge_id: 'challenge', recovery_code: 'RECOVERY' }), cache: 'no-store' }));
});

it.each(['rejected', 'success'])('ignores a stale refresh %s after the MFA token handover', async outcome => {
  useAuthStore.setState({ user, jwt: 'old-access', refreshToken: 'old-refresh', isAuthenticated: true, initialized: true });
  let finish!: (value: Response) => void;
  const pending = new Promise<Response>(resolve => { finish = resolve; });
  const fetcher = vi.fn().mockReturnValueOnce(pending);
  vi.stubGlobal('fetch', fetcher);
  const refresh = useAuthStore.getState().refresh();
  useAuthStore.getState().replaceSessionTokens({ access_token: 'new-access', refresh_token: 'new-refresh' }, 'old-refresh');
  finish(outcome === 'success' ? response({ access_token: 'stale-access' }) :
    { ok: false, status: 401, json: async () => ({ error: 'revoked' }) } as Response);
  await refresh;
  expect(useAuthStore.getState()).toMatchObject({ jwt: 'new-access', refreshToken: 'new-refresh', isAuthenticated: true, user });
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBe('new-refresh');
  expect(fetcher).toHaveBeenCalledTimes(1);
});

it('keeps the MFA session in memory when browser storage refuses writes', () => {
  useAuthStore.setState({ user, jwt: 'old-access', refreshToken: 'old-refresh', isAuthenticated: true, initialized: true });
  vi.spyOn(Storage.prototype, 'setItem').mockImplementation(() => { throw new Error('storage disabled'); });
  expect(() => useAuthStore.getState().replaceSessionTokens({ access_token: 'new-access', refresh_token: 'new-refresh' }, 'old-refresh')).not.toThrow();
  expect(useAuthStore.getState()).toMatchObject({ jwt: 'new-access', refreshToken: 'new-refresh', isAuthenticated: true });
});


it.each([false, true])('ignores an MFA response after leaving its session (new login=%s)', newLogin => {
  useAuthStore.setState({ user: newLogin ? user : null, jwt: newLogin ? 'other-access' : null,
    refreshToken: newLogin ? 'other-refresh' : null, isAuthenticated: newLogin, initialized: true });
  useAuthStore.getState().replaceSessionTokens({ access_token: 'departed-access', refresh_token: 'departed-refresh' }, 'old-refresh');
  expect(useAuthStore.getState().jwt).toBe(newLogin ? 'other-access' : null);
  expect(useAuthStore.getState().refreshToken).toBe(newLogin ? 'other-refresh' : null);
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBeNull();
});

it.each(['logout', 'new login'])('ignores MFA completion while /api/me is pending after %s', async action => {
  let finish!: (value: Response) => void;
  const pending = new Promise<Response>(resolve => { finish = resolve; });
  const fetcher = vi.fn().mockReturnValueOnce(pending)
    .mockResolvedValueOnce(response({ access_token: 'other-access', refresh_token: 'other-refresh' }))
    .mockResolvedValueOnce(response({ user: { ...user, id: 'other' }, bindings: [] }));
  vi.stubGlobal('fetch', fetcher);
  const old = useAuthStore.getState().establishSession({ access_token: 'old-access', refresh_token: 'old-refresh' });
  if (action === 'logout') useAuthStore.getState().logout();
  else await useAuthStore.getState().login('other@example.com', 'pw');
  const writes = vi.spyOn(Storage.prototype, 'setItem');
  finish(response({ user, bindings: [] }));
  expect(await old).toBe(false);
  expect(writes).not.toHaveBeenCalled();
  expect(useAuthStore.getState().jwt).toBe(action === 'logout' ? null : 'other-access');
  expect(sessionStorage.getItem('duduclaw-refresh-token')).toBe(action === 'logout' ? null : 'other-refresh');
});

it('rejects an MFA attempt invalidated before session establishment starts', async () => {
  const attempt = useAuthStore.getState().authAttempt;
  useAuthStore.getState().logout();
  const fetcher = vi.fn(); vi.stubGlobal('fetch', fetcher);
  expect(await useAuthStore.getState().establishSession({ access_token: 'old', refresh_token: 'old' }, attempt)).toBe(false);
  expect(fetcher).not.toHaveBeenCalled();
});

it('ignores session establishment after its MFA page unmounts during /api/me', async () => {
  let finish!: (value: Response) => void;
  vi.stubGlobal('fetch', vi.fn().mockReturnValue(new Promise<Response>(resolve => { finish = resolve; })));
  let active = true;
  const pending = useAuthStore.getState().establishSession({ access_token: 'old', refresh_token: 'old' }, useAuthStore.getState().authAttempt, () => active);
  active = false;
  const writes = vi.spyOn(Storage.prototype, 'setItem');
  finish(response({ user, bindings: [] }));
  expect(await pending).toBe(false);
  expect(writes).not.toHaveBeenCalled();
  expect(useAuthStore.getState().isAuthenticated).toBe(false);
});
