import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MemoryBrowser } from './MemoryBrowser';

/** Route each RPC this component issues to a canned response. */
function mockCalls(entries: unknown[]) {
  mockWsClient.call.mockImplementation((method: string) => {
    if (method === 'memory.browse') return Promise.resolve({ entries });
    if (method === 'agents.inspect') return Promise.resolve({ evolution: { cognitive_memory: true } });
    if (method === 'memory.forget') return Promise.resolve({ success: true, forgotten: true });
    return Promise.resolve({});
  });
}

const ENTRIES = [
  {
    id: 'm1',
    agent_id: 'agnes',
    content: '客戶的合約報價是三萬元',
    timestamp: new Date().toISOString(),
    tags: [],
  },
  {
    id: 'm2',
    agent_id: 'agnes',
    content: '老闆喜歡簡短的回覆語氣',
    timestamp: new Date().toISOString(),
    tags: [],
  },
  {
    id: 'm3',
    agent_id: 'agnes',
    content: '2026-07-29 活躍時段 Top3（UTC 小時）：01:00、05:00、07:00',
    timestamp: new Date().toISOString(),
    tags: ['footprint-distill'],
    source_event: 'footprint_distill',
  },
];

beforeEach(() => {
  vi.clearAllMocks();
  mockCalls(ENTRIES);
});

describe('MemoryBrowser', () => {
  it('groups entries into topic categories with counts', async () => {
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    // The rail lists every non-empty category plus an "All" entry.
    expect(await screen.findByRole('button', { name: /All\s*3/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Customers & deals\s*1/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Preferences & habits\s*1/ })).toBeInTheDocument();
    // Origin beats keywords: the footprint roll-up lands in its own bucket.
    expect(screen.getByRole('button', { name: /Usage footprint\s*1/ })).toBeInTheDocument();
    // Categories with nothing in them are not rendered at all.
    expect(screen.queryByRole('button', { name: /Costs & billing/ })).not.toBeInTheDocument();
  });

  it('drills into one category from the rail', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByRole('button', { name: /Customers & deals\s*1/ }));

    expect(screen.getByText('客戶的合約報價是三萬元')).toBeInTheDocument();
    expect(screen.queryByText('老闆喜歡簡短的回覆語氣')).not.toBeInTheDocument();
  });

  it('deletes a memory only after a confirm click', async () => {
    const user = userEvent.setup();
    renderWithProviders(<MemoryBrowser agentId="agnes" query="" />);

    await user.click(await screen.findByRole('button', { name: /Preferences & habits\s*1/ }));
    await user.click(screen.getByRole('button', { name: 'Delete this memory' }));

    // First click only arms the confirm — nothing has been sent yet.
    expect(mockWsClient.call).not.toHaveBeenCalledWith('memory.forget', expect.anything());

    await user.click(screen.getByRole('button', { name: 'Delete' }));

    await waitFor(() => {
      expect(mockWsClient.call).toHaveBeenCalledWith('memory.forget', {
        agent_id: 'agnes',
        memory_id: 'm2',
      });
    });
    // The row leaves the list without a refetch.
    await waitFor(() => {
      expect(screen.queryByText('老闆喜歡簡短的回覆語氣')).not.toBeInTheDocument();
    });
  });

  it('shows a search-specific empty state', async () => {
    mockCalls([]);
    mockWsClient.call.mockImplementation((method: string) => {
      if (method === 'memory.search') return Promise.resolve({ entries: [] });
      if (method === 'memory.browse') return Promise.resolve({ entries: [] });
      return Promise.resolve({});
    });
    renderWithProviders(<MemoryBrowser agentId="agnes" query="nothing-here" />);

    expect(await screen.findByText(/No memory matches "nothing-here"/)).toBeInTheDocument();
  });
});
