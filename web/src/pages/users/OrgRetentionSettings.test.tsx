import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import en from '@/i18n/en.json';
import { useAuthStore } from '@/stores/auth-store';
import type { TeamHistorySettings } from '@/lib/api';
import { HistorySinceSetting, RetentionSettings } from './OrgRetentionSettings';
import { OrgHistorySettings } from './OrgHistorySettings';

function daysAgo(days: number): Date {
  const d = new Date();
  d.setHours(12, 0, 0, 0);
  d.setDate(d.getDate() - days);
  return d;
}
function dateInput(d: Date): string {
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}

const since = daysAgo(10).toISOString();
const base: TeamHistorySettings = {
  manager_team_history: true,
  since,
  team_history_since: since,
  team_history_since_floor: '2020-01-01T00:00:00Z',
  archive_after_days: 30,
  purge_after_days: 0,
};

beforeEach(() => {
  vi.clearAllMocks();
  useAuthStore.setState({ user: { id: 'admin', role: 'admin' } as never });
});

describe('RM-173 conversation storage settings', () => {
  it('shows the server values and saves both day counts, warning before deletion is enabled', async () => {
    const onSaved = vi.fn();
    mockWsClient.call.mockResolvedValue({ ...base, purge_after_days: 90 });
    renderWithProviders(<RetentionSettings settings={base} onSaved={onSaved} />);
    const archive = screen.getByLabelText(en['teamHistory.retention.archiveLabel']);
    const purge = screen.getByLabelText(en['teamHistory.retention.purgeLabel']);
    expect(archive).toHaveValue(30);
    expect(purge).toHaveValue(0);
    expect(screen.queryByText(en['teamHistory.retention.purgeWarning'])).not.toBeInTheDocument();
    const save = screen.getByRole('button', { name: en['teamHistory.retention.save'] });
    expect(save).toBeDisabled();
    fireEvent.change(purge, { target: { value: '90' } });
    expect(screen.getByText(en['teamHistory.retention.purgeWarning'])).toBeInTheDocument();
    await userEvent.setup().click(save);
    expect(mockWsClient.call).toHaveBeenCalledWith('users.org_settings.update', { archive_after_days: 30, purge_after_days: 90 });
    expect(await screen.findByText(en['teamHistory.retention.saved'])).toBeInTheDocument();
    expect(onSaved).toHaveBeenCalledWith(expect.objectContaining({ purge_after_days: 90 }));
  });

  it('refuses out-of-range values before sending', () => {
    renderWithProviders(<RetentionSettings settings={base} onSaved={vi.fn()} />);
    fireEvent.change(screen.getByLabelText(en['teamHistory.retention.archiveLabel']), { target: { value: '4000' } });
    expect(screen.getByText(en['teamHistory.retention.invalid'])).toBeInTheDocument();
    expect(screen.getByRole('button', { name: en['teamHistory.retention.save'] })).toBeDisabled();
    expect(mockWsClient.call).not.toHaveBeenCalled();
  });

  it('reports a refused save and keeps the confirmed values', async () => {
    const onSaved = vi.fn();
    mockWsClient.call.mockRejectedValue({ code: 'audit_unavailable', message: 'audit unavailable' });
    renderWithProviders(<RetentionSettings settings={base} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText(en['teamHistory.retention.archiveLabel']), { target: { value: '0' } });
    await userEvent.setup().click(screen.getByRole('button', { name: en['teamHistory.retention.save'] }));
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(onSaved).not.toHaveBeenCalled();
  });

  it('is hidden on a gateway that does not report retention settings', () => {
    renderWithProviders(<RetentionSettings settings={{ manager_team_history: true, since }} onSaved={vi.fn()} />);
    expect(screen.queryByLabelText(en['teamHistory.retention.archiveLabel'])).not.toBeInTheDocument();
  });

  it('is part of the organisation settings card', async () => {
    mockWsClient.call.mockResolvedValue(base);
    renderWithProviders(<OrgHistorySettings />);
    expect(await screen.findByLabelText(en['teamHistory.retention.purgeLabel'])).toBeInTheDocument();
    expect(screen.getByLabelText(en['teamHistory.since.label'])).toBeInTheDocument();
  });
});

