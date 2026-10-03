import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';

const configMock = vi.fn();
const updateConfigMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (f: Record<string, unknown>) => updateConfigMock(f),
    },
  },
}));

import { TakeoverMailSection } from './TakeoverMailSection';
import { TeamDefaultsSection } from './TeamDefaultsSection';

beforeEach(() => {
  vi.clearAllMocks();
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
  updateConfigMock.mockResolvedValue({ success: true, changes: [], restart_required: [] });
});

describe('<TakeoverMailSection>', () => {
  it('shows the drop folder on when the key is absent (reader default)', async () => {
    configMock.mockResolvedValue({ config: '[mail]\nenabled = true\n' });
    renderWithProviders(<TakeoverMailSection />);
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Receive from a folder' })).toBeChecked());
  });

  it('reads saved values and sends only what changed', async () => {
    configMock.mockResolvedValue({
      config: '[takeover]\nenabled = false\nduration_minutes = 30\n\n[mail]\nenabled = true\n',
    });
    renderWithProviders(<TakeoverMailSection />);
    const takeover = await screen.findByRole('switch', { name: 'Pause automatically when a person replies' });
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Enable the mailbox' })).toBeChecked());
    expect(screen.getByDisplayValue('30')).toBeInTheDocument();
    fireEvent.click(takeover);
    fireEvent.click(screen.getByRole('switch', { name: 'Receive from Gmail' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save takeover and mailbox settings' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({
      takeover: { enabled: true },
      mail: { gmail_enabled: true },
    });
  });

  it('refuses a pause longer than the maximum before calling the gateway', async () => {
    configMock.mockResolvedValue({ config: '[takeover]\nduration_minutes = 60\nmax_duration_minutes = 90\n' });
    renderWithProviders(<TakeoverMailSection />);
    const duration = await screen.findByDisplayValue('60');
    fireEvent.change(duration, { target: { value: '120' } });
    expect(await screen.findByRole('alert')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Save takeover and mailbox settings' })).toBeDisabled();
  });

  it('does not call the gateway when nothing changed', async () => {
    configMock.mockResolvedValue({ config: '' });
    renderWithProviders(<TakeoverMailSection />);
    await screen.findByRole('switch', { name: 'Enable the mailbox' });
    fireEvent.click(screen.getByRole('button', { name: 'Save takeover and mailbox settings' }));
    await new Promise((r) => setTimeout(r, 20));
    expect(updateConfigMock).not.toHaveBeenCalled();
  });
});

describe('<TeamDefaultsSection>', () => {
  it('reads the synthesizer slot from [team.roles.utility] and sends the gate change alone', async () => {
    configMock.mockResolvedValue({
      config: '[team]\ngate = "auto"\n\n[team.roles.utility]\nmodel = "gpt-5"\n',
    });
    localStorage.setItem('dudu.settings.advanced.settings.automation.teamRoles', '1');
    renderWithProviders(<TeamDefaultsSection />);
    expect(await screen.findByDisplayValue('gpt-5')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('switch', { name: 'Allow teams to form' }));
    fireEvent.click(screen.getByRole('button', { name: 'Save team defaults' }));
    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    expect(updateConfigMock.mock.calls[0][0]).toEqual({ team: { enabled: false } });
  });
});
