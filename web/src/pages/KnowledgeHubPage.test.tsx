import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen, waitFor } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { KnowledgeHubPage } from './KnowledgeHubPage';

const AGENT = { name: 'agent-1', display_name: 'Agent One' };
const PAGE = { path: 'notes/one.md', title: 'One', updated: '2026-08-01T00:00:00Z', tags: ['t'] };

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockImplementation((method: string) => {
    switch (method) {
      case 'agents.list':
        return Promise.resolve({ agents: [AGENT] });
      case 'wiki.pages':
        return Promise.resolve({ pages: [PAGE], exists: true });
      case 'wiki.read':
        return Promise.resolve({ content: 'Just page content here.', path: PAGE.path });
      case 'memory.graph':
        return Promise.resolve({ nodes: [], edges: [], truncated: false });
      default:
        return Promise.resolve({});
    }
  });
});

/**
 * UX audit §2-5 — WikiGraph's `onSelectPage` was never wired up in
 * KnowledgeHubPage's graph view, so a page node looked clickable (cursor:
 * pointer, hover state) but a click did nothing. This regression-tests the
 * fix: clicking a page node in the topic map opens that page's reading view
 * in the browse tab.
 */
describe('KnowledgeHubPage — topic map click opens the page (UX audit §2-5)', () => {
  it('switches to the browse tab and opens the reading view when a graph node is clicked', async () => {
    const { container } = renderWithProviders(<KnowledgeHubPage embedded />);

    const graphTab = await screen.findByRole('radio', { name: 'Topic map' });
    graphTab.click();

    // Wait for the async page/content fetches to populate the topic map with
    // at least one page node.
    const circle = await waitFor(() => {
      const el = container.querySelector('[data-node-kind="page"]');
      if (!el) throw new Error('page node not rendered yet');
      return el;
    });

    circle.dispatchEvent(new MouseEvent('click', { bubbles: true }));

    // Reading view for the clicked page should now be showing — the browse
    // tab should be selected again and the page content fetched.
    expect(await screen.findByText('Just page content here.')).toBeInTheDocument();
    expect(await screen.findByRole('radio', { name: 'Browse', checked: true })).toBeInTheDocument();
    expect(mockWsClient.call).toHaveBeenCalledWith('wiki.read', {
      agent_id: AGENT.name,
      page_path: PAGE.path,
    });
  });
});
