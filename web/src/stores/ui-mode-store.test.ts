import { describe, it, expect, beforeEach, vi } from 'vitest';
import {
  useUiModeStore,
  defaultMode,
  resolveInitialMode,
  storedMode,
} from './ui-mode-store';

const KEY = 'duduclaw-ui-mode';

beforeEach(() => {
  localStorage.clear();
  // Reset store to a clean, unchosen state for each test.
  useUiModeStore.setState({ mode: 'dashboard', chosen: false });
});

describe('ui-mode-store', () => {
  it('defaultMode: personal → workspace, otherwise dashboard', () => {
    expect(defaultMode('personal')).toBe('workspace');
    expect(defaultMode('enterprise')).toBe('dashboard');
    expect(defaultMode(undefined)).toBe('dashboard');
    expect(defaultMode(null)).toBe('dashboard');
  });

  it('storedMode returns null when nothing is persisted', () => {
    expect(storedMode()).toBeNull();
  });

  it('resolveInitialMode: stored choice overrides edition default', () => {
    localStorage.setItem(KEY, 'dashboard');
    expect(resolveInitialMode('personal')).toBe('dashboard');
    localStorage.setItem(KEY, 'workspace');
    expect(resolveInitialMode('enterprise')).toBe('workspace');
  });

  it('setMode persists and marks chosen', () => {
    useUiModeStore.getState().setMode('workspace');
    expect(useUiModeStore.getState().mode).toBe('workspace');
    expect(useUiModeStore.getState().chosen).toBe(true);
    expect(localStorage.getItem(KEY)).toBe('workspace');
  });

  it('toggle flips between modes', () => {
    useUiModeStore.getState().setMode('workspace');
    useUiModeStore.getState().toggle();
    expect(useUiModeStore.getState().mode).toBe('dashboard');
    useUiModeStore.getState().toggle();
    expect(useUiModeStore.getState().mode).toBe('workspace');
  });

  it('initFromEdition seeds default only when unchosen', () => {
    // unchosen → seeds from edition
    useUiModeStore.getState().initFromEdition('personal');
    expect(useUiModeStore.getState().mode).toBe('workspace');
    expect(useUiModeStore.getState().chosen).toBe(false);

    // an explicit choice must not be overwritten by a later edition signal
    useUiModeStore.getState().setMode('dashboard');
    useUiModeStore.getState().initFromEdition('personal');
    expect(useUiModeStore.getState().mode).toBe('dashboard');
  });

  it('storedMode tolerates a throwing localStorage', () => {
    const spy = vi.spyOn(Storage.prototype, 'getItem').mockImplementation(() => {
      throw new Error('blocked');
    });
    expect(storedMode()).toBeNull();
    spy.mockRestore();
  });
});
