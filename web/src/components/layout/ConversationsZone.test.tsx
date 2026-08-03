import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SidebarProvider } from '@/components/mds';
import { ConversationsZone } from './ConversationsZone';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useChatStore } from '@/stores/chat-store';
import { useConnectionStore } from '@/stores/connection-store';
import { useConversationsStore } from '@/stores/conversations-store';

/** Server returns newest-first; `n` sessions titled 對話 1 … 對話 n. */
function fakeSessions(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    session_id: `webchat:c${i}#agent:main#conv:${i}`,
    agent_id: 'main',
    title: `對話 ${i + 1}`,
    last_active: new Date(Date.UTC(2026, 6, 30, 12, 0, 0) - i * 60_000).toISOString(),
    turns: 3,
    tokens: 100,
  }));
}

function renderZone() {
  return renderWithProviders(
    <SidebarProvider>
      <ConversationsZone collapsed={false} sectionCollapsed={false} onToggle={() => {}} />
    </SidebarProvider>,
  );
}

beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' } as never });
  useConnectionStore.setState({ state: 'authenticated' as never });
  useChatStore.setState({ selectedAgentId: null, sessionsRevision: 0, sessionId: null });
  useConversationsStore.setState({ sessions: [], status: 'idle' });
});

describe('ConversationsZone (sidebar 對話紀錄)', () => {
  it('lists recent conversations, newest first, capped at 15', async () => {
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(20) } as never);
    renderZone();

    await waitFor(() => expect(screen.getByText('對話 1')).toBeInTheDocument());
    // The server orders by last_active DESC, so the rail must preserve that
    // order and take the FIRST 15 — not an arbitrary slice.
    expect(screen.getByText('對話 15')).toBeInTheDocument();
    expect(screen.queryByText('對話 16')).not.toBeInTheDocument();
    const rows = screen.getAllByRole('button').filter((b) => /^對話 \d+$/.test(b.textContent ?? ''));
    expect(rows).toHaveLength(15);
    expect(rows[0]).toHaveTextContent('對話 1');
  });

  it('resumes a conversation into the chat view when a row is clicked', async () => {
    const user = userEvent.setup();
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(2) } as never);
    const historySpy = vi.spyOn(api.chatSessions, 'history').mockResolvedValue({
      session_id: 'webchat:c1#agent:main#conv:1',
      agent_id: 'main',
      messages: [{ role: 'user', content: '上次聊到哪', timestamp: new Date().toISOString() }],
    } as never);

    renderZone();
    await waitFor(() => expect(screen.getByText('對話 2')).toBeInTheDocument());
    await user.click(screen.getByText('對話 2'));

    await waitFor(() =>
      expect(historySpy).toHaveBeenCalledWith('webchat:c1#agent:main#conv:1'),
    );
    await waitFor(() => {
      const chat = useChatStore.getState();
      expect(chat.sessionId).toBe('webchat:c1#agent:main#conv:1');
      expect(chat.messages.map((m) => m.content)).toEqual(['上次聊到哪']);
    });
  });

  it('says so plainly when there is no history yet', async () => {
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: [] } as never);
    renderZone();
    await waitFor(() => expect(screen.getByText(/No conversations yet/i)).toBeInTheDocument());
  });

  it('renders nothing in the icon-only collapsed rail', () => {
    vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: fakeSessions(3) } as never);
    const { container } = renderWithProviders(
      <SidebarProvider>
        <ConversationsZone collapsed sectionCollapsed={false} onToggle={() => {}} />
      </SidebarProvider>,
    );
    expect(container.querySelector('[data-slot="sidebar-group"]')).toBeNull();
  });
});
