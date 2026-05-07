import { create } from 'zustand';
import { client, type ConnectionState } from '@/lib/ws-client';
import { useAuthStore } from '@/stores/auth-store';

type TokenGetter = () => string | undefined;

interface ConnectionStore {
  readonly state: ConnectionState;
  readonly error: string | null;
  connectWithAuth: (getToken: TokenGetter) => Promise<void>;
  disconnect: () => void;
}

export const useConnectionStore = create<ConnectionStore>((set) => {
  client.onStateChange = (state) => {
    set({ state, error: null });
  };

  return {
    state: 'disconnected',
    error: null,
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
    },
  };
});
