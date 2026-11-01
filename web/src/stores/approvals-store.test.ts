import { beforeEach, describe, expect, it } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from './auth-store';
import { selectInboxCount, useApprovalsStore } from './approvals-store';
import { ARCHIVED_KEY, PROCESSED_KEY, type InboxItem } from '@/lib/inbox-model';

beforeEach(() => {
  localStorage.clear();
  useAuthStore.setState({ user: { id: 'owner', email: '', display_name: 'Owner', role: 'admin', status: 'active' } });
  useApprovalsStore.getState().setItems([]);
});

function gateway(failBudget = false) {
  mockWsClient.call.mockImplementation(async (method, params) => {
    if (method === 'approvals.list') return { approvals: [{ id: 'a' }] };
    if (method === 'budget.incidents') { if (failBudget) throw new Error('offline'); return { incidents: [{ agent_id: 'staff', ts: 'today' }] }; }
    if (method === 'tasks.list') return { tasks: [{ id: params?.status, status: params?.status }] };
    if (method === 'install_requests.list') return { requests: [{ id: 'i', stage: 'awaiting_manager' }] };
    if (method === 'agents.list') return { agents: [{ name: 'staff' }] };
    if (method === 'decisions.list') return { decisions: [{ id: 'd' }] };
    return {};
  });
}

describe('one actionable inbox count', () => {
  it('includes installs, employee questions and needs_human on every poll, excluding local resolutions', async () => {
    gateway();
    localStorage.setItem(PROCESSED_KEY, JSON.stringify(['approval:a']));
    localStorage.setItem(ARCHIVED_KEY, JSON.stringify(['budget:staff:today']));
    await useApprovalsStore.getState().fetchCount();
    expect(selectInboxCount(useApprovalsStore.getState())).toBe(4);
    await useApprovalsStore.getState().fetchCount();
    expect(selectInboxCount(useApprovalsStore.getState())).toBe(4);
  });

  it('a failed source does not erase known pending work', async () => {
    gateway();
    await useApprovalsStore.getState().fetchCount();
    gateway(true);
    await useApprovalsStore.getState().fetchCount();
    expect(selectInboxCount(useApprovalsStore.getState())).toBe(6);
  });

  it('a late sidebar poll cannot overwrite a newer page update', async () => {
    let resolve!: (value: unknown) => void;
    mockWsClient.call.mockImplementation((method) => method === 'approvals.list'
      ? new Promise((done) => { resolve = done; }) : Promise.resolve({}));
    const poll = useApprovalsStore.getState().fetchCount();
    const item: InboxItem = { id: 'decision:staff:new', type: 'decision', title: '', urgency: 0, actionable: true };
    useApprovalsStore.getState().setItems([item]);
    resolve({ approvals: [] });
    await poll;
    expect(useApprovalsStore.getState().items).toEqual([item]);
    expect(selectInboxCount(useApprovalsStore.getState())).toBe(1);
  });
});
