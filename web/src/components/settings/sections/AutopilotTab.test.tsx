import { useCompanyTimeStore } from '@/lib/company-time';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const listMock = vi.fn();
const createMock = vi.fn();
const updateMock = vi.fn();
const agentsListMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    users: { list: vi.fn().mockResolvedValue({ users: [
      { id: 'creator-secret-id', display_name: '王小明', email: 'creator@example.com' },
      { id: 'updater-secret-id', display_name: '張雅婷', email: 'updater@example.com' },
    ] }) },
    autopilot: {
      list: () => listMock(),
      create: (p: unknown) => createMock(p),
      update: (id: string, f: unknown) => updateMock(id, f),
      remove: vi.fn(),
      history: vi.fn().mockResolvedValue({ entries: [] }),
    },
    agents: {
      list: () => agentsListMock(),
    },
    ticks: {
      sources: vi.fn().mockResolvedValue({ enabled: false, sources: [] }),
      recent: vi.fn().mockResolvedValue({ ticks: [] }),
    },
  },
}));

import type { AutopilotRule } from '@/lib/autopilot-rules';
import { AutopilotTab } from './AutopilotTab';

const TEMPLATE_RULE = {
  id: 'r1',
  name: 'Adverse reaction alert',
  enabled: true,
  trigger_event: 'channel_message',
  conditions: { any: [{ field: 'text', op: 'contains', value: 'rash' }] },
  action: { type: 'notify', channel: 'line', chat_id: 'C999', text: 'Pharmacist needed', screen: { mode: 'local' } },
  created_at: '2026-10-01T00:00:00Z',
  trigger_count: 2,
  sequence: null,
  metadata: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  useCompanyTimeStore.setState({ zone: null, status: 'unset', source: null });
  listMock.mockResolvedValue({ rules: [TEMPLATE_RULE] });
  agentsListMock.mockResolvedValue({
    agents: [{ name: 'sales-lead', display_name: 'Sales lead', icon: '' }],
  });
});

