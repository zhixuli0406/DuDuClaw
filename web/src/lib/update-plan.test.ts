import { describe, expect, it } from 'vitest';
import {
  compareVersions,
  planUpdate,
  rememberDesktopResume,
  RESUME_TTL_MS,
  takeDesktopResume,
  type GatewayUpdateState,
} from './update-plan';
import type { DesktopUpdateInfo } from './desktop-update';

const gateway = (over: Partial<GatewayUpdateState> = {}): GatewayUpdateState => ({
  currentVersion: '1.76.0',
  latestVersion: '1.76.0',
  available: false,
  installable: true,
  restartPendingVersion: null,
  ...over,
});

const desktop = (over: Partial<DesktopUpdateInfo> = {}): DesktopUpdateInfo => ({
  currentVersion: '1.76.0',
  stagedVersion: null,
  gateway: 'external',
  ...over,
});

describe('compareVersions', () => {
  it('compares numerically and ignores a v prefix or suffix', () => {
    expect(compareVersions('1.76.10', '1.76.9')).toBe(1);
    expect(compareVersions('v1.76.1', '1.76.1')).toBe(0);
    expect(compareVersions('1.76.1-rc.1', '1.76.1')).toBe(0);
    expect(compareVersions('1.70.1', '1.76.0')).toBe(-1);
  });
});

describe('planUpdate', () => {
  it('holds an app update that would run ahead of the gateway', () => {
    // The 2026-10-27 field case: app staged 1.76.1, gateway channel tops at 1.76.0.
    const plan = planUpdate(
      gateway({ currentVersion: '1.70.1', latestVersion: '1.76.0', available: true }),
      desktop({ stagedVersion: '1.76.1' }),
    );
    expect(plan.gatewayStep).toBe('install');
    expect(plan.gatewayVersionAfter).toBe('1.76.0');
    expect(plan.desktopStep).toBe('hold');
  });

  it('updates the gateway first, then the app up to the same version', () => {
    const plan = planUpdate(
      gateway({ latestVersion: '1.76.2', available: true }),
      desktop({ stagedVersion: '1.76.2' }),
    );
    expect(plan).toEqual({ gatewayStep: 'install', gatewayVersionAfter: '1.76.2', desktopStep: 'install' });
  });

  it('restarts onto an on-disk binary instead of downloading it again', () => {
    const plan = planUpdate(
      gateway({ currentVersion: '1.70.1', latestVersion: '1.76.0', available: true, restartPendingVersion: '1.76.0' }),
      null,
    );
    expect(plan.gatewayStep).toBe('restart');
    expect(plan.gatewayVersionAfter).toBe('1.76.0');
    expect(plan.desktopStep).toBe('none');
  });

  it('downloads when the channel is newer than the on-disk binary', () => {
    const plan = planUpdate(
      gateway({ currentVersion: '1.70.1', latestVersion: '1.76.1', available: true, restartPendingVersion: '1.76.0' }),
      null,
    );
    expect(plan.gatewayStep).toBe('install');
  });

  it('never installs where the gateway has no channel, and holds the app there', () => {
    const plan = planUpdate(
      gateway({ latestVersion: '1.76.1', available: true, installable: false }),
      desktop({ stagedVersion: '1.76.1' }),
    );
    expect(plan.gatewayStep).toBe('none');
    expect(plan.desktopStep).toBe('hold');
  });

  it('lets a bundled sidecar app or an older shell install directly', () => {
    expect(planUpdate(gateway(), desktop({ stagedVersion: '1.76.2', gateway: 'bundled' })).desktopStep).toBe('install');
    expect(planUpdate(gateway(), desktop({ stagedVersion: '1.76.2', gateway: null })).desktopStep).toBe('install');
  });
});

describe('desktop resume marker', () => {
  it('is read once', () => {
    rememberDesktopResume('1.76.2', 1_000);
    expect(takeDesktopResume(2_000)).toBe('1.76.2');
    expect(takeDesktopResume(2_000)).toBeNull();
  });

  it('expires so a restart that never happened cannot relaunch the app later', () => {
    rememberDesktopResume('1.76.2', 1_000);
    expect(takeDesktopResume(1_000 + RESUME_TTL_MS + 1)).toBeNull();
  });

  it('ignores a malformed marker', () => {
    sessionStorage.setItem('duduclaw-update-resume-desktop', '1.76.2');
    expect(takeDesktopResume()).toBeNull();
  });
});
