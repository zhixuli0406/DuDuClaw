import { describe, it, expect } from 'vitest';
import { scopeForRole, ownedAgentNames } from './data-scope';
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
