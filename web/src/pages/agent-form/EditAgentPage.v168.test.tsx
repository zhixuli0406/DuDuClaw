import { useProductPolicyStore } from '@/lib/product-policy';
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
import { useAuthStore } from '@/stores/auth-store';

// v1.68 W1 — partial writes, typed key/value editor, new controls, admin gating.

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

const BASE = {
  name: 'my-bot',
  display_name: 'My Bot',
  role: 'specialist',
  trigger: '@bot',
  icon: '🤖',
  reports_to: 'boss',
  department: 'ops',
  status: 'active',
  model: { preferred: 'claude-sonnet', api_mode: 'cli' },
  budget: { monthly_limit_cents: 5000, warn_threshold_percent: 80, hard_stop: true },
  heartbeat: { enabled: true, interval_seconds: 3600 },
  permissions: {},
  evolution: {},
  must_not: [],
  must_always: [],
  departments: [],
};

/** A v1.68 gateway's inspect: every section present. */
const FULL = {
  ...BASE,
  heartbeat: { enabled: true, interval_seconds: 3600, cron: '7 */3 1 * *', cron_timezone: 'Asia/Taipei' },
  budget: { ...BASE.budget, daily_cap_cents: 250 },
  model: { ...BASE.model, effort: 'high' },
  runtime: { provider: 'claude', fallback: '', minimal_context: false },
  fork: { enabled: true },
  team: { enabled: true, roles: { executor: { runtime: 'claude' }, verifier: { runtime: 'codex', model: 'gpt-5' } } },
  guardrails: { enabled: true, block_secrets: true, block_injection_echo: false, redact_pii: true, deny_phrases: ['internal price'] },
  memory: { decision_continuity: true, decision_ttl_days: 21 },
  night_engine: { enabled: true },
  capabilities: { approval_required_tools: ['send_message'], scoped_tools: [] },
  prompt: { cli_bare_mode: true },
};

function renderAt(path: string) {
  return render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[path]}>
        <SidebarProvider>
          <Routes>
            <Route path="/agents/:id/edit" element={<EditAgentPage />} />
          </Routes>
        </SidebarProvider>
      </MemoryRouter>
    </IntlProvider>,
  );
}

function setRole(role: 'admin' | 'manager' | 'employee') {
  useAuthStore.setState({
    user: { id: 'u1', email: 'a@b.c', display_name: 'U', role, status: 'active' },
  } as never);
}

let updateAgent: ReturnType<typeof vi.fn>;

beforeEach(() => {
  useProductPolicyStore.setState({policy:{profile:'standard',blocked_channels:[],blocked_runtimes:[],blocked_providers:[]},loading:false,error:false});
  vi.clearAllMocks();
  setRole('admin');
  mockWsClient.call.mockResolvedValue({ ...FULL });
  updateAgent = vi.fn().mockResolvedValue({ success: true });
  useAgentsStore.setState({
    agents: [],
    loading: false,
    loaded: true,
    fetchAgents: vi.fn().mockResolvedValue(undefined),
    updateAgent,
  } as never);
});

async function renameTo(user: ReturnType<typeof userEvent.setup>, name: string) {
  const input = await screen.findByDisplayValue('My Bot');
  await user.clear(input);
  await user.type(input, name);
}

const lastPayload = () => (updateAgent.mock.calls.at(-1) as [string, Record<string, unknown>])[1];

describe('heartbeat cron (audit F1)', () => {
  it('prefills the saved cron and does not send it on an unrelated edit', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit');
    await renameTo(user, 'Renamed');
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    // Exactly the edited field — no cron, no org fields, no local table.
    expect(lastPayload()).toEqual({ display_name: 'Renamed' });
  });

  it('with an older gateway (no cron in inspect) still never sends a blank cron', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ ...BASE });
    renderAt('/agents/my-bot/edit');
    await renameTo(user, 'Renamed');
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).not.toHaveProperty('heartbeat_cron');
    expect(lastPayload()).toEqual({ display_name: 'Renamed' });
  });

  it('shows the saved cron in the automation tab', async () => {
    renderAt('/agents/my-bot/edit?tab=automation');
    expect(await screen.findByDisplayValue('7 */3 1 * *')).toBeInTheDocument();
  });
});

