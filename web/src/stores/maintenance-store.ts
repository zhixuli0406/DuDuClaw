import { create } from 'zustand';
import { api, type MaintenanceStatus } from '@/lib/api';
import { client } from '@/lib/ws-client';

/**
 * Maintenance Mode — Entry A (`DESIGN-maintenance-mode-2026-08.md` §2).
 *
 * A tiny Zustand store, same shape as `tasks-store.ts`'s real-time pattern,
 * so ANY component (the settings card, DevicePage's "顯示詳情" toggle, a
 * future dashboard-wide banner) can read `useMaintenanceStore(s => s.status)`
 * without re-fetching or prop-drilling. `client.subscribe('maintenance.
 * status_changed', ...)` keeps it live across explicit enable/disable calls
 * (§2.7's push requirement); a TTL expiry or gateway-restart force-close does
 * NOT currently push (the gateway's periodic sweep has no broadcast channel
 * wired to it yet — see the maintenance.rs module doc TODO) so `fetchStatus`
 * is ALSO called on a 30s interval by whichever component mounts the card —
 * a deliberate, documented gap short of full §2.7 compliance, not an
 * oversight.
 */
interface MaintenanceStore {
  readonly status: MaintenanceStatus | null;
  readonly loading: boolean;
  readonly error: unknown;
  fetchStatus: () => Promise<void>;
}

export const useMaintenanceStore = create<MaintenanceStore>((set) => {
  client.subscribe('maintenance.status_changed', (payload) => {
    set({ status: payload as MaintenanceStatus, error: null });
  });

  return {
    status: null,
    loading: false,
    error: null,
    fetchStatus: async () => {
      set({ loading: true });
      try {
        const status = await api.maintenance.status();
        set({ status, loading: false, error: null });
      } catch (e) {
        set({ error: e, loading: false });
      }
    },
  };
});

/** True iff maintenance mode is active AND its `show_details` sub-capability
 *  is unlocked — the single condition `formatDeviceOpDetail`'s maintenance
 *  bypass parameter should be driven by. Exported as a pure helper so it has
 *  exactly one implementation regardless of how many components need it. */
export function showDetailsUnlocked(status: MaintenanceStatus | null): boolean {
  return Boolean(status?.active && status.window?.sub_capabilities.includes('show_details'));
}
