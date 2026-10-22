import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, act, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WebChatPage } from './WebChatPage';
import { useChatStore } from '@/stores/chat-store';
import { useAuthStore } from '@/stores/auth-store';
import { api } from '@/lib/api';
import { useAgentsStore } from '@/stores/agents-store';

beforeEach(() => {
  vi.clearAllMocks();
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
