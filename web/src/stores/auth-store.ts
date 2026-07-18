import { create } from 'zustand';
import { client } from '@/lib/ws-client';

export type UserRole = 'admin' | 'manager' | 'employee';

export interface AuthUser {
  id: string;
  email: string;
  display_name: string;
  role: UserRole;
  status: string;
}

export interface AgentBinding {
  user_id: string;
  agent_name: string;
  access_level: 'owner' | 'operator' | 'viewer';
  bound_at: string;
}

interface AuthStore {
  readonly user: AuthUser | null;
  readonly jwt: string | null;
  readonly refreshToken: string | null;
  readonly isAuthenticated: boolean;
  readonly initialized: boolean;
  readonly bindings: AgentBinding[];
  readonly loading: boolean;

  login: (email: string, password: string) => Promise<void>;
  /** Passwordless login step 1: ask the gateway to DM a code. Returns the
   *  challenge id (+ masked target hint when the account has a channel). */
  otpRequest: (email: string) => Promise<{ challenge_id: string; hint?: string }>;
  /** Passwordless login step 2: verify the code and establish the session. */
  otpVerify: (challengeId: string, code: string) => Promise<void>;
  /** First-run onboarding: is this instance unclaimed (needs an admin password)? */
  firstRunStatus: () => Promise<boolean>;
  /** First-run onboarding: set the initial admin password, then establish the
   *  session (no console one-time password needed). */
  firstRunClaim: (password: string) => Promise<void>;
  logout: () => void;
  refresh: () => Promise<void>;
  loadFromStorage: () => Promise<boolean>;
  setUser: (user: AuthUser, bindings: AgentBinding[]) => void;
}

const STORAGE_KEY_REFRESH = 'duduclaw-refresh-token';

// M22 mitigation (client-only): the access token (`jwt`) is already held in
// memory only (zustand state, never persisted), so it is not readable from
// any persistent store. The refresh token is moved from `localStorage` to
// `sessionStorage` to shrink its XSS-exposure surface:
//   - it is scoped to the originating tab/window and is NOT shared with other
//     tabs, and
//   - it is cleared automatically when the tab/window closes (no indefinite
//     persistence on the device).
// A new tab requires a fresh login, which is an acceptable trade-off and a
// strict improvement over an indefinitely-persisted localStorage token.
//
// NOTE: this is a partial mitigation only. The full fix requires the backend
// to deliver the refresh token in an httpOnly + SameSite + Secure cookie so it
// is never reachable from JavaScript at all. That needs gateway Set-Cookie
// support (out of scope for this client-side change). Until then, any code
// reading the token MUST go through `refreshTokenStorage` below.
//
// We also migrate any pre-existing localStorage token to sessionStorage on
// first read so users who logged in before this change are not force-logged-out.
const refreshTokenStorage = {
  get(): string | null {
    if (typeof sessionStorage === 'undefined') return null;
    const fromSession = sessionStorage.getItem(STORAGE_KEY_REFRESH);
    if (fromSession) return fromSession;
    // One-time migration from the legacy localStorage location.
    if (typeof localStorage !== 'undefined') {
      const legacy = localStorage.getItem(STORAGE_KEY_REFRESH);
      if (legacy) {
        sessionStorage.setItem(STORAGE_KEY_REFRESH, legacy);
        localStorage.removeItem(STORAGE_KEY_REFRESH);
        return legacy;
      }
    }
    return null;
  },
  set(token: string): void {
    if (typeof sessionStorage !== 'undefined') {
      sessionStorage.setItem(STORAGE_KEY_REFRESH, token);
    }
    // Ensure no stale copy lingers in localStorage.
    if (typeof localStorage !== 'undefined') {
      localStorage.removeItem(STORAGE_KEY_REFRESH);
    }
  },
  clear(): void {
    if (typeof sessionStorage !== 'undefined') {
      sessionStorage.removeItem(STORAGE_KEY_REFRESH);
    }
    if (typeof localStorage !== 'undefined') {
      localStorage.removeItem(STORAGE_KEY_REFRESH);
    }
  },
};

// Auto-refresh interval — JWT access token TTL is 30min server-side,
// refresh at 25min so we never serve a request with an expired token.
const REFRESH_INTERVAL_MS = 25 * 60 * 1000;

let refreshTimer: ReturnType<typeof setInterval> | null = null;
let visibilityHandler: (() => void) | null = null;
let lastRefreshAt = 0;

function stopRefreshTimer() {
  if (refreshTimer) {
    clearInterval(refreshTimer);
    refreshTimer = null;
  }
  if (visibilityHandler && typeof document !== 'undefined') {
    document.removeEventListener('visibilitychange', visibilityHandler);
    visibilityHandler = null;
  }
}

