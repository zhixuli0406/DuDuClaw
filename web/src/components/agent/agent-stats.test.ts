import { describe, it, expect } from 'vitest';
import {
  agentTaskStats,
  isSameLocalDay,
  isLiveState,
  staffLevel,
  XP_PER_DONE_TASK,
} from './agent-stats';
import type { TaskInfo } from '@/lib/api';

function task(partial: Partial<TaskInfo>): TaskInfo {
  return {
    id: 'id',
    title: 't',
    description: '',
    status: 'todo',
    priority: 'medium',
    assigned_to: 'coder',
    created_by: 'user',
    created_at: '2026-07-10T00:00:00Z',
    updated_at: '2026-07-10T00:00:00Z',
    tags: [],
    ...partial,
  };
}

describe('isLiveState', () => {
  it('is true for active run states, false for resting/lifecycle', () => {
    expect(isLiveState('replying')).toBe(true);
    expect(isLiveState('tool_running')).toBe(true);
    expect(isLiveState('consolidating')).toBe(true);
    expect(isLiveState('awaiting_approval')).toBe(true);
    expect(isLiveState('idle')).toBe(false);
    expect(isLiveState('paused')).toBe(false);
    expect(isLiveState('terminated')).toBe(false);
  });
});

describe('isSameLocalDay', () => {
  const now = new Date('2026-07-10T12:00:00').getTime();
  it('matches same local day', () => {
    expect(isSameLocalDay('2026-07-10T09:00:00', now)).toBe(true);
  });
  it('rejects other days / missing / invalid', () => {
    expect(isSameLocalDay('2026-07-09T23:59:00', now)).toBe(false);
    expect(isSameLocalDay(undefined, now)).toBe(false);
    expect(isSameLocalDay('not-a-date', now)).toBe(false);
  });
});

describe('agentTaskStats', () => {
  const now = new Date('2026-07-10T12:00:00').getTime();
  const tasks: TaskInfo[] = [
    task({ id: '1', assigned_to: 'coder', status: 'done', completed_at: '2026-07-10T08:00:00' }),
    task({ id: '2', assigned_to: 'coder', status: 'done', completed_at: '2026-07-09T08:00:00' }),
    task({ id: '3', assigned_to: 'coder', status: 'done' }), // no completed_at → not today
    task({ id: '4', assigned_to: 'coder', status: 'in_progress' }),
    task({ id: '5', assigned_to: 'coder', status: 'blocked' }),
    task({ id: '6', assigned_to: 'coder', status: 'todo' }),
    task({ id: '7', assigned_to: 'other', status: 'done', completed_at: '2026-07-10T08:00:00' }),
  ];

  it('tallies only the named agent, splitting today from all-time done', () => {
    const s = agentTaskStats(tasks, 'coder', now);
    expect(s.done).toBe(3);
    expect(s.todayDone).toBe(1);
    expect(s.inProgress).toBe(1);
    expect(s.blocked).toBe(1);
    expect(s.todo).toBe(1);
    expect(s.total).toBe(6);
  });

  it('is empty for an unknown agent', () => {
    const s = agentTaskStats(tasks, 'ghost', now);
    expect(s.total).toBe(0);
    expect(s.done).toBe(0);
  });
});

describe('staffLevel', () => {
  it('follows the global Lv = floor(sqrt(done*12/100)) curve', () => {
    expect(staffLevel(0)).toBe(0);
    // 8 done → 96 XP → sqrt(0.96) → 0
    expect(staffLevel(8)).toBe(0);
    // 9 done → 108 XP → floor(sqrt(1.08)) → 1
    expect(staffLevel(9)).toBe(1);
    // 34 done → 408 XP → floor(sqrt(4.08)) → 2
    expect(staffLevel(34)).toBe(2);
    expect(XP_PER_DONE_TASK).toBe(12);
  });

  it('clamps negatives to 0', () => {
    expect(staffLevel(-5)).toBe(0);
  });
});
