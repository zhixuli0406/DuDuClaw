import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore, ApiError } from '@/stores/auth-store';
import { mfaApi } from '@/lib/mfa-api';
import { LoginMfa } from './LoginMfa';

beforeEach(() => { vi.restoreAllMocks(); });
const challenge = { mfa_required: true as const, challenge_id: 'challenge' };
const tokens = { access_token: 'access', refresh_token: 'refresh' };

describe('MFA login', () => {
  it('shows the hint for the selected authenticator or recovery code mode', () => {
    renderWithProviders(<LoginMfa challenge={challenge} onComplete={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByText('Enter a code from your authenticator. This sign-in request expires in 5 minutes and allows up to 5 attempts.')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Use a recovery code' }));
    expect(screen.getByText('Enter an unused recovery code. This sign-in request expires in 5 minutes and allows up to 5 attempts.')).toBeInTheDocument();
    expect(screen.queryByText('Enter a code from your authenticator. This sign-in request expires in 5 minutes and allows up to 5 attempts.')).not.toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Use an authenticator code' }));
    expect(screen.getByText('Enter a code from your authenticator. This sign-in request expires in 5 minutes and allows up to 5 attempts.')).toBeInTheDocument();
    expect(screen.queryByText('Enter an unused recovery code. This sign-in request expires in 5 minutes and allows up to 5 attempts.')).not.toBeInTheDocument();
  });

  it.each([false, true])('verifies the selected factor (recovery=%s) before establishing a session', async recovery => {
    const verify = vi.spyOn(mfaApi, 'verify').mockResolvedValue(tokens);
    const establishSession = vi.fn().mockResolvedValue(undefined);
    const done = vi.fn(); useAuthStore.setState({ establishSession });
    renderWithProviders(<LoginMfa challenge={challenge} onComplete={done} onCancel={vi.fn()} />);
    if (recovery) fireEvent.click(screen.getByRole('button', { name: 'Use a recovery code' }));
    fireEvent.change(screen.getByLabelText(recovery ? 'Recovery code' : 'Authenticator code'), { target: { value: recovery ? 'unused-recovery' : '123456' } });
    expect(establishSession).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'Verify & sign in' }));
    await waitFor(() => expect(done).toHaveBeenCalled());
    expect(verify).toHaveBeenCalledWith('challenge', recovery ? 'unused-recovery' : '123456', recovery);
    expect(establishSession).toHaveBeenCalledWith(tokens, useAuthStore.getState().authAttempt, expect.any(Function));
  });

  it.each([401, 429])('shows localized failure for HTTP %s without logging in', async status => {
    vi.spyOn(mfaApi, 'verify').mockRejectedValue(new ApiError(status, 'raw secret detail'));
    const done = vi.fn();
    renderWithProviders(<LoginMfa challenge={challenge} onComplete={done} onCancel={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Authenticator code'), { target: { value: '123456' } });
    fireEvent.click(screen.getByRole('button', { name: 'Verify & sign in' }));
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent(status === 401 ? 'expired' : 'Too many attempts');
    expect(alert).not.toHaveTextContent('raw secret detail'); expect(done).not.toHaveBeenCalled();
  });

  it('uses HTTP enrollment and delays session persistence until recovery codes are acknowledged', async () => {
    const enrollment = { mfa_enrollment_required: true as const, enrollment_token: 'enroll', challenge_ref: 'ref' };
    const begin = vi.spyOn(mfaApi, 'httpBegin').mockResolvedValue({ enrollment_token: 'enroll', secret: 'SECRET', otpauth_uri: 'otpauth://totp/test?secret=SECRET' });
    vi.spyOn(mfaApi, 'httpConfirm').mockResolvedValue({ ...tokens, recovery_codes: ['one-use'] });
    const establishSession = vi.fn().mockResolvedValue(undefined); useAuthStore.setState({ establishSession });
    const local = vi.spyOn(Storage.prototype, 'setItem');
    renderWithProviders(<LoginMfa challenge={enrollment} onComplete={vi.fn()} onCancel={vi.fn()} />);
    fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'pw' } });
    fireEvent.change(screen.getByLabelText('Channel or Admin enrollment code'), { target: { value: 'proof' } });
    fireEvent.click(screen.getByRole('button', { name: 'Continue setup' }));
    await screen.findByText('SECRET');
    expect(begin).toHaveBeenCalledWith({ password: 'pw', enrollment_token: 'enroll', proof_code: 'proof' });
    fireEvent.change(screen.getByLabelText('Authenticator code'), { target: { value: '123456' } });
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    await screen.findByLabelText('Recovery codes');
    expect(establishSession).not.toHaveBeenCalled(); expect(local).not.toHaveBeenCalled();
    fireEvent.click(screen.getByLabelText('I saved my recovery codes'));
    fireEvent.click(screen.getByRole('button', { name: 'Continue' }));
    await waitFor(() => expect(establishSession).toHaveBeenCalledWith(tokens, useAuthStore.getState().authAttempt, expect.any(Function)));
  });
});

