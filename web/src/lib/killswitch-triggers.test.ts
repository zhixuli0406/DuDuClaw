import { describe, it, expect } from 'vitest';
import { enforcedFrom, triggersPayload } from './killswitch-triggers';
import type { KillswitchConfig } from './api';

const T = { max_replies_per_minute: 10, max_consecutive_errors: 5, error_rate_threshold: 0.5, cost_limit_usd: 10 };
const NONE = { max_replies_per_minute: false, max_consecutive_errors: false, error_rate_threshold: false, cost_limit_usd: false };

describe('killswitch triggers payload', () => {
  it('arms only the trigger the operator ticked ("I only want the cost cap")', () => {
    expect(triggersPayload(T, NONE, { ...T, cost_limit_usd: 25 }, { ...NONE, cost_limit_usd: true })).toEqual({ cost_limit_usd: 25 });
  });

  it('sends nothing when nothing changed, even though defaults are displayed', () => {
    expect(triggersPayload(T, NONE, T, NONE)).toBeUndefined();
    const armed = { ...NONE, max_consecutive_errors: true };
    expect(triggersPayload(T, armed, T, armed)).toBeUndefined();
  });

  it('sends a changed value of an armed trigger and null for one switched off', () => {
    const armed = { ...NONE, max_consecutive_errors: true, cost_limit_usd: true };
    expect(
      triggersPayload(T, armed, { ...T, max_consecutive_errors: 3 }, { ...armed, cost_limit_usd: false }),
    ).toEqual({ max_consecutive_errors: 3, cost_limit_usd: null });
  });

  it('treats an older gateway (no triggers_enforced) as enforcing none', () => {
    expect(enforcedFrom({ triggers: T } as unknown as KillswitchConfig)).toEqual(NONE);
    expect(enforcedFrom({ triggers: T, triggers_enforced: { cost_limit_usd: true } } as unknown as KillswitchConfig).cost_limit_usd).toBe(true);
  });
});
