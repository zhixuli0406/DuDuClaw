import { beforeEach, describe, expect, it, vi } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { GroupContextSettings } from './GroupContextSettings';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'redaction.get') return Promise.resolve({ enabled: true, sources: { user_input: { mode: 'off' } }, poisoned: null });
    if (method === 'channels.group_context_list') return Promise.resolve({ groups: [{ channel: 'discord', group_id: '222', scope_id: '111', max_messages: 50, max_age_hours: 24 }], known_groups: [{ channel: 'discord', group_id: '222' }] });
    if (method === 'channels.discord_agent_choices_get') return Promise.resolve({ agents: [], available_agents: ['worker'], guilds: [{ id: '111', name: 'Team' }] });
    return Promise.resolve({ success: true });
  });
});

describe('GroupContextSettings', () => {
  it('explains retention and deletes retained messages when disabled', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GroupContextSettings platform="discord" active />);
    expect(await screen.findByText('Keep for 24 hours, up to 50 messages')).toBeInTheDocument();
    expect(screen.getByText(/group members’ messages are retained up to 50 messages \/ 24 hours/)).toBeInTheDocument();
    expect(screen.getByText(/Passwords and keys are masked automatically/)).toBeInTheDocument();
    expect(await screen.findByRole('link', { name: 'Open redaction settings' })).toHaveAttribute('href', '/app/system/settings?tab=redaction');
    expect(screen.getByText(/Channel input redaction is off/)).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Disable and clear' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('channels.group_context_remove', { channel: 'discord', group_id: '222' }));
  });

  it('saves only selected Discord choices under the selected guild', async () => {
    const user = userEvent.setup();
    renderWithProviders(<GroupContextSettings platform="discord" active />);
    const guildInputs = await screen.findAllByLabelText('Discord server ID');
    await user.type(guildInputs[guildInputs.length - 1], '111');
    await user.click(await screen.findByRole('checkbox', { name: 'worker' }));
    await user.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(mockWsClient.call).toHaveBeenCalledWith('channels.discord_agent_choices_set', { guild_id: '111', agents: ['worker'] }));
  });
});

for (const [enabled, mode, showNotice] of [[true, 'on', false], [false, 'on', true], [true, 'inherit', true], [true, 'selective', true]] as const) {
  it(`shows redaction notice for enabled=${enabled}, mode=${mode}: ${showNotice}`, async () => {
    const previous = mockWsClient.call.getMockImplementation()!;
    mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) => method === 'redaction.get'
      ? Promise.resolve({ enabled, sources: { user_input: { mode } }, poisoned: null }) : previous(method, ...args));
    renderWithProviders(<GroupContextSettings platform="telegram" active />);
    await screen.findByRole('button', { name: 'Enable / save' });
    await waitFor(() => expect(screen.queryByRole('link', { name: 'Open redaction settings' }) !== null).toBe(showNotice));
  });
}

it('keeps an actionable notice when redaction status cannot be loaded', async () => {
  const previous = mockWsClient.call.getMockImplementation()!;
  mockWsClient.call.mockImplementation((method: string, ...args: unknown[]) => method === 'redaction.get'
    ? Promise.reject(new Error('unavailable')) : previous(method, ...args));
  renderWithProviders(<GroupContextSettings platform="telegram" active />);
  expect(await screen.findByText(/redaction status could not be confirmed/)).toBeInTheDocument();
  expect(screen.getByRole('link', { name: 'Open redaction settings' })).toHaveAttribute('href', '/app/system/settings?tab=redaction');
});
