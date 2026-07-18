import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GovernancePage } from './GovernancePage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
});

describe('GovernancePage', () => {
  it('renders the governance heading', () => {
    mockWsClient.call.mockResolvedValue({ policies: [] });
    renderWithProviders(<GovernancePage />);

    expect(screen.getByText('Governance')).toBeInTheDocument();
  });

  it('shows empty state when no policies', async () => {
    mockWsClient.call.mockResolvedValue({ policies: [] });
    renderWithProviders(<GovernancePage />);

    expect(await screen.findByText('No policies yet.')).toBeInTheDocument();
  });

  it('shows policies returned by the API', async () => {
    mockWsClient.call.mockResolvedValue({
      policies: [
        {
          policy_type: 'rate',
          policy_id: 'default-rate-mcp',
          agent_id: '*',
          resource: 'mcp_calls',
          limit: 200,
          window_seconds: 60,
          action_on_violation: 'reject',
        },
      ],
    });

    renderWithProviders(<GovernancePage />);

    expect(await screen.findByText('default-rate-mcp')).toBeInTheDocument();
  });

  it('opens the add-policy dialog when the add button is clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ policies: [] });

    renderWithProviders(<GovernancePage />);

    const addButton = await screen.findByRole('button', { name: /add policy/i });
    await user.click(addButton);

    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });
});
