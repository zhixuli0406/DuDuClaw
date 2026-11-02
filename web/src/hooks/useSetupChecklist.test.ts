import { renderHook, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
const state = vi.hoisted(() => ({
  role: 'admin', profile: 'enterprise' as string | undefined,
  load: vi.fn().mockResolvedValue({ tasks: 0 }),
}));
vi.mock('@/stores/auth-store', () => ({ useAuthStore: (selector: (s: unknown) => unknown) => selector({ user: { id: 'user', role: state.role } }) }));
vi.mock('@/stores/system-store', () => ({ useSystemStore: (selector: (s: unknown) => unknown) => selector({ status: { edition_profile: state.profile } }) }));
vi.mock('@/lib/setup-checklist', () => ({ loadSetupData: state.load }));
import { useSetupChecklist } from './useSetupChecklist';

describe('setup read authorization', () => {
  beforeEach(() => { state.load.mockClear(); state.role = 'admin'; state.profile = 'enterprise'; });
  it.each(['employee', 'manager'])('does not read for %s', role => {
    state.role = role;
    const { result } = renderHook(() => useSetupChecklist());
    expect(result.current.allowed).toBe(false);
    expect(state.load).not.toHaveBeenCalled();
  });
  it.each(['personal', undefined])('does not read for profile %s', profile => {
    state.profile = profile;
    const { result } = renderHook(() => useSetupChecklist());
    expect(result.current.allowed).toBe(false);
    expect(state.load).not.toHaveBeenCalled();
  });
  it('reads only for an enabled Enterprise Admin', async () => {
    state.load.mockImplementation((onProgress: (key: 'tasks', value: number) => void) => {
      onProgress('tasks', 0);
      return Promise.resolve({ tasks: 0 });
    });
    const { result } = renderHook(() => useSetupChecklist());
    await waitFor(() => expect(result.current.data?.tasks).toBe(0));
    expect(state.load).toHaveBeenCalledTimes(1);
  });
  it('merges per-row progress and ignores late progress after cleanup', async () => {
    let progress: ((key: 'tasks', value: number) => void) | undefined;
    state.load.mockImplementation((onProgress: typeof progress) => { progress = onProgress; return new Promise(() => {}); });
    const { result, unmount } = renderHook(() => useSetupChecklist());
    await waitFor(() => expect(state.load).toHaveBeenCalled());
    progress?.('tasks', 2);
    await waitFor(() => expect(result.current.data?.tasks).toBe(2));
    expect(result.current.data?.accounts).toBeUndefined();
    unmount();
    progress?.('tasks', 3);
    expect(result.current.data?.tasks).toBe(2);
  });
  it('does not read for a dismissed card', () => {
    renderHook(() => useSetupChecklist(false));
    expect(state.load).not.toHaveBeenCalled();
  });
});
