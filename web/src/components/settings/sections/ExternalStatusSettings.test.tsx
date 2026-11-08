import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ExternalStatusSettings } from './ExternalStatusSettings';

const row = (over: Record<string, unknown>) => ({
  id: 'a', name: 'Dept board', created_at: '2026-11-02T08:00:00Z', last_used_at: null, revoked_at: null, active: true,
  user_id: 'u1', user_email: 'amy@example.com', user_display_name: 'Amy', ...over,
});
let tokens: ReturnType<typeof row>[];
let enabled: boolean;

beforeEach(() => {
  vi.clearAllMocks();
  enabled = false;
  tokens = [
    row({}),
    row({ id: 'b', name: 'Sheet', user_id: 'u2', user_email: 'bob@example.com', user_display_name: 'Bob' }),
    row({ id: 'c', name: 'Old portal', revoked_at: '2026-10-01T00:00:00Z', active: false }),
  ];
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'system.config') return { external_status_enabled: enabled };
    if (method === 'status_tokens.admin_list') return { enabled, endpoint_path: '/api/v1/status/employees', tokens: structuredClone(tokens) };
    if (method === 'system.update_config') return { success: true, changes: [] };
    if (method === 'status_tokens.admin_revoke') return { id: 'a', revoked: true };
    return {};
  });
});

describe('ExternalStatusSettings', () => {
  it('is off by default and shows the pre-enable warning', async () => {
    renderWithProviders(<ExternalStatusSettings />);
    const toggle = await screen.findByRole('switch', { name: 'Allow external systems to query AI employee status' });
    await waitFor(() => expect(toggle).toBeEnabled());
    expect(toggle).toHaveAttribute('aria-checked', 'false');
    expect(screen.getByText('Please read before turning on')).toBeInTheDocument();
    expect(screen.getByText(/suspends every credential immediately/)).toBeInTheDocument();
    expect(screen.getByText(/up to 5 credentials/)).toBeInTheDocument();
  });

  it('saves the toggle through system.update_config', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalStatusSettings />);
    const toggle = await screen.findByRole('switch');
    await waitFor(() => expect(toggle).toBeEnabled());
    await user.click(toggle);
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('system.update_config', { external_status_enabled: true }));
    expect(toggle).toHaveAttribute('aria-checked', 'true');
  });

  it('rolls the toggle back when saving fails', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'system.config') return { external_status_enabled: true };
      if (method === 'status_tokens.admin_list') return { enabled: true, endpoint_path: '/p', tokens: [] };
      throw new Error('boom');
    });
    renderWithProviders(<ExternalStatusSettings />);
    const toggle = await screen.findByRole('switch');
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
    await user.click(toggle);
    await waitFor(() => expect(toggle).toHaveAttribute('aria-checked', 'true'));
  });

  it('lists everyone’s credentials with counts and filters by person', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalStatusSettings />);
    // Amy owns two of the three fixture rows (active "Dept board" + revoked "Old portal"); Bob owns one.
    expect(await screen.findAllByText('Amy')).toHaveLength(2);
    expect(screen.getAllByText('Bob')).toHaveLength(1);
    expect(screen.getByText('3 in total, 2 in use')).toBeInTheDocument();
    expect(screen.getByText('Revoked')).toBeInTheDocument();
    await user.type(screen.getByRole('searchbox', { name: 'Search people or credential names' }), 'bob');
    expect(screen.queryByText('Dept board')).not.toBeInTheDocument();
    expect(screen.getByText('Sheet')).toBeInTheDocument();
  });

  it('revokes another person’s credential only after the admin confirms', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalStatusSettings />);
    await user.click(await screen.findByRole('button', { name: 'Revoke Amy Dept board' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Revoke Amy\'s “Dept board”?')).toBeInTheDocument();
    expect(mockWsClient.call).not.toHaveBeenCalledWith('status_tokens.admin_revoke', expect.anything());
    await user.click(within(dialog).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('status_tokens.admin_revoke', { id: 'a' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
  });

  it('shows a friendly error when the revoke is refused', async () => {
    const user = userEvent.setup();
    const base = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation(async (method: string, params?: unknown) => {
      if (method === 'status_tokens.admin_revoke') throw Object.assign(new Error('raw'), { code: 'status_token_not_found' });
      return base(method, params);
    });
    renderWithProviders(<ExternalStatusSettings />);
    await user.click(await screen.findByRole('button', { name: 'Revoke Amy Dept board' }));
    await user.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Revoke' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('That credential no longer exists');
  });
});
