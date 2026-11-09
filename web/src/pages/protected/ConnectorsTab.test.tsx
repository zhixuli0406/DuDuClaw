import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import { useSystemStore } from '@/stores/system-store';
import type { IsolationReadiness, ProtectedConnectorList, ProtectedConnectorRoute, SystemStatus } from '@/lib/api';
import { ConnectorsTab } from './ConnectorsTab';
import { CSRF, NOT_VERIFIED, VERIFIED, calledWith, employeeList, renderTab, routeCalls } from './test-helpers';

const ACCOUNT = (n: number) => `${n}`.repeat(32);
const routes: ProtectedConnectorRoute[] = [
  { account: ACCOUNT(1), label: 'Alice mail', provider: 'google', kind: 'personal',
    binding: { type: 'personal', owner_user_id: 'u-alice' }, generation: 3, active: true },
  { account: ACCOUNT(2), label: 'Team wiki', provider: 'notion', kind: 'shared_home',
    binding: { type: 'shared' }, generation: 1, active: true },
  { account: ACCOUNT(3), label: 'Old repo', provider: 'github', kind: 'shared_tenant',
    binding: { type: 'shared' }, generation: 2, active: false },
];
const list = (over: Partial<ProtectedConnectorList> = {}): ProtectedConnectorList => ({ ...VERIFIED, routes, next: null, ...over });

const components: IsolationReadiness['components'] = {
  launch_service: 'ready', sandbox: 'ready', network: 'ready', storage: 'ready',
  parse_worker: 'ready', update_service: 'ready', mcp_boundary: 'ready', runtime_adapters: 'ready',
};
function setReadiness(readiness: IsolationReadiness) {
  const status: SystemStatus = { version: '1.81.0', uptime_seconds: 1, agents_count: 2, channels_connected: 0,
    gateway_address: 'localhost', isolation_readiness: readiness };
  useSystemStore.setState({ status });
}

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
  setReadiness({ required: true, ready: true, components, employees: [
    { label: 'sales-team', ready: false, reasons: [{ code: 'connector_setup_required', provider: 'database' }] },
  ] });
});
afterEach(() => {
  useSystemStore.setState({ status: null });
});

