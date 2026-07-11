import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentModelPicker } from './AgentModelPicker';
import { useChatStore } from '@/stores/chat-store';

// Spy on router navigation — "Manage" now deep-links to /agents (the former
// setMode('dashboard') hop was removed when the shell modes were collapsed).
const mockNavigate = vi.fn();
vi.mock('react-router', async (importOriginal) => {
  const actual = await importOriginal<typeof import('react-router')>();
  return { ...actual, useNavigate: () => mockNavigate };
});

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
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

  it('opening the menu and choosing Manage navigates to the Agents page', async () => {
    const user = userEvent.setup();
    renderWithProviders(<AgentModelPicker />);
    await user.click(screen.getByRole('button', { name: /Scout/ }));
    await user.click(screen.getByRole('menuitem', { name: /manage agents/i }));
    expect(mockNavigate).toHaveBeenCalledWith('/agents');
  });
});
