import { describe, it, expect, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RecentTasks } from './RecentTasks';

// L2 round 4: a discovery task's `pending_approval` status used to resolve to
// an undefined icon component — React #130 blanked the whole dashboard.
describe('RecentTasks with discovery statuses', () => {
  beforeEach(() => {
    mockWsClient.call.mockReset();
  });

  it('renders a pending_approval / queued discovery task without throwing', async () => {
    mockWsClient.call.mockResolvedValue({
      tasks: [
        { id: 'd1', kind: 'discovery', title: 'Explore parser', description: '', status: 'pending_approval', priority: 'medium', assigned_to: 'a', created_by: 'u', created_at: '2026-09-30T00:00:00Z', updated_at: '2026-09-30T00:00:01Z' },
        { id: 'd2', kind: 'discovery', title: 'Queued exploration', description: '', status: 'queued', priority: 'medium', assigned_to: 'a', created_by: 'u', created_at: '2026-09-30T00:00:00Z', updated_at: '2026-09-30T00:00:00Z' },
      ],
    });
    renderWithProviders(<RecentTasks agents={[]} enabled />);
    expect(await screen.findByText('Explore parser')).toBeInTheDocument();
    expect(screen.getByText('Queued exploration')).toBeInTheDocument();
  });
});
