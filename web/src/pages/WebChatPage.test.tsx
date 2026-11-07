import { useConnectionStore } from '@/stores/connection-store';
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
import { useSystemStore } from '@/stores/system-store';
import { useBrandStore } from '@/stores/brand-store';
import { stashChatAttachments } from '@/lib/chat-handoff';

beforeEach(() => {
  useConnectionStore.setState({ state: 'authenticated' });
  vi.clearAllMocks();
  useBrandStore.getState().setBrand(null);
  useAuthStore.setState({ user: { id: 'admin', display_name: 'Boss', role: 'admin' } as never, bindings: [] });
  useSystemStore.setState({ status: null });
  vi.spyOn(api.agents, 'callable').mockImplementation(async () => ({
    agents: useAgentsStore.getState().agents.map(a => ({ ...a, via: 'admin', selectable: true, assignable: true, call_text: `@${a.display_name}` })),
    unrestricted: true, default_agent: null,
  }) as never);
  useConversationsStore.setState({ viewOnly: false });
  useChatStore.setState({
    messages: [],
    isStreaming: false,
    sessionId: null,
    sessionsRevision: 0,
    connectionState: 'connected' as never,
    reconnectExhausted: false,
    agentName: 'DuDuClaw',
    agentIcon: '',
    resolvedAgent: undefined,
    assignmentError: null,
    resumedExplicitly: false,
  });
});

