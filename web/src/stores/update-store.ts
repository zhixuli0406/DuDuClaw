import { create } from 'zustand';
import { client } from '@/lib/ws-client';
import { api } from '@/lib/api';

export interface UpdateNotification {
  available: boolean;
  current_version: string;
  latest_version: string;
  release_notes: string;
  published_at: string;
  install_method: string;
}

interface UpdateStore {
  readonly notification: UpdateNotification | null;
  readonly dismissed: boolean;
  dismiss: () => void;
  checkNow: () => Promise<void>;
}

const DISMISSED_KEY = 'duduclaw-update-dismissed-version';

export const useUpdateStore = create<UpdateStore>((set, get) => {
  // Listen for real-time update events from the gateway
  client.subscribe('system.update_available', (payload) => {
    const data = payload as UpdateNotification;
    if (!data.available) return;

    // If user already dismissed this specific version, don't show again
    const dismissedVersion = localStorage.getItem(DISMISSED_KEY);
    set({
      notification: data,
      dismissed: dismissedVersion === data.latest_version,
    });
  });

  return {
    notification: null,
    dismissed: false,

    dismiss: () => {
      const info = get().notification;
      if (info) {
        localStorage.setItem(DISMISSED_KEY, info.latest_version);
      }
      set({ dismissed: true });
    },

    checkNow: async () => {
      try {
        const info = await api.system.checkUpdate();
        if (info.available) {
          const dismissedVersion = localStorage.getItem(DISMISSED_KEY);
          set({
            notification: {
              available: info.available,
              current_version: info.current_version,
              latest_version: info.latest_version,
              release_notes: info.release_notes,
              published_at: info.published_at,
              install_method: info.install_method,
            },
            dismissed: dismissedVersion === info.latest_version,
          });
        }
      } catch {
        // Silently fail — update check is non-critical
      }
    },
  };
});
