import { describe, it, expect, vi, afterEach, beforeEach } from 'vitest';
import { api, type SystemStatus } from '@/lib/api';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from './system-store';

const statusOf = (version: string) => ({ version, edition_profile: 'enterprise' }) as unknown as SystemStatus;

beforeEach(() => {
  useConnectionStore.setState({ state: 'authenticated' });
  useSystemStore.setState({ status: statusOf('1.70.1'), loading: false, error: null });
});
afterEach(() => vi.restoreAllMocks());

describe('system status across gateway restarts', () => {
  it('refetches when the WebSocket reconnects and shows the new gateway version', async () => {
    const status = vi.spyOn(api.system, 'status').mockResolvedValue(statusOf('1.76.2'));
    useConnectionStore.setState({ state: 'connecting' });
    // The previous status stays while reconnecting, so edition gating holds.
    expect(useSystemStore.getState().status?.version).toBe('1.70.1');
    useConnectionStore.setState({ state: 'authenticated' });
    await vi.waitFor(() => expect(useSystemStore.getState().status?.version).toBe('1.76.2'));
    expect(status).toHaveBeenCalledTimes(1);
  });

  it('does not refetch on updates that keep the connection state', () => {
    const status = vi.spyOn(api.system, 'status').mockResolvedValue(statusOf('1.76.2'));
    useConnectionStore.setState({ state: 'authenticated', error: null });
    useConnectionStore.setState({ state: 'connecting' });
    expect(status).not.toHaveBeenCalled();
  });

  it('ignores a late answer from the gateway that served before the restart', async () => {
    const resolvers: Array<(s: SystemStatus) => void> = [];
    vi.spyOn(api.system, 'status').mockImplementation(
      () => new Promise<SystemStatus>((resolve) => { resolvers.push(resolve); }),
    );
    const first = useSystemStore.getState().fetchStatus();
    const second = useSystemStore.getState().fetchStatus();
    resolvers[1](statusOf('1.76.2'));
    await second;
    resolvers[0](statusOf('1.76.1'));
    await first;
    expect(useSystemStore.getState().status?.version).toBe('1.76.2');
    expect(useSystemStore.getState().loading).toBe(false);
  });
});
