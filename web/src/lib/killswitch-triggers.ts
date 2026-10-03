import type { KillswitchConfig, KillswitchTriggers, KillswitchUpdate } from '@/lib/api';

/**
 * v1.68 (W2): KILLSWITCH.toml `[triggers]` are enforced only when present in
 * the file (`killswitch.get` → `triggers_enforced`). The editor therefore
 * arms a trigger explicitly and sends only what changed — re-sending all four
 * displayed defaults on every save would arm all four.
 */
export const TRIGGER_KEYS = [
  'max_replies_per_minute',
  'max_consecutive_errors',
  'error_rate_threshold',
  'cost_limit_usd',
] as const satisfies readonly (keyof KillswitchTriggers)[];

export type TriggerKey = (typeof TRIGGER_KEYS)[number];
export type TriggerEnabled = Record<TriggerKey, boolean>;

/** Enforced state as loaded. Older gateways (no field) enforced none. */
export function enforcedFrom(config: KillswitchConfig): TriggerEnabled {
  const e = config.triggers_enforced ?? {};
  return {
    max_replies_per_minute: e.max_replies_per_minute === true,
    max_consecutive_errors: e.max_consecutive_errors === true,
    error_rate_threshold: e.error_rate_threshold === true,
    cost_limit_usd: e.cost_limit_usd === true,
  };
}

/**
 * The `triggers` part of `killswitch.update`: a newly enabled trigger or a
 * changed value of an enabled one is sent; a trigger switched off that was
 * enforced is sent as `null` (removal). Returns `undefined` when nothing
 * changed, so the whole `triggers` key is left out.
 */
export function triggersPayload(
  initial: KillswitchTriggers,
  initialEnabled: TriggerEnabled,
  current: KillswitchTriggers,
  enabled: TriggerEnabled,
): KillswitchUpdate['triggers'] | undefined {
  const out: NonNullable<KillswitchUpdate['triggers']> = {};
  for (const k of TRIGGER_KEYS) {
    if (enabled[k]) {
      if (!initialEnabled[k] || current[k] !== initial[k]) out[k] = current[k];
    } else if (initialEnabled[k]) {
      out[k] = null;
    }
  }
  return Object.keys(out).length > 0 ? out : undefined;
}
