import { create } from 'zustand';
import { api, type BrowserAuditEntry, type BrowserbaseSession, type BrowserbaseCostSummary, type ToolApproval } from '@/lib/api';

interface BrowserState {
  // Audit
  readonly auditEntries: readonly BrowserAuditEntry[];
  readonly auditLoading: boolean;
  // Emergency
  readonly emergencyStatus: 'normal' | 'stopped' | 'unknown';
  // Tool approvals
  readonly toolApprovals: readonly ToolApproval[];
  // Browserbase
  readonly browserbaseSessions: readonly BrowserbaseSession[];
  readonly browserbaseCost: BrowserbaseCostSummary | null;
  readonly browserbaseLoading: boolean;
  // Error tracking
  readonly lastError: string | null;
  fetchBrowserbaseSessions: () => Promise<void>;
  fetchBrowserbaseCost: (hours?: number) => Promise<void>;
  createBrowserbaseSession: () => Promise<void>;
  closeBrowserbaseSession: (sessionId: string) => Promise<void>;
  // Actions
  fetchAuditLog: (limit?: number, agentId?: string) => Promise<void>;
  fetchEmergencyStatus: () => Promise<void>;
  emergencyStop: () => Promise<void>;
  emergencyResume: () => Promise<void>;
  fetchToolApprovals: (agentId?: string) => Promise<void>;
  approveTool: (toolName: string, agentId: string, durationMinutes?: number, sessionScoped?: boolean) => Promise<void>;
  revokeTool: (toolName: string, agentId: string) => Promise<void>;
  clearError: () => void;
}

export const useBrowserStore = create<BrowserState>((set, get) => ({
  auditEntries: [],
  auditLoading: false,
  emergencyStatus: 'unknown',
  toolApprovals: [],
  browserbaseSessions: [],
  browserbaseCost: null,
  browserbaseLoading: false,
  lastError: null,

  clearError: () => set({ lastError: null }),

  fetchAuditLog: async (limit = 20, agentId?: string) => {
    set({ auditLoading: true });
    try {
      const res = await api.browser.auditLog(limit, agentId);
      set({ auditEntries: res?.entries ?? [], auditLoading: false });
    } catch (e) {
      set({ auditEntries: [], auditLoading: false, lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  fetchEmergencyStatus: async () => {
    try {
      const res = await api.browser.emergencyStop('status');
      set({ emergencyStatus: res?.status ?? 'unknown' });
    } catch (e) {
      set({ emergencyStatus: 'unknown', lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  emergencyStop: async () => {
    try {
      await api.browser.emergencyStop('stop');
      set({ emergencyStatus: 'stopped' });
    } catch (e) {
      set({ emergencyStatus: 'unknown', lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  emergencyResume: async () => {
    try {
      await api.browser.emergencyStop('resume');
      set({ emergencyStatus: 'normal' });
    } catch (e) {
      set({ emergencyStatus: 'unknown', lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  fetchToolApprovals: async (agentId?: string) => {
    try {
      const res = await api.browser.toolApprove('list', { agent_id: agentId });
      set({ toolApprovals: res?.approvals ?? [] });
    } catch (e) {
      set({ toolApprovals: [], lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  approveTool: async (toolName: string, agentId: string, durationMinutes?: number, sessionScoped?: boolean) => {
    try {
      await api.browser.toolApprove('approve', {
        tool_name: toolName,
        agent_id: agentId,
        duration_minutes: durationMinutes,
        session_scoped: sessionScoped,
      });
      await get().fetchToolApprovals();
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  fetchBrowserbaseSessions: async () => {
    set({ browserbaseLoading: true });
    try {
      const res = await api.browser.browserbaseSessions('list', { limit: 20 });
      set({ browserbaseSessions: res?.sessions ?? [], browserbaseLoading: false });
    } catch (e) {
      set({ browserbaseSessions: [], browserbaseLoading: false, lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  fetchBrowserbaseCost: async (hours = 24) => {
    try {
      const res = await api.browser.browserbaseCost(hours);
      set({ browserbaseCost: res ?? null });
    } catch (e) {
      set({ browserbaseCost: null, lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  createBrowserbaseSession: async () => {
    try {
      await api.browser.browserbaseSessions('create');
      await get().fetchBrowserbaseSessions();
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  closeBrowserbaseSession: async (sessionId: string) => {
    try {
      await api.browser.browserbaseSessions('close', { session_id: sessionId });
      await get().fetchBrowserbaseSessions();
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },

  revokeTool: async (toolName: string, agentId: string) => {
    try {
      await api.browser.toolApprove('revoke', {
        tool_name: toolName,
        agent_id: agentId,
      });
      await get().fetchToolApprovals();
    } catch (e) {
      set({ lastError: e instanceof Error ? e.message : String(e) });
    }
  },
}));