describe('WebChatPage', () => {
  it('offers recent skills to an employee on a new chat without sending on selection', async () => {
    useAuthStore.setState({ user: { id: 'employee-a', role: 'employee' } as never });
    // Changing accounts disconnects chat; seed the completed employee handshake.
    useChatStore.setState({ connectionState: 'connected', selectedAgentId: 'sales', resolvedAgent: { id: 'sales', name: 'Sales', icon: '' } });
    const send = vi.spyOn(useChatStore.getState(), 'send').mockReturnValue(true);
    const recent = vi.spyOn(api.skills, 'recent').mockResolvedValue({ skills: [
      { name: 'quote', display_name: 'Quote', description: 'Prepare a quote' },
      { name: 'pdf', display_name: 'PDF', description: 'Read a PDF' },
    ] });
    renderWithProviders(<WebChatPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Quote Prepare a quote' }));
    expect(recent).toHaveBeenCalledWith('sales', false);
    fireEvent.click(screen.getByRole('button', { name: 'PDF Read a PDF' }));
    expect(screen.getByRole('combobox')).toHaveValue('/quote /pdf ');
    expect(send).not.toHaveBeenCalled();
    recent.mockRestore();
    send.mockRestore();
  });

  it('uses the verified product name for the default partner header and heading', async () => {
    useBrandStore.getState().setBrand({ product_name: '晨光智能', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    useChatStore.setState({ selectedAgentId: null, agentName: 'DuDuClaw' });
    renderWithProviders(<WebChatPage />);
    expect(await screen.findByRole('heading', { name: '晨光智能' })).toBeInTheDocument();
    expect(screen.getAllByText('晨光智能')).toHaveLength(2);
    expect(screen.queryByText('DuDuClaw')).not.toBeInTheDocument();
  });

  it('shows oversight notice only for the selected server-confirmed staff and removes it after refresh', async () => {
    useChatStore.setState({ selectedAgentId: 'sales', sessionId: null });
    useAgentsStore.setState({ loaded: true, agents: [{ name: 'sales', display_name: 'Sales', role: 'specialist', status: 'active' }] as never });
    const sales = { name: 'sales', display_name: 'Sales', call_text: '@Sales', role: 'specialist', status: 'active', via: 'binding' as const, level: 'operator' as const, assignable: true, selectable: true, manager_may_read: true };
    const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [sales], unrestricted: true, default_agent: null });
    renderWithProviders(<WebChatPage />);
    const notice = await screen.findByRole('link', { name: 'Your manager can read your conversations with this AI staff member (read-only).' });
    expect(notice).toHaveAttribute('href', expect.stringContaining('68-manager-team-history.md'));
    callable.mockResolvedValue({ agents: [{ ...sales, manager_may_read: false }], unrestricted: true, default_agent: null });
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

  it('shows the employee working status while the assistant is streaming', () => {
    renderWithProviders(<WebChatPage />);
    act(() => useChatStore.setState({
      connectionState: 'connected' as never,
      reconnectExhausted: false,
      isStreaming: true,
      activeMessageId: 'turn-1',
      messages: [{ id: 'user-1', role: 'user', content: 'Hello', timestamp: Date.now(), turnId: 'turn-1', delivery: 'running' }],
    }));

    expect(screen.getByRole('status')).toHaveTextContent(/Thinking/);
    expect(screen.getByRole('button', { name: /stop/i })).toBeInTheDocument();
  });

  it('does not send on Enter while a CJK IME is composing, but sends on a plain Enter', async () => {
    const send = vi.fn();
    useChatStore.setState({ send: send as never, isStreaming: false, connectionState: 'connected' as never });

    renderWithProviders(<WebChatPage />);
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    await waitFor(() => expect(input).toBeEnabled());
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

  it('typing @ lists the callable AI staff and Enter inserts the picked name', async () => {
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
    const list = await screen.findByRole('listbox');
    expect(list).toBeInTheDocument();
    expect(screen.getByRole('combobox')).toHaveAttribute('aria-expanded', 'true');
    const options = await screen.findAllByRole('option');
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
    expect(await screen.findAllByRole('option')).toHaveLength(2);
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(screen.queryByRole('listbox')).toBeNull();
  });

  it('lists what agents.callable returns, grouped mine / team / public, and inserts the server call text', async () => {
    const callable = vi.spyOn(api.agents, 'callable').mockResolvedValue({
      unrestricted: true, default_agent: 'dudu',
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
      unrestricted: true, default_agent: 'dudu',
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
    expect(screen.getByRole('link', { name: 'View progress' })).toHaveAttribute('href', '/tasks/1a2b3c4d-0000-5000-8000-000000000000');
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


it('keeps the composer and Send enabled during streaming, with a separate Stop control', async () => {
  const send = vi.fn();
  const cancel = vi.fn();
  useChatStore.setState({ send, cancel, isStreaming: true, activeMessageId: 'running', connectionState: 'connected' });
  renderWithProviders(<WebChatPage />);
  const input = screen.getByPlaceholderText(/message/i);
  await waitFor(() => expect(input).toBeEnabled());
  fireEvent.change(input, { target: { value: 'follow-up' } });
  expect(screen.getByRole('button', { name: 'Send' })).toBeEnabled();
  fireEvent.click(screen.getByRole('button', { name: 'Stop' }));
  expect(cancel).toHaveBeenCalledOnce();
  fireEvent.click(screen.getByRole('button', { name: 'Send' }));
  expect(send).toHaveBeenCalledWith('follow-up', []);
  expect(input).toHaveValue('');
  fireEvent.change(input, { target: { value: 'another' } });
  fireEvent.keyDown(input, { key: 'Enter' });
  expect(send).toHaveBeenLastCalledWith('another', []);
});

it('shows queued cancellation and restores rejected text to the composer', () => {
  const cancel = vi.fn();
  useChatStore.setState({ cancel, isStreaming: true, activeMessageId: 'running', messages: [
    { id: 'one', turnId: 'running', role: 'user', content: 'first', timestamp: 1, delivery: 'running' },
    { id: 'two', turnId: 'queued', role: 'user', content: 'second', timestamp: 2, delivery: 'queued' },
    { id: 'three', turnId: 'rejected', role: 'user', content: 'keep this', timestamp: 3, delivery: 'failed', deliveryError: en['webchat.turnLimit'] },
  ] });
  renderWithProviders(<WebChatPage />);
  expect(screen.getByText(en['webchat.queued'])).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: en['webchat.cancelQueued'] }));
  expect(cancel).toHaveBeenCalledWith('queued');
  expect(screen.getAllByText(en['webchat.turnLimit'])).toHaveLength(1);
  fireEvent.click(screen.getByRole('button', { name: en['webchat.resend'] }));
  expect(screen.getByPlaceholderText(/message/i)).toHaveValue('keep this');
});


describe('server-resolved non-admin chat', () => {
  const sales = { name: 'sales', display_name: 'Sales', role: 'specialist', status: 'active', call_text: '@Sales', via: 'binding', level: 'operator', selectable: true, assignable: true };
  function assigned(role: 'employee' | 'manager' = 'employee') {
    useAuthStore.setState({ user: { id: 'viewer', display_name: 'Viewer', role } as never });
    useAgentsStore.setState({ loaded: true, agents: [{ name: 'main', display_name: 'Main', role: 'main' }, sales] as never });
    useChatStore.setState({ selectedAgentId: 'main', resolvedAgent: { id: 'sales', name: 'Sales', icon: '🐦' }, connectionState: 'connected' });
    vi.spyOn(api.agents, 'callable').mockResolvedValue({ agents: [sales], unrestricted: false, default_agent: null } as never);
  }
  it.each(['employee', 'manager'] as const)('hides all partner chips for %s and scopes history to the server assignment', async role => {
    assigned(role);
    const list = vi.spyOn(api.chatSessions, 'list').mockResolvedValue({ sessions: [] });
    renderWithProviders(<WebChatPage />);
    await waitFor(() => expect(list).toHaveBeenCalledWith({ agent_id: 'sales', limit: 50 }));
    expect(screen.queryByRole('radio')).not.toBeInTheDocument();
    expect(screen.getByRole('heading', { name: 'Sales' })).toBeInTheDocument();
  });
  it('preserves the partner chips for admins', async () => {
    assigned();
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
    vi.mocked(api.agents.callable).mockResolvedValue({ agents: [sales as never], unrestricted: true, default_agent: null });
    renderWithProviders(<WebChatPage />);
    expect(await screen.findByRole('radio', { name: /Sales/ })).toBeInTheDocument();
  });
  it('does not grant Personal edition members unrestricted partner chips', async () => {
    assigned();
    useSystemStore.setState({ status: { edition_profile: 'personal' } as never });
    renderWithProviders(<WebChatPage />);
    await act(async () => {});
    expect(screen.queryByRole('radio')).not.toBeInTheDocument();
  });
  it('shows an actionable unassigned state and disables the composer', () => {
    assigned();
    useChatStore.setState({ resolvedAgent: null, assignmentError: { code: 'no_assigned_agent', message: '', reason: 'no_agents' } });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByText(en['webchat.assignment.none'])).toBeInTheDocument();
    expect(screen.getByPlaceholderText(/message/i)).toBeDisabled();
    expect(screen.queryByText('no_assigned_agent')).not.toBeInTheDocument();
  });
  it('explains how to choose a primary staff member when several are available', () => {
    assigned();
    useChatStore.setState({ resolvedAgent: null, assignmentError: { code: 'no_assigned_agent', message: '', reason: 'multiple_agents' } });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByText(en['webchat.assignment.multiple'])).toBeInTheDocument();
  });
  it('distinguishes an unavailable assignment lookup from having no staff', () => {
    assigned();
    useChatStore.setState({ resolvedAgent: null, assignmentError: { code: 'no_assigned_agent', message: '', reason: 'unavailable' } });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByText(en['webchat.assignment.unavailable'])).toBeInTheDocument();
    expect(screen.queryByText(en['webchat.assignment.none'])).not.toBeInTheDocument();
  });
  it('hides an unavailable historical main selection', () => {
    assigned();
    useChatStore.setState({ resumedExplicitly: true, selectedAgentId: 'main', agentName: 'Main' });
    renderWithProviders(<WebChatPage />);
    expect(screen.queryByRole('heading', { name: 'Main' })).not.toBeInTheDocument();
    expect(screen.queryByRole('heading', { name: 'Sales' })).not.toBeInTheDocument();
  });
  it('ignores a main-agent URL preselection', () => {
    assigned();
    const select = vi.spyOn(useChatStore.getState(), 'selectAgent');
    render(<IntlProvider messages={en} locale="en"><MemoryRouter initialEntries={['/chat?agent=main']}><WebChatPage /></MemoryRouter></IntlProvider>);
    expect(select).not.toHaveBeenCalled();
    expect(screen.getByRole('heading', { name: 'Sales' })).toBeInTheDocument();
  });
  it('explains why a historical main-agent conversation cannot continue', () => {
    assigned();
    useChatStore.setState({ resumedExplicitly: true, messages: [{ id: 'old', role: 'user', content: 'Earlier question', timestamp: 0 }], assignmentError: { code: 'main_agent_admin_only', message: '' } });
    renderWithProviders(<WebChatPage />);
    expect(screen.getByRole('alert')).toHaveTextContent(en['webchat.assignment.mainAdminOnly']);
    expect(screen.getByPlaceholderText(/message/i)).toBeDisabled();
  });
});

describe('WebChatPage primary preference', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { id: 'admin', role: 'admin', primary_agent_id: 'nova' } as never });
    useAgentsStore.setState({ loaded: true, agents: [
      { name: 'main', role: 'main', display_name: 'Main' },
      { name: 'nova', role: 'specialist', display_name: 'Nova' },
    ] as never });
    useChatStore.setState({ selectedAgentId: null, messages: [], resumedExplicitly: false });
  });
  it('uses the visible primary when opening a fresh admin conversation', async () => {
    renderWithProviders(<WebChatPage />);
    await waitFor(() => expect(useChatStore.getState().selectedAgentId).toBe('nova'));
  });
  it('does not replace the partner of a historical conversation', () => {
    useChatStore.setState({ selectedAgentId: 'history-staff', resumedExplicitly: true });
    renderWithProviders(<WebChatPage />);
    expect(useChatStore.getState().selectedAgentId).toBe('history-staff');
  });
  it('does not replace an explicit partner with the account default', async () => {
    render(<IntlProvider messages={en} locale="en"><MemoryRouter initialEntries={['/chat?agent=other']}><WebChatPage /></MemoryRouter></IntlProvider>);
    await waitFor(() => expect(useChatStore.getState().selectedAgentId).toBe('other'));
  });
  it('restores the primary when starting another new conversation', () => {
    useChatStore.setState({ selectedAgentId: 'other', resumedExplicitly: true, messages: [{ id: 'past', role: 'user', content: 'Old question', timestamp: 0 }] });
    act(() => useConversationsStore.getState().startNew());
    expect(useChatStore.getState().selectedAgentId).toBe('nova');
    expect(useChatStore.getState().messages).toEqual([]);
    expect(useChatStore.getState().resumedExplicitly).toBe(false);
  });
  it('leaves enterprise member assignment to the gateway', () => {
    useAuthStore.setState({ user: { id: 'employee', role: 'employee', primary_agent_id: 'nova' } as never });
    useSystemStore.setState({ status: { edition_profile: 'enterprise' } as never });
    useChatStore.setState({ resolvedAgent: { id: 'assigned', name: 'Assigned', icon: '' }, assignmentError: null });
    const select = vi.spyOn(useChatStore.getState(), 'selectAgent');
    renderWithProviders(<WebChatPage />);
    expect(select).not.toHaveBeenCalled();
  });
});

