import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { UpdateConfirmCard } from './UpdateConfirmCard';
import { api } from '@/lib/api';

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('<UpdateConfirmCard> — O-3 inline update confirmation', () => {
  it('target=device: confirm calls api.device.updateApply() and shows the success state', async () => {
    const updateApply = vi
      .spyOn(api.device, 'updateApply')
      .mockResolvedValue({ success: true, stdout: 'duduclaw-sysd: applied 1.2.3', stderr: '' });
    const applyUpdate = vi.spyOn(api.system, 'applyUpdate');

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'device' }} />);
    const user = userEvent.setup();

    expect(screen.getByText('Apply device update?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Apply update' }));

    await waitFor(() => expect(updateApply).toHaveBeenCalledTimes(1));
    expect(applyUpdate).not.toHaveBeenCalled();
    expect(await screen.findByText('Update complete')).toBeInTheDocument();
    expect(screen.getByText('duduclaw-sysd: applied 1.2.3')).toBeInTheDocument();
  });

  it('target=system: confirm calls api.system.applyUpdate() and shows the success state', async () => {
    const applyUpdate = vi
      .spyOn(api.system, 'applyUpdate')
      .mockResolvedValue({ success: true, message: 'Updated to 1.62.1', needs_restart: true });
    const updateApply = vi.spyOn(api.device, 'updateApply');

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'system' }} />);
    const user = userEvent.setup();

    expect(screen.getByText('Apply app update?')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Apply update' }));

    await waitFor(() => expect(applyUpdate).toHaveBeenCalledTimes(1));
    expect(updateApply).not.toHaveBeenCalled();
    expect(await screen.findByText('Update complete')).toBeInTheDocument();
    expect(screen.getByText('Updated to 1.62.1')).toBeInTheDocument();
  });

  it('target=system: needs_restart:true shows the auto-restart reminder', async () => {
    vi.spyOn(api.system, 'applyUpdate')
      .mockResolvedValue({ success: true, message: 'Updated to 1.62.1', needs_restart: true });

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'system' }} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Apply update' }));

    expect(await screen.findByText('Update complete')).toBeInTheDocument();
    expect(
      screen.getByText(
        'Update applied — the service will restart automatically and the page will reload shortly.',
      ),
    ).toBeInTheDocument();
  });

  it('target=system: needs_restart:false does NOT show the auto-restart reminder', async () => {
    vi.spyOn(api.system, 'applyUpdate')
      .mockResolvedValue({ success: true, message: 'Updated to 1.62.1', needs_restart: false });

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'system' }} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Apply update' }));

    expect(await screen.findByText('Update complete')).toBeInTheDocument();
    expect(
      screen.queryByText(
        'Update applied — the service will restart automatically and the page will reload shortly.',
      ),
    ).not.toBeInTheDocument();
  });

  it('target=device: DeviceOpResult has no needs_restart field, so the reminder never shows', async () => {
    vi.spyOn(api.device, 'updateApply')
      .mockResolvedValue({ success: true, stdout: 'ok', stderr: '' });

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'device' }} />);
    const user = userEvent.setup();
    await user.click(screen.getByRole('button', { name: 'Apply update' }));

    expect(await screen.findByText('Update complete')).toBeInTheDocument();
    expect(
      screen.queryByText(
        'Update applied — the service will restart automatically and the page will reload shortly.',
      ),
    ).not.toBeInTheDocument();
  });

  it('target=device: "view full page" link points at /device', () => {
    renderWithProviders(<UpdateConfirmCard payload={{ target: 'device' }} />);
    expect(screen.getByRole('link', { name: /view full page/i })).toHaveAttribute('href', '/device');
  });

  it('target=system: "view full page" link points at /app/system/updates, not /device', () => {
    renderWithProviders(<UpdateConfirmCard payload={{ target: 'system' }} />);
    expect(screen.getByRole('link', { name: /view full page/i })).toHaveAttribute(
      'href',
      '/app/system/updates',
    );
  });

  it('cancel resolves the card without calling any update RPC', async () => {
    const updateApply = vi.spyOn(api.device, 'updateApply');
    renderWithProviders(<UpdateConfirmCard payload={{ target: 'device' }} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(await screen.findByText('Cancelled — nothing was changed')).toBeInTheDocument();
    expect(updateApply).not.toHaveBeenCalled();
  });

  it('a thrown RPC error surfaces inline and allows an immediate retry', async () => {
    const updateApply = vi
      .spyOn(api.device, 'updateApply')
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce({ success: true, stdout: '', stderr: '' });

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'device' }} />);
    const user = userEvent.setup();

    const confirmButton = screen.getByRole('button', { name: 'Apply update' });
    await user.click(confirmButton);
    expect(await screen.findByRole('alert')).toHaveTextContent('boom');

    // Retry without leaving the confirm view.
    expect(confirmButton).toBeEnabled();
    await user.click(confirmButton);
    await waitFor(() => expect(updateApply).toHaveBeenCalledTimes(2));
    expect(await screen.findByText('Update complete')).toBeInTheDocument();
  });

  it('a settled success:false result (no thrown error) also allows an immediate retry', async () => {
    const applyUpdate = vi
      .spyOn(api.system, 'applyUpdate')
      .mockResolvedValueOnce({ success: false, message: 'checksum mismatch', needs_restart: false })
      .mockResolvedValueOnce({ success: true, message: 'Updated', needs_restart: false });

    renderWithProviders(<UpdateConfirmCard payload={{ target: 'system' }} />);
    const user = userEvent.setup();

    const confirmButton = screen.getByRole('button', { name: 'Apply update' });
    await user.click(confirmButton);
    expect(await screen.findByRole('alert')).toHaveTextContent('checksum mismatch');
    expect(confirmButton).toBeEnabled();

    await user.click(confirmButton);
    await waitFor(() => expect(applyUpdate).toHaveBeenCalledTimes(2));
    expect(await screen.findByText('Update complete')).toBeInTheDocument();
  });
});
