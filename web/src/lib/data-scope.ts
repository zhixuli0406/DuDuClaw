import { useMemo } from 'react';
import type { AgentDetail, AgentInfo } from '@/lib/api';
import type { UserRole, AgentBinding } from '@/stores/auth-store';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';

/**
 * Data-scope layer (dashboard-redesign §3.4 WP11-T11.3). Turns a viewer's role
 * into the breadth of data a page shows:
 *   - admin    → `all`     (whole company)
 *   - manager  → `reports`  (their subtree)
 *   - employee → `own`      (only their bound AI staff)
 *
 * The gateway RPC layer is the authoritative gate (fail-closed); these hooks are
 * a client-side courtesy so an employee never sees a "whole company" view even
 * if a future RPC over-returns. Pure derivation is factored into
 * `scopeForRole` for testability.
 */
export type DataScope = 'all' | 'reports' | 'own';

export function scopeForRole(role: UserRole | undefined): DataScope {
  if (role === 'admin') return 'all';
  if (role === 'manager') return 'reports';
  return 'own'; // employee or unknown → most restrictive (fail-closed)
}

/** Set of agent names the viewer is bound to (any access level). */
export function ownedAgentNames(bindings: readonly AgentBinding[]): Set<string> {
  return new Set(bindings.map((b) => b.agent_name));
}

export function useDataScope(): DataScope {
  const role = useAuthStore((s) => s.user?.role);
  return scopeForRole(role);
}

/** Display-only team entries never contain model, budget, or memory data. */
export type VisibleAgent = AgentInfo & Partial<AgentDetail> & {
  readOnly?: boolean;
  teamMember?: string;
};

export function useVisibleAgents(includeTeam: false): ReadonlyArray<AgentDetail>;
export function useVisibleAgents(includeTeam?: true): ReadonlyArray<VisibleAgent>;
export function useVisibleAgents(includeTeam = true): ReadonlyArray<VisibleAgent> {
  const scope = useDataScope();
  const agents = useAgentsStore((s) => s.agents);
  const teamAgents = useAgentsStore((s) => s.teamAgents);
  const teamSubject = useAgentsStore((s) => s.teamSubject);
  const user = useAuthStore((s) => s.user);
  const bindings = useAuthStore((s) => s.bindings);
  return useMemo(() => {
    if (scope === 'own') {
      const owned = ownedAgentNames(bindings);
      return agents.filter((a) => owned.has(a.name));
    }
    if (scope !== 'reports') return agents;
    const owned = ownedAgentNames(bindings);
    const directAgents = agents.filter(a => owned.has(a.name));
    if (!includeTeam || teamSubject !== JSON.stringify([user?.id, user?.role])) return directAgents;
    const direct = new Set(directAgents.map(a => a.name));
    const team: VisibleAgent[] = (teamAgents ?? []).filter(a => a.via === 'team' && !direct.has(a.name)).map(a => ({
      name: a.name, display_name: a.display_name, department: a.department,
      role: a.role === 'main' || a.role === 'specialist' ? a.role : 'worker',
      status: a.status === 'active' || a.status === 'paused' ? a.status : 'terminated',
      trigger: a.trigger ?? '', icon: a.icon ?? '', reports_to: '',
      readOnly: true, teamMember: a.via_user_name || a.via_user || '',
    }));
    return [...directAgents, ...team];
  }, [scope, agents, bindings, includeTeam, teamAgents, teamSubject, user?.id, user?.role]);
}
