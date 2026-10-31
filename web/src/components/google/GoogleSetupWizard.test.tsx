import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useAuthStore } from '@/stores/auth-store';
import { api } from '@/lib/api';
import { GoogleSetupWizard, GOOGLE_SETUP_STORAGE } from './GoogleSetupWizard';

beforeEach(() => {
  vi.restoreAllMocks(); sessionStorage.clear();
  useAuthStore.setState({ user: { id: 'admin', email: 'admin@example.com', display_name: 'Admin', role: 'admin', status: 'active' } });
  vi.spyOn(api.mcp, 'oauthProviders').mockResolvedValue({ providers: [{ provider_id: 'google', name: 'Google', configured: false, auth_url: '', scopes: [], token_status: 'none', expires_at: null, redirect_uri: 'http://localhost:9999/api/mcp/oauth/callback' }] });
  vi.spyOn(api.mcp, 'oauthStatus').mockResolvedValue({ authenticated: false, expires_at: null });
  vi.spyOn(api.googleCredentials, 'get').mockResolvedValue({ integration_enabled: true, effective: 'none', required_scopes: [], service_account: { configured: false, error: '' }, apps_script: { configured: false, error: '' } });
});
const renderWizard = () => renderWithProviders(<GoogleSetupWizard open onOpenChange={vi.fn()} />);

describe('Google setup wizard', () => {
  it.each([
    ['Company Workspace with a super administrator', /Create a service account and a JSON key/],
    ['Company Workspace with my own OAuth client', /Internal allows only organization members/],
    ['Personal Gmail: mail, calendar and spreadsheets', /three services and eight actions/],
    ['Personal Gmail: full Google tools', /Testing authorization expires after seven days/],
  ])('guides the %s path', async (name, text) => {
    renderWizard(); await userEvent.click(screen.getByRole('button', { name }));
    expect(screen.getByText(text)).toBeInTheDocument();
    const saved = JSON.parse(sessionStorage.getItem(GOOGLE_SETUP_STORAGE)!);
    expect(Object.keys(saved).sort()).toEqual(['path', 'step', 'testing']);
  });
  it('resumes an OAuth path with a visible actual redirect URI', async () => {
    sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 3, testing: true, secret: 'must-never-persist' }));
    renderWizard();
    expect(screen.getByText(/credentials are saved/i)).toBeInTheDocument();
    expect(await screen.findByText('http://localhost:9999/api/mcp/oauth/callback')).toBeInTheDocument();
    expect(sessionStorage.getItem(GOOGLE_SETUP_STORAGE)).not.toContain('secret');
  });
  it('shows OAuth capabilities and the actual URI in the credential setup instruction', async () => {
    sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 2, testing: true }));
    renderWizard();
    expect(await screen.findByText('Search your inbox by sender, subject, or keyword')).toBeInTheDocument();
    const instruction = screen.getByText('Add this authorized redirect URI:').closest('li');
    expect(instruction).toHaveTextContent('http://localhost:9999/api/mcp/oauth/callback');
  });

  it('shows a failed service and a safe enable link, without allowing completion', async () => {
    sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 4, testing: true }));
    vi.spyOn(api.googleCredentials, 'test').mockResolvedValue({ ok: false, path: 'direct', detail: 'services_checked', services: [{ service: 'Gmail', ok: false, verified: true, reason: 'service_disabled', enable_url: 'https://console.cloud.google.com/apis/library/gmail.googleapis.com' }] });
    renderWizard(); await userEvent.click(screen.getByRole('button', { name: 'Run service checks' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('Service check failed');
    expect(screen.getByRole('link', { name: 'Enable this API' })).toHaveAttribute('href', 'https://console.cloud.google.com/apis/library/gmail.googleapis.com');
    expect(screen.getByRole('button', { name: 'Done' })).toBeDisabled();
  });
  it('labels scope-only checks and rejects an untrusted enable URL', async () => {
    sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 4, testing: true }));
    vi.spyOn(api.googleCredentials, 'test').mockResolvedValue({ ok: false, result: 'partial', path: 'direct', account: 'owner@example.test', detail: 'services_checked', services: ['gmail','calendar','drive','sheets','docs'].map(service => ({ service, ok: true, verified: !['sheets','docs'].includes(service), reason: 'scope_granted', enable_url: 'https://console.cloud.google.com.evil.example/' })) });
    renderWizard(); await userEvent.click(screen.getByRole('button', { name: 'Run service checks' }));
    expect(await screen.findAllByText('Document reads have not been checked.')).toHaveLength(2);
    expect(screen.queryByRole('link', { name: 'Enable this API' })).not.toBeInTheDocument();
    expect(await screen.findByText(/Authenticated account: owner@example.test/)).toBeInTheDocument();
    expect(screen.getByRole('status')).toHaveTextContent('Google connection setup is complete');
    await userEvent.click(screen.getByRole('button', { name: 'Done' }));
    expect(screen.getByText(/Google connection setup is complete/)).toBeInTheDocument();
    expect(screen.getByText(/Sheets and Docs have permission granted; documents have not been read/)).toBeInTheDocument();
    expect(screen.getAllByText('Document reads have not been checked.')).toHaveLength(2);
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    await userEvent.click(screen.getByRole('button', { name: 'Back' }));
    await userEvent.click(screen.getByRole('button', { name: 'Next' }));
    await waitFor(() => expect(screen.getByRole('button', { name: 'Done' })).toBeDisabled());
  });
  it('does not offer the wizard to a non-admin', () => {
    useAuthStore.setState({ user: { id: 'employee', email: 'employee@example.com', display_name: 'Employee', role: 'employee', status: 'active' } });
    renderWizard(); expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });
});

