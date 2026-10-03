import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useRestartRequiredStore } from '@/stores/restart-required-store';

const statusMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: { system: { status: () => statusMock() } },
}));

import { RestartRequiredBanner } from './RestartRequiredBanner';

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.clear();
  useRestartRequiredStore.getState().clear();
  // A gateway that has been up for a day: started long before any save.
  statusMock.mockResolvedValue({ uptime_seconds: 86400 });
});

describe('<RestartRequiredBanner>', () => {
  it('renders nothing when no setting is pending', () => {
    renderWithProviders(<RestartRequiredBanner />);
    expect(screen.queryByTestId('restart-required-banner')).not.toBeInTheDocument();
  });

  it('lists the keys that need a restart and persists them', async () => {
    useRestartRequiredStore.getState().add(['telemetry.otlp_endpoint', 'general.log_level']);
    useRestartRequiredStore.getState().add(['telemetry.otlp_endpoint']);
    renderWithProviders(<RestartRequiredBanner />);
    const banner = await screen.findByTestId('restart-required-banner');
    expect(banner.textContent).toContain('telemetry.otlp_endpoint、general.log_level');
    expect(JSON.parse(localStorage.getItem('duduclaw.restartRequired') ?? '{}').keys).toEqual([
      'telemetry.otlp_endpoint',
      'general.log_level',
    ]);
  });

  it('clears itself once the gateway restarted after the save', async () => {
    useRestartRequiredStore.getState().add(['tick']);
    statusMock.mockResolvedValue({ uptime_seconds: 0 });
    renderWithProviders(<RestartRequiredBanner />);
    await waitFor(() => expect(screen.queryByTestId('restart-required-banner')).not.toBeInTheDocument());
  });

  it('can be dismissed', async () => {
    useRestartRequiredStore.getState().add(['tick']);
    renderWithProviders(<RestartRequiredBanner />);
    fireEvent.click(await screen.findByRole('button', { name: 'Dismiss notice' }));
    expect(screen.queryByTestId('restart-required-banner')).not.toBeInTheDocument();
    expect(useRestartRequiredStore.getState().keys).toEqual([]);
  });
});
