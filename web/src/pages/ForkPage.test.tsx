import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { ForkPage } from './ForkPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ forks: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('ForkPage', () => {
  it('renders the parallel-branches collection header', () => {
    renderWithProviders(<ForkPage />);
    expect(screen.getByRole('heading', { name: 'Forks' })).toBeInTheDocument();
  });

  it('offers a refresh action', () => {
    renderWithProviders(<ForkPage />);
    expect(screen.getByRole('button', { name: 'Refresh' })).toBeInTheDocument();
  });
});
