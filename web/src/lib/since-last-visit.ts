/**
 * "你不在的時候" (since-last-visit) aggregation, W2-1
 * (`05-ux-redesign-proposal.md` Wave 2, `02-ux-methodology.md` §P3-2). Low
 * frequency dashboard visitors default to *recognition* over *recall*
 * (NN/g): instead of making them ask "what happened since I last looked?",
 * tell them. This module is the pure aggregation logic — filtering and
 * counting `ActivityEvent`s by category since an anchor time — plus the thin
 * localStorage wrapper that tracks "since when".
 */
import type { ActivityEvent, ActivityType } from './api';

export const LAST_VISIT_KEY = 'duduclaw:home:lastVisit';

/**
 * Below this gap the block is redundant — the visitor just refreshed the
 * page or tabbed back in, so there is nothing meaningfully "while you were
 * away" to report (spec: "首次訪問或間隔 <30 分鐘則不顯示此區塊").
 */
export const SINCE_LAST_VISIT_MIN_GAP_MS = 30 * 60 * 1000;

/**
 * Read the last-visit stamp (epoch ms), or `null` on a first visit / an
 * unparseable value / inaccessible storage (private-mode). `null` is the
 * "first visit" signal {@link shouldShowSinceLastVisit} keys off.
 */
export function readLastVisit(): number | null {
  try {
    const raw = localStorage.getItem(LAST_VISIT_KEY);
    if (!raw) return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  } catch {
    return null;
  }
}

/**
 * Stamp "now" as the last-visit time for the *next* visit. Best-effort —
 * a private-mode/quota failure just means the block won't show next time
 * either, which is the safe direction to fail in (never throws).
 */
export function writeLastVisit(nowMs: number): void {
  try {
    localStorage.setItem(LAST_VISIT_KEY, String(nowMs));
  } catch {
    /* private mode — no persistence, no crash */
  }
}

/**
 * Pure gate: should the since-last-visit block render at all? False on a
 * first visit (no prior stamp — nothing to recap) or when the gap since the
 * last stamp is too short to be worth summarizing.
 */
export function shouldShowSinceLastVisit(
  lastVisitMs: number | null,
  nowMs: number,
  minGapMs: number = SINCE_LAST_VISIT_MIN_GAP_MS,
): boolean {
  if (lastVisitMs == null) return false;
  if (nowMs < lastVisitMs) return false; // clock skew — don't guess at a negative gap
  return nowMs - lastVisitMs >= minGapMs;
}

/**
 * Event-type categories the block reports on, in display priority order —
 * mapped onto the existing `ActivityType` union already used by
 * `ActivityFeed`/`activity.list` (no new backend taxonomy). Deliberately
 * excludes routine chatter (`task_created`, `task_assigned`, `agent_reply`,
 * `autopilot_*`, `error`) — this block narrates outcomes accumulated while
 * the user was away, not every intermediate step (spec: "聚合成事件類型計
 * 數…不是逐筆 log"). `error`/`task_blocked`-shaped concerns belong to the
 * "需要你處理" list above, not this recap.
 */
export const SINCE_VISIT_CATEGORIES: readonly ActivityType[] = [
  'task_completed',
  'skill_learned',
  'evolution_triggered',
  'memory_distilled',
  'wiki_written',
  'agent_created',
];

export interface SinceVisitSummary {
  type: ActivityType;
  count: number;
}

/**
 * Aggregate events into per-category counts since `sinceMs` (inclusive) up
 * to `nowMs`, most-significant category first ({@link SINCE_VISIT_CATEGORIES}
 * order), zero counts dropped. Events outside the tracked categories, with an
 * unparseable timestamp, or timestamped in the future are ignored.
 */
export function summarizeSinceLastVisit(
  events: ReadonlyArray<Pick<ActivityEvent, 'type' | 'timestamp'>>,
  sinceMs: number,
  nowMs: number,
): SinceVisitSummary[] {
  const counts = new Map<ActivityType, number>();
  for (const event of events) {
    const t = Date.parse(event.timestamp);
    if (!Number.isFinite(t) || t < sinceMs || t > nowMs) continue;
    if (!SINCE_VISIT_CATEGORIES.includes(event.type)) continue;
    counts.set(event.type, (counts.get(event.type) ?? 0) + 1);
  }
  return SINCE_VISIT_CATEGORIES.filter((type) => (counts.get(type) ?? 0) > 0).map((type) => ({
    type,
    count: counts.get(type) as number,
  }));
}
