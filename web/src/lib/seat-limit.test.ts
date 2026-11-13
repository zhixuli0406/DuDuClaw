import { describe, expect, it } from 'vitest';
import { SEAT_LIMIT_ERROR_CODE, seatLimitOf, seatStatusKey } from './seat-limit';

describe('seatLimitOf', () => {
  it('reads the structured seat refusal', () => {
    const info = seatLimitOf({
      code: SEAT_LIMIT_ERROR_CODE,
      message: '名額已滿',
      max: 3,
      current: 3,
      seat_holders: [
        { id: 'a', display_name: 'A', status: 'active' },
        { id: 'b', status: 'archived' },
        { display_name: 'no id' },
        'junk',
      ],
    });
    expect(info).toEqual({
      message: '名額已滿',
      max: 3,
      current: 3,
      holders: [
        { id: 'a', display_name: 'A', status: 'active' },
        { id: 'b', display_name: 'b', status: 'archived' },
      ],
    });
  });

  it('ignores anything that is not a seat refusal', () => {
    for (const err of [null, undefined, 'Agent name is required', new Error('x'), { code: 'other' }, 5]) {
      expect(seatLimitOf(err)).toBeNull();
    }
  });

  it('labels known statuses only', () => {
    expect(seatStatusKey('archived')).toBe('agents.seatLimit.status.archived');
    expect(seatStatusKey('unreadable')).toBe('agents.seatLimit.status.unreadable');
    expect(seatStatusKey('custom')).toBeNull();
  });
});
