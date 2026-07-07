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
  /** True between `system.update_installed` and the post-restart reload. */
  readonly restarting: boolean;
  dismiss: () => void;
  checkNow: () => Promise<void>;
}

const DISMISSED_KEY = 'duduclaw-update-dismissed-version';

const sleep = (ms: number) => new Promise<void>((resolve) => setTimeout(resolve, ms));

/**
 * The gateway raises SIGINT ~3s after broadcasting `system.update_installed`,
 * finishes graceful shutdown, then re-execs the updated binary on the same
 * port. Wait for it to go down and come back, then hard-reload so the
 * browser picks up the new embedded dashboard assets.
 */
async function waitForGatewayAndReload(): Promise<void> {
  // Let the old gateway actually go down first (3s notice window + drain).
  await sleep(6000);
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    try {
      const res = await fetch('/', { cache: 'no-store' });
      if (res.ok) {
        window.location.reload();
        return;
      }
    } catch {
      // Gateway still restarting — keep polling
    }
    await sleep(2000);
  }
  // Gave up waiting — reload anyway so the user sees the real state
  window.location.reload();
}

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

  // Fired by both the auto-update loop and the manual "install update"
  // button (broadcast to all tabs). The gateway is about to restart.
  client.subscribe('system.update_installed', () => {
    if (get().restarting) return;
    set({ restarting: true });
    void waitForGatewayAndReload();
  });

  return {
    notification: null,
    dismissed: false,
    restarting: false,

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
