import { describe, expect, it } from 'vitest';
import { defaultAssignAgent, sortPrimaryStaff } from './primary-agent';
import type { AgentBinding } from '@/stores/auth-store';

describe('primary AI staff', () => {
  const agents = [{ name: 'owner', role: 'main' }, { name: 'viewer' }, { name: 'unbound' }];
  const bindings = [
    { agent_name: 'owner', access_level: 'owner' },
    { agent_name: 'viewer', access_level: 'viewer' },
  ] as AgentBinding[];
  it('puts the bound primary first, then binding level, excluding unbound staff', () => {
    expect(sortPrimaryStaff(agents, bindings, 'viewer').map(a => a.name)).toEqual(['viewer', 'owner']);
    expect(sortPrimaryStaff(agents, bindings, 'unbound').map(a => a.name)).toEqual(['owner', 'viewer']);
  });
  it('defaults assignment to a visible primary and falls back for removed staff', () => {
    expect(defaultAssignAgent(agents, 'viewer')).toBe('viewer');
    expect(defaultAssignAgent(agents, 'removed')).toBe('owner');
    expect(defaultAssignAgent([], 'viewer')).toBe('');
  });
});
