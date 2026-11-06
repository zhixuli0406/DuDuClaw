import '@/test/mocks';
import type { CallableAgent } from '@/lib/api';
import { callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';

/** A completed gateway response for component tests that start after authorization.
 * Initial-load, failure and demotion behavior is covered by the store/scope tests. */
export function setConfirmedCallableAgents(unrestricted: boolean, agents: readonly CallableAgent[] = [], sessionId: string | null = null) {
  const key = JSON.stringify([callableSubject(), sessionId]);
  useCallableAgentsStore.setState({ entries: { [key]: { agents, unrestricted, loading: false } } });
}
