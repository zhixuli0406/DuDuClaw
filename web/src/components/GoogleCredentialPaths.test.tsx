import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GoogleCredentialPaths } from './GoogleCredentialPaths';
import { api, type GoogleCredentialsStatus } from '@/lib/api';

const BASE: GoogleCredentialsStatus = {
  integration_enabled: true,
  effective: 'none',
  service_account: { configured: false, key_file: '', subject: '', error: '' },
  apps_script: { configured: false, url: '', error: '' },
  required_scopes: [
    'https://www.googleapis.com/auth/gmail.readonly',
    'https://www.googleapis.com/auth/calendar.events',
  ],
};

function mockGet(overrides: Partial<GoogleCredentialsStatus> = {}) {
  return vi.spyOn(api.googleCredentials, 'get').mockResolvedValue({ ...BASE, ...overrides });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('GoogleCredentialPaths', () => {
  it('opens on the path that is already configured', async () => {
    mockGet({
      effective: 'apps_script',
      apps_script: {
        configured: true,
        url: 'https://script.google.com/macros/s/AK/exec',
        error: '',
      },
    });
    renderWithProviders(<GoogleCredentialPaths />);

    // The bridge URL field only renders when that mode is selected, so its
    // presence proves the form opened on the configured path.
    await waitFor(() =>
      expect(screen.getByDisplayValue('https://script.google.com/macros/s/AK/exec')).toBeInTheDocument(),
    );
  });

  it('surfaces a broken section instead of showing it as unconfigured', async () => {
    // A present-but-invalid section must not read as "nothing set up" — the
    // tools would fail while the UI looked clean.
    mockGet({
      service_account: {
        configured: false,
        key_file: '',
        subject: '',
        error: 'service account key file not found: /keys/missing.json',
      },
    });
    renderWithProviders(<GoogleCredentialPaths />);
    await waitFor(() =>
      expect(screen.getByText(/service account key file not found/i)).toBeInTheDocument(),
    );
  });

  it('saving the OAuth mode clears the alternative credential sections', async () => {
    const user = userEvent.setup();
    mockGet({
      effective: 'apps_script',
      apps_script: { configured: true, url: 'https://script.google.com/macros/s/AK/exec', error: '' },
    });
    const setSpy = vi.spyOn(api.googleCredentials, 'set').mockResolvedValue({ ok: true, mode: 'none' });
    renderWithProviders(<GoogleCredentialPaths />);

    await waitFor(() => expect(screen.getByText(/OAuth connect/i)).toBeInTheDocument());
    await user.click(screen.getByText(/OAuth connect/i));
    await user.click(screen.getByRole('button', { name: /save/i }));

    await waitFor(() => expect(setSpy).toHaveBeenCalledWith({ mode: 'none' }));
  });

  it('omits the secret when left blank so editing the URL keeps the stored one', async () => {
    const user = userEvent.setup();
    mockGet({
      effective: 'apps_script',
      apps_script: { configured: true, url: 'https://script.google.com/macros/s/AK/exec', error: '' },
    });
    const setSpy = vi
      .spyOn(api.googleCredentials, 'set')
      .mockResolvedValue({ ok: true, mode: 'apps_script' });
    renderWithProviders(<GoogleCredentialPaths />);

    await waitFor(() =>
      expect(screen.getByDisplayValue('https://script.google.com/macros/s/AK/exec')).toBeInTheDocument(),
    );
    await user.click(screen.getByRole('button', { name: /save/i }));

    await waitFor(() =>
      expect(setSpy).toHaveBeenCalledWith({
        mode: 'apps_script',
        url: 'https://script.google.com/macros/s/AK/exec',
      }),
    );
    // No `secret` key at all — sending an empty string would wipe the stored one.
    expect(setSpy.mock.calls[0][0]).not.toHaveProperty('secret');
  });

  it('sends the service-account fields when that mode is saved', async () => {
    const user = userEvent.setup();
    mockGet();
    const setSpy = vi
      .spyOn(api.googleCredentials, 'set')
      .mockResolvedValue({ ok: true, mode: 'service_account' });
    renderWithProviders(<GoogleCredentialPaths />);

    await waitFor(() => expect(screen.getByText(/Service account/i)).toBeInTheDocument());
    await user.click(screen.getByText(/Service account/i));
    await user.type(screen.getByLabelText(/Service account key file/i), 'keys/sa.json');
    await user.type(screen.getByLabelText(/User to impersonate/i), 'boss@customer.com');
    await user.click(screen.getByRole('button', { name: /save/i }));

    await waitFor(() =>
      expect(setSpy).toHaveBeenCalledWith({
        mode: 'service_account',
        key_file: 'keys/sa.json',
        subject: 'boss@customer.com',
      }),
    );
  });

  it('warns when the master gate is off, since the credential test still passes', async () => {
    // The trap this guards: configure a path, watch 測試連線 go green, and still
    // see no Google tools — the test exercises the credential, not the gate.
    mockGet({ integration_enabled: false });
    renderWithProviders(<GoogleCredentialPaths />);
    await waitFor(() =>
      expect(screen.getByText(/master switch is still off/i)).toBeInTheDocument(),
    );
  });

  it('does not warn about the gate once it is enabled', async () => {
    mockGet({ integration_enabled: true });
    renderWithProviders(<GoogleCredentialPaths />);
    await waitFor(() => expect(screen.getByText(/Credential path/i)).toBeInTheDocument());
    expect(screen.queryByText(/master switch is still off/i)).not.toBeInTheDocument();
  });

  it('renders nothing when the status call fails (no half-configured UI)', async () => {
    vi.spyOn(api.googleCredentials, 'get').mockRejectedValue(new Error('nope'));
    const { container } = renderWithProviders(<GoogleCredentialPaths />);
    await waitFor(() => expect(container.querySelector('[data-slot="card"]')).toBeNull());
  });
});
