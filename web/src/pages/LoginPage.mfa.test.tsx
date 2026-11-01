import { beforeEach, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';

beforeEach(() => {
  useAuthStore.setState({ loading: false, firstRunStatus: async () => false,
    tryLocalSession: async () => false, login: vi.fn(), otpRequest: vi.fn(), otpVerify: vi.fn() });
});
it('keeps the unenrolled password form and forwards credentials unchanged', async () => {
  const login = vi.fn().mockResolvedValue(undefined); useAuthStore.setState({ login });
  renderWithProviders(<LoginPage />);
  fireEvent.change(await screen.findByLabelText('Email'), { target: { value: 'user@example.com' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
  await waitFor(() => expect(login).toHaveBeenCalledWith('user@example.com', 'password'));
  expect(screen.queryByText('Verify your sign-in')).toBeNull();
});
it('moves password login to the MFA code step and supports restarting', async () => {
  useAuthStore.setState({ login: vi.fn().mockResolvedValue({ mfa_required: true, challenge_id: 'mfa' }) });
  renderWithProviders(<LoginPage />);
  fireEvent.change(await screen.findByLabelText('Email'), { target: { value: 'user@example.com' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
  expect(await screen.findByText('Verify your sign-in')).toBeInTheDocument();
  expect(screen.queryByLabelText('Password')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Restart sign-in' }));
  expect(screen.getByLabelText('Password')).toHaveValue('');
});
it('moves channel OTP verification to mandatory enrollment without treating it as login success', async () => {
  useAuthStore.setState({ otpRequest: vi.fn().mockResolvedValue({ challenge_id: 'otp' }),
    otpVerify: vi.fn().mockResolvedValue({ mfa_enrollment_required: true, enrollment_token: 'enroll', challenge_ref: 'challenge-ref' }) });
  renderWithProviders(<LoginPage />);
  fireEvent.click(await screen.findByRole('button', { name: 'Log in with a channel code instead' }));
  fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'user@example.com' } });
  // Use the form's submit button independently of the localized OTP wording.
  fireEvent.submit(screen.getByLabelText('Email').closest('form')!);
  const input = await screen.findByLabelText('Verification code');
  fireEvent.change(input, { target: { value: '123456' } });
  fireEvent.submit(input.closest('form')!);
  expect(await screen.findByText('challenge-ref')).toBeInTheDocument();
  expect(screen.getByLabelText('Current password')).toBeInTheDocument();
  expect(screen.getByLabelText('Channel or Admin enrollment code')).toBeInTheDocument();
});
