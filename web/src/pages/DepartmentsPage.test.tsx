import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { DepartmentsPage } from './DepartmentsPage';
import { useConnectionStore } from '@/stores/connection-store';

beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' as never, error: null });
  mockWsClient.call.mockResolvedValue({ users: [], departments: [], agents: [] });
});

describe('DepartmentsPage', () => {
  it('renders the departments heading', () => {
    renderWithProviders(<DepartmentsPage />);

    expect(screen.getByRole('heading', { name: 'Departments' })).toBeInTheDocument();
  });

  it('shows empty state when no departments', async () => {
    renderWithProviders(<DepartmentsPage />);

    expect(await screen.findByText('No departments yet')).toBeInTheDocument();
  });

  it('opens create department dialog when button clicked', async () => {
    const user = userEvent.setup();
    renderWithProviders(<DepartmentsPage />);

    const createButton = screen.getByRole('button', { name: /create/i });
    await user.click(createButton);

    expect(screen.getByRole('dialog')).toBeInTheDocument();
  });
});
