import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const listMock = vi.fn();
const createMock = vi.fn();
const updateMock = vi.fn();
const agentsListMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
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

