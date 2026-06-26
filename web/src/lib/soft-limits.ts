/**
 * Personal-edition soft limits for the dashboard.
 *
 * SOURCE OF TRUTH is `crates/duduclaw-license/features.toml` (`max_agents` /
 * `max_channels` per tier), enforced server-side. This table MIRRORS the
 * personal cloud tiers purely to render a **non-blocking** "you're near your
 * plan limit" hint + upgrade CTA. It never blocks any action. Keep in sync
 * with features.toml; `soft-limits.test.ts` pins the expected values.
 *
 * `0` / absent = unlimited (no hint). Tier keys match the `edition` string
 * returned by `system.version` (license tier; `OpenSource` → "community").
 */
export interface SoftLimit {
  readonly agents: number;
  readonly channels: number;
}

export const PERSONAL_SOFT_LIMITS: Readonly<Record<string, SoftLimit>> = {
  hobby: { agents: 1, channels: 1 },
  solo: { agents: 1, channels: 2 },
  studio: { agents: 3, channels: 5 },
};

export interface SoftLimitStatus {
  readonly tier: string;
  readonly limit: SoftLimit;
  readonly agentsOver: boolean;
  readonly channelsOver: boolean;
  /** True when at least one dimension is at or over the soft limit. */
  readonly anyOver: boolean;
}

/**
 * Compute soft-limit status for a tier. Returns `null` when the tier has no
 * soft limit (unlimited / unknown), so callers render nothing.
 */
export function softLimitStatus(
  tier: string | undefined,
  agentCount: number,
  channelCount: number
): SoftLimitStatus | null {
  if (!tier) return null;
  const limit = PERSONAL_SOFT_LIMITS[tier];
  if (!limit) return null;
  const agentsOver = limit.agents > 0 && agentCount >= limit.agents;
  const channelsOver = limit.channels > 0 && channelCount >= limit.channels;
  return {
    tier,
    limit,
    agentsOver,
    channelsOver,
    anyOver: agentsOver || channelsOver,
  };
}
