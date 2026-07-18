import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Routes, Route } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentsPage } from './AgentsPage';
import { useAgentsStore } from '@/stores/agents-store';

beforeEach(() => {
  vi.clearAllMocks();
  // Default: the API returns empty agents list
  mockWsClient.call.mockResolvedValue({ agents: [] });
  useAgentsStore.setState({ agents: [], loading: false, loaded: true, error: null });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

const roster = [
  {
    name: 'my-bot',
    display_name: 'My Bot',
    status: 'active',
    role: 'main',
    trigger: '@bot',
    model: { preferred: 'claude-sonnet' },
    heartbeat: { enabled: false },
  },
  {
    name: 'helper',
    display_name: 'Helper',
    status: 'paused',
    role: 'specialist',
    trigger: '@helper',
    model: { preferred: 'gpt-4' },
    heartbeat: { enabled: true },
  },
];

describe('AgentsPage', () => {
  it('renders page heading', () => {
    renderWithProviders(<AgentsPage />);
    expect(screen.getByText('Agent Management')).toBeInTheDocument();
  });

  it('shows empty state when no agents', async () => {
    renderWithProviders(<AgentsPage />);

    await waitFor(() => {
      expect(
        screen.getByText('No agents yet? Create your first AI assistant!'),
      ).toBeInTheDocument();
    });
  });

  it('renders a staff ListGrid row per agent with lifecycle status', async () => {
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({ agents: roster as never[], loading: false, loaded: true });

    renderWithProviders(<AgentsPage />);

    expect(screen.getByText('My Bot')).toBeInTheDocument();
    expect(screen.getByText('Helper')).toBeInTheDocument();
    // Lifecycle status text renders inline in the status column ("Active" also
    // labels the scope segment, so assert both the row status and the unique
    // "Paused" cell).
    expect(screen.getAllByText('Active').length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText('Paused')).toBeInTheDocument();
    // Each row exposes a kebab of lifecycle actions.
    expect(screen.getAllByRole('button', { name: 'More actions' })).toHaveLength(2);
  });

  it('navigates to the create page on the hire button', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({ agents: roster as never[], loading: false, loaded: true });

    renderWithProviders(
      <Routes>
        <Route path="/" element={<AgentsPage />} />
        <Route path="/agents/new" element={<div>create-page-probe</div>} />
      </Routes>,
    );

    const createBtn = screen.getByRole('button', { name: 'Create Agent' });
    await user.click(createBtn);

    await waitFor(() => {
      expect(screen.getByText('create-page-probe')).toBeInTheDocument();
    });
  });
});
