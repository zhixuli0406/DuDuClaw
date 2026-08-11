import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ChannelsPage } from './ChannelsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
});

describe('ChannelsPage', () => {
  it('renders channel list heading', () => {
    mockWsClient.call.mockResolvedValue({ channels: [] });
    renderWithProviders(<ChannelsPage />);

    expect(screen.getByText('Channel Management')).toBeInTheDocument();
  });

  it('shows empty state when no channels', async () => {
    mockWsClient.call.mockResolvedValue({ channels: [] });
    renderWithProviders(<ChannelsPage />);

    expect(
      await screen.findByText('No channels configured yet')
    ).toBeInTheDocument();
  });

  it('shows channels returned by API', async () => {
    mockWsClient.call.mockResolvedValue({
      channels: [
        { name: 'telegram', connected: true, last_connected: null, error: null },
        { name: 'line', connected: false, last_connected: null, error: 'Token expired' },
      ],
    });

    renderWithProviders(<ChannelsPage />);

    expect(await screen.findByText('telegram')).toBeInTheDocument();
    expect(await screen.findByText('line')).toBeInTheDocument();
  });

  it('opens add channel dialog when button clicked', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockResolvedValue({ channels: [] });

    renderWithProviders(<ChannelsPage />);

    const addButton = screen.getByRole('button', { name: /add channel/i });
    await user.click(addButton);

    // The dialog should open — look for form elements
    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });
});

// W2-2 (E1/E2) — 行為 / 存取 detail dialog.
describe('ChannelsPage — behavior & access detail dialog', () => {
  const configSettings = {
    mention_only: false,
    auto_thread: false,
    allowed_channels: [],
    allowed_guilds: [],
    agent_override: '',
    response_mode: 'auto' as const,
    thread_archive_minutes: null,
  };
  const accessSettings = {
    require_pairing: false,
    allowed_users: [],
    blocked_users: [],
    admin_users: [],
  };

  function mockRpcs() {
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'channels.status') {
        return Promise.resolve({
          channels: [{ name: 'discord', connected: true, last_connected: null, error: null }],
        });
      }
      if (method === 'channels.config_get') {
        return Promise.resolve({
          success: true,
          channel: 'discord',
          scope_id: 'global',
          settings: configSettings,
          scopes: [],
        });
      }
      if (method === 'channels.access_get') {
        return Promise.resolve({ success: true, channel: 'discord', settings: accessSettings });
      }
      if (method === 'channels.pairing_list') {
        return Promise.resolve({ success: true, approved: ['u-alice'] });
      }
      return Promise.resolve(null);
    });
  }

  async function openDetailDialog(user: ReturnType<typeof userEvent.setup>) {
    renderWithProviders(<ChannelsPage />);
    await screen.findByText('discord');
    const kebab = screen.getByRole('button', { name: /more/i });
    await user.click(kebab);
    const detailItem = await screen.findByText('Behavior & Access');
    await user.click(detailItem);
  }

  it('loads behavior settings and shows both tabs', async () => {
    const user = userEvent.setup();
    mockRpcs();

    await openDetailDialog(user);

    expect(await screen.findByRole('tab', { name: /behavior/i })).toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /access/i })).toBeInTheDocument();
    expect(await screen.findByText('Reply only when mentioned')).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith(
      'channels.config_get',
      expect.objectContaining({ channel: 'discord' }),
    );
  });

  it('saves behavior settings via channels.config_set', async () => {
    const user = userEvent.setup();
    mockRpcs();

    await openDetailDialog(user);
    await screen.findByText('Reply only when mentioned');

    // The behavior tab's first switch is always "mention_only" (row order is
    // fixed); SettingsRow doesn't wire an accessible name onto the control.
    const mentionSwitch = screen.getAllByRole('switch')[0];
    await user.click(mentionSwitch);
    const saveButtons = screen.getAllByRole('button', { name: 'Save' });
    await user.click(saveButtons[saveButtons.length - 1]);

    expect(mockWsClient.call).toHaveBeenCalledWith(
      'channels.config_set',
      expect.objectContaining({
        channel: 'discord',
        settings: expect.objectContaining({ mention_only: true }),
      }),
    );
  });

  it('switches to the access tab, lists paired users, and revokes one', async () => {
    const user = userEvent.setup();
    mockRpcs();

    await openDetailDialog(user);
    await screen.findByText('Reply only when mentioned');

    await user.click(screen.getByRole('tab', { name: /access/i }));
    expect(await screen.findByText('Require pairing before first use')).toBeInTheDocument();
    expect(await screen.findByText('u-alice')).toBeInTheDocument();

    const revokeButton = screen.getByRole('button', { name: /revoke/i });
    await user.click(revokeButton);

    expect(mockWsClient.call).toHaveBeenCalledWith(
      'channels.pairing_revoke',
      expect.objectContaining({ subject: 'u-alice' }),
    );
  });
});
