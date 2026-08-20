import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SubscriptionSetupWizard } from './SubscriptionSetupWizard';

/**
 * WP-D: the "訂閱帳號" device-code-style setup wizard. Covers the three
 * states a headless-box operator actually walks through — start (URL + QR
 * appear), submit success, submit failure with a structured error code — plus
 * cancel-on-close. Backend RPC contract: `accounts.setup_token_*`
 * (`handlers.rs`); pure-function/live coverage for the PTY orchestration
 * itself lives in `setup_token_wizard.rs`.
 */

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.subscribe.mockReturnValue(vi.fn());
});

function mockCall(handlers: Record<string, (params?: unknown) => unknown>) {
  mockWsClient.call.mockImplementation((method: string, params?: unknown) => {
    const h = handlers[method];
    if (!h) return Promise.reject(new Error(`unexpected call: ${method}`));
    return Promise.resolve(h(params));
  });
}

describe('SubscriptionSetupWizard', () => {
  it('starts the flow on open and shows the authorize link + QR code', async () => {
    mockCall({
      'accounts.setup_token_start': () => ({
        session_id: 'sess-1',
        auth_url: 'https://claude.com/cai/oauth/authorize?code=true&state=abc',
        expires_in_seconds: 300,
        program: '/usr/local/bin/claude',
      }),
    });

    renderWithProviders(<SubscriptionSetupWizard open onClose={vi.fn()} />);

    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('accounts.setup_token_start'));
    const link = await screen.findByRole('link', { name: /open authorization page/i });
    expect(link).toHaveAttribute('href', 'https://claude.com/cai/oauth/authorize?code=true&state=abc');
    // Code input is ready for paste-back.
    expect(screen.getByPlaceholderText(/paste the code you received/i)).toBeInTheDocument();
  });

  it('polls status until the URL appears when start returns none yet', async () => {
    let statusCalls = 0;
    mockCall({
      'accounts.setup_token_start': () => ({
        session_id: 'sess-2',
        auth_url: null,
        expires_in_seconds: 300,
        program: '/usr/local/bin/claude',
      }),
      'accounts.setup_token_status': () => {
        statusCalls += 1;
        return {
          session_id: 'sess-2',
          status: 'running',
          auth_url: statusCalls >= 1 ? 'https://claude.com/cai/oauth/authorize?code=true' : null,
          expires_in_seconds: 300,
        };
      },
    });

    renderWithProviders(<SubscriptionSetupWizard open onClose={vi.fn()} />);

    expect(await screen.findByText(/waiting for the link/i)).toBeInTheDocument();
    expect(
      await screen.findByRole('link', { name: /open authorization page/i }, { timeout: 3000 }),
    ).toBeInTheDocument();
  });

  it('submits the pasted code and reports success', async () => {
    const user = userEvent.setup();
    const onSuccess = vi.fn();
    mockCall({
      'accounts.setup_token_start': () => ({
        session_id: 'sess-3',
        auth_url: 'https://claude.com/cai/oauth/authorize?code=true',
        expires_in_seconds: 300,
        program: '/usr/local/bin/claude',
      }),
      'accounts.setup_token_submit': (params) => {
        expect(params).toEqual({ session_id: 'sess-3', code: 'ABC123' });
        return { success: true, account_id: 'claude-subscription-1' };
      },
    });

    renderWithProviders(<SubscriptionSetupWizard open onClose={vi.fn()} onSuccess={onSuccess} />);

    const input = await screen.findByPlaceholderText(/paste the code you received/i);
    await user.type(input, 'ABC123');
    await user.click(screen.getByRole('button', { name: /^submit$/i }));

    expect(await screen.findByText(/subscription account connected/i)).toBeInTheDocument();
    expect(screen.getByText('claude-subscription-1')).toBeInTheDocument();
    expect(onSuccess).toHaveBeenCalledTimes(1);
  });

  it('maps a structured invalid_code error to its i18n copy and offers a restart', async () => {
    const user = userEvent.setup();
    mockCall({
      'accounts.setup_token_start': () => ({
        session_id: 'sess-4',
        auth_url: 'https://claude.com/cai/oauth/authorize?code=true',
        expires_in_seconds: 300,
        program: '/usr/local/bin/claude',
      }),
      'accounts.setup_token_submit': () =>
        Promise.reject({ code: 'invalid_code', message: 'raw backend diagnostic — must not leak' }),
    });

    renderWithProviders(<SubscriptionSetupWizard open onClose={vi.fn()} />);

    const input = await screen.findByPlaceholderText(/paste the code you received/i);
    await user.type(input, 'WRONG');
    await user.click(screen.getByRole('button', { name: /^submit$/i }));

    expect(
      await screen.findByText("That code is wrong or has expired — start over and try again."),
    ).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /start over/i })).toBeInTheDocument();
  });

  it('cancels the in-flight session when closed before completion', async () => {
    const onClose = vi.fn();
    mockCall({
      'accounts.setup_token_start': () => ({
        session_id: 'sess-5',
        auth_url: 'https://claude.com/cai/oauth/authorize?code=true',
        expires_in_seconds: 300,
        program: '/usr/local/bin/claude',
      }),
      'accounts.setup_token_cancel': (params) => {
        expect(params).toEqual({ session_id: 'sess-5' });
        return { success: true };
      },
    });

    const user = userEvent.setup();
    renderWithProviders(<SubscriptionSetupWizard open onClose={onClose} />);
    await screen.findByRole('link', { name: /open authorization page/i });

    // Two elements are named "Close" — the dialog's own icon-only [x] and
    // our explicit footer button; only the latter carries visible text.
    const closeButtons = screen.getAllByRole('button', { name: /close/i });
    const footerClose = closeButtons.find((b) => b.textContent?.trim() === 'Close');
    expect(footerClose).toBeTruthy();
    await user.click(footerClose!);

    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('accounts.setup_token_cancel', { session_id: 'sess-5' }),
    );
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
