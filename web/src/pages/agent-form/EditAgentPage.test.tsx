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
import { toast } from '@/lib/toast';
import { useAuthStore } from '@/stores/auth-store';

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
  // v1.68 — capability / org / sandbox fields are admin-only; these tests
  // exercise them as an administrator (non-admin gating is covered in
  // EditAgentPage.v168.test.tsx).
  useAuthStore.setState({
    user: { id: 'u-admin', email: 'a@b.c', display_name: 'Admin', role: 'admin', status: 'active' },
  } as never);
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

  it('warns that sandboxed tasks are refused only when sandbox is on and network is off', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=brain');

    const sandboxSwitch = await screen.findByRole('switch', { name: 'Sandbox Isolation' });
    const hint = en['agents.edit.sandbox.needsNetwork'];
    expect(screen.queryByText(hint)).not.toBeInTheDocument();

    await user.click(sandboxSwitch);
    expect(await screen.findByText(hint)).toBeInTheDocument();

    await user.click(sandboxSwitch);
    await waitFor(() => expect(screen.queryByText(hint)).not.toBeInTheDocument());
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
  // Gemini CLI runtime deprecation: no longer offered, but a saved value stays
  // visible and labelled; Antigravity is offered instead.
  describe('runtime pickers — Gemini CLI deprecation', () => {
    it('offers Antigravity and not Gemini when the saved provider is claude', async () => {
      const user = userEvent.setup();
      renderAt('/agents/my-bot/edit?tab=brain');

      await user.click(await screen.findByRole('combobox', { name: 'AI Backend (Provider)' }));
      expect(await screen.findByRole('option', { name: 'Antigravity (Google)' })).toBeInTheDocument();
      expect(screen.queryByRole('option', { name: /Gemini/ })).not.toBeInTheDocument();
    });

    it('keeps a saved gemini provider visible, labelled deprecated', async () => {
      const user = userEvent.setup();
      mockWsClient.call.mockResolvedValue({ ...DETAIL, runtime: { provider: 'gemini', fallback: '' } });
      renderAt('/agents/my-bot/edit?tab=brain');

      const picker = await screen.findByRole('combobox', { name: 'AI Backend (Provider)' });
      await waitFor(() => {
        expect(picker).toHaveTextContent('Gemini (deprecated)');
      });
      await user.click(picker);
      expect(await screen.findByRole('option', { name: 'Gemini (deprecated)' })).toBeInTheDocument();
      expect(screen.getByRole('option', { name: 'Antigravity (Google)' })).toBeInTheDocument();
    });

    it('keeps a saved gemini fallback visible, labelled deprecated', async () => {
      const user = userEvent.setup();
      mockWsClient.call.mockResolvedValue({ ...DETAIL, runtime: { provider: 'claude', fallback: 'gemini' } });
      renderAt('/agents/my-bot/edit?tab=brain');

      const picker = await screen.findByRole('combobox', { name: 'Fallback Backend' });
      await waitFor(() => {
        expect(picker).toHaveTextContent('Gemini (deprecated)');
      });
      await user.click(picker);
      expect(await screen.findByRole('option', { name: 'Gemini (deprecated)' })).toBeInTheDocument();
    });
  });

  describe('save-time runtime auto-align feedback', () => {
    async function saveWith(res: unknown) {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(res);
      useAgentsStore.setState({ updateAgent } as never);
      renderAt('/agents/my-bot/edit');
      const nameInput = await screen.findByDisplayValue('My Bot');
      await user.clear(nameInput);
      await user.type(nameInput, 'Renamed Bot');
      await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
      // let the post-save handler run
      await waitFor(() => expect(updateAgent).toHaveReturned());
      await new Promise((r) => setTimeout(r, 50));
    }

    it('warns when the gateway skipped auto-align for a deprecated runtime', async () => {
      const info = vi.spyOn(toast, 'info').mockImplementation(() => {});
      await saveWith({ success: true, runtime_provider_align_skipped: 'deprecated_runtime' });
      expect(info).toHaveBeenCalledWith(
        en['agents.edit.runtimeAlignSkipped'],
        expect.anything(),
      );
      info.mockRestore();
    });

    it('stays silent when the response carries no skip marker', async () => {
      const info = vi.spyOn(toast, 'info').mockImplementation(() => {});
      await saveWith({ success: true, runtime_provider_align_skipped: null });
      expect(info).not.toHaveBeenCalled();
      info.mockRestore();
    });
  });

  describe('saved gemini provider survives an unrelated runtime edit', () => {
    // v1.68 partial writes: an untouched provider is not sent at all, so the
    // saved gemini value cannot be replaced by the picker.
    it('sends only the fallback when only the fallback changes, keeping gemini labelled deprecated', async () => {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(undefined);
      useAgentsStore.setState({ updateAgent } as never);
      mockWsClient.call.mockResolvedValue({ ...DETAIL, runtime: { provider: 'gemini', fallback: '' } });
      renderAt('/agents/my-bot/edit?tab=brain');

      const provider = await screen.findByRole('combobox', { name: 'AI Backend (Provider)' });
      await waitFor(() => expect(provider).toHaveTextContent('Gemini (deprecated)'));

      await user.click(await screen.findByRole('combobox', { name: 'Fallback Backend' }));
      await user.click(await screen.findByRole('option', { name: 'Antigravity (Google)' }));

      await waitFor(
        () => {
          expect(updateAgent).toHaveBeenCalledWith(
            'my-bot',
            expect.objectContaining({
              runtime: { fallback: 'antigravity' },
            }),
          );
        },
        { timeout: 3000 },
      );
      expect(screen.getByRole('combobox', { name: 'AI Backend (Provider)' })).toHaveTextContent('Gemini (deprecated)');
    });
  });

  describe('computer-use mode — native removed', () => {
    const openModePicker = async () => {
      const picker = await screen.findByRole('combobox', { name: 'Computer Use Mode' });
      return picker;
    };

    it('does not offer native as a new choice', async () => {
      const user = userEvent.setup();
      renderAt('/agents/my-bot/edit?tab=tools');

      await user.click(await openModePicker());
      expect(await screen.findByRole('option', { name: 'Container-isolated' })).toBeInTheDocument();
      expect(screen.getByRole('option', { name: 'Auto' })).toBeInTheDocument();
      expect(screen.queryByRole('option', { name: /Direct on this machine/ })).not.toBeInTheDocument();
      expect(screen.queryByText(/has been removed/)).not.toBeInTheDocument();
    });

    it('keeps a saved native value visible, labelled removed, with the notice', async () => {
      mockWsClient.call.mockResolvedValue({ ...DETAIL, capabilities: { computer_use_mode: 'native' } });
      renderAt('/agents/my-bot/edit?tab=tools');

      const picker = await openModePicker();
      await waitFor(() => expect(picker).toHaveTextContent('Direct on this machine (removed)'));
      expect(screen.getByText(/has been removed\. Computer operation will not start/)).toBeInTheDocument();
    });

    it('does not replace a saved native value when an unrelated capability is saved', async () => {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(undefined);
      useAgentsStore.setState({ updateAgent } as never);
      mockWsClient.call.mockResolvedValue({ ...DETAIL, capabilities: { computer_use_mode: 'native' } });
      renderAt('/agents/my-bot/edit?tab=tools');

      const picker = await openModePicker();
      await waitFor(() => expect(picker).toHaveTextContent('Direct on this machine (removed)'));

      await user.click(screen.getByRole('switch', { name: 'Allow Git/GPG credentials' }));
      await user.click(await screen.findByRole('button', { name: 'Confirm' }));

      await waitFor(
        () => {
          expect(updateAgent).toHaveBeenCalledWith(
            'my-bot',
            expect.objectContaining({
              capabilities: { git_credentials: true },
            }),
          );
        },
        { timeout: 3000 },
      );
      expect(screen.getByRole('combobox', { name: 'Computer Use Mode' })).toHaveTextContent('Direct on this machine (removed)');
    });
  });

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

  // O-4 follow-up — `[capabilities] system_operator` marks this agent as the
  // machine's designated operator persona (the `os_*` system-operation tool
  // face). Same danger-confirm gate and default-off contract as
  // git_credentials above, plus a persistent high-risk banner once enabled.
  describe('system_operator danger-zone switch', () => {
    it('renders unchecked by default and only writes true after danger confirmation', async () => {
      const user = userEvent.setup();
      const updateAgent = vi.fn().mockResolvedValue(undefined);
      useAgentsStore.setState({ updateAgent } as never);

      renderAt('/agents/my-bot/edit?tab=tools');

      await waitFor(() => {
        expect(screen.getByRole('heading', { name: 'Tools & permissions', level: 2 })).toBeInTheDocument();
      });

      const opSwitch = screen.getByRole('switch', { name: 'Allow operating this computer' });
      expect(opSwitch).not.toBeChecked();
      // No standing high-risk banner while the capability is off.
      expect(screen.queryByText(/Highest permission tier/)).not.toBeInTheDocument();

      // Clicking ON must not apply immediately — it opens the shared
      // danger-confirm dialog instead of flipping the switch right away.
      await user.click(opSwitch);
      expect(opSwitch).not.toBeChecked();
      expect(updateAgent).not.toHaveBeenCalled();

      await waitFor(() => {
        expect(screen.getByText('Confirm high-risk setting')).toBeInTheDocument();
      });
      // The specific risk copy (not just the generic "high risk" boilerplate)
      // must be visible before the operator can confirm.
      expect(
        screen.getByText(/the same as handing over control of the machine itself/),
      ).toBeInTheDocument();

      await user.click(screen.getByRole('button', { name: 'Confirm' }));

      await waitFor(() => {
        expect(opSwitch).toBeChecked();
      });
      // The persistent high-risk banner appears once the switch is on.
      expect(screen.getByText(/Highest permission tier/)).toBeInTheDocument();
      await waitFor(
        () => {
          expect(updateAgent).toHaveBeenCalledWith(
            'my-bot',
            expect.objectContaining({
              capabilities: expect.objectContaining({ system_operator: true }),
            }),
          );
        },
        { timeout: 3000 },
      );
    });

    it('prefills a checked state from agents.inspect without re-triggering the confirm dialog', async () => {
      mockWsClient.call.mockResolvedValue({ ...DETAIL, capabilities: { system_operator: true } });
      renderAt('/agents/my-bot/edit?tab=tools');

      const opSwitch = await screen.findByRole('switch', { name: 'Allow operating this computer' });
      await waitFor(() => {
        expect(opSwitch).toBeChecked();
      });
      // A value merged in from the server must never pop the confirm dialog —
      // that gate is only for operator-initiated ON clicks.
      expect(screen.queryByText('Confirm high-risk setting')).not.toBeInTheDocument();
    });
  });
});
