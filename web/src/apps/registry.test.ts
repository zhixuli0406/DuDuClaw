import { describe, it, expect } from 'vitest';
import { isNewFeature as isNewFeatureFromNavModel } from '@/components/layout/nav-model';
import { APPS, APPS_BY_ID, isAppVisible, visibleApps, isNewFeature, type AppId } from './registry';

const EXPECTED_IDS: AppId[] = ['system', 'workbench', 'staff', 'comms', 'memory', 'files', 'monitor'];

describe('apps/registry — the seven L3 apps (§2 L3)', () => {
  it('has exactly the seven apps the design doc table lists, in a stable order', () => {
    expect(APPS.map((a) => a.id)).toEqual(EXPECTED_IDS);
  });

  it('every app has a unique id, a routePrefix of the form /app/<id>, and non-empty i18n keys', () => {
    const seen = new Set<string>();
    for (const app of APPS) {
      expect(seen.has(app.id)).toBe(false);
      seen.add(app.id);
      expect(app.routePrefix).toBe(`/app/${app.id}`);
      expect(app.nameId).toBe(`app.${app.id}.name`);
      expect(app.descId).toBe(`app.${app.id}.desc`);
      expect(app.defaultPath.startsWith('/')).toBe(true);
      // None of the seven are appliance-only yet (see the field's doc comment).
      expect(app.applianceOnly).toBe(false);
    }
  });

  it('APPS_BY_ID is a faithful index of APPS (no hand-maintained drift)', () => {
    expect(Object.keys(APPS_BY_ID).sort()).toEqual([...EXPECTED_IDS].sort());
    for (const app of APPS) {
      expect(APPS_BY_ID[app.id]).toBe(app); // same object, not a copy
    }
  });
});

describe('apps/registry — isAppVisible / visibleApps (role gate, fail-closed like nav-visibility.ts)', () => {
  it('an app with no minRole is visible to every authenticated role', () => {
    const workbench = APPS_BY_ID.workbench;
    expect(workbench.minRole).toBeUndefined();
    expect(isAppVisible(workbench, 'employee')).toBe(true);
    expect(isAppVisible(workbench, 'manager')).toBe(true);
    expect(isAppVisible(workbench, 'admin')).toBe(true);
  });

  it('an admin-gated app is hidden from employee/manager and visible to admin', () => {
    const system = APPS_BY_ID.system;
    expect(system.minRole).toBe('admin');
    expect(isAppVisible(system, 'employee')).toBe(false);
    expect(isAppVisible(system, 'manager')).toBe(false);
    expect(isAppVisible(system, 'admin')).toBe(true);
  });

  it('a manager-gated app is visible to manager and admin, hidden from employee', () => {
    const monitor = APPS_BY_ID.monitor;
    expect(monitor.minRole).toBe('manager');
    expect(isAppVisible(monitor, 'employee')).toBe(false);
    expect(isAppVisible(monitor, 'manager')).toBe(true);
    expect(isAppVisible(monitor, 'admin')).toBe(true);
  });

  it('fails closed when the role is unknown, exactly like nav-visibility.ts::isVisible', () => {
    expect(isAppVisible(APPS_BY_ID.system, undefined)).toBe(false);
    // Ungated apps stay visible even with an unknown role — same as hasMinRole(undefined, undefined).
    expect(isAppVisible(APPS_BY_ID.workbench, undefined)).toBe(true);
  });

  it('visibleApps filters the whole registry consistently with isAppVisible', () => {
    const forEmployee = visibleApps('employee').map((a) => a.id);
    expect(forEmployee).toContain('workbench');
    expect(forEmployee).toContain('staff');
    expect(forEmployee).toContain('memory');
    expect(forEmployee).toContain('files');
    expect(forEmployee).not.toContain('system');
    expect(forEmployee).not.toContain('comms');
    expect(forEmployee).not.toContain('monitor');

    const forAdmin = visibleApps('admin').map((a) => a.id);
    expect(forAdmin).toEqual(EXPECTED_IDS);
  });
});

describe('apps/registry — isNewFeature is the single source nav-model.ts re-exports (§3 C8)', () => {
  it('nav-model.ts::isNewFeature resolves to literally the same function', () => {
    expect(isNewFeatureFromNavModel).toBe(isNewFeature);
  });

  it('still behaves as documented (mirrors the AppSidebar.test.tsx cases for the shared impl)', () => {
    expect(isNewFeature('1.58.0', '1.57.0')).toBe(true);
    expect(isNewFeature('1.58.0', '1.59.0')).toBe(false);
    expect(isNewFeature(undefined, '1.57.0')).toBe(false);
  });
});
