import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GoogleIntegrationPage } from './GoogleIntegrationPage';
import { api, type GoogleCredentialsStatus } from '@/lib/api';

const BASE: GoogleCredentialsStatus = {
  integration_enabled: true,
  effective: 'none',
  service_account: { configured: false, key_file: '', subject: '', error: '' },
  apps_script: { configured: false, url: '', error: '' },
  required_scopes: [],
};

function mockGet(overrides: Partial<GoogleCredentialsStatus> = {}) {
  return vi.spyOn(api.googleCredentials, 'get').mockResolvedValue({ ...BASE, ...overrides });
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('GoogleIntegrationPage — master gate (G6)', () => {
  it('shows an enable prompt, not a silent dead end, when the backend gate is off', async () => {
    // The regression this pins: the tab was unconditionally visible while
    // `[integrations] google_workspace` defaulted to false, so the page looked
    // ready while every agent tool call 403'd.
    mockGet({ integration_enabled: false });
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(screen.getByTestId('google-gate-off')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-on')).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /啟用|enable/i })).toBeInTheDocument();
  });

  it('enables the integration through the backend and flips the banner', async () => {
    mockGet({ integration_enabled: false });
    const set = vi
      .spyOn(api.googleCredentials, 'setIntegration')
      .mockResolvedValue({ enabled: true, changed: true });

    renderWithProviders(<GoogleIntegrationPage />);
    await waitFor(() => expect(screen.getByTestId('google-gate-off')).toBeInTheDocument());

    await userEvent.click(screen.getByRole('button', { name: /啟用|enable/i }));

    await waitFor(() => expect(set).toHaveBeenCalledWith(true));
    await waitFor(() => expect(screen.getByTestId('google-gate-on')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
  });

  it('shows the enabled state with a way back off', async () => {
    mockGet({ integration_enabled: true });
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(screen.getByTestId('google-gate-on')).toBeInTheDocument());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
  });

  it('shows neither banner while the status is unknown', async () => {
    // A failed status read must not claim the integration is off — that would
    // push the operator to "enable" something that is already on.
    vi.spyOn(api.googleCredentials, 'get').mockRejectedValue(new Error('offline'));
    renderWithProviders(<GoogleIntegrationPage />);

    await waitFor(() => expect(api.googleCredentials.get).toHaveBeenCalled());
    expect(screen.queryByTestId('google-gate-off')).not.toBeInTheDocument();
    expect(screen.queryByTestId('google-gate-on')).not.toBeInTheDocument();
  });
});
