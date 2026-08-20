import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { appUrl, isShellMode, openApp } from './navigate';

function setShellFlag(value: boolean | undefined) {
  if (value === undefined) {
    delete (window as { __DUDUCLAW_SHELL__?: boolean }).__DUDUCLAW_SHELL__;
  } else {
    window.__DUDUCLAW_SHELL__ = value;
  }
}

describe('platform/navigate — appUrl (canonical /app/<id>/<path> format)', () => {
  it('composes the bare app-root URL with no path', () => {
    expect(appUrl('workbench')).toBe('/app/workbench');
  });

  it('composes a sub-path URL, tolerating a leading slash on the input', () => {
    expect(appUrl('workbench', '/goals')).toBe('/app/workbench/goals');
    expect(appUrl('workbench', 'goals')).toBe('/app/workbench/goals');
  });

  it('falls back to a synthesized prefix for an unknown app id (never throws)', () => {
    expect(appUrl('does-not-exist' as never)).toBe('/app/does-not-exist');
  });
});

describe('platform/navigate — isShellMode (N-2 seam)', () => {
  afterEach(() => setShellFlag(undefined));

  it('is false when the shell has not set the flag (every presentation that exists today)', () => {
    setShellFlag(undefined);
    expect(isShellMode()).toBe(false);
  });

  it('is true once the on-box shell sets window.__DUDUCLAW_SHELL__', () => {
    setShellFlag(true);
    expect(isShellMode()).toBe(true);
  });

  it('is false for any non-true value (fail-closed to SPA behavior)', () => {
    setShellFlag(false);
    expect(isShellMode()).toBe(false);
  });
});

describe('platform/navigate — openApp', () => {
  let navigateMock: ReturnType<typeof vi.fn<(path: string) => void>>;
  let windowOpenSpy: ReturnType<typeof vi.spyOn>;

  beforeEach(() => {
    navigateMock = vi.fn<(path: string) => void>();
    windowOpenSpy = vi.spyOn(window, 'open').mockImplementation(() => null);
    setShellFlag(undefined);
  });

  afterEach(() => {
    windowOpenSpy.mockRestore();
    setShellFlag(undefined);
  });

  it('SPA mode: navigates in-place to the given path, never opens a window', () => {
    openApp(navigateMock, 'workbench', '/goals');
    expect(navigateMock).toHaveBeenCalledWith('/goals');
    expect(windowOpenSpy).not.toHaveBeenCalled();
  });

  it('SPA mode: with no path, navigates to the app default path', () => {
    openApp(navigateMock, 'workbench');
    expect(navigateMock).toHaveBeenCalledWith('/chat');
  });

  it('SPA mode: tolerates a path without a leading slash', () => {
    openApp(navigateMock, 'staff', 'agents');
    expect(navigateMock).toHaveBeenCalledWith('/agents');
  });

  it('shell mode: opens a new window at the /app/<id>/<path> URL, never calls navigate', () => {
    setShellFlag(true);
    openApp(navigateMock, 'workbench', '/goals');
    expect(windowOpenSpy).toHaveBeenCalledWith('/app/workbench/goals', '_blank', 'noopener');
    expect(navigateMock).not.toHaveBeenCalled();
  });

  it('shell mode: with no path, opens the bare app-root URL', () => {
    setShellFlag(true);
    openApp(navigateMock, 'staff');
    expect(windowOpenSpy).toHaveBeenCalledWith('/app/staff', '_blank', 'noopener');
  });

  it('unknown app id is a no-op in either mode', () => {
    openApp(navigateMock, 'bogus' as never, '/x');
    expect(navigateMock).not.toHaveBeenCalled();
    expect(windowOpenSpy).not.toHaveBeenCalled();

    setShellFlag(true);
    openApp(navigateMock, 'bogus' as never, '/x');
    expect(windowOpenSpy).not.toHaveBeenCalled();
  });
});
