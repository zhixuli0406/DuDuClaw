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
