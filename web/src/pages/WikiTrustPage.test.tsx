import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { WikiTrustPage } from './WikiTrustPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('WikiTrustPage', () => {
  it('renders the collection header and summary tiles', () => {
    renderWithProviders(<WikiTrustPage />);
    expect(screen.getByRole('heading', { name: 'Wiki Trust' })).toBeInTheDocument();
    expect(screen.getByText('Archived')).toBeInTheDocument();
    expect(screen.getByText('Locked')).toBeInTheDocument();
  });

  it('shows the empty state when no rows fall in range', () => {
    renderWithProviders(<WikiTrustPage />);
    expect(screen.getByText('No trust data')).toBeInTheDocument();
  });
});
