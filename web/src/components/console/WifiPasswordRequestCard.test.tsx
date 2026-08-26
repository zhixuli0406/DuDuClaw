import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { WifiPasswordRequestCard } from './WifiPasswordRequestCard';
import { api } from '@/lib/api';
import { useChatStore, type PendingAttachment } from '@/stores/chat-store';

/**
 * T1 (`commercial/docs/DESIGN-agent-body-network-2026-08.md` §5/§12)
 * component tests. These are the tests referenced by the security review
 * (`DESIGN-agent-body-network-2026-08.md` §13's appendix) as proving the
 * three load-bearing security properties at the component boundary:
 *  1. the password never reaches `send()` / the chat transcript,
 *  2. the local password state is cleared the instant it's read (not just
 *     "eventually", or "after success") — verified DURING the busy state,
 *     before the RPC even resolves,
 *  3. `network.wifi_connect` is the ONLY call this component ever makes with
 *     the typed password, and it always carries
 *     `source: 'operator_console_prompted'`.
 */
describe('<WifiPasswordRequestCard> — T1 secure password hand-off', () => {
  let sendMock: ReturnType<typeof vi.fn<(text: string, attachments?: readonly PendingAttachment[]) => void>>;

  beforeEach(() => {
    vi.restoreAllMocks();
    // Default: no matching network found — the security-hint enrichment is
    // best-effort and must never block or alter the base render.
    vi.spyOn(api.network, 'wifiScan').mockResolvedValue({ networks: [], scanning: false });
    sendMock = vi.fn<(text: string, attachments?: readonly PendingAttachment[]) => void>();
    // `send()` is a no-op without a live WebSocket (`wsRef` stays null in a
    // unit test), so the real implementation can never be exercised here —
    // replace it via zustand's own `setState` (not a property mutation)
    // so the component's `useChatStore((s) => s.send)` selector picks up
    // the replacement on its next read.
    useChatStore.setState({ send: sendMock });
  });

  it('renders the SSID and a masked password field, connect disabled until typed', () => {
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    expect(screen.getByText(/iPhone-Sam/)).toBeInTheDocument();

    const input = screen.getByPlaceholderText(/wi-?fi password/i) as HTMLInputElement;
    expect(input).toHaveAttribute('type', 'password');
    expect(input).toHaveAttribute('autocomplete', 'new-password');

    const connectButton = screen.getByRole('button', { name: /connect/i });
    expect(connectButton).toBeDisabled();
  });

  it('never sends the password to the agent — success calls wifiConnect with the typed value and only the ssid+outcome reaches send()', async () => {
    const wifiConnect = vi
      .spyOn(api.network, 'wifiConnect')
      .mockResolvedValue({ state: 'connected', ssid: 'iPhone-Sam' });
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    const user = userEvent.setup();

    await user.type(screen.getByPlaceholderText(/wi-?fi password/i), 'correct horse battery staple');
    await user.click(screen.getByRole('button', { name: /connect/i }));

    await waitFor(() =>
      expect(wifiConnect).toHaveBeenCalledWith(
        'iPhone-Sam',
        'correct horse battery staple',
        'operator_console_prompted',
      ),
    );
    await waitFor(() => expect(sendMock).toHaveBeenCalledTimes(1));

    // The ONLY string sent to the agent must never contain the password.
    const [followupText] = sendMock.mock.calls[0];
    expect(followupText).not.toContain('correct horse battery staple');
    expect(followupText).toContain('iPhone-Sam');
  });

  it('clears the local password state immediately on submit — before the RPC even resolves', async () => {
    let resolveConnect: (v: { state: string; ssid: string }) => void = () => {};
    vi.spyOn(api.network, 'wifiConnect').mockImplementation(
      () => new Promise((resolve) => { resolveConnect = resolve; }),
    );
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    const user = userEvent.setup();

    const input = screen.getByPlaceholderText(/wi-?fi password/i) as HTMLInputElement;
    await user.type(input, 'super-secret');
    await user.click(screen.getByRole('button', { name: /connect/i }));

    // Still "busy" (RPC deliberately unresolved) — the field must already
    // be empty and disabled, not merely cleared once the promise settles.
    await waitFor(() => expect(input).toBeDisabled());
    expect(input.value).toBe('');

    resolveConnect({ state: 'connected', ssid: 'iPhone-Sam' });
  });

  it('a wrong-password failure shows the classified error and allows an immediate retry without notifying the agent', async () => {
    const wifiConnect = vi
      .spyOn(api.network, 'wifiConnect')
      .mockRejectedValueOnce({ code: 'wrong_password', message: 'wrong_password_boom' })
      .mockResolvedValueOnce({ state: 'connected', ssid: 'iPhone-Sam' });
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    const user = userEvent.setup();

    const input = screen.getByPlaceholderText(/wi-?fi password/i);
    await user.type(input, 'wrong-one');
    await user.click(screen.getByRole('button', { name: /connect/i }));

    expect(await screen.findByRole('alert')).toHaveTextContent('wrong_password_boom');
    // A retry-in-place must never spam the agent's context — only a final
    // success or an explicit Cancel does (design §5.3).
    expect(sendMock).not.toHaveBeenCalled();

    // Immediate retry: the gate re-arms without any extra confirmation step.
    await user.type(screen.getByPlaceholderText(/wi-?fi password/i), 'right-one');
    await user.click(screen.getByRole('button', { name: /connect/i }));
    await waitFor(() => expect(wifiConnect).toHaveBeenCalledTimes(2));
    await waitFor(() => expect(sendMock).toHaveBeenCalledTimes(1));
  });

  it('cancel notifies the agent the human gave up, without ever calling wifiConnect', async () => {
    const wifiConnect = vi.spyOn(api.network, 'wifiConnect');
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    const user = userEvent.setup();

    await user.type(screen.getByPlaceholderText(/wi-?fi password/i), 'whatever');
    await user.click(screen.getByRole('button', { name: 'Cancel' }));

    expect(wifiConnect).not.toHaveBeenCalled();
    expect(sendMock).toHaveBeenCalledTimes(1);
    const [followupText] = sendMock.mock.calls[0];
    expect(followupText).not.toContain('whatever');
    expect(followupText).toContain('iPhone-Sam');
  });

  it('shows a best-effort security-type hint when a matching network is found by a background scan', async () => {
    vi.spyOn(api.network, 'wifiScan').mockResolvedValue({
      networks: [
        { ssid: 'iPhone-Sam', signal_bars: 2, security: 'psk', connected: false, known: false, hidden: false },
      ],
      scanning: false,
    });
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    expect(await screen.findByText(/WPA/i)).toBeInTheDocument();
  });

  it('a failed background scan never blocks password entry (best-effort only)', async () => {
    vi.spyOn(api.network, 'wifiScan').mockRejectedValue(new Error('not connected'));
    renderWithProviders(<WifiPasswordRequestCard payload={{ ssid: 'iPhone-Sam' }} />);
    // No crash, connect still reachable once a password is typed.
    const user = userEvent.setup();
    await user.type(screen.getByPlaceholderText(/wi-?fi password/i), 'x');
    expect(screen.getByRole('button', { name: /connect/i })).toBeEnabled();
  });
});
