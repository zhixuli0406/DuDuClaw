import { describe, it, expect } from 'vitest';
import { screen } from '@testing-library/react';
import { Trophy } from 'lucide-react';
import { renderWithProviders } from '@/test/render';
import { AchievementBadge } from './AchievementBadge';

describe('<AchievementBadge>', () => {
  it('shows an unlocked achievement with its date', () => {
    renderWithProviders(
      <AchievementBadge icon={Trophy} name="效率發明家" unlocked unlockedAt="2026-07-10" />,
    );
    expect(screen.getByText('效率發明家')).toBeInTheDocument();
    expect(screen.getByText('2026-07-10')).toBeInTheDocument();
  });

  it('renders locked state with progress', () => {
    renderWithProviders(
      <AchievementBadge icon={Trophy} name="時間煉金師" unlocked={false} progress={0.4} />,
    );
    expect(screen.getByText('時間煉金師')).toBeInTheDocument();
  });
});
