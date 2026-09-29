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

  // WP-C (2026-08) — `/device`'s progressive-disclosure gate: hidden unless a
  // `device.status` probe confirms the running gateway IS the appliance image.
  it('requiresData "appliance" fails closed without a confirmed probe, on every edition', () => {
    const device: Gated = { requiresData: 'appliance' };
    expect(isVisible(device, 'admin', false)).toBe(false); // no ctx at all
    expect(isVisible(device, 'admin', false, { isAppliance: false })).toBe(false);
    expect(isVisible(device, 'admin', false, { isAppliance: true })).toBe(true);
    // Personal is gated by the same probe, not by edition.
    expect(isVisible(device, 'admin', true, { isAppliance: true })).toBe(true);
    expect(isVisible(device, 'admin', true, { isAppliance: false })).toBe(false);
  });

  // X1 (2026-09-29 audit) — the one deliberately fail-OPEN gate: an operator
  // kill switch read out of `system.status`. Only an explicit `false` hides the
  // surface, so a gateway too old to report the field (or a status that has not
  // landed yet) keeps showing a feature that is in fact live.
  it('requiresFeature "decision" fails OPEN and hides only on an explicit false', () => {
    const lab: Gated = { requiresFeature: 'decision' };
    expect(isVisible(lab, 'admin', false)).toBe(true); // no ctx at all
    expect(isVisible(lab, 'admin', false, {})).toBe(true); // field absent
    expect(isVisible(lab, 'admin', false, { decisionEnabled: undefined })).toBe(true);
    expect(isVisible(lab, 'admin', false, { decisionEnabled: true })).toBe(true);
    expect(isVisible(lab, 'admin', false, { decisionEnabled: false })).toBe(false);
    // Same on Personal — this is an operator preference, not an edition split.
    expect(isVisible(lab, 'admin', true, { decisionEnabled: false })).toBe(false);
    expect(isVisible(lab, 'admin', true, { decisionEnabled: true })).toBe(true);
    // An untagged item is never touched by the switch.
    expect(isVisible({}, 'admin', false, { decisionEnabled: false })).toBe(true);
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
