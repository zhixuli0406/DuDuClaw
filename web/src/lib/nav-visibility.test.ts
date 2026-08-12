import { describe, it, expect } from 'vitest';
import { isVisible, filterVisible, ORG_CHART_MIN_AGENTS, type Gated } from './nav-visibility';

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

  // D6 (09-edition-split-features.md §4) — the org chart's progressive
  // disclosure gate, same mechanism as `/forks`'s `requiresData: 'forks'`.
  it('requiresData "org" fails closed below ORG_CHART_MIN_AGENTS, on every edition', () => {
    const org: Gated = { requiresData: 'org' };
    expect(isVisible(org, 'admin', false)).toBe(false); // no ctx at all
    expect(isVisible(org, 'admin', false, { agentCount: 0 })).toBe(false);
    expect(isVisible(org, 'admin', false, { agentCount: ORG_CHART_MIN_AGENTS - 1 })).toBe(false);
    expect(isVisible(org, 'admin', false, { agentCount: ORG_CHART_MIN_AGENTS })).toBe(true);
    expect(isVisible(org, 'admin', false, { agentCount: ORG_CHART_MIN_AGENTS + 5 })).toBe(true);
    // Personal is gated by the same threshold, not by edition.
    expect(isVisible(org, 'admin', true, { agentCount: ORG_CHART_MIN_AGENTS })).toBe(true);
    expect(isVisible(org, 'admin', true, { agentCount: 1 })).toBe(false);
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
