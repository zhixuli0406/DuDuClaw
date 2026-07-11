import { useMemo } from 'react';
import type { AgentDetail } from '@/lib/api';
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

/**
 * The AI staff the current viewer should see. For `own`, filters to bound
 * agents. For `all`/`reports`, trusts the (backend-scoped) `agents.list`
 * result as-is.
 *
 * NOTE (DEGRADED): the `reports` subtree is computed server-side from the
 * person org (`manager_id`, WP11-T11.0) which is not yet wired, so a manager
 * currently sees whatever `agents.list` returns. The employee `own` path is
 * fully enforced client-side here.
 */
export function useVisibleAgents(): ReadonlyArray<AgentDetail> {
  const scope = useDataScope();
  const agents = useAgentsStore((s) => s.agents);
  const bindings = useAuthStore((s) => s.bindings);
  return useMemo(() => {
    if (scope === 'own') {
      const owned = ownedAgentNames(bindings);
      return agents.filter((a) => owned.has(a.name));
    }
    return agents;
  }, [scope, agents, bindings]);
}
