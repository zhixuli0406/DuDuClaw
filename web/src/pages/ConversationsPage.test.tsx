import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ConversationsPage } from './ConversationsPage';
import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { useAuthStore } from '@/stores/auth-store';
import { useChatStore } from '@/stores/chat-store';
import { useConnectionStore } from '@/stores/connection-store';

/** `n` webchat sessions, newest first, alternating between two employees. */
function fakeSessions(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    session_id: `webchat:c${i}#agent:${i % 2 === 0 ? 'main' : 'sales'}#conv:${i}`,
    agent_id: i % 2 === 0 ? 'main' : 'sales',
    title: `對話 ${i + 1}`,
    last_active: new Date(Date.UTC(2026, 7, 4, 12, 0, 0) - i * 60_000).toISOString(),
    turns: 3,
    tokens: 100,
  }));
}

const AGENTS = [
  { name: 'main', display_name: '嘟嘟', role: 'main', status: 'active' },
  { name: 'sales', display_name: '業務小美', role: 'staff', status: 'active' },
];

beforeEach(() => {
  vi.restoreAllMocks();
  // The page refreshes the roster on mount; without this the display names
  // would be replaced by raw ids and the label assertions would be vacuous.
  vi.spyOn(api.agents, 'list').mockResolvedValue({ agents: AGENTS } as never);
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' } as never });
  useConnectionStore.setState({ state: 'authenticated' as never });
  useChatStore.setState({ selectedAgentId: null, sessionId: null });
  useAgentsStore.setState({ agents: AGENTS as never, loaded: true } as never);
});

describe('ConversationsPage (full history, D17)', () => {
  it('pages through the history 20 rows at a time, newest first', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(25) } as never);
    renderWithProviders(<ConversationsPage />);

    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());
    expect(screen.getByText('對話 20')).toBeInTheDocument();
    expect(screen.queryByText('對話 21')).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Next page/i }));
    expect(screen.getByText('對話 21')).toBeInTheDocument();
    expect(screen.queryByText('對話 1')).not.toBeInTheDocument();
  });

  it('filters by free text across title and employee name', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(4) } as never);
    renderWithProviders(<ConversationsPage />);
    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());

    // 業務小美 owns the odd-indexed sessions (對話 2 and 對話 4).
    await user.type(screen.getByRole('textbox'), '業務小美');
    await waitFor(() => expect(screen.queryByText('對話 1')).not.toBeInTheDocument());
    expect(screen.getByText('對話 2')).toBeInTheDocument();
    expect(screen.getByText('對話 4')).toBeInTheDocument();
  });

  it('lets an admin enumerate every employee', async () => {
    const list = vi
      .spyOn(api.chatSessions, 'list')
      .mockResolvedValue({ sessions: fakeSessions(2) } as never);
    renderWithProviders(<ConversationsPage />);
    await waitFor(() => expect(list).toHaveBeenCalledWith({ limit: 200 }));
  });

  it('scopes the listing for a non-admin, who may not enumerate everyone', async () => {
    // `chat.sessions.list` is fail-closed: a non-admin MUST name an employee or
    // the gateway rejects the call. The page therefore scopes to the employee
    // the chat view is pointed at (falling back to the main agent) rather than
    // firing an unscoped request and showing an error.
    useAuthStore.setState({ user: { display_name: 'E', role: 'employee' } as never });
    const list = vi
      .spyOn(api.chatSessions, 'list')
      .mockResolvedValue({ sessions: fakeSessions(2) } as never);
    renderWithProviders(<ConversationsPage />);
    await waitFor(() => expect(list).toHaveBeenCalledWith({ agent_id: 'main', limit: 200 }));
    expect(list).not.toHaveBeenCalledWith({ limit: 200 });

    // …and the filter must not advertise a breadth the RPC would refuse: it
    // names the employee actually in scope instead of "All AI staff".
    expect(screen.getByRole('combobox')).toHaveTextContent('嘟嘟');
    expect(screen.queryByText(/All AI staff/i)).not.toBeInTheDocument();
  });

  it('drops internal work sessions — only real conversations are listed', async () => {
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({
      sessions: [
        ...fakeSessions(1),
        {
          session_id: 'cron:daily-report',
          agent_id: 'main',
          title: '每日巡邏',
          last_active: new Date().toISOString(),
          turns: 1,
          tokens: 10,
        },
      ],
    } as never);
    renderWithProviders(<ConversationsPage />);
    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());
    expect(screen.queryByText('每日巡邏')).not.toBeInTheDocument();
  });

  it('says so plainly when a filter matches nothing', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(2) } as never);
    renderWithProviders(<ConversationsPage />);
    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());

    await user.type(screen.getByRole('textbox'), 'zzzz-no-such-thing');
    await waitFor(() =>
      expect(screen.getByText(/No conversations match/i)).toBeInTheDocument(),
    );
    // A filtered-empty view must offer a way out of the filter, not tell the
    // user to go start a conversation (2026-08-04 review, FINDING-5).
    expect(screen.getByText(/clear the filters/i)).toBeInTheDocument();
    expect(screen.queryByText(/Say something to an AI employee/i)).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Clear filters/i }));
    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());
  });
});
