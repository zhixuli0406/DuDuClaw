import { create } from 'zustand';
import { api } from '@/lib/api';
import { useAuthStore } from './auth-store';
import { actionableInboxItems, inboxSourcesDeniedFor, type InboxItem } from '@/lib/inbox-model';

interface ApprovalsStore {
  readonly pendingCount: number;
  readonly items: readonly InboxItem[];
  fetchCount: () => Promise<void>;
  setPendingCount: (n: number) => void;
  setItems: (items: readonly InboxItem[]) => void;
}

export const selectInboxCount = (state: ApprovalsStore) => state.pendingCount;
let requestVersion = 0;

/** All four surfaces use the same population, including locally resolved items. */
export const useApprovalsStore = create<ApprovalsStore>((set, get) => ({
  pendingCount: 0,
  items: [],
  fetchCount: async () => {
    const version = ++requestVersion;
    const denied = inboxSourcesDeniedFor(useAuthStore.getState().user?.role);
    const results = await Promise.all([
      denied.has('approvals') ? null : api.approvals.list().catch(() => null),
      denied.has('budget') ? null : api.budget.incidents().catch(() => null),
      api.tasks.list({ status: 'blocked' }).catch(() => null),
      api.tasks.list({ status: 'needs_human' }).catch(() => null),
      denied.has('installs') ? null : api.installRequests.list().catch(() => null),
      api.agents.list().catch(() => null),
    ]);
    const [approvals, budget, blocked, needsHuman, installs, agents] = results;
    const items: InboxItem[] = [];
    const add = (id: string, type: InboxItem['type'], status?: string) => items.push({ id, type, status, title: '', urgency: 0, actionable: true });
    for (const a of approvals?.approvals ?? []) add(`approval:${a.id}`, 'approval', 'pending');
    for (const i of budget?.incidents ?? []) add(`budget:${i.agent_id}:${i.ts}`, 'budget', i.event);
    for (const t of [...(blocked?.tasks ?? []), ...(needsHuman?.tasks ?? [])]) add(`blocked:${t.id}`, 'blocked', t.status);
    for (const i of installs?.requests ?? []) add(`install:${i.id}`, 'install', i.stage);
    // Same visible coverage as the Inbox; bounded waves avoid an RPC burst.
    const agentIds = (agents?.agents ?? []).slice(0, 12).map((a) => a.name);
    const failedDecisionAgents = new Set<string>();
    for (let offset = 0; offset < agentIds.length; offset += 4) {
      await Promise.all(agentIds.slice(offset, offset + 4).map(async (id) => {
        const result = await api.decisions.list(id, 10).catch(() => { failedDecisionAgents.add(id); return null; });
        for (const d of result?.decisions ?? []) add(`decision:${id}:${d.id}`, 'decision', 'open');
      }));
    }
    if (version !== requestVersion) return;
    // A failed source must not erase previously known pending work.
    const failedTypes = new Set<InboxItem['type']>();
    if (!approvals && !denied.has('approvals')) failedTypes.add('approval');
    if (!budget && !denied.has('budget')) failedTypes.add('budget');
    if (!blocked || !needsHuman || blocked.partial_error || needsHuman.partial_error) failedTypes.add('blocked');
    if (!installs && !denied.has('installs')) failedTypes.add('install');
    if (!agents) failedTypes.add('decision');
    for (const old of get().items) {
      if (failedTypes.has(old.type) || (old.type === 'decision' && [...failedDecisionAgents].some((id) => old.id.startsWith(`decision:${id}:`)))) {
        if (!items.some((item) => item.id === old.id)) items.push(old);
      }
    }
    set({ items, pendingCount: actionableInboxItems(items).length });
  },
  setItems: (items) => {
    ++requestVersion;
    set({ items, pendingCount: actionableInboxItems(items).length });
  },
  // Retained for existing consumers; new code supplies item identities instead.
  setPendingCount: (n) => set({ pendingCount: Math.max(0, n) }),
}));
