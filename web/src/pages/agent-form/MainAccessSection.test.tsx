import '@/test/mocks';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { callsOf, mockRpc } from '@/pages/knowledge/test-fixtures';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import type { MainAccess } from '@/lib/api';
import { MainAccessSection } from './MainAccessSection';

const existing = { user_id: 'hidden-existing-id', display_name: 'Alice', email: 'alice@example.com', role: 'employee' as const,
  status: 'active' as const, granted_by_name: 'Admin Ada', granted_at: '2026-11-27T01:30:00Z' };
const empty: MainAccess = { hash: 'h1', users: [], risk_notice: 'Server risk notice', effective: true };
const listed: MainAccess = { ...empty, users: [existing] };
const candidate = { id: 'hidden-new-id', display_name: 'Bob', email: 'bob@example.com', role: 'manager', status: 'active' };

function setup(data: MainAccess = empty, save: (params: unknown) => unknown = () => data, get: () => MainAccess = () => data) {
  mockRpc({
    'agents.main_access.get': get,
    'agents.main_access.set': save,
    'users.list': () => ({ users: [candidate,
      { ...candidate, id: existing.user_id, display_name: 'Already listed' },
      { ...candidate, id: 'hidden-admin-id', role: 'admin', display_name: 'Excluded admin' },
      { ...candidate, id: 'hidden-inactive-id', status: 'suspended', display_name: 'Excluded suspended' },
    ] }),
  });
}
function show(role = 'main') {
  return renderWithProviders(<MainAccessSection agentRole={role} agentName="Main Bot" />);
}
async function openAdd() {
  const user = userEvent.setup();
  await user.click(await screen.findByRole('button', { name: 'Add member' }));
  const dialog = await screen.findByRole('dialog');
  await user.selectOptions(within(dialog).getByRole('combobox'), '0');
  await user.click(screen.getByRole('checkbox'));
  return user;
}

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { role: 'admin' } } as never);
  useSystemStore.setState({ status: { edition_profile: 'enterprise' } } as never);
  setup();
});

