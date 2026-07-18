import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DistributorsPage } from './DistributorsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // The page fires distributor.status() + distributor.list() via Promise.all —
  // one resolved value satisfies both calls.
  mockWsClient.call.mockResolvedValue({
    issuer_configured: false,
    stats: null,
    distributors: [],
  });
});

describe('DistributorsPage', () => {
  it('renders the page header', () => {
    renderWithProviders(<DistributorsPage />);
    // en.json → "manage.distributors": "White-label"
    expect(screen.getByRole('heading', { name: 'White-label' })).toBeInTheDocument();
  });
});
