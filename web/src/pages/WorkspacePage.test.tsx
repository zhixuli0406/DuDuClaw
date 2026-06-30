import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WorkspacePage } from './WorkspacePage';
import { useChatStore } from '@/stores/chat-store';

beforeEach(() => {
  vi.clearAllMocks();
  useChatStore.setState({
    messages: [],
    isStreaming: false,
    sessionId: null,
    connectionState: 'connected' as never,
    agentName: 'DuDuClaw',
    agentIcon: '🐾',
    model: 'claude',
    supportsVision: false,
  });
});

describe('WorkspacePage', () => {
  it('idle: shows hero, prompt, Claw value props and launcher grid', () => {
    renderWithProviders(<WorkspacePage />);
    expect(screen.getByRole('heading', { name: /DuDuClaw Workspace/i })).toBeInTheDocument();
    expect(screen.getByLabelText(/enter a prompt/i)).toBeInTheDocument();
    expect(screen.getByRole('heading', { name: /your first AI employee/i })).toBeInTheDocument();
    // A non-gated launcher card is always visible.
    expect(screen.getByText('All Agents')).toBeInTheDocument();
  });

  it('conversing: shows the reply stream and a new-chat action', () => {
    useChatStore.setState({
      messages: [
        { id: '1', role: 'user', content: 'ping', timestamp: Date.now() },
        { id: '2', role: 'assistant', content: 'pong', timestamp: Date.now() },
      ],
    });
    renderWithProviders(<WorkspacePage />);
    expect(screen.getByText('ping')).toBeInTheDocument();
    expect(screen.getByText('pong')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /new chat/i })).toBeInTheDocument();
    // Launcher grid is hidden while conversing.
    expect(screen.queryByText('All Agents')).not.toBeInTheDocument();
  });
});
