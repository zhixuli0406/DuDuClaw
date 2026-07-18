import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { InboxPage } from './InboxPage';

beforeEach(() => {
  vi.clearAllMocks();
});

describe('InboxPage (Multica list+detail split)', () => {
  it('renders the list column header + scope tabs', () => {
    renderWithProviders(<InboxPage />);
    // Page title in the left list header.
    expect(screen.getByRole('heading', { name: 'Inbox' })).toBeInTheDocument();
    // The five scope tabs.
    expect(screen.getByRole('button', { name: /Mine/ })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /^All/ })).toBeInTheDocument();
  });

  it('shows the empty detail placeholder when nothing is selected', () => {
    renderWithProviders(<InboxPage />);
    expect(screen.getByText('Select an item to see its details')).toBeInTheDocument();
  });
});
