import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { XpBar, levelFromXp, xpForLevel, levelProgress } from './XpBar';

describe('XP level helpers', () => {
  it('levelFromXp follows floor(sqrt(xp/100))', () => {
    expect(levelFromXp(0)).toBe(0);
    expect(levelFromXp(100)).toBe(1);
    expect(levelFromXp(399)).toBe(1);
    expect(levelFromXp(400)).toBe(2);
    expect(levelFromXp(-5)).toBe(0);
  });

  it('xpForLevel is the inverse threshold', () => {
    expect(xpForLevel(0)).toBe(0);
    expect(xpForLevel(1)).toBe(100);
    expect(xpForLevel(3)).toBe(900);
  });

  it('levelProgress is a clamped fraction within the level', () => {
    expect(levelProgress(100)).toBe(0); // exactly at Lv.1 start
    expect(levelProgress(250)).toBeCloseTo((250 - 100) / (400 - 100), 5);
    expect(levelProgress(0)).toBe(0);
  });
});

describe('<XpBar>', () => {
  it('renders the level and a progressbar', () => {
    renderWithProviders(<XpBar xp={250} />);
    expect(screen.getByText('Lv.1')).toBeInTheDocument();
    const bar = screen.getByRole('progressbar');
    expect(bar).toHaveAttribute('aria-valuenow', '50');
  });
});
