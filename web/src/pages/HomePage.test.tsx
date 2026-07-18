import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { HomePage } from './HomePage';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';

/**
 * WP1.5 — Multica-style home overview smoke tests. Locks in the reading-type
 * layout: the greeting line, the fixed 用量摘要 KPI strip, and the removal of the
 * PixiJS world stage from the home canvas (it now lives on `/world`).
 */
beforeEach(() => {
  vi.clearAllMocks();
  // A superset payload satisfies every home RPC accessor (each reads its own
  // field with a `?? []` guard); `available:false` keeps the KPI tiles honest.
  mockWsClient.call.mockResolvedValue({
    widgets: [],
    layout: null,
    tasks: [],
    approvals: [],
    incidents: [],
    events: [],
    channels: [],
    users: [],
    available: false,
  });
  useAgentsStore.setState({ agents: [], loading: false, error: null });
  useConnectionStore.setState({ state: 'authenticated' });
  useAuthStore.setState({
    user: { user_id: 'u1', display_name: 'Alice', role: 'employee' } as never,
  });
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('HomePage (WP1.5)', () => {
  it('renders the greeting line and the usage KPI strip', async () => {
    renderWithProviders(<HomePage />);

    // The four fixed KPI tiles (§5.5) render regardless of telemetry state.
    expect(await screen.findByText('Cost (24h)')).toBeInTheDocument();
    expect(screen.getByText('Tokens (24h)')).toBeInTheDocument();
    expect(screen.getByText('Runs (24h)')).toBeInTheDocument();
    expect(screen.getByText('Cache efficiency')).toBeInTheDocument();

    // Greeting heading carries the user's name (bucket varies by time of day).
    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading.textContent).toContain('Alice');
  });

  it('does not mount the world stage on the home canvas', async () => {
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');

    // The world stage moved to its own `/world` page — no office scene here.
    expect(screen.queryByLabelText('Office scene')).toBeNull();
    expect(document.querySelector('canvas')).toBeNull();
  });

  it('offers the layout edit affordance', async () => {
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');
    expect(screen.getByText('Edit layout')).toBeInTheDocument();
  });
});
