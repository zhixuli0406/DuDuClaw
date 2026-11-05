import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { AutomationReminders } from './AutomationReminders';

beforeEach(() => vi.clearAllMocks());
describe('AutomationReminders', () => {
  it('keeps Viewer reminders read-only', async () => {
    mockWsClient.call.mockResolvedValue({ reminders: [{ id: 'r', agent_id: 'sales', message: 'Read only reminder',
      trigger_at: '2099-01-01T09:00:00Z', status: 'pending', can_manage: false }] });
    renderWithProviders(<AutomationReminders enabled />);
    await screen.findByText('Read only reminder');
    expect(screen.queryByRole('button', { name: 'Cancel' })).not.toBeInTheDocument();
  });
  it('lets users retry a failed read without displaying an empty success', async () => {
    mockWsClient.call.mockRejectedValueOnce(new Error('unavailable')).mockResolvedValueOnce({ reminders: [] });
    renderWithProviders(<AutomationReminders enabled />);
    const retry = await screen.findByRole('button', { name: 'Refresh' });
    fireEvent.click(retry);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Refresh' })).not.toBeInTheDocument());
  });
});
