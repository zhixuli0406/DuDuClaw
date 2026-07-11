import { describe, it, expect } from 'vitest';
import { isVisible, filterVisible, type Gated } from './nav-visibility';

describe('nav-visibility (dashboard-redesign WP11-T11.1)', () => {
  it('role gating: minRole is honoured', () => {
    const adminOnly: Gated = { minRole: 'admin' };
    expect(isVisible(adminOnly, 'admin', false)).toBe(true);
    expect(isVisible(adminOnly, 'manager', false)).toBe(false);
    expect(isVisible(adminOnly, 'employee', false)).toBe(false);
    expect(isVisible({}, 'employee', false)).toBe(true); // no gate → everyone
  });

  it('enterprise surfaces hide on the personal edition', () => {
    const ent: Gated = { enterprise: true };
    expect(isVisible(ent, 'admin', false)).toBe(true);
    expect(isVisible(ent, 'admin', true)).toBe(false);
  });

  it('ownScope never gates visibility (data-scope hint only)', () => {
    const own: Gated = { ownScope: true };
    expect(isVisible(own, 'employee', false)).toBe(true);
  });

  it('operatorOnly fails closed without proven operator access', () => {
    const op: Gated = { operatorOnly: true };
    // No context → hidden (fail-closed).
    expect(isVisible(op, 'admin', false)).toBe(false);
    // Explicitly no operator access → hidden.
    expect(isVisible(op, 'admin', false, { hasOperatorAccess: false })).toBe(false);
    // Proven operator access → visible.
    expect(isVisible(op, 'admin', false, { hasOperatorAccess: true })).toBe(true);
  });

  it('filterVisible applies every gate together', () => {
    const items: Gated[] = [
      { minRole: 'admin' },
      { enterprise: true },
      { operatorOnly: true },
      {},
    ];
    const asManagerPersonal = filterVisible(items, 'manager', true);
    // admin-only dropped, enterprise dropped (personal), operatorOnly dropped
    // (no ctx), open item kept.
    expect(asManagerPersonal).toEqual([{}]);
  });
});
