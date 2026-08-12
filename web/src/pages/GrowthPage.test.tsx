import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useGrowthStore } from '@/stores/growth-store';
import { GrowthPage } from './GrowthPage';

beforeEach(() => {
  vi.clearAllMocks();
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
