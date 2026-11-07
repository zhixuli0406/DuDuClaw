import '@/test/mocks';
import { mockCallableGrants } from '@/test/callable-grants';
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { callsOf, mockRpc } from '@/pages/knowledge/test-fixtures';
import type { AgentOwnChannels } from '@/lib/api';
import { AgentOwnChannelsSection, guideUrl } from './AgentOwnChannelsSection';

function status(canEdit: boolean, over: Partial<AgentOwnChannels> = {}): AgentOwnChannels {
  return {
    agent_id: 'a1',
    telegram: {
      configured: false, state: 'not_configured', connected: false, bot_username: null, error: null,
      last_event: null, shared_bot_configured: true, can_edit: canEdit,
    },
    googlechat: {
      configured: false, state: 'not_configured', connected: false, project_number: null,
      service_account_email: null, endpoint_path: '/webhook/googlechat/agents/a1', error: null,
      last_event: null, shared_app_configured: false, can_edit: canEdit,
    },
    ...over,
  };
}

beforeEach(() => { vi.clearAllMocks(); mockCallableGrants(['a1']); });

describe('AgentOwnChannelsSection', () => {
  it('lets an admin connect a bot and shows who it is', async () => {
    const user = userEvent.setup();
    let configured = false;
    mockRpc({
      'agents.channels.status': () =>
        configured
          ? status(true, {
              telegram: { ...status(true).telegram, configured: true, state: 'connected', connected: true, bot_username: 'sales_bot' },
            })
          : status(true),
      'agents.channels.telegram.set': () => {
        configured = true;
        return { success: true, bot_username: 'sales_bot', verified: true, hot_started: true, replaces_shared_bot: false };
      },
    });
    renderWithProviders(<AgentOwnChannelsSection agentId="a1" />);
    expect(await screen.findByText(/shared Telegram bot/)).toBeInTheDocument();
    expect(screen.getByText(/\/webhook\/googlechat\/agents\/a1/)).toBeInTheDocument();
    await user.type(screen.getByLabelText('Telegram bot token'), '123456789:AAEabcdefghijklmnopqrstu');
    await user.click(screen.getAllByRole('button', { name: 'Connect' })[0]);
    await waitFor(() =>
      expect(callsOf('agents.channels.telegram.set').at(-1)).toEqual({ agent_id: 'a1', token: '123456789:AAEabcdefghijklmnopqrstu' }),
    );
    expect(await screen.findByText('Connected as @sales_bot')).toBeInTheDocument();
  });

  it.each(['http', 'pubsub'] as const)('saves a per-employee %s app through manual setup', async (connection) => {
    const user = userEvent.setup();
    mockRpc({
      'agents.channels.status': () => status(true),
      'system.config': () => ({ config: '' }),
      'agents.channels.googlechat.set': () => ({ verified: true }),
    });
    renderWithProviders(<AgentOwnChannelsSection agentId="a1" />);
    await user.selectOptions(await screen.findByLabelText('Connection type'), connection);
    if (connection === 'pubsub') await user.type(screen.getByLabelText('Full Pub/Sub subscription name'), 'projects/my-project/subscriptions/chat');
    await user.click(screen.getByText('Manual setup'));
    await user.type(screen.getByLabelText('Google Cloud project number'), '123456789');
    await user.click(screen.getByLabelText('Service-account JSON key'));
    await user.paste('{"type":"service_account"}');
    await user.click(screen.getAllByRole('button', { name: 'Connect' }).at(-1)!);
    await waitFor(() => expect(callsOf('agents.channels.googlechat.set').at(-1)).toEqual({
      agent_id: 'a1', project_number: '123456789', service_account_json: '{"type":"service_account"}',
      connection, pubsub_subscription: connection === 'pubsub' ? 'projects/my-project/subscriptions/chat' : '',
    }));
  });

  it('shows the owner the status but no controls', async () => {
    mockRpc({
      'agents.channels.status': () =>
        status(false, {
          googlechat: { ...status(false).googlechat, configured: true, state: 'error', error: 'token status 401' },
        }),
    });
    renderWithProviders(<AgentOwnChannelsSection agentId="a1" />);
    expect(await screen.findByText(/Connection problem: token status 401/)).toBeInTheDocument();
    expect(screen.queryByLabelText('Telegram bot token')).toBeNull();
    expect(screen.queryByLabelText('Service-account JSON key')).toBeNull();
    expect(screen.getByText(/Only an admin can set up a Google Chat app/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Remove/ })).toBeNull();
  });

  it('is hidden when the viewer may not read it', async () => {
    mockRpc({
      'agents.channels.status': () => {
        throw new Error('permission denied');
      },
    });
    const { container } = renderWithProviders(<AgentOwnChannelsSection agentId="a1" />);
    await waitFor(() => expect(callsOf('agents.channels.status')).toHaveLength(1));
    expect(container.textContent).not.toMatch(/Own channels/);
  });

  it('links the guide in the viewer language', () => {
    expect(guideUrl('zh-TW')).toMatch(/zh-TW\/per-employee-channels\.md$/);
    expect(guideUrl('ja-JP')).toMatch(/ja-JP\/per-employee-channels\.md$/);
    expect(guideUrl('en')).toMatch(/guides\/per-employee-channels\.md$/);
  });
});
