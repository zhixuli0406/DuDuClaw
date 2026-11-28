import { memberGoogleSignin } from '@/lib/google-signin';
import { afterEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AdditionalFactors } from './AdditionalFactors';
import * as transport from '@/lib/mfa-api';
import * as passkey from '@/lib/passkey';

afterEach(() => { vi.restoreAllMocks(); vi.unstubAllGlobals(); });
it('preserves the password and displays the independent proof reference before starting a passkey ceremony', async () => {
  vi.spyOn(passkey, 'passkeyAvailable').mockReturnValue(true);
  const create = vi.spyOn(passkey, 'createPasskey');
  const request = vi.spyOn(transport, 'accountRequest')
    .mockResolvedValueOnce({ passkeys: [], available: true })
    .mockResolvedValueOnce({ enrollment_token: 'proof-token', challenge_ref: 'abcdef123456', proof_required: true })
    .mockImplementationOnce(() => new Promise(() => {}));
  renderWithProviders(<AdditionalFactors status={{ enabled: false, enforced: true, factors: { totp: false, email: false, passkey: false }, email_available: false }} onChange={vi.fn().mockResolvedValue(undefined)} />);
  fireEvent.change(screen.getByLabelText('Password to change verification methods'), { target: { value: 'password' } });
  fireEvent.change(screen.getByLabelText('Passkey name'), { target: { value: 'My laptop' } });
  await waitFor(() => expect(screen.getByRole('button', { name: 'Add a passkey' })).toBeEnabled());
  fireEvent.click(screen.getByRole('button', { name: 'Add a passkey' }));
  expect(await screen.findByText('abcdef123456')).toBeInTheDocument();
  expect(screen.getByLabelText('Password to change verification methods')).toHaveValue('password');
  expect(create).not.toHaveBeenCalled();
  fireEvent.change(screen.getByLabelText('Channel or Admin enrollment code'), { target: { value: 'proof' } });
  fireEvent.click(screen.getByRole('button', { name: 'Add a passkey' }));
  await waitFor(() => expect(request).toHaveBeenLastCalledWith('/api/mfa/passkey/register/begin', { password: 'password', name: 'My laptop', enrollment_token: 'proof-token', proof_code: 'proof' }));
});

it('explains why email and passkey enrollment are disabled in account settings', async () => {
  vi.stubGlobal('isSecureContext', false);
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  renderWithProviders(<AdditionalFactors status={{ enabled: false, enforced: false, factors: { totp: false, email: false, passkey: false }, email_available: false, email_unavailable_reason: 'no_smtp' }} onChange={vi.fn()} />);
  await waitFor(() => expect(screen.getByRole('button', { name: 'Add a passkey' })).toBeDisabled());
  expect(screen.getByRole('button', { name: 'Set up email verification' })).toBeDisabled();
  expect(screen.getByText(/server has no outgoing email/)).toHaveTextContent('SMTP');
  expect(screen.getByText(/Passkeys require HTTPS or localhost/)).toBeInTheDocument();
});

it('explains an administrator-disabled relay in account settings', async () => {
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  renderWithProviders(<AdditionalFactors status={{ enabled: false, enforced: false, factors: { totp: false, email: false, passkey: false }, email_available: false, email_unavailable_reason: 'relay_disabled' }} onChange={vi.fn()} />);
  expect(await screen.findByText(/administrator has disabled the licensed email relay/)).toHaveTextContent('SMTP');
  expect(screen.getByRole('button', { name: 'Set up email verification' })).toBeDisabled();
});

it('shows the installation key fingerprint while the relay awaits console approval', async () => {
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  renderWithProviders(<AdditionalFactors status={{ enabled: false, enforced: false, factors: { totp: false, email: false, passkey: false }, email_available: false, email_unavailable_reason: 'relay_key_unbound', relay_key_fingerprint: '1a2b-3c4d-5e6f-7a8b' }} onChange={vi.fn()} />);
  expect(await screen.findByText(/approve this installation in the license console/)).toBeInTheDocument();
  expect(screen.getByTestId('relay-key-fingerprint')).toHaveTextContent('Installation key fingerprint: 1a2b-3c4d-5e6f-7a8b');
});

it('removes email without a password and shows a missing Google proof refusal', async () => {
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  const message = '請先用公司 Google 帳號重新驗證身分；無法驗證時，請管理員重設兩步驟驗證。';
  const remove = vi.spyOn(transport.mfaApi, 'emailRemove').mockRejectedValue({ code: 'reauth', message });
  renderWithProviders(<AdditionalFactors hasPassword={false} status={{ enabled: true, enforced: false, factors: { totp: false, email: true, passkey: false } }} onChange={vi.fn()} />);
  expect(screen.queryByLabelText('Password to change verification methods')).toBeNull();
  const button = screen.getByRole('button', { name: 'Remove email verification' });
  expect(button).toBeEnabled(); fireEvent.click(button);
  await waitFor(() => expect(remove).toHaveBeenCalledWith(''));
  expect(await screen.findByRole('alert')).toHaveTextContent(message);
});

it.each([
  ['https://accounts.google.com/o/oauth2/auth', true],
  ['https://accounts.google.com.evil.test/auth', false],
  ['http://accounts.google.com/auth', false],
  ['https://accounts.google.com:444/auth', false],
  ['https://user:secret@accounts.google.com/auth', false],
  ['/relative', false],
])('validates the reauth destination %s', async (url, allowed) => {
  const assign = vi.fn();
  vi.stubGlobal('location', { ...window.location, assign });
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  const start = vi.spyOn(memberGoogleSignin, 'reauthStart').mockResolvedValue({ authorization_url: url });
  renderWithProviders(<AdditionalFactors hasPassword={false} status={{ enabled: false, enforced: false }} onChange={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Re-authenticate with company Google account' }));
  await waitFor(() => expect(start).toHaveBeenCalledTimes(1));
  if (allowed) await waitFor(() => expect(assign).toHaveBeenCalledWith(url));
  else { expect(await screen.findByRole('alert')).toBeInTheDocument(); expect(assign).not.toHaveBeenCalled(); }
});

it('displays a Google reauth start refusal', async () => {
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  const message = 'Google 未提供近期身分驗證紀錄，請聯絡管理員重設兩步驟驗證。';
  vi.spyOn(memberGoogleSignin, 'reauthStart').mockRejectedValue({ code: 'signin_reauth_unavailable', message });
  renderWithProviders(<AdditionalFactors hasPassword={false} status={{ enabled: false, enforced: false }} onChange={vi.fn()} />);
  fireEvent.click(screen.getByRole('button', { name: 'Re-authenticate with company Google account' }));
  expect(await screen.findByRole('alert')).toHaveTextContent(message);
});

it('requires a password to remove email on password accounts', async () => {
  vi.spyOn(transport, 'accountRequest').mockResolvedValue({ passkeys: [], available: true });
  renderWithProviders(<AdditionalFactors status={{ enabled: true, enforced: false, factors: { totp: false, email: true, passkey: false } }} onChange={vi.fn()} />);
  const button = screen.getByRole('button', { name: 'Remove email verification' });
  expect(button).toBeDisabled();
  fireEvent.change(screen.getByLabelText('Password to change verification methods'), { target: { value: 'password' } });
  expect(button).toBeEnabled();
});
