import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { DailyReportCard } from '@/components/growth/DailyReportCard';
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useGrowthStore } from '@/stores/growth-store';
import { GrowthPage } from './GrowthPage';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  useConnectionStore.setState({ state: 'authenticated' as never });
  mockWsClient.call.mockResolvedValue({});
  useGrowthStore.setState({
    snapshot: null,
    loaded: false,
    levelUpNonce: 0,
    error: null,
    retry: null,
  });
});

afterEach(() => {
  useGrowthStore.setState({ error: null, retry: null });
});

describe('GrowthPage', () => {
  it('renders the growth header', () => {
    renderWithProviders(<GrowthPage />);
    expect(screen.getByRole('heading', { name: 'Growth' })).toBeInTheDocument();
  });

  // `GrowthMount` used to discard the poll error, so `loaded` never flipped and
  // this page sat on its skeleton indefinitely with zero feedback — the most
  // severe finding of the phase-4 audit (P05 Blocker, "infinite spinner").
  it('shows a retryable error instead of an endless skeleton when the poll fails', async () => {
    const retry = vi.fn();
    useGrowthStore.setState({ error: new Error('Failed to fetch'), retry });

    renderWithProviders(<GrowthPage />);

    const alert = screen.getByRole('alert');
    expect(alert).toBeInTheDocument();
    expect(alert).toHaveTextContent(/Can't reach the backend service/);
    screen.getByRole('button', { name: 'Try again' }).click();
    expect(retry).toHaveBeenCalledTimes(1);
  });

  it('keeps the loaded figures on screen when a later poll fails', () => {
    useGrowthStore.setState({
      loaded: true,
      snapshot: null,
      error: new Error('boom'),
      retry: vi.fn(),
    });
    renderWithProviders(<GrowthPage />);
    // Inline banner (not the full-page card) so the page body still renders.
    expect(screen.getByRole('alert')).toHaveAttribute('data-variant', 'inline');
  });
});

it('does not load the company daily report for employees in the page or popup', () => {
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  useConnectionStore.setState({ state: 'authenticated' as never });
  renderWithProviders(<><GrowthPage /><DailyReportCard /></>);
  expect(mockWsClient.call.mock.calls.some(([m]) => m === 'growth.daily_report')).toBe(false);
});

it('renders repeated reduced snapshots without company details or report calls', () => {
  const reduced = { xp: 450, level: 2, xp_into_level: 50, xp_for_next_level: 500 };
  useGrowthStore.getState().applySnapshot(reduced);
  useGrowthStore.getState().applySnapshot(reduced);
  renderWithProviders(<GrowthPage />);
  expect(screen.getByText('Lv')).toBeInTheDocument();
  expect(screen.queryByText('Achievement wall')).not.toBeInTheDocument();
  expect(screen.queryByText('AI staff')).not.toBeInTheDocument();
  expect(mockWsClient.call).not.toHaveBeenCalled();
});

it('renders a reduced snapshot for a cached Manager role without missing-field errors', () => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never });
  useConnectionStore.setState({ state: 'disconnected' as never });
  useGrowthStore.getState().applySnapshot({ xp: 450, level: 2, xp_into_level: 50, xp_for_next_level: 500 });
  renderWithProviders(<GrowthPage />);
  expect(screen.getByText('Lv')).toBeInTheDocument();
  expect(screen.queryByText('Achievement wall')).not.toBeInTheDocument();
  expect(screen.queryByText('AI staff')).not.toBeInTheDocument();
});

it('preserves the Manager fact grid and achievement wall on a full snapshot', () => {
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never });
  useConnectionStore.setState({ state: 'disconnected' as never });
  useGrowthStore.getState().applySnapshot({
    xp: 450, level: 2, xp_into_level: 50, xp_for_next_level: 500,
    facts: { agents_count: 7, tasks_completed: 5, knowledge_pages: 4, skills_acquired: 3, routines_completed: 2, custom_skills_approved: 1 },
    achievements: [],
  });
  renderWithProviders(<GrowthPage />);
  expect(screen.getByText('AI staff')).toBeInTheDocument();
  expect(screen.getByText('Achievement wall')).toBeInTheDocument();
});
