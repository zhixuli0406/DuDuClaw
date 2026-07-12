import { describe, expect, it } from 'vitest';
import { cycleStepStatus, planProgress, stepMoveTarget } from './plan-utils';
import type { PlanStep, PlanStepStatus } from './api';

function step(id: string, status: PlanStepStatus): PlanStep {
  return {
    id,
    plan_id: 'p1',
    text: id,
    assignee_kind: 'agent',
    assignee: 'agnes',
    status,
    step_order: 0,
    created_at: '2026-07-11T00:00:00Z',
    updated_at: '2026-07-11T00:00:00Z',
  };
}

describe('cycleStepStatus', () => {
  it('advances todo → doing → done → todo', () => {
    expect(cycleStepStatus('todo')).toBe('doing');
    expect(cycleStepStatus('doing')).toBe('done');
    expect(cycleStepStatus('done')).toBe('todo');
  });
  it('un-skips a skipped step back to todo', () => {
    expect(cycleStepStatus('skipped')).toBe('todo');
  });
});

describe('planProgress', () => {
  it('counts done and skipped as settled', () => {
    const p = planProgress([step('a', 'done'), step('b', 'skipped'), step('c', 'todo'), step('d', 'doing')]);
    expect(p.settled).toBe(2);
    expect(p.total).toBe(4);
    expect(p.pct).toBe(50);
  });
  it('is 0% on an empty plan (no divide-by-zero)', () => {
    expect(planProgress([])).toEqual({ settled: 0, total: 0, pct: 0 });
  });
});

describe('stepMoveTarget', () => {
  it('moves within bounds', () => {
    expect(stepMoveTarget(1, 'up', 3)).toBe(0);
    expect(stepMoveTarget(1, 'down', 3)).toBe(2);
  });
  it('returns null at the edges (no-op moves skip the RPC)', () => {
    expect(stepMoveTarget(0, 'up', 3)).toBeNull();
    expect(stepMoveTarget(2, 'down', 3)).toBeNull();
  });
});
