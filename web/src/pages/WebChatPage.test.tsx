import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, act, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WebChatPage } from './WebChatPage';
import { useChatStore, historyToMessages } from '@/stores/chat-store';
import { useConversationsStore } from '@/stores/conversations-store';
import { useAuthStore } from '@/stores/auth-store';
import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';
import { render } from '@testing-library/react';
import { IntlProvider } from 'react-intl';
import { MemoryRouter, Route, Routes } from 'react-router';
import en from '@/i18n/en.json';
import { useBrandStore } from '@/stores/brand-store';
import { stashChatAttachments } from '@/lib/chat-handoff';

beforeEach(() => {
  vi.clearAllMocks();
  useBrandStore.getState().setBrand(null);
  useChatStore.setState({
    messages: [],
    isStreaming: false,
    sessionId: null,
    sessionsRevision: 0,
    connectionState: 'connected' as never,
    agentName: 'DuDuClaw',
    agentIcon: '',
  });
});

describe('WebChatPage', () => {
  it('uses the verified product name for the default partner header and heading', () => {
    useBrandStore.getState().setBrand({ product_name: '晨光智能', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    useChatStore.setState({ selectedAgentId: null, agentName: 'DuDuClaw' });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByRole('heading', { name: '晨光智能' })).toBeInTheDocument();
    expect(screen.getAllByText('晨光智能')).toHaveLength(2);
    expect(screen.queryByText('DuDuClaw')).not.toBeInTheDocument();
  });

  it('shows oversight notice only for the selected server-confirmed staff and removes it after refresh', async () => {
    useChatStore.setState({ selectedAgentId: 'sales', sessionId: null });
    useAgentsStore.setState({ loaded: true, agents: [{ name: 'sales', display_name: 'Sales', role: 'specialist', status: 'active' }] as never });
    const sales = { name: 'sales', display_name: 'Sales', call_text: '@Sales', role: 'specialist', status: 'active', via: 'binding' as const, level: 'operator' as const, assignable: true, selectable: true, manager_may_read: true };
    const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [sales], default_agent: null });
    renderWithProviders(<WebChatPage />);
    const notice = await screen.findByRole('link', { name: 'Your manager can read your conversations with this AI staff member (read-only).' });
    expect(notice).toHaveAttribute('href', expect.stringContaining('68-manager-team-history.md'));
    callable.mockResolvedValue({ agents: [{ ...sales, manager_may_read: false }], default_agent: null });
    fireEvent(window, new Event('focus'));
    await waitFor(() => expect(screen.queryByRole('link', { name: /Your manager can read/ })).not.toBeInTheDocument());
  });

  it('picks up the 問一問 draft and the handed-over files from the 交辦任務 page', async () => {
    const attachmentsKey = stashChatAttachments([{ name: 'brief.txt', mime: 'text/plain', dataBase64: 'aGk=' }]);
    render(
      <IntlProvider messages={en} locale="en" defaultLocale="en">
        <MemoryRouter initialEntries={[{ pathname: '/chat', state: { draft: 'look at this', attachmentsKey } }]}>
          <Routes>
            <Route path="/chat" element={<WebChatPage />} />
          </Routes>
        </MemoryRouter>
      </IntlProvider>,
    );
    expect(await screen.findByDisplayValue('look at this')).toBeInTheDocument();
    expect(screen.getByText('brief.txt')).toBeInTheDocument();
  });

  it('renders the chat composer', () => {
    renderWithProviders(<WebChatPage />);
    expect(screen.getByPlaceholderText(/message/i)).toBeInTheDocument();
  });

  it('renders the list+detail split (conversations column)', () => {
    renderWithProviders(<WebChatPage />);
    // The left column carries a "New conversation" action.
    expect(screen.getAllByLabelText(/new conversation/i).length).toBeGreaterThan(0);
  });

  it('displays messages from store', () => {
    useChatStore.setState({
      messages: [
        { id: '1', role: 'user', content: 'Hello there', timestamp: Date.now() },
        { id: '2', role: 'assistant', content: 'Hi! How can I help?', timestamp: Date.now() },
      ],
    });

    renderWithProviders(<WebChatPage />);

    expect(screen.getByText('Hello there')).toBeInTheDocument();
    expect(screen.getByText('Hi! How can I help?')).toBeInTheDocument();
  });

  it('shows a busy indicator while the assistant is streaming', () => {
    useChatStore.setState({ isStreaming: true });

    renderWithProviders(<WebChatPage />);

    const animated = document.querySelectorAll('[class*="animate"]');
    expect(animated.length).toBeGreaterThan(0);
  });

  it('does not send on Enter while a CJK IME is composing, but sends on a plain Enter', () => {
    const send = vi.fn();
    useChatStore.setState({ send: send as never, isStreaming: false, connectionState: 'connected' as never });

    renderWithProviders(<WebChatPage />);
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '你好' } });

    // Composing Enter — Chrome/Firefox signal (nativeEvent.isComposing).
    fireEvent.keyDown(input, { key: 'Enter', isComposing: true });
    // Composing Enter — Safari signal (keyCode 229 after compositionend).
    fireEvent.keyDown(input, { key: 'Enter', keyCode: 229 });
    expect(send).not.toHaveBeenCalled();

    // A real Enter (composition already committed) sends.
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith('你好', expect.anything());
  });

  it('refreshes the conversation list when a reply completes (sessionsRevision bump)', async () => {
    const listSpy = vi
      .spyOn(api.chatSessions, 'list')
      .mockResolvedValue({ sessions: [] } as never);
    // `chat.sessions.list` is fail-closed: only an admin may enumerate without
    // scoping to an agent, so the viewer has to be one for the RPC to fire.
    useAuthStore.setState({ user: { display_name: 'Boss', role: 'admin' } as never });
    useChatStore.setState({ connectionState: 'connected' as never, sessionsRevision: 0 });

    renderWithProviders(<WebChatPage />);
    await waitFor(() => expect(listSpy).toHaveBeenCalled());
    const before = listSpy.mock.calls.length;

    // Completing a reply bumps sessionsRevision → the list re-fetches so a
    // just-created conversation shows up (and stays resumable) immediately.
    await act(async () => {
      useChatStore.setState({ sessionsRevision: 1 });
    });
    await waitFor(() => expect(listSpy.mock.calls.length).toBeGreaterThan(before));
  });

  it('draws the product mark, not the 🐾 placeholder, when no agent icon is set', () => {
    useChatStore.setState({ agentIcon: '', selectedAgentId: null });
    renderWithProviders(<WebChatPage />);

    expect(document.querySelector('[data-brand-mark]')).not.toBeNull();
    expect(screen.queryByText('🐾')).toBeNull();
  });

  it('draws the agent icon when the server sends one', () => {
    useChatStore.setState({ agentIcon: '🦊', selectedAgentId: null });
    renderWithProviders(<WebChatPage />);

    expect(screen.getByText('🦊')).toBeInTheDocument();
    expect(document.querySelector('[data-brand-mark]')).toBeNull();
  });

  it('allows typing a message into the composer', () => {
    renderWithProviders(<WebChatPage />);

    // NOTE: the split panes have no measured size under jsdom's mocked
    // ResizeObserver, so userEvent's visibility check skips typing — drive the
    // controlled textarea directly instead.
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: 'Test message' } });

    expect(input).toHaveValue('Test message');
  });

  it('typing @ lists the callable AI staff and Enter inserts the picked name', () => {
    const send = vi.fn();
    useChatStore.setState({ send: send as never, isStreaming: false, connectionState: 'connected' as never });
    useAgentsStore.setState({
      loaded: true,
      agents: [
        { name: 'dudu', display_name: '嘟嘟總機', role: 'main', status: 'active', trigger: '@嘟嘟總機', icon: '', reports_to: '' },
        { name: 'finance', display_name: '財務小幫手', role: 'specialist', status: 'active', trigger: '@財務小幫手', icon: '', reports_to: '' },
      ] as never,
    });
    renderWithProviders(<WebChatPage />);
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '@財', selectionStart: 2 } });
    const list = screen.getByRole('listbox');
    expect(list).toBeInTheDocument();
    expect(screen.getByRole('combobox')).toHaveAttribute('aria-expanded', 'true');
    const options = screen.getAllByRole('option');
    expect(options).toHaveLength(1);
    expect(options[0]).toHaveTextContent('財務小幫手');
    expect(input).toHaveAttribute('aria-activedescendant', options[0].id);

    // Enter picks instead of sending.
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(send).not.toHaveBeenCalled();
    expect(input.value).toBe('@財務小幫手 ');
    expect(screen.queryByRole('listbox')).toBeNull();

    // Escape closes the list for a bare @.
    fireEvent.change(input, { target: { value: '@', selectionStart: 1 } });
    expect(screen.getAllByRole('option')).toHaveLength(2);
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(screen.queryByRole('listbox')).toBeNull();
  });

  it('lists what agents.callable returns, grouped mine / team / public, and inserts the server call text', async () => {
    const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({
      default_agent: 'dudu',
      agents: [
        { name: 'dudu', display_name: '嘟嘟總機', call_text: '@嘟嘟總機', role: 'main', status: 'active', via: 'default', level: null, assignable: false, selectable: false },
        { name: 'faq', display_name: '常見問題', call_text: '@常見問題', role: 'specialist', status: 'active', via: 'public', level: null, assignable: false, selectable: false },
        { name: 'hr', display_name: '人資助理', call_text: '@人資助理', role: 'specialist', status: 'active', via: 'team', via_user: 'u2', via_user_name: '林小華', depth: 1, level: 'viewer', assignable: false, selectable: true },
        { name: 'sales-rep', display_name: '業務專員', call_text: '@業務專員', role: 'specialist', status: 'active', via: 'team', via_user: 'u1', via_user_name: '王小明', depth: 2, level: 'operator', assignable: true, selectable: true },
      ],
    });
    useChatStore.setState({ isStreaming: false, connectionState: 'connected' as never });
    // agents.list (direct bindings) knows nothing about the team.
    useAgentsStore.setState({ loaded: true, agents: [] as never });
    renderWithProviders(<WebChatPage />);
    await waitFor(() => expect(callable).toHaveBeenCalled());
    // Team staff are pickable partners; default and public ones are not chips.
    await waitFor(() => expect(screen.getByRole('radio', { name: /業務專員/ })).toBeInTheDocument());
    expect(screen.queryByRole('radio', { name: /常見問題/ })).toBeNull();

    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '@', selectionStart: 1 } });
    await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(4));
    const headings = screen.getAllByTestId('mention-group').map((h) => h.textContent);
    expect(headings).toEqual([
      'My AI staff',
      "My team: 林小華's AI staff",
      "My team: 王小明's AI staff",
      'Public AI staff',
    ]);
    // Keyboard order follows the sections.
    const options = screen.getAllByRole('option').map((o) => o.textContent);
    expect(options[0]).toContain('嘟嘟總機');
    expect(options[3]).toContain('常見問題');
    fireEvent.change(input, { target: { value: '@業', selectionStart: 2 } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(input.value).toBe('@業務專員 ');
  });

  it('after /交辦 the @ list offers only staff who can take a task', async () => {
    const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({
      default_agent: 'dudu',
      agents: [
        { name: 'dudu', display_name: '嘟嘟總機', call_text: '@嘟嘟總機', role: 'main', status: 'active', via: 'default', level: null, assignable: false, selectable: false },
        { name: 'hr', display_name: '人資助理', call_text: '@人資助理', role: 'specialist', status: 'active', via: 'team', via_user: 'u2', via_user_name: '林小華', depth: 1, level: 'viewer', assignable: false, selectable: true },
        { name: 'sales-rep', display_name: '業務專員', call_text: '@業務專員', role: 'specialist', status: 'active', via: 'team', via_user: 'u1', via_user_name: '王小明', depth: 2, level: 'operator', assignable: true, selectable: true },
      ],
    });
    useChatStore.setState({ isStreaming: false, connectionState: 'connected' as never });
    useAgentsStore.setState({ loaded: true, agents: [] as never });
    renderWithProviders(<WebChatPage />);
    await waitFor(() => expect(callable).toHaveBeenCalled());
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: '/交辦 @', selectionStart: 5 } });
    await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(1));
    expect(screen.getAllByRole('option')[0]).toHaveTextContent('業務專員');
    // Ordinary text lists everyone callable again.
    fireEvent.change(input, { target: { value: '@', selectionStart: 1 } });
    await waitFor(() => expect(screen.getAllByRole('option')).toHaveLength(3));
  });

  it('shows a task card under a reply that created a task', () => {
    useChatStore.setState({
      isStreaming: false,
      messages: [
        { id: 'u1', role: 'user', content: '/交辦 @業務專員 整理報價單', timestamp: 1 },
        {
          id: 'a1',
          role: 'assistant',
          content: '📌 已交辦給【業務專員】',
          timestamp: 2,
          task: { id: '1a2b3c4d-0000-5000-8000-000000000000', agent: 'sales-rep' },
        },
      ] as never,
    });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByText('Task #1a2b3c4d assigned')).toBeInTheDocument();
    expect(screen.getByRole('link', { name: 'View progress' })).toHaveAttribute('href', '/goals?assigned=me');
  });

  it('labels a reply that another AI staff member answered', () => {
    useChatStore.setState({
      selectedAgentId: null,
      messages: [
        { id: '1', role: 'assistant', content: '本月支出 3 萬', timestamp: Date.now(), agentId: 'finance', agentName: '財務小幫手' },
      ],
    });
    useAgentsStore.setState({
      loaded: true,
      agents: [{ name: 'dudu', display_name: '嘟嘟總機', role: 'main', status: 'active', trigger: '', icon: '', reports_to: '' }] as never,
    });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByText(/財務小幫手/)).toBeInTheDocument();
  });
});


