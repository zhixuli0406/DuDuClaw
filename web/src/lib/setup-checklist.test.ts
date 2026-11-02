import { describe, expect, it, vi } from 'vitest';
vi.mock('@/lib/ws-client', () => ({ client: { call: vi.fn(), subscribe: vi.fn() } }));
import { client } from '@/lib/ws-client';
import { loadSetupData, setupItems, setupProgress, type SetupData } from './setup-checklist';

const empty: SetupData = {
  company: '', knowledge: 0, runtime: false, accounts: 0, google: false, odoo: false,
  agents: 0, users: [], channels: false,
  skills: 0, routines: 0, tasks: 0,
};
const filled: SetupData = {
  company: 'Company', knowledge: 1, runtime: true, accounts: 1, google: true, odoo: true,
  agents: 1, users: [{ role: 'admin', status: 'active', bindings: [] }, { role: 'employee', status: 'active', bindings: [{}] }],
  channels: true, skills: 1, routines: 1, tasks: 1,
};

describe('setup checklist table', () => {
  it.each(setupItems)('$id checks empty and populated data', item => {
    expect(item.check(empty)).toBe(item.id === 'permissions');
    expect(item.check(filled)).toBe(true);
  });
  it('uses accounts alone for account readiness', () => {
    const item = setupItems.find(i => i.id === 'accounts')!;
    expect(item.check({ ...empty, accounts: 1 })).toBe(true);
    expect(item.check({ ...empty, runtime: true, accounts: 0 })).toBe(false);
    expect(item.check({ ...filled, accounts: null })).toBeNull();
  });
  it('requires two active users and all employee bindings', () => {
    expect(setupItems.find(i => i.id === 'users')!.check({ ...filled, users: filled.users!.map(u => ({ ...u, status: 'suspended' })) })).toBe(false);
    expect(setupItems.find(i => i.id === 'permissions')!.check({ ...filled, users: [...filled.users!, { role: 'employee', status: 'active', bindings: [] }] })).toBe(false);
  });
  it('excludes optional items from progress and unknown items from completion', () => {
    expect(setupProgress({ ...filled, google: false, odoo: null, routines: 0 })).toEqual({ done: 9, total: 9 });
    expect(setupProgress({ ...filled, tasks: null })).toEqual({ done: 8, total: 9 });
  });
  it('turns malformed responses into unknown rather than rejecting the load', async () => {
    vi.mocked(client.call).mockResolvedValue({});
    const data = await loadSetupData();
    for (const item of setupItems) expect(item.check(data)).toBeNull();
    expect(setupProgress(data)).toEqual({ done: 0, total: 9 });
  });
  it('reports every failed read as unknown', async () => {
    const fail = () => Promise.reject(new Error('offline'));
    vi.mocked(client.call).mockImplementation(fail);
    const data = await loadSetupData();
    for (const item of setupItems) expect(item.check(data)).toBeNull();
    expect(setupProgress(data)).toEqual({ done: 0, total: 9 });
  });
  it('publishes each row as it resolves and uses 10 second timeouts', async () => {
    const pending = new Map<string, (value: unknown) => void>();
    vi.mocked(client.call).mockImplementation((method, _params, _skip, timeout) => {
      expect(timeout).toBe(10_000);
      return new Promise(resolve => pending.set(method, resolve));
    });
    const updates: Array<[keyof SetupData, unknown]> = [];
    const loading = loadSetupData((key, value) => updates.push([key, value]));
    pending.get('accounts.list')!({ accounts: [{}] });
    await vi.waitFor(() => expect(updates).toContainEqual(['accounts', 1]));
    expect(updates).toHaveLength(1);
    for (const resolve of pending.values()) resolve({});
    await loading;
  });
});
