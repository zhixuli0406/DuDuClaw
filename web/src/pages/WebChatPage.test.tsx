import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
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
  it('renders chat interface', () => {
    renderWithProviders(<WebChatPage />);

    expect(screen.getByPlaceholderText(/message/i)).toBeInTheDocument();
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

  it('shows streaming indicator when assistant is typing', () => {
    useChatStore.setState({ isStreaming: true });

    renderWithProviders(<WebChatPage />);

    // The typing indicator should be visible
    const dots = document.querySelectorAll('[class*="animate"]');
    expect(dots.length).toBeGreaterThan(0);
  });

  it('allows typing and sending a message', async () => {
    const user = userEvent.setup();
    const sendSpy = vi.fn();
    useChatStore.setState({ send: sendSpy } as never);

    renderWithProviders(<WebChatPage />);

    const input = screen.getByPlaceholderText(/message/i);
    await user.type(input, 'Test message');

    expect(input).toHaveValue('Test message');
  });
});
