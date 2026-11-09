import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import type { IsolationReadiness, ProtectedEmployeeRequest, SystemStatus } from '@/lib/api';
import type { QuarantineRow, RestoreRequestRow } from '@/lib/isolation-restore-api';
import { ADMIN_RESTORE_PATH } from './restore/AdminRestorePage';
import { EmployeesTab } from './EmployeesTab';
import { CSRF, NOT_VERIFIED, calledWith, employeeList, renderTab, routeCalls } from './test-helpers';

const request = (over: Partial<ProtectedEmployeeRequest>): ProtectedEmployeeRequest => ({
  request_id: '1'.repeat(32), label: 'bob-helper', display_name: 'Bob 的助理', created_at: '2026-11-08T09:00:00Z',
  status: 'pending', stage: 'requested', owner_user_id: 'u-bob', ...over,
});

function setReadiness(readiness: IsolationReadiness | null) {
  const status: SystemStatus = { version: '1.81.0', uptime_seconds: 1, agents_count: 2, channels_connected: 0,
    gateway_address: 'localhost', isolation_readiness: readiness };
  useSystemStore.setState({ status });
}
const components: IsolationReadiness['components'] = {
  launch_service: 'ready', sandbox: 'ready', network: 'ready', storage: 'ready',
  parse_worker: 'ready', update_service: 'ready', mcp_boundary: 'ready', runtime_adapters: 'ready',
};

const originalFetchStatus = useSystemStore.getState().fetchStatus;

beforeEach(() => {
  vi.clearAllMocks();
  // Entering `authenticated` makes the store refetch `system.status`; keep the seeded readiness.
  useSystemStore.setState({ fetchStatus: async () => {} });
  useConnectionStore.setState({ state: 'authenticated' });
  setReadiness({ required: true, ready: true, components });
});
afterEach(() => {
  useSystemStore.setState({ status: null, fetchStatus: originalFetchStatus });
});

