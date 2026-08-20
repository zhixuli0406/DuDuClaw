import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { AccountTab } from './AccountTab';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useSystemStore } from '@/stores/system-store';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.restoreAllMocks();
  useAuthStore.setState({
    user: { id: '1', email: 'admin@local', display_name: 'Admin', role: 'admin', status: 'active' },
    jwt: 'jwt-123',
  } as never);
  useSystemStore.setState({ status: null });
  useConnectionStore.setState({ mustChangePassword: false } as never);
});

// The three password fields (現在/新/確認) render through `SettingsRow`,
// which pairs its label with the control visually — not via `<label
// htmlFor>` — so there's no accessible name to query by. Positional lookup
// (current → new → confirm, the form's own field order) is the pragmatic
// alternative other pages in this codebase reach for in the same situation.
async function fillAndSubmit(container: HTMLElement) {
  const user = userEvent.setup();
  const [currentInput, newInput, confirmInput] = Array.from(
    container.querySelectorAll<HTMLInputElement>('input[type="password"]'),
  );
  await user.type(currentInput, 'oldpass1');
  await user.type(newInput, 'newpass1');
  await user.type(confirmInput, 'newpass1');
  await user.click(screen.getByRole('button', { name: 'Update password' }));
}

describe('<AccountTab> — WP-0 must-change-password recovery', () => {
  it('shows no banner and does not reconnect when the flag is clear', async () => {
    useConnectionStore.setState({ mustChangePassword: false } as never);
    vi.spyOn(api.users, 'changePassword').mockResolvedValue({ success: true } as never);
    const connectSpy = vi.fn().mockResolvedValue(undefined);
    const disconnectSpy = vi.fn();
    useConnectionStore.setState({ connectWithAuth: connectSpy, disconnect: disconnectSpy } as never);

    const { container } = renderWithProviders(<AccountTab />);

    expect(screen.queryByText('Set your password first')).toBeNull();

    await fillAndSubmit(container);

    await waitFor(() => expect(api.users.changePassword).toHaveBeenCalledWith('oldpass1', 'newpass1'));
    expect(disconnectSpy).not.toHaveBeenCalled();
    expect(connectSpy).not.toHaveBeenCalled();
  });

  it('shows the mustChange banner, and forces a reconnect after a successful change', async () => {
    useConnectionStore.setState({ mustChangePassword: true } as never);
    vi.spyOn(api.users, 'changePassword').mockResolvedValue({ success: true } as never);
    const disconnectSpy = vi.fn();
    // The reconnect resolves and flips the flag back to false server-side —
    // simulate that by clearing it inside the mocked connectWithAuth, same
    // as the real `connect` handshake response would.
    const connectSpy = vi.fn().mockImplementation(async () => {
      useConnectionStore.setState({ mustChangePassword: false } as never);
    });
    useConnectionStore.setState({ connectWithAuth: connectSpy, disconnect: disconnectSpy } as never);

    const { container } = renderWithProviders(<AccountTab />);

    expect(screen.getByText('Set your password first')).toBeInTheDocument();

    await fillAndSubmit(container);

    await waitFor(() => expect(api.users.changePassword).toHaveBeenCalledWith('oldpass1', 'newpass1'));
    await waitFor(() => expect(disconnectSpy).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(connectSpy).toHaveBeenCalledTimes(1));
  });

  it('does not navigate away if the server still reports the flag set after reconnecting', async () => {
    useConnectionStore.setState({ mustChangePassword: true } as never);
    vi.spyOn(api.users, 'changePassword').mockResolvedValue({ success: true } as never);
    const disconnectSpy = vi.fn();
    // Reconnect resolves but the flag stays true (server didn't clear it) —
    // the banner must still be visible, proving no premature navigation.
    const connectSpy = vi.fn().mockResolvedValue(undefined);
    useConnectionStore.setState({ connectWithAuth: connectSpy, disconnect: disconnectSpy } as never);

    const { container } = renderWithProviders(<AccountTab />);
    await fillAndSubmit(container);

    await waitFor(() => expect(connectSpy).toHaveBeenCalledTimes(1));
    expect(screen.getByText('Set your password first')).toBeInTheDocument();
  });
});
