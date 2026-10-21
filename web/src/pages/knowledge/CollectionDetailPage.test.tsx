import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import '@/test/mocks';
import { CollectionDetailPage, resolveTab } from './CollectionDetailPage';
import { collection, mockRpc, renderAt, source } from './test-fixtures';
import type { KbRole } from '@/lib/knowledge-api';

function setup(role: KbRole) {
  mockRpc({
    'knowledge.collections.get': () => ({ collection: collection({ my_role: role }) }),
    'knowledge.sources.list': () => ({ sources: [source()] }),
  });
}

beforeEach(() => vi.clearAllMocks());

describe('CollectionDetailPage', () => {
  it('owners get every tab; the tab lives in ?tab=', async () => {
    const user = userEvent.setup();
    setup('owner');
    renderAt(<CollectionDetailPage />, '/knowledge-base/col_1', '/knowledge-base/:id');
    expect(await screen.findByRole('heading', { name: 'HR handbook' })).toBeInTheDocument();
    for (const name of ['Sources', 'Try a search', 'Sharing', 'Access record', 'Settings']) {
      expect(screen.getByRole('tab', { name })).toBeInTheDocument();
    }
    expect(await screen.findByText('leave.pdf')).toBeInTheDocument();
    await user.click(screen.getByRole('tab', { name: 'Try a search' }));
    expect(await screen.findByRole('search')).toBeInTheDocument();
  });

  it('viewers do not get the access record or settings', async () => {
    setup('viewer');
    renderAt(<CollectionDetailPage />, '/knowledge-base/col_1?tab=settings', '/knowledge-base/:id');
    expect(await screen.findByRole('heading', { name: 'HR handbook' })).toBeInTheDocument();
    expect(screen.queryByRole('tab', { name: 'Access record' })).toBeNull();
    expect(screen.queryByRole('tab', { name: 'Settings' })).toBeNull();
    // A settings deep link falls back to the sources tab.
    expect(screen.getByRole('tab', { name: 'Sources' })).toHaveAttribute('aria-selected', 'true');
  });

  it('a collection that cannot be opened shows an error, not a blank page', async () => {
    mockRpc({});
    renderAt(<CollectionDetailPage />, '/knowledge-base/col_x', '/knowledge-base/:id');
    expect(await screen.findByText('This collection could not be opened')).toBeInTheDocument();
  });

  it('resolveTab', () => {
    expect(resolveTab(null, true)).toBe('sources');
    expect(resolveTab('log', true)).toBe('log');
    expect(resolveTab('log', false)).toBe('sources');
    expect(resolveTab('nonsense', true)).toBe('sources');
    expect(resolveTab('permissions', false)).toBe('permissions');
  });
});
