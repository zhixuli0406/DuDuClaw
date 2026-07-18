import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { MemoryPage } from './MemoryPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('MemoryPage', () => {
  it('renders the collection header', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('heading', { name: 'Memory' })).toBeInTheDocument();
  });

  it('renders the view switcher with all three segments', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Memories' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Key Insights' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Self-Improvement' })).toBeInTheDocument();
  });
});
