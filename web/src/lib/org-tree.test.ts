import { describe, it, expect } from 'vitest';
import {
  buildOrgTree,
  managerCandidates,
  orgErrorMessageId,
  subtreeIds,
  type OrgPerson,
} from './org-tree';

const p = (id: string, role: string, manager?: string, status = 'active'): OrgPerson => ({
  id,
  role,
  status,
  manager_user_id: manager ?? null,
});

// mgr → lead → emp1; mgr → emp2; other (alone); admin (alone)
const ORG = [
  p('admin', 'admin'),
  p('mgr', 'manager'),
  p('lead', 'manager', 'mgr'),
  p('emp1', 'employee', 'lead'),
  p('emp2', 'employee', 'mgr'),
  p('other', 'employee'),
];

describe('buildOrgTree', () => {
  it('nests people under their manager and lists the rest as unassigned', () => {
    const tree = buildOrgTree(ORG);
    expect(tree.roots.map((r) => r.person.id)).toEqual(['mgr']);
    const mgr = tree.roots[0];
    expect(mgr.reports.map((r) => r.person.id)).toEqual(['lead', 'emp2']);
    expect(mgr.reports[0].reports.map((r) => r.person.id)).toEqual(['emp1']);
    expect(tree.unassigned.map((u) => u.id)).toEqual(['admin', 'other']);
  });

  it('leaves offboarded people out and treats a gone manager as none', () => {
    const tree = buildOrgTree([...ORG.filter((u) => u.id !== 'lead'), p('lead', 'manager', 'mgr', 'offboarded')]);
    const shown: string[] = [...tree.unassigned.map((u) => u.id)];
    const walk = (n: (typeof tree.roots)[number]) => {
      shown.push(n.person.id);
      n.reports.forEach(walk);
    };
    tree.roots.forEach(walk);
    expect(shown).not.toContain('lead');
    // emp1's manager left: emp1 has no reports, so it is unassigned.
    expect(tree.unassigned.map((u) => u.id)).toContain('emp1');
  });

  it('terminates on a hand-written cycle and still shows everyone in it', () => {
    const tree = buildOrgTree([p('a', 'manager', 'b'), p('b', 'manager', 'a'), p('c', 'manager', 'c')]);
    expect(tree.roots).toEqual([]);
    expect(tree.unassigned.map((u) => u.id).sort()).toEqual(['a', 'b', 'c']);
  });

  it('caps the depth at 8 levels and lists anything deeper as unassigned', () => {
    const chain = Array.from({ length: 10 }, (_, i) => p(`u${i}`, 'manager', i === 0 ? undefined : `u${i - 1}`));
    const tree = buildOrgTree(chain);
    let depth = 0;
    let node = tree.roots[0];
    while (node) {
      depth += 1;
      node = node.reports[0];
    }
    expect(depth).toBe(8);
    expect(tree.unassigned.map((u) => u.id)).toEqual(['u8', 'u9']);
  });
});

describe('managerCandidates', () => {
  it('offers active managers and admins, never the person or anyone below them', () => {
    expect(managerCandidates(ORG, 'lead').map((u) => u.id)).toEqual(['admin', 'mgr']);
    expect(managerCandidates(ORG, 'mgr').map((u) => u.id)).toEqual(['admin']);
    expect(managerCandidates(ORG, 'emp1').map((u) => u.id)).toEqual(['admin', 'mgr', 'lead']);
  });

  it('offers every eligible manager for a person being created', () => {
    expect(managerCandidates(ORG, null).map((u) => u.id)).toEqual(['admin', 'mgr', 'lead']);
  });

  it('skips suspended managers and employees', () => {
    const org = [p('m1', 'manager', undefined, 'suspended'), p('e1', 'employee'), p('m2', 'manager')];
    expect(managerCandidates(org, null).map((u) => u.id)).toEqual(['m2']);
  });
});

describe('subtreeIds', () => {
  it('collects everyone below and stays finite on a cycle', () => {
    expect([...subtreeIds(ORG, 'mgr')].sort()).toEqual(['emp1', 'emp2', 'lead', 'mgr']);
    expect([...subtreeIds([p('a', 'manager', 'b'), p('b', 'manager', 'a')], 'a')].sort()).toEqual(['a', 'b']);
  });
});

describe('orgErrorMessageId', () => {
  it('maps the gateway codes to plain-language messages', () => {
    expect(orgErrorMessageId({ code: 'manager_cycle', message: 'x' })).toBe('users.manager.error.cycle');
    expect(orgErrorMessageId({ code: 'manager_too_deep' })).toBe('users.manager.error.tooDeep');
    expect(orgErrorMessageId({ code: 'manager_role' })).toBe('users.manager.error.role');
    expect(orgErrorMessageId({ code: 'manager_has_reports' })).toBe('users.manager.error.hasReports');
  });

  it('returns null for anything else', () => {
    expect(orgErrorMessageId(new Error('boom'))).toBeNull();
    expect(orgErrorMessageId({ code: 'enterprise_edition_required' })).toBeNull();
    expect(orgErrorMessageId('manager_cycle')).toBeNull();
    expect(orgErrorMessageId(null)).toBeNull();
  });
});
