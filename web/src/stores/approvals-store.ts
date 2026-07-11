import { create } from 'zustand';
import { api } from '@/lib/api';

/**
 * Inbox-count store (WP14-T14.7, extended by dashboard-redesign §4.2). Holds the
 * unified "needs me" count so the sidebar / mobile nav can show one badge — the
 * full list lives in InboxPage. Since the redesign merged approvals + budget +
 * blocked tasks into one Inbox, `fetchCount` sums those three cheap sources
 * (decisions are excluded here — they need a per-agent call; InboxPage refines
 * the exact total while the page is open via `setPendingCount`).
 *
 * Every source is best-effort and silent on failure: a manager-gated RPC that
 * errors for a non-privileged viewer simply contributes 0 (fail-safe).
 */
interface ApprovalsStore {
  readonly pendingCount: number;
  fetchCount: () => Promise<void>;
  setPendingCount: (n: number) => void;
}

export const useApprovalsStore = create<ApprovalsStore>((set) => ({
  pendingCount: 0,
  fetchCount: async () => {
    const [approvals, budget, blocked] = await Promise.all([
      api.approvals.list().catch(() => null),
      api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
    ]);
    const approvalsN = approvals?.count ?? approvals?.approvals?.length ?? 0;
    const budgetN = budget?.incidents?.length ?? 0;
    const blockedN = blocked?.tasks?.length ?? 0;
    set({ pendingCount: Math.max(0, approvalsN + budgetN + blockedN) });
  },
  setPendingCount: (n: number) => set({ pendingCount: Math.max(0, n) }),
}));
