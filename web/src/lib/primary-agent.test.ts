import { describe, expect, it } from 'vitest';
import { defaultAssignAgent, sortPrimaryStaff } from './primary-agent';
import type { AgentBinding } from '@/stores/auth-store';

describe('primary AI staff', () => {
  const agents = [{ name: 'owner', role: 'main' }, { name: 'operator' }, { name: 'viewer' }, { name: 'unbound' }];
  const bindings = [
    { agent_name: 'owner', access_level: 'owner' },
    { agent_name: 'operator', access_level: 'operator' },
    { agent_name: 'viewer', access_level: 'viewer' },
  ] as AgentBinding[];
  it('puts the bound primary first, then binding level, excluding unbound staff', () => {
    expect(sortPrimaryStaff(agents, bindings, 'operator').map(a => a.name)).toEqual(['operator', 'owner', 'viewer']);
    expect(sortPrimaryStaff(agents, bindings, 'unbound').map(a => a.name)).toEqual(['owner', 'operator', 'viewer']);
  });
  it('ignores a primary preference after its live binding becomes Viewer', () => {
    const downgraded = bindings.map(binding => binding.agent_name === 'operator'
      ? { ...binding, access_level: 'viewer' as const } : binding);
    expect(sortPrimaryStaff(agents, downgraded, 'operator').map(a => a.name)).toEqual(['owner', 'operator', 'viewer']);
    expect(sortPrimaryStaff(agents, bindings, 'viewer').map(a => a.name)).toEqual(['owner', 'operator', 'viewer']);
  });
  it('defaults assignment to a visible primary and falls back for removed staff', () => {
    expect(defaultAssignAgent(agents, 'operator')).toBe('operator');
    expect(defaultAssignAgent(agents, 'removed')).toBe('owner');
    expect(defaultAssignAgent([], 'operator')).toBe('');
  });
});
