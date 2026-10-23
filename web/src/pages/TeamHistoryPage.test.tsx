import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { TeamHistoryPage } from './TeamHistoryPage';

const team = {
  manager: { id: 'mgr', display_name: 'Manager' }, max_depth: 8, history_enabled: true,
  members: [{ id: 'emp', display_name: 'Wang', role: 'employee', status: 'active', depth: 1,
    agents: [{ name: 'sales', display_name: 'Sales', level: 'operator', history_readable: true }] }],
};
const row = (id: string) => ({ session_id: id, class: 'external', channel: 'line', report: null,
  title: `Conversation ${id}`, title_masked: true, last_active: '2026-10-23T10:00:00Z', turns: 2 });
const detail = (id: string) => ({ session_id: id, agent_id: 'sales', class: 'external', read_only: true,
  ner: false, since: '2026-10-23T00:00:00Z', masked_counts: { PHONE: 1 }, messages: [
    { role: 'user', sender_label: 'Customer 1', timestamp: '2026-10-23T10:00:00Z', parts: [
      { t: 'text', v: 'Hello <img src=x onerror=alert(1)>' }, { t: 'mask', cat: 'PHONE', n: 1 },
      { t: 'attachment', kind: 'file' }, { t: 'mask', cat: 'NAME', n: 2 },
    ] },
  ] });

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
    if (method === 'users.my_team') return Promise.resolve(team);
    if (method === 'users.team_history.list') return Promise.resolve({ sessions: [row('a'), row('b')], ner: false, since: '2026-10-23T00:00:00Z', next_before: 'cursor-2' });
    if (method === 'users.team_history.get') return Promise.resolve(detail(String(params?.session_id)));
    return Promise.reject(new Error(`Unexpected RPC ${method}`));
  });
});