describe('protected mode — AI employees tab', () => {
  it('lists colleagues’ requests and protected employees with plain status words', async () => {
    routeCalls({
      'isolation.employee_list': () => employeeList({ requests: [
        request({}),
        request({ request_id: '2'.repeat(32), label: 'bob-writer', display_name: 'Writer', stage: 'proposed' }),
        request({ request_id: '3'.repeat(32), label: 'bob-old', display_name: 'Old', status: 'rejected' }),
        request({ request_id: '4'.repeat(32), label: 'bob-done', display_name: 'Done', status: 'applied', stage: 'proposed' }),
      ] }),
    });
    renderTab(<EmployeesTab />);
    expect(await screen.findByText('Colleagues’ requests')).toBeInTheDocument();
    expect(screen.getByText('Bob 的助理')).toBeInTheDocument();
    expect(screen.getByText('To prepare')).toBeInTheDocument();
    expect(screen.getByText('Awaiting approval')).toBeInTheDocument();
    expect(screen.getByText('Rejected')).toBeInTheDocument();
    // Applied requests are shown as employees, not as requests.
    expect(screen.queryByText('Done')).not.toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Go to the approval center' })).toHaveAttribute('href', '/app/system/migration');
    expect(screen.getByRole('button', { name: 'Clear rejected and expired requests' })).toBeEnabled();

    const employees = screen.getByRole('region', { name: 'Protected AI employees' });
    expect(within(employees).getByText('alice-helper')).toBeInTheDocument();
    expect(within(employees).getByText('Alice Chen')).toBeInTheDocument();
    expect(within(employees).getByText('Personal')).toBeInTheDocument();
    expect(within(employees).getByText('Shared')).toBeInTheDocument();
    expect(within(employees).getAllByText('Running')).toHaveLength(2);
    // Only Personal employees can be retired here; ids never show.
    expect(screen.getByRole('button', { name: 'Retire alice-helper' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Retire sales-team' })).not.toBeInTheDocument();
    expect(document.body).not.toHaveTextContent('aaaaaaaa');
    expect(document.body).not.toHaveTextContent('u-alice');
  });

  it('shows the attention banner from readiness and jumps to connector setup', async () => {
    setReadiness({ required: true, ready: true, status: 'action_required', components,
      reasons: [{ code: 'connector_setup_required', provider: 'google' }],
      employees: [
        { label: 'alice-helper', ready: false, reasons: [{ code: 'connector_setup_required', provider: 'google' }, { code: 'personal_binding_conflict' }] },
        { label: 'sales-team', ready: true, reasons: [] },
      ] });
    routeCalls({ 'isolation.employee_list': () => employeeList() });
    const goTo = vi.fn();
    renderTab(<EmployeesTab />, { goTo });
    expect(await screen.findByText('1 AI employees need attention')).toBeInTheDocument();
    expect(screen.getByText(/The Google connector is not set up yet\./)).toBeInTheDocument();
    expect(screen.getByText(/have not signed in on this computer yet/)).toBeInTheDocument();
    // The banner comes from readiness at once; the row badge waits for the list.
    expect(await screen.findByText('Needs attention')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Finish setup' }));
    expect(goTo).toHaveBeenCalledWith('connectors', { employee: 'alice-helper', provider: 'google' });
  });

  it('lists archived employees with restore actions and hides ones that are live again', async () => {
    const archived = (over: Partial<QuarantineRow>): QuarantineRow => ({
      employee_id: 'c'.repeat(32), incarnation: 1, label: 'old-sales', kind: 'shared', seal: 'sealed',
      restorable: true, purge: { purgeable: false }, ...over,
    });
    routeCalls({
      'isolation.employee_list': () => ({
        ...employeeList(),
        restores: { quarantine: [
          archived({}),
          archived({ employee_id: 'd'.repeat(32), label: 'old-bob', kind: 'personal', owner_user_id: 'u-bob',
            root_flag: 'ROOT_GONE' }),
          // Same id as a live employee: the live row wins, no archived duplicate.
          archived({ employee_id: 'b'.repeat(32), label: 'sales-team' }),
        ] },
      }),
    });
    renderTab(<EmployeesTab />);
    const employees = await screen.findByRole('region', { name: 'Protected AI employees' });
    expect(within(employees).getByText('old-sales')).toBeInTheDocument();
    expect(within(employees).getByText('old-bob')).toBeInTheDocument();
    expect(within(employees).getAllByText('sales-team')).toHaveLength(1);
    expect(within(employees).getByText('Retired')).toBeInTheDocument();
    expect(within(employees).getByText('Retired · a new employee can take over')).toBeInTheDocument();
    expect(within(employees).getByRole('button', { name: 'Prepare restore: old-sales' })).toBeInTheDocument();
    expect(within(employees).getByRole('link', { name: 'View old-sales' })).toHaveAttribute('href', ADMIN_RESTORE_PATH);
    expect(within(employees).getByRole('link', { name: 'View old-bob' })).toHaveAttribute('href', ADMIN_RESTORE_PATH);
    expect(screen.getByRole('link', { name: 'Restore, hand over and remove' })).toHaveAttribute('href', ADMIN_RESTORE_PATH);
    // Retired rows never offer the live-employee retire button.
    expect(screen.queryByRole('button', { name: 'Retire old-sales' })).not.toBeInTheDocument();
  });

  it('lists restore and take-over requests with links to decide them', async () => {
    const restoreRequest = (over: Partial<RestoreRequestRow>): RestoreRequestRow => ({
      request_id: '5'.repeat(32), employee_id: 'c'.repeat(32), incarnation: 1, label: 'old-sales',
      employee_kind: 'personal', kind: 'restore', owner_user_id: 'u-alice', ...over,
    });
    routeCalls({
      'isolation.employee_list': () => ({
        ...employeeList(),
        restores: { requests: [
          restoreRequest({}),
          restoreRequest({ request_id: '6'.repeat(32), label: 'old-bob', kind: 'adoption', owner_user_id: 'u-bob',
            owner_unverified: true }),
        ] },
      }),
    });
    renderTab(<EmployeesTab />);
    expect(await screen.findByText('Restore “old-sales”')).toBeInTheDocument();
    expect(screen.getByText('Hand “old-bob” over to a new employee')).toBeInTheDocument();
    expect(screen.getByText('Needs another admin')).toBeInTheDocument();
    expect(screen.getByText('Owner did not confirm with two-step verification')).toBeInTheDocument();
    expect(screen.getByText('· 2 waiting')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Approve restore' })).toHaveAttribute('href', ADMIN_RESTORE_PATH);
    expect(screen.getByRole('link', { name: 'Approve hand-over' })).toHaveAttribute('href', ADMIN_RESTORE_PATH);
    // Restore requests are not an empty list.
    expect(screen.queryByText('No requests from colleagues')).not.toBeInTheDocument();
  });

  it('shows empty states and disables clearing when nothing is closed', async () => {
    routeCalls({ 'isolation.employee_list': () => employeeList({ employees: [], requests: [] }) });
    renderTab(<EmployeesTab />);
    expect(await screen.findByText('No requests from colleagues')).toBeInTheDocument();
    expect(screen.getByText('No protected AI employees yet')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Clear rejected and expired requests' })).toBeDisabled();
  });

  it('shows the server’s refusal when the list cannot load', async () => {
    routeCalls({ 'isolation.employee_list': () => Promise.reject('這個環境未啟用受保護模式，不需要申請受保護的 AI 員工。') });
    renderTab(<EmployeesTab />);
    expect(await screen.findByRole('alert')).toHaveTextContent('這個環境未啟用受保護模式');
    expect(screen.queryByText('Colleagues’ requests')).not.toBeInTheDocument();
  });

  it('re-verifies identity before preparing a request, then sends the fresh token', async () => {
    let verified = false;
    routeCalls({
      'isolation.employee_list': () => ({ ...employeeList({ requests: [request({})] }), ...(verified ? {} : NOT_VERIFIED) }),
      'isolation.connector_step_up': () => { verified = true; return { verified: true }; },
      'isolation.employee_propose': () => ({ request_id: '1'.repeat(32), revision: 2, label: 'bob-helper', status: 'pending', approval_required: true }),
    });
    renderTab(<EmployeesTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Prepare and send for approval' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Bob Lin')).toBeInTheDocument();
    const confirm = within(dialog).getByRole('button', { name: 'Verify identity and send for approval' });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText('Current password'), { target: { value: 'pw-not-kept' } });
    fireEvent.change(within(dialog).getByLabelText('Six-digit authenticator code'), { target: { value: '123456' } });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    await waitFor(() => expect(calledWith('isolation.employee_propose')).toHaveLength(1));
    expect(calledWith('isolation.connector_step_up')).toEqual([{ password: 'pw-not-kept', code: '123456' }]);
    expect(calledWith('isolation.employee_propose')[0]).toEqual({
      label: 'bob-helper', binding_kind: 'personal', request_id: '1'.repeat(32), csrf_token: CSRF,
    });
    expect(await screen.findByText('“Bob 的助理” was sent to the approval center.')).toBeInTheDocument();
    expect(screen.queryByDisplayValue('pw-not-kept')).not.toBeInTheDocument();
  });

  it('never calls the mutation when step-up verification is refused', async () => {
    routeCalls({
      'isolation.employee_list': () => ({ ...employeeList({ requests: [request({ status: 'rejected' })] }), ...NOT_VERIFIED }),
      'isolation.connector_step_up': () => Promise.reject('驗證失敗，請確認密碼與新的驗證碼後重試。'),
      'isolation.employee_purge_requests': () => ({ purged: 1, index_rows_removed: 1 }),
    });
    renderTab(<EmployeesTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Clear rejected and expired requests' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('Current password'), { target: { value: 'wrong' } });
    fireEvent.change(within(dialog).getByLabelText('Six-digit authenticator code'), { target: { value: '000000' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Verify identity and clear' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('驗證失敗，請確認密碼與新的驗證碼後重試。');
    expect(calledWith('isolation.employee_purge_requests')).toHaveLength(0);
  });

  it('explains retiring and shows the server refusal inside the dialog', async () => {
    routeCalls({
      'isolation.employee_list': () => employeeList(),
      'isolation.employee_tombstone': () => Promise.reject('這不是個人專屬員工，或找不到這位員工。'),
    });
    renderTab(<EmployeesTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Retire alice-helper' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/kept for 90 days/)).toBeInTheDocument();
    expect(within(dialog).getByText(/can be restored to its original owner later/)).toBeInTheDocument();
    expect(within(dialog).getByText(/“alice-helper” stays reserved for 90 days/)).toBeInTheDocument();
    // A valid step-up from the list skips the fields.
    expect(within(dialog).queryByLabelText('Current password')).not.toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Retire' }));
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('這不是個人專屬員工，或找不到這位員工。');
    expect(calledWith('isolation.employee_tombstone')[0]).toEqual({ label: 'alice-helper', employee_id: 'a'.repeat(32), csrf_token: CSRF });
  });

  it('reports whether retired data can be restored', async () => {
    routeCalls({
      'isolation.employee_list': () => employeeList(),
      'isolation.employee_tombstone': () => ({ label: 'alice-helper', tombstoned: true, readback_required: true,
        seal: 'oversize', routes_revoked: 2, pending_fences: [], runs_stopped: true }),
    });
    renderTab(<EmployeesTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Retire alice-helper' }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Retire' }));
    expect(await screen.findByText('“alice-helper” was retired.')).toBeInTheDocument();
    expect(screen.getByText(/can’t be restored later/)).toBeInTheDocument();
    expect(screen.getByText('2 connector accounts were revoked as well.')).toBeInTheDocument();
  });

  it('blocks admin actions until two-step verification is set up', async () => {
    routeCalls({ 'isolation.employee_list': () => ({ ...employeeList(), step_up_valid: false, csrf_token: null, mfa_enrolled: false }) });
    renderTab(<EmployeesTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Add shared employee' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('alert')).toHaveTextContent('needs two-step verification');
    fireEvent.change(within(dialog).getByLabelText('Display name'), { target: { value: 'Sales' } });
    fireEvent.change(within(dialog).getByLabelText('Employee ID'), { target: { value: 'sales-desk' } });
    expect(within(dialog).getByRole('button', { name: 'Verify identity and send for approval' })).toBeDisabled();
  });
});
