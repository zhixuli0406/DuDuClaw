import { mockWsClient } from '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { rootStateOf, type IsolationEmployeeList, type QuarantineRow, type RestoreRequestRow } from '@/lib/isolation-restore-api';
import { AdminRestorePage } from './AdminRestorePage';
import { rowActions } from './QuarantineTable';
import { approveState } from './RestoreApproveCard';
import { allowedLevels } from './SharedRestoreDialog';

const TOKEN = Array(32).fill(5);
const now = Math.floor(Date.now() / 1000);
const DAY = 86400;

const row = (over: Partial<QuarantineRow>): QuarantineRow => ({
  employee_id: 'e1', incarnation: 3, label: 'sales-bot', display_name: 'Sales Bot', kind: 'shared',
  seal: 'sealed', restorable: true, root_flag: null,
  purge: { purgeable: false, purge_after: now + 20 * DAY, early: true, early_purge: null, purged_at: null },
  ...over,
});
const req = (over: Partial<RestoreRequestRow>): RestoreRequestRow => ({
  request_id: 'r1', employee_id: 'e9', incarnation: 1, label: 'helper', display_name: 'Helper',
  employee_kind: 'personal', kind: 'restore', filed_by: 'u-owner', filed_by_name: 'Alice', ...over,
});

let list: IsolationEmployeeList;
let handlers: Record<string, (params: Record<string, unknown>) => unknown>;

beforeEach(() => {
  vi.clearAllMocks();
  handlers = {};
  list = { step_up_valid: true, csrf_token: TOKEN, mfa_enrolled: true, restores: { quarantine: [], requests: [] } };
  mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown> = {}) => {
    if (method === 'isolation.employee_list') return list;
    const handler = handlers[method];
    return handler ? handler(params) : null;
  });
});

describe('restore rules (pure)', () => {
  it('runtime state comes from root_flag, the purge block and restorable', () => {
    expect(rootStateOf(row({}))).toBe('restorable');
    expect(rootStateOf(row({ root_flag: 'ROOT_GONE', restorable: false }))).toBe('root_gone');
    expect(rootStateOf(row({ root_flag: 'ROOT_RETIRED' }))).toBe('retired');
    expect(rootStateOf(row({ root_flag: 'ROOT_INCONSISTENT' }))).toBe('unknown');
    expect(rootStateOf(row({ root_flag: 'SOMETHING_NEW' }))).toBe('unknown');
    expect(rootStateOf(row({ purge: { purgeable: false, purged_at: now } }))).toBe('pending_release');
    expect(rootStateOf(row({ restorable: false }))).toBe('closed');
  });

  it('row actions follow kind, root state and the purge block', () => {
    expect(rowActions(row({}))).toEqual(['shared_restore', 'early_purge']);
    expect(rowActions(row({ kind: 'personal', admin_may_file_for_owner: true }))).toEqual(['file_for_owner', 'early_purge']);
    expect(rowActions(row({ kind: 'personal' }))).toEqual(['early_purge']);
    expect(rowActions(row({ open_request: { kind: 'restore' } }))).toEqual(['early_purge']);
    expect(rowActions(row({ purge: { purgeable: true } }))).toEqual(['shared_restore', 'purge']);
    expect(rowActions(row({ purge: { purgeable: false, early: true, early_purge: { by_me: false } } }))).toEqual(['shared_restore', 'confirm_early_purge']);
    expect(rowActions(row({ purge: { purgeable: false, early: true, early_purge: { by_me: true } } }))).toEqual(['shared_restore']);
    expect(rowActions(row({ purge: { purgeable: false, purged_at: now } }))).toEqual(['retire']);
    expect(rowActions(row({ purge: { purgeable: false, purged_at: now }, retire_proposed: true }))).toEqual([]);
    expect(rowActions(row({ purge: { purgeable: false, purged_at: now }, root_flag: 'ROOT_RETIRED' }))).toEqual([]);
    // 執行環境無法確認: no restore, take-over or purge until the host is checked again.
    expect(rowActions(row({ root_flag: 'ROOT_INCONSISTENT' }))).toEqual([]);
    // The server says it can no longer be restored: only the purge remains.
    expect(rowActions(row({ restorable: false }))).toEqual(['early_purge']);
    // Runtime gone: a take-over is still possible.
    expect(rowActions(row({ root_flag: 'ROOT_GONE', restorable: false }))).toEqual(['shared_restore', 'early_purge']);
  });

  it('approval needs a different admin; a sole admin waits out the 7-day cooling', () => {
    expect(approveState(req({}))).toBe('ready');
    expect(approveState(req({ filed_by_me: true }))).toBe('second_admin');
    expect(approveState(req({ filed_by_me: true, sole_admin: true, cooling_until: now + DAY }))).toBe('cooling');
    expect(approveState(req({ filed_by_me: true, sole_admin: true, cooling_until: now - DAY }))).toBe('sole_admin_self');
    // Filed for an owner who did not verify: never self-approvable.
    expect(approveState(req({ filed_by_me: true, sole_admin: true, owner_unverified: true }))).toBe('second_admin');
    // Contract fields: `filed_by` = the signed-in Admin, `self_approved` from the server.
    expect(approveState(req({ filed_by: 'u-admin' }), Date.now(), 'u-admin')).toBe('second_admin');
    expect(approveState(req({ filed_by: 'u-admin', self_approved: true }), Date.now(), 'u-admin')).toBe('sole_admin_self');
    expect(approveState(req({ filed_by: 'u-other' }), Date.now(), 'u-admin')).toBe('ready');
  });

  it('member levels are capped at the snapshot level', () => {
    expect(allowedLevels('owner')).toEqual(['owner', 'operator', 'viewer']);
    expect(allowedLevels('operator')).toEqual(['operator', 'viewer']);
    expect(allowedLevels('viewer')).toEqual(['viewer']);
  });
});