describe('TeamHistoryPage', () => {
  it('renders escaped read-only parts, masking labels and the regex-only warning without links or a composer', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TeamHistoryPage />);
    await user.click(await screen.findByRole('button', { name: /Conversation a/ }));
    expect(await screen.findByText('Hello <img src=x onerror=alert(1)>')).toBeInTheDocument();
    expect(screen.getByText('Phone 1')).toBeInTheDocument();
    expect(screen.getByText('Name 2')).toBeInTheDocument();
    expect(screen.getByText(/Names and addresses may remain visible/)).toBeInTheDocument();
    expect(screen.getByText(/Every conversation you open is recorded/)).toBeInTheDocument();
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(screen.queryByRole('link', { name: /file/i })).not.toBeInTheDocument();
    expect(document.querySelector('img[src="x"]')).toBeNull();
  });

  it('sends explicit server filters and follows the returned opaque cursor', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TeamHistoryPage />);
    await screen.findByRole('button', { name: /Conversation a/ });
    await user.selectOptions(screen.getByLabelText('Conversation type'), 'report_webchat');
    await user.selectOptions(screen.getByLabelText('Report'), 'emp');
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.team_history.list', expect.objectContaining({ agent_id: 'sales', class: 'report_webchat', report_user_id: 'emp' })));
    await user.click(await screen.findByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('users.team_history.list', expect.objectContaining({ before: 'cursor-2' })));
  });

  it('never puts an older in-flight response into the newly selected conversation', async () => {
    const user = userEvent.setup();
    let finish!: (value: unknown) => void;
    const normal = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method, params) => method === 'users.team_history.get' && params.session_id === 'a'
      ? new Promise(resolve => { finish = resolve; }) : normal(method, params));
    renderWithProviders(<TeamHistoryPage />);
    await user.click(await screen.findByRole('button', { name: /Conversation a/ }));
    await user.click(screen.getByRole('button', { name: /Conversation b/ }));
    await screen.findByText('Hello <img src=x onerror=alert(1)>');
    await act(async () => finish({ ...detail('a'), messages: [{ role: 'user', parts: [{ t: 'text', v: 'STALE SECRET' }] }] }));
    expect(screen.queryByText('STALE SECRET')).not.toBeInTheDocument();
  });

  it.each([false, true])('restarts the scope when a load-more response advances the activation window (nonempty: %s)', async (nonempty) => {
    const user = userEvent.setup();
    let teamReads = 0;
    let firstPages = 0;
    let refreshTeam!: (value: unknown) => void;
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'users.my_team') return ++teamReads === 1 ? Promise.resolve(team) : new Promise(resolve => { refreshTeam = resolve; });
      if (method === 'users.team_history.get') return Promise.resolve(detail(String(params?.session_id)));
      if (method === 'users.team_history.list') {
        if (params?.before) return Promise.resolve({ sessions: nonempty ? [row('discarded-next-page')] : [], ner: false, since: '2026-10-23T11:00:00Z', next_before: null });
        return Promise.resolve(++firstPages === 1
          ? { sessions: [row('a')], ner: false, since: '2026-10-23T00:00:00Z', next_before: 'old-window-cursor' }
          : { sessions: [row('fresh')], ner: false, since: '2026-10-23T11:00:00Z', next_before: null });
      }
      return Promise.reject(new Error(`Unexpected RPC ${method}`));
    });
    renderWithProviders(<TeamHistoryPage />);
    await user.click(await screen.findByRole('button', { name: /Conversation a/ }));
    await screen.findByText('Hello <img src=x onerror=alert(1)>');
    await user.click(screen.getByRole('button', { name: 'Load more' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: /Conversation a/ })).not.toBeInTheDocument());
    expect(screen.queryByText('Hello <img src=x onerror=alert(1)>')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /discarded-next-page/ })).not.toBeInTheDocument();
    await waitFor(() => expect(teamReads).toBe(2));
    expect(firstPages).toBe(1); // Wait for fresh scope before the new first page.
    await act(async () => refreshTeam(team));
    await screen.findByRole('button', { name: /Conversation fresh/ });
    expect(firstPages).toBe(2);
    expect(mockWsClient.call.mock.calls.filter(([method, params]) => method === 'users.team_history.list' && params.before)).toHaveLength(1);
    expect(screen.queryByText('Hello <img src=x onerror=alert(1)>')).not.toBeInTheDocument();
  });

  it('also restarts instead of mixing a newly activated transcript into an old-window list', async () => {
    const user = userEvent.setup();
    let restarted = false;
    mockWsClient.call.mockImplementation((method: string, params?: Record<string, unknown>) => {
      if (method === 'users.my_team') return Promise.resolve(team);
      if (method === 'users.team_history.list') return Promise.resolve({ sessions: restarted ? [] : [row('a'), row('b')], ner: false, since: restarted ? '2026-10-23T11:00:00Z' : '2026-10-23T00:00:00Z' });
      if (method === 'users.team_history.get') {
        const transcript = detail(String(params?.session_id));
        if (params?.session_id === 'b') {
          restarted = true;
          return Promise.resolve({ ...transcript, since: '2026-10-23T11:00:00Z', messages: [{ role: 'user', sender_label: 'Customer 1', timestamp: '', parts: [{ t: 'text', v: 'NEW WINDOW RESPONSE' }] }] });
        }
        return Promise.resolve(transcript);
      }
      return Promise.reject(new Error(`Unexpected RPC ${method}`));
    });
    renderWithProviders(<TeamHistoryPage />);
    await user.click(await screen.findByRole('button', { name: /Conversation a/ }));
    await screen.findByText('Hello <img src=x onerror=alert(1)>');
    await user.click(screen.getByRole('button', { name: /Conversation b/ }));
    await waitFor(() => expect(screen.queryByRole('button', { name: /Conversation a/ })).not.toBeInTheDocument());
    expect(screen.queryByText('NEW WINDOW RESPONSE')).not.toBeInTheDocument();
    expect(screen.queryByText('Hello <img src=x onerror=alert(1)>')).not.toBeInTheDocument();
    expect(mockWsClient.call.mock.calls.filter(([method]) => method === 'users.my_team')).toHaveLength(2);
  });

  it('clears previously displayed content when a refreshed read is refused', async () => {
    const user = userEvent.setup();
    renderWithProviders(<TeamHistoryPage />);
    await user.click(await screen.findByRole('button', { name: /Conversation a/ }));
    await screen.findByText('Hello <img src=x onerror=alert(1)>');
    mockWsClient.call.mockRejectedValue({ code: 'not_found', message: 'session not found' });
    await user.click(screen.getByRole('button', { name: 'Refresh' }));
    await waitFor(() => expect(screen.queryByText('Hello <img src=x onerror=alert(1)>')).not.toBeInTheDocument());
    expect(await screen.findByRole('alert')).toBeInTheDocument();
  });

  it('shows the organisation-off state without attempting a disclosure', async () => {
    mockWsClient.call.mockResolvedValue({ ...team, history_enabled: false });
    renderWithProviders(<TeamHistoryPage />);
    expect(await screen.findByText('Your organisation has turned off manager conversation access.')).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('users.team_history.get', expect.anything());
  });
});
