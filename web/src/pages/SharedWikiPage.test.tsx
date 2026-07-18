import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { SharedWikiPage } from './SharedWikiPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('SharedWikiPage', () => {
  it('renders the collection header when standalone', () => {
    renderWithProviders(<SharedWikiPage />);
    expect(screen.getByRole('heading', { name: 'Shared Wiki' })).toBeInTheDocument();
  });

  it('renders the view switcher with all four segments', () => {
    renderWithProviders(<SharedWikiPage />);
    expect(screen.getByRole('radio', { name: 'Browse' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Search' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Stats' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Namespace Policy' })).toBeInTheDocument();
  });

  it('does not render its own header when embedded', () => {
    renderWithProviders(<SharedWikiPage embedded />);
    expect(screen.queryByRole('heading', { name: 'Shared Wiki' })).not.toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Browse' })).toBeInTheDocument();
  });
});
