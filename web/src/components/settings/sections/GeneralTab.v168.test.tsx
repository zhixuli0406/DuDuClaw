import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
      autostartStatus: () => Promise.resolve({ supported: false, enabled: false }),
    },
  },
}));

import { GeneralTab } from './GeneralTab';
import { useAuthStore } from '@/stores/auth-store';

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: null } as never);
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
});

describe('<GeneralTab> dashboard public URL (RM-129)', () => {
  it('loads, validates and saves [dashboard] public_url for admins', async () => {
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } } as never);
    configMock.mockResolvedValue({ config: '[dashboard]\npublic_url = "https://example.com/"\n' });
    renderWithProviders(<GeneralTab />);
    const input = await screen.findByPlaceholderText('https://example.com/agents/');
    await waitFor(() => expect(input).toHaveValue('https://example.com/'));

    fireEvent.change(input, { target: { value: 'https://example.com/agents?x=1' } });
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await new Promise((r) => setTimeout(r, 20));
    expect(updateConfigMock).not.toHaveBeenCalled();

    fireEvent.change(input, { target: { value: 'http://dashboard.example/agents' } });
    expect(await screen.findByRole('alert')).toHaveTextContent('HTTPS');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(updateConfigMock).not.toHaveBeenCalled();

    fireEvent.change(input, { target: { value: ' https://example.com/agents/ ' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ dashboard: { public_url: 'https://example.com/agents/' } }));
  });

  it('hides the field from non-admins', async () => {
    configMock.mockResolvedValue({ config: '[dashboard]\npublic_url = "https://example.com/"\n' });
    renderWithProviders(<GeneralTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    expect(screen.queryByPlaceholderText('https://example.com/agents/')).not.toBeInTheDocument();
  });
});

describe('<GeneralTab> v1.68', () => {
  it('keeps migrated log and rotation controls out of common settings and does not rewrite them', async () => {
    // `[redaction] level` and `[tick] strategy` used to win the unanchored regex.
    configMock.mockResolvedValue({
      config: '[redaction]\nlevel = "trace"\n\n[general]\nlog_level = "warn"\n\n[tick]\nstrategy = "aggressive"\n\n[rotation]\nstrategy = "round_robin"\n',
    });
    renderWithProviders(<GeneralTab />);
    await waitFor(() => expect(configMock).toHaveBeenCalled());
    expect(screen.queryByRole('combobox', { name: 'Log Level' })).not.toBeInTheDocument();
    expect(screen.queryByRole('combobox', { name: 'Account Rotation Strategy' })).not.toBeInTheDocument();
    // Saving without edits sends nothing — proving the loaded values match.
    fireEvent.click(await screen.findByRole('button', { name: 'Save' }));
    await new Promise((r) => setTimeout(r, 20));
    expect(updateConfigMock).not.toHaveBeenCalled();
  });
});
