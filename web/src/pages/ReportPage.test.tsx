import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ReportPage } from './ReportPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
});

describe('ReportPage', () => {
  it('renders the report header and the period range control', () => {
    renderWithProviders(<ReportPage />);
    expect(screen.getByRole('heading', { name: 'Reports' })).toBeInTheDocument();
    // Segmented period control (day / week / month).
    expect(screen.getByRole('radio', { name: 'Month' })).toBeInTheDocument();
  });
});

describe('ReportPage notification effectiveness (W2-8, notify.stats)', () => {
  // The section's fetch is gated on `connectionState === 'authenticated'`
  // (the default 'disconnected' test-store state would otherwise leave it
  // stuck in its loading skeleton forever).
  beforeEach(() => {
    useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  });

  it('shows an empty state when there is no notification data yet', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'notify.stats') {
        return Promise.resolve({ days: 30, broken_threshold: 0.5, min_sample: 10, types: [] });
      }
      // Other RPCs (analytics.summary, cost.summary, …) must resolve to a
      // falsy shape, not `{}` — the page treats any truthy `summary` as real
      // data and dereferences fields on it (e.g. `summary.total_conversations`)
      // that an empty object doesn't carry.
      return Promise.resolve(null);
    });

    renderWithProviders(<ReportPage />);

    expect(await screen.findByText('Notification effectiveness')).toBeInTheDocument();
    expect(await screen.findByText('No notification data yet')).toBeInTheDocument();
  });

  it('flags a broken type in red with the noisy-notification hint, and renders a healthy one plainly', async () => {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'notify.stats') {
        return Promise.resolve({
          days: 30,
          broken_threshold: 0.5,
          min_sample: 10,
          types: [
            { type: 'decision.install', pushed: 12, actionable: 12, acted: 5, action_rate: 0.42, broken: true },
            { type: 'decision.goal', pushed: 12, actionable: 12, acted: 12, action_rate: 1.0, broken: false },
            { type: 'evolution.stagnation', pushed: 8, actionable: 0, acted: 0, action_rate: 0, broken: false },
          ],
        });
      }
      // Other RPCs (analytics.summary, cost.summary, …) must resolve to a
      // falsy shape, not `{}` — the page treats any truthy `summary` as real
      // data and dereferences fields on it (e.g. `summary.total_conversations`)
      // that an empty object doesn't carry.
      return Promise.resolve(null);
    });

    renderWithProviders(<ReportPage />);

    expect(await screen.findByText('decision.install')).toBeInTheDocument();
    expect(await screen.findByText('This type may be too noisy — consider downgrading or turning it off.')).toBeInTheDocument();
    expect(await screen.findByText('decision.goal')).toBeInTheDocument();
    expect(screen.getByText('evolution.stagnation')).toBeInTheDocument();
    expect(screen.getByText('FYI only, nothing actionable')).toBeInTheDocument();
  });
});
