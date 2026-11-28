import { useCompanyTimeStore } from '@/lib/company-time';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const listMock = vi.fn();
const createMock = vi.fn();
const updateMock = vi.fn();
const agentsListMock = vi.fn();
const fieldsMock = vi.fn();
const runsMock = vi.fn();
const cancelMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    users: { list: vi.fn().mockResolvedValue({ users: [
      { id: 'creator-secret-id', display_name: '王小明', email: 'creator@example.com' },
      { id: 'updater-secret-id', display_name: '張雅婷', email: 'updater@example.com' },
    ] }) },
    autopilot: {
      templateFields: (...args: unknown[]) => fieldsMock(...args),
      runs: { list: (...args: unknown[]) => runsMock(...args), cancel: (...args: unknown[]) => cancelMock(...args) },
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
  fieldsMock.mockResolvedValue({ fields: ['task.id', 'task.title', 'to', 'from', 'task.assigned_to'] });
  runsMock.mockResolvedValue({ runs: [] });
  cancelMock.mockResolvedValue({ cancelled: true });
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
  const rule = { ...TEMPLATE_RULE, trigger_event: 'task_created', action, active_window: { days: [1], start: '09:00', end: '18:00', timezone: 'Asia/Taipei' }, run_policy: { outside_window: 'skip' } };
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

describe('S-19 rule dialog', () => {
  it('edits and clears an execution window, sending both nulls', async () => {
    const rule = { ...TEMPLATE_RULE, active_window: { days: [1, 2, 3, 4, 5], start: '09:00', end: '18:00', timezone: 'Asia/Taipei' }, run_policy: { outside_window: 'skip' } };
    const dialog = await openEditor(rule);
    expect(screen.getByText('Mon–Fri 09:00–18:00 · Asia/Taipei')).toBeInTheDocument();
    fireEvent.click(dialog.getByRole('checkbox', { name: 'Any time' }));
    fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMock).toHaveBeenCalledWith('r1', { active_window: null, run_policy: null }));
  });
  it('shows company-zone, time and weekday errors at the top', async () => {
    const dialog = await openEditor();
    fireEvent.click(dialog.getByRole('checkbox', { name: 'Any time' }));
    expect(dialog.getByRole('link', { name: 'Open general settings' })).toHaveAttribute('href', '/settings?tab=general');
    fireEvent.change(dialog.getByLabelText('Start time'), { target: { value: '18:00' } });
    for (const day of ['Mon', 'Tue', 'Wed', 'Thu', 'Fri']) fireEvent.click(dialog.getByRole('button', { name: day }));
    fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    const alert = await dialog.findByRole('alert');
    expect(alert).toHaveTextContent('must start before it ends');
    expect(alert).toHaveTextContent('Select at least one day');
    expect(alert).toHaveTextContent('Set the company time zone');
    expect(updateMock).not.toHaveBeenCalled();
  });
  it('saves a company-zone execution window with the skip policy', async () => {
    useCompanyTimeStore.setState({ zone: 'Asia/Taipei', status: 'set' });
    const dialog = await openEditor();
    fireEvent.click(dialog.getByRole('checkbox', { name: 'Any time' }));
    expect(dialog.getByRole('combobox', { name: 'Time zone' })).toHaveTextContent('Company time zone (Asia/Taipei)');
    fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMock).toHaveBeenCalledWith('r1', expect.objectContaining({ active_window: { days: [1, 2, 3, 4, 5], start: '09:00', end: '18:00', timezone: null }, run_policy: { outside_window: 'skip' } })));
  });
  it('inserts event fields with a readable preview and saves their template', async () => {
    const rule = { ...TEMPLATE_RULE, trigger_event: 'task_created', action: { type: 'run_skill', target_agent: 'sales-lead', skill_name: 'report' } };
    const dialog = await openEditor(rule);
    fireEvent.click(dialog.getByRole('button', { name: 'Add parameter' }));
    fireEvent.change(dialog.getByLabelText('Name'), { target: { value: 'note' } });
    await waitFor(() => expect(dialog.getByRole('button', { name: 'Insert event field' })).toBeEnabled());
    fireEvent.click(dialog.getByRole('button', { name: 'Insert event field' }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Task title' }));
    expect(dialog.getByLabelText('Value')).toHaveValue('{{task.title}}');
    expect(dialog.getByText('〔Task title〕')).toBeInTheDocument();
    fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(updateMock).toHaveBeenCalledWith('r1', { action: { type: 'steps', steps: [{ ...rule.action, params: { note: { template: '{{task.title}}', type: 'text' } } }] } }));
    expect(fieldsMock).toHaveBeenCalledWith('task_created', null);
  });
  it('outlines invalid parameter rows and limits addition to ten', async () => {
    const dialog = await openEditor({ ...TEMPLATE_RULE, action: { type: 'run_skill', target_agent: 'sales-lead', skill_name: 'report' } });
    fireEvent.click(dialog.getByRole('button', { name: 'Add parameter' }));
    fireEvent.change(dialog.getByLabelText('Name'), { target: { value: 'Bad' } });
    fireEvent.change(dialog.getByLabelText('Value'), { target: { value: '{{order.id}}' } });
    await waitFor(() => expect(fieldsMock).toHaveBeenCalled());
    fireEvent.click(dialog.getByRole('button', { name: 'Save changes' }));
    const alert = await dialog.findByRole('alert');
    expect(alert).toHaveTextContent('Names must start with a lowercase letter');
    expect(alert).toHaveTextContent('uses a field unavailable for this trigger: id');
    expect(dialog.getByLabelText('Name')).toHaveAttribute('aria-invalid', 'true');
    for (let i = 1; i < 10; i++) fireEvent.click(dialog.getByRole('button', { name: 'Add parameter' }));
    expect(dialog.getByRole('button', { name: 'Add parameter' })).toBeDisabled();
    expect(updateMock).not.toHaveBeenCalled();
  });
});