describe('MainAccessSection', () => {
  it.each(['specialist', 'manager'])('hides for %s agents without fetching', role => {
    const { container } = show(role);
    expect(container).toBeEmptyDOMElement();
    expect(callsOf('agents.main_access.get')).toHaveLength(0);
  });
  it.each(['manager', 'employee'])('hides for %s viewers', role => {
    useAuthStore.setState({ user: { role } } as never);
    expect(show().container).toBeEmptyDOMElement();
    expect(callsOf('agents.main_access.get')).toHaveLength(0);
  });
  it('hides in Personal', () => {
    useSystemStore.setState({ status: { edition_profile: 'personal' } } as never);
    expect(show().container).toBeEmptyDOMElement();
    expect(callsOf('agents.main_access.get')).toHaveLength(0);
  });
  it('shows the empty state and server risk notice', async () => {
    show();
    expect(await screen.findByText('Only admins can currently use this employee.')).toBeInTheDocument();
    expect(screen.getByText('Server risk notice')).toBeInTheDocument();
    expect(callsOf('agents.main_access.get')).toEqual([{}]);
  });
  it('falls back to the canvas risk sentence for blank notices', async () => {
    setup({ ...empty, risk_notice: '  ' });
    show();
    expect(await screen.findByText(/Added members can chat with/)).toBeInTheDocument();
  });
  it('shows names, columns, dates and the inactive warning without ids', async () => {
    setup({ ...listed, effective: false, ineffective_reason: '目前設定尚未生效。' });
    const { container } = show();
    expect(await screen.findByText('Alice')).toBeInTheDocument();
    for (const name of ['Member', 'Added by', 'Added at']) expect(screen.getByRole('columnheader', { name })).toBeInTheDocument();
    expect(screen.getByText('Admin Ada')).toBeInTheDocument();
    expect(screen.getByText(/Nov 27, 2026/)).toBeInTheDocument();
    expect(screen.getByText('目前設定尚未生效。')).toBeInTheDocument();
    expect(container.innerHTML).not.toContain(existing.user_id);
  });
  it('adds via risk acknowledgement using the full list and filters candidates', async () => {
    setup(listed, () => ({ ...listed, users: [...listed.users, { ...existing, user_id: candidate.id, display_name: candidate.display_name }] }));
    const { container } = show();
    const user = await openAdd();
    expect(screen.getByRole('dialog')).toHaveTextContent('Bob can chat with and hand work to Main Bot');
    expect(screen.getByRole('option', { name: 'Bob · bob@example.com' })).toBeInTheDocument();
    for (const name of ['Already listed', 'Excluded admin', 'Excluded suspended']) expect(screen.queryByRole('option', { name: new RegExp(name) })).toBeNull();
    expect(document.body.innerHTML).not.toContain('hidden-new-id');
    await user.click(screen.getByRole('button', { name: 'Add' }));
    await waitFor(() => expect(callsOf('agents.main_access.set')).toEqual([{ expected_hash: 'h1', user_ids: [existing.user_id, candidate.id], acknowledged: true }]));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(container.innerHTML).not.toContain('hidden-');
    expect(screen.getByText('Bob')).toBeInTheDocument();
  });
  it('removes directly with a reduced list and no acknowledgement or dialog', async () => {
    setup({ ...listed, users: [...listed.users, { ...existing, user_id: candidate.id, display_name: 'Bob', granted_by_name: null, granted_at: null }] }, () => listed);
    show();
    const row = (await screen.findByText('Bob')).closest('tr')!;
    expect(within(row).getAllByText('—')).toHaveLength(2);
    await userEvent.click(within(row).getByRole('button', { name: 'Remove' }));
    await waitFor(() => expect(callsOf('agents.main_access.set')).toEqual([{ expected_hash: 'h1', user_ids: [existing.user_id] }]));
    expect(screen.queryByRole('dialog')).toBeNull();
  });
  it.each(['設定頁的資料已過期，請重新整理後再儲存。', '設定已被其他人變更，請重新整理後再儲存。'])('locks removal on conflict until reload: %s', async message => {
    let reads = 0;
    setup(listed, () => { throw new Error(message); }, () => ++reads === 1 ? listed : empty);
    show();
    await userEvent.click(await screen.findByRole('button', { name: 'Remove' }));
    expect(await screen.findByRole('alert')).toHaveTextContent(message);
    expect(screen.getByRole('button', { name: 'Remove' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Add member' })).toBeDisabled();
    expect(callsOf('agents.main_access.get')).toHaveLength(1);
    await userEvent.click(screen.getByRole('button', { name: 'Reload' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Add member' })).toBeEnabled());
    expect(screen.queryByText('Alice')).toBeNull();
    expect(callsOf('agents.main_access.get')).toHaveLength(2);
  });
  it('closes the add dialog on conflict and locks editing', async () => {
    setup(listed, () => { throw new Error('設定已被其他人變更，請重新整理後再儲存。'); });
    show();
    const user = await openAdd();
    await user.click(screen.getByRole('button', { name: 'Add' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('設定已被其他人變更，請重新整理後再儲存。');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.getByRole('button', { name: 'Add member' })).toBeDisabled();
    expect(callsOf('agents.main_access.get')).toHaveLength(1);
  });
  it('uses the displayed list and response hash on successive saves without re-reading', async () => {
    setup(listed, () => ({ ...listed, hash: 'h2', users: [...listed.users, { ...existing, user_id: candidate.id, display_name: 'Bob' }] }));
    show();
    const user = await openAdd();
    await user.click(screen.getByRole('button', { name: 'Add' }));
    const row = (await screen.findByText('Bob')).closest('tr')!;
    await user.click(within(row).getByRole('button', { name: 'Remove' }));
    expect(callsOf('agents.main_access.set')).toEqual([
      { expected_hash: 'h1', user_ids: [existing.user_id, candidate.id], acknowledged: true },
      { expected_hash: 'h2', user_ids: [existing.user_id] },
    ]);
    expect(callsOf('agents.main_access.get')).toHaveLength(1);
  });
  it('locks settings without a hash', async () => {
    setup({ ...listed, hash: '' }); show();
    expect(await screen.findByRole('button', { name: 'Remove' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Add member' })).toBeDisabled();
    expect(callsOf('agents.main_access.set')).toHaveLength(0);
  });
  it('shows a server refusal', async () => {
    setup(listed, () => { throw new Error('只有管理員可以修改。'); });
    show();
    await userEvent.click(await screen.findByRole('button', { name: 'Remove' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('只有管理員可以修改。');
    expect(screen.getByText('Alice')).toBeInTheDocument();
  });
  it('reloads a fresh list before reopening an acknowledgement refusal', async () => {
    let reads = 0;
    setup(listed, () => { throw { code: 'acknowledgement_required', message: '請重新確認加入後的影響。' }; },
      () => ++reads < 2 ? listed : empty);
    show();
    const user = await openAdd();
    await user.click(screen.getByRole('button', { name: 'Add' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('請重新確認加入後的影響。');
    expect(screen.queryByText('Alice')).toBeNull();
    expect(screen.getByRole('checkbox')).not.toBeChecked();
    expect(callsOf('agents.main_access.get')).toHaveLength(2);
  });
  it('asks for acknowledgement again after an acknowledgement_required refusal', async () => {
    setup(listed, () => { throw { code: 'acknowledgement_required', message: '請重新確認加入後的影響。' }; });
    show();
    const user = await openAdd();
    await user.click(screen.getByRole('button', { name: 'Add' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('請重新確認加入後的影響。');
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    expect(callsOf('agents.main_access.get')).toHaveLength(2);
    expect(screen.getByRole('checkbox')).not.toBeChecked();
    expect(screen.getByRole('button', { name: 'Add' })).toBeDisabled();
  });
});
