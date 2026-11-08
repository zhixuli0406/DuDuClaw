import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { renderWithProviders } from '@/test/render';
import { useAgentsStore } from '@/stores/agents-store';

// SystemTab loads current values via system.config on mount and writes them back
// via system.update_config on save. Mock the api so we can assert the
// remote-access allowlist (allowed_origins) round-trips correctly.
const configMock = vi.fn();
const updateConfigMock = vi.fn();
const updateMemoryPrivacyMock = vi.fn();
vi.mock('@/lib/api', () => ({
  api: {
    system: {
      config: () => configMock(),
      updateConfig: (fields: Record<string, unknown>) => updateConfigMock(fields),
      updateMemoryPrivacy: (settings: Record<string, unknown>) => updateMemoryPrivacyMock(settings),
    },
  },
}));

import { SystemTab } from './SystemTab';

beforeEach(() => {
  // Two seeded origins so we can render chips and remove one.
  configMock.mockResolvedValue({
    config: 'bind = "127.0.0.1"\nport = 3100\n',
    allowed_origins: ['dash.example.com', 'box.tailnet.ts.net'],
  });
  // Clear recorded calls so earlier tests' saves don't leak into later
  // `not.toHaveBeenCalled()` assertions (vitest doesn't clear them for us).
  updateConfigMock.mockReset();
  updateConfigMock.mockResolvedValue({ success: true, changes: [], applied: true });
  updateMemoryPrivacyMock.mockReset();
  updateMemoryPrivacyMock.mockResolvedValue({ success: true, changes: [] });
  // fetchAgents is called on mount; stub it out and keep the roster empty.
  useAgentsStore.setState({ fetchAgents: vi.fn() as never, agents: [] as never });
});

describe('<SystemTab> remote-access allowlist', () => {
  it('renders the seeded allowed origins as chips', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => {
      expect(screen.getByText('dash.example.com')).toBeInTheDocument();
      expect(screen.getByText('box.tailnet.ts.net')).toBeInTheDocument();
    });
  });

  it('adds a new origin via the input + Add button', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    const input = screen.getByLabelText('Add a URL') as HTMLInputElement;
    fireEvent.change(input, { target: { value: 'new.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: /^Add$/i }));

    await waitFor(() => expect(screen.getByText('new.example.com')).toBeInTheDocument());
    // Draft input is cleared after adding.
    expect(input.value).toBe('');
  });

  it('removes an origin when its X is clicked', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    fireEvent.click(screen.getByLabelText('Remove dash.example.com'));
    await waitFor(() => expect(screen.queryByText('dash.example.com')).not.toBeInTheDocument());
    // The other chip survives.
    expect(screen.getByText('box.tailnet.ts.net')).toBeInTheDocument();
  });

  it('sends the edited allowlist in the update_config payload', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    // Add one, remove one → payload should reflect the net set.
    const input = screen.getByLabelText('Add a URL');
    fireEvent.change(input, { target: { value: 'added.example.com' } });
    fireEvent.click(screen.getByRole('button', { name: /^Add$/i }));
    fireEvent.click(screen.getByLabelText('Remove box.tailnet.ts.net'));

    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const payload = updateConfigMock.mock.calls[0][0] as { allowed_origins: string[] };
    expect(payload.allowed_origins).toEqual(['dash.example.com', 'added.example.com']);
  });
});

describe('<SystemTab> automatic-memory privacy card (RM-039)', () => {
  it('shows all four categories on by default when system.config has no memory_privacy', async () => {
    renderWithProviders(<SystemTab />);
    for (const name of ['Health', 'Family', 'Personal finances', 'Salary']) {
      await waitFor(() => expect(screen.getByRole('switch', { name: `Do not record ${name} content` })).toBeChecked());
    }
    expect(screen.getByText('What automatic memory does not record')).toBeInTheDocument();
  });

  it('saves a toggled category straight away with excluded_categories, without touching the form Save payload', async () => {
    const user = userEvent.setup();
    renderWithProviders(<SystemTab />);
    const health = await screen.findByRole('switch', { name: 'Do not record Health content' });
    await waitFor(() => expect(health).toBeEnabled());
    await user.click(health);
    await waitFor(() => expect(updateMemoryPrivacyMock).toHaveBeenCalledWith({
      excluded_categories: ['family', 'personal_finance', 'salary'],
    }));
    expect(health).not.toBeChecked();
    expect(await screen.findByText('Saved')).toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /^Save$/i }));
    expect(updateConfigMock).not.toHaveBeenCalled(); // nothing else changed
  });

  it('rolls the switch back when the gateway refuses the change', async () => {
    updateMemoryPrivacyMock.mockRejectedValue(new Error('refused'));
    const user = userEvent.setup();
    renderWithProviders(<SystemTab />);
    const salary = await screen.findByRole('switch', { name: 'Do not record Salary content' });
    await waitFor(() => expect(salary).toBeEnabled());
    await user.click(salary);
    await waitFor(() => expect(updateMemoryPrivacyMock).toHaveBeenCalled());
    await waitFor(() => expect(salary).toBeChecked());
  });

  it('adds and removes a custom keyword chip, sending custom_keywords', async () => {
    configMock.mockResolvedValue({
      config: '',
      allowed_origins: [],
      memory_privacy: {
        excluded_categories: ['health', 'family', 'personal_finance', 'salary'],
        custom_keywords: { health: ['rehab'] },
        builtin_categories: ['health', 'family', 'personal_finance', 'salary'],
        load_state: 'configured',
      },
    });
    const user = userEvent.setup();
    renderWithProviders(<SystemTab />);
    await user.click(await screen.findByRole('button', { name: /1 custom keyword/ }));
    expect(screen.getByText('rehab')).toBeInTheDocument();

    await user.type(screen.getByLabelText('Add a keyword for Health'), 'clinic{Enter}');
    await waitFor(() => expect(updateMemoryPrivacyMock).toHaveBeenCalledWith({
      custom_keywords: { health: ['rehab', 'clinic'] },
    }));
    expect(await screen.findByText('clinic')).toBeInTheDocument();

    await user.click(screen.getByLabelText('Remove keyword rehab'));
    await waitFor(() => expect(updateMemoryPrivacyMock).toHaveBeenLastCalledWith({
      custom_keywords: { health: ['clinic'] },
    }));
    expect(screen.queryByText('rehab')).not.toBeInTheDocument();
  });
});

