import '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { callsOf, mockRpc } from '@/pages/knowledge/test-fixtures';
import { useProductPolicyStore } from '@/lib/product-policy';
import { AddChannelDialog } from './AddChannelDialog';

beforeEach(() => {
  vi.clearAllMocks();
  useProductPolicyStore.setState({ policy: { profile: 'standard', blocked_channels: [], blocked_runtimes: [], blocked_providers: [] } });
});

describe('shared Google Chat setup', () => {
  it.each(['http', 'pubsub'] as const)('keeps manual fields available and sends %s settings to channels.add', async (connection) => {
    const user = userEvent.setup();
    mockRpc({
      'agents.list': () => ({ agents: [] }),
      'system.config': () => ({ config: '' }),
      'channels.add': () => ({ success: true, hot_started: true }),
    });
    renderWithProviders(<AddChannelDialog open fixedType="googlechat" onClose={vi.fn()} onCreated={vi.fn()} />);
    await user.selectOptions(screen.getByLabelText('Connection type'), connection);
    if (connection === 'pubsub') await user.type(screen.getByLabelText('Full Pub/Sub subscription name'), 'projects/my-project/subscriptions/chat');
    await user.click(screen.getByRole('button', { name: 'Manual setup' }));
    await user.click(screen.getByPlaceholderText(/service account json/i));
    await user.paste('{"type":"service_account"}');
    await user.type(screen.getByPlaceholderText('Project Number'), '123456789');
    await user.click(screen.getByRole('button', { name: 'Add Channel' }));
    await waitFor(() => expect(callsOf('channels.add').at(-1)).toEqual({
      type: 'googlechat', config: { token: '{"type":"service_account"}', secret: '123456789', connection, pubsub_subscription: connection === 'pubsub' ? 'projects/my-project/subscriptions/chat' : '' },
    }));
  });
});

it('closes a locked employee dialog on revocation and never falls back to a global channel', async () => {
  mockCallableGrants(['alice']);
  mockRpc({ 'agents.list': () => ({ agents: [] }), 'system.config': () => ({ config: '' }) });
  const close = vi.fn();
  renderWithProviders(<AddChannelDialog open fixedType="telegram" lockedAgent="alice" onClose={close} onCreated={vi.fn()} />);
  await userEvent.click(screen.getByPlaceholderText(/bot token/i));
  await userEvent.paste('bot-secret');
  const submit = screen.getByRole('button', { name: 'Add Channel' });
  await act(async () => { mockCallableGrants([]); submit.click(); });
  expect(close).toHaveBeenCalled();
  expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  expect(callsOf('channels.add')).toEqual([]);
});
