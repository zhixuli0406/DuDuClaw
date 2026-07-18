import { describe, it, expect, vi, beforeEach } from 'vitest';
import { screen } from '@testing-library/react';
import { mockWsClient } from '@/test/mocks';
import { renderWithProviders } from '@/test/render';
import { RunsPage } from './RunsPage';

beforeEach(() => {
  vi.clearAllMocks();
  mockWsClient.call.mockResolvedValue({ runs: [], agents: [] });
  try { localStorage.clear(); } catch { /* jsdom */ }
});

describe('RunsPage', () => {
  it('renders the run-log header in the split layout', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByRole('heading', { name: 'Run log' })).toBeInTheDocument();
  });

  it('shows the empty transcript prompt when no run is selected', () => {
    renderWithProviders(<RunsPage />);
    expect(screen.getByText('Pick a run on the left')).toBeInTheDocument();
  });
});
