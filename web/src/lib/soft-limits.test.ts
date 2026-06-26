import { describe, it, expect } from 'vitest';
import { softLimitStatus, PERSONAL_SOFT_LIMITS } from './soft-limits';

describe('softLimitStatus', () => {
  it('mirrors features.toml personal tier limits', () => {
    expect(PERSONAL_SOFT_LIMITS.hobby).toEqual({ agents: 1, channels: 1 });
    expect(PERSONAL_SOFT_LIMITS.solo).toEqual({ agents: 1, channels: 2 });
    expect(PERSONAL_SOFT_LIMITS.studio).toEqual({ agents: 3, channels: 5 });
  });

  it('returns null for unlimited / unknown tiers', () => {
    expect(softLimitStatus('business', 99, 99)).toBeNull();
    expect(softLimitStatus('self_host_pro', 99, 99)).toBeNull();
    expect(softLimitStatus('community', 99, 99)).toBeNull();
    expect(softLimitStatus(undefined, 1, 1)).toBeNull();
  });

  it('flags over-limit per dimension, non-destructively', () => {
    const s = softLimitStatus('solo', 1, 2);
    expect(s).not.toBeNull();
    expect(s!.agentsOver).toBe(true); // 1 >= 1
    expect(s!.channelsOver).toBe(true); // 2 >= 2
    expect(s!.anyOver).toBe(true);
  });

  it('does not flag when under limit', () => {
    const s = softLimitStatus('studio', 1, 2);
    expect(s!.agentsOver).toBe(false);
    expect(s!.channelsOver).toBe(false);
    expect(s!.anyOver).toBe(false);
  });

  it('flags only the dimension that is over', () => {
    const s = softLimitStatus('studio', 3, 1); // agents at 3 (limit 3), channels under
    expect(s!.agentsOver).toBe(true);
    expect(s!.channelsOver).toBe(false);
    expect(s!.anyOver).toBe(true);
  });
});
