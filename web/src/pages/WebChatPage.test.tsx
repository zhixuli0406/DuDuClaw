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
