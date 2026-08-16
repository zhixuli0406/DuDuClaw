import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Routes, Route } from 'react-router';
import en from '@/i18n/en.json';
import { mockWsClient } from '@/test/mocks';
import { SidebarProvider } from '@/components/mds';
import { EditAgentPage } from './EditAgentPage';
import { useAgentsStore } from '@/stores/agents-store';

// Keep the live model registry out of the smoke test — the ModelSelect only
// needs a stable, empty list here.
vi.mock('@/hooks/useAvailableModels', () => ({
  useAvailableModels: () => ({
    models: [],
    loading: false,
    error: null,
    discoveredAt: null,
    refreshing: false,
    refresh: vi.fn(),
  }),
}));

const DETAIL = {
  name: 'my-bot',
  display_name: 'My Bot',
  role: 'specialist',
  trigger: '@bot',
  icon: '🤖',
  reports_to: '',
  department: '',
  status: 'active',
  model: { preferred: 'claude-sonnet', api_mode: 'cli' },
  budget: { monthly_limit_cents: 5000, warn_threshold_percent: 80, hard_stop: true },
  heartbeat: { enabled: false },
  permissions: {},
  evolution: {},
  // contract.get / departments.list read off this same envelope; missing fields
  // fall back to defaults.
  must_not: [],
  must_always: [],
  departments: [],
};

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <Routes>
            <Route path="/agents/:id/edit" element={<EditAgentPage />} />
            <Route path="/agents" element={<div>roster-probe</div>} />
          </Routes>
        </SidebarProvider>
      </MemoryRouter>
    </IntlProvider>,
  );
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ ...DETAIL });
  useAgentsStore.setState({
    agents: [],
    loading: false,
    loaded: true,
    fetchAgents: vi.fn().mockResolvedValue(undefined),
    updateAgent: vi.fn().mockResolvedValue(undefined),
  } as never);
});

