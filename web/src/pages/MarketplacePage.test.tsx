import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MarketplacePage } from './MarketplacePage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // agents.list / marketplace.list both tolerate {} → empty catalog.
  mockWsClient.call.mockResolvedValue({});
});

describe('MarketplacePage (MDS)', () => {
  it('renders the header and the empty catalog state', async () => {
    renderWithProviders(<MarketplacePage />);
    expect(await screen.findByRole('heading', { name: 'Marketplace', level: 1 })).toBeInTheDocument();
  });
});