it('preserves the composer draft during history refresh and window focus', async () => {
  const user = { id: 1, role: 'user', content: 'waiting question', timestamp: '2026-11-01T00:00:00Z', tokens: 0 };
  const reply = { id: 2, role: 'assistant', content: 'arrived reply', timestamp: '2026-11-01T00:01:00Z', tokens: 2 };
  useConversationsStore.setState({ viewOnly: false });
  useChatStore.getState().resumeSession('webchat:past', historyToMessages([user]));
  const load = vi.spyOn(api.chatSessions, 'history').mockResolvedValue({
    session_id: 'webchat:past', agent_id: 'dudu', channel: 'webchat', privacy_mode: 'own', messages: [user, reply],
  });
  const resumeView = vi.spyOn(useConversationsStore.getState(), 'refreshView');
  const view = renderWithProviders(<WebChatPage />);
  try {
    const input = screen.getByPlaceholderText(/message/i);
    fireEvent.change(input, { target: { value: 'unsent draft' } });
    fireEvent(window, new Event('focus'));
    expect(resumeView).not.toHaveBeenCalled();
    await act(async () => { await useConversationsStore.getState().refreshUnanswered(() => true); });
    expect(screen.getByText('arrived reply')).toBeInTheDocument();
    expect(screen.getByDisplayValue('unsent draft')).toBe(input);
  } finally {
    view.unmount();
    load.mockRestore();
    resumeView.mockRestore();
    useChatStore.getState().reset();
  }
});
