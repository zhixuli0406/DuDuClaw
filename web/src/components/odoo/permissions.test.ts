import { describe, expect, it } from 'vitest';
import { normalizeActions, parseActions, partialRead, reconcileUnblocks } from './permissions';

describe('Odoo permission storage', () => {
  it('groups bare and model actions without losing unknown tokens', () => {
    const parsed = parseActions(['read', 'write:crm.lead', 'search:crm.lead', 'read:crm.lead', 'future:x', 'read:bad/model', 'read:', '', ' read']);
    expect(parsed.rules).toEqual([{ model: '', verbs: ['read'] }, { model: 'crm.lead', verbs: ['write', 'search', 'read'] }]);
    expect(parsed.other).toEqual(['future:x', 'read:bad/model', 'read:', '', ' read']);
    expect(partialRead(parsed.rules[0])).toBe(true);
    expect(partialRead(parsed.rules[1])).toBe(false);
  });
  it('normalizes read and search together only at serialization, keeping unknown bytes', () => {
    expect(normalizeActions(['read', 'search:hr.employee', 'execute:crm.lead', ' strange ', ''])).toEqual([
      'read', 'search', 'read:hr.employee', 'search:hr.employee', 'execute:crm.lead', ' strange ', '',
    ]);
    expect(normalizeActions([])).toEqual([]);
  });
  it('removes the sensitive grant only after its final explicit use, preserving unrelated legacy grants', () => {
    const before = { allowed_models: ['hr.employee'], allowed_actions: ['read:hr.employee'], unblock_models: ['hr.employee', 'res.partner'] };
    const next = { ...before, allowed_models: [] };
    expect(reconcileUnblocks(before, next, ['hr.employee', 'res.partner'])).toEqual(before.unblock_models);
    expect(reconcileUnblocks(before, { ...next, allowed_actions: [] }, ['hr.employee', 'res.partner'])).toEqual(['res.partner']);
  });
});

it('counts a legacy action:model in Other settings as a real table use', () => {
  const before = { allowed_models: ['hr.employee'], allowed_actions: ['action:hr.employee'], unblock_models: ['hr.employee'] };
  const next = { ...before, allowed_models: [] };
  expect(reconcileUnblocks(before, next, ['hr.employee'])).toEqual(['hr.employee']);
  expect(reconcileUnblocks(next, { ...next, allowed_actions: [] }, ['hr.employee'])).toEqual([]);
});

describe('Odoo account action mapping', () => {
  it('maps every action and keeps denial distinct from unknown', async () => {
    const { actionAccess } = await import('./permissions');
    const access = { read: true, create: false, write: null, unlink: false, checked_at: 'now' };
    expect(actionAccess(access, 'read')).toBe(true);
    expect(actionAccess(access, 'create')).toBe(false);
    expect(actionAccess(access, 'write')).toBeNull();
    expect(actionAccess(access, 'unlink')).toBe(false);
    expect(actionAccess(access, 'execute')).toBeNull();
    expect(actionAccess({ ...access, write: false }, 'execute')).toBe(false);
    expect(actionAccess({ ...access, write: true }, 'execute')).toBe(true);
    expect(actionAccess(undefined, 'read')).toBeNull();
  });
});
