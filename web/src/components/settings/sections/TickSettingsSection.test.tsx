import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor, within } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
const listMock = vi.fn();
const upsertMock = vi.fn();
const removeMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
    },
    tickSources: {
      list: () => listMock(),
      upsert: (s: unknown) => upsertMock(s),
      remove: (id: string) => removeMock(id),
    },
  },
}));

import { TickSettingsSection } from './TickSettingsSection';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useRestartRequiredStore.getState().clear();
  configMock.mockResolvedValue({ config: '[tick]\nenabled = true\ndns_ttl_secs = 60\n' });
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
  listMock.mockResolvedValue({
    hot_reload_available: true,
    sources: [
      { id: 'btc', kind: 'http_poll', url: 'https://x.test/api', interval_secs: 30, headers_count: 2, valid: true },
    ],
  });
});

describe('<TickSettingsSection>', () => {
  it('lists configured sources with the header count only', async () => {
    renderWithProviders(<TickSettingsSection />);
    expect(await screen.findByText('btc')).toBeInTheDocument();
    expect(screen.getByText('2 headers')).toBeInTheDocument();
    expect(screen.queryByTestId('tick-restart-notice')).not.toBeInTheDocument();
  });

  it('sends only the changed [tick] key', async () => {
    renderWithProviders(<TickSettingsSection />);
    await screen.findByText('btc');
    fireEvent.click(screen.getByRole('switch', { name: 'Allow command sources' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save live feed settings' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ tick: { allow_command_sources: true } });
  });

  it('shows the restart notice and banner keys when the gateway asks for a restart', async () => {
    upsertMock.mockResolvedValue({ success: true, hot_reloaded: false, restart_required: true });
    renderWithProviders(<TickSettingsSection />);
    await screen.findByText('btc');
    fireEvent.click(screen.getByRole('button', { name: 'Add feed' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.change(within(dialog).getByLabelText('ID'), { target: { value: 'eth' } });
    fireEvent.change(within(dialog).getByLabelText('URL'), { target: { value: 'https://y.test/api' } });
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(upsertMock).toHaveBeenCalled());
    const payload = upsertMock.mock.calls[0][0] as Record<string, unknown>;
    expect(payload.id).toBe('eth');
    expect(payload.kind).toBe('http_poll');
    expect('headers' in payload).toBe(false);
    expect(await screen.findByTestId('tick-restart-notice')).toBeInTheDocument();
    expect(useRestartRequiredStore.getState().keys).toEqual(['tick.sources']);
  });

  it('does not claim a restart when the sources were respawned live', async () => {
    upsertMock.mockResolvedValue({ success: true, hot_reloaded: true, restart_required: false });
    renderWithProviders(<TickSettingsSection />);
    await screen.findByText('btc');
    fireEvent.click(screen.getByRole('button', { name: 'Edit btc' }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(upsertMock).toHaveBeenCalled());
    expect(screen.queryByTestId('tick-restart-notice')).not.toBeInTheDocument();
    expect(useRestartRequiredStore.getState().keys).toEqual([]);
  });

  it('says the gateway cannot edit sources when the RPC is missing', async () => {
    listMock.mockRejectedValue(new Error('Unknown method: tick.sources.list'));
    renderWithProviders(<TickSettingsSection />);
    expect(await screen.findByText(/Older gateways cannot edit feeds/)).toBeInTheDocument();
  });
});