describe('EditAgentPage', () => {
  it('renders every sub-tab in the rail across both groups', async () => {
    renderAt('/agents/my-bot/edit');

    // Wait for inspect to resolve into the settings shell.
    await waitFor(() => {
      expect(screen.getByRole('tab', { name: 'General' })).toBeInTheDocument();
    });

    // WP-C: 模型 + 執行環境 collapsed into one 腦袋與引擎 rail item, so the rail
    // is eight items, not nine.
    for (const label of [
      'Skills',
      'Tools & permissions',
      'Integrations',
      'General',
      'Brain & engine',
      'Budget',
      'Automation',
      'Advanced',
    ]) {
      expect(screen.getByRole('tab', { name: label })).toBeInTheDocument();
    }
    for (const gone of ['Model', 'Runtime']) {
      expect(screen.queryByRole('tab', { name: gone })).not.toBeInTheDocument();
    }
    // Group labels present in the rail.
    expect(screen.getByText('Capabilities')).toBeInTheDocument();
    expect(screen.getByText('Settings')).toBeInTheDocument();
  });

  it('merges model + runtime controls into the one Brain & engine panel', async () => {
    renderAt('/agents/my-bot/edit?tab=brain');

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Brain & engine', level: 2 })).toBeInTheDocument();
    });
    // A control from the former 模型 tab and one from the former 執行環境 tab now
    // share a panel — the whole point of the merge.
    expect(screen.getByText('Which model')).toBeInTheDocument();
    expect(screen.getByText('Which engine')).toBeInTheDocument();
    expect(screen.getByText('Accounts it may use')).toBeInTheDocument();
    expect(screen.getByText('Helper model')).toBeInTheDocument();
  });

  it('keeps legacy ?tab=model / ?tab=runtime deep links working', async () => {
    const { unmount } = renderAt('/agents/my-bot/edit?tab=model');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Brain & engine', level: 2 })).toBeInTheDocument();
    });
    unmount();

    renderAt('/agents/my-bot/edit?tab=runtime');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Brain & engine', level: 2 })).toBeInTheDocument();
    });
  });

  it('cross-links the budget tab to the account-level ceiling (R-EDIT-SOURCE)', async () => {
    renderAt('/agents/my-bot/edit?tab=budget');
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Budget', level: 2 })).toBeInTheDocument();
    });
    expect(
      screen.getByText(/The account's own ceiling is set under Accounts & Budget/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole('button', { name: /Set an account ceiling under Accounts & Budget/ }),
    ).toBeInTheDocument();
  });

  it('honors the ?tab= query for the active panel', async () => {
    renderAt('/agents/my-bot/edit?tab=budget');

    // The Budget sub-tab heading (SettingsTab h2) is what renders when ?tab=budget.
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Budget', level: 2 })).toBeInTheDocument();
    });
    // The default General panel heading is not mounted (Base UI unmounts inactive).
    expect(screen.queryByRole('heading', { name: 'General', level: 2 })).not.toBeInTheDocument();
  });

  it('switching the rail tab swaps the visible panel', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit');

    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'General', level: 2 })).toBeInTheDocument();
    });

    await user.click(screen.getByRole('tab', { name: 'Budget' }));
    await waitFor(() => {
      expect(screen.getByRole('heading', { name: 'Budget', level: 2 })).toBeInTheDocument();
    });
  });

  it('a field edit auto-saves via updateAgent after the debounce', async () => {
    const user = userEvent.setup();
    const updateAgent = vi.fn().mockResolvedValue(undefined);
    useAgentsStore.setState({ updateAgent } as never);

    renderAt('/agents/my-bot/edit');

    // The display-name field is pre-populated from inspect on the General tab.
    const nameInput = await screen.findByDisplayValue('My Bot');
    await user.clear(nameInput);
    await user.type(nameInput, 'Renamed Bot');

    // No manual Save button — the ~1s debounce fires the single-flight save.
    expect(screen.queryByRole('button', { name: 'Save' })).not.toBeInTheDocument();

    // Allow the debounce window (1s) to elapse; the save is best-effort async.
    await waitFor(
      () => {
        expect(updateAgent).toHaveBeenCalledWith(
          'my-bot',
          expect.objectContaining({ display_name: 'Renamed Bot' }),
        );
      },
      { timeout: 3000 },
    );
  });

  // Same P05 correction as AgentDetailPage: a failed inspect is a failure, not
  // evidence that the staff member doesn't exist.
  it('reports a failed inspect as a retryable error, not as "not found"', async () => {
    mockWsClient.call.mockRejectedValue(new Error('nope'));
    renderAt('/agents/ghost/edit');
    await waitFor(() => {
      expect(screen.getByRole('alert')).toBeInTheDocument();
    });
    expect(screen.getByRole('button', { name: 'Try again' })).toBeInTheDocument();
    expect(
      screen.queryByText('This staff member could not be found'),
    ).not.toBeInTheDocument();
  });

  // The per-agent Odoo edit form used to live on OdooPage.tsx (AgentOdooOverride);
  // WP-D made that copy read-only and pointed here instead, but the read-only
  // summary flagged two fields as having no edit path on this tab yet:
  // `unblock_models` and an explicit "clear stored secret" action. Both are
  // ported here now — same `agents.update` `odoo` payload this tab already
  // writes through (single-writer), not a second RPC.
  describe('Odoo integration tab — unblock_models + clear stored secret', () => {
    it('renders the unblock_models field and a clear-secret switch for each credential', async () => {
      renderAt('/agents/my-bot/edit?tab=integration');

      await waitFor(() => {
        expect(screen.getByRole('heading', { name: 'Integrations', level: 2 })).toBeInTheDocument();
      });

      expect(screen.getByText('Unblocked Models')).toBeInTheDocument();
      expect(screen.getByPlaceholderText('res.partner')).toBeInTheDocument();

      // One "Clear stored secret" switch under API Key, one under Password.
      const clearSwitches = screen.getAllByRole('switch', { name: 'Clear stored secret' });
      expect(clearSwitches).toHaveLength(2);
      for (const sw of clearSwitches) {
        expect(sw).not.toBeChecked();
      }
    });

    it('sends unblock_models and an explicit empty api_key when clear is ticked', async () => {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(undefined);
      useAgentsStore.setState({ updateAgent } as never);

      renderAt('/agents/my-bot/edit?tab=integration');

      await waitFor(() => {
        expect(screen.getByRole('heading', { name: 'Integrations', level: 2 })).toBeInTheDocument();
      });

      // Add one unblock_models chip.
      const unblockInput = screen.getByPlaceholderText('res.partner');
      await user.type(unblockInput, 'res.partner{Enter}');
      expect(screen.getByText('res.partner')).toBeInTheDocument();

      // Tick the API key's clear-secret switch (first of the two).
      const [clearApiKeySwitch] = screen.getAllByRole('switch', { name: 'Clear stored secret' });
      await user.click(clearApiKeySwitch);

      await waitFor(
        () => {
          expect(updateAgent).toHaveBeenCalledWith(
            'my-bot',
            expect.objectContaining({
              odoo: expect.objectContaining({
                unblock_models: ['res.partner'],
                api_key: '',
              }),
            }),
          );
        },
        { timeout: 3000 },
      );

      // Clearing must win even if the operator also typed a new key — the
      // clear toggle takes precedence over whatever is in the text field.
      const call = updateAgent.mock.calls.at(-1) as [string, { odoo?: { password?: string } }];
      expect(call[1].odoo).not.toHaveProperty('password');
    });
  });

  // WP-10A follow-up — `[capabilities] git_credentials` hands this agent's
  // spawned CLI subprocess the operator's own SSH/GPG identity, so the
  // dashboard switch must go through the same danger-confirm gate as
  // computer_use / browser_via_bash / recording, default unchecked.
  describe('git_credentials danger-zone switch', () => {
    it('renders unchecked by default and only writes true after danger confirmation', async () => {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(undefined);
      useAgentsStore.setState({ updateAgent } as never);

      renderAt('/agents/my-bot/edit?tab=tools');

      await waitFor(() => {
        expect(screen.getByRole('heading', { name: 'Tools & permissions', level: 2 })).toBeInTheDocument();
      });

      const gitSwitch = screen.getByRole('switch', { name: 'Allow Git/GPG credentials' });
      expect(gitSwitch).not.toBeChecked();

      // Clicking ON must not apply immediately — it opens the shared
      // danger-confirm dialog instead of flipping the switch right away.
      await user.click(gitSwitch);
      expect(gitSwitch).not.toBeChecked();
      expect(updateAgent).not.toHaveBeenCalled();

      await waitFor(() => {
        expect(screen.getByText('Confirm high-risk setting')).toBeInTheDocument();
      });
      // The specific risk copy (not just the generic "high risk" boilerplate)
      // must be visible before the operator can confirm. Matched on a phrase
      // unique to the dialog copy — the row's own help text also mentions
      // "push git ... sign commits" so a looser match would false-positive
      // even with the dialog closed.
      expect(
        screen.getByText(/effectively your own push\/signing identity, not just git/),
      ).toBeInTheDocument();

      await user.click(screen.getByRole('button', { name: 'Confirm' }));

      await waitFor(() => {
        expect(gitSwitch).toBeChecked();
      });
      await waitFor(
        () => {
          expect(updateAgent).toHaveBeenCalledWith(
            'my-bot',
            expect.objectContaining({
              capabilities: expect.objectContaining({ git_credentials: true }),
            }),
          );
        },
        { timeout: 3000 },
      );
    });

    it('prefills a checked state from agents.inspect without re-triggering the confirm dialog', async () => {
      mockWsClient.call.mockResolvedValue({ ...DETAIL, capabilities: { git_credentials: true } });
      renderAt('/agents/my-bot/edit?tab=tools');

      const gitSwitch = await screen.findByRole('switch', { name: 'Allow Git/GPG credentials' });
      await waitFor(() => {
        expect(gitSwitch).toBeChecked();
      });
      // A value merged in from the server must never pop the confirm dialog —
      // that gate is only for operator-initiated ON clicks.
      expect(screen.queryByText('Confirm high-risk setting')).not.toBeInTheDocument();
    });
  });
});
