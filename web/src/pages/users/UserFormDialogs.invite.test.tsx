import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { CreateUserDialog } from './UserFormDialogs';

beforeEach(() => { vi.clearAllMocks(); mockWsClient.call.mockResolvedValue({ departments: [] }); });
it('invites without sending a temporary password and previews the email first', async () => {
  const onCreated = vi.fn();
  mockWsClient.call.mockImplementation(async (method: string) => method === 'users.create' ? { user: {}, invite: { sent: false, reason: 'unconfigured', link: 'https://example.com/invite#secret' } } : { departments: [] });
  renderWithProviders(<CreateUserDialog users={[]} onClose={vi.fn()} onCreated={onCreated} />);
  fireEvent.change(await screen.findByPlaceholderText('Email address'), { target: { value: 'person@example.com' } });
  fireEvent.change(screen.getByPlaceholderText('Display name'), { target: { value: 'Person' } });
  fireEvent.click(screen.getByRole('checkbox', { name: 'Send invitation email' }));
  expect(screen.queryByPlaceholderText('Password (min 8 characters)')).toBeNull();
  expect(screen.getByText('Email preview (sent in Traditional Chinese)')).toBeInTheDocument();
  fireEvent.click(screen.getByRole('button', { name: 'Create' }));
  await waitFor(() => expect(onCreated).toHaveBeenCalledWith(expect.objectContaining({ sent: false })));
  const call = mockWsClient.call.mock.calls.find(([method]) => method === 'users.create');
  expect(call?.[1]).toEqual(expect.objectContaining({ invite: true, public_url: window.location.origin }));
  expect(call?.[1]).not.toHaveProperty('password');
});
it('does not create a password account with a short password', async () => {
  renderWithProviders(<CreateUserDialog users={[]} onClose={vi.fn()} onCreated={vi.fn()} />);
  fireEvent.change(await screen.findByPlaceholderText('Email address'), { target: { value: 'person@example.com' } });
  fireEvent.change(screen.getByPlaceholderText('Display name'), { target: { value: 'Person' } });
  fireEvent.change(screen.getByPlaceholderText('Password (min 8 characters)'), { target: { value: 'short' } });
  fireEvent.click(screen.getByRole('button', { name: 'Create' }));
  expect(mockWsClient.call.mock.calls.some(([method]) => method === 'users.create')).toBe(false);
});
