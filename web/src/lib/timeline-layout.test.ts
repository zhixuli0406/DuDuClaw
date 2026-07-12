import { describe, expect, it } from 'vitest';
import type { TimelineRow } from '@/lib/api';
import { buildLanes, pickTickStepMs, timeTicks, xForMs } from './timeline-layout';

const T0 = Date.parse('2026-07-11T00:00:00Z');
const H = 3_600_000;

function row(partial: Partial<TimelineRow> & { agent_id: string }): TimelineRow {
  return {
    kind: 'task',
    label: 'row',
    started_at: new Date(T0).toISOString(),
    ended_at: new Date(T0 + H).toISOString(),
    status: 'done',
    ref_id: Math.random().toString(36).slice(2),
    ...partial,
  };
}

describe('buildLanes', () => {
  const opts = { fromMs: T0, toMs: T0 + 24 * H, nowMs: T0 + 24 * H };

  it('groups rows into per-agent lanes sorted by agent id', () => {
    const lanes = buildLanes(
      [row({ agent_id: 'mia' }), row({ agent_id: 'bruno' })],
      opts,
    );
    expect(lanes.map((l) => l.agentId)).toEqual(['bruno', 'mia']);
    expect(lanes.every((l) => l.subRowCount === 1)).toBe(true);
  });

  it('stacks overlapping bars into sub-rows (first-fit)', () => {
    const lanes = buildLanes(
      [
        row({ agent_id: 'a', ref_id: 'r1', started_at: iso(0), ended_at: iso(4) }),
        row({ agent_id: 'a', ref_id: 'r2', started_at: iso(1), ended_at: iso(2) }), // overlaps r1
        row({ agent_id: 'a', ref_id: 'r3', started_at: iso(2), ended_at: iso(3) }), // fits after r2
        row({ agent_id: 'a', ref_id: 'r4', started_at: iso(5), ended_at: iso(6) }), // free again
      ],
      opts,
    );
    const lane = lanes[0];
    const bySub = Object.fromEntries(lane.rows.map((r) => [r.row.ref_id, r.subRow]));
    expect(bySub.r1).toBe(0);
    expect(bySub.r2).toBe(1); // overlaps r1 → next sub-row
    expect(bySub.r3).toBe(1); // r2 ended → reuses its sub-row
    expect(bySub.r4).toBe(0); // everything ended → back to top
    expect(lane.subRowCount).toBe(2);
  });

  it('extends running rows (ended_at null) to nowMs and flags them', () => {
    const nowMs = T0 + 10 * H;
    const lanes = buildLanes(
      [row({ agent_id: 'a', started_at: iso(1), ended_at: null })],
      { ...opts, nowMs },
    );
    const placed = lanes[0].rows[0];
    expect(placed.running).toBe(true);
    expect(placed.endMs).toBe(nowMs);
  });

  it('marks instants and packs adjacent dots with minPackMs breathing room', () => {
    const lanes = buildLanes(
      [
        row({ agent_id: 'a', ref_id: 'd1', started_at: iso(1), ended_at: iso(1) }),
        row({ agent_id: 'a', ref_id: 'd2', started_at: iso(1.05), ended_at: iso(1.05) }),
      ],
      { ...opts, minPackMs: H }, // dots reserve 1h of packing room
    );
    const lane = lanes[0];
    expect(lane.rows.every((r) => r.instant)).toBe(true);
    // Second dot lands inside the first dot's packing footprint → own sub-row.
    expect(new Set(lane.rows.map((r) => r.subRow)).size).toBe(2);
  });

  it('drops rows with unparseable timestamps instead of corrupting the layout', () => {
    const lanes = buildLanes(
      [row({ agent_id: 'a', started_at: 'not-a-date' }), row({ agent_id: 'a' })],
      opts,
    );
    expect(lanes[0].rows).toHaveLength(1);
  });

  it('clamps end-before-start noise to a zero-width bar', () => {
    const lanes = buildLanes(
      [row({ agent_id: 'a', started_at: iso(5), ended_at: iso(3) })],
      opts,
    );
    const placed = lanes[0].rows[0];
    expect(placed.endMs).toBe(placed.startMs);
  });

  function iso(hours: number): string {
    return new Date(T0 + hours * H).toISOString();
  }
});

describe('time axis', () => {
  it('picks a step that yields at most maxTicks ticks', () => {
    expect(pickTickStepMs(H, 8)).toBe(15 * 60_000); // 1h span → 15m grid (4 ticks)
    expect(pickTickStepMs(24 * H, 8)).toBe(3 * H); // 24h span → 3h grid (8 ticks)
    expect(pickTickStepMs(7 * 24 * H, 8)).toBe(24 * H); // 7d span → daily grid
  });

  it('generates ticks inside the window on round boundaries', () => {
    const from = T0 + 10 * 60_000; // 00:10
    const to = T0 + H + 10 * 60_000; // 01:10
    const ticks = timeTicks(from, to, 8);
    expect(ticks.length).toBeGreaterThan(0);
    expect(ticks.length).toBeLessThanOrEqual(8);
    for (const t of ticks) {
      expect(t.ms).toBeGreaterThanOrEqual(from);
      expect(t.ms).toBeLessThanOrEqual(to);
      expect(t.ms % (15 * 60_000)).toBe(0); // aligned to the 15m grid
    }
  });

  it('returns an empty axis for a degenerate window', () => {
    expect(timeTicks(T0, T0)).toEqual([]);
  });

  it('maps timestamps into pixel space with clamping', () => {
    const from = T0;
    const to = T0 + 10 * H;
    expect(xForMs(from, from, to, 1000)).toBe(0);
    expect(xForMs(to, from, to, 1000)).toBe(1000);
    expect(xForMs(from + 5 * H, from, to, 1000)).toBe(500);
    expect(xForMs(from - H, from, to, 1000)).toBe(0); // clamped
    expect(xForMs(to + H, from, to, 1000)).toBe(1000); // clamped
  });
});