describe('<AdminRestorePage> — 管理員 · 還原、承接與註銷', () => {
  it('lists quarantined employees with seal, runtime state and days left', async () => {
    list.restores!.quarantine = [
      row({}),
      row({ employee_id: 'e2', label: 'old-helper', display_name: 'Old Helper', kind: 'personal', root_flag: 'ROOT_GONE', restorable: false, seal: 'oversize', owner_name: 'Bob' }),
      row({ employee_id: 'e3', label: 'mystery', display_name: 'Mystery', root_flag: 'ROOT_INCONSISTENT', seal: 'pending' }),
      row({ employee_id: 'e4', label: 'freed', display_name: 'Freed', purge: { purgeable: false, purged_at: now } }),
    ];
    renderWithProviders(<AdminRestorePage />);
    const table = await screen.findByRole('table', { name: 'Removed employees' });
    const t = within(table);
    expect(t.getByText('Can be restored')).toBeInTheDocument();
    expect(t.getByText('Workspace no longer exists')).toBeInTheDocument();
    expect(t.getByText('Cannot confirm')).toBeInTheDocument();
    expect(t.getByText('Waiting to be released')).toBeInTheDocument();
    expect(t.getByText('Too large to keep')).toBeInTheDocument();
    expect(t.getByText('Owner: Bob')).toBeInTheDocument();
    expect(t.getAllByText('20 days left').length).toBeGreaterThan(0);
    expect(t.getByText('Deleted')).toBeInTheDocument();
  });

  it('approve card states: second admin, sole-admin cooling, sole-admin self-approval, owner unverified', async () => {
    list.restores!.requests = [
      req({ request_id: 'a', display_name: 'Mine', filed_by_me: true }),
      req({ request_id: 'b', display_name: 'Cooling', filed_by_me: true, sole_admin: true, cooling_until: now + 3 * DAY }),
      req({ request_id: 'c', display_name: 'Solo', filed_by_me: true, sole_admin: true, cooling_until: now - DAY }),
      req({ request_id: 'd', display_name: 'Unverified', filed_by_admin: true, owner_unverified: true, filed_by_name: 'Carol' }),
      req({ request_id: 'e', display_name: 'Other' }),
    ];
    renderWithProviders(<AdminRestorePage />);
    const mine = await screen.findByRole('group', { name: /Restore "Mine"/ });
    expect(within(mine).getByText('You sent this request, so a different admin must approve it.')).toBeInTheDocument();
    expect(within(mine).getByRole('button', { name: 'Approve' })).toBeDisabled();
    expect(within(mine).getByRole('button', { name: 'Withdraw request' })).toBeEnabled();

    const cooling = screen.getByRole('group', { name: /Restore "Cooling"/ });
    expect(within(cooling).getByText(/after the 7-day waiting period ends/)).toBeInTheDocument();
    expect(within(cooling).getByRole('button', { name: 'Approve my own request' })).toBeDisabled();

    const solo = screen.getByRole('group', { name: /Restore "Solo"/ });
    expect(within(solo).getByText(/You are the only admin\. This is recorded as approving your own request/)).toBeInTheDocument();
    expect(within(solo).getByRole('button', { name: 'Approve my own request' })).toBeEnabled();

    const unverified = screen.getByRole('group', { name: /Restore "Unverified"/ });
    expect(within(unverified).getAllByText('Owner did not confirm with two-step verification').length).toBeGreaterThan(0);
    expect(within(unverified).getByText('Sent for the owner by admin Carol')).toBeInTheDocument();
    expect(within(unverified).getByText(/Check with the owner directly before approving/)).toBeInTheDocument();
    expect(within(unverified).getByRole('button', { name: 'Approve' })).toBeEnabled();

    // Board "Restore", second-admin state: someone else filed it, so this admin may approve.
    const other = screen.getByRole('group', { name: /Restore "Other"/ });
    expect(within(other).getByText(/You are not the requester, so you can approve/)).toBeInTheDocument();
    expect(within(other).getByRole('button', { name: 'Approve' })).toBeEnabled();
  });

  it('approving asks for step-up, then shows the outcome and one-click re-grants', async () => {
    list = { step_up_valid: false, csrf_token: null, mfa_enrolled: true, restores: {
      quarantine: [], requests: [req({ request_id: 'r-shared', display_name: 'Desk', employee_kind: 'shared' })],
    } };
    let verified = false;
    mockWsClient.call.mockImplementation(async (method: string, params: Record<string, unknown> = {}) => {
      if (method === 'isolation.employee_list') {
        return verified ? { ...list, step_up_valid: true, csrf_token: TOKEN, restores: { quarantine: [], requests: [] } } : list;
      }
      if (method === 'isolation.connector_step_up') { verified = true; return { verified: true }; }
      if (method === 'isolation.employee_restore_decide') return {
        state: 'done', incarnation: 4, self_approved: false, work_paused: true, routes_need_provisioning: 1,
        label_state: { regrant: ['website_credential:crm-login', { item: 'org_edge:lead', kind: 'org_edge', label: 'Reports to Sales Lead' }], skipped: [], listed: ['route:old-mailbox'] },
        members: [{ user_id: 'u1', access_level: 'owner' }, { user_id: 'u2', access_level: 'viewer' }],
        members_dropped: [{ user_id: 'u3', name: 'Dan', reason: '帳號已停用' }], routes_reapproved: 2,
      };
      if (method === 'isolation.employee_restore_regrant') return { granted: true };
      return params && null;
    });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    expect(await screen.findByText(/you will be asked for your password and a code/)).toBeInTheDocument();
    const card = await screen.findByRole('group', { name: /Restore "Desk"/ });
    // The proof has lapsed, so the button says it will re-verify first (board: 再驗證身分並核准).
    await user.click(within(card).getByRole('button', { name: 'Verify identity and approve' }));
    const dialog = await screen.findByRole('dialog');
    await user.type(within(dialog).getByLabelText('Current password'), 'pw');
    await user.type(within(dialog).getByLabelText('Verification code'), '123456');
    await user.click(within(dialog).getByRole('button', { name: 'Verify and continue' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.employee_restore_decide', {
      request_id: 'r-shared', decision: 'approve', csrf_token: TOKEN,
    }));
    // The card stays after the reload, with its outcome.
    const result = await screen.findByRole('status', { name: 'Result' });
    expect(within(result).getByText('"Desk" is restored.')).toBeInTheDocument();
    expect(within(result).getByText(/stays paused until the owner continues/)).toBeInTheDocument();
    expect(within(result).getByText(/need to be set up again/)).toBeInTheDocument();
    expect(within(result).getByText('Access restored for 2 people.')).toBeInTheDocument();
    expect(within(result).getByText('2 connected accounts opened again.')).toBeInTheDocument();
    expect(within(result).getByText('Dan · 帳號已停用')).toBeInTheDocument();
    expect(within(result).getByText('Reports to Sales Lead')).toBeInTheDocument();
    expect(within(result).getByText('Listed only; check by hand')).toBeInTheDocument();
    await user.click(within(result).getByRole('button', { name: 'Grant again: crm-login' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.employee_restore_regrant', {
      employee_id: 'e9', incarnation: 4, item: 'website_credential:crm-login', csrf_token: TOKEN,
    }));
    expect(await within(result).findByText('Granted')).toBeInTheDocument();
  });

  it('maps a refused approval code to friendly text with the server message', async () => {
    list.restores!.requests = [req({ display_name: 'Desk' })];
    handlers['isolation.employee_restore_decide'] = () => Promise.reject({ code: 'NO_SECOND_ADMIN', message: '沒有第二位管理員。' });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    const card = await screen.findByRole('group', { name: /Restore "Desk"/ });
    await user.click(within(card).getByRole('button', { name: 'Approve' }));
    const alert = await within(card).findByRole('alert');
    expect(alert).toHaveTextContent('There is no other admin who can approve');
    expect(alert).toHaveTextContent('沒有第二位管理員。');
  });

  it('adoption card shows what is carried and what stays sealed', async () => {
    list.restores!.requests = [req({ display_name: 'Desk', kind: 'adoption', target_label: 'desk-2', carried: ['memory', 'wiki'], sealed: ['sessions', 'mystery-key'] })];
    renderWithProviders(<AdminRestorePage />);
    const card = await screen.findByRole('group', { name: /Hand "Desk" over to a new employee/ });
    expect(within(card).getByText('desk-2')).toBeInTheDocument();
    expect(within(card).getByText('Memories')).toBeInTheDocument();
    expect(within(card).getByText('Conversation history')).toBeInTheDocument();
    expect(within(card).getByText('Other items')).toBeInTheDocument();
    expect(within(card).queryByText('mystery-key')).toBeNull();
  });

  it('shared restore: snapshot members with capped levels, inactive dropped, routes ticked', async () => {
    list.restores!.quarantine = [row({})];
    handlers['isolation.shared_restore_options'] = () => ({
      members: [
        { user_id: 'u1', name: 'Ann', access_level: 'owner', is_primary: true, active: true },
        { user_id: 'u2', name: 'Ben', access_level: 'operator', active: true },
        { user_id: 'u3', name: 'Cid', access_level: 'viewer', active: false, dropped_reason: '已離職' },
      ],
      routes: [
        { account: 'acc1', generation: 1, provider: 'Gmail', kind: 'Mailbox' },
        { account: 'acc2', generation: 4, provider: 'Drive', kind: 'Folder' },
      ],
    });
    handlers['isolation.shared_restore_request'] = () => ({ request_id: 'new' });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    await user.click(await screen.findByRole('button', { name: 'Prepare restore: Sales Bot' }));
    const dialog = await screen.findByRole('dialog');
    await within(dialog).findByText('Ann');
    expect(within(dialog).getByText('Main contact')).toBeInTheDocument();
    const benLevel = within(dialog).getByRole('combobox', { name: 'Access for Ben' });
    expect(within(benLevel).queryByRole('option', { name: 'Owner' })).toBeNull();
    expect(within(benLevel).getAllByRole('option').map((o) => o.textContent)).toEqual(['Can operate', 'Can view']);
    expect(within(dialog).getByRole('checkbox', { name: 'Cid' })).toBeDisabled();
    expect(within(dialog).getByText(/Account is turned off; not restored · 已離職/)).toBeInTheDocument();
    await user.selectOptions(benLevel, 'viewer');
    await user.click(within(dialog).getByRole('checkbox', { name: 'Drive · Folder' }));
    await user.click(within(dialog).getByRole('button', { name: 'Send restore request' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.shared_restore_request', {
      employee_id: 'e1', incarnation: 3,
      members: [{ user_id: 'u1', access_level: 'owner' }, { user_id: 'u2', access_level: 'viewer' }],
      routes: [{ account: 'acc1', generation: 1 }],
      csrf_token: TOKEN,
    }));
  });

  it('early purge needs a second admin and lists what will be lost', async () => {
    list.restores!.quarantine = [row({})];
    handlers['isolation.quarantine_purge'] = () => ({ state: 'requested', lost: ['memory', 'files'] });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    await user.click(await screen.findByRole('button', { name: 'Delete early: Sales Bot' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('This data will be gone for good')).toBeInTheDocument();
    expect(within(dialog).getByText('Memories')).toBeInTheDocument();
    expect(within(dialog).getByText(/only when a different admin agrees/)).toBeInTheDocument();
    const send = within(dialog).getByRole('button', { name: 'Ask for early delete' });
    expect(send).toBeDisabled();
    await user.click(within(dialog).getByRole('checkbox', { name: 'I understand this data will be gone for good.' }));
    await user.click(send);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.quarantine_purge', {
      employee_id: 'e1', incarnation: 3, csrf_token: TOKEN, confirm_early: true,
    }));
    expect(await within(dialog).findByText(/Early delete requested/)).toBeInTheDocument();
  });

  it('90-day purge can also release host resources', async () => {
    list.restores!.quarantine = [row({ purge: { purgeable: true, purge_after: now - DAY } })];
    handlers['isolation.quarantine_purge'] = () => ({ state: 'purged' });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    await user.click(await screen.findByRole('button', { name: 'Delete data: Sales Bot' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('checkbox', { name: /Also release host resources/ }));
    await user.click(within(dialog).getByRole('checkbox', { name: 'I understand this data will be gone for good.' }));
    await user.click(within(dialog).getByRole('button', { name: 'Delete data' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.quarantine_purge', {
      employee_id: 'e1', incarnation: 3, csrf_token: TOKEN, retire: true,
    }));
    expect(await within(dialog).findByText('The data was deleted.')).toBeInTheDocument();
  });

  it('retire releases the workspace and says the host identity is never reused', async () => {
    list.restores!.quarantine = [row({ purge: { purgeable: false, purged_at: now } })];
    handlers['isolation.employee_retire_propose'] = () => ({ request_id: 'ret' });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    await user.click(await screen.findByRole('button', { name: 'Release workspace: Sales Bot' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/never given to another employee/)).toBeInTheDocument();
    await user.click(within(dialog).getByRole('checkbox'));
    await user.click(within(dialog).getByRole('button', { name: 'Release workspace' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.employee_retire_propose', {
      employee_id: 'e1', csrf_token: TOKEN,
    }));
    expect(await within(dialog).findByText('Release request sent.')).toBeInTheDocument();
  });

  it('an admin files a hand-over for an owner without a second factor; it is marked and needs another admin', async () => {
    list.restores!.quarantine = [row({ kind: 'personal', root_flag: 'ROOT_GONE', restorable: false, admin_may_file_for_owner: true, owner_name: 'Bob' })];
    handlers['isolation.adoption_request_for_owner'] = () => ({ request_id: 'fo' });
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    await user.click(await screen.findByRole('button', { name: 'Request hand-over for owner: Sales Bot' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/Owner did not confirm with two-step verification/)).toBeInTheDocument();
    const send = within(dialog).getByRole('button', { name: 'Send for owner' });
    expect(send).toBeDisabled();
    await user.click(within(dialog).getByRole('checkbox', { name: /I checked directly with Bob/ }));
    await user.click(send);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('isolation.adoption_request_for_owner', {
      employee_id: 'e1', incarnation: 3, csrf_token: TOKEN,
    }));
  });

  it('closing the step-up dialog sends nothing and shows no error', async () => {
    list = { ...list, step_up_valid: false, csrf_token: null };
    list.restores = { quarantine: [], requests: [req({ display_name: 'Desk' })] };
    const user = userEvent.setup();
    renderWithProviders(<AdminRestorePage />);
    const card = await screen.findByRole('group', { name: /Restore "Desk"/ });
    await user.click(within(card).getByRole('button', { name: 'Do not approve' }));
    const dialog = await screen.findByRole('dialog');
    await user.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(mockWsClient.call).not.toHaveBeenCalledWith('isolation.employee_restore_decide', expect.anything());
    expect(within(card).queryByRole('alert')).toBeNull();
  });
});
