import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent, act, waitFor } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WebChatPage } from './WebChatPage';
import { useChatStore } from '@/stores/chat-store';
import { api } from '@/lib/api';

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

  it('allows typing a message into the composer', () => {
    renderWithProviders(<WebChatPage />);

    // NOTE: the split panes have no measured size under jsdom's mocked
    // ResizeObserver, so userEvent's visibility check skips typing — drive the
    // controlled textarea directly instead.
    const input = screen.getByPlaceholderText(/message/i) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: 'Test message' } });

    expect(input).toHaveValue('Test message');
  });
});
