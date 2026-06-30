import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { PromptBar } from './PromptBar';
import { useChatStore } from '@/stores/chat-store';

const send = vi.fn();

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
    send: send as never,
  });
});

describe('PromptBar', () => {
  it('sends trimmed text on Enter and clears the input', async () => {
    const user = userEvent.setup();
    renderWithProviders(<PromptBar />);
    const box = screen.getByLabelText(/enter a prompt/i);
    await user.type(box, '  hello world  ');
    await user.keyboard('{Enter}');
    expect(send).toHaveBeenCalledTimes(1);
    expect(send).toHaveBeenCalledWith('hello world', []);
    expect((box as HTMLTextAreaElement).value).toBe('');
  });

  it('does not send an empty prompt', async () => {
    const user = userEvent.setup();
    renderWithProviders(<PromptBar />);
    const box = screen.getByLabelText(/enter a prompt/i);
    box.focus();
    await user.keyboard('{Enter}');
    expect(send).not.toHaveBeenCalled();
  });

  it('Shift+Enter inserts a newline instead of sending', async () => {
    const user = userEvent.setup();
    renderWithProviders(<PromptBar />);
    const box = screen.getByLabelText(/enter a prompt/i);
    await user.type(box, 'line1');
    await user.keyboard('{Shift>}{Enter}{/Shift}');
    expect(send).not.toHaveBeenCalled();
  });

  it('disables the composer when disconnected', () => {
    useChatStore.setState({ connectionState: 'disconnected' as never });
    renderWithProviders(<PromptBar />);
    const box = screen.getByLabelText(/enter a prompt/i) as HTMLTextAreaElement;
    expect(box).toBeDisabled();
  });

  it('calls onSent after a successful send', async () => {
    const onSent = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<PromptBar onSent={onSent} />);
    const box = screen.getByLabelText(/enter a prompt/i);
    await user.type(box, 'hi');
    await user.keyboard('{Enter}');
    expect(onSent).toHaveBeenCalledTimes(1);
  });
});