describe('<SystemTab> memory dedup gate ([memory] novelty_gate)', () => {
  it('defaults the toggle to on when the field is absent from system.config', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Memory dedup gate' })).toBeChecked());
  });

  it('reflects a saved off state from system.config', async () => {
    configMock.mockResolvedValue({ config: '', allowed_origins: [], novelty_gate_enabled: false });
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Memory dedup gate' })).not.toBeChecked());
  });

  it('sends the toggled value in the update_config payload on save', async () => {
    renderWithProviders(<SystemTab />);
    // Wait for the async system.config() load to land (mirrors the allowlist
    // tests above) before interacting — otherwise the load's `setNoveltyGate`
    // can resolve AFTER the click and silently overwrite the toggle back.
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());
    const toggle = screen.getByRole('switch', { name: 'Memory dedup gate' });
    expect(toggle).toBeChecked();

    fireEvent.click(toggle);
    expect(toggle).not.toBeChecked();

    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    // `updateConfigMock.mock.calls` accumulates across every test in this file
    // (nothing clears it between tests) — take the LAST call, not the first,
    // since an earlier describe block's test also saves.
    const lastCall = updateConfigMock.mock.calls.at(-1) as [{ novelty_gate_enabled: boolean }];
    expect(lastCall[0].novelty_gate_enabled).toBe(false);
  });
});

describe('<SystemTab> daily digest ([notify] daily_digest)', () => {
  it('defaults the toggle to off and hides the time field when absent from system.config', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Enable daily digest' })).not.toBeChecked());
    expect(screen.queryByLabelText('Send time')).not.toBeInTheDocument();
  });

  it('reflects a saved on state + time from system.config', async () => {
    configMock.mockResolvedValue({
      config: '',
      allowed_origins: [],
      daily_digest_enabled: true,
      daily_digest_at: '07:30',
    });
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByRole('switch', { name: 'Enable daily digest' })).toBeChecked());
    expect(screen.getByDisplayValue('07:30')).toBeInTheDocument();
  });

  it('sends the toggled flag and time in the update_config payload on save', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    const toggle = screen.getByRole('switch', { name: 'Enable daily digest' });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(toggle).toBeChecked();

    const timeInput = screen.getByLabelText('Send time') as HTMLInputElement;
    fireEvent.change(timeInput, { target: { value: '20:15' } });

    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const lastCall = updateConfigMock.mock.calls.at(-1) as [{ daily_digest: boolean; daily_digest_at: string }];
    expect(lastCall[0].daily_digest).toBe(true);
    expect(lastCall[0].daily_digest_at).toBe('20:15');
  });
});

// S20: `[miniapp] enabled` had no dashboard surface at all until 2026-09
// (`grep miniapp web/src` was empty), so the Telegram Mini App was only
// reachable by hand-editing config.toml. Default stays off.
describe('<SystemTab> Telegram mini app ([miniapp] enabled)', () => {
  it('defaults the toggle to off when the field is absent from system.config', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() =>
      expect(screen.getByRole('switch', { name: 'Approve inside Telegram' })).not.toBeChecked(),
    );
  });

  it('reflects a saved on state from system.config', async () => {
    configMock.mockResolvedValue({ config: '', allowed_origins: [], miniapp_enabled: true });
    renderWithProviders(<SystemTab />);
    await waitFor(() =>
      expect(screen.getByRole('switch', { name: 'Approve inside Telegram' })).toBeChecked(),
    );
  });

  it('sends the toggled value in the update_config payload on save', async () => {
    renderWithProviders(<SystemTab />);
    await waitFor(() => expect(screen.getByText('dash.example.com')).toBeInTheDocument());

    const toggle = screen.getByRole('switch', { name: 'Approve inside Telegram' });
    expect(toggle).not.toBeChecked();
    fireEvent.click(toggle);
    expect(toggle).toBeChecked();

    fireEvent.click(screen.getByRole('button', { name: /^Save$/i }));

    await waitFor(() => expect(updateConfigMock).toHaveBeenCalled());
    const lastCall = updateConfigMock.mock.calls.at(-1) as [{ miniapp_enabled: boolean }];
    expect(lastCall[0].miniapp_enabled).toBe(true);
  });
});
