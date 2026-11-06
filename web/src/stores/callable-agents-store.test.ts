import { act, fireEvent, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from './auth-store';
import { CALLABLE_REFRESH_INTERVAL_MS, useCallableAgents, useCallableAgentsStore } from './callable-agents-store';

beforeEach(() => {
  vi.useFakeTimers();
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  useAuthStore.setState({ user: { id: 'viewer', role: 'admin' } as never, bindings: [] });
  mockWsClient.call.mockReset();
  mockWsClient.call.mockResolvedValue({ agents: [{ name: 'mine', selectable: true, via: 'binding' }], unrestricted: true });
});
afterEach(() => {
  useAuthStore.setState({ user: null });
  vi.restoreAllMocks();
  vi.useRealTimers();
});

it('refreshes every 60 seconds while visible, pauses hidden, and refreshes on return', async () => {
  const { result } = renderHook(() => useCallableAgents());
  expect(result.current.unrestricted).toBe(false);
  await act(async () => {});
  expect(result.current.unrestricted).toBe(true);
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS); });
  expect(mockWsClient.call).toHaveBeenCalledTimes(2);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('hidden');
  act(() => document.dispatchEvent(new Event('visibilitychange')));
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS * 3); });
  expect(mockWsClient.call).toHaveBeenCalledTimes(2);
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  await act(async () => { document.dispatchEvent(new Event('visibilitychange')); });
  expect(mockWsClient.call).toHaveBeenCalledTimes(3);
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS); });
  expect(mockWsClient.call).toHaveBeenCalledTimes(4);
});

it('retains same-identity confirmed grants during refresh, then drops all grants on failure', async () => {
  const { result } = renderHook(() => useCallableAgents());
  await act(async () => {});
  const confirmed = result.current.agents;
  let reject!: (error: Error) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise((_, no) => { reject = no; }));
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS); });
  expect(result.current.loading).toBe(true);
  expect(result.current.agents).toBe(confirmed);
  expect(result.current.unrestricted).toBe(true);
  await act(async () => { reject(new Error('revoked')); });
  expect(result.current).toMatchObject({ agents: null, loading: false, unrestricted: false });
});

it('refreshes when a picker opens by pointer or keyboard', async () => {
  renderHook(() => useCallableAgents());
  await act(async () => {});
  const picker = document.createElement('button');
  picker.setAttribute('aria-haspopup', 'menu');
  document.body.append(picker);
  try {
    await act(async () => { fireEvent.pointerDown(picker); });
    expect(mockWsClient.call).toHaveBeenCalledTimes(2);
    await act(async () => { fireEvent.keyDown(picker, { key: 'ArrowDown' }); });
    expect(mockWsClient.call).toHaveBeenCalledTimes(3);
  } finally { picker.remove(); }
});

it('never restores unrestricted access from an old identity response', async () => {
  let resolve!: (value: unknown) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise(yes => { resolve = yes; }));
  const { result } = renderHook(() => useCallableAgents());
  mockWsClient.call.mockResolvedValue({ agents: [], unrestricted: false });
  await act(async () => { useAuthStore.setState({ user: { id: 'next', role: 'employee' } as never }); });
  await act(async () => { resolve({ agents: [{ name: 'old' }], unrestricted: true }); });
  expect(result.current).toMatchObject({ agents: [], unrestricted: false });
  expect(Object.values(useCallableAgentsStore.getState().entries)).toHaveLength(1);
});
