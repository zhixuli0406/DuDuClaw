import { create } from 'zustand';
import { client, type ConnectionState } from '@/lib/ws-client';

interface ConnectionStore {
  readonly state: ConnectionState;
  readonly error: string | null;
  connect: (token?: string) => Promise<void>;
  disconnect: () => void;
}

export const useConnectionStore = create<ConnectionStore>((set) => {
  client.onStateChange = (state) => {
    set({ state, error: null });
  };

  return {
    state: 'disconnected',
    error: null,
    connect: async (token?: string) => {
      try {
        // In production, Dashboard is served from the same Rust binary as the WS gateway.
        // In development, Vite proxies /ws to the Rust gateway.
        // Either way, window.location.host is the correct target.
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const url = `${protocol}//${window.location.host}/ws`;
        // console.log('[DuDuClaw] Connecting to WebSocket:', url);
        await client.connect(url, token);
        // console.log('[DuDuClaw] WebSocket connected');
      } catch (e) {
        const msg = String(e);
        console.error('[DuDuClaw] WebSocket connection failed:', msg);
        set({ error: msg });
      }
    },
    disconnect: () => {
      client.disconnect();
    },
  };
});
