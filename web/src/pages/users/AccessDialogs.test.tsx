import { beforeEach, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { BindAgentDialog } from './AccessDialogs';

beforeEach(() => { vi.clearAllMocks(); });
it('requires an explicit AI employee choice and defaults to operator access', async () => {
  mockWsClient.call.mockResolvedValue({ agents: [{ name: 'alice', display_name: 'Alice' }] });
  renderWithProviders(<BindAgentDialog userId="u" boundAgents={[]} onClose={vi.fn()} onBound={vi.fn()} />);
  expect(await screen.findByText('Choose an AI employee')).toBeInTheDocument();
  const selects = screen.getAllByRole('combobox');
  expect(selects[1]).toHaveTextContent('Operator');
  const submit = screen.getByRole('button', { name: 'Authorize access' });
  expect(submit).toBeDisabled();
  await userEvent.click(selects[0]);
  await userEvent.click(await screen.findByRole('option', { name: 'Alice · alice' }));
  await userEvent.click(submit);
  await waitFor(() => expect(mockWsClient.call.mock.calls.some(([method, args]) => method === 'users.bind_agent' && args.access_level === 'operator' && args.agent_name === 'alice')).toBe(true));
});
