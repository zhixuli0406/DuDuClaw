import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';

/**
 * WP5.1 — LoginPage Multica migration smoke test. Locks in the centered-card
 * layout on the app-shell surface and the three-mode form (password default).
 */
beforeEach(() => {
  useAuthStore.setState({
    loading: false,
    // Not a first-run instance → the password form is shown.
    firstRunStatus: (async () => false) as never,
    login: vi.fn(async () => undefined) as never,
    otpRequest: vi.fn() as never,
    otpVerify: vi.fn() as never,
    firstRunClaim: vi.fn() as never,
  });
});

describe('<LoginPage>', () => {
  it('renders the brand seal, subtitle and password sign-in form', async () => {
    renderWithProviders(<LoginPage />);

    // Product name + subtitle (§5.8 login card).
    expect(await screen.findByText('DuDuClaw')).toBeInTheDocument();
    expect(screen.getByText('Sign in to your account')).toBeInTheDocument();

    // Email + password fields and the brand submit button.
    expect(screen.getByLabelText('Email')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Sign In' })).toBeInTheDocument();
  });
});
