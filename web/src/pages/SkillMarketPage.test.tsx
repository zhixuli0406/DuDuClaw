import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { SkillMarketPage } from './SkillMarketPage';

/** Handlers registered via `client.subscribe`, keyed by event name — lets a
 *  test fire a server push the way the gateway would. */
const wsHandlers = new Map<string, (payload: unknown) => void>();

beforeEach(() => {
  vi.clearAllMocks();
  wsHandlers.clear();
  // Every RPC resolves to an empty envelope so the page renders empty states.
  mockWsClient.call.mockResolvedValue({});
  mockWsClient.subscribe.mockImplementation((event: string, handler: (p: unknown) => void) => {
    wsHandlers.set(event, handler);
    return () => wsHandlers.delete(event);
  });
  // M3: My Skills now reads only while the socket is authenticated.
  useConnectionStore.setState({ state: 'authenticated' });
  try {
    localStorage.clear();
  } catch {
    /* jsdom */
  }
});

describe('SkillMarketPage', () => {
  it('renders the collection header with the build-skill action', () => {
    renderWithProviders(<SkillMarketPage />);
    // Header title (nav.skills) + primary CTA (skills.new.title).
    expect(screen.getByRole('heading', { name: 'Skills' })).toBeInTheDocument();
    expect(screen.getByText('Build a skill')).toBeInTheDocument();
  });

  it('renders the section switcher with all four tabs', () => {
    renderWithProviders(<SkillMarketPage />);
    expect(screen.getByRole('radio', { name: 'Market' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Team Skills' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'My Skills' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Leaderboard' })).toBeInTheDocument();
  });

  it('shows the category browser on the default Market tab', () => {
    renderWithProviders(<SkillMarketPage />);
    expect(screen.getByText('Browse by Category')).toBeInTheDocument();
    expect(screen.getByText('security')).toBeInTheDocument();
  });

  it('switches to the leaderboard tab and shows its empty state', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SkillMarketPage />);
    await user.click(screen.getByRole('radio', { name: 'Leaderboard' }));
    await waitFor(() => {
      expect(
        screen.getByText('No approved skills with a time-saving estimate yet'),
      ).toBeInTheDocument();
    });
  });

  // ── WP6: channel-action → dashboard live feedback ──────────────────────

  it('refetches My Skills when a skill.changed push arrives', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list')
        return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
      if (method === 'skills.list') return Promise.resolve({ skills: [] });
      return Promise.resolve({});
    });

    renderWithProviders(<SkillMarketPage />);
    await user.click(screen.getByRole('radio', { name: 'My Skills' }));

    await waitFor(() => expect(wsHandlers.has('skill.changed')).toBe(true));
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'skills.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'skills.list').length;

    // The synthesis pipeline just graduated a skill for this agent.
    wsHandlers.get('skill.changed')?.({
      action: 'synthesized',
      agent_id: 'agnes',
      skill: 'invoice-ocr',
    });

    await waitFor(
      () => {
        const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'skills.list').length;
        expect(after).toBeGreaterThan(before);
      },
      { timeout: 2000 },
    );
  });

  // M4 — a synthesis run graduates several skills back-to-back.
  it('collapses a burst of skill.changed pushes into a single refetch', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'agents.list')
        return Promise.resolve({ agents: [{ name: 'agnes', display_name: 'Agnes' }] });
      if (method === 'skills.list') return Promise.resolve({ skills: [] });
      return Promise.resolve({});
    });

    renderWithProviders(<SkillMarketPage />);
    await user.click(screen.getByRole('radio', { name: 'My Skills' }));
    await waitFor(() => expect(wsHandlers.has('skill.changed')).toBe(true));
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.some((c) => c[0] === 'skills.list')).toBe(true),
    );
    const before = mockWsClient.call.mock.calls.filter((c) => c[0] === 'skills.list').length;

    const push = wsHandlers.get('skill.changed');
    for (let i = 0; i < 5; i++) push?.({ action: 'synthesized', agent_id: 'agnes' });

    await new Promise((r) => setTimeout(r, 700));
    const after = mockWsClient.call.mock.calls.filter((c) => c[0] === 'skills.list').length;
    expect(after).toBe(before + 1);
  });
});
