import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DashboardPage } from './DashboardPage';
import { useAgentsStore } from '@/stores/agents-store';
import { useConnectionStore } from '@/stores/connection-store';
import { api } from '@/lib/api';

vi.mock('@/lib/api', async (importOriginal) => {
  const original = await importOriginal<typeof import('@/lib/api')>();
  return {
    ...original,
    api: {
      ...original.api,
      agents: { ...original.api.agents, list: vi.fn() },
      accounts: { ...original.api.accounts, budgetSummary: vi.fn() },
      system: {
        ...original.api.system,
        status: vi.fn(),
        doctor: vi.fn(),
      },
    },
  };
});

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ agents: [], loading: false, error: null });
  useConnectionStore.setState({ state: 'disconnected', error: null });
});

describe('DashboardPage', () => {
  it('renders stat cards when agents exist', async () => {
    const mockAgents = [
      { name: 'test-agent', display_name: 'Test', status: 'active', role: 'generalist' },
      { name: 'agent-2', display_name: 'Agent 2', status: 'paused', role: 'specialist' },
    ];

    useAgentsStore.setState({ agents: mockAgents as never[], loading: false });
    useConnectionStore.setState({ state: 'authenticated' as never });

    vi.mocked(api.agents.list).mockResolvedValue({ agents: mockAgents as never[] });
    vi.mocked(api.accounts.budgetSummary).mockResolvedValue({
      total_budget_cents: 10000,
      total_spent_cents: 2500,
    } as never);
    vi.mocked(api.system.doctor).mockResolvedValue({
      checks: [{ name: 'claude_cli', status: 'pass', message: 'OK' }],
      summary: { pass: 1, warn: 0, fail: 0 },
    } as never);

    renderWithProviders(<DashboardPage />);

    expect(screen.getByText('Dashboard')).toBeInTheDocument();
    expect(screen.getByText('Agent Status')).toBeInTheDocument();
    expect(screen.getByText('Channel Connections')).toBeInTheDocument();
    expect(screen.getByText('Monthly Budget')).toBeInTheDocument();
    expect(screen.getByText('System Health')).toBeInTheDocument();
  });

  it('shows loading state for health check', () => {
    useAgentsStore.setState({
      agents: [{ name: 'a', status: 'active' }] as never[],
      loading: false,
    });

    renderWithProviders(<DashboardPage />);

    expect(screen.getByText('Loading...')).toBeInTheDocument();
  });

  it('displays budget values correctly', async () => {
    useAgentsStore.setState({
      agents: [{ name: 'a', status: 'active' }] as never[],
      loading: false,
    });
    useConnectionStore.setState({ state: 'authenticated' as never });

    vi.mocked(api.agents.list).mockResolvedValue({ agents: [] } as never);
    vi.mocked(api.accounts.budgetSummary).mockResolvedValue({
      total_budget_cents: 5000,
      total_spent_cents: 1234,
    } as never);
    vi.mocked(api.system.doctor).mockResolvedValue({
      checks: [],
      summary: { pass: 0, warn: 0, fail: 0 },
    } as never);

    renderWithProviders(<DashboardPage />);

    await waitFor(() => {
      expect(screen.getByText('$12.34')).toBeInTheDocument();
    });
  });
});
