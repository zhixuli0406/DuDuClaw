import { describe, it, expect, vi, beforeEach } from 'vitest';

// Capture calls to the WS client without a live socket.
const call = vi.fn();
vi.mock('./ws-client', () => ({
  client: { call: (...args: unknown[]) => call(...args) },
}));

import {
  createCustomSkill,
  generateCustomSkill,
  updateCustomSkill,
  submitCustomSkill,
  listCustomSkills,
  retireCustomSkill,
  decideApproval,
  CUSTOM_SKILL_STATUSES,
} from './api-custom-skills';

beforeEach(() => {
  call.mockReset();
  call.mockResolvedValue({});
});

describe('api-custom-skills wrappers', () => {
  it('createCustomSkill → skills.custom_create with params', () => {
    createCustomSkill({ display_name: 'X', built_by_agent: 'a', tags: 'x,y' });
    expect(call).toHaveBeenCalledWith('skills.custom_create', {
      display_name: 'X',
      built_by_agent: 'a',
      tags: 'x,y',
    });
  });

  it('generateCustomSkill → skills.custom_generate with id + instruction', () => {
    generateCustomSkill({ id: '1', instruction: 'go' });
    expect(call).toHaveBeenCalledWith('skills.custom_generate', { id: '1', instruction: 'go' });
  });

  it('updateCustomSkill → skills.custom_update (tags stay a comma string)', () => {
    updateCustomSkill({ id: '1', display_name: 'Y', tags: 'a,b' });
    expect(call).toHaveBeenCalledWith('skills.custom_update', { id: '1', display_name: 'Y', tags: 'a,b' });
  });

  it('submitCustomSkill → skills.custom_submit with only the id', () => {
    submitCustomSkill('1');
    expect(call).toHaveBeenCalledWith('skills.custom_submit', { id: '1' });
  });

  it('listCustomSkills → skills.custom_list with no params', () => {
    listCustomSkills();
    expect(call).toHaveBeenCalledWith('skills.custom_list');
  });

  it('retireCustomSkill → skills.custom_retire with the id', () => {
    retireCustomSkill('9');
    expect(call).toHaveBeenCalledWith('skills.custom_retire', { id: '9' });
  });

  it('decideApproval omits reason when absent', () => {
    decideApproval('a1', true);
    expect(call).toHaveBeenCalledWith('approvals.decide', { id: 'a1', approve: true });
  });

  it('decideApproval includes reason on reject', () => {
    decideApproval('a1', false, 'not safe');
    expect(call).toHaveBeenCalledWith('approvals.decide', { id: 'a1', approve: false, reason: 'not safe' });
  });

  it('passes the resolved value straight through', async () => {
    const rec = { id: '1', status: 'draft' };
    call.mockResolvedValueOnce(rec);
    await expect(createCustomSkill({ display_name: 'X' })).resolves.toBe(rec);
  });

  it('exposes the six lifecycle statuses in order', () => {
    expect(CUSTOM_SKILL_STATUSES).toEqual([
      'draft',
      'generating',
      'pending_approval',
      'approved',
      'rejected',
      'retired',
    ]);
  });
});
