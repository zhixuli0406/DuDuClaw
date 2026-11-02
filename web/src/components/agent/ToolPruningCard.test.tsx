import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';
import en from '@/i18n/en.json';
vi.mock('@/lib/ws-client', () => ({ client: { call: vi.fn(), subscribe: vi.fn() } }));
import { client } from '@/lib/ws-client';
import { ToolPruningCard } from './ToolPruningCard';
const status = { mode: 'on', listed_count: 246, prunable_names: ['example_read'], estimated_tokens: 100, estimate: true };
beforeEach(() => {
  useAuthStore.setState({ user: { id: 'admin', email: 'admin@example.test', display_name: 'Admin', role: 'admin', status: 'active' } });
  useConnectionStore.setState({ state: 'authenticated' });
  vi.mocked(client.call).mockResolvedValue(status);
});
afterEach(() => { cleanup(); vi.mocked(client.call).mockReset(); useAuthStore.setState({ user: null }); });
describe('ToolPruningCard', () => {
  it('restores and reads status back before updating displayed candidates', async () => {
    renderWithProviders(<ToolPruningCard agentId="a1" />);
    const restore = await screen.findByRole('button', { name: en['toolPruning.restore'] });
    expect(client.call).toHaveBeenCalledWith('agents.tool_pruning_status', { agent_id: 'a1' });
    await userEvent.click(screen.getByText(en['toolPruning.expand']));
    expect(screen.getByText('example_read')).toBeInTheDocument();
    vi.mocked(client.call).mockImplementation(async method => method === 'agents.tool_pruning_status' ? { ...status, prunable_names: [], estimated_tokens: 0 } : { success: true });
    await userEvent.click(restore);
    await waitFor(() => expect(screen.queryByText('example_read')).not.toBeInTheDocument());
    expect(client.call).toHaveBeenCalledWith('agents.tool_pruning_restore', { agent_id: 'a1' });
  });
  it('updates only the selected capability and hides restore in observation mode', async () => {
    vi.mocked(client.call).mockResolvedValue({ ...status, mode: 'shadow' });
    renderWithProviders(<ToolPruningCard agentId="a1" />);
    const mode = await screen.findByRole('combobox', { name: en['toolPruning.mode'] });
    expect(screen.queryByRole('button', { name: en['toolPruning.restore'] })).not.toBeInTheDocument();
    await userEvent.selectOptions(mode, 'off');
    expect(client.call).toHaveBeenCalledWith('agents.update', { agent_id: 'a1', capabilities: { tool_pruning: 'off' } });
  });
  it('keeps failed status requests visible and allows retry', async () => {
    vi.mocked(client.call).mockRejectedValue(new Error('not available'));
    renderWithProviders(<ToolPruningCard agentId="a1" />);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.queryByRole('combobox')).not.toBeInTheDocument();
    vi.mocked(client.call).mockResolvedValue(status);
    await userEvent.click(screen.getByRole('button', { name: en['toolPruning.retry'] }));
    expect(await screen.findByRole('combobox')).toBeInTheDocument();
  });
  it('does not request Admin status for a manager', () => {
    useAuthStore.setState({ user: { id: 'manager', email: 'm@example.test', display_name: 'Manager', role: 'manager', status: 'active' } });
    renderWithProviders(<ToolPruningCard agentId="a1" />);
    expect(client.call).not.toHaveBeenCalled();
    expect(screen.queryByText(en['toolPruning.title'])).not.toBeInTheDocument();
  });
});
