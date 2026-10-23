import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { AgentSkillsPanel } from './AgentSkillsPanel';

function setup(canManage: boolean) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'skills.list')
      return Promise.resolve({
        skills: [
          { name: 'tone', content: '', scope: 'global', agent_id: 'agnes', enabled: true },
          { name: 'quote', content: '', scope: 'agent', agent_id: 'agnes', enabled: false, use_count: 2, last_used_at: '2026-10-20T00:00:00Z' },
        ],
        skill_state: { ok: true },
        can_manage: canManage,
      });
    if (method === 'skills.archived')
      return Promise.resolve({
        agent_id: 'agnes',
        can_restore: canManage,
        archived: [{ archive_id: '20261023T000000000Z-abcd1234', skill_name: 'old-quote', archived_at: '2026-10-22T00:00:00Z', archived_by: 'u1' }],
      });
    return Promise.resolve({ results: [{ agent_id: 'agnes', skill_name: 'quote', ok: true }] });
  });
}

beforeEach(() => vi.clearAllMocks());

describe('AgentSkillsPanel', () => {
  it('switches a skill back on, removes an own skill and restores an archived one', async () => {
    const user = userEvent.setup();
    setup(true);
    renderWithProviders(<AgentSkillsPanel agentId="agnes" agentName="Agnes" />);
    const sw = await screen.findByRole('switch', { name: 'Agnes uses “quote”' });
    expect(sw).not.toBeChecked();
    await user.click(sw);
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('skills.set_enabled', {
        items: [{ agent_id: 'agnes', skill_name: 'quote' }],
        enabled: true,
      }),
    );
    // Company-wide skills have no remove button.
    expect(screen.queryByRole('button', { name: 'Remove “tone”' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Remove “quote”' }));
    await user.click(await screen.findByRole('button', { name: 'Remove' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('skills.remove', {
        items: [{ agent_id: 'agnes', skill_name: 'quote' }],
      }),
    );
    await user.click(screen.getByRole('button', { name: 'Restore' }));
    await waitFor(() =>
      expect(mockWsClient.call).toHaveBeenCalledWith('skills.restore', {
        agent_id: 'agnes',
        archive_id: '20261023T000000000Z-abcd1234',
      }),
    );
  });

  it('is read-only without Owner access', async () => {
    setup(false);
    renderWithProviders(<AgentSkillsPanel agentId="agnes" agentName="Agnes" />);
    expect(await screen.findByRole('switch', { name: 'Agnes uses “quote”' })).toHaveAttribute('data-disabled');
    expect(screen.getByRole('button', { name: 'Restore' })).toBeDisabled();
  });
});