describe('RM-173 team conversations start date', () => {
  it('moves the date later without asking for a password', async () => {
    const onSaved = vi.fn();
    mockWsClient.call.mockResolvedValue(base);
    renderWithProviders(<HistorySinceSetting settings={base} onSaved={onSaved} />);
    expect(screen.getByText(en['teamHistory.since.warning'])).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(en['teamHistory.since.label']), { target: { value: dateInput(daysAgo(5)) } });
    await userEvent.setup().click(screen.getByRole('button', { name: en['teamHistory.since.save'] }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    const [, params] = mockWsClient.call.mock.calls[0];
    expect(params).not.toHaveProperty('password');
    expect(Date.parse(params.team_history_since)).toBeGreaterThan(Date.parse(since));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('moving it earlier asks for the password (and the second factor when required) first', async () => {
    const user = userEvent.setup();
    const onSaved = vi.fn();
    renderWithProviders(<HistorySinceSetting settings={base} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText(en['teamHistory.since.label']), { target: { value: dateInput(daysAgo(40)) } });
    await user.click(screen.getByRole('button', { name: en['teamHistory.since.save'] }));
    const dialog = await screen.findByRole('dialog', { name: en['teamHistory.stepUp.title'] });
    expect(dialog).toHaveTextContent(en['teamHistory.stepUp.description']);
    expect(mockWsClient.call).not.toHaveBeenCalled();

    mockWsClient.call.mockRejectedValueOnce({ code: 'mfa_code_required', message: 'MFA_CODE_REQUIRED' });
    await user.type(screen.getByLabelText(en['teamHistory.stepUp.password']), 'my-password');
    await user.click(screen.getByRole('button', { name: en['teamHistory.stepUp.confirm'] }));
    const code = await screen.findByLabelText(en['teamHistory.stepUp.code']);
    expect(onSaved).not.toHaveBeenCalled();

    mockWsClient.call.mockResolvedValueOnce({ ...base, team_history_since: daysAgo(40).toISOString() });
    await user.type(code, '123456');
    await user.click(screen.getByRole('button', { name: en['teamHistory.stepUp.confirm'] }));
    await waitFor(() => expect(onSaved).toHaveBeenCalled());
    const [method, params] = mockWsClient.call.mock.calls[1];
    expect(method).toBe('users.org_settings.update');
    expect(params).toMatchObject({ password: 'my-password', code: '123456' });
    expect(Date.parse(params.team_history_since)).toBeLessThan(Date.parse(since));
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('a wrong password changes nothing and says so', async () => {
    const user = userEvent.setup();
    const onSaved = vi.fn();
    mockWsClient.call.mockRejectedValue({ code: 'step_up_failed', message: 'wrong' });
    renderWithProviders(<HistorySinceSetting settings={base} onSaved={onSaved} />);
    fireEvent.change(screen.getByLabelText(en['teamHistory.since.label']), { target: { value: dateInput(daysAgo(40)) } });
    await user.click(screen.getByRole('button', { name: en['teamHistory.since.save'] }));
    await user.type(await screen.findByLabelText(en['teamHistory.stepUp.password']), 'nope');
    await user.click(screen.getByRole('button', { name: en['teamHistory.stepUp.confirm'] }));
    expect(await screen.findByRole('alert')).toHaveTextContent(en['teamHistory.stepUp.failed']);
    expect(onSaved).not.toHaveBeenCalled();
  });

  it('cannot be changed while manager conversation access is off', () => {
    renderWithProviders(<HistorySinceSetting settings={{ ...base, manager_team_history: false, since: null, team_history_since: null }} onSaved={vi.fn()} />);
    expect(screen.getByLabelText(en['teamHistory.since.label'])).toBeDisabled();
    expect(screen.getByText(en['teamHistory.since.off'])).toBeInTheDocument();
  });
});