it('drops a revoked prefilled mention before sending the handoff draft', async () => {
  useAuthStore.setState({ user: { id: 'employee', role: 'employee' } as never });
  useAgentsStore.setState({ loaded: true, agents: [{ name: 'sales', display_name: 'Sales', status: 'active' }] as never });
  useChatStore.setState({ connectionState: 'connected', selectedAgentId: null, resolvedAgent: { id: 'other', name: 'Other', icon: '' }, assignmentError: null });
  let resolve!: (value: unknown) => void;
  vi.spyOn(api.agents, 'callable').mockImplementation(() => new Promise(r => { resolve = r; }) as never);
  const send = vi.spyOn(useChatStore.getState(), 'send').mockImplementation(() => true);
  render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[{ pathname: '/chat', state: { draft: '@Sales prepare a quote' } }]}>
        <WebChatPage />
      </MemoryRouter>
    </IntlProvider>,
  );
  expect(screen.getByRole('combobox')).not.toHaveValue('@Sales prepare a quote');
  await act(async () => resolve({ agents: [], unrestricted: false, default_agent: null }));
  await waitFor(() => expect(screen.getByRole('combobox')).toHaveValue(' prepare a quote'));
  expect(send).not.toHaveBeenCalled();
  send.mockRestore();
});

it('shows the initial connecting status without a reconnect banner', () => {
  useChatStore.setState({ connectionState: 'connecting' });
  renderWithProviders(<WebChatPage />);
  expect(screen.getByText('Connecting…')).toBeVisible();
  expect(screen.queryByText(/Your draft is kept/)).not.toBeInTheDocument();
  expect(screen.queryByRole('button', { name: 'Reconnect' })).not.toBeInTheDocument();
});

