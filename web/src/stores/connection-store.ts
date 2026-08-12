import { create } from 'zustand';
import { client, type ConnectionState } from '@/lib/ws-client';
import { useAuthStore } from '@/stores/auth-store';

type TokenGetter = () => string | undefined;

interface ConnectionStore {
  readonly state: ConnectionState;
  readonly error: string | null;
  /**
   * True once the WebSocket has reached `authenticated` at least once this
   * session. It lets `AuthGuard` tell two visually-identical situations apart:
   * a *first* handshake still in flight (a brief, expected wait → spinner) vs. a
   * connection that was live and then dropped mid-session (the gateway went
   * away → a reconnecting banner with text + a manual retry, never an endless
   * bare spinner that hides every page's own error state). Reset on an explicit
   * `disconnect()` (logout) so a fresh login starts from "never connected".
   */
  readonly everAuthenticated: boolean;
  connectWithAuth: (getToken: TokenGetter) => Promise<void>;
  disconnect: () => void;
}

export const useConnectionStore = create<ConnectionStore>((set) => {
  client.onStateChange = (state) => {
    // Functional update so a momentary drop to `connecting`/`disconnected`
    // during auto-reconnect never clears the "have connected before" memory.
    set((prev) => ({
      state,
      error: null,
      everAuthenticated: prev.everAuthenticated || state === 'authenticated',
    }));
  };

  return {
    state: 'disconnected',
    error: null,
    everAuthenticated: false,
    connectWithAuth: async (getToken: TokenGetter) => {
      try {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const url = `${protocol}//${window.location.host}/ws`;
        // authRefreshHook: when WS handshake fails with an auth-shaped error,
        // ws-client triggers this before reconnecting so getToken() returns
        // a fresh JWT. Defends against the JWT-expiry death loop.
        const authRefreshHook = () => useAuthStore.getState().refresh();
        await client.connect(url, getToken, authRefreshHook);
      } catch (e) {
        const msg = String(e);
        set({ error: msg });
      }
    },
    disconnect: () => {
      client.disconnect();
      // Forget the connected-before memory on an intentional teardown (logout);
      // the next login is a first connection again.
      set({ everAuthenticated: false });
    },
  };
});
