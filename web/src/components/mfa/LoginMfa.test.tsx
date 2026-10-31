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
    expect(establishSession).toHaveBeenCalledWith(tokens);
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
    await waitFor(() => expect(establishSession).toHaveBeenCalledWith(tokens));
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
