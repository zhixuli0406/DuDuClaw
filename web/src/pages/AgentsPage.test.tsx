import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { Routes, Route } from 'react-router';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { toastBus, type ToastInput } from '@/lib/toast';
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

  // The four lifecycle handlers were fire-and-forget `void` calls over a store
  // that swallowed its own errors, so a failed pause produced nothing at all
  // on screen (P05 Blocker, phase-4 audit).
  it('reports a failed lifecycle action instead of failing silently', async () => {
    const user = userEvent.setup();
    const seen: ToastInput[] = [];
    const off = toastBus.subscribe((t) => seen.push(t));
    const pauseAgent = vi.fn().mockRejectedValue(new Error('Failed to fetch'));
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({
      agents: roster as never[],
      loading: false,
      loaded: true,
      pauseAgent,
    } as never);

    renderWithProviders(<AgentsPage />);
    await user.click(screen.getAllByRole('button', { name: 'More actions' })[0]);
    await user.click(await screen.findByRole('menuitem', { name: /Pause/ }));

    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0].variant).toBe('error');
    // Plain language, not the raw thrown string.
    expect(seen[0].message).toContain("Can't reach the backend service");
    expect(seen[0].message).not.toContain('Failed to fetch');
    off();
  });

  it('confirms a lifecycle action that actually landed', async () => {
    const user = userEvent.setup();
    const seen: ToastInput[] = [];
    const off = toastBus.subscribe((t) => seen.push(t));
    const pauseAgent = vi.fn().mockResolvedValue(undefined);
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({
      agents: roster as never[],
      loading: false,
      loaded: true,
      pauseAgent,
    } as never);

    renderWithProviders(<AgentsPage />);
    await user.click(screen.getAllByRole('button', { name: 'More actions' })[0]);
    await user.click(await screen.findByRole('menuitem', { name: /Pause/ }));

    await waitFor(() => expect(seen).toHaveLength(1));
    expect(seen[0].variant).toBe('success');
    off();
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

  // X03 (UX audit §3.3): the Model column used to be dead text; it now opens
  // the staff member's own 腦袋與引擎 edit tab. Also proves the row's own
  // `to`-navigation (down to AgentDetailPage) doesn't hijack the click.
  it('opens the brain edit tab from the Model column, not the row link', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ agents: roster });
    useAgentsStore.setState({ agents: roster as never[], loading: false, loaded: true });

    renderWithProviders(
      <Routes>
        <Route path="/" element={<AgentsPage />} />
        <Route path="/agents/:id" element={<div>detail-page-probe</div>} />
        <Route path="/agents/:id/edit" element={<div>edit-page-probe</div>} />
      </Routes>,
    );

    const modelLink = await screen.findByRole('button', { name: 'claude-sonnet' });
    await user.click(modelLink);

    await waitFor(() => {
      expect(screen.getByText('edit-page-probe')).toBeInTheDocument();
    });
    expect(screen.queryByText('detail-page-probe')).not.toBeInTheDocument();
  });
});
