import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { ConfirmActionCard } from './ConfirmActionCard';
import { api } from '@/lib/api';

beforeEach(() => {
  vi.restoreAllMocks();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('<ConfirmActionCard> — O-3 inline destructive confirmation', () => {
  it('factory reset stays blocked until the exact word "RESET" is typed, then calls device.factoryReset', async () => {
    const factoryReset = vi
      .spyOn(api.device, 'factoryReset')
      .mockResolvedValue({ success: true, stdout: '', stderr: '' });

    renderWithProviders(<ConfirmActionCard payload={{ action: 'factory_reset' }} />);
    const user = userEvent.setup();

    const confirmButton = screen.getByRole('button', { name: 'Factory reset' });
    expect(confirmButton).toBeDisabled();

    await user.type(screen.getByPlaceholderText('RESET'), 'RESET');
    expect(confirmButton).toBeEnabled();

    await user.click(confirmButton);
    await waitFor(() => expect(factoryReset).toHaveBeenCalledTimes(1));
    expect(
      await screen.findByText('Factory reset started — this device is about to restart'),
    ).toBeInTheDocument();
  });

  it('restart stays blocked for a mandatory countdown, then calls device.power("restart")', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const power = vi.spyOn(api.device, 'power').mockResolvedValue({ success: true, stdout: '', stderr: '' });

    renderWithProviders(<ConfirmActionCard payload={{ action: 'restart' }} />);

    const confirmButton = screen.getByRole('button', { name: 'Restart' });
    expect(confirmButton).toBeDisabled();
    expect(screen.getByText(/Confirm available in \d+s/)).toBeInTheDocument();

    await vi.advanceTimersByTimeAsync(6000);
    expect(confirmButton).toBeEnabled();

    await user.click(confirmButton);
    await waitFor(() => expect(power).toHaveBeenCalledWith('restart'));
  });

  it('cancel resolves the card without calling any device RPC', async () => {
    const power = vi.spyOn(api.device, 'power');
    renderWithProviders(<ConfirmActionCard payload={{ action: 'shutdown' }} />);
    const user = userEvent.setup();

    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(await screen.findByText('Cancelled — nothing was changed')).toBeInTheDocument();
    expect(power).not.toHaveBeenCalled();
  });

  it('a failed device.power call surfaces the error and allows an immediate retry', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const power = vi
      .spyOn(api.device, 'power')
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce({ success: true, stdout: '', stderr: '' });

    renderWithProviders(<ConfirmActionCard payload={{ action: 'shutdown' }} />);
    await vi.advanceTimersByTimeAsync(6000);

    const confirmButton = screen.getByRole('button', { name: 'Shut down' });
    await user.click(confirmButton);
    expect(await screen.findByRole('alert')).toHaveTextContent('boom');

    // Retry without re-arming the countdown — the button is enabled again
    // immediately since the gate condition (countdown already elapsed)
    // hasn't changed.
    expect(confirmButton).toBeEnabled();
    await user.click(confirmButton);
    await waitFor(() => expect(power).toHaveBeenCalledTimes(2));
  });
});