describe('new controls round-trip from inspect to payload', () => {
  it('prefills effort, minimal context, team roles and sends only the touched role field', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=brain');

    const effort = await screen.findByRole('combobox', { name: 'Reasoning effort' });
    await waitFor(() => expect(effort).toHaveTextContent('High'));
    expect(screen.getByRole('switch', { name: 'Slim startup context' })).not.toBeChecked();
    expect(screen.getByDisplayValue('gpt-5')).toBeInTheDocument();
    expect(screen.getByRole('switch', { name: 'Allow forming a team' })).toBeChecked();

    await user.click(screen.getByRole('switch', { name: 'Slim startup context' }));
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ runtime: { minimal_context: true } });
  });

  it('warns when executor and verifier use the same vendor', async () => {
    mockWsClient.call.mockResolvedValue({
      ...FULL,
      team: { enabled: true, roles: { executor: { runtime: 'codex' }, verifier: { runtime: 'codex' } } },
    });
    renderAt('/agents/my-bot/edit?tab=brain');
    expect(await screen.findByText(en['agents.v168.team.sameVendor'])).toBeInTheDocument();
  });

  it('offers the generic command-line runtimes and notes the tool-limit gap', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=brain');
    await user.click(await screen.findByRole('combobox', { name: 'AI Backend (Provider)' }));
    await user.click(await screen.findByRole('option', { name: 'Qwen (generic command-line tool)' }));
    expect(await screen.findAllByText(en['agents.v168.genericCli.note'])).not.toHaveLength(0);
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ runtime: { provider: 'qwen' } });
  });

  it('does not offer a runtime blocked by the installation policy', async () => {
    useProductPolicyStore.setState({policy:{profile:'restricted',blocked_channels:['slack'],blocked_runtimes:['claude'],blocked_providers:['openai']}});
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=brain');
    await user.click(await screen.findByRole('combobox', { name: 'AI Backend (Provider)' }));
    expect(await screen.findByRole('option', { name: 'Qwen (generic command-line tool)' })).toBeInTheDocument();
    expect(screen.queryByRole('option', { name: 'Claude' })).not.toBeInTheDocument();
  });

  it('prefills fork + guardrails and sends one section per edit', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=tools');

    const fork = await screen.findByRole('switch', { name: 'Allow parallel branches' });
    await waitFor(() => expect(fork).toBeChecked());
    expect(screen.getByRole('switch', { name: 'Turn on reply protection' })).toBeChecked();
    expect(screen.getByRole('switch', { name: 'Block echoed injected instructions' })).not.toBeChecked();
    expect(screen.getByText('internal price')).toBeInTheDocument();

    await user.click(fork);
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ fork: { enabled: false } });
  });

  it('prefills memory + night engine and sends the edited memory key only', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=automation');

    const night = await screen.findByRole('switch', { name: 'Turn on night-time tidy-up' });
    await waitFor(() => expect(night).toBeChecked());
    expect(screen.getByRole('switch', { name: 'Decision continuity' })).toBeChecked();
    expect(screen.getByDisplayValue('21')).toBeInTheDocument();

    await user.click(screen.getByRole('switch', { name: 'Decision continuity' }));
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ memory: { decision_continuity: false } });
  });

  it('prefills the daily cap in the budget tab', async () => {
    renderAt('/agents/my-bot/edit?tab=budget');
    expect(await screen.findByText('Daily cap')).toBeInTheDocument();
    expect(screen.getByDisplayValue('2.50')).toBeInTheDocument();
  });

  it('adds a valid tool name to an approval list and refuses an invalid one', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=tools');
    await screen.findByText('send_message');

    const [approvalInput] = screen.getAllByPlaceholderText('send_message');
    await user.type(approvalInput, 'Bad-Name{Enter}');
    expect(await screen.findByText(/"Bad-Name" is not a valid tool name/)).toBeInTheDocument();

    await user.clear(approvalInput);
    await user.type(approvalInput, 'delete_file{Enter}');
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ capabilities: { approval_required_tools: ['send_message', 'delete_file'] } });
  });
});

