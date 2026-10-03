import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { useBrandingStore } from '@/lib/branding';
import { LoginPage } from './LoginPage';

// v1.68 (W2): 品牌 → 副標題 used to be saved and rendered nowhere.
beforeEach(() => {
  useAuthStore.setState({
    loading: false,
    firstRunStatus: (async () => false) as never,
    login: vi.fn(async () => undefined) as never,
    otpRequest: vi.fn() as never,
    otpVerify: vi.fn() as never,
    firstRunClaim: vi.fn() as never,
  });
});

describe('<LoginPage> branding subtitle', () => {
  it('shows the distributor subtitle in place of the default tagline', async () => {
    useBrandingStore.setState({ branding: { product_name: 'Acme Desk', subtitle: '  Your office helper ' } as never });
    renderWithProviders(<LoginPage />);
    expect(await screen.findByTestId('login-subtitle')).toHaveTextContent('Your office helper');
    expect(screen.queryByText('Sign in to your account')).not.toBeInTheDocument();
  });

  it('falls back to the default tagline when no subtitle is set', async () => {
    useBrandingStore.setState({ branding: { product_name: 'Acme Desk', subtitle: '' } as never });
    renderWithProviders(<LoginPage />);
    expect(await screen.findByTestId('login-subtitle')).toHaveTextContent('Sign in to your account');
  });
});
