import { create } from 'zustand';
import { api } from '@/lib/api';
import { client } from '@/lib/ws-client';
import { INBOX_INVALIDATIONS, subscribeInvalidations } from '@/lib/live-events';
import { inboxSourcesDeniedFor } from '@/lib/inbox-model';
import { useAuthStore } from '@/stores/auth-store';

/** The sidebar's aggregate count. A failed refresh preserves the last count
 * and marks it stale, so a lost connection never looks like an empty inbox. */
interface ApprovalsStore {
  readonly pendingCount: number;
  readonly stale: boolean;
  fetchCount: () => Promise<boolean>;
  setPendingCount: (n: number) => void;
}

export const useApprovalsStore = create<ApprovalsStore>((set, get) => {
  let requestSeq = 0;
  const invalidate = () => {
    ++requestSeq; // A response started before this event is no longer current.
    set({ stale: true });
  };
  subscribeInvalidations(INBOX_INVALIDATIONS, () => get().fetchCount(), invalidate);
  client.subscribe('connection.stale', invalidate);

  return {
    pendingCount: 0,
    stale: true,
    fetchCount: async () => {
      const seq = ++requestSeq;
      const denied = inboxSourcesDeniedFor(useAuthStore.getState().user?.role);
      try {
        const [approvals, budget, blocked, needsHuman] = await Promise.all([
          denied.has('approvals') ? Promise.resolve(null) : api.approvals.list(),
          denied.has('budget') ? Promise.resolve(null) : api.budget.incidents(),
          api.tasks.list({ status: 'blocked' }),
          api.tasks.list({ status: 'needs_human' }),
        ]);
        const complete = blocked?.partial_error == null && needsHuman?.partial_error == null;
        if (seq !== requestSeq) return complete;
        if (!complete) {
          set({ stale: true });
          return false;
        }
        const approvalsN = approvals?.count ?? approvals?.approvals?.length ?? 0;
        const budgetN = budget?.incidents?.length ?? 0;
        const tasksN = (blocked?.tasks?.length ?? 0) + (needsHuman?.tasks?.length ?? 0);
        set({ pendingCount: Math.max(0, approvalsN + budgetN + tasksN), stale: false });
        return true;
      } catch {
        if (seq === requestSeq) set({ stale: true });
        return false;
      }
    },
    setPendingCount: (n: number) => set({ pendingCount: Math.max(0, n) }),
  };
});