function startRefreshTimer(refresh: () => Promise<void>) {
  stopRefreshTimer();
  lastRefreshAt = Date.now();

  const tick = () => {
    void refresh()
      .then(() => { lastRefreshAt = Date.now(); })
      .catch(() => { /* refresh handles its own logout */ });
  };

  refreshTimer = setInterval(tick, REFRESH_INTERVAL_MS);

  // Background tabs throttle setInterval to ~1/min; when the user returns,
  // proactively refresh if more than the interval has elapsed since last refresh.
  if (typeof document !== 'undefined') {
    visibilityHandler = () => {
      if (document.visibilityState === 'visible'
          && Date.now() - lastRefreshAt >= REFRESH_INTERVAL_MS) {
        tick();
      }
    };
    document.addEventListener('visibilitychange', visibilityHandler);
  }
}

/** Error carrying the HTTP status so callers can tell a rejected credential
 *  (401/403 → wipe token, re-login) apart from a transient failure
 *  (429 rate-limit / 5xx / network → keep token, retry later). */
export class ApiError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = 'ApiError';
    this.status = status;
  }
}

/**
 * Should an auth failure keep the refresh token and retry, rather than force a
 * logout? Only an explicit credential rejection (401 Unauthorized / 403
 * Forbidden) means the refresh token is actually invalid. Everything else —
 * 429 (the per-IP `/api/refresh` rate limit tripped by opening many tabs),
 * 5xx, or a network blip — is transient: wiping the token there needlessly
 * bounces a still-valid session back to the login screen (bug #1).
 */
export function isRetryableAuthError(err: unknown): boolean {
  if (err instanceof ApiError) {
    return err.status !== 401 && err.status !== 403;
  }
  // Non-HTTP failures (fetch/network/JSON parse) are transient.
  return true;
}

// Backoff base for transient `/api/refresh` failures during initial load.
const RETRY_BASE_MS = 250;
const RETRY_MAX_ATTEMPTS = 4;

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

async function apiPost<T>(path: string, body: Record<string, unknown>): Promise<T> {
  const res = await fetch(path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(body),
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new ApiError(res.status, data.error || `HTTP ${res.status}`);
  }
  return data as T;
}

async function apiGet<T>(path: string, jwt: string): Promise<T> {
  const res = await fetch(path, {
    headers: { Authorization: `Bearer ${jwt}` },
  });
  const data = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new ApiError(res.status, data.error || `HTTP ${res.status}`);
  }
  return data as T;
}

// H8 fix: in-flight lock for refresh to prevent concurrent refresh calls
let refreshPromise: Promise<void> | null = null;

