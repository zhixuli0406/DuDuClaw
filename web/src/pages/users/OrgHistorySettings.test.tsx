import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { OrgHistorySettings } from './OrgHistorySettings';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
  mockWsClient.call.mockResolvedValue({ manager_team_history: true, since: '2026-10-23T00:00:00Z' });
});

describe('organisation conversation settings', () => {
  it('reads the real setting and applies only the server-confirmed value', async () => {
    const user = userEvent.setup();
    renderWithProviders(<OrgHistorySettings />);
    const toggle = await screen.findByRole('switch', { name: 'Manager conversation access' });
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
    mockWsClient.call.mockResolvedValue({ manager_team_history: false, since: null });
    await user.click(toggle);
    expect(mockWsClient.call).toHaveBeenCalledWith('users.org_settings.update', { manager_team_history: false });
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'false'));
    expect(screen.getByText(/Only conversations after activation are visible/)).toBeInTheDocument();
  });

  it('keeps the previous confirmed setting and reports a refused update', async () => {
    const user = userEvent.setup();
    renderWithProviders(<OrgHistorySettings />);
    const toggle = await screen.findByRole('switch');
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
    mockWsClient.call.mockRejectedValue(new Error('audit unavailable'));
    await user.click(toggle);
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(toggle).toHaveAttribute('aria-checked', 'true');
  });

  it('does not fetch or offer the administrator setting to a manager', () => {
    useAuthStore.setState({ user: { id: 'mgr', role: 'manager' } as never });
    renderWithProviders(<OrgHistorySettings />);
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });
});
