import { describe, it, expect, beforeEach, vi } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ActivityFeed } from './ActivityFeed';
import { useTasksStore } from '@/stores/tasks-store';
import { useConnectionStore } from '@/stores/connection-store';
import type { ActivityEvent } from '@/lib/api';

const now = Date.now();
const ev = (id: string, type: ActivityEvent['type'], agent: string, summary: string): ActivityEvent => ({
  id,
  type,
  agent_id: agent,
  summary,
  timestamp: new Date(now).toISOString(),
});

beforeEach(() => {
  vi.clearAllMocks();
  // Keep the fetch effect from wiping seeded data.
  useConnectionStore.setState({ state: 'disconnected' as never, error: null });
});

describe('ActivityFeed three-tier denoising', () => {
  it('hides Tier 3 routine chatter by default and reveals it on "show all"', async () => {
    useTasksStore.setState({
      activities: [
        ev('a', 'task_created', 'bot', 'headline task'),
        ev('b', 'agent_reply', 'bot', 'routine chatter'),
      ] as never,
    });

    renderWithProviders(<ActivityFeed />);

    expect(screen.getByText('headline task')).toBeInTheDocument();
    expect(screen.queryByText('routine chatter')).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole('button', { name: /show all details/i }));
    expect(screen.getByText('routine chatter')).toBeInTheDocument();
  });

  it('folds ≥3 consecutive same-agent updates into one collapsed row', async () => {
    useTasksStore.setState({
      activities: [
        ev('1', 'task_created', 'bot', 'u1'),
        ev('2', 'task_completed', 'bot', 'u2'),
        ev('3', 'task_blocked', 'bot', 'u3'),
      ] as never,
    });

    renderWithProviders(<ActivityFeed />);
    // Individual summaries are folded away; a "consecutive updates" row shows.
    expect(screen.queryByText('u2')).not.toBeInTheDocument();
    expect(screen.getByText(/consecutive updates/i)).toBeInTheDocument();
  });
});
