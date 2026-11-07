import { mockWsClient } from '@/test/mocks';
import { vi } from 'vitest';
import { api } from '@/lib/api';
import type { CallableAgent } from '@/lib/api';
import { callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';

/** A completed gateway response for component tests that start after authorization.
 * Initial-load, failure and demotion behavior is covered by the store/scope tests. */
export function setConfirmedCallableAgents(unrestricted: boolean, agents: readonly CallableAgent[] = [], sessionId: string | null = null) {
  vi.spyOn(api.agents, 'callable').mockImplementation(async session => {
    const response = await mockWsClient.call('agents.callable', session ? { session_id: session } : {});
    if (Array.isArray(response?.agents) && (typeof response.unrestricted === 'boolean' || response.agents.some((a: CallableAgent) => typeof a.selectable === 'boolean'))) return response;
    return { agents: [...agents], unrestricted, default_agent: null };
  });
  const key = JSON.stringify([callableSubject(), sessionId]);
  useCallableAgentsStore.setState({ entries: { [key]: { agents, unrestricted, loading: false, confirmedAt: Date.now() } } });
}
