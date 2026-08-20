import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';

// `useIsAppliance` keeps module-level cache state (`knownResult`/`inflight`)
// so results survive across consumers within a session. Each test needs a
// clean slate, so we `vi.resetModules()` + dynamically re-import the hook
// per test rather than a single static import (see beforeEach below).
const statusMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      status: () => statusMock(),
    },
  },
}));

beforeEach(() => {
  vi.resetModules();
  statusMock.mockReset();
});

describe('useIsAppliance — enabled gate', () => {
  it('skips the RPC entirely and returns false when disabled', async () => {
    const { useIsAppliance } = await import('./useIsAppliance');
    const { result } = renderHook(() => useIsAppliance(false));
    expect(result.current).toBe(false);
    expect(statusMock).not.toHaveBeenCalled();
  });
});

describe('useIsAppliance — reads system.status is_appliance (R2: no admin gate)', () => {
  it('resolves true from a non-admin-shaped RPC response (the hook itself carries no role check)', async () => {
    statusMock.mockResolvedValue({ is_appliance: true });
    const { useIsAppliance } = await import('./useIsAppliance');
    const { result } = renderHook(() => useIsAppliance(true));

    await waitFor(() => expect(result.current).toBe(true));
    expect(statusMock).toHaveBeenCalledTimes(1);
  });

  it('stays false when the gateway reports is_appliance: false', async () => {
    statusMock.mockResolvedValue({ is_appliance: false });
    const { useIsAppliance } = await import('./useIsAppliance');
    const { result } = renderHook(() => useIsAppliance(true));

    // Let the resolved promise flush.
    await waitFor(() => expect(statusMock).toHaveBeenCalledTimes(1));
    expect(result.current).toBe(false);
  });

  it('treats a missing is_appliance field (older gateway) as false', async () => {
    statusMock.mockResolvedValue({ version: '1.0.0' });
    const { useIsAppliance } = await import('./useIsAppliance');
    const { result } = renderHook(() => useIsAppliance(true));

    await waitFor(() => expect(statusMock).toHaveBeenCalledTimes(1));
    expect(result.current).toBe(false);
  });
});

describe('useIsAppliance — cache semantics', () => {
  it('caches a definitive negative result (no RPC on the next mount)', async () => {
    statusMock.mockResolvedValue({ is_appliance: false });
    const { useIsAppliance } = await import('./useIsAppliance');

    const first = renderHook(() => useIsAppliance(true));
    await waitFor(() => expect(statusMock).toHaveBeenCalledTimes(1));
    expect(first.result.current).toBe(false);

    // A second consumer mounting later must not re-fetch — the negative
    // result is cached for the whole session (appliance identity is baked
    // into the image, never flips at runtime).
    const second = renderHook(() => useIsAppliance(true));
    expect(second.result.current).toBe(false);
    expect(statusMock).toHaveBeenCalledTimes(1);
  });

  it('caches a positive result across mounts', async () => {
    statusMock.mockResolvedValue({ is_appliance: true });
    const { useIsAppliance } = await import('./useIsAppliance');

    const first = renderHook(() => useIsAppliance(true));
    await waitFor(() => expect(first.result.current).toBe(true));

    const second = renderHook(() => useIsAppliance(true));
    expect(second.result.current).toBe(true);
    expect(statusMock).toHaveBeenCalledTimes(1);
  });

  it('fails closed (false) on RPC error, WITHOUT caching — the next mount retries', async () => {
    statusMock.mockRejectedValueOnce(new Error('network blip'));
    const { useIsAppliance } = await import('./useIsAppliance');

    const first = renderHook(() => useIsAppliance(true));
    await waitFor(() => expect(statusMock).toHaveBeenCalledTimes(1));
    expect(first.result.current).toBe(false);

    // Uncached failure → the next mount retries the RPC, and this time it
    // succeeds.
    statusMock.mockResolvedValueOnce({ is_appliance: true });
    const second = renderHook(() => useIsAppliance(true));
    await waitFor(() => expect(second.result.current).toBe(true));
    expect(statusMock).toHaveBeenCalledTimes(2);
  });

  it('dedupes concurrent consumers into a single in-flight request', async () => {
    let resolve!: (v: { is_appliance: boolean }) => void;
    statusMock.mockReturnValue(
      new Promise((r) => {
        resolve = r;
      }),
    );
    const { useIsAppliance } = await import('./useIsAppliance');

    const a = renderHook(() => useIsAppliance(true));
    const b = renderHook(() => useIsAppliance(true));
    expect(statusMock).toHaveBeenCalledTimes(1);

    resolve({ is_appliance: true });
    await waitFor(() => expect(a.result.current).toBe(true));
    await waitFor(() => expect(b.result.current).toBe(true));
  });
});
