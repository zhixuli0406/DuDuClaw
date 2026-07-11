import { describe, it, expect } from 'vitest';
import '@/test/mocks';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AchievementCell } from './AchievementCell';
import type { Achievement } from '@/lib/api-growth';

function make(overrides: Partial<Achievement> = {}): Achievement {
  return {
    id: 'inbox_zero_streak_7',
    unlocked: false,
    progress_current: 0,
    progress_denominator: 0,
    xp_reward: 25,
    available: true,
    unavailable_reason: null,
    unlocked_at: null,
    ...overrides,
  };
}

describe('AchievementCell', () => {
  it('renders available:false as "not available", not a 0-progress lock', () => {
    renderWithProviders(
      <AchievementCell
        ach={make({ available: false, unavailable_reason: 'no per-day snapshot yet' })}
      />,
    );

    // The explicit unavailable label is shown…
    const chip = screen.getByText('Not available yet');
    expect(chip).toBeTruthy();
    // …carrying the backend reason as a tooltip (honest, not a fabricated 0%).
    expect(chip.getAttribute('title')).toBe('no per-day snapshot yet');
    // …and there is no progressbar (which the locked variant would render).
    expect(screen.queryByRole('progressbar')).toBeNull();
  });

  it('renders an unlocked achievement with its unlock date', () => {
    renderWithProviders(
      <AchievementCell
        ach={make({ id: 'first_agent', unlocked: true, unlocked_at: '2026-07-10T12:00:00Z' })}
      />,
    );
    expect(screen.getByText('First teammate')).toBeTruthy();
    expect(screen.getByText('2026-07-10')).toBeTruthy();
  });
});
