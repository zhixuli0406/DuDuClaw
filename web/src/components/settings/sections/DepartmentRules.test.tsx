import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import { AutopilotRuleDialog, type DepartmentRuleMode } from './AutopilotRuleDialog';
import { AutopilotTab } from './AutopilotTab';
import { DepartmentAutomationTab } from '@/components/tasks/DepartmentAutomationTab';

const mode: DepartmentRuleMode = {
  department: 'Sales', agents: [{ id: 'sales-agent', display_name: 'Sales assistant' }],
  notifyDestinations: [
    { channel: 'line', chat_id: 'private-chat-id', kind: 'self', agent_id: null, label: 'My private messages' },
    { channel: 'telegram', chat_id: 'private-agent-chat', kind: 'agent', agent_id: 'sales-agent', label: 'Sales notifications' },
  ],
};
const rule = { id: 'sales-rule', name: 'Sales rule', enabled: true, owner_department: 'Sales',
  trigger_event: 'task_created', action: { type: 'steps', steps: [{ type: 'notify', channel: 'line', chat_id: 'private-chat-id', text: 'Hello' }] },
  conditions: {}, trigger_count: 0, created_at: '', sequence: null };

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'manager', role: 'manager' } as never, bindings: [],
    deptGrants: [{ department: 'Sales', groups: ['automation'] }] });
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockImplementation((method: string) => Promise.resolve(
    method === 'autopilot.list' ? { rules: [rule] }
    : method === 'autopilot.template_fields' ? { fields: ['task.title'] }
    : method === 'users.dept_grants.mine' ? { grants: [], agents: [
      ...mode.agents.map(a => ({ ...a, groups: ['automation'] })),
      { id: 'drift', display_name: 'Needs sync', groups: ['automation'] },
      { id: 'support-agent', display_name: 'Support assistant', groups: ['automation'] },
    ], needs_sync: [{ id: 'drift', display_name: 'Needs sync' }] }
    : method === 'autopilot.notify_destinations' ? { destinations: mode.notifyDestinations }
    : method === 'agents.list' ? { agents: [
      { name: 'sales-agent', display_name: 'Sales assistant', department: 'Sales' },
      { name: 'drift', display_name: 'Needs sync', department: 'Sales' },
      { name: 'support-agent', display_name: 'Support assistant', department: 'Support' },
    ] }
    : method === 'autopilot.runs.list' ? { runs: [{ run_id: 'run', rule_id: 'sales-rule', state: 'waiting',
      created_at: '2026-11-28T00:00:00Z', step_index: 0, steps: [] }] }
    : {}));
});

it('offers only three triggers and usable agents of the selected department', async () => {
  renderWithProviders(<DepartmentAutomationTab />);
  await userEvent.click(await screen.findByRole('button', { name: 'New department automation rule' }));
  const dialog = within(await screen.findByRole('dialog'));
  expect(dialog.getByText(/are still approved by an administrator/)).toBeInTheDocument();
  fireEvent.keyDown(dialog.getByRole('combobox', { name: 'Trigger event' }), { key: 'ArrowDown' });
  await screen.findByRole('listbox');
  expect(screen.getAllByRole('option').map(o => o.textContent)).toEqual(['Task created', 'Task updated', 'Task status changed']);
  await userEvent.click(screen.getByRole('option', { name: 'Task updated' }));
  fireEvent.keyDown(dialog.getByRole('combobox', { name: 'Action agent' }), { key: 'ArrowDown' });
  await screen.findByRole('listbox');
  expect(screen.getAllByRole('option')).toHaveLength(1);
  expect(screen.getByRole('option')).toHaveTextContent('Sales assistant');
  expect(document.body).not.toHaveTextContent('Needs sync');
  expect(document.body).not.toHaveTextContent('Support assistant');
  expect(mockWsClient.call).toHaveBeenCalledWith('autopilot.template_fields', { trigger_event: 'task_updated' });
  await userEvent.click(screen.getByRole('option', { name: /Sales assistant/ }));
  fireEvent.click(dialog.getByRole('button', { name: '＋ Add step' }));
  fireEvent.click(await screen.findByRole('menuitem', { name: 'Run skill' }));
  const skillStep = within(dialog.getByRole('group', { name: 'Step 2' }));
  fireEvent.keyDown(skillStep.getByRole('combobox', { name: 'Action agent' }), { key: 'ArrowDown' });
  await screen.findByRole('listbox');
  expect(screen.getAllByRole('option')).toHaveLength(1);
  expect(screen.getByRole('option')).toHaveTextContent('Sales assistant');
});

