import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { useSystemStore } from '@/stores/system-store';
import { MemoryPage } from './MemoryPage';

/** Point the edition gate at one profile for the duration of a test. */
function setEdition(profile: 'personal' | 'enterprise') {
  useSystemStore.setState({
    status: { edition_profile: profile },
  } as Parameters<typeof useSystemStore.setState>[0]);
}

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({});
  setEdition('enterprise');
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('MemoryPage', () => {
  it('renders the collection header', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('heading', { name: 'Memory' })).toBeInTheDocument();
  });

  it('renders memory, knowledge and learning segments (enterprise)', () => {
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Memories' })).toBeInTheDocument();
    // The knowledge base merged in here (2026-07-30 client feedback).
    expect(screen.getByRole('radio', { name: 'Personal' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Shared' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Key Insights' })).toBeInTheDocument();
    expect(screen.getByRole('radio', { name: 'Self-Improvement' })).toBeInTheDocument();
  });

  it('collapses the two knowledge bases into one tab on Personal', () => {
    setEdition('personal');
    renderWithProviders(<MemoryPage />);
    expect(screen.getByRole('radio', { name: 'Knowledge base' })).toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Shared' })).not.toBeInTheDocument();
    expect(screen.queryByRole('radio', { name: 'Personal' })).not.toBeInTheDocument();
  });
});
