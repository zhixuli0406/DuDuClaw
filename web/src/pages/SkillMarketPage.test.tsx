import { setConfirmedCallableAgents } from '@/test/callable-agents';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useConnectionStore } from '@/stores/connection-store';
import { SkillMarketPage } from './SkillMarketPage';

/** Handlers registered via `client.subscribe`, keyed by event name — lets a
 *  test fire a server push the way the gateway would. */
const wsHandlers = new Map<string, (payload: unknown) => void>();

beforeEach(() => {
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never, bindings: [] });
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

  // ── "The skill library shows nothing" ────────────────────────────────
  //
  // The tab used to open on `agents[0]`, which is whichever agent the
  // gateway's HashMap yielded first — unrelated to the staffer the customer
  // actually talks to. Skills sitting on any other staffer were invisible with
  // no hint that a picker was the reason.

  describe('My Skills', () => {
    const twoAgents = [
      { name: 'agnes', display_name: 'Agnes' },
      { name: 'bruno', display_name: 'Bruno' },
    ];

    it('defaults to the aggregate view and shows skills from every staffer', async () => {
      const user = userEvent.setup();
      mockWsClient.call.mockImplementation((method: string, params?: unknown) => {
        if (method === 'agents.list') return Promise.resolve({ agents: twoAgents });
        if (method === 'skills.list') {
          // The aggregate call carries no agent_id.
          expect((params as { agent_id?: string } | undefined)?.agent_id).toBeUndefined();
          return Promise.resolve({
            global_skills: [{ name: 'company-tone', content: '', scope: 'global' }],
            agents: [
              { agent_id: 'agnes', skills: [] },
              { agent_id: 'bruno', skills: [{ name: 'invoice-ocr', content: '', scope: 'agent' }] },
            ],
          });
        }
        return Promise.resolve({});
      });

      renderWithProviders(<SkillMarketPage />);
      await user.click(screen.getByRole('radio', { name: 'My Skills' }));

      // Belongs to a staffer the old default would never have selected.
      await waitFor(() => expect(screen.getByText('invoice-ocr')).toBeInTheDocument());
      expect(screen.getByText('company-tone')).toBeInTheDocument();
      expect(screen.getByText('Bruno')).toBeInTheDocument();
    });

    it('names the folders it scanned when the list is empty', async () => {
      const user = userEvent.setup();
      mockWsClient.call.mockImplementation((method: string) => {
        if (method === 'agents.list') return Promise.resolve({ agents: twoAgents });
        if (method === 'skills.list')
          return Promise.resolve({
            global_skills: [],
            agents: [],
            scanned: [
              { layer: 'global', path: '/home/.duduclaw/skills', exists: false, count: 0 },
              {
                layer: 'agent',
                path: '/home/.duduclaw/agents/agnes/SKILLS',
                exists: true,
                count: 0,
              },
            ],
          });
        return Promise.resolve({});
      });

      renderWithProviders(<SkillMarketPage />);
      await user.click(screen.getByRole('radio', { name: 'My Skills' }));

      await waitFor(() => expect(screen.getByText('No skills here yet')).toBeInTheDocument());
      expect(screen.getByText('/home/.duduclaw/skills')).toBeInTheDocument();
      expect(screen.getByText('does not exist')).toBeInTheDocument();
      expect(screen.getByText('/home/.duduclaw/agents/agnes/SKILLS')).toBeInTheDocument();
    });

    it('shows a read failure as an error, not as "you have no skills"', async () => {
      const user = userEvent.setup();
      mockWsClient.call.mockImplementation((method: string) => {
        if (method === 'agents.list') return Promise.resolve({ agents: twoAgents });
        if (method === 'skills.list') return Promise.reject(new Error('Agent not found: agnes'));
        return Promise.resolve({});
      });

      renderWithProviders(<SkillMarketPage />);
      await user.click(screen.getByRole('radio', { name: 'My Skills' }));

      await waitFor(() =>
        expect(screen.getByText('Could not read the skill list')).toBeInTheDocument(),
      );
      expect(screen.getByText(/Agent not found: agnes/)).toBeInTheDocument();
      expect(screen.queryByText('No skills here yet')).not.toBeInTheDocument();
    });
  });

  // A market index that never loaded returns zero hits for every query, which
  // is indistinguishable from "nothing matched" unless we read `total_indexed`.
  it('distinguishes an unloaded market index from an empty result set', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'skills.search') return Promise.resolve({ skills: [], total_indexed: 0 });
      return Promise.resolve({});
    });

    renderWithProviders(<SkillMarketPage />);
    await user.click(screen.getByText('security'));

    await waitFor(() =>
      expect(
        screen.getByText(
          'The market index has not loaded — check the connection to GitHub and try again.',
        ),
      ).toBeInTheDocument(),
    );
    expect(screen.queryByText('No matching skills found')).not.toBeInTheDocument();
  });
});

