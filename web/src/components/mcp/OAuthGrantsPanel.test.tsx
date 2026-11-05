import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { OAuthGrantsPanel } from './OAuthGrantsPanel';

const grant = {
  id: '7731faf7-03a8-4cdf-ac76-e04d68f1f2a9', client_name: 'Desktop client',
  scopes: ['memory:read'], issued_unix: 1780000000, last_used_unix: null,
};

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'owner', email: '', display_name: 'Owner', role: 'admin', status: 'active' } });
  mockWsClient.call.mockResolvedValue({ grants: [grant] });
});

describe('OAuthGrantsPanel', () => {
  it('lists grants and revokes only after confirmation', async () => {
    let revoked = false;
    mockWsClient.call.mockImplementation(async (method) => {
      if (method === 'mcp_oauth_grants.revoke') { revoked = true; return { success: true, revoked: true }; }
      return { grants: revoked ? [] : [grant] };
    });
    renderWithProviders(<OAuthGrantsPanel />);
    expect(await screen.findByText('Desktop client')).toBeInTheDocument();
    expect(screen.getByText('memory:read')).toBeInTheDocument();
    expect(screen.getByText('Not used yet')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    expect(mockWsClient.call).not.toHaveBeenCalledWith('mcp_oauth_grants.revoke', expect.anything());
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('mcp_oauth_grants.revoke', { id: grant.id }));
    expect(await screen.findByText('No active OAuth grants')).toBeInTheDocument();
  });

  it('does not list grants for non-admin users', () => {
    useAuthStore.setState({ user: { id: 'employee', email: '', display_name: 'Employee', role: 'employee', status: 'active' } });
    renderWithProviders(<OAuthGrantsPanel />);
    expect(mockWsClient.call).not.toHaveBeenCalled();
    expect(screen.queryByText('OAuth connections')).not.toBeInTheDocument();
  });

  it('shows a retryable error instead of an empty list', async () => {
    mockWsClient.call.mockRejectedValueOnce(new Error('unavailable'));
    renderWithProviders(<OAuthGrantsPanel />);
    const retry = await screen.findByRole('button', { name: /try again/i });
    expect(screen.queryByText('No active OAuth grants')).not.toBeInTheDocument();
    mockWsClient.call.mockResolvedValue({ grants: [] });
    fireEvent.click(retry);
    expect(await screen.findByText('No active OAuth grants')).toBeInTheDocument();
  });

  it('keeps the grant and confirmation visible when revocation fails', async () => {
    renderWithProviders(<OAuthGrantsPanel />);
    await screen.findByText('Desktop client');
    fireEvent.click(screen.getByRole('button', { name: 'Revoke' }));
    const dialog = await screen.findByRole('dialog');
    mockWsClient.call.mockRejectedValueOnce(new Error('offline'));
    fireEvent.click(within(dialog).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(within(dialog).getByRole('button', { name: 'Revoke' })).toBeEnabled());
    expect(screen.getByRole('dialog')).toBeInTheDocument();
    // A modal hides the background list from the accessibility tree.
    expect(screen.getByRole('heading', { name: 'Desktop client', hidden: true })).toBeInTheDocument();
    fireEvent.click(within(dialog).getByRole('button', { name: 'Cancel' }));
    expect(await screen.findByRole('heading', { name: 'Desktop client' })).toBeVisible();
  });
});
