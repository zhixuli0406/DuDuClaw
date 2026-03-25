import { create } from 'zustand';
import { api, type DoctorCheck, type SystemStatus } from '@/lib/api';
import { client } from '@/lib/ws-client';

interface SystemStore {
  readonly status: SystemStatus | null;
  readonly doctorChecks: ReadonlyArray<DoctorCheck>;
  readonly loading: boolean;
  readonly error: string | null;
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
    error: null,
    fetchStatus: async () => {
      set({ loading: true, error: null });
      try {
        const status = await api.system.status();
        set({ status, loading: false });
      } catch {
        set({ loading: false, error: '無法取得系統狀態' });
      }
    },
    runDoctor: async () => {
      set({ loading: true, error: null });
      try {
        const result = await api.system.doctor();
        set({ doctorChecks: result.checks, loading: false });
      } catch {
        set({ loading: false, error: '健康檢查失敗' });
      }
    },
  };
});
