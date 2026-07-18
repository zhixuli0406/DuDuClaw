import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore, isRetryableAuthError, ApiError } from './auth-store';

const STORAGE_KEY_REFRESH = 'duduclaw-refresh-token';

/** Build a minimal fetch Response stand-in. */
function res(status: number, body: unknown) {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: async () => body,
  } as unknown as Response;
}

const ME_OK = { user: { id: 'u1', email: 'a@b.c', display_name: 'A', role: 'admin', status: 'active' }, bindings: [] };

function resetStore() {
  useAuthStore.setState({
    user: null,
    jwt: null,
    refreshToken: null,
    isAuthenticated: false,
    initialized: false,
    bindings: [],
    loading: false,
  });
  sessionStorage.clear();
  localStorage.clear();
}

beforeEach(() => {
  vi.clearAllMocks();
  resetStore();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe('isRetryableAuthError (#1 — 401/403 vs transient)', () => {
  it('does NOT retry a rejected credential (401 / 403)', () => {
    expect(isRetryableAuthError(new ApiError(401, 'unauthorized'))).toBe(false);
    expect(isRetryableAuthError(new ApiError(403, 'forbidden'))).toBe(false);
  });

  it('retries transient failures (429 rate-limit, 5xx)', () => {
    expect(isRetryableAuthError(new ApiError(429, 'too many'))).toBe(true);
    expect(isRetryableAuthError(new ApiError(500, 'boom'))).toBe(true);
    expect(isRetryableAuthError(new ApiError(503, 'unavailable'))).toBe(true);
  });

  it('retries non-HTTP failures (network / parse)', () => {
    expect(isRetryableAuthError(new Error('network down'))).toBe(true);
    expect(isRetryableAuthError('weird')).toBe(true);
  });
});

describe('loadFromStorage (#1 — token lifecycle)', () => {
  it('clears the refresh token and stays logged out on 401', async () => {
    sessionStorage.setItem(STORAGE_KEY_REFRESH, 'rt-1');
    const fetchMock = vi.fn(async () => res(401, { error: 'bad token' }));
    vi.stubGlobal('fetch', fetchMock);

    const ok = await useAuthStore.getState().loadFromStorage();

    expect(ok).toBe(false);
    expect(useAuthStore.getState().isAuthenticated).toBe(false);
    // 401 = credential genuinely dead → token wiped, must re-login.
    expect(sessionStorage.getItem(STORAGE_KEY_REFRESH)).toBeNull();
    // No retry on a hard credential rejection.
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it('KEEPS the token on 429 and recovers the session on retry', async () => {
    sessionStorage.setItem(STORAGE_KEY_REFRESH, 'rt-1');
    let refreshCalls = 0;
    const fetchMock = vi.fn(async (url: string) => {
      if (String(url).includes('/api/refresh')) {
        refreshCalls += 1;
        // First attempt trips the rate limit, second succeeds.
        return refreshCalls === 1
          ? res(429, { error: 'rate limited' })
          : res(200, { access_token: 'jwt-1' });
      }
      return res(200, ME_OK); // /api/me
    });
    vi.stubGlobal('fetch', fetchMock);

    const ok = await useAuthStore.getState().loadFromStorage();

    expect(ok).toBe(true);
    expect(useAuthStore.getState().isAuthenticated).toBe(true);
    expect(useAuthStore.getState().jwt).toBe('jwt-1');
    // Token was never wiped by the transient 429.
    expect(sessionStorage.getItem(STORAGE_KEY_REFRESH)).toBe('rt-1');
    expect(refreshCalls).toBe(2);
  });

  it('never wipes the token even when all retries fail (persistent 429)', async () => {
    sessionStorage.setItem(STORAGE_KEY_REFRESH, 'rt-1');
    const fetchMock = vi.fn(async () => res(429, { error: 'rate limited' }));
    vi.stubGlobal('fetch', fetchMock);

    const ok = await useAuthStore.getState().loadFromStorage();

    expect(ok).toBe(false);
    expect(useAuthStore.getState().initialized).toBe(true);
    // Critical: the refresh token survives so the next reload re-establishes
    // the session without a fresh login (#1).
    expect(sessionStorage.getItem(STORAGE_KEY_REFRESH)).toBe('rt-1');
  }, 10_000);
});

describe('refresh (#1 — only 401/403 logs out)', () => {
  it('logs out on 401', async () => {
    sessionStorage.setItem(STORAGE_KEY_REFRESH, 'rt-1');
    useAuthStore.setState({ refreshToken: 'rt-1', jwt: 'old', isAuthenticated: true, user: ME_OK.user as never });
    vi.stubGlobal('fetch', vi.fn(async () => res(401, { error: 'bad token' })));

    await useAuthStore.getState().refresh();

    expect(useAuthStore.getState().isAuthenticated).toBe(false);
    expect(sessionStorage.getItem(STORAGE_KEY_REFRESH)).toBeNull();
    expect(mockWsClient.disconnect).toHaveBeenCalled();
  });

  it('keeps the session on 429 (no logout, token preserved)', async () => {
    sessionStorage.setItem(STORAGE_KEY_REFRESH, 'rt-1');
    useAuthStore.setState({ refreshToken: 'rt-1', jwt: 'old', isAuthenticated: true, user: ME_OK.user as never });
    vi.stubGlobal('fetch', vi.fn(async () => res(429, { error: 'rate limited' })));

    await useAuthStore.getState().refresh();

    expect(useAuthStore.getState().isAuthenticated).toBe(true);
    expect(sessionStorage.getItem(STORAGE_KEY_REFRESH)).toBe('rt-1');
    expect(mockWsClient.disconnect).not.toHaveBeenCalled();
  });
});
