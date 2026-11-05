import { afterEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { AdditionalFactors } from './AdditionalFactors';
import * as transport from '@/lib/mfa-api';
import * as passkey from '@/lib/passkey';

afterEach(() => vi.restoreAllMocks());
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
