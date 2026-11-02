import type { AgentBinding } from '@/stores/auth-store';

/** Current bindings are authoritative; primary preference wins within them. */
export function sortPrimaryStaff<T extends { name: string }>(agents: readonly T[], bindings: readonly AgentBinding[], primary?: string | null): T[] {
  const rank = { owner: 0, operator: 1, viewer: 2 };
  const access = new Map(bindings.map(binding => [binding.agent_name, rank[binding.access_level]]));
  return agents.filter(agent => access.has(agent.name)).slice().sort((a, b) =>
    Number(b.name === primary) - Number(a.name === primary) || access.get(a.name)! - access.get(b.name)!);
}

export function defaultAssignAgent(agents: readonly { name: string; role?: string }[], primary?: string | null): string {
  return agents.find(agent => agent.name === primary)?.name
    ?? agents.find(agent => agent.role === 'main')?.name ?? agents[0]?.name ?? '';
}
