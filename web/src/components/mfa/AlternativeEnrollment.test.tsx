import { afterEach, expect, it, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AlternativeEnrollment } from './AlternativeEnrollment';

afterEach(() => vi.unstubAllGlobals());
it('keeps unavailable email enrollment disabled with setup guidance', () => {
  renderWithProviders(<AlternativeEnrollment factor="email" enrollmentToken="enroll" emailAvailable={false} emailUnavailableReason="no_smtp_no_relay" onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Send code' })).toBeDisabled();
  expect(screen.getByText(/server has no email delivery configured/)).toHaveTextContent('SMTP');
});
it('explains an unsupported browser when passkey enrollment is disabled over HTTPS', () => {
  vi.stubGlobal('isSecureContext', true);
  vi.stubGlobal('PublicKeyCredential', undefined);
  renderWithProviders(<AlternativeEnrollment factor="passkey" enrollmentToken="enroll" onComplete={vi.fn()} onCancel={vi.fn()} />);
  expect(screen.getByRole('button', { name: 'Confirm' })).toBeDisabled();
  expect(screen.getByText(/This browser does not support passkeys/)).toHaveTextContent('WebAuthn');
});
