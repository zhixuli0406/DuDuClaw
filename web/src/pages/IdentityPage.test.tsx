import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { IdentityPage } from './IdentityPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // identity.configGet resolves {} → defaults applied, loading resolves.
  mockWsClient.call.mockResolvedValue({});
});

describe('IdentityPage (MDS)', () => {
  it('renders the provider (lookup source) card after config loads', async () => {
    renderWithProviders(<IdentityPage />);
    expect(await screen.findByText('Lookup source')).toBeInTheDocument();
  });
});