it('persists the Testing answer for an existing OAuth connection without reauthorization', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 2, testing: true }));
  vi.mocked(api.googleCredentials.get).mockResolvedValue({ integration_enabled: true, effective: 'oauth', required_scopes: [], service_account: { configured: false, error: '' }, apps_script: { configured: false, error: '' } });
  const save = vi.spyOn(api.googleCredentials, 'set').mockResolvedValue({ ok: true, mode: 'oauth' });
  const start = vi.spyOn(api.mcp, 'oauthStart');
  renderWizard();
  await userEvent.click(await screen.findByRole('button', { name: 'I have connected; check the services' }));
  await waitFor(() => expect(save).toHaveBeenCalledWith({ mode: 'oauth', testing: true }));
  expect(start).not.toHaveBeenCalled();
  expect(screen.getByText(/credentials are saved/i)).toBeInTheDocument();
});

it('shows an unknown reason as escaped diagnostic text', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 4, testing: true }));
  vi.spyOn(api.googleCredentials, 'test').mockResolvedValue({ ok: false, path: 'direct', detail: 'service_probe_results', services: [{ service: 'Gmail', ok: false, verified: false, reason: '<img src=x onerror=alert(1)>', enable_url: null }] });
  renderWizard();
  await userEvent.click(screen.getByRole('button', { name: 'Run service checks' }));
  expect(await screen.findByText('<img src=x onerror=alert(1)>')).toBeInTheDocument();
  expect(document.body.querySelector('img')).toBeNull();
});

it('requires fresh OAuth authorization before checking a different active path', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 2, testing: true }));
  vi.mocked(api.googleCredentials.get).mockResolvedValue({ integration_enabled: true, effective: 'apps_script', required_scopes: [], service_account: { configured: false, error: '' }, apps_script: { configured: true, error: '' } });
  const save = vi.spyOn(api.googleCredentials, 'set');
  renderWizard();
  await userEvent.click(await screen.findByRole('button', { name: 'I have connected; check the services' }));
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  expect(save).not.toHaveBeenCalled();
});

it('does not accept a legacy success that only checked scopes', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 4, testing: true }));
  vi.spyOn(api.googleCredentials, 'test').mockResolvedValue({ ok: true, path: 'direct', account: 'owner@example.test', detail: 'service_probe_results', services: ['gmail', 'calendar', 'drive', 'sheets', 'docs'].map(service => ({ service, ok: true, verified: false, reason: 'scope_granted' })) });
  renderWizard();
  await userEvent.click(screen.getByRole('button', { name: 'Run service checks' }));
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Done' })).toBeDisabled();
});

it('does not offer a guessed redirect URI when provider loading fails', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 3, testing: true }));
  vi.mocked(api.mcp.oauthProviders).mockRejectedValue(new Error('offline'));
  renderWizard();
  expect(await screen.findByText('The redirect URI is unavailable. Reopen setup to try loading it again.')).toBeInTheDocument();
  expect(await screen.findByRole('alert')).toBeInTheDocument();
  expect(screen.getByRole('button', { name: 'Copy' })).toBeDisabled();
  expect(screen.queryByText('http://localhost:3000/api/mcp/oauth/callback')).not.toBeInTheDocument();
});

it('labels the OAuth scope list with the Google field where it must be pasted', async () => {
  sessionStorage.setItem(GOOGLE_SETUP_STORAGE, JSON.stringify({ path: 'external', step: 1, testing: true }));
  vi.mocked(api.googleCredentials.get).mockResolvedValue({ integration_enabled: true, effective: 'none', required_scopes: ['scope-a', 'scope-b'], service_account: { configured: false, error: '' }, apps_script: { configured: false, error: '' } });
  renderWizard();
  expect(await screen.findByText('scope-a,scope-b')).toBeInTheDocument();
  expect(screen.getByText(/^Paste this complete scope list into .*Manually add scopes/)).toBeInTheDocument();
});
