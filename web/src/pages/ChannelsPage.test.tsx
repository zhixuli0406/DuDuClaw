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
