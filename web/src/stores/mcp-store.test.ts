import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import '@/test/mocks';
import { api } from '@/lib/api';
import { useMcpStore } from './mcp-store';

/**
 * Consent polling must observe a token that actually moved.
 *
 * `mcp.oauth.status.authenticated` now reports connection state rather than
 * access-token freshness — it stays true for a stale-but-refreshable token. A
 * poll that keyed off it would declare success on its first tick when the user
 * is re-authorizing an already-connected provider (say, to grant a new scope),
 * off the very token that was already on file, before the consent window has
 * even been answered.
 */
describe('mcp-store consent polling', () => {
  beforeEach(() => {
    vi.useFakeTimers();
    useMcpStore.setState({ oauthProviders: [], _oauthPollTimer: undefined });
  });
  afterEach(() => {
    vi.useRealTimers();
    vi.restoreAllMocks();
  });

  const CONNECTED_UNTIL = '2026-08-04T10:00:00Z';

  function seedConnectedProvider() {
    useMcpStore.setState({
      oauthProviders: [
        {
          provider_id: 'google',
          name: 'Google',
          auth_url: 'https://accounts.google.com/o/oauth2/v2/auth',
          scopes: [],
          configured: true,
          token_status: 'authenticated',
          expires_at: CONNECTED_UNTIL,
        },
      ],
    });
    vi.spyOn(api.mcp, 'oauthStart').mockResolvedValue({
      auth_url: 'https://accounts.google.com/consent',
      state: 's',
    });
  }

  it('does not report success off the token that was already stored', async () => {
    seedConnectedProvider();
    // The old token: still refreshable, so `authenticated` is true — but its
    // expiry has not moved, meaning consent has not completed.
    vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({
      authenticated: true,
      access_token_valid: false,
      can_refresh: true,
      expires_at: CONNECTED_UNTIL,
      scopes: [],
    });
    const fetchProviders = vi.spyOn(api.mcp, 'oauthProviders');

    await useMcpStore.getState().startOAuth('google');
    await vi.advanceTimersByTimeAsync(3500);

    expect(fetchProviders).not.toHaveBeenCalled();
  });

  it('reports success once the stored token moves past where it started', async () => {
    seedConnectedProvider();
    vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({
      authenticated: true,
      access_token_valid: true,
      can_refresh: true,
      expires_at: '2026-08-04T11:00:00Z',
      scopes: [],
    });
    const fetchProviders = vi
      .spyOn(api.mcp, 'oauthProviders')
      .mockResolvedValue({ providers: [] });

    await useMcpStore.getState().startOAuth('google');
    await vi.advanceTimersByTimeAsync(3500);

    expect(fetchProviders).toHaveBeenCalled();
  });

  it('falls back to `authenticated` against a gateway that omits the new field', async () => {
    seedConnectedProvider();
    vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({
      authenticated: true,
      expires_at: null,
      scopes: [],
    });
    const fetchProviders = vi
      .spyOn(api.mcp, 'oauthProviders')
      .mockResolvedValue({ providers: [] });

    await useMcpStore.getState().startOAuth('google');
    await vi.advanceTimersByTimeAsync(3500);

    expect(fetchProviders).toHaveBeenCalled();
  });
});
