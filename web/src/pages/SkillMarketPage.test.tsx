import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SkillMarketPage } from './SkillMarketPage';

beforeEach(() => {
  vi.clearAllMocks();
  // Every RPC resolves to an empty envelope so the page renders empty states.
  mockWsClient.call.mockResolvedValue({});
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('SkillMarketPage', () => {
  it('renders the collection header with the build-skill action', () => {
    renderWithProviders(<SkillMarketPage />);
    // Header title (nav.skills) + primary CTA (skills.new.title).
    expect(screen.getByRole('heading', { name: 'Skills' })).toBeInTheDocument();
    expect(screen.getByText('Build a skill')).toBeInTheDocument();
  });

  it('renders the section switcher with all four tabs', () => {
    renderWithProviders(<SkillMarketPage />);
    expect(screen.getByRole('radio', { name: 'Market' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Team Skills' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'My Skills' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Leaderboard' })).toBeInTheDocument();
  });

  it('shows the category browser on the default Market tab', () => {
    renderWithProviders(<SkillMarketPage />);
    expect(screen.getByText('Browse by Category')).toBeInTheDocument();
    expect(screen.getByText('security')).toBeInTheDocument();
  });

  it('switches to the leaderboard tab and shows its empty state', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SkillMarketPage />);
    await user.click(screen.getByRole('radio', { name: 'Leaderboard' }));
    await waitFor(() => {
      expect(
        screen.getByText('No approved skills with a time-saving estimate yet'),
      ).toBeInTheDocument();
    });
  });
});
