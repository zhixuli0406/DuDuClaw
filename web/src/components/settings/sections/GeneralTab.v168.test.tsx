import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
const companyTimezoneMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      companyTimezone: () => companyTimezoneMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
      autostartStatus: () => Promise.resolve({ supported: false, enabled: false }),
    },
  },
}));

import { GeneralTab } from './GeneralTab';
import { useAuthStore } from '@/stores/auth-store';
import { resetCompanyTimeZone, useCompanyTimeStore } from '@/lib/company-time';

beforeEach(() => {
  vi.clearAllMocks();
  resetCompanyTimeZone();
  companyTimezoneMock.mockResolvedValue({ timezone: null, status: 'unset', source: null });
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

describe('<GeneralTab> memory language (RM-178)', () => {
  it('loads the memory language and saves only the changed memory key', async () => {
    const user = userEvent.setup();
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } } as never);
    configMock.mockResolvedValue({ config: '[general]\ndefault_language = "en"\n[memory]\nlanguage = "zh-TW"\n' });
    renderWithProviders(<GeneralTab />);
    const select = await screen.findByRole('combobox', { name: 'Memory language' });
    await waitFor(() => expect(select).toHaveTextContent('Traditional Chinese'));
    await user.click(select);
    await user.click(await screen.findByRole('option', { name: 'Follow reply language' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ memory: { language: 'auto' } }));
  });

  it('disables memory language for non-admins', async () => {
    configMock.mockResolvedValue({ config: '' });
    renderWithProviders(<GeneralTab />);
    expect(await screen.findByRole('combobox', { name: 'Memory language' })).toBeDisabled();
  });
});


describe('<GeneralTab> company time zone (RM-222)', () => {
  beforeEach(() => {
    useAuthStore.setState({ user: { id: 'admin', role: 'admin' } } as never);
  });

  it('loads the stored value and sends only the changed company zone', async () => {
    configMock.mockResolvedValue({ config: '[general]\ncompany_timezone = "Asia/Taipei"\n' });
    companyTimezoneMock.mockResolvedValue({ timezone: 'Asia/Tokyo', status: 'set', source: null });
    renderWithProviders(<GeneralTab />);
    const input = screen.getByRole('combobox', { name: 'Company time zone' });
    await waitFor(() => expect(input).toHaveValue('Asia/Taipei'));
    fireEvent.change(input, { target: { value: 'Asia/Tokyo' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ company_timezone: 'Asia/Tokyo' }));
    await waitFor(() => expect(companyTimezoneMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(useCompanyTimeStore.getState().zone).toBe('Asia/Tokyo'));
  });

  it('clears the stored zone with an empty string', async () => {
    configMock.mockResolvedValue({ config: '[general]\ncompany_timezone = "Asia/Taipei"\n' });
    renderWithProviders(<GeneralTab />);
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Company time zone' })).toHaveValue('Asia/Taipei'));
    fireEvent.click(screen.getByRole('button', { name: 'Clear' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ company_timezone: '' }));
    await waitFor(() => expect(companyTimezoneMock).toHaveBeenCalledTimes(1));
  });

  it('confirms the automatically detected value even when unchanged', async () => {
    configMock.mockResolvedValue({ config: '[general]\ncompany_timezone = "Asia/Taipei"\ncompany_timezone_source = "detected_at_upgrade"\n' });
    companyTimezoneMock.mockResolvedValue({ timezone: 'Asia/Taipei', status: 'set', source: null });
    renderWithProviders(<GeneralTab />);
    expect(await screen.findByText('Filled in automatically from this host’s time zone. Please confirm.')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalledWith({ company_timezone: 'Asia/Taipei' }));
    await waitFor(() => expect(companyTimezoneMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument());
  });

  it('explains the fallback when the company zone is unset', async () => {
    configMock.mockResolvedValue({ config: '' });
    renderWithProviders(<GeneralTab />);
    expect(await screen.findByText('Not set: new schedules use UTC. Times on screen follow each person’s computer time zone.')).toBeInTheDocument();
  });

  it('shows an invalid stored value and rejects an unknown selection', async () => {
    configMock.mockResolvedValue({ config: '[general]\ncompany_timezone = "Not/AZone"\n' });
    renderWithProviders(<GeneralTab />);
    expect(await screen.findByText('The current setting is invalid. Please choose a time zone again.')).toBeInTheDocument();
    fireEvent.change(screen.getByRole('combobox', { name: 'Company time zone' }), { target: { value: 'Still/Invalid' } });
    expect(screen.queryByText('The current setting is invalid. Please choose a time zone again.')).not.toBeInTheDocument();
    expect(screen.getByText('Choose a time zone from the list.')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(updateConfigMock).not.toHaveBeenCalled();
  });

  it('disables changes and confirmation for non-admins', async () => {
    useAuthStore.setState({ user: null } as never);
    configMock.mockResolvedValue({ config: '[general]\ncompany_timezone = "Asia/Taipei"\ncompany_timezone_source = "detected_at_upgrade"\n' });
    renderWithProviders(<GeneralTab />);
    await waitFor(() => expect(screen.getByRole('combobox', { name: 'Company time zone' })).toHaveValue('Asia/Taipei'));
    expect(screen.getByRole('combobox', { name: 'Company time zone' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
    expect(screen.queryByRole('button', { name: 'Confirm' })).not.toBeInTheDocument();
  });
});
