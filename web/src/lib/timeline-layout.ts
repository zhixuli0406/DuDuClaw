import type { TimelineRow } from '@/lib/api';

/**
 * Work Timeline lane layout (G11) — pure functions so the Gantt geometry is
 * unit-testable without React or SVG. The page maps the returned millisecond
 * positions onto pixels; nothing here touches the DOM.
 */

/** A row placed inside a lane: resolved times + assigned stacking sub-row. */
export interface PlacedRow {
  readonly row: TimelineRow;
  /** Effective start in epoch ms. */
  readonly startMs: number;
  /** Effective end in epoch ms (running rows extend to `nowMs`). */
  readonly endMs: number;
  /** True when the source row is a point event (`ended_at === started_at`). */
  readonly instant: boolean;
  /** True when the source row is still running (`ended_at === null`). */
  readonly running: boolean;
  /** Stacking sub-row index inside the lane (0 = top). */
  readonly subRow: number;
}

/** One agent lane: its placed rows and how many sub-rows they stack into. */
export interface TimelineLane {
  readonly agentId: string;
  readonly rows: readonly PlacedRow[];
  readonly subRowCount: number;
}

export interface LaneLayoutOptions {
  readonly fromMs: number;
  readonly toMs: number;
  /** "Now" anchor for open-ended (running) rows. */
  readonly nowMs: number;
  /**
   * Minimum packing footprint in ms — instants and hairline bars reserve this
   * much horizontal room when deciding sub-row stacking, so adjacent dots drop
   * to their own sub-row instead of rendering on top of each other. Purely a
   * layout concern; the rendered position still uses the true timestamps.
   */
  readonly minPackMs?: number;
}

function parseMs(value: string): number {
  const ms = Date.parse(value);
  return Number.isFinite(ms) ? ms : NaN;
}

/**
 * Group rows into per-agent lanes and greedily assign stacking sub-rows so
 * overlapping bars never draw over each other (first-fit interval packing —
 * each row takes the first sub-row whose last occupant ends before it starts).
 *
 * Rows with unparseable timestamps are dropped (fail-safe skip). Lanes are
 * sorted by agent id for a stable, non-jumping layout across refreshes.
 */
export function buildLanes(
  rows: readonly TimelineRow[],
  opts: LaneLayoutOptions,
): TimelineLane[] {
  const { nowMs, minPackMs = 0 } = opts;
  const byAgent = new Map<string, PlacedRow[]>();

  for (const row of rows) {
    const startMs = parseMs(row.started_at);
    if (!Number.isFinite(startMs)) continue;
    const running = row.ended_at === null;
    const endRaw = running ? nowMs : parseMs(row.ended_at as string);
    if (!Number.isFinite(endRaw)) continue;
    const endMs = Math.max(endRaw, startMs);
    const placed: PlacedRow = {
      row,
      startMs,
      endMs,
      instant: !running && row.ended_at === row.started_at,
      running,
      subRow: 0,
    };
    const lane = byAgent.get(row.agent_id);
    if (lane) lane.push(placed);
    else byAgent.set(row.agent_id, [placed]);
  }

  const lanes: TimelineLane[] = [];
  for (const agentId of [...byAgent.keys()].sort()) {
    const laneRows = byAgent
      .get(agentId)!
      .sort((a, b) => a.startMs - b.startMs || a.endMs - b.endMs);
    // First-fit packing: subRowEnds[i] = packed end of the last row in sub-row i.
    const subRowEnds: number[] = [];
    const placedRows: PlacedRow[] = [];
    for (const pr of laneRows) {
      const packedEnd = Math.max(pr.endMs, pr.startMs + minPackMs);
      let subRow = subRowEnds.findIndex((end) => end <= pr.startMs);
      if (subRow === -1) {
        subRow = subRowEnds.length;
        subRowEnds.push(packedEnd);
      } else {
        subRowEnds[subRow] = packedEnd;
      }
      placedRows.push({ ...pr, subRow });
    }
    lanes.push({ agentId, rows: placedRows, subRowCount: subRowEnds.length || 1 });
  }
  return lanes;
}

// ── Time axis ticks ─────────────────────────────────────────

export interface TimeTick {
  readonly ms: number;
  /** True when the tick crosses a local midnight (label should show the date). */
  readonly isDayStart: boolean;
}

const TICK_STEPS_MS: readonly number[] = [
  60_000, // 1m
  5 * 60_000,
  15 * 60_000,
  30 * 60_000,
  3_600_000, // 1h
  3 * 3_600_000,
  6 * 3_600_000,
  12 * 3_600_000,
  86_400_000, // 1d
  2 * 86_400_000,
  7 * 86_400_000,
];

/** Pick the smallest "nice" step that yields at most `maxTicks` ticks. */
export function pickTickStepMs(spanMs: number, maxTicks = 8): number {
  for (const step of TICK_STEPS_MS) {
    if (spanMs / step <= maxTicks) return step;
  }
  return TICK_STEPS_MS[TICK_STEPS_MS.length - 1];
}

/**
 * Generate axis ticks aligned to the step grid (local time), covering
 * `[fromMs, toMs]`. Ticks land on round boundaries (e.g. whole hours), not on
 * the arbitrary window start.
 */
export function timeTicks(fromMs: number, toMs: number, maxTicks = 8): TimeTick[] {
  if (!(toMs > fromMs)) return [];
  const step = pickTickStepMs(toMs - fromMs, maxTicks);
  // Align to local-time boundaries so hour/day labels read naturally.
  const tzOffsetMs = new Date(fromMs).getTimezoneOffset() * 60_000;
  const firstLocal = Math.ceil((fromMs - tzOffsetMs) / step) * step;
  const ticks: TimeTick[] = [];
  for (let local = firstLocal; local + tzOffsetMs <= toMs; local += step) {
    const ms = local + tzOffsetMs;
    if (ms < fromMs) continue;
    ticks.push({ ms, isDayStart: local % 86_400_000 === 0 });
  }
  return ticks;
}

/** Map a timestamp into `[0, width]` pixel space for the given window. */
export function xForMs(ms: number, fromMs: number, toMs: number, width: number): number {
  if (toMs <= fromMs) return 0;
  const clamped = Math.min(Math.max(ms, fromMs), toMs);
  return ((clamped - fromMs) / (toMs - fromMs)) * width;
}
