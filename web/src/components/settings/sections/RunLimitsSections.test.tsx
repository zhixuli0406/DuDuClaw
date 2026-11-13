import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, screen, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { RunLimitsSections } from './RunLimitsSections';

const config = vi.fn();
const update = vi.fn();
vi.mock('@/lib/api', () => ({ api: { system: {
  config: () => config(), updateConfig: (fields: unknown) => update(fields),
} } }));

beforeEach(() => {
  vi.clearAllMocks();
  config.mockResolvedValue({ config: '[runtime]\nmax_timeout_secs=7200\ndefault_timeout_secs=600\n[agent_files]\nmax_pending_per_user=5\n' });
  update.mockResolvedValue({ success: true, restart_required: [] });
});

function saveCard(label: string) {
  const input = screen.getByRole('spinbutton', { name: label });
  // SettingsSection renders a semantic section with its own save button.
  const section = input.closest('section');
  expect(section).not.toBeNull();
  fireEvent.click(within(section!).getByRole('button', { name: /^Save (run|schedule|review) limits$/ }));
}

describe('Run limit settings cards', () => {
  it('gives each new save button a distinct accessible name', () => {
    renderWithProviders(<RunLimitsSections />);
    expect(screen.getByRole('button', { name: 'Save run limits' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save schedule limits' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save review limits' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /^Save$/i })).not.toBeInTheDocument();
  });

  it('shows the existing schedule bounds and saves only the changed assignment setting', async () => {
    renderWithProviders(<RunLimitsSections />);
    const interval = await screen.findByRole('spinbutton', { name: 'Minimum interval (minutes)' });
    await waitFor(() => expect(screen.getByRole('spinbutton', { name: 'Run limit ceiling (seconds)' })).toHaveValue(7200));
    expect(interval).toHaveAttribute('min', '60');
    expect(interval).toHaveAttribute('max', '10080');
    fireEvent.change(interval, { target: { value: '120' } });
    saveCard('Minimum interval (minutes)');
    await waitFor(() => expect(update).toHaveBeenCalledWith({ goal_defaults: { assign_schedule_min_interval_minutes: 120 } }));
  });

  it('clears the optional global default without changing the ceiling', async () => {
    renderWithProviders(<RunLimitsSections />);
    const field = await screen.findByRole('spinbutton', { name: 'Default run limit (seconds)' });
    await waitFor(() => expect(field).toHaveValue(600));
    fireEvent.change(field, { target: { value: '' } });
    saveCard('Default run limit (seconds)');
    await waitFor(() => expect(update).toHaveBeenCalledWith({ runtime: { default_timeout_secs: null } }));
  });

  it('loads and saves the pending request cap in the agent_files table', async () => {
    renderWithProviders(<RunLimitsSections />);
    const field = await screen.findByRole('spinbutton', { name: 'Pending requests per person' });
    await waitFor(() => expect(field).toHaveValue(5));
    expect(field).toHaveAttribute('min', '1');
    expect(field).toHaveAttribute('max', '200');
    fireEvent.change(field, { target: { value: '10' } });
    saveCard('Pending requests per person');
    await waitFor(() => expect(update).toHaveBeenCalledWith({ agent_files: { max_pending_per_user: 10 } }));
  });
});
