import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@/lib/api', async (importOriginal) => {
  const original = await importOriginal<typeof import('@/lib/api')>();
  return {
    ...original,
    api: {
      ...original.api,
      system: { ...original.api.system, status: vi.fn() },
      agents: { ...original.api.agents, list: vi.fn() },
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
