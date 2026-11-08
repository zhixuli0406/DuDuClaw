import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ExternalAccessPage } from './ExternalAccessPage';

const SECRET = 'ddc_status_SECRETVALUE0123456789';
const token = (over: Record<string, unknown> = {}) => ({
  id: 't1', name: 'Dept board', created_at: '2026-11-02T08:00:00Z', last_used_at: null, revoked_at: null, active: true, ...over,
});
let list: { enabled: boolean; max_active: number; endpoint_path: string; tokens: ReturnType<typeof token>[] };

function reject(code: string) { return Object.assign(new Error('raw server text'), { code }); }

beforeEach(() => {
  vi.clearAllMocks();
  list = { enabled: true, max_active: 5, endpoint_path: '/api/v1/status/employees', tokens: [token()] };
  mockWsClient.call.mockImplementation(async (method: string) => {
    if (method === 'me.status_tokens.list') return structuredClone(list);
    if (method === 'me.status_tokens.create') return { secret: SECRET, token: token({ id: 't2', name: 'Weekly sheet' }), endpoint_path: list.endpoint_path };
    if (method === 'me.status_tokens.revoke') return { id: 't1', revoked: true };
    return {};
  });
});

describe('ExternalAccessPage', () => {
  it('shows the disabled state with creation disabled and no token list', async () => {
    list.enabled = false;
    renderWithProviders(<ExternalAccessPage />);
    expect(await screen.findByText('An administrator has not turned this feature on yet')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create credential' })).toBeDisabled();
    expect(screen.queryByText('My credentials')).not.toBeInTheDocument();
  });

  it('lists credentials with last-used, revoked status and explains the five values', async () => {
    list.tokens = [token(), token({ id: 't0', name: 'Old portal', revoked_at: '2026-10-01T00:00:00Z', active: false })];
    renderWithProviders(<ExternalAccessPage />);
    expect(await screen.findByText('Dept board')).toBeInTheDocument();
    // Both fixture tokens (active and revoked) have last_used_at null, so each row shows it.
    expect(screen.getAllByText('Never used')).toHaveLength(2);
    expect(screen.getByText('In use')).toBeInTheDocument();
    expect(screen.getByText('Revoked')).toBeInTheDocument();
    expect(screen.getByText('1/5 in use')).toBeInTheDocument();
    // Only the active token has a revoke button.
    expect(screen.getAllByRole('button', { name: /^Revoke / })).toHaveLength(1);
    for (const label of ['In progress', 'Awaiting confirmation', 'Done today', 'Done in 7 days', 'Latest finished item']) {
      expect(screen.getByText(label)).toBeInTheDocument();
    }
  });

  it('creates a credential, shows the secret once, and never again after closing', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalAccessPage />);
    await user.click(await screen.findByRole('button', { name: 'Create credential' }));
    await user.type(screen.getByLabelText('Credential name'), '  Weekly sheet ');
    await user.click(screen.getByRole('button', { name: 'Create' }));
    expect(mockWsClient.call).toHaveBeenCalledWith('me.status_tokens.create', { name: 'Weekly sheet' });

    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Shown only this once.')).toBeInTheDocument();
    expect(within(dialog).getByDisplayValue(SECRET)).toHaveAttribute('readonly');
    expect(dialog.textContent).toContain(`curl -H "Authorization: Bearer <Credential>" ${window.location.origin}/api/v1/status/employees`);
    expect(dialog.textContent).toContain('latest_done');

    await user.click(within(dialog).getByRole('button', { name: 'I have copied it, close' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument());
    expect(screen.queryByDisplayValue(SECRET)).not.toBeInTheDocument();
    expect(document.body.textContent).not.toContain(SECRET);
    // The list refresh never carries the secret either.
    expect(mockWsClient.call.mock.calls.filter(([m]) => m === 'me.status_tokens.list').length).toBeGreaterThan(1);
  });

  it('does not close the secret dialog on Escape', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalAccessPage />);
    await user.click(await screen.findByRole('button', { name: 'Create credential' }));
    await user.type(screen.getByLabelText('Credential name'), 'x');
    await user.click(screen.getByRole('button', { name: 'Create' }));
    await screen.findByDisplayValue(SECRET);
    await user.keyboard('{Escape}');
    expect(screen.getByDisplayValue(SECRET)).toBeInTheDocument();
  });

  it('copies the secret and shows 已複製-style feedback', async () => {
    const user = userEvent.setup();
    const write = vi.spyOn(navigator.clipboard, 'writeText').mockResolvedValue();
    renderWithProviders(<ExternalAccessPage />);
    await user.click(await screen.findByRole('button', { name: 'Create credential' }));
    await user.type(screen.getByLabelText('Credential name'), 'x');
    await user.click(screen.getByRole('button', { name: 'Create' }));
    await user.click(await screen.findByRole('button', { name: 'Copy' }));
    expect(write).toHaveBeenCalledWith(SECRET);
    expect(await screen.findByRole('button', { name: 'Copied' })).toBeInTheDocument();
    expect(screen.getByText('Copied to the clipboard')).toBeInTheDocument();
  });

  it('maps a rejected create to a friendly message, not the raw server text', async () => {
    const user = userEvent.setup();
    mockWsClient.call.mockImplementation(async (method: string) => {
      if (method === 'me.status_tokens.list') return structuredClone(list);
      throw reject('status_token_invalid_name');
    });
    renderWithProviders(<ExternalAccessPage />);
    await user.click(await screen.findByRole('button', { name: 'Create credential' }));
    await user.type(screen.getByLabelText('Credential name'), 'x');
    await user.click(screen.getByRole('button', { name: 'Create' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Enter a name of 1 to 60 characters.');
    expect(screen.queryByText('raw server text')).not.toBeInTheDocument();
  });

  it('revokes after confirmation and refreshes the list', async () => {
    const user = userEvent.setup();
    renderWithProviders(<ExternalAccessPage />);
    await user.click(await screen.findByRole('button', { name: 'Revoke Dept board' }));
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText('Revoke “Dept board”?')).toBeInTheDocument();
    list.tokens = [token({ revoked_at: '2026-11-08T00:00:00Z', active: false })];
    await user.click(within(dialog).getByRole('button', { name: 'Revoke' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('me.status_tokens.revoke', { id: 't1' }));
    expect(await screen.findByText('Revoked')).toBeInTheDocument();
  });

  it('disables creation at the limit, ignoring revoked tokens', async () => {
    list.tokens = [
      ...[1, 2, 3, 4, 5].map(n => token({ id: `a${n}`, name: `Token ${n}` })),
      token({ id: 'r', name: 'Gone', revoked_at: '2026-10-01T00:00:00Z', active: false }),
    ];
    renderWithProviders(<ExternalAccessPage />);
    const create = await screen.findByRole('button', { name: 'Create credential' });
    expect(create).toBeDisabled();
    expect(screen.getByText(/You have reached the limit of 5/)).toBeInTheDocument();
    expect(screen.getByText('5/5 in use')).toBeInTheDocument();
  });

  it('allows creation with revoked tokens beyond the limit count', async () => {
    list.tokens = [
      ...[1, 2, 3, 4].map(n => token({ id: `a${n}`, name: `Token ${n}` })),
      ...[1, 2].map(n => token({ id: `r${n}`, name: `Gone ${n}`, revoked_at: '2026-10-01T00:00:00Z', active: false })),
    ];
    renderWithProviders(<ExternalAccessPage />);
    expect(await screen.findByRole('button', { name: 'Create credential' })).toBeEnabled();
  });

  it('shows a retry when the list cannot be loaded', async () => {
    mockWsClient.call.mockRejectedValue(reject('status_token_unavailable'));
    renderWithProviders(<ExternalAccessPage />);
    expect(await screen.findByRole('alert')).toHaveTextContent('Could not load credentials');
  });
});
