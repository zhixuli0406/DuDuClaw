import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { beforeEach, expect, it, vi } from 'vitest';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import en from '@/i18n/en.json';
import { SessionHistoryMenu } from './SessionHistoryMenu';

const privateRow = {
  session_id: 'whatsapp:+886912345678', agent_id: 'sales',
  title: 'OWNER MENU PRIVATE TITLE', last_active: '2026-10-24T00:00:00Z',
  turns: 2, tokens: 0, channel: 'whatsapp',
};
const masked = {
  ...privateRow, session_id: `view:v1:s:${'b'.repeat(64)}`,
  title: 'IGNORED SERVER TITLE', read_only: true,
};
beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({
    user: { id: 'same', display_name: 'Employee', role: 'employee', status: 'active' } as never,
    bindings: [{ user_id: 'same', agent_name: 'sales', access_level: 'owner', bound_at: '' }],
  });
});
function operator() {
  useAuthStore.setState({ bindings: [{ user_id: 'same', agent_name: 'sales', access_level: 'operator', bound_at: '' }] });
}
async function openMenu() {
  await userEvent.setup().click(screen.getByRole('button', { name: en['webchat.history'] }));
}
it('erases the open Owner menu immediately on a same-user binding downgrade and shows only the new scoped rows', async () => {
  let resolveScoped!: (value: unknown) => void;
  const list = vi.spyOn(api.chatSessions, 'list')
    .mockResolvedValueOnce({ sessions: [privateRow] } as never)
    .mockImplementationOnce(() => new Promise(resolve => { resolveScoped = resolve; }) as never);
  const onResume = vi.fn();
  renderWithProviders(<SessionHistoryMenu agentId="sales" activeSessionId={null} onResume={onResume} />);
  await openMenu();
  await screen.findByText(privateRow.title);
  act(operator);
  expect(screen.queryByText(privateRow.title)).not.toBeInTheDocument();
  await waitFor(() => expect(list).toHaveBeenCalledTimes(2));
  await act(async () => { resolveScoped({ sessions: [masked] }); });
  expect(await screen.findByText(en['boundHistory.label'])).toBeInTheDocument();
  expect(screen.queryByText(masked.title)).not.toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole('menuitem'));
  expect(onResume).toHaveBeenCalledWith(masked);
});
it('does not land an old Owner response after a status transition and fresh scoped response', async () => {
  let resolveOwner!: (value: unknown) => void;
  vi.spyOn(api.chatSessions, 'list')
    .mockImplementationOnce(() => new Promise(resolve => { resolveOwner = resolve; }) as never)
    .mockResolvedValueOnce({ sessions: [masked] } as never);
  renderWithProviders(<SessionHistoryMenu agentId="sales" activeSessionId={null} onResume={vi.fn()} />);
  await openMenu();
  await waitFor(() => expect(resolveOwner).toBeTypeOf('function'));
  act(() => { useAuthStore.setState({ user: { ...useAuthStore.getState().user!, status: 'suspended' } }); });
  expect(await screen.findByText(en['webchat.history.empty'])).toBeInTheDocument();
  act(() => { useAuthStore.setState({ user: { ...useAuthStore.getState().user!, status: 'active' } }); });
  await screen.findByText(en['boundHistory.label']);
  await act(async () => { resolveOwner({ sessions: [privateRow] }); });
  expect(screen.queryByText(privateRow.title)).not.toBeInTheDocument();
  expect(screen.getByText(en['boundHistory.label'])).toBeInTheDocument();
});
it('RM-173: lists archived conversations on demand and restores only the caller\'s own', async () => {
  const user = userEvent.setup();
  const own = { ...privateRow, session_id: 'webchat:mine', channel: 'webchat', title: 'MY OLD CHAT', archived: true };
  const list = vi.spyOn(api.chatSessions, 'list').mockImplementation(((params: { archived?: boolean }) =>
    Promise.resolve({ sessions: params.archived ? [own, { ...masked, archived: true }] : [privateRow] })) as never);
  const restore = vi.spyOn(api.chatSessions, 'restore').mockResolvedValue({ session_id: own.session_id, restored: true });
  renderWithProviders(<SessionHistoryMenu agentId="sales" activeSessionId={null} onResume={vi.fn()} />);
  await openMenu();
  await screen.findByText(privateRow.title);
  expect(list).toHaveBeenLastCalledWith({ agent_id: 'sales', limit: 50 });
  await user.click(screen.getByRole('button', { name: en['webchat.history.archivedToggle'] }));
  expect(await screen.findByText(own.title)).toBeInTheDocument();
  expect(list).toHaveBeenLastCalledWith({ agent_id: 'sales', limit: 50, archived: true });
  expect(screen.queryByText(privateRow.title)).not.toBeInTheDocument();
  // Archived rows are not resumable menu items; a masked read-only row has no restore.
  expect(screen.queryByRole('menuitem')).not.toBeInTheDocument();
  expect(screen.getAllByRole('button', { name: new RegExp(`^${en['webchat.history.restore']} `) })).toHaveLength(1);
  await user.click(screen.getByRole('button', { name: `${en['webchat.history.restore']} ${own.title}` }));
  expect(restore).toHaveBeenCalledWith(own.session_id);
  await waitFor(() => expect(screen.queryByText(own.title)).not.toBeInTheDocument());
  // Back to the normal list.
  await user.click(screen.getByRole('button', { name: en['webchat.history.archivedToggle'] }));
  expect(await screen.findByText(privateRow.title)).toBeInTheDocument();
});
it('RM-173: a refused restore keeps the row and says so', async () => {
  const user = userEvent.setup();
  const own = { ...privateRow, session_id: 'webchat:mine', channel: 'webchat', title: 'MY OLD CHAT', archived: true };
  vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: [own] } as never);
  vi.spyOn(api.chatSessions, 'restore').mockRejectedValue(new Error('permission denied'));
  renderWithProviders(<SessionHistoryMenu agentId="sales" activeSessionId={null} onResume={vi.fn()} />);
  await openMenu();
  await user.click(screen.getByRole('button', { name: en['webchat.history.archivedToggle'] }));
  await user.click(await screen.findByRole('button', { name: `${en['webchat.history.restore']} ${own.title}` }));
  expect(await screen.findByRole('alert')).toHaveTextContent(en['webchat.history.restoreFailed']);
  expect(screen.getByText(own.title)).toBeInTheDocument();
});
it('clears cached rows on refresh refusal, then accepts an untitled own row on retry', async () => {
  vi.spyOn(api.chatSessions, 'list')
    .mockResolvedValueOnce({ sessions: [privateRow] } as never)
    .mockRejectedValueOnce(new Error('permission denied'))
    .mockResolvedValueOnce({ sessions: [{ ...privateRow, session_id: 'webchat:mine', channel: 'webchat', title: null }] } as never);
  renderWithProviders(<SessionHistoryMenu agentId="sales" activeSessionId={null} onResume={vi.fn()} />);
  await openMenu();
  await screen.findByText(privateRow.title);
  await userEvent.setup().click(screen.getByTitle(en['webchat.history.reload']));
  await screen.findByText(en['webchat.history.error']);
  expect(screen.queryByText(privateRow.title)).not.toBeInTheDocument();
  await userEvent.setup().click(screen.getByRole('button', { name: en['webchat.history.retry'] }));
  expect(await screen.findByText(en['webchat.history.untitled'])).toBeInTheDocument();
});
