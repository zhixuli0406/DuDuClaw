import { describe, it, expect, vi } from 'vitest';
import { screen } from '@testing-library/react';
import { renderWithProviders } from '@/test/render';
import { WikiGraph } from './WikiGraph';
import type { WikiPageMeta } from '@/lib/api';

const page = (path: string, title: string, tags: string[] = []): WikiPageMeta => ({
  path,
  title,
  updated: '2026-08-01T00:00:00Z',
  tags,
});

describe('WikiGraph', () => {
  it('shows the empty state when there are no pages', () => {
    renderWithProviders(<WikiGraph pages={[]} pageContents={{}} />);
    expect(screen.getByText('No pages to visualize yet.')).toBeInTheDocument();
  });

  it('counts topics, not just pages, in the stats bar', () => {
    renderWithProviders(
      <WikiGraph pages={[page('policies/refund.md', 'Refund policy', ['support'])]} pageContents={{}} />,
    );
    // 1 page + 2 topics (tag `support`, folder `policies`) + 2 page-topic links —
    // the pre-WP16 graph rendered this as "1 nodes · 0 links".
    expect(screen.getByText('1 pages · 2 topics · 2 links')).toBeInTheDocument();
  });

  it('renders one circle per page and one square per topic', () => {
    const { container } = renderWithProviders(
      <WikiGraph
        pages={[page('a/one.md', 'One', ['shared']), page('a/two.md', 'Two', ['shared'])]}
        pageContents={{}}
      />,
    );
    expect(container.querySelectorAll('[data-node-kind="page"]')).toHaveLength(2);
    // Topics: tag `shared` + folder `a`.
    expect(container.querySelectorAll('[data-node-kind="topic"]')).toHaveLength(2);
  });

  it('folds memory-graph entities in as topics', () => {
    renderWithProviders(
      <WikiGraph
        pages={[page('auto/charter/team.md', 'Team charter', [])]}
        pageContents={{}}
        memoryEdges={[
          {
            subject: 'wiki:auto/charter/team',
            predicate: 'covers',
            object: 'person:Wang Ming',
            memory_id: 'm1',
            origin_trust: 0.9,
            quarantined: false,
          },
        ]}
      />,
    );
    // 1 page + folder topic `charter` + entity topic `Wang Ming`.
    expect(screen.getByText('1 pages · 2 topics · 2 links')).toBeInTheDocument();
  });

  it('tells the user why nothing is connected yet instead of showing a bare dot', () => {
    // A root-level page with no tags still gets an 「Uncategorized」 topic node,
    // so it is never a lone dot — but nothing is *related* yet, so say so.
    renderWithProviders(<WikiGraph pages={[page('notes.md', 'Notes', [])]} pageContents={{}} />);
    expect(screen.getByText('1 pages · 1 topics · 1 links')).toBeInTheDocument();
    expect(screen.getByText(/Every page still stands alone/)).toBeInTheDocument();
  });

  it('drops the hint once two pages actually share a topic', () => {
    renderWithProviders(
      <WikiGraph
        pages={[page('a/one.md', 'One', ['shared']), page('a/two.md', 'Two', ['shared'])]}
        pageContents={{}}
      />,
    );
    expect(screen.queryByText(/Every page still stands alone/)).not.toBeInTheDocument();
  });

  it('opens a page when its node is clicked', async () => {
    const onSelectPage = vi.fn();
    const { container } = renderWithProviders(
      <WikiGraph
        pages={[page('a/one.md', 'One', ['t'])]}
        pageContents={{}}
        onSelectPage={onSelectPage}
      />,
    );
    const circle = container.querySelector('[data-node-kind="page"]');
    expect(circle).not.toBeNull();
    circle?.dispatchEvent(new MouseEvent('click', { bubbles: true }));
    expect(onSelectPage).toHaveBeenCalledWith('a/one.md');
  });
});