import { useAuthStore } from '@/stores/auth-store';

// Start these component scenarios after the gateway confirmed the admin roster.
beforeEach(() => setConfirmedCallableAgents(true));

import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { canSelectAgent, callableSubject, useCallableAgentsStore } from '@/stores/callable-agents-store';

it('clears a revoked installation scope and does not submit an approval request', async () => {
  const user = userEvent.setup();
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never,
    bindings: [{ agent_name: 'agnes', access_level: 'owner' }] as never });
  useAgentsStore.setState({ agents: [{ name: 'agnes', display_name: 'Agnes' }] as never, fetchAgents: vi.fn() });
  vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [{ name: 'agnes', display_name: 'Agnes', via: 'binding', selectable: true }], unrestricted: false, default_agent: null } as never);
  vi.spyOn(api.skillMarket, 'search').mockResolvedValue({ skills: [{ name: 'Test skill', url: 'https://example.com/skill', tags: [] }], total_indexed: 1 } as never);
  vi.spyOn(api.skills, 'vet').mockResolvedValue({ passed: true, content: 'safe', vet_result: { score: 100, findings: [] } } as never);
  const request = vi.spyOn(api.skills, 'installRequest').mockResolvedValue({ stage: 'awaiting_admin' } as never);
  renderWithProviders(<SkillMarketPage />);
  await user.click(screen.getByText('security'));
  await user.click(await screen.findByRole('button', { name: 'Install' }));
  const dialog = await screen.findByRole('dialog');
  await user.click(within(dialog).getByRole('combobox'));
  await user.click(await screen.findByRole('option', { name: 'Agent: Agnes' }));
  await waitFor(() => expect(within(dialog).getByRole('combobox')).toHaveTextContent('Agent: Agnes'));
  await user.click(within(dialog).getByRole('button', { name: 'Run Security Scan' }));
  const submit = within(dialog).getByRole('button', { name: 'Submit for approval' });
  await waitFor(() => expect(submit).toBeEnabled());
  vi.mocked(api.agents.callable).mockResolvedValue({ agents: [], unrestricted: false, default_agent: null });
  act(() => {
    const key = JSON.stringify([callableSubject(), null]);
    useCallableAgentsStore.setState({ entries: { [key]: { agents: [], unrestricted: false, loading: false, confirmedAt: Date.now() + 1 } } });
    fireEvent.click(submit);
  });
  expect(request).not.toHaveBeenCalled();
  await waitFor(() => expect({
    scope: within(dialog).getByRole('combobox').textContent,
    allowed: canSelectAgent('agnes'),
    disabled: screen.getByRole<HTMLButtonElement>('button', { name: 'Submit for approval' }).disabled,
  }).toEqual({ scope: 'Install Scope', allowed: false, disabled: true }));
  expect(within(dialog).getByRole('combobox')).not.toHaveTextContent('Agnes');
});
