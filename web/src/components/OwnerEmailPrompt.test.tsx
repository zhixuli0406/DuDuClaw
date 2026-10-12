import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { OwnerEmailPrompt } from './OwnerEmailPrompt';
import { api } from '@/lib/api';
import { useAuthStore } from '@/stores/auth-store';
import { useConnectionStore } from '@/stores/connection-store';

function mockMe(placeholder: boolean, passwordRequired = false) {
  vi.spyOn(api.users, 'me').mockResolvedValue({
    user: {},
    bindings: [],
    email_setup: { placeholder, password_required: passwordRequired },
  } as never);
}

beforeEach(() => {
  vi.restoreAllMocks();
  localStorage.clear();
  useConnectionStore.setState({
    state: 'authenticated',
    connectWithAuth: vi.fn().mockResolvedValue(undefined),
    disconnect: vi.fn(),
  } as never);
  useAuthStore.setState({ refresh: vi.fn().mockResolvedValue(undefined) } as never);
});

describe('<OwnerEmailPrompt>', () => {
  it('asks the placeholder owner for an email and saves it', async () => {
    const user = userEvent.setup();
    mockMe(true);
    vi.spyOn(api.users, 'changeEmail').mockResolvedValue({ status: 'changed', email: 'owner@example.com' });

    renderWithProviders(<OwnerEmailPrompt />);
    expect(await screen.findByText('Set your email')).toBeInTheDocument();
    expect(screen.queryByLabelText('Current password')).toBeNull();

    await user.type(screen.getByLabelText('Your email'), 'owner@example.com');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(api.users.changeEmail).toHaveBeenCalledWith('owner@example.com', undefined));
    await waitFor(() => expect(screen.queryByText('Set your email')).toBeNull());
  });

  it('stays hidden once the owner has a real email', async () => {
    mockMe(false);
    renderWithProviders(<OwnerEmailPrompt />);
    await waitFor(() => expect(api.users.me).toHaveBeenCalled());
    expect(screen.queryByText('Set your email')).toBeNull();
  });

  it('"Later" hides it and remembers that in this browser', async () => {
    const user = userEvent.setup();
    mockMe(true);
    const { unmount } = renderWithProviders(<OwnerEmailPrompt />);
    await user.click(await screen.findByRole('button', { name: 'Later' }));
    await waitFor(() => expect(screen.queryByText('Set your email')).toBeNull());
    unmount();

    renderWithProviders(<OwnerEmailPrompt />);
    await new Promise((r) => setTimeout(r, 0));
    expect(screen.queryByText('Set your email')).toBeNull();
    expect(api.users.me).toHaveBeenCalledTimes(1);
  });
});
