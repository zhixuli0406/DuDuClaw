import { useMemo } from 'react';
import type { AgentDetail, AgentInfo, CallableAgent } from '@/lib/api';
import type { UserRole, AgentBinding } from '@/stores/auth-store';
import { useAuthStore } from '@/stores/auth-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useCallableAgents } from '@/stores/callable-agents-store';

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

/** Full-roster visibility must be confirmed by the gateway's live identity. */
export function useAgentVisibilityUnrestricted(sessionId?: string | null): boolean {
  return useCallableAgents(sessionId).unrestricted;
}

export function useVisibleAgents(includeTeam: false, sessionId?: string | null): ReadonlyArray<AgentDetail>;
export function useVisibleAgents(includeTeam?: true, sessionId?: string | null): ReadonlyArray<VisibleAgent>;
export function useVisibleAgents(includeTeam = true, sessionId?: string | null): ReadonlyArray<VisibleAgent> {
  const agents = useAgentsStore((s) => s.agents);
  const { agents: callable, unrestricted } = useCallableAgents(sessionId);
  return useMemo(() => selectVisibleAgents(agents, callable, unrestricted, includeTeam),
    [unrestricted, agents, callable, includeTeam]);
}

/** Shared projection for React pickers and the read-only custom-widget bridge. */
export function selectVisibleAgents(
  agents: readonly AgentDetail[], callable: readonly CallableAgent[] | null,
  unrestricted: boolean, includeTeam = true,
): readonly VisibleAgent[] {
  if (unrestricted) return agents;
  // Only an explicit selectable grant may expose an employee, including main.
  const allowed = (callable ?? []).filter(a => a.selectable === true);
  const directIds = new Set(allowed.filter(a => a.via === 'binding').map(a => a.name));
  const direct = agents.filter(a => directIds.has(a.name));
  if (!includeTeam) return direct;
  const detailed = new Set(direct.map(a => a.name));
  const display: VisibleAgent[] = allowed.filter(a => !detailed.has(a.name)).map(a => ({
    name: a.name, display_name: a.display_name, department: a.department,
    role: a.role === 'main' || a.role === 'specialist' ? a.role : 'worker',
    status: a.status === 'active' || a.status === 'paused' ? a.status : 'terminated',
    trigger: a.trigger ?? '', icon: a.icon ?? '', reports_to: '', has_avatar: a.has_avatar,
    readOnly: true, teamMember: a.via_user_name || a.via_user || '',
  }));
  return [...direct, ...display];
}
