import { afterEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AlternativeEnrollment } from './AlternativeEnrollment';
import { mfaApi } from '@/lib/mfa-api';

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
it('keeps unavailable email enrollment disabled with setup guidance', () => {
  renderWithProviders(<AlternativeEnrollment factor="email" enrollmentToken="enroll" emailAvailable={false} emailUnavailableReason="no_smtp" onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Send code' })).toBeDisabled();
  expect(screen.getByText(/server has no outgoing email/)).toHaveTextContent('SMTP');
});
it('explains an unsupported browser when passkey enrollment is disabled over HTTPS', () => {
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  renderWithProviders(<AlternativeEnrollment factor="passkey" enrollmentToken="enroll" onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeDisabled();
  expect(screen.getByText(/This browser does not support passkeys/)).toHaveTextContent('WebAuthn');
});


it('sends the independent proof for both Email enrollment steps', async () => {
  const send = vi.spyOn(mfaApi, 'emailSend').mockResolvedValue({ sent: true, expires_in: 600 });
  const tokens = { access_token: 'access', refresh_token: 'refresh' };
  const verify = vi.spyOn(mfaApi, 'emailVerify').mockResolvedValue(tokens);
  const complete = vi.fn().mockResolvedValue(undefined);
  renderWithProviders(<AlternativeEnrollment factor="email" enrollmentToken="enroll" challengeRef="ref" emailAvailable onComplete={complete} onCancel={vi.fn()} />);
  expect(screen.getByText('ref')).toBeInTheDocument();
  expect(screen.getByLabelText('Channel or Admin enrollment code')).toBeRequired();
  fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'password' } });
  fireEvent.change(screen.getByLabelText('Channel or Admin enrollment code'), { target: { value: 'proof' } });
  fireEvent.click(screen.getByRole('button', { name: 'Send code' }));
  await screen.findByRole('button', { name: 'Confirm' });
  expect(send).toHaveBeenCalledWith({ enrollment_token: 'enroll', password: 'password', proof_code: 'proof' });
  fireEvent.change(screen.getByLabelText('Email verification code'), { target: { value: '123456' } });
  fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
  await waitFor(() => expect(complete).toHaveBeenCalledWith(tokens));
  expect(verify).toHaveBeenCalledWith({ enrollment_token: 'enroll', password: 'password', proof_code: 'proof', code: '123456' });
});

it('uses an already confirmed bootstrap proof for Email without asking for another code', async () => {
  const send = vi.spyOn(mfaApi, 'emailSend').mockResolvedValue({ sent: true, expires_in: 600 });
  renderWithProviders(<AlternativeEnrollment factor="email" enrollmentToken="enroll" emailAvailable proofDone onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.queryByLabelText('Channel or Admin enrollment code')).toBeNull();
  fireEvent.change(screen.getByLabelText('Current password'), { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Send code' }));
  await waitFor(() => expect(send).toHaveBeenCalledWith({ enrollment_token: 'enroll', password: 'password', proof_code: '' }));
});