describe('typed key/value editor', () => {
  // `[prompt]` is refused by the gateway's typed key/value write now, so the
  // editor no longer prefills it; rows are added by hand.
  async function addRow(user: ReturnType<typeof userEvent.setup>, key: string, type: string, value: string) {
    await user.click(screen.getByRole('button', { name: 'Add' }));
    const keys = screen.getAllByRole('textbox', { name: 'Key' });
    await user.type(keys[keys.length - 1], key);
    const types = screen.getAllByRole('combobox', { name: 'Type' });
    await user.selectOptions(types[types.length - 1], type);
    if (type === 'boolean') {
      const values = screen.getAllByRole('combobox', { name: 'Value' });
      await user.selectOptions(values[values.length - 1], value);
    } else {
      const values = screen.getAllByRole('textbox', { name: 'Value' });
      await user.type(values[values.length - 1], value);
    }
  }

  it('does not prefill the refused [prompt] section and sends typed values', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=advanced');

    expect(await screen.findByText(en['agents.v168.kv.title'])).toBeInTheDocument();
    expect(screen.queryByDisplayValue('cli_bare_mode')).not.toBeInTheDocument();
    await addRow(user, 'skill_synthesis_threshold', 'integer', '4');

    await waitFor(
      () =>
        expect(updateAgent).toHaveBeenCalledWith(
          'my-bot',
          {
            advanced_kv: [
              { section: 'evolution', key: 'skill_synthesis_threshold', value: 4, type: 'integer' },
            ],
          },
        ),
      { timeout: 3000 },
    );
  });

  it('flags a row in the [prompt] section as not editable here', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=advanced');
    await screen.findByText(en['agents.v168.kv.title']);
    await addRow(user, 'cli_bare_mode', 'boolean', 'true');
    const sections = screen.getAllByRole('combobox', { name: 'Section' });
    await user.clear(sections[sections.length - 1]);
    await user.type(sections[sections.length - 1], 'prompt');
    expect(screen.getByText(en['agents.v168.kv.error.section'])).toBeInTheDocument();
  });

  it('shows the server parse error inline when the save is rejected', async () => {
    const user = userEvent.setup();
    updateAgent.mockRejectedValue(new Error('agent.toml does not parse: invalid type at line 12'));
    renderAt('/agents/my-bot/edit?tab=advanced');

    await screen.findByText(en['agents.v168.kv.title']);
    await addRow(user, 'skill_synthesis_enabled', 'boolean', 'false');

    expect(
      await screen.findByText(/The server rejected this save: agent\.toml does not parse: invalid type at line 12/, {}, { timeout: 3000 }),
    ).toBeInTheDocument();
  });

  it('flags a value that does not match its type', async () => {
    const user = userEvent.setup();
    renderAt('/agents/my-bot/edit?tab=advanced');
    await screen.findByText(en['agents.v168.kv.title']);
    await addRow(user, 'skill_synthesis_threshold', 'integer', 'four');
    expect(screen.getByText(en['agents.v168.kv.error.value'])).toBeInTheDocument();
  });
});

describe('admin-only fields', () => {
  it('disables org, capability and sandbox controls for a non-admin and never sends them', async () => {
    const user = userEvent.setup();
    setRole('manager');
    renderAt('/agents/my-bot/edit');

    expect(await screen.findByRole('combobox', { name: 'Reports To' })).toBeDisabled();
    expect(screen.getByText(en['agents.v168.adminOnly.hint'])).toBeInTheDocument();

    await renameTo(user, 'Renamed');
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ display_name: 'Renamed' });
  });

  it('disables capability switches in the tools tab for a non-admin', async () => {
    const user = userEvent.setup();
    setRole('employee');
    renderAt('/agents/my-bot/edit?tab=tools');

    const git = await screen.findByRole('switch', { name: 'Allow Git/GPG credentials' });
    // Base UI switches are spans: disabled shows as data-disabled/aria-disabled.
    expect(git).toHaveAttribute('data-disabled');
    await user.click(git);
    expect(screen.queryByText('Confirm high-risk setting')).not.toBeInTheDocument();
    // The non-capability fork switch stays editable.
    expect(screen.getByRole('switch', { name: 'Allow parallel branches' })).not.toHaveAttribute('data-disabled');
  });

  it('disables the permission switches, guardrails and engine for a non-admin', async () => {
    setRole('manager');
    const { unmount } = renderAt('/agents/my-bot/edit?tab=tools');
    expect(await screen.findByRole('switch', { name: en['agents.edit.canSendCrossAgent'] })).toHaveAttribute('data-disabled');
    expect(screen.getByRole('switch', { name: en['agents.edit.canScheduleTasks'] })).toHaveAttribute('data-disabled');
    expect(screen.getByRole('switch', { name: en['agents.v168.guardrails.enabled'] })).toHaveAttribute('data-disabled');
    unmount();
    renderAt('/agents/my-bot/edit?tab=brain');
    expect(await screen.findByRole('combobox', { name: en['agents.runtime.provider'] })).toBeDisabled();
  });

  it('keeps the controls enabled for an admin', async () => {
    renderAt('/agents/my-bot/edit');
    expect(await screen.findByRole('combobox', { name: 'Reports To' })).not.toBeDisabled();
    expect(screen.queryByText(en['agents.v168.adminOnly.hint'])).not.toBeInTheDocument();
  });
});

