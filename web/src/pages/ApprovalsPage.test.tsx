import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ApprovalsPage } from './ApprovalsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  // Benign empty object keeps every optional chain safe (approvals.list → {}).
  mockWsClient.call.mockResolvedValue({});
});

describe('ApprovalsPage (MDS)', () => {
  it('renders the header and the empty state when nothing is pending', async () => {
    renderWithProviders(<ApprovalsPage />);
    expect(await screen.findByText('Approval center')).toBeInTheDocument();
    expect(await screen.findByText('Nothing waiting for approval')).toBeInTheDocument();
  });
});
