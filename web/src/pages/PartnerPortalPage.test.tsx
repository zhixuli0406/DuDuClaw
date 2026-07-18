import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PartnerPortalPage } from './PartnerPortalPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // profile/stats/customers all resolve {} → empty profile → onboarding card.
  mockWsClient.call.mockResolvedValue({});
});

describe('PartnerPortalPage (MDS)', () => {
  it('renders the onboarding card when no partner profile exists', async () => {
    renderWithProviders(<PartnerPortalPage />);
    expect(await screen.findByText('Set up your partner profile')).toBeInTheDocument();
  });
});
