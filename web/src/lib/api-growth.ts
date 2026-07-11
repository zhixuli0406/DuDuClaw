import { client } from './ws-client';

/**
 * Growth RPC wrappers (dashboard-redesign-v2 §6, V10). Kept in a dedicated file
 * — separate from the monolithic `api.ts` — so the V10 wave can add the
 * gamification surface without colliding with the parallel V7/V13 waves editing
 * `api.ts`. Shapes mirror the gateway `growth.rs` / `handlers.rs` payloads
 * exactly (W5-BE); adjust here and in the Rust structs in lockstep.
 *
 * Everything here is READ-ONLY and derived from real data on the gateway side.
 * The front end never computes XP or unlock state — it only renders what
 * `growth.snapshot` reports (§6.3: "前端只讀不算").
 */

/** The six datable facts the XP score is derived from (`GrowthFacts`). */
export interface GrowthFacts {
  agents_count: number;
  tasks_completed: number;
  knowledge_pages: number;
  skills_acquired: number;
  routines_completed: number;
  custom_skills_approved: number;
}

/**
 * One achievement on the wall. `available: false` is a first-class state — it
 * means the gateway cannot yet evaluate this achievement (e.g. no per-day
 * snapshot history), NOT that the user is at 0 progress. The UI must render it
 * as "暫不可用" with `unavailable_reason`, never as a 0/N progress lock (§6.3).
 */
export interface Achievement {
  id: string;
  unlocked: boolean;
  progress_current: number;
  progress_denominator: number;
  xp_reward: number;
  /** False ⇒ not evaluable yet; show "暫不可用", not a locked progress bar. */
  available: boolean;
  unavailable_reason?: string | null;
  /** RFC3339 unlock timestamp, present once `unlocked`. */
  unlocked_at?: string | null;
}

/** `growth.snapshot` — company XP/level + the achievement wall. */
export interface GrowthSnapshot {
  xp: number;
  level: number;
  /** XP accumulated inside the current level (`xp − level²·100`). */
  xp_into_level: number;
  /** XP span of the current level (`((level+1)² − level²)·100`). */
  xp_for_next_level: number;
  facts: GrowthFacts;
  achievements: Achievement[];
}

/**
 * `growth.daily_report` — one day's settlement card. Omitting `date` ⇒ the
 * gateway reports *yesterday* (a settled day, served from cache). Passing
 * today's date returns a live, still-changing figure: `cost_cents` is a
 * rolling-24h telemetry approximation and the row is not cached — so callers
 * that request "today" should label it as in-progress, not final.
 */
export interface DailyReport {
  /** YYYY-MM-DD (UTC on the gateway). */
  date: string;
  tasks_completed: number;
  /** Spend for the day in integer cents (rolling-24h approximation for today). */
  cost_cents: number;
  /** Agent id with the most completions that day, or null. */
  most_active_agent: string | null;
  new_knowledge_pages: number;
  xp_gained: number;
  /** Human-readable note on how `xp_gained` was derived (honesty annotation). */
  xp_basis: string;
}

export const growthApi = {
  /** Company XP/level + achievement wall. Any authed viewer may read. */
  snapshot: () => client.call('growth.snapshot') as Promise<GrowthSnapshot>,
  /**
   * A day's settlement card. Pass `date` (YYYY-MM-DD) for a specific day;
   * omit for yesterday.
   */
  dailyReport: (date?: string) =>
    client.call('growth.daily_report', date ? { date } : {}) as Promise<DailyReport>,
};
