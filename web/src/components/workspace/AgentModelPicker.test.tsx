import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentModelPicker } from './AgentModelPicker';
import { useAgentsStore } from '@/stores/agents-store';

const ROSTER = [
  { name: 'scout', display_name: 'Scout', icon: '🐾', role: 'main', model: { preferred: 'claude-opus' } },
  { name: 'nova', display_name: 'Nova', icon: '🛰', role: 'worker', model: { preferred: 'gemini-pro' } },
];

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useAgentsStore.setState({ agents: ROSTER as never, loaded: true });
});

describe('AgentModelPicker', () => {
  it('shows the selected AI employee', () => {
    renderWithProviders(<AgentModelPicker value="scout" onChange={vi.fn()} />);
    expect(screen.getByRole('button', { name: /Scout/ })).toBeInTheDocument();
  });

  it('lists the roster with each employee’s own model and reports the pick', async () => {
    const onChange = vi.fn();
    const user = userEvent.setup();
    renderWithProviders(<AgentModelPicker value="scout" onChange={onChange} />);
    await user.click(screen.getByRole('button', { name: /Scout/ }));
    // No hardcoded model list: each row shows whatever that agent's own config says.
    expect(screen.getByText('gemini-pro')).toBeInTheDocument();
    await user.click(screen.getByRole('menuitemradio', { name: /Nova/ }));
    expect(onChange).toHaveBeenCalledWith('nova');
  });
});
