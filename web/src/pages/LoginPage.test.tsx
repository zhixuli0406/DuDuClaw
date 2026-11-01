import { useBrandStore } from '@/stores/brand-store';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';

/**
 * WP5.1 — LoginPage Multica migration smoke test. Locks in the centered-card
 * layout on the app-shell surface and the three-mode form (password default).
 */
beforeEach(() => {
    useBrandStore.getState().setBrand(null);
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
  it('uses an effective distributor brand without customer-side branding controls', async () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner Desk', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    renderWithProviders(<LoginPage />);
    expect(await screen.findByText('Partner Desk')).toBeInTheDocument();
    expect(screen.getByRole('img', { name: 'Partner Desk' })).toHaveAttribute('src', 'data:image/png;base64,iVBORw0KGgo=');
    expect(screen.getByText('Sign in to your account')).toBeInTheDocument();
    expect(screen.queryByText(/change brand/i)).not.toBeInTheDocument();
  });

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

describe('<LoginPage> first-run claim', () => {
  it('asks for the owner email and sends it with the password', async () => {
    const firstRunClaim = vi.fn(async () => undefined);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    renderWithProviders(<LoginPage />);

    const email = await screen.findByLabelText('Your email');
    const submit = screen.getByRole('button', { name: 'Set password & sign in' });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    // No email yet → cannot submit.
    expect(submit).toBeDisabled();

    fireEvent.change(email, { target: { value: 'owner@example.com' } });
    expect(submit).toBeEnabled();
    fireEvent.click(submit);

    await waitFor(() =>
      expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw'),
    );
    expect(screen.queryByPlaceholderText('admin@local')).not.toBeInTheDocument();
  });
});
