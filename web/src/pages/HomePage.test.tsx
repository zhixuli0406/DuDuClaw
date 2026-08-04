import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { HomePage, greetingName } from './HomePage';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useAgentsStore } from '@/stores/agents-store';
import { useSystemStore } from '@/stores/system-store';

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
  useSystemStore.setState({ status: null } as never);
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

/**
 * WP18-C (2026-08-04 client report): a fresh single-owner install greeted its
 * owner with 「晚安 Administrator」 — the seeded DB placeholder read back as a
 * name. The onboarding wizard now asks 「怎麼稱呼你」 and writes it to the
 * account; when it is left blank the greeting falls back to the warm default.
 */
describe('HomePage greeting name (WP18-C)', () => {
  it('shows the name the owner set, and falls back when only the placeholder is stored', () => {
    // Set → shown, on both editions.
    expect(greetingName('Louis', true, 'there')).toBe('Louis');
    expect(greetingName('Louis', false, 'there')).toBe('Louis');
    // Not set → the seeded placeholder is treated as absent on Personal…
    expect(greetingName('Administrator', true, 'there')).toBe('there');
    expect(greetingName('  ', true, 'there')).toBe('there');
    expect(greetingName(undefined, true, 'there')).toBe('there');
    // …but Enterprise is untouched: an admin typed that name on purpose.
    expect(greetingName('Administrator', false, 'there')).toBe('Administrator');
  });

  it('personal edition: renders the fallback instead of the seeded placeholder', async () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    useAuthStore.setState({
      user: { user_id: 'u1', id: 'u1', display_name: 'Administrator', role: 'admin' } as never,
    });
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');

    const heading = screen.getByRole('heading', { level: 1 });
    expect(heading.textContent).not.toContain('Administrator');
    expect(heading.textContent).toContain('there');
  });

  it('personal edition: renders the name saved by the onboarding wizard', async () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    useAuthStore.setState({
      user: { user_id: 'u1', id: 'u1', display_name: '小李', role: 'admin' } as never,
    });
    renderWithProviders(<HomePage />);
    await screen.findByText('Cost (24h)');

    expect(screen.getByRole('heading', { level: 1 }).textContent).toContain('小李');
  });
});
