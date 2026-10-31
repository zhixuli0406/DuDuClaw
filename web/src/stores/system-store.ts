import { loadPublicBrand, refreshBrandRevision, useBrandStore } from '@/stores/brand-store';
import { create } from 'zustand';
import { api, type DoctorCheck, type SystemStatus } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { useConnectionStore } from '@/stores/connection-store';

interface SystemStore {
  readonly status: SystemStatus | null;
  readonly doctorChecks: ReadonlyArray<DoctorCheck>;
  readonly loading: boolean;
  readonly error: string | null;
  fetchStatus: () => Promise<void>;
  runDoctor: () => Promise<void>;
}

// Only the newest `system.status` request may write the store, so a slow
// answer from the gateway that was serving before a restart cannot overwrite
// the one that is serving now.
let statusRequest = 0;

export const useSystemStore = create<SystemStore>((set) => {
  // Subscribe to system events for real-time updates
  client.subscribe('system.status_changed', (payload) => {
    const data = payload as SystemStatus;
    set({ status: data });
    useBrandStore.getState().setBrand(null);
    void loadPublicBrand();
  });

  return {
    status: null,
    doctorChecks: [],
    loading: false,
    error: null,
    fetchStatus: async () => {
      const request = ++statusRequest;
      set({ loading: true, error: null });
      try {
        const status = await api.system.status();
        if (request === statusRequest) set({ status, loading: false });
      } catch {
        if (request === statusRequest) set({ loading: false, error: 'system.fetchStatusError' });
      }
    },
    runDoctor: async () => {
      set({ loading: true, error: null });
      try {
        const result = await api.system.doctor();
        set({ doctorChecks: result.checks, loading: false });
      } catch {
        set({ loading: false, error: 'system.healthCheckError' });
      }
    },
  };
});

// The status is fetched once per page, but the gateway behind the page can be
// restarted onto another build (an update, a launchd kickstart) while the
// WebSocket silently reconnects. Without a refetch the sidebar kept the version
// and edition of the gateway that answered first: a desktop window opened on
// 1.70.1 still said v1.70.1 after three restarts onto 1.76.2 (2026-10-27).
// Refetch on every transition into `authenticated`; keep the previous status
// meanwhile so edition gating does not flicker during the reconnect.
useConnectionStore.subscribe((state, previous) => {
  if (state.state === previous.state || state.state !== 'authenticated') return;
  void useSystemStore.getState().fetchStatus();
  useBrandStore.getState().setBrand(null);
  void refreshBrandRevision();
});

// Returning to a long-lived tab must also notice gateway polling updates.
if (typeof window !== 'undefined') {
  window.addEventListener('focus', () => {
    if (useConnectionStore.getState().state === 'authenticated') void refreshBrandRevision();
  });
}
