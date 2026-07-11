import { describe, it, expect, vi, beforeEach } from 'vitest';

// Mock the celebration bus so the store's diff can be observed without the
// react-dom portal / DOM side effects.
vi.mock('@/components/ui/CelebrationLayer', () => ({ celebrate: vi.fn() }));

import { celebrate } from '@/components/ui/CelebrationLayer';
import { useGrowthStore, growthEventBus, type GrowthEvent } from './growth-store';
import type { GrowthSnapshot, Achievement } from '@/lib/api-growth';

const ZERO_FACTS = {
  agents_count: 0,
  tasks_completed: 0,
  knowledge_pages: 0,
  skills_acquired: 0,
  routines_completed: 0,
  custom_skills_approved: 0,
};

function ach(id: string, unlocked: boolean, available = true): Achievement {
  return {
    id,
    unlocked,
    progress_current: unlocked ? 1 : 0,
    progress_denominator: 1,
    xp_reward: 25,
    available,
    unavailable_reason: available ? null : 'no per-day snapshot',
    unlocked_at: unlocked ? '2026-07-10T00:00:00Z' : null,
  };
}

function snap(overrides: Partial<GrowthSnapshot> = {}): GrowthSnapshot {
  return {
    xp: 100,
    level: 1,
    xp_into_level: 0,
    xp_for_next_level: 300,
    facts: ZERO_FACTS,
    achievements: [ach('first_agent', true), ach('tasks_100', false)],
    ...overrides,
  };
}

describe('growth-store', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    useGrowthStore.setState({ snapshot: null, loaded: false, levelUpNonce: 0 });
  });

  it('never celebrates on the first (baseline) snapshot', () => {
    const events: GrowthEvent[] = [];
    const off = growthEventBus.subscribe((e) => events.push(e));

    useGrowthStore.getState().applySnapshot(snap());

    expect(celebrate).not.toHaveBeenCalled();
    expect(events).toEqual([]);
    expect(useGrowthStore.getState().loaded).toBe(true);
    expect(useGrowthStore.getState().snapshot?.level).toBe(1);
    off();
  });

  it('celebrates + emits an event for a newly-unlocked achievement', () => {
    const events: GrowthEvent[] = [];
    const off = growthEventBus.subscribe((e) => events.push(e));

    // Baseline: tasks_100 still locked.
    useGrowthStore.getState().applySnapshot(snap());
    // Next: tasks_100 flips to unlocked.
    useGrowthStore.getState().applySnapshot(
      snap({ achievements: [ach('first_agent', true), ach('tasks_100', true)] }),
    );

    expect(celebrate).toHaveBeenCalledTimes(1);
    expect(celebrate).toHaveBeenCalledWith('badge');
    expect(events).toContainEqual({ type: 'achievement_unlocked', id: 'tasks_100' });
    off();
  });

  it('bumps levelUpNonce + emits level_up on a level increase', () => {
    const events: GrowthEvent[] = [];
    const off = growthEventBus.subscribe((e) => events.push(e));

    useGrowthStore.getState().applySnapshot(snap({ level: 1 }));
    useGrowthStore.getState().applySnapshot(snap({ level: 2 }));

    expect(useGrowthStore.getState().levelUpNonce).toBe(1);
    expect(events).toContainEqual({ type: 'level_up', level: 2 });
    off();
  });

  it('does not re-celebrate when the same snapshot is applied again', () => {
    useGrowthStore.getState().applySnapshot(
      snap({ achievements: [ach('first_agent', true), ach('tasks_100', false)] }),
    );
    useGrowthStore.getState().applySnapshot(
      snap({ achievements: [ach('first_agent', true), ach('tasks_100', true)] }),
    );
    vi.clearAllMocks();
    // Re-apply the identical (already-unlocked) snapshot.
    useGrowthStore.getState().applySnapshot(
      snap({ achievements: [ach('first_agent', true), ach('tasks_100', true)] }),
    );

    expect(celebrate).not.toHaveBeenCalled();
    expect(useGrowthStore.getState().levelUpNonce).toBe(0);
  });
});