describe('removed dead controls and enforced permissions', () => {
  it('no longer shows sticker, stagnation, container extras or skill scan/auto-activate controls', async () => {
    const { unmount } = renderAt('/agents/my-bot/edit?tab=advanced');
    await screen.findByText(en['agents.v168.kv.title']);
    expect(screen.queryByText(en['agents.edit.sticker'])).not.toBeInTheDocument();
    unmount();

    const r2 = renderAt('/agents/my-bot/edit?tab=skills');
    expect(await screen.findByText(en['agents.v168.skillScanAlways'])).toBeInTheDocument();
    expect(screen.queryByRole('switch', { name: en['agents.edit.skillSecurityScan'] })).not.toBeInTheDocument();
    expect(screen.queryByRole('switch', { name: en['agents.edit.skillAutoActivate'] })).not.toBeInTheDocument();
    expect(screen.getByText(en['agents.v168.perm.canModifySkills.help'])).toBeInTheDocument();
    r2.unmount();

    const r3 = renderAt('/agents/my-bot/edit?tab=brain');
    await screen.findByRole('switch', { name: 'Sandbox Isolation' });
    expect(screen.queryByRole('switch', { name: en['agents.edit.readonlyProject'] })).not.toBeInTheDocument();
    expect(screen.queryByText(en['agents.container.cmd'])).not.toBeInTheDocument();
    r3.unmount();

    renderAt('/agents/my-bot/edit?tab=automation');
    await screen.findByRole('switch', { name: 'Turn on night-time tidy-up' });
    expect(screen.queryByText(en['agents.adv.stagnation'])).not.toBeInTheDocument();
    expect(screen.queryByText(en['agents.adv.tokenBudgetPerCheck'])).not.toBeInTheDocument();
  });

  it('prefills the permission toggles from inspect and explains what each blocks', async () => {
    mockWsClient.call.mockResolvedValue({
      ...FULL,
      permissions: { can_send_cross_agent: false, can_schedule_tasks: true, permissions_enforced_since: '2026-10-03' },
    });
    renderAt('/agents/my-bot/edit?tab=tools');
    const cross = await screen.findByRole('switch', { name: en['agents.edit.canSendCrossAgent'] });
    await waitFor(() => expect(cross).not.toBeChecked());
    expect(screen.getByRole('switch', { name: en['agents.edit.canScheduleTasks'] })).toBeChecked();
    expect(screen.getByText(en['agents.v168.perm.canScheduleTasks.help'])).toBeInTheDocument();
    expect(screen.getByText(en['agents.v168.perm.canCreateAgents.help'])).toBeInTheDocument();
  });

  it('shows absent permission keys as ON (allowed server-side) and sends nothing for them', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ ...FULL, permissions: {} });
    renderAt('/agents/my-bot/edit?tab=tools');
    for (const id of ['agents.edit.canSendCrossAgent', 'agents.edit.canScheduleTasks', 'agents.edit.canCreateAgents'] as const) {
      const sw = await screen.findByRole('switch', { name: en[id] });
      await waitFor(() => expect(sw).toBeChecked());
    }
    await user.click(screen.getByRole('switch', { name: 'Allow parallel branches' }));
    await waitFor(() => expect(updateAgent).toHaveBeenCalled(), { timeout: 3000 });
    expect(lastPayload()).toEqual({ fork: { enabled: false } });
  });
});

