import { vi } from 'vitest';
import { api, type CallableAgent } from '@/lib/api';
import { callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';

/** Model a server response, including refreshes triggered by picker gestures. */
export function mockCallableGrants(names: string[], unrestricted = false) {
  const agents = names.map(name => ({ name, display_name: name, selectable: true,
    assignable: true, via: 'binding', level: 'owner', call_text: `@${name}` } satisfies CallableAgent));
  vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents, unrestricted, default_agent: null });
  useCallableAgentsStore.setState({ entries: {
    [JSON.stringify([callableSubject(), null])]: { agents, unrestricted, loading: false, confirmedAt: Date.now() },
  } });
}
