import { mockWsClient } from '@/test/mocks';
import { act, fireEvent, renderHook } from '@testing-library/react';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useSelectableAgents, useVisibleAgents } from '@/lib/data-scope';
import { useAgentsStore } from './agents-store';
import { useAuthStore } from './auth-store';
import { CALLABLE_TIMEOUT_MS, canSelectAgent, CALLABLE_REFRESH_INTERVAL_MS, useCallableAgents, useCallableAgentsStore } from './callable-agents-store';

beforeEach(() => {
  vi.restoreAllMocks();
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

it('retains display grants but empties selection during refresh and clears both on failure', async () => {
  useAgentsStore.setState({ agents: [{ name: 'mine' }] as never });
  const { result } = renderHook(() => ({ ...useCallableAgents(), display: useVisibleAgents(), selection: useSelectableAgents() }));
  await act(async () => {});
  const confirmed = result.current.agents;
  let reject!: (error: Error) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise((_, no) => { reject = no; }));
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_REFRESH_INTERVAL_MS); });
  expect(result.current.loading).toBe(true);
  expect(result.current.agents).toBe(confirmed);
  expect(result.current.unrestricted).toBe(true);
  expect(result.current.display.map(a => a.name)).toEqual(['mine']);
  expect(result.current.selection).toEqual([]);
  expect(canSelectAgent('mine')).toBe(false);
  await act(async () => { reject(new Error('revoked')); });
  expect(result.current).toMatchObject({ agents: null, loading: false, unrestricted: false, display: [], selection: [] });
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

it('requires a response after activation even for previously unrestricted grants, including demotion', async () => {
  useAgentsStore.setState({ agents: [{ name: 'old' }, { name: 'mine' }] as never });
  const { result, rerender } = renderHook(({ open }) => ({ display: useVisibleAgents(), selection: useSelectableAgents(true, undefined, open) }), { initialProps: { open: false } });
  await act(async () => {});
  let resolve!: (value: unknown) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise(yes => { resolve = yes; }));
  rerender({ open: true });
  expect(result.current.selection).toEqual([]);
  expect(result.current.display.map(a => a.name)).toContain('old');
  expect(canSelectAgent('old')).toBe(false);
  await act(async () => { resolve({ agents: [{ name: 'mine', via: 'binding', selectable: true }], unrestricted: false }); });
  expect(result.current.selection.map(a => a.name)).toEqual(['mine']);
  expect(canSelectAgent('old')).toBe(false);
  expect(canSelectAgent('mine')).toBe(true);
});

it('clears display and selection on timeout and ignores the late response', async () => {
  useAgentsStore.setState({ agents: [{ name: 'mine' }] as never });
  const { result } = renderHook(() => ({ display: useVisibleAgents(), selection: useSelectableAgents() }));
  await act(async () => {});
  let resolve!: (value: unknown) => void;
  mockWsClient.call.mockImplementationOnce(() => new Promise(yes => { resolve = yes; }));
  act(() => { void useCallableAgentsStore.getState().load(Object.keys(useCallableAgentsStore.getState().entries)[0], null, true); });
  await act(async () => { await vi.advanceTimersByTimeAsync(CALLABLE_TIMEOUT_MS); });
  expect(result.current).toEqual({ display: [], selection: [] });
  await act(async () => { resolve({ agents: [], unrestricted: true }); });
  expect(result.current).toEqual({ display: [], selection: [] });
});
