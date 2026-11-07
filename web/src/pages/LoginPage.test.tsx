import { useBrandStore } from '@/stores/brand-store';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';
import { useLocation } from 'react-router';

function LocationProbe() {
  const location = useLocation();
  return <span data-testid="location">{location.pathname}</span>;
}

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
    renderWithProviders(<><LoginPage /><LocationProbe /></>);
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

  it('shows MFA enrollment after a successful first-run claim without navigating', async () => {
    const challenge = { mfa_enrollment_required: true as const, enrollment_token: 't', challenge_ref: 'r' };
    const firstRunClaim = vi.fn(async () => challenge);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    const { container } = renderWithProviders(<><LoginPage /><LocationProbe /></>);

    const initialPath = screen.getByTestId('location').textContent;
    fireEvent.change(await screen.findByLabelText('Your email'), { target: { value: 'owner@example.com' } });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password & sign in' }));

    await screen.findByLabelText('Channel or Admin enrollment code');
    expect(container.textContent).toContain('Enrollment reference: r');
    expect(screen.getByText('r')).toBeInTheDocument();
    expect(screen.getByLabelText('Channel or Admin enrollment code')).toBeInTheDocument();
    expect(screen.queryByLabelText('Your email')).not.toBeInTheDocument();
    expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw');
    expect(screen.getByTestId('location')).toHaveTextContent(initialPath ?? '');
  });

  it('navigates after a first-run claim with no MFA challenge', async () => {
    const firstRunClaim = vi.fn(async () => undefined);
    useAuthStore.setState({
      firstRunStatus: (async () => true) as never,
      firstRunClaim: firstRunClaim as never,
    });
    renderWithProviders(<><LoginPage /><LocationProbe /></>);

    fireEvent.change(await screen.findByLabelText('Your email'), { target: { value: 'owner@example.com' } });
    fireEvent.change(screen.getByLabelText('Set a password'), { target: { value: 'a-strong-pw' } });
    fireEvent.change(screen.getByLabelText('Confirm password'), { target: { value: 'a-strong-pw' } });
    fireEvent.click(screen.getByRole('button', { name: 'Set password & sign in' }));

    await waitFor(() => expect(firstRunClaim).toHaveBeenCalledWith('owner@example.com', 'a-strong-pw'));
    await waitFor(() => expect(screen.getByTestId('location')).toHaveTextContent('/'));
  });
});

it('keeps persistence opt-in and passes the checked choice to password login', async () => {
  const login = vi.fn().mockResolvedValue({ mfa_required: true, challenge_id: 'pending' });
  useAuthStore.setState({ login, tryLocalSession: vi.fn().mockResolvedValue(false) });
  renderWithProviders(<LoginPage />);
  const checkbox = await screen.findByLabelText('Keep me signed in (180 days by default)');
  expect(checkbox).not.toBeChecked();
  fireEvent.click(checkbox);
  fireEvent.change(screen.getByLabelText('Email'), { target: { value: 'owner@example.com' } });
  fireEvent.change(screen.getByLabelText('Password'), { target: { value: 'current-password' } });
  fireEvent.click(screen.getByRole('button', { name: 'Sign In' }));
  await waitFor(() => expect(login).toHaveBeenCalledWith('owner@example.com', 'current-password', true));
});
