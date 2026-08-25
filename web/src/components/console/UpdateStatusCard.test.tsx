import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { UpdateStatusCard } from './UpdateStatusCard';
import { api } from '@/lib/api';
import { toast } from '@/lib/toast';
import {
  FIXTURE_UPDATE_STATUS,
  FIXTURE_UPDATE_STATUS_WITH_SYSTEM,
  FIXTURE_UPDATE_STATUS_SYSTEM_ONLY,
} from './fixtures';
import type { UpdateStatusArtifact } from './artifact-types';

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('<UpdateStatusCard> — system (self-update) half', () => {
  it('renders nothing extra when payload.system is absent (backward compatible)', () => {
    renderWithProviders(<UpdateStatusCard payload={FIXTURE_UPDATE_STATUS.payload as UpdateStatusArtifact['payload']} />);
    expect(screen.queryByText('DuDuClaw version')).not.toBeInTheDocument();
  });

  it('renders the DuDuClaw version section with an "update available" badge when system.available is true', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_WITH_SYSTEM.payload as UpdateStatusArtifact['payload']} />,
    );
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update available')).toBeInTheDocument();
    expect(screen.getByText('v1.61.2 → v1.62.0')).toBeInTheDocument();
  });

  it('renders an "up to date" badge when system.available is false', () => {
    renderWithProviders(
      <UpdateStatusCard
        payload={{
          action: 'check',
          result: { success: true, stdout: '', stderr: '' },
          system: {
            available: false,
            current_version: '1.62.0',
            latest_version: '1.62.0',
          },
        }}
      />,
    );
    expect(screen.getByText('Up to date')).toBeInTheDocument();
    expect(screen.getByText('v1.62.0 → v1.62.0')).toBeInTheDocument();
  });

  it('renders a check-failed note (not the version card) when system is the error shape, classified through formatError rather than shown verbatim', () => {
    // no-linux-surface item 7/11: `system.error` is `updater::check_update()`'s
    // raw backend text — the card must run it through `formatError` (same
    // classify+mask treatment a thrown error gets), never render it as-is.
    // See `formatError`'s doc comment in `lib/toast.ts` for why "unknown"
    // classification + verbatim (non-technical-looking) detail is the
    // expected shape for a plain English sentence like this one.
    renderWithProviders(
      <UpdateStatusCard
        payload={{
          action: 'check',
          result: { success: true, stdout: '', stderr: '' },
          system: { error: 'GitHub API rate limited' },
        }}
      />,
    );
    expect(
      screen.getByText('Version check failed: no response just now (GitHub API rate limited)'),
    ).toBeInTheDocument();
    expect(screen.queryByText('DuDuClaw version')).not.toBeInTheDocument();
  });
});

describe('<UpdateStatusCard> — P5: optional `result` (device) half', () => {
  it('renders only the system (version) section when `result` is absent (non-appliance install), without throwing', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_SYSTEM_ONLY.payload as UpdateStatusArtifact['payload']} />,
    );
    // The version section IS present, sourced from `system` alone.
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update available')).toBeInTheDocument();
    // The device-result section (success/fail badge, rollback button, details
    // toggle) must be entirely absent — never a fabricated `DeviceOpResult`.
    expect(screen.queryByText('Update complete')).not.toBeInTheDocument();
    expect(screen.queryByText("The update didn't finish — see the details below")).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Roll back to previous version/ })).not.toBeInTheDocument();
    expect(screen.queryByText('Show details')).not.toBeInTheDocument();
  });

  it('renders both sections when both `result` and `system` are present (appliance install, unchanged)', () => {
    renderWithProviders(
      <UpdateStatusCard payload={FIXTURE_UPDATE_STATUS_WITH_SYSTEM.payload as UpdateStatusArtifact['payload']} />,
    );
    expect(screen.getByText('DuDuClaw version')).toBeInTheDocument();
    expect(screen.getByText('Update complete')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Roll back to previous version/ })).toBeInTheDocument();
  });
});

describe('<UpdateStatusCard> — rollback (inline confirm, three-way response handling)', () => {
  const payload = FIXTURE_UPDATE_STATUS_WITH_SYSTEM.payload as UpdateStatusArtifact['payload'];

  it('reveals an inline confirm step before calling device.update_rollback', async () => {
    const rollback = vi
      .spyOn(api.device, 'updateRollback')
      .mockResolvedValue({ success: true, stdout: '', stderr: '' });
    const successToast = vi.spyOn(toast, 'success');
    const user = userEvent.setup();

    renderWithProviders(<UpdateStatusCard payload={payload} />);

    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));
    // Nothing destructive happens until the inline confirm step is accepted.
    expect(rollback).not.toHaveBeenCalled();
    expect(
      screen.getByText('The device will restart and switch back to the previous version. Your data will not be affected.'),
    ).toBeInTheDocument();

    // Two buttons now share the "Roll back to previous version" label — the
    // original trigger is hidden while confirming, so this is unambiguous.
    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));

    await waitFor(() => expect(rollback).toHaveBeenCalledTimes(1));
    await waitFor(() =>
      expect(successToast).toHaveBeenCalledWith('Rollback scheduled — the device is restarting…'),
    );
    expect(await screen.findByText('Rollback scheduled — the device is restarting…')).toBeInTheDocument();
  });

  it("shows the backend's own reason verbatim when device.update_rollback answers unsupported — not a generic error", async () => {
    const backendMessage = '沒有可回退的版本。';
    vi.spyOn(api.device, 'updateRollback').mockRejectedValue({ code: 'unsupported', message: backendMessage });
    const infoToast = vi.spyOn(toast, 'info');
    const errorToast = vi.spyOn(toast, 'error');
    const user = userEvent.setup();

    renderWithProviders(<UpdateStatusCard payload={payload} />);

    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));
    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));

    await waitFor(() => expect(infoToast).toHaveBeenCalledWith(backendMessage));
    expect(errorToast).not.toHaveBeenCalled();
    // The exact backend text renders inline too, not a generic clause.
    expect(await screen.findByText(backendMessage)).toBeInTheDocument();
  });

  it('surfaces a generic failure toast for a non-unsupported rejection', async () => {
    vi.spyOn(api.device, 'updateRollback').mockRejectedValue(new Error('network error'));
    const errorToast = vi.spyOn(toast, 'error');
    const infoToast = vi.spyOn(toast, 'info');
    const user = userEvent.setup();

    renderWithProviders(<UpdateStatusCard payload={payload} />);

    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));
    await user.click(screen.getByRole('button', { name: 'Roll back to previous version' }));

    await waitFor(() => expect(errorToast).toHaveBeenCalledTimes(1));
    expect(infoToast).not.toHaveBeenCalled();
  });
});
