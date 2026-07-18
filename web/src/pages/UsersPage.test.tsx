import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { UsersPage } from './UsersPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockResolvedValue({ users: [], departments: [], agents: [] });
});

describe('UsersPage', () => {
  it('renders the users heading', () => {
    renderWithProviders(<UsersPage />);

    expect(screen.getByRole('heading', { name: 'User Management' })).toBeInTheDocument();
  });

  it('shows empty state when no users', async () => {
    renderWithProviders(<UsersPage />);

    expect(await screen.findByText('User Management', { selector: 'p' })).toBeInTheDocument();
  });

  it('opens create user dialog when button clicked', async () => {
    const user = userEvent.setup();
    renderWithProviders(<UsersPage />);

    const createButton = screen.getByRole('button', { name: /create user/i });
    await user.click(createButton);

    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });
});
