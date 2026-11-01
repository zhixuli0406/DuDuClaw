import { renderHook } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import type { AgentDetail, CallableAgent } from '@/lib/api';
import { describe, it, expect, beforeEach } from 'vitest';
import { scopeForRole, ownedAgentNames, useVisibleAgents } from './data-scope';
import type { AgentBinding } from '@/stores/auth-store';

describe('data-scope (dashboard-redesign WP11-T11.3)', () => {
  it('scopeForRole maps roles to breadth, fail-closed for unknown', () => {
    expect(scopeForRole('admin')).toBe('all');
    expect(scopeForRole('manager')).toBe('reports');
    expect(scopeForRole('employee')).toBe('own');
    expect(scopeForRole(undefined)).toBe('own'); // fail-closed
  });

  it('ownedAgentNames collects bound agent names', () => {
    const bindings: AgentBinding[] = [
      { user_id: 'u', agent_name: 'alice', access_level: 'owner', bound_at: '' },
      { user_id: 'u', agent_name: 'bob', access_level: 'viewer', bound_at: '' },
    ];
    const set = ownedAgentNames(bindings);
    expect(set.has('alice')).toBe(true);
    expect(set.has('bob')).toBe(true);
    expect(set.has('carol')).toBe(false);
    expect(set.size).toBe(2);
  });
});


describe('useVisibleAgents', () => {
  const direct = { name: 'mine', display_name: 'Mine' } as AgentDetail;
  const other = { name: 'other', display_name: 'Other' } as AgentDetail;
  const team = { name: 'team', display_name: 'Team', via: 'team', via_user_name: 'Report', level: 'operator' } as CallableAgent;
  beforeEach(() => {
    useAuthStore.setState({ user: { id: 'u', role: 'manager' } as never, bindings: [{ user_id: 'u', agent_name: 'mine', access_level: 'operator', bound_at: '' }] });
    useAgentsStore.setState({ agents: [direct], teamAgents: [team, { ...team, name: 'public', via: 'public' }], teamSubject: JSON.stringify(['u', 'manager']) });
  });
  it('employee sees only directly bound agents even if the detailed roster over-returns', () => {
    useAuthStore.setState({ user: { id: 'u', role: 'employee' } as never, bindings: [{ user_id: 'u', agent_name: 'mine', access_level: 'operator', bound_at: '' }] });
    useAgentsStore.setState({ agents: [direct, other] });
    const { result } = renderHook(() => useVisibleAgents());
    expect(result.current.map(a => a.name)).toEqual(['mine']);
  });
  it('manager gets only team grants as display-only entries without sensitive fields', () => {
    const { result } = renderHook(() => useVisibleAgents());
    expect(result.current.map(a => a.name)).toEqual(['mine', 'team']);
    expect(result.current[1]).toMatchObject({ readOnly: true, teamMember: 'Report' });
    expect(result.current[1]).not.toHaveProperty('budget');
    expect(result.current[1]).not.toHaveProperty('model');
  });
  it('direct-only surfaces omit team entries, duplicate direct entries are not replaced', () => {
    useAgentsStore.setState({ teamAgents: [team, { ...team, name: 'mine' }] });
    const { result } = renderHook(() => useVisibleAgents(false));
    expect(result.current).toEqual([direct]);
    const merged = renderHook(() => useVisibleAgents());
    expect(merged.result.current.filter(a => a.name === 'mine')).toEqual([direct]);
  });
  it('drops team entries when the viewer changes', () => {
    useAuthStore.setState({ user: { id: 'new', role: 'manager' } as never, bindings: [] });
    const { result } = renderHook(() => useVisibleAgents());
    expect(result.current).toEqual([]);
  });
  it('fetches detailed data directly and limits callable entries to team grants', async () => {
    mockWsClient.call.mockImplementation(async (method: string) => method === 'agents.callable' ? { agents: [team, { ...team, name: 'public', via: 'public' }] } : { agents: [direct] });
    await useAgentsStore.getState().fetchAgents();
    expect(useAgentsStore.getState().agents).toEqual([direct]);
    expect(useAgentsStore.getState().teamAgents).toEqual([team]);
  });
});