describe('protected mode — connector accounts tab', () => {
  it('groups accounts by employee, shows plain statuses and never an account secret or id', async () => {
    routeCalls({
      'isolation.connector_list': () => list(),
      'isolation.employee_list': () => employeeList(),
    });
    renderTab(<ConnectorsTab />);
    const personal = await screen.findByRole('region', { name: 'Personal employee · owner Alice Chen' });
    expect(within(personal).getByText('Alice mail')).toBeInTheDocument();
    expect(within(personal).getByText('Owner’s own account')).toBeInTheDocument();
    expect(within(personal).getByText('Available')).toBeInTheDocument();
    expect(screen.getByText('Employees: alice-helper')).toBeInTheDocument();

    const shared = screen.getByRole('region', { name: 'Shared employees' });
    expect(within(shared).getByText('Team wiki')).toBeInTheDocument();
    expect(within(shared).getByText('Company account installed on this computer')).toBeInTheDocument();
    expect(within(shared).getByText('Revoked')).toBeInTheDocument();
    expect(within(shared).getByRole('button', { name: 'Create again' })).toBeInTheDocument();
    // Readiness says the shared employee still needs a database connector.
    expect(within(shared).getByText('Needs setup')).toBeInTheDocument();
    expect(within(shared).getByText('needed by sales-team')).toBeInTheDocument();
    expect(within(shared).getByRole('button', { name: 'Finish setup' })).toBeInTheDocument();

    expect(screen.getByRole('button', { name: 'Revoke Google · Alice mail' })).toBeInTheDocument();
    for (const n of [1, 2, 3]) expect(document.body).not.toHaveTextContent(ACCOUNT(n));
    expect(document.querySelector('input[type="password"]')).toBeNull();
  });

  it('follows the list cursor to load every page', async () => {
    routeCalls({
      'isolation.connector_list': (params) => params.after
        ? list({ routes: [routes[1]], next: null })
        : list({ routes: [routes[0]], next: ACCOUNT(1) }),
      'isolation.employee_list': () => employeeList(),
    });
    renderTab(<ConnectorsTab />);
    expect(await screen.findByText('Team wiki')).toBeInTheDocument();
    expect(calledWith('isolation.connector_list')).toEqual([{}, { after: ACCOUNT(1) }]);
  });

  it('shows the empty state', async () => {
    useSystemStore.setState({ status: null });
    routeCalls({
      'isolation.connector_list': () => list({ routes: [] }),
      'isolation.employee_list': () => employeeList({ employees: [] }),
    });
    renderTab(<ConnectorsTab />);
    expect(await screen.findByText('No connector accounts yet')).toBeInTheDocument();
  });

  it('shows the server’s refusal when the catalog is unavailable', async () => {
    routeCalls({
      'isolation.connector_list': () => Promise.reject('連線帳號目錄無法使用，已拒絕操作。'),
      'isolation.employee_list': () => employeeList(),
    });
    renderTab(<ConnectorsTab />);
    expect(await screen.findByRole('alert')).toHaveTextContent('連線帳號目錄無法使用，已拒絕操作。');
    expect(screen.getByRole('button', { name: 'Add connector' })).toBeDisabled();
  });

  it('re-verifies identity before revoking and shows the refusal', async () => {
    let verified = false;
    routeCalls({
      'isolation.connector_list': () => list(NOT_VERIFIED),
      'isolation.employee_list': () => ({ ...employeeList(), ...(verified ? VERIFIED : NOT_VERIFIED) }),
      'isolation.connector_step_up': () => { verified = true; return { verified: true }; },
      'isolation.connector_revoke': () => Promise.reject('已停用這個連線帳號，但憑證尚未清除完成，請再執行一次撤銷。'),
    });
    renderTab(<ConnectorsTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Revoke Google · Alice mail' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/refused the next time it uses this account/)).toBeInTheDocument();
    const confirm = within(dialog).getByRole('button', { name: 'Verify identity and revoke' });
    expect(confirm).toBeDisabled();
    fireEvent.change(within(dialog).getByLabelText('Current password'), { target: { value: 'pw' } });
    fireEvent.change(within(dialog).getByLabelText('Six-digit authenticator code'), { target: { value: '654321' } });
    fireEvent.click(confirm);
    expect(await within(dialog).findByRole('alert')).toHaveTextContent('憑證尚未清除完成');
    expect(calledWith('isolation.connector_revoke')).toEqual([{ account: ACCOUNT(1), csrf_token: CSRF }]);
  });

  it('imports previous accounts as proposals and points to the approval center', async () => {
    routeCalls({
      'isolation.connector_list': () => list(),
      'isolation.employee_list': () => employeeList(),
      'isolation.connector_import_legacy': () => ({ staged: 3, unchanged: 0, closed: 0, unproven_personal: 1, approval_required: true }),
    });
    renderTab(<ConnectorsTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Import previous accounts' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/nothing takes effect directly/)).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Import' }));
    expect(await screen.findByText('3 proposals are awaiting approval')).toBeInTheDocument();
    expect(screen.getByText('1 personal accounts were skipped because their owner can’t be confirmed.')).toBeInTheDocument();
    expect(screen.getByText(/Approve all/)).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'Go to the approval center' })).toHaveAttribute('href', '/app/system/migration');
    expect(calledWith('isolation.connector_import_legacy')).toEqual([{ csrf_token: CSRF }]);
  });

  it('walks the four-step wizard and provisions a reference-only database account', async () => {
    routeCalls({
      'isolation.connector_list': () => list(),
      'isolation.employee_list': () => employeeList(),
      'db_sources.grants.list': () => ({ sources: [
        { name: 'orders', label: 'Orders', agents: ['sales-team'] },
        { name: 'hr', label: 'HR', agents: ['someone-else'] },
      ], agents: [], stale: {}, errors: [] }),
      'isolation.connector_provision': () => ({ account: ACCOUNT(9), details: true, readback_required: true }),
    });
    renderTab(<ConnectorsTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Add connector' }));
    const dialog = await screen.findByRole('dialog');
    const next = () => fireEvent.click(within(dialog).getByRole('button', { name: 'Next' }));

    expect(within(dialog).getByRole('button', { name: 'Next' })).toBeDisabled();
    fireEvent.click(within(dialog).getByRole('radio', { name: /sales-team/ }));
    next();
    fireEvent.click(within(dialog).getByRole('radio', { name: /^Database/ }));
    next();
    fireEvent.change(within(dialog).getByLabelText('Account name'), { target: { value: 'Orders DB' } });
    const source = await within(dialog).findByLabelText('Database source');
    expect(within(dialog).queryByRole('option', { name: 'HR' })).not.toBeInTheDocument();
    fireEvent.change(source, { target: { value: 'orders' } });
    expect(dialog.querySelector('input[type="password"]')).toBeNull();
    next();

    expect(within(dialog).getByText('Orders')).toBeInTheDocument();
    expect(within(dialog).getByText('4. Review and verify identity')).toHaveAttribute('aria-current', 'step');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Create connector' }));
    await waitFor(() => expect(calledWith('isolation.connector_provision')).toHaveLength(1));
    expect(calledWith('isolation.connector_provision')[0]).toEqual({
      provider: 'database', kind: 'shared_home', binding: 'shared', label: 'Orders DB',
      details: { employee: 'sales-team', source: 'orders' }, csrf_token: CSRF,
    });
    expect(await screen.findByText('“Database · Orders DB” was created.')).toBeInTheDocument();
  });

  it('opens the wizard at the source step for a row that needs setup', async () => {
    routeCalls({
      'isolation.connector_list': () => list(),
      'isolation.employee_list': () => employeeList(),
      'db_sources.grants.list': () => ({ sources: [], agents: [], stale: {}, errors: [] }),
    });
    renderTab(<ConnectorsTab />);
    fireEvent.click(await screen.findByRole('button', { name: 'Finish setup' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('3. Pick source')).toHaveAttribute('aria-current', 'step');
    expect(await within(dialog).findByText(/isn’t allowed to use any database source yet/)).toBeInTheDocument();
    expect(within(dialog).getByRole('button', { name: 'Next' })).toBeDisabled();
  });
});
