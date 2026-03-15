import { create } from 'zustand';
import { client, type ConnectionState } from '@/lib/ws-client';

interface ConnectionStore {
  readonly state: ConnectionState;
  readonly error: string | null;
  connect: (token?: string) => Promise<void>;
  disconnect: () => void;
}

export const useConnectionStore = create<ConnectionStore>((set) => {
  // Listen for state changes from the client
  client.onStateChange = (state) => {
    set({ state, error: null });
  };

  return {
    state: 'disconnected',
    error: null,
    connect: async (token?: string) => {
      try {
        const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        const url = `${protocol}//${window.location.host}/ws`;
        await client.connect(url, token);
      } catch (e) {
        set({ error: String(e) });
      }
    },
    disconnect: () => {
      client.disconnect();
    },
  };
});