it('does not establish a session when verification finishes after leaving the MFA page', async () => {
  let resolve!: (value: typeof tokens) => void;
  vi.spyOn(mfaApi, 'verify').mockReturnValue(new Promise(r => { resolve = r; }));
  const establishSession = vi.fn(); useAuthStore.setState({ establishSession });
  const { unmount } = renderWithProviders(<LoginMfa challenge={challenge} onComplete={vi.fn()} onCancel={vi.fn()} />);
  fireEvent.change(screen.getByLabelText('Authenticator code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Verify & sign in' }));
  unmount(); resolve(tokens);
  await Promise.resolve();
  expect(establishSession).not.toHaveBeenCalled();
});

it('ignores verification after logout invalidates the mounted login attempt', async () => {
  let resolve!: (value: typeof tokens) => void;
  vi.spyOn(mfaApi, 'verify').mockReturnValue(new Promise(r => { resolve = r; }));
  const establishSession = vi.fn(); useAuthStore.setState({ establishSession });
  const done = vi.fn();
  renderWithProviders(<LoginMfa challenge={challenge} onComplete={done} onCancel={vi.fn()} />);
  fireEvent.change(screen.getByLabelText('Authenticator code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Verify & sign in' }));
  useAuthStore.getState().logout();
  resolve(tokens);
  await waitFor(() => expect(screen.getByLabelText('Authenticator code')).toHaveValue(''));
  expect(establishSession).not.toHaveBeenCalled();
  expect(done).not.toHaveBeenCalled();
});

it('selects the last-used enrolled email factor and sends only the challenge-bound code', async () => {
  const send = vi.spyOn(mfaApi, 'emailSend').mockResolvedValue({ sent: true, expires_in: 600 });
  const verify = vi.spyOn(mfaApi, 'emailVerify').mockResolvedValue(tokens);
  const establishSession = vi.fn().mockResolvedValue(true);
  useAuthStore.setState({ establishSession });
  const done = vi.fn();
  renderWithProviders(<LoginMfa challenge={{ ...challenge, factors: { totp: true, email: true, passkey: false }, last_used: 'email', email_available: true }} onComplete={done} onCancel={vi.fn()} />);
  expect(screen.getByRole('combobox')).toHaveValue('email');
  fireEvent.click(screen.getByRole('button', { name: 'Send code' }));
  await waitFor(() => expect(send).toHaveBeenCalledWith({ challenge_id: 'challenge' }));
  expect(await screen.findByRole('button', { name: /Resend code/ })).toBeDisabled();
  fireEvent.change(screen.getByLabelText('Email verification code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Verify & sign in' }));
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(verify).toHaveBeenCalledWith({ challenge_id: 'challenge', code: '123456' });
});

it('cannot invoke email delivery when the server reports unavailable SMTP', async () => {
  const send = vi.spyOn(mfaApi, 'emailSend');
  renderWithProviders(<LoginMfa challenge={{ ...challenge, factors: { totp: true, email: true, passkey: false }, last_used: 'email', email_available: false }} onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Send code' })).toBeDisabled();
  expect(send).not.toHaveBeenCalled();
});
