import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import type { DeptGrantEntry, UserDetail } from '@/lib/api';
import { DeptGrantsDialog } from './DeptGrantsDialog';

const manager = { id: 'private-manager-id', display_name: 'Chen', email: 'chen@example.com', role: 'manager', status: 'active', bindings: [], created_at: '', updated_at: '' } as UserDetail;
const grant: DeptGrantEntry = { user_id: manager.id, display_name: 'Chen', email: manager.email, department: 'Sales', groups: ['schedules', 'skills'], granted_by: 'private-admin-id', granted_at: '' };
let rows: DeptGrantEntry[];
let failure: unknown;
const updated = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  rows = [grant, { ...grant, user_id: 'another-private-id', department: 'Hidden department' }];
  failure = null;
  mockWsClient.call.mockImplementation(async (method: string, params: unknown) => {
    if (method === 'users.dept_grants.list') return rows;
    if (method === 'users.dept_grants.options') return { departments: [{ name: 'Sales', agent_count: 8 }, { name: 'Manufacturing', agent_count: 12 }], mirror_mismatch_count: 2 };
    if (method === 'users.dept_grants.set') {
      if (failure) throw failure;
      const change = params as { department: string; groups: DeptGrantEntry['groups'] };
      rows = rows.filter(row => row.user_id !== manager.id || row.department !== change.department);
      if (change.groups.length) rows.push({ ...grant, ...change });
      return { ok: true };
    }
    return {};
  });
});

async function open() {
  const user = userEvent.setup();
  renderWithProviders(<DeptGrantsDialog user={manager} onClose={vi.fn()} onUpdated={updated} />);
  await screen.findByRole('option', { name: 'Manufacturing（12 AI employees）' });
  return user;
}
async function edit() {
  const user = await open();
  await user.click(screen.getByRole('button', { name: 'Edit' }));
  return user;
}
async function confirmRisk(user: ReturnType<typeof userEvent.setup>) {
  const dialog = screen.getByRole('dialog', { name: /Allow Chen/ });
  await user.click(within(dialog).getByRole('checkbox', { name: 'I understand the effects of this grant' }));
  await user.click(within(dialog).getByRole('button', { name: 'Grant' }));
}

describe('DeptGrantsDialog', () => {
  it('lists only this manager’s grants with counts, tags and the company mismatch note without user ids', async () => {
    await open();
    expect(screen.getByText('· 8 AI employees')).toBeInTheDocument();
    expect(screen.getByText('Schedules', { selector: '[data-slot="badge"]' })).toBeInTheDocument();
    expect(screen.getByText(/2 AI employees have inconsistent department data/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Sync organization data in the org chart.' })).toHaveAttribute('href', '/org');
    expect(screen.queryByText('Hidden department')).not.toBeInTheDocument();
    expect(document.body.textContent).not.toMatch(/private-.*-id|another-private-id/);
    expect(screen.queryByRole('option', { name: 'Sales（8 AI employees）' })).not.toBeInTheDocument();
    expect(screen.getAllByRole('checkbox').every(box => box.getAttribute('aria-checked') === 'false')).toBe(true);
  });

  it('confirms risk before adding and sends the selected groups with acknowledgement, then refreshes', async () => {
    const user = await open();
    await user.selectOptions(screen.getByRole('combobox'), 'Manufacturing');
    await user.click(screen.getByRole('checkbox', { name: 'Schedules' }));
    await user.click(screen.getByRole('checkbox', { name: 'Usage' }));
    await user.click(screen.getByRole('button', { name: 'Add grant' }));
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.dept_grants.set')).toBe(false);
    expect(screen.getByText(/manage Schedules and Usage for 12 AI employees/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Grant' })).toBeDisabled();
    await confirmRisk(user);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.set', { user_id: manager.id, department: 'Manufacturing', groups: ['schedules', 'cost'], acknowledged: true }));
    await waitFor(() => expect(updated).toHaveBeenCalledOnce());
    expect(await screen.findByText('· 12 AI employees')).toBeInTheDocument();
  });

  it('confirms risk when editing adds a group', async () => {
    const user = await edit();
    await user.click(screen.getByRole('checkbox', { name: 'Usage' }));
    await user.click(screen.getByRole('button', { name: 'Save grant' }));
    await confirmRisk(user);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.set', { user_id: manager.id, department: 'Sales', groups: ['schedules', 'skills', 'cost'], acknowledged: true }));
  });

  it('removes some groups without risk or acknowledgement', async () => {
    const user = await edit();
    await user.click(screen.getByRole('checkbox', { name: 'Skills' }));
    await user.click(screen.getByRole('button', { name: 'Save grant' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.set', { user_id: manager.id, department: 'Sales', groups: ['schedules'] }));
    expect(screen.queryByRole('dialog', { name: /Allow Chen/ })).not.toBeInTheDocument();
  });

  it('confirms revocation and explains that existing schedules and rules keep running', async () => {
    const user = await open();
    await user.click(screen.getByRole('button', { name: 'Revoke grant' }));
    const dialog = screen.getByRole('dialog', { name: 'Revoke the grant for Sales?' });
    expect(within(dialog).getByText(/schedules and rules created by this manager keep running/)).toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.dept_grants.set')).toBe(false);
    await user.click(within(dialog).getByRole('button', { name: 'Revoke grant' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.set', { user_id: manager.id, department: 'Sales', groups: [] }));
    expect(await screen.findByText('No department grants yet. Chen can currently manage only directly assigned AI employees.')).toBeInTheDocument();
  });

  it('preserves a server refusal', async () => {
    failure = '此部門尚未確認或已不存在，請先由管理員同步部門資料後再授權。';
    const user = await edit();
    await user.click(screen.getByRole('checkbox', { name: 'Skills' }));
    await user.click(screen.getByRole('button', { name: 'Save grant' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(failure as string);
  });

  it('falls back to risk confirmation when the server requires acknowledgement', async () => {
    failure = { code: 'acknowledgement_required', message: '請確認授權影響。' };
    const user = await edit();
    await user.click(screen.getByRole('checkbox', { name: 'Skills' }));
    await user.click(screen.getByRole('button', { name: 'Save grant' }));
    await screen.findByRole('dialog', { name: /Allow Chen/ });
    failure = null;
    await confirmRisk(user);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.dept_grants.set', { user_id: manager.id, department: 'Sales', groups: ['schedules'], acknowledged: true }));
  });

  it('shows the empty state with no default groups and disables adding until a department and groups are selected', async () => {
    rows = [];
    await open();
    expect(screen.getByText('No department grants yet. Chen can currently manage only directly assigned AI employees.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Add grant' })).toBeDisabled();
    expect(screen.getAllByRole('checkbox').every(box => box.getAttribute('aria-checked') === 'false')).toBe(true);
  });
});
