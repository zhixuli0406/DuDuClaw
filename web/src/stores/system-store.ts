import { create } from 'zustand';
import { api, type DoctorCheck, type SystemStatus } from '@/lib/api';
import { client } from '@/lib/ws-client';

interface SystemStore {
  readonly status: SystemStatus | null;
  readonly doctorChecks: ReadonlyArray<DoctorCheck>;
  readonly loading: boolean;
  fetchStatus: () => Promise<void>;
  runDoctor: () => Promise<void>;
}

export const useSystemStore = create<SystemStore>((set) => {
  // Subscribe to system events for real-time updates
  client.subscribe('system.status_changed', (payload) => {
    const data = payload as SystemStatus;
    set({ status: data });
  });

  return {
    status: null,
    doctorChecks: [],
    loading: false,
    fetchStatus: async () => {
      set({ loading: true });
      try {
        const status = await api.system.status();
        set({ status, loading: false });
      } catch {
        set({ loading: false });
      }
    },
    runDoctor: async () => {
      set({ loading: true });
      try {
        const result = await api.system.doctor();
        set({ doctorChecks: result.checks, loading: false });
      } catch {
        set({ loading: false });
      }
    },
  };
});
