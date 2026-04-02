import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentsPage } from './AgentsPage';
import { useAgentsStore } from '@/stores/agents-store';

beforeEach(() => {
  vi.clearAllMocks();
  // Default: the API returns empty agents list
  mockWsClient.call.mockResolvedValue({ agents: [] });
  useAgentsStore.setState({ agents: [], loading: false, error: null });
});

describe('AgentsPage', () => {
  it('renders page heading', () => {
    renderWithProviders(<AgentsPage />);
    expect(screen.getByText('Agent Management')).toBeInTheDocument();
  });

  it('shows empty state when no agents', async () => {
    renderWithProviders(<AgentsPage />);

    await waitFor(() => {
      expect(
        screen.getByText('No agents yet? Create your first AI assistant!')
      ).toBeInTheDocument();
    });
  });

  it('renders agent cards when agents exist', () => {
    useAgentsStore.setState({
      agents: [
        {
          name: 'my-bot',
          display_name: 'My Bot',
          status: 'active',
          role: 'main',
          sandboxed: false,
        },
        {
          name: 'helper',
          display_name: 'Helper',
          status: 'paused',
          role: 'specialist',
          sandboxed: true,
        },
      ] as never[],
      loading: false,
    });

    renderWithProviders(<AgentsPage />);

    expect(screen.getByText('My Bot')).toBeInTheDocument();
    expect(screen.getByText('Helper')).toBeInTheDocument();
    expect(screen.getByText('Active')).toBeInTheDocument();
    expect(screen.getByText('Paused')).toBeInTheDocument();
  });

  it('opens create dialog on button click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AgentsPage />);

    // Find the header area's create button
    const headerDiv = screen.getByText('Agent Management').parentElement!;
    const createBtn = within(headerDiv).getByRole('button');
    await user.click(createBtn);

    await waitFor(() => {
      expect(screen.getByText('Display Name')).toBeInTheDocument();
    });
  });
});
