import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { mockWsClient } from '@/test/mocks';
import { useConnectionStore } from '@/stores/connection-store';
import { MigrationConfirmationsPage } from './MigrationConfirmationsPage';
import type { ConfirmationItem, ConfirmationList } from '@/lib/migration-confirmations';

const item = (high_risk: boolean, id: number): ConfirmationItem => ({
  binding: { operation_id: { proposal_id: Array(16).fill(id), revision: 1 }, proposal_digest: Array(32).fill(id) },
  category: high_risk ? 'mcp_server' : 'department', status: 'pending', high_risk, apply_available: true, rotation_pending: false,
  fields: [{ name: 'name', value: { kind: 'text', value: '<script>unsafe()</script>' } }],
});
const response = (items = [item(true, 1), item(false, 2)]): ConfirmationList => ({
  active: true, incomplete: true, items, mutations_available: true,
  csrf_token: Array(32).fill(9), mfa_enrolled: true, step_up_valid: true,
});
beforeEach(() => {
  vi.clearAllMocks();
  useConnectionStore.setState({ state: 'authenticated' });
  mockWsClient.call.mockResolvedValue(response());
});
describe('migration confirmation', () => {
  it('labels rejected employee content as not migrated and retained until the snapshot deadline', async () => {
    mockWsClient.call.mockResolvedValue(response([{ ...item(true, 1), category: 'employee',
      status: 'rejected', content_status: 'not_migrated_rejected' }]));
    renderWithProviders(<MigrationConfirmationsPage />);
    await screen.findByText('Not migrated (rejected); content remains in the encrypted snapshot until its retention deadline.');
    expect(screen.queryByText('Applied and verified')).toBeNull();
  });
  it('renders full untrusted text inertly and excludes high-risk proposals from bulk selection', async () => {
    renderWithProviders(<MigrationConfirmationsPage />);
    await screen.findAllByText('<script>unsafe()</script>');
    expect(document.querySelector('script')).toBeNull();
    expect(screen.getAllByRole('checkbox')).toHaveLength(2);
    fireEvent.click(screen.getAllByRole('checkbox')[0]);
    expect(screen.getByRole('button', { name: 'Approve selected' })).toBeDisabled();
    fireEvent.click(screen.getAllByRole('checkbox')[0]);
    fireEvent.click(screen.getAllByRole('checkbox')[1]);
    fireEvent.click(screen.getByRole('button', { name: 'Approve selected' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('migration.confirmations.approve', {
      bindings: [item(false, 2).binding], csrf_token: Array(32).fill(9),
    }));
  });
  it('keeps confirmation disabled when the authenticated adapter is unavailable', async () => {
    mockWsClient.call.mockResolvedValue({ ...response(), mutations_available: false, csrf_token: null });
    renderWithProviders(<MigrationConfirmationsPage />);
    await screen.findByText(/Review only/);
    for (const button of screen.getAllByRole('button', { name: 'Approve this item' })) expect(button).toBeDisabled();
  });
  it('masks secret fingerprints and keeps retained secrets visibly incomplete', async () => {
    const proposal = item(true, 1);
    proposal.rotation_pending = true;
    proposal.fields = [{ name: 'credential', value: { kind: 'secret_fingerprint', keyed_fingerprint: Array(32).fill(87), needs_rotation: true } }];
    mockWsClient.call.mockResolvedValue(response([proposal]));
    renderWithProviders(<MigrationConfirmationsPage />);
    await screen.findByText(/Rotation required/);
    expect(screen.queryByText(/87,87/)).toBeNull();
    expect(screen.getByText(/Migration is incomplete/)).toBeInTheDocument();
  });
  it('does not present successful approval as applied without readback', async () => {
    mockWsClient.call.mockResolvedValueOnce(response([item(false, 2)]))
      .mockResolvedValueOnce({ approved: true }).mockRejectedValueOnce(new Error('readback unavailable'));
    renderWithProviders(<MigrationConfirmationsPage />);
    fireEvent.click(await screen.findByRole('button', { name: 'Approve this item' }));
    await screen.findByRole('alert');
    expect(screen.queryByText('Applied and verified')).toBeNull();
  });
  it('verifies password and TOTP before enabling mutations', async () => {
    mockWsClient.call.mockResolvedValueOnce({ ...response(), csrf_token: null, step_up_valid: false })
      .mockResolvedValueOnce({ verified: true }).mockResolvedValueOnce(response());
    renderWithProviders(<MigrationConfirmationsPage />);
    fireEvent.change(await screen.findByLabelText('Current password'), { target: { value: 'not-stored' } });
    fireEvent.change(screen.getByLabelText('Six-digit authenticator code'), { target: { value: '123456' } });
    fireEvent.click(screen.getByRole('button', { name: 'Verify identity' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('migration.confirmations.step_up', { password: 'not-stored', code: '123456' }));
    await waitFor(() => expect(screen.queryByLabelText('Current password')).toBeNull());
    expect(screen.getAllByRole('button', { name: 'Approve this item' })[0]).toBeEnabled();
  });
  it('rejects selected pending items including high risk without approving them', async () => {
    renderWithProviders(<MigrationConfirmationsPage />);
    const boxes = await screen.findAllByRole('checkbox');
    boxes.forEach(box => fireEvent.click(box));
    fireEvent.click(screen.getByRole('button', { name: 'Reject selected' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('migration.confirmations.reject', {
      bindings: [item(true, 1).binding, item(false, 2).binding], csrf_token: Array(32).fill(9),
    }));
  });
  it('keeps an unsupported item rejectable but not approvable', async () => {
    mockWsClient.call.mockResolvedValue(response([{ ...item(true, 1), apply_available: false }]));
    renderWithProviders(<MigrationConfirmationsPage />);
    expect(await screen.findByRole('button', { name: 'Approve this item' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Reject and disable' })).toBeEnabled();
  });
  it('rotates an applied credential using the bound proposal and reloads readback', async () => {
    const proposal = { ...item(true, 1), category: 'configuration_section', status: 'applied' as const, rotation_pending: true };
    mockWsClient.call.mockResolvedValueOnce(response([proposal])).mockResolvedValueOnce({ readback_required: true })
      .mockResolvedValueOnce(response([{ ...proposal, rotation_pending: false }]));
    renderWithProviders(<MigrationConfirmationsPage />);
    fireEvent.change(await screen.findByLabelText('New credential'), { target: { value: 'new-secret-canary' } });
    fireEvent.click(screen.getByRole('button', { name: 'Update and verify credential' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('migration.confirmations.rotate_secret', {
      binding: proposal.binding, new_value: 'new-secret-canary', csrf_token: Array(32).fill(9),
    }));
    await waitFor(() => expect(screen.queryByLabelText('New credential')).toBeNull());
    expect(screen.queryByText('new-secret-canary')).toBeNull();
  });
});
