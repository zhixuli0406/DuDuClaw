import { beforeEach, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { BindAgentDialog, BindChannelIdentityDialog } from './AccessDialogs';

beforeEach(() => { vi.clearAllMocks(); });
it('offers Google Chat as a bindable channel, explains it, and sends it to the bind endpoint', async () => {
  const fetchMock = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ success: true }) });
  vi.stubGlobal('fetch', fetchMock);
  const onBound = vi.fn();
  try {
    renderWithProviders(<BindChannelIdentityDialog userId="u1" onClose={vi.fn()} onBound={onBound} />);
    await userEvent.click(screen.getByRole('combobox'));
    await userEvent.click(await screen.findByRole('option', { name: 'Google Chat' }));
    expect(screen.getByText(/it cannot receive login codes/)).toBeInTheDocument();
    expect(screen.getByText(/has not yet been verified with a real Google Workspace/)).toBeInTheDocument();
    await userEvent.type(screen.getByPlaceholderText('Paste their user ID on that channel'), 'users/123456');
    await userEvent.click(screen.getByRole('button', { name: 'Verify identity' }));
    await waitFor(() => expect(onBound).toHaveBeenCalled());
    const [url, init] = fetchMock.mock.calls[0];
    expect(url).toBe('/api/channel-identity/bind');
    expect(JSON.parse(init.body)).toEqual({ user_id: 'u1', channel: 'googlechat', channel_user_id: 'users/123456' });
  } finally {
    vi.unstubAllGlobals();
  }
});

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