const WAITING_RUN = {
  run_id: 'run1', state: 'waiting', step_index: 1, step_count: 3,
  next_at: '2026-11-27T14:35:00Z', outcome: null, detail: null,
  event_name: 'task_created', created_at: '2026-11-27T14:05:00Z', updated_at: '2026-11-27T14:05:00Z',
  steps: [
    { step_index: 0, step_type: 'delegate', state: 'done', detail: null, started_at: null, finished_at: null },
    { step_index: 1, step_type: 'wait', state: 'waiting', detail: null, started_at: null, finished_at: null },
    { step_index: 2, step_type: 'run_skill', state: 'pending', detail: null, started_at: null, finished_at: null },
  ],
};

describe('S-19 run history', () => {
  it('loads runs on expansion, shows steps and confirms cancellation before refreshing', async () => {
    runsMock.mockResolvedValue({ runs: [WAITING_RUN] });
    listMock.mockResolvedValue({ rules: [{ ...TEMPLATE_RULE, action: { type: 'steps', steps: [
      { type: 'delegate' }, { type: 'wait', minutes: 30 }, { type: 'run_skill' },
    ] } }] });
    renderWithProviders(<AutopilotTab />);
    await screen.findByText(TEMPLATE_RULE.name);
    expect(runsMock).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Run history' }));
    expect(await screen.findByText('Wait 30 minutes')).toBeInTheDocument();
    expect(screen.getByText('Resume at 14:35')).toBeInTheDocument();
    expect(screen.getByText('Completed')).toBeInTheDocument();
    expect(screen.getByText('Not started')).toBeInTheDocument();
    expect(runsMock).toHaveBeenCalledWith('r1', 20);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel this run' }));
    expect(cancelMock).not.toHaveBeenCalled();
    const dialog = within(await screen.findByRole('dialog'));
    expect(dialog.getByText('Cancelling a waiting run will create an audit record.')).toBeInTheDocument();
    fireEvent.click(dialog.getByRole('button', { name: 'Cancel this run' }));
    await waitFor(() => expect(cancelMock).toHaveBeenCalledWith('run1'));
    await waitFor(() => expect(runsMock).toHaveBeenCalledTimes(2));
  });
  it('translates all refusal sentences and never shows raw run codes', async () => {
    const codes = ['outside_window', 'outside_window_tz_unresolved', 'stale_rule_changed', 'rule_disabled', 'rule_removed', 'missed_late', 'param_injection_blocked', 'scope_denied', 'cancelled_by_admin', 'param_missing:secret', 'new_private_code'];
    runsMock.mockResolvedValue({ runs: codes.map((code, i) => ({ ...WAITING_RUN, run_id: String(i), state: 'settled', outcome: code, detail: null, steps: [{ ...WAITING_RUN.steps[2], state: 'failed', detail: code }] })) });
    renderWithProviders(<AutopilotTab />);
    await screen.findByText(TEMPLATE_RULE.name);
    fireEvent.click(screen.getByRole('button', { name: 'Run history' }));
    await screen.findAllByText('Outside the execution window; not executed');
    for (const sentence of ['No time zone available; not executed', 'The rule changed while waiting; remaining steps were cancelled', 'The rule was disabled or removed; remaining steps were cancelled', 'The system was offline for over an hour; this step was not retried', 'Possible instruction injection in parameters; blocked', 'Outside the allowed scope; not executed', 'Cancelled by an administrator', 'The event is missing a field required by a parameter', 'Not completed']) {
      expect(screen.getAllByText(sentence).length).toBeGreaterThan(0);
    }
    for (const code of codes) expect(document.body).not.toHaveTextContent(code);
    expect(screen.queryByRole('button', { name: 'Cancel this run' })).toBeNull();
  });
  it('shows fetch errors with a refresh action', async () => {
    runsMock.mockRejectedValueOnce(new Error('private_backend_error'));
    renderWithProviders(<AutopilotTab />);
    await screen.findByText(TEMPLATE_RULE.name);
    fireEvent.click(screen.getByRole('button', { name: 'Run history' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load run history');
    expect(document.body).not.toHaveTextContent('private_backend_error');
    fireEvent.click(screen.getAllByRole('button', { name: 'Refresh' }).at(-1)!);
    expect(await screen.findByText('No runs yet')).toBeInTheDocument();
  });
});

it('shows the owning department on admin automation rule cards', async () => {
  listMock.mockResolvedValue({ rules: [{ ...TEMPLATE_RULE, owner_department: 'Customer support' }, { ...TEMPLATE_RULE, id: 'company-rule', owner_department: null }] });
  renderWithProviders(<AutopilotTab />);
  expect(await screen.findByText('Department: Customer support')).toBeInTheDocument();
  expect(screen.getAllByText(/^Department:/)).toHaveLength(1);
});
