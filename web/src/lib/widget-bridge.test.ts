import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@/lib/api', async (importOriginal) => {
  const original = await importOriginal<typeof import('@/lib/api')>();
  return {
    ...original,
    api: {
      ...original.api,
      system: { ...original.api.system, status: vi.fn() },
      agents: { ...original.api.agents, list: vi.fn(), callable: vi.fn() },
    },
  };
});

// Must import after mocking so the module picks up the mocked `api`.
const { api } = await import('@/lib/api');
const { handleWidgetMessage, clearBridgeCache, BRIDGE_METHOD_NAMES } = await import('./widget-bridge');

beforeEach(() => {
  vi.clearAllMocks();
  clearBridgeCache();
});

describe('widget-bridge cache', () => {
  it('coalesces two calls to the same method within the TTL into one API call', async () => {
    vi.mocked(api.system.status).mockResolvedValue({ version: '1.0.0' } as never);

    const [a, b] = await Promise.all([
      handleWidgetMessage({ type: 'duduclaw:rpc', seq: 1, method: 'system.status' }, []),
      handleWidgetMessage({ type: 'duduclaw:rpc', seq: 2, method: 'system.status' }, []),
    ]);

    expect(api.system.status).toHaveBeenCalledTimes(1);
    expect(a?.ok).toBe(true);
    expect(b?.ok).toBe(true);
    expect(a?.result).toEqual(b?.result);
  });

  it('does not cache a rejected call — the next request tries again', async () => {
    vi.mocked(api.system.status)
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce({ version: '2.0.0' } as never);

    const first = await handleWidgetMessage({ type: 'duduclaw:rpc', seq: 1, method: 'system.status' }, []);
    expect(first?.ok).toBe(false);
    expect(first?.error).toBe('boom');

    const second = await handleWidgetMessage({ type: 'duduclaw:rpc', seq: 2, method: 'system.status' }, []);
    expect(second?.ok).toBe(true);
    expect(api.system.status).toHaveBeenCalledTimes(2);
  });

  it('rejects a method that is not on the allowlist', async () => {
    const reply = await handleWidgetMessage(
      { type: 'duduclaw:rpc', seq: 1, method: 'agents.deleteEveryone' },
      [],
    );
    expect(reply?.ok).toBe(false);
    expect(reply?.error).toMatch(/not allowed/);
    // Sanity: the allowlist itself is non-empty and doesn't include it.
    expect(BRIDGE_METHOD_NAMES).not.toContain('agents.deleteEveryone');
  });
});

const { useAuthStore } = await import('@/stores/auth-store');
const { useSystemStore } = await import('@/stores/system-store');
const { useCallableAgentsStore } = await import('@/stores/callable-agents-store');

it.each(['employee', 'manager', 'admin', 'failure'])('filters widget agent summaries for %s', async mode => {
  useAuthStore.setState({ user: { id: mode, role: mode === 'failure' ? 'employee' : mode } as never });
  useSystemStore.setState({ status: null });
  useCallableAgentsStore.setState({ entries: {} });
  const roster = ['mine', 'main', 'other'].map(name => ({ name, display_name: name, role: name === 'main' ? 'main' : 'worker' }));
  vi.mocked(api.agents.list).mockResolvedValue({ agents: roster } as never);
  if (mode === 'failure') vi.mocked(api.agents.callable).mockRejectedValue(new Error('offline'));
  else vi.mocked(api.agents.callable).mockResolvedValue({ agents: [{ ...roster[0], selectable: true, via: 'binding' }], default_agent: null } as never);
  const result = await handleWidgetMessage({ type: 'duduclaw:rpc', seq: 1, method: 'agents.summary' }, []);
  const names = (result?.result as { agents: { name: string }[] }).agents.map(a => a.name);
  expect(names).toEqual(mode === 'admin' ? ['mine', 'main', 'other'] : mode === 'failure' ? [] : ['mine']);
  // An account transition cannot reuse the previous summary's cached projection.
  useAuthStore.setState({ user: { id: 'next', role: 'employee' } as never });
  vi.mocked(api.agents.callable).mockRejectedValue(new Error('revoked'));
  const next = await handleWidgetMessage({ type: 'duduclaw:rpc', seq: 2, method: 'agents.summary' }, []);
  expect(next?.result).toEqual({ agents: [] });
});
