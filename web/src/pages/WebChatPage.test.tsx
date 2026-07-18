import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, fireEvent } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WebChatPage } from './WebChatPage';
import { useChatStore } from '@/stores/chat-store';

beforeEach(() => {
  vi.clearAllMocks();
  useChatStore.setState({
    messages: [],
    isStreaming: false,
    sessionId: null,
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