describe('<AutopilotTab>', () => {
  it('lists a template rule with its trigger, action and destination', async () => {
    renderWithProviders(<AutopilotTab />);
    expect(await screen.findByText('Adverse reaction alert')).toBeTruthy();
    expect(screen.getByText('Channel message received')).toBeTruthy();
    expect(screen.getByText('Send notification')).toBeTruthy();
    expect(screen.getByText('(line)')).toBeTruthy();
  });

  it('shows the server validation error inside the dialog when a save is refused', async () => {
    createMock.mockRejectedValue(new Error("action.prompt is required for type 'delegate'"));
    renderWithProviders(<AutopilotTab />);
    await screen.findByText('Adverse reaction alert');

    fireEvent.click(screen.getByRole('button', { name: 'New Rule' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('Rule name'), { target: { value: 'Triage' } });
    await waitFor(() => expect(agentsListMock).toHaveBeenCalled());
    fireEvent.change(within(dialog).getByLabelText('What the AI employee should do'), {
      target: { value: 'Look at {task.title}' },
    });
    fireEvent.click(within(dialog).getByRole('button', { name: 'New Rule' }));

    await waitFor(() => expect(createMock).toHaveBeenCalledTimes(1));
    expect(createMock.mock.calls[0][0]).toEqual({
      name: 'Triage',
      trigger_event: 'task_created',
      conditions: { all: [] },
      action: { type: 'delegate', target_agent: 'sales-lead', prompt: 'Look at {task.title}' },
    });
    const alert = await within(dialog).findByRole('alert');
    expect(alert.textContent).toContain('The rule was not saved');
    expect(alert.textContent).toContain("action.prompt is required for type 'delegate'");
    // The dialog stays open so the person can fix the input.
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  it('lists missing fields locally without calling the server', async () => {
    renderWithProviders(<AutopilotTab />);
    await screen.findByText('Adverse reaction alert');
    fireEvent.click(screen.getByRole('button', { name: 'New Rule' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: 'New Rule' }));
    const alert = await within(dialog).findByRole('alert');
    expect(alert.textContent).toContain('Please fill in "Rule name"');
    expect(createMock).not.toHaveBeenCalled();
  });

  it('editing a template rule sends only the changed fields and keeps extras', async () => {
    updateMock.mockResolvedValue({ rule: TEMPLATE_RULE });
    renderWithProviders(<AutopilotTab />);
    await screen.findByText('Adverse reaction alert');
    fireEvent.click(screen.getByRole('button', { name: 'Edit rule' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('Notification text'), {
      target: { value: 'Pharmacist, please take over' },
    });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMock).toHaveBeenCalledTimes(1));
    expect(updateMock.mock.calls[0]).toEqual([
      'r1',
      {
        action: {
          type: 'notify',
          channel: 'line',
          chat_id: 'C999',
          text: 'Pharmacist, please take over',
          screen: { mode: 'local' },
        },
      },
    ]);
  });

  it('says a rule without conditions runs every time the trigger happens', async () => {
    renderWithProviders(<AutopilotTab />);
    await screen.findByText('Adverse reaction alert');
    fireEvent.click(screen.getByRole('button', { name: 'New Rule' }));
    const dialog = await screen.findByRole('dialog');
    expect(
      within(dialog).getByText('No conditions: the rule runs every time the trigger happens.'),
    ).toBeTruthy();
  });
});



it('shows the company zone and uses it for the last trigger and history', async () => {
  useCompanyTimeStore.setState({ zone: 'Asia/Tokyo', status: 'set', source: null });
  const timestamp = '2026-10-01T00:00:00Z';
  listMock.mockResolvedValue({ rules: [{ ...TEMPLATE_RULE, last_triggered_at: timestamp }] });
  const { api } = await import('@/lib/api');
  vi.mocked(api.autopilot.history).mockResolvedValueOnce({ entries: [{
    id: 'h1', rule_id: 'r1', rule_name: TEMPLATE_RULE.name, triggered_at: timestamp, result: 'success', details: null,
  }] });
  renderWithProviders(<AutopilotTab />);
  expect(await screen.findByText('Times shown in (Asia/Tokyo)')).toBeInTheDocument();
  expect(screen.getByText(/Last triggered:.*9:00/)).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Trigger History' }));
  const dialog = await screen.findByRole('dialog');
  expect(within(dialog).getByRole('heading', { name: 'Trigger History (Asia/Tokyo)' })).toBeInTheDocument();
  expect(within(dialog).getByText(/9:00/)).toBeInTheDocument();
});


async function openEditor(rule: AutopilotRule = TEMPLATE_RULE) {
  listMock.mockResolvedValue({ rules: [rule] });
  renderWithProviders(<AutopilotTab />);
  await screen.findByText(rule.name);
  fireEvent.click(screen.getByRole('button', { name: 'Edit rule' }));
  return within(await screen.findByRole('dialog'));
}

async function addStep(dialog: ReturnType<typeof within>, name: string) {
  fireEvent.click(dialog.getByRole('button', { name: '＋ Add step' }));
  fireEvent.click(await screen.findByRole('menuitem', { name }));
}

it('adds, moves and removes steps, then saves the ordered steps action', async () => {
  updateMock.mockResolvedValue({});
  const dialog = await openEditor();
  await addStep(dialog, 'Wait N minutes');
  const waitRow = within(dialog.getByRole('group', { name: 'Step 2' }));
  expect(waitRow.queryByRole('checkbox')).toBeNull();
  fireEvent.change(waitRow.getByLabelText('Wait in minutes'), { target: { value: '30' } });
  await addStep(dialog, 'Delegate');
  fireEvent.change(within(dialog.getByRole('group', { name: 'Step 3' })).getByLabelText('What the AI employee should do'), { target: { value: 'Follow up' } });
  fireEvent.click(dialog.getByRole('button', { name: 'Move step 1 down' }));
  expect(within(dialog.getByRole('group', { name: 'Step 1' })).getByRole('spinbutton')).toHaveValue(30);
  fireEvent.click(dialog.getByRole('button', { name: 'Move step 2 up' }));
  await addStep(dialog, 'Run skill');
  fireEvent.click(dialog.getByRole('button', { name: 'Remove step 4' }));
  const first = within(dialog.getByRole('group', { name: 'Step 1' }));
  fireEvent.click(first.getByRole('checkbox', { name: 'Require approval before running' }));
  fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
  await waitFor(() => expect(updateMock).toHaveBeenCalledTimes(1));
  expect(updateMock).toHaveBeenCalledWith('r1', { action: {
    type: 'steps', screen: { mode: 'local' }, steps: [
      { type: 'notify', channel: 'line', chat_id: 'C999', text: 'Pharmacist needed', require_approval: true },
      { type: 'wait', minutes: 30 },
      { type: 'delegate', target_agent: 'sales-lead', prompt: 'Follow up' },
    ],
  } });
});

it('shows wait order and duration errors at the top and outlines offending rows', async () => {
  const dialog = await openEditor();
  await addStep(dialog, 'Wait N minutes');
  fireEvent.change(dialog.getByLabelText('Wait in minutes'), { target: { value: '1441' } });
  fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
  const alert = await dialog.findByRole('alert');
  expect(alert).toHaveTextContent('Each wait must be 1–1440 minutes');
  expect(alert).toHaveTextContent('Waits cannot be consecutive or last');
  const row = dialog.getByRole('group', { name: 'Step 2' });
  expect(row.className).toContain('outline-destructive');
  expect(alert.compareDocumentPosition(row) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  expect(updateMock).not.toHaveBeenCalled();
});

it('shows the five-step limit before sending to the server', async () => {
  const dialog = await openEditor();
  for (let i = 0; i < 5; i++) await addStep(dialog, 'Wait N minutes');
  fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
  expect(await dialog.findByRole('alert')).toHaveTextContent('Use between 1 and 5 steps.');
  expect(dialog.getByRole('group', { name: 'Step 6' }).className).toContain('outline-destructive');
  expect(updateMock).not.toHaveBeenCalled();
});

it('edits an existing steps rule while preserving extensions and skill params', async () => {
  updateMock.mockResolvedValue({});
  const action = { type: 'steps', screen: { mode: 'local' }, steps: [
    { type: 'run_skill', target_agent: 'sales-lead', skill_name: 'report', params: { note: '{{task.title}}' }, require_approval: true },
  ] };
  const rule = { ...TEMPLATE_RULE, action, active_window: { days: [1] }, run_policy: { mode: 'skip' } };
  const dialog = await openEditor(rule);
  fireEvent.change(dialog.getByLabelText('Skill name'), { target: { value: 'new-report' } });
  fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
  await waitFor(() => expect(updateMock).toHaveBeenCalledWith('r1', { action: {
    ...action, steps: [{ ...action.steps[0], skill_name: 'new-report' }],
  } }));
});

it('shows step counts, upgrade labels, resolved names and lost creator access', async () => {
  listMock.mockResolvedValue({ rules: [TEMPLATE_RULE, {
    ...TEMPLATE_RULE, id: 'r2', name: 'Follow-up',
    action: { type: 'steps', steps: [{ type: 'wait', minutes: 10 }, TEMPLATE_RULE.action] },
    created_by_user_id: 'creator-secret-id', updated_by_user_id: 'updater-secret-id', creator_access_lost: true,
  }] });
  renderWithProviders(<AutopilotTab />);
  expect(await screen.findByText('2 steps')).toBeInTheDocument();
  expect(screen.getAllByText('Created before upgrade')).toHaveLength(1);
  expect(await screen.findByText('Created by: 王小明 · Last edited by: 張雅婷')).toBeInTheDocument();
  expect(screen.getByText('Creator no longer has access')).toBeInTheDocument();
  expect(document.body).not.toHaveTextContent('secret-id');
});
