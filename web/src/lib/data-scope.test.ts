import { act, renderHook, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';
import { useCallableAgentsStore } from '@/stores/callable-agents-store';
import type { AgentDetail, CallableAgent } from '@/lib/api';
import { describe, it, expect, beforeEach } from 'vitest';
import { scopeForRole, ownedAgentNames, useVisibleAgents } from './data-scope';

const roster = ['mine', 'other', 'main'].map(name => ({ name, display_name: name, role: name === 'main' ? 'main' : 'worker', status: 'active' })) as AgentDetail[];
const grant = (name: string, extra = {}): CallableAgent => ({ name, display_name: name, role: name === 'main' ? 'main' : 'worker', status: 'active', via: 'binding', selectable: true, assignable: true, level: 'operator', call_text: `@${name}`, ...extra });
beforeEach(() => {
  useAuthStore.setState({ user: { id: 'u', role: 'employee' } as never, bindings: [] });
  useSystemStore.setState({ status: null });
  useCallableAgentsStore.setState({ entries: {} });
  useAgentsStore.setState({ agents: roster });
  mockWsClient.call.mockImplementation(async () => ({ agents: [grant('mine'), grant('main', { selectable: false }), grant('public', { via: 'public', selectable: false }), grant('team', { via: 'team', via_user_name: 'Report' })] }));
});
it('maps scope and direct binding names', () => {
  expect(['admin', 'manager', 'employee', undefined].map(r => scopeForRole(r as never))).toEqual(['all', 'reports', 'own', 'own']);
  expect(ownedAgentNames([{ agent_name: 'mine' } as never])).toEqual(new Set(['mine']));
});
for (const role of ['employee', 'manager']) describe(role, () => {
  beforeEach(() => useAuthStore.setState({ user: { id: 'u', role } as never }));
  it('fails closed while loading, then uses only selectable grants, with team display data only', async () => {
    const { result } = renderHook(() => useVisibleAgents());
    expect(result.current).toEqual([]);
    await waitFor(() => expect(result.current.map(a => a.name)).toEqual(['mine', 'team']));
    expect(result.current[1]).toMatchObject({ readOnly: true, teamMember: 'Report' });
    expect(result.current[1]).not.toHaveProperty('model');
    expect(result.current[1]).not.toHaveProperty('budget');
  });
  it('keeps file/settings pickers direct-only', async () => {
    const { result } = renderHook(() => useVisibleAgents(false));
    await waitFor(() => expect(result.current).toEqual([roster[0]]));
  });
  it('shows no agents on callable failure', async () => {
    mockWsClient.call.mockRejectedValue(new Error('offline'));
    const { result } = renderHook(() => useVisibleAgents());
    await waitFor(() => expect(Object.values(useCallableAgentsStore.getState().entries)[0]?.loading).toBe(false));
    expect(result.current).toEqual([]);
  });
});
it.each(['admin', 'personal'])('%s keeps the full roster even on failure', async mode => {
  if (mode === 'admin') useAuthStore.setState({ user: { id: 'a', role: 'admin' } as never });
  else useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
  mockWsClient.call.mockRejectedValue(new Error('offline'));
  const { result } = renderHook(() => useVisibleAgents());
  expect(result.current).toEqual(roster);
  await act(async () => {});
});
it('accepts main only with an explicit selectable grant', async () => {
  mockWsClient.call.mockResolvedValue({ agents: [grant('main')] });
  const { result } = renderHook(() => useVisibleAgents());
  await waitFor(() => expect(result.current.map(a => a.name)).toEqual(['main']));
});
it('ignores an old account response and deduplicates concurrent consumers', async () => {
  let resolve!: (value: unknown) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise(r => { resolve = r; }));
  const { result } = renderHook(() => [useVisibleAgents(), useVisibleAgents()]);
  act(() => useAuthStore.setState({ user: { id: 'new', role: 'employee' } as never }));
  await act(async () => resolve({ agents: [grant('other')] }));
  await waitFor(() => expect(result.current[0].map(a => a.name)).toEqual(['mine', 'team']));
  expect(result.current[1]).toEqual(result.current[0]);
});
it('drops stale grants during refresh and after failure', async () => {
  const { result } = renderHook(() => useVisibleAgents());
  await waitFor(() => expect(result.current.length).toBe(2));
  mockWsClient.call.mockRejectedValue(new Error('revoked'));
  act(() => window.dispatchEvent(new Event('focus')));
  expect(result.current).toEqual([]);
  await act(async () => {});
  expect(result.current).toEqual([]);
});