describe('boundaries (contract) review', () => {
  const contractCalls = () =>
    mockWsClient.call.mock.calls.filter(([m]) => m === 'contract.update') as Array<[string, Record<string, unknown>]>;

  function mockContract(update: (params: Record<string, unknown>) => unknown) {
    mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown>) => {
      if (method === 'contract.get') {
        return { agent_id: 'my-bot', must_not: [], must_always: [], max_tool_calls_per_turn: 0, hash: 'h1' };
      }
      if (method === 'contract.update') return update(params);
      return { ...FULL };
    });
  }

  const held = {
    status: 'needs_confirmation',
    file: 'contract',
    agent_id: 'my-bot',
    review: {
      analysis_id: 'an-7',
      verdict: 'warn',
      basis: 'rules+ai',
      summary: '',
      reasons: [{ category: 'loosened_redline', severity: 'medium', message: 'x', evidence: '' }],
      can_confirm: true,
      can_request_approval: true,
    },
  };

  it('opens the review dialog on needs_confirmation and confirms with the analysis id', async () => {
    const user = userEvent.setup();
    mockContract((p) => (p.action === 'confirm'
      ? { status: 'applied', file: 'contract', agent_id: 'my-bot', hash: 'h2' }
      : held));
    renderAt('/agents/my-bot/edit?tab=tools');

    const box = await screen.findByRole('textbox', { name: en['agents.contract.mustNot'] });
    await user.type(box, 'share prices');
    const dialog = await screen.findByRole('dialog', {}, { timeout: 3000 });
    expect(dialog).toHaveAccessibleName(en['agentFiles.review.title.needsConfirmation']);
    expect(contractCalls()).toHaveLength(1);
    expect(contractCalls()[0][1]).toEqual({
      agent_id: 'my-bot',
      must_not: ['share prices'],
      must_always: [],
      max_tool_calls_per_turn: 0,
      base_hash: 'h1',
    });

    await user.type(screen.getByLabelText(en['agentFiles.review.reasonLabel']), 'the shop wants it');
    await user.click(screen.getByRole('button', { name: en['agentFiles.review.confirm'] }));

    await waitFor(() => expect(contractCalls()).toHaveLength(2));
    expect(contractCalls()[1][1]).toEqual({
      agent_id: 'my-bot',
      must_not: ['share prices'],
      must_always: [],
      max_tool_calls_per_turn: 0,
      action: 'confirm',
      analysis_id: 'an-7',
      reason: 'the shop wants it',
      base_hash: 'h1',
    });
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    // The held answer is not re-sent by the autosave.
    await new Promise((r) => setTimeout(r, 1300));
    expect(contractCalls()).toHaveLength(2);
  });

  it('sends a non-admin owner\'s request to an admin', async () => {
    const user = userEvent.setup();
    setRole('employee');
    useAuthStore.setState({
      bindings: [{ user_id: 'u1', agent_name: 'my-bot', access_level: 'owner', bound_at: '2026-10-01' }],
    } as never);
    mockContract((p) => (p.action === 'request_approval'
      ? { status: 'pending_approval', file: 'contract', agent_id: 'my-bot', approval_id: 'apr-12345678-x' }
      : { ...held, status: 'blocked', review: { ...held.review, verdict: 'block', can_confirm: false } }));
    renderAt('/agents/my-bot/edit?tab=tools');

    const box = await screen.findByRole('textbox', { name: en['agents.contract.mustNot'] });
    await user.type(box, 'nothing');
    await screen.findByRole('dialog', {}, { timeout: 3000 });
    expect(screen.queryByRole('button', { name: en['agentFiles.review.confirm'] })).not.toBeInTheDocument();
    await user.type(screen.getByLabelText(en['agentFiles.review.reasonLabel']), 'please review');
    await user.click(screen.getByRole('button', { name: en['agentFiles.review.requestApproval'] }));

    await waitFor(() => expect(contractCalls()).toHaveLength(2));
    expect(contractCalls()[1][1]).toMatchObject({ action: 'request_approval', analysis_id: 'an-7', reason: 'please review' });
    expect(await screen.findByRole('dialog')).toHaveAccessibleName(en['agentFiles.review.title.pending']);
    useAuthStore.setState({ bindings: [] } as never);
  });

  it('reloads the boundaries after a conflict', async () => {
    const user = userEvent.setup();
    mockContract(() => ({ status: 'conflict', file: 'contract', agent_id: 'my-bot', current_hash: 'h9' }));
    renderAt('/agents/my-bot/edit?tab=tools');
    const box = await screen.findByRole('textbox', { name: en['agents.contract.mustNot'] });
    await user.type(box, 'mine');
    await screen.findByRole('dialog', {}, { timeout: 3000 });
    const getsBefore = mockWsClient.call.mock.calls.filter(([m]) => m === 'contract.get').length;
    await user.click(screen.getByRole('button', { name: en['agentFiles.review.reload'] }));
    await waitFor(() =>
      expect(mockWsClient.call.mock.calls.filter(([m]) => m === 'contract.get').length).toBe(getsBefore + 1),
    );
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.getByRole('textbox', { name: en['agents.contract.mustNot'] })).toHaveValue('');
  });

  it('is read-only for someone who neither owns the employee nor is an admin', async () => {
    setRole('employee');
    useAuthStore.setState({
      bindings: [{ user_id: 'u1', agent_name: 'my-bot', access_level: 'viewer', bound_at: '2026-10-01' }],
    } as never);
    mockContract(() => held);
    renderAt('/agents/my-bot/edit?tab=tools');
    const box = await screen.findByRole('textbox', { name: en['agents.contract.mustNot'] });
    expect(box).toBeDisabled();
    expect(screen.getByText(en['agents.contract.readOnly'])).toBeInTheDocument();
    useAuthStore.setState({ bindings: [] } as never);
  });
});