it('creates a department notify rule using destination values without rendering chat identifiers', async () => {
  renderWithProviders(<AutopilotRuleDialog departmentMode={mode} onClose={vi.fn()} onSaved={vi.fn()} />);
  const dialog = within(screen.getByRole('dialog'));
  fireEvent.change(dialog.getByLabelText('Rule name'), { target: { value: 'Department notification' } });
  await userEvent.click(dialog.getByRole('button', { name: '＋ Add step' }));
  await userEvent.click(await screen.findByRole('menuitem', { name: 'Send notification' }));
  await userEvent.click(dialog.getByRole('button', { name: 'Remove step 1' }));
  fireEvent.keyDown(dialog.getByRole('combobox', { name: 'Notification recipient' }), { key: 'ArrowDown' });
  await screen.findByRole('listbox');
  expect(screen.getByRole('option', { name: 'My private messages' })).toBeInTheDocument();
  expect(screen.getByRole('option', { name: 'Sales notifications' })).toBeInTheDocument();
  expect(document.body.innerHTML).not.toContain('private-chat-id');
  expect(document.body.innerHTML).not.toContain('private-agent-chat');
  await userEvent.click(screen.getByRole('option', { name: 'My private messages' }));
  fireEvent.change(dialog.getByLabelText('Notification text'), { target: { value: 'Hello department' } });
  await userEvent.click(dialog.getByRole('button', { name: 'New department automation rule' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('autopilot.create', {
    name: 'Department notification', trigger_event: 'task_created', owner_department: 'Sales', conditions: { all: [] },
    action: { type: 'steps', steps: [{ type: 'notify', channel: 'line', chat_id: 'private-chat-id', text: 'Hello department' }] },
  }));
  expect(document.body.innerHTML).not.toContain('private-chat-id');
});

it('does not offer run cancellation to managers', async () => {
  renderWithProviders(<AutopilotTab departmentMode={mode} />);
  await screen.findByText('Sales rule');
  await userEvent.click(screen.getByRole('button', { name: 'Run history' }));
  await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('autopilot.runs.list', { rule_id: 'sales-rule', limit: 20 }));
  expect(await screen.findByText('Waiting')).toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Cancel this run' })).not.toBeInTheDocument();
  expect(document.body.innerHTML).not.toContain('private-chat-id');
});

it('shows the server refusal plainly and keeps the editor open', async () => {
  const original = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) => method === 'autopilot.create'
    ? Promise.reject(new Error('你已無此部門的自動化規則授權。')) : original(method, ...args));
  renderWithProviders(<AutopilotRuleDialog departmentMode={mode} onClose={vi.fn()} onSaved={vi.fn()} />);
  const dialog = within(screen.getByRole('dialog'));
  fireEvent.change(dialog.getByLabelText('Rule name'), { target: { value: 'Refused rule' } });
  fireEvent.change(dialog.getByLabelText('What the AI employee should do'), { target: { value: 'Do the task' } });
  await userEvent.click(dialog.getByRole('button', { name: 'New department automation rule' }));
  expect(await dialog.findByRole('alert')).toHaveTextContent('你已無此部門的自動化規則授權。');
  expect(screen.getByRole('dialog')).toBeInTheDocument();
});
