import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { BillingPage } from './BillingPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // The page tolerates missing fields — a benign object keeps every optional
  // chain safe (usage?.x.used ?? 0 / budget.incidents → {}).
  mockWsClient.call.mockResolvedValue({});
});

describe('BillingPage', () => {
  it('renders the billing header title', () => {
    renderWithProviders(<BillingPage />);
    expect(screen.getByText('Billing')).toBeInTheDocument();
  });
});
