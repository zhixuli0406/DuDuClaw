import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentModelPicker } from './AgentModelPicker';
import { useChatStore } from '@/stores/chat-store';
import { useUiModeStore } from '@/stores/ui-mode-store';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useUiModeStore.setState({ mode: 'workspace', chosen: true });
  useChatStore.setState({
    agentName: 'Scout',
    agentIcon: '🐾',
    model: 'claude-opus',
  });
});

describe('AgentModelPicker', () => {
  it('shows the active agent and model from session_info', () => {
    renderWithProviders(<AgentModelPicker />);
    expect(screen.getByRole('button', { name: /Scout/ })).toBeInTheDocument();
    expect(screen.getByText(/claude-opus/)).toBeInTheDocument();
  });

  it('opening the menu and choosing Manage switches to dashboard mode', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AgentModelPicker />);
    await user.click(screen.getByRole('button', { name: /Scout/ }));
    await user.click(screen.getByRole('menuitem', { name: /manage agents/i }));
    expect(useUiModeStore.getState().mode).toBe('dashboard');
  });
});
