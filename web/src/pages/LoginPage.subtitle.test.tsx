import { useCompanyStore } from '@/stores/company-store';
import { useBrandStore } from '@/stores/brand-store';
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { LoginPage } from './LoginPage';

// Customer identity is separate from the signed software identity.
beforeEach(() => {
  useCompanyStore.getState().setCompany(null);
    useBrandStore.getState().setBrand(null);
  useAuthStore.setState({
    loading: false,
    firstRunStatus: (async () => false) as never,
    login: vi.fn(async () => undefined) as never,
    otpRequest: vi.fn() as never,
    otpVerify: vi.fn() as never,
    firstRunClaim: vi.fn() as never,
  });
});

describe('<LoginPage> software identity', () => {
  it('shows the company above sign-in and keeps the product in the footer', async () => {
    useBrandStore.getState().setBrand({ product_name: 'Partner Desk', logo_light: 'data:image/png;base64,iVBORw0KGgo=' });
    const logo = `/api/company/logo/${'a'.repeat(64)}.png`;
    useCompanyStore.getState().setCompany({ name: 'Acme Team', logo_url: logo });
    renderWithProviders(<LoginPage />);
    expect(await screen.findByRole('heading', { name: 'Acme Team' })).toBeInTheDocument();
    expect(screen.getByRole('img', { name: 'Acme Team' })).toHaveAttribute('src', logo);
    expect(screen.getByText('Partner Desk')).toBeInTheDocument();
  });

  it('keeps DuDuClaw identity and tagline when arbitrary cached branding is present', async () => {
    localStorage.setItem('duduclaw-branding-cache', JSON.stringify({branding: {product_name: 'Acme Desk', subtitle: 'Your office helper', logo_data_uri: 'data:image/png;base64,AAAA'}, white_label_active: true}));
    renderWithProviders(<LoginPage />);
    expect(await screen.findByTestId('login-subtitle')).toHaveTextContent('Sign in to your account');
    expect(screen.getByText('DuDuClaw')).toBeInTheDocument();
    expect(screen.queryByText('Acme Desk')).not.toBeInTheDocument();
    expect(document.querySelector('img[src="data:image/png;base64,AAAA"]')).toBeNull();
  });

  it('falls back to the default tagline when no subtitle is set', async () => {
    localStorage.clear();
    renderWithProviders(<LoginPage />);
    expect(await screen.findByTestId('login-subtitle')).toHaveTextContent('Sign in to your account');
  });
});