export const useAuthStore = create<AuthStore>((set, get) => ({
  user: null,
  jwt: null,
  refreshToken: null,
  isAuthenticated: false,
  initialized: false,
  bindings: [],
  loading: false,

  login: async (email: string, password: string) => {
    set({ loading: true });
    try {
      const data = await apiPost<{
        access_token: string;
        refresh_token: string;
        user: AuthUser;
      }>('/api/login', { email, password });

      refreshTokenStorage.set(data.refresh_token);

      // Intentional: fetch bindings + validate token server-side (login response
      // doesn't include bindings to keep the REST endpoint simple)
      const me = await apiGet<{ user: AuthUser; bindings: AgentBinding[] }>(
        '/api/me',
        data.access_token
      );

      set({
        user: me.user,
        jwt: data.access_token,
        refreshToken: data.refresh_token,
        isAuthenticated: true,
        initialized: true,
        bindings: me.bindings,
        loading: false,
      });
      startRefreshTimer(get().refresh);
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  otpRequest: async (email: string) => {
    const data = await apiPost<{ challenge_id: string; sent: boolean; hint?: string }>(
      '/api/otp/request',
      { email },
    );
    return { challenge_id: data.challenge_id, hint: data.hint };
  },

  otpVerify: async (challengeId: string, code: string) => {
    set({ loading: true });
    try {
      const data = await apiPost<{
        access_token: string;
        refresh_token: string;
        user: AuthUser;
      }>('/api/otp/verify', { challenge_id: challengeId, code });

      refreshTokenStorage.set(data.refresh_token);
      const me = await apiGet<{ user: AuthUser; bindings: AgentBinding[] }>(
        '/api/me',
        data.access_token,
      );
      set({
        user: me.user,
        jwt: data.access_token,
        refreshToken: data.refresh_token,
        isAuthenticated: true,
        initialized: true,
        bindings: me.bindings,
        loading: false,
      });
      startRefreshTimer(get().refresh);
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  firstRunStatus: async () => {
    try {
      const res = await fetch('/api/first-run/status');
      if (!res.ok) return false;
      const data = (await res.json()) as { claimable?: boolean };
      return data.claimable === true;
    } catch {
      return false; // fail-closed: no claim UI if we can't confirm
    }
  },

  firstRunClaim: async (password: string) => {
    set({ loading: true });
    try {
      await apiPost('/api/first-run/claim', { password });
      // Claim only sets the password; establish the session via the normal
      // login path so token wiring / refresh timer are identical.
      await get().login('admin@local', password);
    } catch (e) {
      set({ loading: false });
      throw e;
    }
  },

  // R2 fix: disconnect WebSocket on logout (via client singleton, avoids circular dep)
  logout: () => {
    stopRefreshTimer();
    client.disconnect();
    refreshTokenStorage.clear();
    set({
      user: null,
      jwt: null,
      refreshToken: null,
      isAuthenticated: false,
      initialized: true, // keep initialized=true so AuthGuard redirects to login
      bindings: [],
    });
  },

  // H8 fix: singleton Promise prevents concurrent refresh calls
  // R2 fix: microtask-deferred cleanup to prevent sub-ms race window
  refresh: async () => {
    if (refreshPromise) return refreshPromise;
    refreshPromise = (async () => {
      try {
        const refreshToken = get().refreshToken ?? refreshTokenStorage.get();
        if (!refreshToken) {
          get().logout();
          return;
        }

        const data = await apiPost<{ access_token: string }>('/api/refresh', {
          refresh_token: refreshToken,
        });

        const me = await apiGet<{ user: AuthUser; bindings: AgentBinding[] }>(
          '/api/me',
          data.access_token
        );

        set({
          jwt: data.access_token,
          user: me.user,
          bindings: me.bindings,
          isAuthenticated: true,
          initialized: true,
        });
        // Re-arm in case timer was lost (e.g., first refresh after loadFromStorage)
        startRefreshTimer(get().refresh);
      } catch (err) {
        // Only a rejected credential (401/403) means the refresh token is dead
        // and we must log out. A transient failure (429 rate-limit / 5xx /
        // network) keeps the session — the current access token is usually
        // still valid (we refresh at 25min for a 30min TTL) and the next timer
        // tick (or an on-demand call) retries. Never bounce to login here (#1).
        if (!isRetryableAuthError(err)) {
          get().logout();
        }
      } finally {
        // Defer cleanup by one microtask so all concurrent awaiters
        // share the same promise (R2 race fix)
        await Promise.resolve();
        refreshPromise = null;
      }
    })();
    return refreshPromise;
  },

  // C6 fix: verifies JWT via server, uses refresh token to get fresh access token.
  // Sets `initialized: true` when done (regardless of success/failure) so AuthGuard
  // only runs this once (R2 AuthGuard re-mount fix).
  loadFromStorage: async () => {
    if (get().initialized) return get().isAuthenticated;

    const refreshToken = refreshTokenStorage.get();
    if (!refreshToken) {
      set({ initialized: true });
      return false;
    }

    // Retry transient failures with backoff so a `/api/refresh` rate-limit
    // spike (429, tripped by opening many tabs) or a network blip doesn't wipe
    // the token and bounce a valid session to /login (#1). Only an explicit
    // 401/403 clears the token.
    for (let attempt = 0; attempt < RETRY_MAX_ATTEMPTS; attempt++) {
      try {
        const data = await apiPost<{ access_token: string }>('/api/refresh', {
          refresh_token: refreshToken,
        });

        const me = await apiGet<{ user: AuthUser; bindings: AgentBinding[] }>(
          '/api/me',
          data.access_token
        );

        set({
          user: me.user,
          jwt: data.access_token,
          refreshToken,
          isAuthenticated: true,
          initialized: true,
          bindings: me.bindings,
        });
        startRefreshTimer(get().refresh);
        return true;
      } catch (err) {
        if (!isRetryableAuthError(err)) {
          // 401/403: the refresh token is genuinely invalid — re-login.
          refreshTokenStorage.clear();
          set({ initialized: true });
          return false;
        }
        // Transient: back off and retry, but KEEP the token.
        if (attempt < RETRY_MAX_ATTEMPTS - 1) {
          await delay(RETRY_BASE_MS * 2 ** attempt);
        }
      }
    }

    // Retries exhausted (persistent 429/network). Preserve the token so the
    // next reload re-establishes the session without a fresh login; surface
    // login for now but never wipe the credential.
    set({ initialized: true });
    return false;
  },

  setUser: (user: AuthUser, bindings: AgentBinding[]) => {
    set({ user, bindings });
  },
}));
