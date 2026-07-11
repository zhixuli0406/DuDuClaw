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
  // Roster view preference persists in localStorage; reset so each test starts
  // from the default character-card view (§5.4 T6.1).
  try { localStorage.clear(); } catch { /* jsdom */ }
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

  it('renders roster character cards, and lifecycle status in the management view', async () => {
    const user = userEvent.setup();
    const roster = [
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
    ];
    // The mount effect re-fetches agents (and tasks) — keep the mock returning
    // the roster so the async fetch doesn't wipe the seeded list.
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({ agents: roster as never[], loading: false });

    renderWithProviders(<AgentsPage />);

    // Default character-card roster shows the staff names.
    expect(screen.getByText('My Bot')).toBeInTheDocument();
    expect(screen.getByText('Helper')).toBeInTheDocument();

    // Lifecycle status text (Active/Paused) lives in the management view — the
    // second segmented toggle tab.
    const tabs = screen.getAllByRole('tab');
    await user.click(tabs[1]);
    expect(await screen.findByText('Active')).toBeInTheDocument();
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