it.each([
  ['reconnecting', 'Reconnecting…'],
  ['disconnected', 'Disconnected'],
] as const)('shows %s beside the composer and keeps the unsent draft', (state, label) => {
  renderWithProviders(<WebChatPage />);
  const input = screen.getByRole('combobox');
  fireEvent.change(input, { target: { value: 'unsent draft' } });
  act(() => useChatStore.setState({ connectionState: state }));
  expect(screen.getByText(`${label} Your draft is kept. You can send it when the connection returns.`)).toBeVisible();
  expect(screen.getByRole('button', { name: 'Reconnect' })).toBeVisible();
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  expect(input).toHaveValue('unsent draft');
});

it('offers manual reconnect when automatic retries stop', () => {
  const reconnect = vi.spyOn(useChatStore.getState(), 'reconnect').mockImplementation(() => {});
  renderWithProviders(<WebChatPage />);
  act(() => useChatStore.setState({ connectionState: 'disconnected', reconnectExhausted: true }));
  expect(screen.getByText(/Automatic retries have stopped/)).toBeVisible();
  fireEvent.click(screen.getByRole('button', { name: 'Reconnect' }));
  expect(reconnect).toHaveBeenCalledTimes(1);
  reconnect.mockRestore();
});

it('retains handed-over attachments when send reports a transport failure', async () => {
  const send = vi.spyOn(useChatStore.getState(), 'send').mockReturnValue(false);
  const attachmentsKey = stashChatAttachments([{ name: 'brief.txt', mime: 'text/plain', dataBase64: 'aGk=' }]);
  render(
    <IntlProvider messages={en} locale="en" defaultLocale="en">
      <MemoryRouter initialEntries={[{ pathname: '/chat', state: { draft: 'keep this', attachmentsKey } }]}>
        <WebChatPage />
      </MemoryRouter>
    </IntlProvider>,
  );
  const input = await screen.findByDisplayValue('keep this');
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  expect(send).toHaveBeenCalledWith('keep this', [{ name: 'brief.txt', mime: 'text/plain', dataBase64: 'aGk=' }]);
  expect(input).toHaveValue('keep this');
  expect(screen.getByText('brief.txt')).toBeVisible();
  send.mockRestore();
});
